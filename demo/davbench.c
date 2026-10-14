// davbench.c: decode one OBU file REPS times with dav1d (AV1) or dav2d (AV2),
// in process, so a CLI's startup and file muxing never land in the timing.
// The output line matches fast-decode's av2bench, hash included: the planes
// are hashed byte for byte (FNV-1a) the way AVM holds them, so a dav2d decode
// can be compared bit for bit against AVM's. build.sh compiles this twice,
// once with -DDAV=1 against dav1d and once with -DDAV=2 against dav2d.
//   davbench <file.obu> <reps> <threads>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#if DAV == 2
#include <dav2d/dav2d.h>
typedef Dav2dContext Ctx;
typedef Dav2dSettings Settings;
typedef Dav2dData Data;
typedef Dav2dPicture Pic;
#define dav(f) dav2d_##f
#define LAYOUT_I400 DAV2D_PIXEL_LAYOUT_I400
#define LAYOUT_I420 DAV2D_PIXEL_LAYOUT_I420
#define LAYOUT_I422 DAV2D_PIXEL_LAYOUT_I422
#else
#include <dav1d/dav1d.h>
typedef Dav1dContext Ctx;
typedef Dav1dSettings Settings;
typedef Dav1dData Data;
typedef Dav1dPicture Pic;
#define dav(f) dav1d_##f
#define LAYOUT_I400 DAV1D_PIXEL_LAYOUT_I400
#define LAYOUT_I420 DAV1D_PIXEL_LAYOUT_I420
#define LAYOUT_I422 DAV1D_PIXEL_LAYOUT_I422
#endif

static double now_ms(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static int cmp(const void *a, const void *b) {
  const double d = *(const double *)a - *(const double *)b;
  return d < 0 ? -1 : d > 0;
}

static uint64_t plane_hash(const Pic *pic) {
  uint64_t h = 1469598103934665603ULL;
  const int bps = pic->p.bpc > 8 ? 2 : 1;
  const int xs = pic->p.layout == LAYOUT_I420 || pic->p.layout == LAYOUT_I422;
  const int ys = pic->p.layout == LAYOUT_I420;
  for (int p = 0; p < (pic->p.layout == LAYOUT_I400 ? 1 : 3); p++) {
    const int w = p ? (pic->p.w + xs) >> xs : pic->p.w;
    const int ph = p ? (pic->p.h + ys) >> ys : pic->p.h;
    for (int y = 0; y < ph; y++) {
      const uint8_t *row = (const uint8_t *)pic->data[p] + (ptrdiff_t)y * pic->stride[p ? 1 : 0];
      for (int x = 0; x < w * bps; x++) {
        h ^= row[x];
        h *= 1099511628211ULL;
      }
    }
  }
  return h;
}

static void nofree(const uint8_t *buf, void *cookie) {
  (void)buf;
  (void)cookie;
}

int main(int argc, char **argv) {
  if (argc < 4) {
    fprintf(stderr, "usage: davbench <file.obu> <reps> <threads>\n");
    return 2;
  }
  FILE *f = fopen(argv[1], "rb");
  if (!f) return 2;
  fseek(f, 0, SEEK_END);
  const long n = ftell(f);
  rewind(f);
  uint8_t *b = malloc(n);
  if (!b || fread(b, 1, n, f) != (size_t)n) return 2;
  fclose(f);
  const int reps = atoi(argv[2]), threads = atoi(argv[3]);
  if (reps < 1) return 2;
  double *t = malloc(sizeof(double) * reps);
  uint64_t hash = 0;
  int unstable = 0;
  for (int r = -2; r < reps; r++) {  // two warm-up decodes, as av2bench does
    Settings s;
    dav(default_settings)(&s);
    s.n_threads = threads;
    s.max_frame_delay = 1;  // a still: never wait for frames that won't come
    Ctx *c = NULL;
    if (dav(open)(&c, &s)) return 1;
    Data d = { 0 };
    if (dav(data_wrap)(&d, b, n, nofree, NULL)) return 1;
    Pic pic = { 0 };
    const double t0 = now_ms();
    int got = 0;
    while (!got) {
      if (d.sz) {
        const int e = dav(send_data)(c, &d);
        if (e && e != -EAGAIN) {
          fprintf(stderr, "decode failed (%d)\n", e);
          return 1;
        }
      }
      const int e = dav(get_picture)(c, &pic);
      if (!e) got = 1;
      else if (e != -EAGAIN) {
        fprintf(stderr, "no picture (%d)\n", e);
        return 1;
      } else if (!d.sz) {
        fprintf(stderr, "decoder drained without a picture\n");
        return 1;
      }
    }
    const double t1 = now_ms();
    if (r >= 0) t[r] = t1 - t0;
    const uint64_t h = plane_hash(&pic);
    if (r > -2 && h != hash) unstable = 1;
    hash = h;
    dav(picture_unref)(&pic);
    dav(close)(&c);
  }
  qsort(t, reps, sizeof(double), cmp);
  printf("hash %016llx threads=%d median %.1f ms min %.1f ms%s\n", (unsigned long long)hash, threads,
         t[reps / 2], t[0], unstable ? " UNSTABLE" : "");
  return unstable;
}
