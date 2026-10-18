// av2oracle.c: what pristine AVM makes of an AV2 still, in a form the Rust
// decoder can be diffed against. Decodes one OBU file and prints
//
//   picture <width> <height> <bit_depth> <ss_x> <ss_y> <monochrome>
//   hash <FNV-1a of the planes, as ../../fast-decode/av2bench.c hashes them>
//   seq.* / frame.* lines  the parsed headers, read out of AVM's own structs,
//                          in the format of ../av2-tone/src/dump.rs
//   tile <row> <col> <offset> <size>
//                          each tile's bytes, offset from the start of the file
//   frame_context <hex>    the FRAME_CONTEXT every tile of the key frame
//                          started from (cm->default_frame_context), all
//                          sizeof(FRAME_CONTEXT) bytes
//
// and, given a second path, writes the cropped planes there as raw 16-bit
// little-endian samples, Y then U then V, each plane tightly packed.
//
// With --entropy it checks nothing about a file. It drives AVM's own entropy
// encoder (avm_dsp/entenc.c) over a random symbol stream, decodes the result
// with AVM's own reader, and prints the stream, every operation, and AVM's
// tell after each, for the Rust decoder to replay (see entropy_mode below).
//
// AVM keeps its decoder state behind a private struct. The shim reaches it
// without patching AVM: it swaps in a thread-worker interface (a public AVM
// hook) whose execute() remembers the frame worker's AV2Decoder before
// handing the call to AVM's own. AVM is linked as built, untouched.
//
//   av2oracle <file.obu> [planes.bin]
//   av2oracle --entropy <seed> <operations>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "avm/avm_decoder.h"
#include "avm/avmdx.h"
#include "avm_dsp/bitreader.h"
#include "avm_dsp/entenc.h"
#include "avm_dsp/prob.h"
#include "avm_util/avm_thread.h"
#include "av2/common/av2_common_int.h"
#include "av2/decoder/decoder.h"
#include "av2/decoder/dthread.h"

static AVxWorkerInterface avm_default;
static AV2Decoder *pbi = NULL;

static void execute_and_remember(AVxWorker *const w) {
  if (w->thread_name && !strcmp(w->thread_name, "avm frameworker") && w->data1)
    pbi = ((FrameWorkerData *)w->data1)->pbi;
  avm_default.execute(w);
}

static void list_int(const char *key, const int *v, int n) {
  printf("%s", key);
  for (int i = 0; i < n; i++) printf(" %d", v[i]);
  printf("\n");
}

static void dump_seq(const SequenceHeader *s) {
  printf("seq.id %d\n", (int)s->seq_header_id);
  printf("seq.profile %d\n", (int)s->seq_profile_idc);
  printf("seq.single_picture %d\n", (int)s->single_picture_header_flag);
  printf("seq.level_tier %d %d\n", (int)s->seq_max_level_idx, (int)s->seq_tier);
  printf("seq.format %d %d %d %d\n", (int)s->bit_depth, s->subsampling_x, s->subsampling_y, (int)s->monochrome);
  printf("seq.still %d\n", (int)s->still_picture);
  printf("seq.max_size %d %d %d %d\n", s->max_frame_width, s->max_frame_height, s->num_bits_width,
         s->num_bits_height);
  printf("seq.conf_win %d %d %d %d %d\n", (int)s->conf.conf_win_enabled_flag, (int)s->conf.conf_win_left_offset,
         (int)s->conf.conf_win_right_offset, (int)s->conf.conf_win_top_offset,
         (int)s->conf.conf_win_bottom_offset);
  printf("seq.sb_size %d\n", s->mib_size * 4);
  printf("seq.partition %d %d %d %d %d\n", (int)s->enable_sdp, (int)s->enable_extended_sdp,
         (int)s->enable_ext_partitions, (int)s->enable_uneven_4way_partitions, (int)s->max_pb_aspect_ratio_log2_m1);
  printf("seq.segment %d %d\n", (int)s->enable_ext_seg, (int)s->seq_seg_info_present_flag);
  if (s->seq_seg_info_present_flag) {
    const SegmentationInfoSyntax *g = &s->seg_params;
    printf("seq.seg_change %d\n", (int)g->allow_seg_info_change);
    printf("seq.seg_mask");
    for (int i = 0; i < MAX_SEGMENTS; i++) printf(" %d", (int)g->feature_mask[i]);
    printf("\nseq.seg_alt_q");
    for (int i = 0; i < MAX_SEGMENTS; i++) printf(" %d", (int)g->feature_data[i][SEG_LVL_ALT_Q]);
    printf("\nseq.seg_derived %d %d\n", (int)g->segid_preskip, (int)g->last_active_segid);
  }
  printf("seq.intra %d %d %d %d %d %d %d\n", (int)s->enable_intra_dip, (int)s->enable_intra_edge_filter,
         (int)s->enable_mrls, (int)s->enable_cfl_intra, (int)s->cfl_ds_filter_index, (int)s->enable_mhccp,
         (int)s->enable_ibp);
  printf("seq.inter %d %d %d %d %d %d\n", (int)s->enable_refmvbank, (int)s->enable_drl_reorder, s->ref_frames,
         (int)s->def_max_bvp_drl_bits, (int)s->allow_frame_max_bvp_drl_bits, (int)s->enable_bawp);
  printf("seq.scc %d %d\n", (int)s->force_screen_content_tools, (int)s->force_integer_mv);
  printf("seq.transform %d %d %d %d %d %d %d\n", (int)s->enable_fsc, (int)s->enable_idtx_intra, (int)s->enable_ist,
         (int)s->enable_inter_ist, (int)s->enable_chroma_dctonly, (int)s->reduced_tx_part_set, (int)s->enable_cctx);
  printf("seq.quant %d %d %d %d %d\n", (int)s->enable_tcq, (int)s->enable_parity_hiding, (int)s->enable_avg_cdf,
         (int)s->avg_cdf_type, (int)s->separate_uv_delta_q);
  printf("seq.dc_ac %d %d %d %d %d %d %d\n", (int)s->equal_ac_dc_q, (int)s->base_y_dc_delta_q,
         (int)s->y_dc_delta_q_enabled, (int)s->base_uv_dc_delta_q, (int)s->uv_dc_delta_q_enabled,
         (int)s->base_uv_ac_delta_q, (int)s->uv_ac_delta_q_enabled);
  printf("seq.filter %d %d %d %d %d %d %d %d\n", (int)s->disable_loopfilters_across_tiles, (int)s->enable_cdef,
         (int)s->enable_gdf, (int)s->gdf_unit_matches_sb_size, (int)s->enable_ccso, (int)s->ccso_unit_matches_sb_size,
         (int)s->enable_cdef_on_skip_txfm, (int)s->df_par_bits_minus2);
  printf("seq.restoration %d\n", (int)s->enable_restoration);
  if (s->enable_restoration)
    printf("seq.lr_disable %d %d\n", (int)s->lr_tools_disable_mask[0], (int)s->lr_tools_disable_mask[1]);
  printf("seq.tile_info %d\n", (int)s->seq_tile_info_present_flag);
  printf("seq.film_grain %d\n", (int)s->film_grain_params_present);
}

static void dump_frame(const AV2Decoder *d) {
  const AV2_COMMON *cm = &d->common;
  const SequenceHeader *s = &cm->seq_params;
  const FeatureFlags *f = &cm->features;
  const CommonQuantParams *q = &cm->quant_params;
  const MACROBLOCKD *xd = &d->dcb.xd;
  const int planes = s->monochrome ? 1 : 3;
  const int segs = s->enable_ext_seg ? MAX_SEGMENTS : MAX_SEGMENTS_8;

  printf("frame.ids %d %d\n", cm->cur_mfh_id, cm->mfh_params[cm->cur_mfh_id].mfh_seq_header_id);
  printf("frame.type_show_refresh %d %d %d\n", (int)cm->current_frame.frame_type, cm->immediate_output_picture,
         cm->current_frame.refresh_frame_flags);
  printf("frame.sb_size %d\n", cm->mib_size * 4);
  printf("frame.size %d %d %d %d\n", cm->width, cm->height, cm->mi_params.mi_cols, cm->mi_params.mi_rows);
  printf("frame.scc %d %d\n", (int)f->allow_screen_content_tools, (int)f->cur_frame_force_integer_mv);
  printf("frame.intrabc %d %d %d\n", (int)f->allow_intrabc, (int)f->allow_global_intrabc,
         (int)f->allow_local_intrabc);
  if (f->allow_intrabc) printf("frame.bvp_drl %d\n", (int)f->max_bvp_drl_bits);
  printf("frame.cdf_off %d\n", (int)f->disable_cdf_update);
  const CommonTileParams *t = &cm->tiles;
  printf("frame.tiles %d %d %d %d %d\n", t->uniform_spacing, t->cols, t->rows, t->log2_cols, t->log2_rows);
  list_int("frame.tile_cols", t->col_start_sb, t->cols + 1);
  list_int("frame.tile_rows", t->row_start_sb, t->rows + 1);
  if (t->cols * t->rows > 1) printf("frame.tile_bytes %d %d\n", d->context_update_tile_id, d->tile_size_bytes);
  printf("frame.q %d %d %d %d %d %d\n", q->base_qindex, q->y_dc_delta_q, q->u_dc_delta_q, q->u_ac_delta_q,
         q->v_dc_delta_q, q->v_ac_delta_q);
  const struct segmentation *g = &cm->seg;
  printf("frame.seg %d\n", (int)g->enabled);
  if (g->enabled) {
    printf("frame.seg_flags %d %d %d %d %d\n", (int)g->update_map, (int)g->update_data, (int)g->temporal_update,
           (int)g->segid_preskip, (int)g->last_active_segid);
    printf("frame.seg_mask");
    for (int i = 0; i < segs; i++) printf(" %d", (int)g->feature_mask[i]);
    printf("\nframe.seg_alt_q");
    for (int i = 0; i < segs; i++) printf(" %d", (int)g->feature_data[i][SEG_LVL_ALT_Q]);
    printf("\n");
  }
  printf("frame.qm %d\n", (int)q->using_qmatrix);
  if (q->using_qmatrix) {
    printf("frame.qm_y");
    for (int i = 0; i < q->pic_qm_num; i++) printf(" %d", q->qm_y[i]);
    printf("\n");
    if (planes > 1) {
      printf("frame.qm_u");
      for (int i = 0; i < q->pic_qm_num; i++) printf(" %d", q->qm_u[i]);
      printf("\nframe.qm_v");
      for (int i = 0; i < q->pic_qm_num; i++) printf(" %d", q->qm_v[i]);
      printf("\n");
    }
    printf("frame.qm_index");
    for (int i = 0; i < segs; i++)
      if (!xd->lossless[i]) printf(" %d", q->qm_index[i]);
    printf("\n");
  }
  printf("frame.delta_q %d %d\n", cm->delta_q_info.delta_q_present_flag, cm->delta_q_info.delta_q_res);
  list_int("frame.qindex", xd->qindex, segs);
  list_int("frame.lossless", xd->lossless, segs);
  printf("frame.coded_lossless %d\n", (int)f->coded_lossless);
  printf("frame.tcq_ph %d %d\n", (int)f->tcq_mode, (int)f->allow_parity_hiding);
  const struct loopfilter *lf = &cm->lf;
  printf("frame.deblock %d %d %d %d %d %d %d %d\n", lf->apply_deblocking_filter[0], lf->apply_deblocking_filter[1],
         lf->apply_deblocking_filter_u, lf->apply_deblocking_filter_v, lf->delta_q_luma[0], lf->delta_q_luma[1],
         lf->delta_q_u, lf->delta_q_v);
  if (s->enable_gdf) {
    const GdfInfo *gi = &cm->gdf_info;
    printf("frame.gdf %d %d %d %d %d\n", gi->gdf_mode, gi->gdf_pic_qp_idx, gi->gdf_pic_scale_idx, gi->gdf_block_size,
           gi->gdf_block_num);
  }
  if (s->enable_cdef) {
    const CdefInfo *c = &cm->cdef_info;
    printf("frame.cdef %d %d %d %d\n", c->cdef_frame_enable, c->cdef_damping, c->nb_cdef_strengths,
           c->cdef_on_skip_txfm_frame_enable);
    list_int("frame.cdef_y", c->cdef_strengths, c->nb_cdef_strengths);
    list_int("frame.cdef_uv", c->cdef_uv_strengths, c->nb_cdef_strengths);
  }
  if (s->enable_restoration) {
    printf("frame.lr_type");
    for (int p = 0; p < planes; p++) printf(" %d", (int)cm->rst_info[p].frame_restoration_type);
    printf("\nframe.lr_unit");
    for (int p = 0; p < planes; p++) printf(" %d", cm->rst_info[p].restoration_unit_size);
    printf("\n");
  }
  if (s->enable_ccso) {
    const CcsoInfo *c = &cm->ccso_info;
    static const int intervals[2] = { 3, 2 };
    printf("frame.ccso %d %d\n", (int)c->ccso_frame_flag, c->ccso_blk_size);
    for (int p = 0; p < planes; p++) {
      printf("frame.ccso%d %d\n", p, (int)c->ccso_enable[p]);
      if (!c->ccso_enable[p]) continue;
      printf("frame.ccso%d_params %d %d %d %d %d %d\n", p, (int)c->ccso_bo_only[p], (int)c->scale_idx[p],
             (int)c->quant_idx[p], (int)c->ext_filter_support[p], (int)c->edge_clf[p], c->max_band_log2[p]);
      const int n = c->ccso_bo_only[p] ? 1 : intervals[c->edge_clf[p]];
      printf("frame.ccso%d_offsets", p);
      for (int d0 = 0; d0 < n; d0++)
        for (int d1 = 0; d1 < n; d1++)
          for (int band = 0; band < (1 << c->max_band_log2[p]); band++)
            printf(" %d", (int)c->filter_offset[p][(band << 4) + (d0 << 2) + d1]);
      printf("\n");
    }
  }
  printf("frame.tx %d %d\n", (int)f->tx_mode, (int)f->reduced_tx_set_used);
  printf("frame.header_bits %u\n", d->uncomp_hdr_size_in_bits);
}

// ---- --entropy ------------------------------------------------------------
// One stream: a CDF per alphabet size 2..16, random but valid (strictly
// decreasing inverse CDF ending at 0, a random starting counter so all three
// rate intervals come up early, random per-interval rates), then a run of
// operations. Symbols adapt their CDF when the operation says so; the stream
// is all-adapting, never-adapting or mixed, chosen by the seed.
static uint32_t rng_state;
static uint32_t rnd(void) {
  rng_state ^= rng_state << 13;
  rng_state ^= rng_state >> 17;
  rng_state ^= rng_state << 5;
  return rng_state;
}
static int below(int n) { return (int)(rnd() % (uint32_t)n); }

enum { OP_SYM, OP_BOOL, OP_LIT, OP_BIT, OP_UNARY };
typedef struct {
  int op, arg, adapt, value;
} Op;

static void random_cdf(avm_cdf_prob *cdf, int n) {
  int w[16], total = 0;
  const int shape = below(3);
  for (int i = 0; i < n; i++) {
    w[i] = 1 + below(shape == 0 ? 64 : shape == 1 ? 4 : 1024);
    total += w[i];
  }
  if (shape == 2) {  // one symbol nearly certain
    w[below(n)] += 40 * total;
    total = 0;
    for (int i = 0; i < n; i++) total += w[i];
  }
  int c = 0, cum = 0;
  for (int i = 0; i < n - 1; i++) {
    cum += w[i];
    int next = (int)((int64_t)cum * CDF_PROB_TOP / total);
    if (next <= c) next = c + 1;
    if (next > CDF_PROB_TOP - (n - 1 - i)) next = CDF_PROB_TOP - (n - 1 - i);
    c = next;
    cdf[i] = AVM_ICDF(c);
  }
  cdf[n - 1] = 0;
  cdf[n] = below(3) ? 0 : below(33);
  for (int k = 1; k <= 3; k++) cdf[n + k] = below(7);
}

// A symbol drawn from the CDF half the time, uniform the other half, so both
// likely and unlikely symbols get coded.
static int pick(const avm_cdf_prob *cdf, int n) {
  if (below(2)) return below(n);
  const int x = below(CDF_PROB_TOP);
  for (int s = 0; s < n - 1; s++)
    if (x < CDF_PROB_TOP - cdf[s]) return s;
  return n - 1;
}

static int entropy_mode(uint32_t seed, int nops) {
  rng_state = seed * 2654435761u + 1;
  avm_cdf_prob enc_cdf[17][CDF_SIZE(16)], dec_cdf[17][CDF_SIZE(16)];
  for (int n = 2; n <= 16; n++) random_cdf(enc_cdf[n], n);
  memcpy(dec_cdf, enc_cdf, sizeof(enc_cdf));
  const int adapt_mode = seed % 3;  // 0 always, 1 never, 2 mixed
  Op *ops = calloc((size_t)nops, sizeof(*ops));
  if (!ops) return 2;

  od_ec_enc enc;
  avm_od_ec_enc_init(&enc, 1024);
  for (int i = 0; i < nops; i++) {
    Op *o = &ops[i];
    const int r = below(100);
    o->op = r < 70 ? OP_SYM : r < 80 ? OP_BOOL : r < 88 ? OP_LIT : r < 94 ? OP_BIT : OP_UNARY;
    switch (o->op) {
      case OP_SYM:
        o->arg = 2 + below(15);
        o->adapt = adapt_mode == 0 ? 1 : adapt_mode == 1 ? 0 : below(2);
        o->value = pick(enc_cdf[o->arg], o->arg);
        avm_od_ec_encode_cdf_q15(&enc, o->value, enc_cdf[o->arg], o->arg);
        if (o->adapt) update_cdf(enc_cdf[o->arg], o->value, o->arg);
        break;
      case OP_BOOL: {  // avm_write's 8-bit probability
        o->arg = below(256);
        o->value = below(2);
        const int p = (0x7FFFFF - (o->arg << 15) + o->arg) >> 8;
        avm_od_ec_encode_bool_q15(&enc, o->value, p);
        break;
      }
      case OP_LIT: {  // avm_write_literal's 8-bit chunks
        o->arg = 1 + below(24);
        o->value = (int)(rnd() & ((1u << o->arg) - 1));
        int bits = o->arg, data = o->value;
        while (bits > 0) {
          const int n = bits >= 8 ? 8 : bits;
          od_ec_encode_literal_bypass(&enc, data >> (bits - n), n);
          bits -= n;
          data &= (1 << bits) - 1;
        }
        break;
      }
      case OP_BIT:
        o->value = below(2);
        od_ec_encode_literal_bypass(&enc, o->value, 1);
        break;
      case OP_UNARY:  // value zeros, then a one unless value reached the cap
        o->arg = 1 + below(16);
        o->value = below(o->arg + 1);
        for (int k = 0; k < o->value; k++) od_ec_encode_literal_bypass(&enc, 0, 1);
        if (o->value < o->arg) od_ec_encode_literal_bypass(&enc, 1, 1);
        break;
    }
  }
  uint32_t nbytes = 0;
  unsigned char *data = avm_od_ec_enc_done(&enc, &nbytes);
  if (!data || enc.error) {
    fprintf(stderr, "AVM's encoder failed\n");
    return 1;
  }

  printf("entropy %u %u %d\ndata ", seed, nbytes, nops);
  for (uint32_t i = 0; i < nbytes; i++) printf("%02x", data[i]);
  printf("\n");
  for (int n = 2; n <= 16; n++) {
    printf("cdf %d", n);
    for (int k = 0; k < CDF_SIZE(n); k++) printf(" %u", dec_cdf[n][k]);
    printf("\n");
  }
  avm_reader r;
  if (avm_reader_init(&r, data, nbytes)) return 1;
  for (int i = 0; i < nops; i++) {
    const Op *o = &ops[i];
    int v = -1;
    switch (o->op) {
      case OP_SYM:
        r.allow_update_cdf = (uint8_t)o->adapt;
        v = avm_read_symbol(&r, dec_cdf[o->arg], o->arg, 0);
        printf("op sym %d %d", o->arg, o->adapt);
        break;
      case OP_BOOL:
        v = avm_read(&r, o->arg, 0);
        printf("op bool %d", o->arg);
        break;
      case OP_LIT:
        v = avm_read_literal(&r, o->arg, 0);
        printf("op lit %d", o->arg);
        break;
      case OP_BIT:
        v = avm_read_bit(&r, 0);
        printf("op bit");
        break;
      case OP_UNARY:
        v = avm_read_unary(&r, o->arg, 0);
        printf("op unary %d", o->arg);
        break;
    }
    if (v != o->value) {
      fprintf(stderr, "AVM decoded %d at operation %d, encoded %d\n", v, i, o->value);
      return 1;
    }
    printf(" %d %u %llu\n", v, avm_reader_tell(&r), (unsigned long long)avm_reader_tell_frac(&r));
  }
  for (int n = 2; n <= 16; n++) {
    printf("final %d", n);
    for (int k = 0; k < CDF_SIZE(n); k++) printf(" %u", dec_cdf[n][k]);
    printf("\n");
  }
  printf("end\n");
  avm_od_ec_enc_clear(&enc);
  free(ops);
  return 0;
}

int main(int argc, char **argv) {
  if (argc == 4 && !strcmp(argv[1], "--entropy")) return entropy_mode((uint32_t)strtoul(argv[2], NULL, 10), atoi(argv[3]));
  if (argc < 2) {
    fprintf(stderr, "usage: av2oracle <file.obu> [planes.bin]\n");
    return 2;
  }
  FILE *in = fopen(argv[1], "rb");
  if (!in) {
    fprintf(stderr, "cannot open %s\n", argv[1]);
    return 2;
  }
  fseek(in, 0, SEEK_END);
  const long n = ftell(in);
  rewind(in);
  uint8_t *buf = malloc(n > 0 ? (size_t)n : 1);
  if (!buf || fread(buf, 1, (size_t)n, in) != (size_t)n) return 2;
  fclose(in);

  avm_default = *avm_get_worker_interface();
  AVxWorkerInterface hooked = avm_default;
  hooked.execute = execute_and_remember;
  if (!avm_set_worker_interface(&hooked)) {
    fprintf(stderr, "avm_set_worker_interface refused the hook\n");
    return 1;
  }

  avm_codec_ctx_t ctx;
  avm_codec_dec_cfg_t cfg = { 0 };
  cfg.threads = 1;
  if (avm_codec_dec_init(&ctx, avm_codec_av2_dx(), &cfg, 0)) return 1;
  if (avm_codec_decode(&ctx, buf, (size_t)n, NULL)) {
    fprintf(stderr, "AVM: %s\n", avm_codec_error_detail(&ctx) ? avm_codec_error_detail(&ctx) : "decode failed");
    return 1;
  }
  avm_codec_iter_t it = NULL;
  avm_image_t *img = avm_codec_get_frame(&ctx, &it);
  if (!img) {
    avm_codec_decode(&ctx, NULL, 0, NULL);
    it = NULL;
    img = avm_codec_get_frame(&ctx, &it);
  }
  if (!img || !pbi) {
    fprintf(stderr, img ? "the frame worker never ran\n" : "AVM output no frame\n");
    return 1;
  }
  if (!(img->fmt & AVM_IMG_FMT_HIGHBITDEPTH)) {
    fprintf(stderr, "expected AVM's 16-bit planes\n");
    return 1;
  }

  const int nplanes = img->monochrome ? 1 : 3;
  printf("picture %u %u %u %u %u %d\n", img->d_w, img->d_h, img->bit_depth, img->x_chroma_shift,
         img->y_chroma_shift, img->monochrome);
  FILE *out = argc > 2 ? fopen(argv[2], "wb") : NULL;
  if (argc > 2 && !out) {
    fprintf(stderr, "cannot write %s\n", argv[2]);
    return 2;
  }
  uint64_t h = 1469598103934665603ULL;
  for (int p = 0; p < nplanes; p++) {
    const int w = p ? (int)((img->d_w + img->x_chroma_shift) >> img->x_chroma_shift) : (int)img->d_w;
    const int ph = p ? (int)((img->d_h + img->y_chroma_shift) >> img->y_chroma_shift) : (int)img->d_h;
    for (int y = 0; y < ph; y++) {
      const uint8_t *row = img->planes[p] + (size_t)y * img->stride[p];
      for (int x = 0; x < w * 2; x++) {
        h ^= row[x];
        h *= 1099511628211ULL;
      }
      // AVM holds samples as native uint16_t; the file is little-endian.
      if (out) {
        for (int x = 0; x < w; x++) {
          const uint16_t v = ((const uint16_t *)row)[x];
          const uint8_t le[2] = { (uint8_t)(v & 0xff), (uint8_t)(v >> 8) };
          fwrite(le, 1, 2, out);
        }
      }
    }
  }
  if (out && fclose(out)) return 2;
  printf("hash %016llx\n", (unsigned long long)h);
  dump_seq(&pbi->common.seq_params);
  dump_frame(pbi);
  const AV2_COMMON *cm = &pbi->common;
  for (int r = 0; r < cm->tiles.rows; r++)
    for (int c = 0; c < cm->tiles.cols; c++)
      printf("tile %d %d %td %zu\n", r, c, pbi->tile_buffers[r][c].data - buf, pbi->tile_buffers[r][c].size);
  // On a key frame every tile copies cm->fc after av2_setup_past_independence,
  // which leaves the same values in cm->default_frame_context. cm->fc itself
  // holds the adapted CDFs once decoding ends, so it is not dumped.
  const uint8_t *fc = (const uint8_t *)cm->default_frame_context;
  printf("frame_context ");
  for (size_t i = 0; i < sizeof(FRAME_CONTEXT); i++) printf("%02x", fc[i]);
  printf("\n");
  avm_codec_destroy(&ctx);
  free(buf);
  return 0;
}
