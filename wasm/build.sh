#!/usr/bin/env bash
# build.sh: compile AVM's reference AV2 decoder to WebAssembly for /garage/av2.
#
# WHAT IT WRITES. public/garage/av2/av2dec.<sha8>.wasm, named by the first 8 hex
# of its own sha256 so the URL names exact bytes (the /pixel-peeper tile
# convention), and removes any older av2dec.*.wasm beside it. Then update
# WASM_URL in src/client/garage/av2-decode.js, which this script prints.
#
# WHAT IT IS. The AVM 1.0.0 that ../libavif-avm/build.sh pins, built
# decoder-only, single-threaded, no SIMD, as a standalone reactor module with
# native WebAssembly longjmp (AVM reports errors by longjmp). That leaves five
# imports, all stdio and memory plumbing, which av2-decode.js supplies by hand,
# so no Emscripten JS glue ships. Native longjmp uses WebAssembly exception
# handling in its legacy encoding (Emscripten's default): Chrome 95, Firefox
# 100, Safari 15.2. av2dec.c is the small C surface the worker calls.
#
# AVM's cmake re-enables CONFIG_TENSORFLOW_LITE for its encoder experiments and
# prints a warning saying so, but with the encoder off no TFLite object reaches
# libavm.a (0 of its 97 members, checked with emar t).
#
# CONTROLS, 2026-10-13. Decoded planes are BIT-EXACT against native avifdec on
# 16 files (8 and 10-bit; 4:0:0, 4:2:0, 4:2:2, 4:4:4; lossless; qp 20 to 220;
# speeds 2, 6, 9). RGBA matches avifdec's FULL-DEPTH decode rounded to 8 bits
# on 100.00% of samples (max delta 1); against avifdec -d 8 it reads up to 3
# off, because that build's libyuv 8-bit path is the coarse one.
# contract-av2-decoder-matches-avm.test.mjs holds the shipped samples to the
# plane hashes native AVM produced (expected.json, written by expect.ts).
#
# COST. 1,760,696 B. Brotli q11 gets it to about 430 KB, but it ships with no
# precompressed twin, so the edge compresses it on the fly: measured on a branch
# preview 2026-10-13, 535,708 B br, 556,087 B zstd, 617,286 B gzip. Warm decode
# under Bun on an M3 Max: 80 to 130 ms per 600px tile; in Chrome on the
# preview, 39 to 92 ms. -msimd128 auto-vectorization measured no
# faster, so it stays off.
#
#   ./build.sh            needs emcc (brew install emscripten) and the AVM
#                         source from ../libavif-avm/build.sh, or AVM_SRC=<dir>
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
AVM_SRC="${AVM_SRC:-$HERE/../libavif-avm/build/_deps/libavm-src}"
OUT="$HERE/build"
DEST="$REPO/public/garage/av2"

command -v emcc >/dev/null || { echo "error: emcc not found (brew install emscripten)" >&2; exit 1; }
[ -f "$AVM_SRC/av2/av2_dx_iface.c" ] || {
  echo "error: no AVM source at $AVM_SRC; run ../libavif-avm/build.sh or set AVM_SRC" >&2; exit 1; }
grep -q 'v1.0.0' "$AVM_SRC/CHANGELOG" 2>/dev/null || echo "warning: $AVM_SRC may not be AVM v1.0.0" >&2

SJLJ="-sSUPPORT_LONGJMP=wasm"
emcmake cmake -S "$AVM_SRC" -B "$OUT/avm" -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DAVM_TARGET_CPU=generic -DCONFIG_AV2_ENCODER=0 -DCONFIG_MULTITHREAD=0 \
  -DCONFIG_RUNTIME_CPU_DETECT=0 -DCONFIG_TENSORFLOW_LITE=0 \
  -DENABLE_EXAMPLES=0 -DENABLE_TESTS=0 -DENABLE_TOOLS=0 -DENABLE_DOCS=0 \
  -DCMAKE_C_FLAGS="$SJLJ" -DCMAKE_CXX_FLAGS="$SJLJ" >/dev/null
ninja -C "$OUT/avm" avm >/dev/null

EXPORTS='_av2_decode,_av2_width,_av2_height,_av2_depth,_av2_x_shift,_av2_y_shift,_av2_monochrome,_av2_highbd,_av2_plane,_av2_stride,_av2_matrix,_av2_full_range,_av2_rgba,_malloc,_free'
emcc -O3 "$SJLJ" "$HERE/av2dec.c" "$OUT/avm/libavm.a" -I"$AVM_SRC" -I"$OUT/avm" \
  --no-entry -sSTANDALONE_WASM -sFILESYSTEM=0 \
  -sALLOW_MEMORY_GROWTH=1 -sINITIAL_MEMORY=64MB \
  -sEXPORTED_FUNCTIONS="$EXPORTS" -o "$OUT/av2dec.wasm" 2> >(grep -v 'warning' >&2 || true)

hash="$(shasum -a 256 "$OUT/av2dec.wasm" | cut -c1-8)"
rm -f "$DEST"/av2dec.*.wasm
cp "$OUT/av2dec.wasm" "$DEST/av2dec.$hash.wasm"
echo "wrote public/garage/av2/av2dec.$hash.wasm ($(stat -f %z "$OUT/av2dec.wasm") B)"
echo "set WASM_URL in src/client/garage/av2-decode.js to /garage/av2/av2dec.$hash.wasm"
