#!/usr/bin/env bash
# build.sh: a fast native AV2 decoder for this machine, next to a pristine one.
#
# WHAT IT IS. AVM 1.0.0 (the AV2 reference decoder, pinned by
# ../encoder/build.sh) built decoder-only with four accelerations, each
# bit-exact against the untouched build:
#   patches/0001  a NEON gdf_inference_unit. GDF, AV2's learned in-loop filter,
#                 was 71% of a single-threaded decode, and AVM has no ARM code
#                 for it.
#   patches/0002  the GDF frame pass across cores (Grand Central Dispatch), by
#                 unit row, packed into phases whose footprints don't overlap.
#   patches/0003  deblocking behind reconstruction: superblock rows are
#                 deblocked while the tiles below are still decoding, with an
#                 unfiltered shadow for IntraBC reads (AVM_LF_PIPELINE=0 off).
#   SIMDe         AVM's own AVX2/SSE kernels compiled for arm64 through
#                 ../encoder/simde.ts --decoder (113 dispatch entries).
#
# WHAT IT WRITES, under build/ (gitignored):
#   build/pristine/   decoder-only AVM, untouched   -> build/av2bench-pristine
#   build/fast/       the same plus the three above -> build/av2bench
# check.ts decodes a corpus with both and refuses any byte of difference.
#
# MEASURED on an M3 Max, a 1600x2400 10-bit 4:4:4 still with 4 tile rows,
# best of 41 with other jobs holding about 5 of the 14 cores (2026-10-14):
#   1 decoder thread, GDF serial    pristine 227 ms   fast 81 ms
#   8 decoder threads               pristine 182 ms   fast 18 ms
# On a 4:2:0 file dav2d also reads: dav2d 76.5 ms on one thread and 13.8 ms
# on 8; fast 55.7 ms (GDF serial) and 11.0 ms. Pristine numbers drift with
# load (227 to 314 ms on one thread across runs), so compare ratios.
#
#   ./build.sh    needs cmake, ninja, clang, bun, the AVM source from
#                 ../encoder/build.sh (or AVM_SRC=<dir>) and the SIMDe
#                 checkout from its --tuned build (or SIMDE=<dir>)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
AVM_SRC="${AVM_SRC:-$HERE/../encoder/build/_deps/libavm-src}"
SIMDE="${SIMDE:-$HERE/../encoder/build/simde}"
OUT="$HERE/build"

for cmd in cmake ninja clang bun rsync; do
  command -v "$cmd" >/dev/null || { echo "error: $cmd not found" >&2; exit 1; }
done
[ -f "$AVM_SRC/av2/av2_dx_iface.c" ] || {
  echo "error: no AVM source at $AVM_SRC; run ../encoder/build.sh or set AVM_SRC" >&2; exit 1; }
[ -f "$SIMDE/simde/x86/avx2.h" ] || {
  echo "error: no SIMDe checkout at $SIMDE; run ../encoder/build.sh --tuned or set SIMDE" >&2; exit 1; }

CMAKE_FLAGS=(-G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo -DCONFIG_AV2_ENCODER=0
  -DCONFIG_TENSORFLOW_LITE=0 -DENABLE_EXAMPLES=0 -DENABLE_TESTS=0 -DENABLE_TOOLS=0 -DENABLE_DOCS=0)

# Each build compiles its own copy, so a patch never touches AVM_SRC.
copy_src() {
  rm -rf "$1"
  mkdir -p "$1"
  rsync -a --exclude .git --exclude ParaKit "$AVM_SRC/" "$1/"
}

copy_src "$OUT/pristine/src"
cmake -S "$OUT/pristine/src" -B "$OUT/pristine/build" "${CMAKE_FLAGS[@]}" >/dev/null
ninja -C "$OUT/pristine/build" avm >/dev/null

copy_src "$OUT/fast/src"
# patch, never git apply: the copy sits inside this repository, and git apply
# run there resolves the patch's paths from the repository root, finds none of
# them, skips every hunk and exits 0. That shipped a "fast" build with no
# patches in it once (2026-10-13), so the two greps below refuse that.
for p in "$HERE"/patches/*.patch; do
  patch -p1 --forward --quiet -d "$OUT/fast/src" -i "$p"
done
grep -q "gdf_inference_unit_neon" "$OUT/fast/src/av2/common/arm/gdf_block_neon.c" &&
  grep -q "gdf_filter_frame_orig" "$OUT/fast/src/av2/common/gdf.c" &&
  grep -q "av2_lf_pipeline_start" "$OUT/fast/src/av2/decoder/decodeframe.c" || {
  echo "error: the patches did not land in $OUT/fast/src" >&2; exit 1; }
cmake -S "$OUT/fast/src" -B "$OUT/fast/build" "${CMAKE_FLAGS[@]}" >/dev/null
ninja -C "$OUT/fast/build" avm >/dev/null
rm -rf "$OUT/fast/simde-work"
bun "$HERE/../encoder/simde.ts" --decoder --src "$OUT/fast/src" --build "$OUT/fast/build" \
  --simde "$SIMDE" --work "$OUT/fast/simde-work"

clang -O2 -g "$HERE/av2bench.c" "$OUT/pristine/build/libavm.a" -I"$OUT/pristine/src" \
  -I"$OUT/pristine/build" -o "$OUT/av2bench-pristine" -lm -lpthread
clang -O2 -g "$HERE/av2bench.c" "$OUT/fast/build/libavm.a" -I"$OUT/fast/src" \
  -I"$OUT/fast/build" -o "$OUT/av2bench" -lm -lpthread
echo "built $OUT/av2bench and $OUT/av2bench-pristine"
echo "check: bun $HERE/check.ts <dir of AV2 .avif files>"
