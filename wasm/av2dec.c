// av2dec.c: the smallest useful surface over AVM's reference decoder for a
// browser. JS parses the AVIF container, hands the av02 item's OBUs here, and
// gets back RGBA for a canvas plus the raw planes for bit-exact checks.
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "avm/avm_decoder.h"
#include "avm/avmdx.h"

static avm_codec_ctx_t ctx;
static int ctx_open = 0;
static avm_image_t *img = NULL;
static uint8_t *rgba = NULL;
static size_t rgba_cap = 0;

// Decode one temporal unit. Returns 0 on success, else an avm_codec_err_t.
int av2_decode(const uint8_t *data, size_t len) {
  if (ctx_open) avm_codec_destroy(&ctx);
  avm_codec_dec_cfg_t cfg = { 0 };
  cfg.threads = 1;
  if (avm_codec_dec_init(&ctx, avm_codec_av2_dx(), &cfg, 0)) return -1;
  ctx_open = 1;
  avm_codec_err_t err = avm_codec_decode(&ctx, data, len, NULL);
  if (err) return (int)err;
  // Take the first shown frame. A flush (decode NULL) before this call drops
  // the frame list, so only flush when the unit didn't output a frame itself.
  avm_codec_iter_t it = NULL;
  img = avm_codec_get_frame(&ctx, &it);
  if (!img) {
    avm_codec_decode(&ctx, NULL, 0, NULL);
    it = NULL;
    img = avm_codec_get_frame(&ctx, &it);
  }
  return img ? 0 : -2;
}

int av2_width(void) { return img ? (int)img->d_w : 0; }
int av2_height(void) { return img ? (int)img->d_h : 0; }
int av2_depth(void) { return img ? (int)img->bit_depth : 0; }
int av2_x_shift(void) { return img ? (int)img->x_chroma_shift : 0; }
int av2_y_shift(void) { return img ? (int)img->y_chroma_shift : 0; }
int av2_monochrome(void) { return img ? img->monochrome : 0; }
int av2_highbd(void) { return img ? !!(img->fmt & AVM_IMG_FMT_HIGHBITDEPTH) : 0; }
const uint8_t *av2_plane(int i) { return img ? img->planes[i] : NULL; }
int av2_stride(int i) { return img ? img->stride[i] : 0; }
// The in-band colour signalling; JS prefers the container's colr box.
int av2_matrix(void) { return img ? (int)img->mc : -1; }
int av2_full_range(void) { return img ? img->range == AVM_CR_FULL_RANGE : 0; }

static inline int sample(int p, int x, int y) {
  const uint8_t *row = img->planes[p] + (size_t)y * img->stride[p];
  return av2_highbd() ? ((const uint16_t *)row)[x] : row[x];
}

// Chroma at luma position (x, y). 4:4:4 reads straight through; subsampled
// chroma is bilinear with 3:1 weights, the same filter libavif's built-in
// path uses, so a viewer and avifdec agree on what a 4:2:0 file looks like.
static int chroma(int p, int x, int y) {
  const int sx = img->x_chroma_shift, sy = img->y_chroma_shift;
  if (!sx && !sy) return sample(p, x, y);
  const int cw = (int)((img->d_w + sx) >> sx), ch = (int)((img->d_h + sy) >> sy);
  int x0 = x >> sx, x1 = x0, y0 = y >> sy, y1 = y0;
  if (sx) x1 = (x & 1) ? x0 + 1 : x0 - 1;
  if (sy) y1 = (y & 1) ? y0 + 1 : y0 - 1;
  if (x1 < 0) x1 = 0;
  if (x1 >= cw) x1 = cw - 1;
  if (y1 < 0) y1 = 0;
  if (y1 >= ch) y1 = ch - 1;
  const int wx = sx ? 3 : 4, wy = sy ? 3 : 4;
  const int v = wx * wy * sample(p, x0, y0) + (4 - wx) * wy * sample(p, x1, y0) +
                wx * (4 - wy) * sample(p, x0, y1) +
                (4 - wx) * (4 - wy) * sample(p, x1, y1);
  return (v + 8) >> 4;
}

static inline uint8_t clamp8(double v) {
  return v <= 0 ? 0 : v >= 255 ? 255 : (uint8_t)(v + 0.5);
}

// YUV to 8-bit sRGB-encoded RGBA. mc is the CICP matrix (0 identity, 1 BT.709,
// 5/6 BT.601, 9 BT.2020 NCL); anything else falls back to BT.601.
const uint8_t *av2_rgba(int mc, int full_range) {
  if (!img) return NULL;
  const int w = (int)img->d_w, h = (int)img->d_h;
  const size_t need = (size_t)w * h * 4;
  if (need > rgba_cap) {
    free(rgba);
    rgba = malloc(need);
    rgba_cap = rgba ? need : 0;
    if (!rgba) return NULL;
  }
  double kr = 0.299, kb = 0.114;
  if (mc == 1) kr = 0.2126, kb = 0.0722;
  if (mc == 9) kr = 0.2627, kb = 0.0593;
  const double kg = 1 - kr - kb;
  const double maxv = (double)((1 << img->bit_depth) - 1);
  const double sc = (double)(1 << (img->bit_depth - 8));
  const double yoff = full_range ? 0 : 16 * sc;
  const double yrange = full_range ? maxv : 219 * sc;
  const double crange = full_range ? maxv : 224 * sc;
  const double mid = (double)(1 << (img->bit_depth - 1));
  const int mono = img->monochrome;
  uint8_t *o = rgba;
  for (int y = 0; y < h; y++) {
    for (int x = 0; x < w; x++, o += 4) {
      const double Y = (sample(0, x, y) - yoff) / yrange;
      double r, g, b;
      if (mono) {
        r = g = b = Y;
      } else if (mc == 0) {  // identity: planes are G, B, R
        g = Y;
        b = (chroma(1, x, y) - yoff) / yrange;
        r = (chroma(2, x, y) - yoff) / yrange;
      } else {
        const double Cb = (chroma(1, x, y) - mid) / crange;
        const double Cr = (chroma(2, x, y) - mid) / crange;
        r = Y + 2 * (1 - kr) * Cr;
        b = Y + 2 * (1 - kb) * Cb;
        g = (Y - kr * r - kb * b) / kg;
      }
      o[0] = clamp8(r * 255);
      o[1] = clamp8(g * 255);
      o[2] = clamp8(b * 255);
      o[3] = 255;
    }
  }
  return rgba;
}
