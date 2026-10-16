// av2bench.c: decode one AV2 OBU file REPS times with THREADS decoder threads.
// Prints the median and minimum decode time, and an FNV-1a hash of the decoded
// planes, byte for byte as the decoder holds them, so two builds can be
// compared bit for bit. Every decode is hashed; if any differs from the rest
// the line ends in UNSTABLE and the exit status is 1. build.sh links it
// against each libavm.a.
//   av2bench <file.obu> <reps> <threads>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#include "avm/avm_decoder.h"
#include "avm/avmdx.h"

static double now_ms(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static int cmp(const void *a, const void *b) {
  const double d = *(const double *)a - *(const double *)b;
  return d < 0 ? -1 : d > 0;
}

static uint64_t plane_hash(const avm_image_t *img) {
  uint64_t h = 1469598103934665603ULL;
  const int bps = (img->fmt & AVM_IMG_FMT_HIGHBITDEPTH) ? 2 : 1;
  for (int p = 0; p < (img->monochrome ? 1 : 3); p++) {
    const int w = p ? (int)((img->d_w + img->x_chroma_shift) >> img->x_chroma_shift) : (int)img->d_w;
    const int ph = p ? (int)((img->d_h + img->y_chroma_shift) >> img->y_chroma_shift) : (int)img->d_h;
    for (int y = 0; y < ph; y++) {
      const uint8_t *row = img->planes[p] + (size_t)y * img->stride[p];
      for (int x = 0; x < w * bps; x++) {
        h ^= row[x];
        h *= 1099511628211ULL;
      }
    }
  }
  return h;
}

int main(int argc, char **argv) {
  if (argc < 4) {
    fprintf(stderr, "usage: av2bench <file.obu> <reps> <threads>\n");
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
  for (int r = -2; r < reps; r++) {  // two warm-up decodes
    avm_codec_ctx_t ctx;
    avm_codec_dec_cfg_t cfg = { 0 };
    cfg.threads = threads;
    if (avm_codec_dec_init(&ctx, avm_codec_av2_dx(), &cfg, 0)) return 1;
    // AVM turns row multithreading off by default: each tile is then parsed
    // and reconstructed by one thread. AV2BENCH_ROW_MT=1 turns it on.
    if (getenv("AV2BENCH_ROW_MT")) avm_codec_control(&ctx, AV2D_SET_ROW_MT, 1u);
    const double t0 = now_ms();
    if (avm_codec_decode(&ctx, b, n, NULL)) {
      fprintf(stderr, "decode failed\n");
      return 1;
    }
    avm_codec_iter_t it = NULL;
    avm_image_t *img = avm_codec_get_frame(&ctx, &it);
    const double t1 = now_ms();
    if (!img) return 1;
    if (r >= 0) t[r] = t1 - t0;
    // Every decode is hashed, outside the timed span: a race in a threaded
    // stage shows up as one decode that differs from the others.
    const uint64_t h = plane_hash(img);
    if (r > -2 && h != hash) unstable = 1;
    hash = h;
    avm_codec_destroy(&ctx);
  }
  qsort(t, reps, sizeof(double), cmp);
  printf("hash %016llx threads=%d median %.1f ms min %.1f ms%s\n", (unsigned long long)hash, threads,
         t[reps / 2], t[0], unstable ? " UNSTABLE" : "");
  return unstable;
}
