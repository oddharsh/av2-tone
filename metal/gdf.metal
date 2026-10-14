// gdf.metal: AV2's GDF inference (gdf_inference_unit_c in AVM 1.0.0
// av2/common/gdf_block.c) as one Metal thread per pixel, frame-wide.
//
// The C kernel walks one 64-row unit at a time and keeps a per-row
// accumulator array; nothing in it carries state between pixels, so a frame
// is one dispatch here. Lap rows advance every second pixel row and cls is
// one entry per 2x2, both indexed from the unit's top-left, so frame-wide
// indexing matches the C as long as units start on even rows and columns
// (they start at multiples of 64).
//
// Integer semantics follow the C exactly: int is 32-bit and wraps in both,
// right shifts of negatives never happen (the C negates first), and the
// int16 rec samples are read as signed because the C casts rec_y to
// const int16_t *.
#include <metal_stdlib>
using namespace metal;

constant int REC_NUM = 18;
constant int GRD_NUM = 4;
constant int INP_TOT = REC_NUM + GRD_NUM;
constant int CLS_NUM = 4;
constant int LUT_IDX_NUM = 3;
constant int GRD_SHIFT = 4;
constant int INP_PREC = 10;
// GDF_TEST_INP_PREC - GDF_TRAIN_INP_PREC + GDF_TRAIN_PAR_SCALE_LOG2
constant int NORM_SHIFT = 10 - 0 + 5;

constant short2 FWD[REC_NUM] = {
  {-6, 0}, {-5, 0}, {-4, 0}, {-3, 0},
  {-2, -1}, {-2, 0}, {-2, 1},
  {-1, -2}, {-1, -1}, {-1, 0}, {-1, 1}, {-1, 2},
  {0, -6}, {0, -5}, {0, -4}, {0, -3}, {0, -2}, {0, -1},
};
// bwd is fwd mirrored through the centre pixel.

struct GdfParams {
  uint width, height;    // frame region filtered
  uint rec_stride;       // in samples; rec points at (0, 0) of the region
  uint lap_stride;       // in uint16, per lap row (one lap row per 2 rows)
  uint cls_stride;       // in uint32, per cls row
  uint err_stride;       // in int16
  int pxl_shift;         // GDF_TEST_INP_PREC - min(bit_depth, 10)
  int is_intra;
  // compensation, for the fused kernel
  int err_shift, scale, pxl_max;
};

struct GdfTables {
  short alpha[CLS_NUM * INP_TOT];
  short weight[CLS_NUM * INP_TOT * LUT_IDX_NUM];
  int bias[CLS_NUM * LUT_IDX_NUM];
};

static inline int gdf_err(uint2 p, const device short *rec,
                          const device ushort *lap, const device uint *cls,
                          constant GdfParams &P, constant GdfTables &T,
                          const device char *lut) {
  const int x = int(p.x), y = int(p.y);
  const int rs = int(P.rec_stride);
  const device short *c = rec + y * rs + x;
  const int centre = c[0];
  const int cls_idx = int(cls[(y >> 1) * P.cls_stride + (x >> 1)]);
  const int lim = 1 << (INP_PREC - 1);

  int acc0 = 0, acc1 = 0, acc2 = 0;
  for (int k = 0; k < INP_TOT; k++) {
    const int off = k * CLS_NUM + cls_idx;
    const int a = T.alpha[off];
    int inp;
    if (k < REC_NUM) {
      const int dy = FWD[k].x, dx = FWD[k].y;
      const int f = (int(c[dy * rs + dx]) - centre) * (1 << P.pxl_shift);
      const int b = (int(c[-dy * rs - dx]) - centre) * (1 << P.pxl_shift);
      inp = clamp(f, -a, a) + clamp(b, -a, a);
    } else {
      // The four lap planes sit back to back, plane_size apart (see host).
      const uint plane_size = P.lap_stride * ((P.height + 1) >> 1);
      const int g = int(lap[(k - REC_NUM) * plane_size +
                            (y >> 1) * P.lap_stride + x]);
      inp = clamp((g << P.pxl_shift) >> GRD_SHIFT, -a, a);
    }
    inp = clamp(inp, -lim, lim - 1);
    acc0 += inp * int(T.weight[off]);
    acc1 += inp * int(T.weight[off + INP_TOT * CLS_NUM]);
    acc2 += inp * int(T.weight[off + 2 * INP_TOT * CLS_NUM]);
  }

  const int fmax = P.is_intra ? 16 : 10;
  const int idx_min = -(fmax >> 1);
  const int idx_max = fmax - 1 + idx_min;
  const int idx_scale = max(-idx_min, idx_max);
  const int round_half = 1 << (NORM_SHIFT - 1);
  int acc[3] = { acc0 + T.bias[cls_idx], acc1 + T.bias[cls_idx + CLS_NUM],
                 acc2 + T.bias[cls_idx + 2 * CLS_NUM] };
  int lut_off = 0;
  int mul = fmax * fmax;
  for (int i = 0; i < LUT_IDX_NUM; i++) {
    const int v = acc[i];
    const int n = (v > 0 ? ((idx_scale * v + round_half) >> NORM_SHIFT)
                         : -((idx_scale * (-v) + round_half) >> NORM_SHIFT)) -
                  idx_min;
    lut_off += clamp(n, 0, fmax - 1) * mul;
    mul /= fmax;
  }
  return int(lut[lut_off]);
}

// gdf_set_lap_and_cls_unit_c, one thread per 2x2 cell. The C builds each lap
// value with two rolling sums (pairs of 2x2 cells across, then down), which
// unrolls to a plain window: lap(L, P) sums each direction's |2c - a - b|
// over pixel rows 2L-1..2L+2 and columns 2P-1..2P+2. The C anchors that window
// at (i_min - 1, j_min - 1) with i_min and j_min even, so it is the same
// window frame-wide and the unit grid drops out.
kernel void gdf_lap_cls(const device short *rec [[buffer(0)]],
                        device ushort *lap [[buffer(1)]],
                        device uint *cls [[buffer(2)]],
                        constant GdfParams &P [[buffer(4)]],
                        uint2 q [[thread_position_in_grid]]) {
  const uint cw = (P.width + 1) >> 1, ch = (P.height + 1) >> 1;
  if (q.x >= cw || q.y >= ch) return;
  const int rs = int(P.rec_stride);
  const int y0 = 2 * int(q.y) - 1, x0 = 2 * int(q.x) - 1;
  uint v = 0, h = 0, d0 = 0, d1 = 0;
  for (int dy = 0; dy < 4; dy++) {
    const device short *r = rec + (y0 + dy) * rs + x0;
    for (int dx = 0; dx < 4; dx++) {
      const int c = int(r[dx]) << 1;
      v += uint(abs(c - r[dx - rs] - r[dx + rs]));
      h += uint(abs(c - r[dx - 1] - r[dx + 1]));
      d0 += uint(abs(c - r[dx - rs - 1] - r[dx + rs + 1]));
      d1 += uint(abs(c - r[dx - rs + 1] - r[dx + rs - 1]));
    }
  }
  const uint mask = (1u << (16 - P.pxl_shift)) - 1;
  v &= mask; h &= mask; d0 &= mask; d1 &= mask;
  cls[q.y * P.cls_stride + q.x] = (v > h ? 0u : 1u) | (d0 > d1 ? 0u : 2u);
  const uint plane = P.lap_stride * ch;
  const uint at = q.y * P.lap_stride + 2 * q.x;
  // Both pixels of the pair read the same value, as the C's (x | x << 16).
  lap[at] = lap[at + 1] = ushort(v);
  lap[plane + at] = lap[plane + at + 1] = ushort(h);
  lap[2 * plane + at] = lap[2 * plane + at + 1] = ushort(d0);
  lap[3 * plane + at] = lap[3 * plane + at + 1] = ushort(d1);
}

// Inference only: writes the expected coding error, as gdf_inference_unit_c.
kernel void gdf_inference(const device short *rec [[buffer(0)]],
                          const device ushort *lap [[buffer(1)]],
                          const device uint *cls [[buffer(2)]],
                          device short *err [[buffer(3)]],
                          constant GdfParams &P [[buffer(4)]],
                          constant GdfTables &T [[buffer(5)]],
                          const device char *lut [[buffer(6)]],
                          uint2 p [[thread_position_in_grid]]) {
  if (p.x >= P.width || p.y >= P.height) return;
  err[p.y * P.err_stride + p.x] = short(gdf_err(p, rec, lap, cls, P, T, lut));
}

// Inference fused with gdf_compensation_unit_c: reads the guided copy, writes
// the filtered frame. Safe to fuse because the C reads inp_ptr (the guided
// copy) and writes cur_frame, two different buffers.
kernel void gdf_fused(const device short *rec [[buffer(0)]],
                      const device ushort *lap [[buffer(1)]],
                      const device uint *cls [[buffer(2)]],
                      device ushort *out [[buffer(3)]],
                      constant GdfParams &P [[buffer(4)]],
                      constant GdfTables &T [[buffer(5)]],
                      const device char *lut [[buffer(6)]],
                      uint2 p [[thread_position_in_grid]]) {
  if (p.x >= P.width || p.y >= P.height) return;
  const int e = gdf_err(p, rec, lap, cls, P, T, lut);
  // int16_t res_pxl = scale * err, in the C: keep the int16 wrap.
  int r = int(short(P.scale * e));
  const int h = P.err_shift > 0 ? 1 << (P.err_shift - 1) : 0;
  r = r > 0 ? (r + h) >> P.err_shift : -(((-r) + h) >> P.err_shift);
  r = int(short(r));
  const int v = int(rec[p.y * P.rec_stride + p.x]);
  out[p.y * P.err_stride + p.x] = ushort(clamp(r + v, 0, P.pxl_max));
}
