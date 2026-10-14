// gdfbench.m: AV2 GDF on the Apple GPU, checked against AVM's own C kernels.
//
// For each 10-bit 4:4:4 .y4m (luma only: GDF filters Y), this walks the same
// unit grid as gdf_filter_frame (64-row units starting at -GDF_TEST_STRIPE_OFF,
// 64-column units), runs gdf_set_lap_and_cls_unit_c, gdf_inference_unit_c and
// gdf_compensation_unit_c per unit as the oracle, then runs gdf.metal over the
// whole frame in one dispatch and requires every pixel to match, for every
// qp index and every reference-distance table (intra and the five inter).
//
// Then it times one frame built by tiling the inputs to --size (default
// 1600x2400, the decoder session's benchmark frame): the C kernels on one
// core, and the GPU, both as GPU execution time and as wall time from encode
// to completion. It leaves the stripe boundary line substitution
// (gdf_setup_reference_lines) to the host, as the decoder does today.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "av2/common/gdf_block.h"

enum { B = 8, UNIT = GDF_TEST_STRIPE_SIZE, LAP_S = UNIT + GDF_ERR_STRIDE_MARGIN,
       CLS_S = (UNIT >> 1) + GDF_ERR_STRIDE_MARGIN, ERR_S = UNIT + GDF_ERR_STRIDE_MARGIN };

typedef struct {
  uint32_t width, height, rec_stride, lap_stride, cls_stride, err_stride;
  int32_t pxl_shift, is_intra, err_shift, scale, pxl_max;
} GdfParams;

typedef struct {
  int16_t alpha[GDF_TRAIN_CLS_NUM * GDF_OPTS_INP_TOT];
  int16_t weight[GDF_TRAIN_CLS_NUM * GDF_OPTS_INP_TOT * GDF_NET_LUT_IDX_NUM];
  int32_t bias[GDF_TRAIN_CLS_NUM * GDF_NET_LUT_IDX_NUM];
} GdfTables;

typedef struct {
  int w, h, stride;   // stride of the padded plane, in samples
  uint16_t *pad;      // (h + 2B) x stride, edge-replicated
  uint16_t *rec;      // pad + B * stride + B
} Plane;

typedef struct {
  int w, h, lap_stride, cls_stride;
  uint16_t *lap;      // 4 planes, lap_stride x ((h + 1) >> 1) each
  uint32_t *cls;
  int16_t *err;       // w x h
  uint16_t *out;      // w x h, after compensation
} Ref;

static double now_ms(void) {
  static mach_timebase_info_data_t tb;
  if (!tb.denom) mach_timebase_info(&tb);
  return (double)mach_absolute_time() * tb.numer / tb.denom / 1e6;
}

static Plane plane_new(int w, int h) {
  Plane p = { w, h, w + 2 * B, NULL, NULL };
  p.pad = calloc((size_t)(h + 2 * B) * p.stride, 2);
  p.rec = p.pad + B * p.stride + B;
  return p;
}

static void plane_extend(Plane *p) {
  for (int y = 0; y < p->h; y++) {
    uint16_t *r = p->rec + y * p->stride;
    for (int x = 1; x <= B; x++) { r[-x] = r[0]; r[p->w - 1 + x] = r[p->w - 1]; }
  }
  for (int y = 1; y <= B; y++) {
    memcpy(p->rec - B - y * p->stride, p->rec - B, p->stride * 2);
    memcpy(p->rec - B + (p->h - 1 + y) * p->stride, p->rec - B + (p->h - 1) * p->stride, p->stride * 2);
  }
}

static Plane read_y4m_luma(const char *path) {
  FILE *f = fopen(path, "rb");
  if (!f) { perror(path); exit(1); }
  char hdr[256];
  if (!fgets(hdr, sizeof hdr, f) || strncmp(hdr, "YUV4MPEG2", 9)) { fprintf(stderr, "%s: not y4m\n", path); exit(1); }
  int w = 0, h = 0;
  for (char *t = strtok(hdr, " \n"); t; t = strtok(NULL, " \n")) {
    if (t[0] == 'W') w = atoi(t + 1);
    if (t[0] == 'H') h = atoi(t + 1);
    if (t[0] == 'C' && !strstr(t, "p10")) { fprintf(stderr, "%s: need 10-bit, got %s\n", path, t); exit(1); }
  }
  char fr[64];
  if (!fgets(fr, sizeof fr, f)) { fprintf(stderr, "%s: no frame\n", path); exit(1); }
  Plane p = plane_new(w, h);
  for (int y = 0; y < h; y++)
    if (fread(p.rec + y * p.stride, 2, w, f) != (size_t)w) { fprintf(stderr, "%s: short\n", path); exit(1); }
  fclose(f);
  plane_extend(&p);
  return p;
}

static void tables_for(int ref_dst, int qp, GdfTables *t, const int8_t **lut, size_t *lut_n) {
  if (ref_dst == 0) {
    memcpy(t->alpha, gdf_intra_alpha_table[qp], sizeof t->alpha);
    memcpy(t->weight, gdf_intra_weight_table[qp], sizeof t->weight);
    memcpy(t->bias, gdf_intra_bias_table[qp], sizeof t->bias);
    *lut = gdf_intra_error_table[qp];
    *lut_n = sizeof gdf_intra_error_table[qp];
  } else {
    memcpy(t->alpha, gdf_inter_alpha_table[ref_dst - 1][qp], sizeof t->alpha);
    memcpy(t->weight, gdf_inter_weight_table[ref_dst - 1][qp], sizeof t->weight);
    memcpy(t->bias, gdf_inter_bias_table[ref_dst - 1][qp], sizeof t->bias);
    *lut = gdf_inter_error_table[ref_dst - 1][qp];
    *lut_n = sizeof gdf_inter_error_table[ref_dst - 1][qp];
  }
}

static Ref ref_new(int w, int h) {
  Ref r = { w, h, (w + 1) & ~1, (w + 1) >> 1, NULL, NULL, NULL, NULL };
  r.lap = calloc((size_t)4 * r.lap_stride * ((h + 1) >> 1), 2);
  r.cls = calloc((size_t)r.cls_stride * ((h + 1) >> 1), 4);
  r.err = calloc((size_t)w * h, 2);
  r.out = calloc((size_t)w * h, 2);
  return r;
}

// The oracle, unit by unit, exactly as gdf_filter_frame calls the C kernels.
// t_lap/t_inf/t_cmp accumulate the time of each stage.
static void run_c(const Plane *p, int ref_dst, int qp, int scale, Ref *r,
                  double *t_lap, double *t_inf, double *t_cmp) {
  static uint16_t lap_s[4][(UNIT / 2 + 2) * LAP_S];
  static uint32_t cls_s[(UNIT / 2 + 2) * CLS_S];
  static int16_t err_s[UNIT * ERR_S];
  uint16_t *lap_ptr[4] = { lap_s[0], lap_s[1], lap_s[2], lap_s[3] };
  const int bit_depth = 10, pxl_max = 1023, pxl_shift = 0;
  const int err_shift = GDF_RDO_SCALE_NUM_LOG2 + GDF_TEST_INP_PREC - bit_depth;
  const size_t plane_n = (size_t)r->lap_stride * ((r->h + 1) >> 1);
  // gdf_compensation_unit_c writes in place; the oracle needs the unfiltered
  // frame for later units' guided reads, so it compensates a copy.
  for (int y = 0; y < p->h; y++) memcpy(r->out + y * p->w, p->rec + y * p->stride, p->w * 2);
  for (int v = -GDF_TEST_STRIPE_OFF; v < p->h; v += UNIT) {
    const int i_min = v < 0 ? 0 : v, i_max = v + UNIT < p->h ? v + UNIT : p->h;
    for (int u = 0; u < p->w; u += UNIT) {
      const int j_min = u, j_max = u + UNIT < p->w ? u + UNIT : p->w;
      double t0 = now_ms();
      gdf_set_lap_and_cls_unit_c(i_min, i_max, j_min, j_max,
                                 p->rec + (i_min - 1) * p->stride + (j_min - 1), p->stride,
                                 bit_depth, lap_ptr, LAP_S, cls_s, CLS_S);
      double t1 = now_ms();
      gdf_inference_unit_c(i_min, i_max, j_min, j_max, qp, p->rec + i_min * p->stride + j_min,
                           p->stride, lap_ptr, LAP_S, cls_s, CLS_S, err_s, ERR_S, pxl_shift, ref_dst);
      double t2 = now_ms();
      gdf_compensation_unit_c(r->out + i_min * p->w + j_min, p->w, err_s, ERR_S, err_shift, scale,
                              pxl_max, i_max - i_min, j_max - j_min);
      double t3 = now_ms();
      *t_lap += t1 - t0; *t_inf += t2 - t1; *t_cmp += t3 - t2;
      for (int y = 0; y < i_max - i_min; y++) {
        memcpy(r->err + (i_min + y) * r->w + j_min, err_s + y * ERR_S, (j_max - j_min) * 2);
        if (y & 1) continue;
        const int ly = (i_min + y) >> 1;
        for (int g = 0; g < 4; g++)
          memcpy(r->lap + g * plane_n + ly * r->lap_stride + j_min, lap_s[g] + (y >> 1) * LAP_S, (j_max - j_min) * 2);
        memcpy(r->cls + ly * r->cls_stride + (j_min >> 1), cls_s + (y >> 1) * CLS_S, ((j_max - j_min + 1) >> 1) * 4);
      }
    }
  }
}

typedef struct {
  id<MTLDevice> dev;
  id<MTLCommandQueue> q;
  id<MTLComputePipelineState> inf, fused, lapcls;
} Gpu;

static Gpu gpu_new(const char *shader_path) {
  Gpu g;
  g.dev = MTLCreateSystemDefaultDevice();
  if (!g.dev) { fprintf(stderr, "no Metal device\n"); exit(1); }
  NSError *e = nil;
  NSString *src = [NSString stringWithContentsOfFile:@(shader_path) encoding:NSUTF8StringEncoding error:&e];
  if (!src) { fprintf(stderr, "%s\n", e.description.UTF8String); exit(1); }
  MTLCompileOptions *o = [MTLCompileOptions new];
  o.mathMode = MTLMathModeSafe;
  id<MTLLibrary> lib = [g.dev newLibraryWithSource:src options:o error:&e];
  if (!lib) { fprintf(stderr, "metal compile: %s\n", e.description.UTF8String); exit(1); }
  g.inf = [g.dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"gdf_inference"] error:&e];
  g.fused = [g.dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"gdf_fused"] error:&e];
  g.lapcls = [g.dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"gdf_lap_cls"] error:&e];
  if (!g.inf || !g.fused || !g.lapcls) { fprintf(stderr, "pipeline: %s\n", e.description.UTF8String); exit(1); }
  g.q = [g.dev newCommandQueue];
  return g;
}

// lap/cls hold the C oracle's gradients; glap/gcls are the GPU's own.
typedef struct { id<MTLBuffer> rec, lap, cls, glap, gcls, dst, lut; size_t rec_off; } GpuBufs;

static GpuBufs gpu_bufs(Gpu *g, const Plane *p, const Ref *r) {
  GpuBufs b;
  const MTLResourceOptions sh = MTLResourceStorageModeShared;
  b.rec = [g->dev newBufferWithBytes:p->pad length:(size_t)(p->h + 2 * B) * p->stride * 2 options:sh];
  b.rec_off = (size_t)(B * p->stride + B) * 2;
  b.lap = [g->dev newBufferWithBytes:r->lap length:(size_t)4 * r->lap_stride * ((r->h + 1) >> 1) * 2 options:sh];
  b.cls = [g->dev newBufferWithBytes:r->cls length:(size_t)r->cls_stride * ((r->h + 1) >> 1) * 4 options:sh];
  b.glap = [g->dev newBufferWithLength:b.lap.length options:sh];
  b.gcls = [g->dev newBufferWithLength:b.cls.length options:sh];
  b.dst = [g->dev newBufferWithLength:(size_t)r->w * r->h * 2 options:sh];
  b.lut = [g->dev newBufferWithLength:4096 options:sh];
  return b;
}

// Encodes, commits and waits for one dispatch. Returns GPU execution time in
// ms; *wall gets encode-to-completion.
enum { INFERENCE, FUSED, ALL_GPU };
static double gpu_run(Gpu *g, GpuBufs *b, const Plane *p, const Ref *r, int mode,
                      int ref_dst, int qp, int scale, double *wall) {
  GdfTables t; const int8_t *lut; size_t lut_n;
  tables_for(ref_dst, qp, &t, &lut, &lut_n);
  memcpy(b->lut.contents, lut, lut_n);
  GdfParams P = { (uint32_t)p->w, (uint32_t)p->h, (uint32_t)p->stride, (uint32_t)r->lap_stride,
                  (uint32_t)r->cls_stride, (uint32_t)r->w, 0, ref_dst == 0,
                  GDF_RDO_SCALE_NUM_LOG2 + GDF_TEST_INP_PREC - 10, scale, 1023 };
  double t0 = now_ms();
  id<MTLCommandBuffer> cb = [g->q commandBuffer];
  id<MTLComputeCommandEncoder> enc = [cb computeCommandEncoder];
  id<MTLBuffer> lap = mode == ALL_GPU ? b->glap : b->lap, cls = mode == ALL_GPU ? b->gcls : b->cls;
  [enc setBuffer:b->rec offset:b->rec_off atIndex:0];
  [enc setBuffer:lap offset:0 atIndex:1];
  [enc setBuffer:cls offset:0 atIndex:2];
  [enc setBytes:&P length:sizeof P atIndex:4];
  if (mode == ALL_GPU) {
    // Same encoder, serial dispatch: Metal's hazard tracking orders the
    // inference reads after the lap/cls writes.
    id<MTLComputePipelineState> lc = g->lapcls;
    [enc setComputePipelineState:lc];
    const NSUInteger lw = lc.threadExecutionWidth, lh = lc.maxTotalThreadsPerThreadgroup / lw;
    [enc dispatchThreads:MTLSizeMake((p->w + 1) >> 1, (p->h + 1) >> 1, 1) threadsPerThreadgroup:MTLSizeMake(lw, lh, 1)];
  }
  id<MTLComputePipelineState> ps = mode == INFERENCE ? g->inf : g->fused;
  [enc setComputePipelineState:ps];
  [enc setBuffer:b->dst offset:0 atIndex:3];
  [enc setBytes:&P length:sizeof P atIndex:4];
  [enc setBytes:&t length:sizeof t atIndex:5];
  [enc setBuffer:b->lut offset:0 atIndex:6];
  const NSUInteger tw = ps.threadExecutionWidth, th = ps.maxTotalThreadsPerThreadgroup / tw;
  [enc dispatchThreads:MTLSizeMake(p->w, p->h, 1) threadsPerThreadgroup:MTLSizeMake(tw, th, 1)];
  [enc endEncoding];
  [cb commit];
  [cb waitUntilCompleted];
  if (wall) *wall = now_ms() - t0;
  if (cb.error) { fprintf(stderr, "gpu: %s\n", cb.error.description.UTF8String); exit(1); }
  return (cb.GPUEndTime - cb.GPUStartTime) * 1e3;
}

static int check(Gpu *g, const Plane *p, const char *name) {
  Ref r = ref_new(p->w, p->h);
  int bad = 0, combos = 0;
  for (int ref_dst = 0; ref_dst <= GDF_TRAIN_REFDST_NUM; ref_dst++)
    for (int qp = 0; qp < GDF_TRAIN_QP_NUM; qp++) {
      const int scale = 1 + (qp + ref_dst) % GDF_RDO_SCALE_NUM;  // gdf_pic_scale_idx + 1
      double a = 0, b2 = 0, c = 0;
      run_c(p, ref_dst, qp, scale, &r, &a, &b2, &c);
      GpuBufs b = gpu_bufs(g, p, &r);
      gpu_run(g, &b, p, &r, INFERENCE, ref_dst, qp, scale, NULL);
      const int16_t *ge = b.dst.contents;
      int diff = 0;
      for (size_t i = 0; i < (size_t)p->w * p->h; i++) diff += ge[i] != r.err[i];
      gpu_run(g, &b, p, &r, FUSED, ref_dst, qp, scale, NULL);
      const uint16_t *go = b.dst.contents;
      int diff_out = 0;
      for (size_t i = 0; i < (size_t)p->w * p->h; i++) diff_out += go[i] != r.out[i];
      // ALL_GPU: the GPU's own lap/cls, then fused; lap, cls and output must
      // all match the C.
      gpu_run(g, &b, p, &r, ALL_GPU, ref_dst, qp, scale, NULL);
      int diff_lap = 0, diff_cls = 0, diff_all = 0;
      const uint16_t *gl = b.glap.contents, *cl = b.lap.contents;
      for (size_t i = 0; i < b.lap.length / 2; i++) diff_lap += gl[i] != cl[i];
      const uint32_t *gc = b.gcls.contents, *cc = b.cls.contents;
      for (size_t i = 0; i < b.cls.length / 4; i++) diff_cls += gc[i] != cc[i];
      for (size_t i = 0; i < (size_t)p->w * p->h; i++) diff_all += go[i] != r.out[i];
      // A run where GDF changes nothing would pass vacuously; count the
      // pixels it moved so a dead filter shows up.
      int moved = 0, nonzero = 0;
      for (size_t i = 0; i < (size_t)p->w * p->h; i++) nonzero += r.err[i] != 0;
      for (int y = 0; y < p->h; y++)
        for (int x = 0; x < p->w; x++) moved += r.out[y * p->w + x] != p->rec[y * p->stride + x];
      combos++;
      if (diff || diff_out || diff_lap || diff_cls || diff_all) {
        bad++;
        printf("  MISMATCH %s ref_dst %d qp %d: err %d, out %d, lap %d, cls %d, all-GPU out %d\n", name, ref_dst,
               qp, diff, diff_out, diff_lap, diff_cls, diff_all);
      } else if (ref_dst == 0 && qp == 0) {
        printf("  %s: intra qp0 err nonzero on %d px, output moves %d of %d px\n", name, nonzero, moved, p->w * p->h);
      }
    }
  printf("%s %dx%d: %d/%d table sets bit-exact (err, output, and the all-GPU lap, cls and output)\n", name, p->w, p->h,
         combos - bad, combos);
  return bad;
}

int main(int argc, char **argv) {
  @autoreleasepool {
    const char *shader = NULL;
    int W = 1600, H = 2400, iters = 30, nin = 0;
    const char *in[16];
    for (int i = 1; i < argc; i++) {
      if (!strcmp(argv[i], "--shader")) shader = argv[++i];
      else if (!strcmp(argv[i], "--size")) sscanf(argv[++i], "%dx%d", &W, &H);
      else if (!strcmp(argv[i], "--iters")) iters = atoi(argv[++i]);
      else if (nin < 16) in[nin++] = argv[i];
    }
    if (!shader || !nin) { fprintf(stderr, "usage: gdfbench --shader gdf.metal [--size WxH] [--iters n] a.y4m ...\n"); return 2; }
    Gpu g = gpu_new(shader);
    printf("device: %s\n", g.dev.name.UTF8String);

    Plane src[16];
    int bad = 0;
    for (int i = 0; i < nin; i++) {
      src[i] = read_y4m_luma(in[i]);
      const char *base = strrchr(in[i], '/');
      bad += check(&g, &src[i], base ? base + 1 : in[i]);
    }

    // The timing frame: inputs tiled, cycling, at their native size.
    Plane f = plane_new(W, H);
    for (int y = 0; y < H; y++)
      for (int x = 0; x < W; x++) {
        const Plane *s = &src[((x / src[0].w) + (y / src[0].h)) % nin];
        f.rec[y * f.stride + x] = s->rec[(y % s->h) * s->stride + (x % s->w)];
      }
    plane_extend(&f);
    Ref r = ref_new(W, H);
    double best_lap = 1e9, best_inf = 1e9, best_cmp = 1e9;
    for (int it = 0; it < 7; it++) {
      double a = 0, b2 = 0, c = 0;
      run_c(&f, 0, 2, 2, &r, &a, &b2, &c);
      if (a < best_lap) best_lap = a;
      if (b2 < best_inf) best_inf = b2;
      if (c < best_cmp) best_cmp = c;
    }
    GpuBufs b = gpu_bufs(&g, &f, &r);
    double gi = 1e9, wi = 1e9, gf = 1e9, wf = 1e9, ga = 1e9, wa = 1e9, w;
    for (int it = 0; it < iters; it++) {
      double t = gpu_run(&g, &b, &f, &r, INFERENCE, 0, 2, 2, &w);
      if (t < gi) gi = t;
      if (w < wi) wi = w;
      t = gpu_run(&g, &b, &f, &r, FUSED, 0, 2, 2, &w);
      if (t < gf) gf = t;
      if (w < wf) wf = w;
      t = gpu_run(&g, &b, &f, &r, ALL_GPU, 0, 2, 2, &w);
      if (t < ga) ga = t;
      if (w < wa) wa = w;
    }
    printf("\ntiming frame %dx%d (luma), best of 7 CPU / %d GPU:\n", W, H, iters);
    printf("  C, one core:   lap+cls %.2f ms, inference %.2f ms, compensation %.2f ms, total %.2f ms\n",
           best_lap, best_inf, best_cmp, best_lap + best_inf + best_cmp);
    printf("  GPU inference: %.2f ms on the GPU, %.2f ms wall (encode to completion)\n", gi, wi);
    printf("  GPU fused:     %.2f ms on the GPU, %.2f ms wall (inference + compensation)\n", gf, wf);
    printf("  GPU, all of GDF: %.2f ms on the GPU, %.2f ms wall (lap+cls, inference, compensation)\n", ga, wa);
    return bad ? 1 : 0;
  }
}
