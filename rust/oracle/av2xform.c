// av2xform.c: every transform block pristine AVM reconstructs, for the Rust
// decoder's reconstruction to be diffed against one block at a time.
//
// The linker's --wrap puts this file's __wrap_av2_inverse_transform_block in
// front of AVM's own (idct.c), which decodeframe.c calls once per transform
// block with coefficients, after dequantization and AV2's cross-chroma
// transform (CCTX) and before the secondary transform (IST). AVM itself is
// linked as built, untouched. For each call it prints
//
//   txb <plane> <x> <y> <tx_size> <tx_type> <eob> <use_ddt> <reduced_tx_set>
//   dq <count> <index>:<value> ...      the nonzero dequantized coefficients,
//                                       index into the tx_size_2d buffer AVM
//                                       hands the inverse transform
//   pred <w*h samples>                  the destination before the call: the
//                                       block's prediction
//   recon <w*h samples>                 the destination after it
//
// x and y are the block's position in its plane, in samples. A block whose
// eob is 0 is still reported; AVM's own function returns without touching
// the destination for it.
//
//   av2xform <file.obu>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#include "avm/avm_decoder.h"
#include "avm/avmdx.h"
#include "av2/common/av2_common_int.h"
#include "av2/common/blockd.h"
#include "av2/common/idct.h"

void __real_av2_inverse_transform_block(const MACROBLOCKD *xd, const tran_low_t *dqcoeff, int plane,
                                        TX_TYPE tx_type, TX_SIZE tx_size, uint16_t *dst, int stride, int eob,
                                        int use_ddt, int reduced_tx_set);

static void samples(const char *key, const uint16_t *dst, int stride, int w, int h) {
  printf("%s", key);
  for (int y = 0; y < h; y++)
    for (int x = 0; x < w; x++) printf(" %u", dst[y * stride + x]);
  printf("\n");
}

void __wrap_av2_inverse_transform_block(const MACROBLOCKD *xd, const tran_low_t *dqcoeff, int plane,
                                        TX_TYPE tx_type, TX_SIZE tx_size, uint16_t *dst, int stride, int eob,
                                        int use_ddt, int reduced_tx_set) {
  const YV12_BUFFER_CONFIG *buf = xd->cur_buf;
  const ptrdiff_t off = dst - buf->buffers[plane];
  if (stride != buf->strides[plane > 0] || off < 0) {
    fprintf(stderr, "a transform block outside the frame buffer\n");
    exit(1);
  }
  const int w = tx_size_wide[tx_size], h = tx_size_high[tx_size];
  printf("txb %d %d %d %d %d %d %d %d\n", plane, (int)(off % stride), (int)(off / stride), (int)tx_size,
         (int)tx_type, eob, use_ddt, reduced_tx_set);
  int count = 0;
  for (int i = 0; i < tx_size_2d[tx_size]; i++) count += dqcoeff[i] != 0;
  printf("dq %d", count);
  for (int i = 0; i < tx_size_2d[tx_size]; i++)
    if (dqcoeff[i]) printf(" %d:%d", i, dqcoeff[i]);
  printf("\n");
  samples("pred", dst, stride, w, h);
  __real_av2_inverse_transform_block(xd, dqcoeff, plane, tx_type, tx_size, dst, stride, eob, use_ddt,
                                     reduced_tx_set);
  samples("recon", dst, stride, w, h);
}

int main(int argc, char **argv) {
  if (argc != 2) {
    fprintf(stderr, "usage: av2xform <file.obu>\n");
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
  uint8_t *data = malloc(n > 0 ? (size_t)n : 1);
  if (!data || fread(data, 1, (size_t)n, in) != (size_t)n) return 2;
  fclose(in);

  avm_codec_ctx_t ctx;
  avm_codec_dec_cfg_t cfg = { 0 };
  cfg.threads = 1;
  if (avm_codec_dec_init(&ctx, avm_codec_av2_dx(), &cfg, 0)) return 1;
  if (avm_codec_decode(&ctx, data, (size_t)n, NULL)) {
    fprintf(stderr, "AVM: %s\n", avm_codec_error_detail(&ctx) ? avm_codec_error_detail(&ctx) : "decode failed");
    return 1;
  }
  avm_codec_iter_t it = NULL;
  if (!avm_codec_get_frame(&ctx, &it)) {
    avm_codec_decode(&ctx, NULL, 0, NULL);
    it = NULL;
    if (!avm_codec_get_frame(&ctx, &it)) {
      fprintf(stderr, "AVM output no frame\n");
      return 1;
    }
  }
  avm_codec_destroy(&ctx);
  return 0;
}
