#!/usr/bin/env bash
# build.sh: compile AVM's reference AV2 decoder to WebAssembly.
#
# WHAT IT WRITES. build/av2dec.<sha8>.wasm, named by the first 8 hex of its own
# sha256 so a URL can name exact bytes. With DEST=<dir> it also copies the file
# there and removes any older av2dec.*.wasm beside it: aadhar.sh's /garage/av2
# builds with DEST=<site>/public/garage/av2, then points WASM_URL in
# src/client/garage/av2-decode.js at the new name.
#
# WHAT IT IS. The AVM 1.0.0 that ../encoder/build.sh pins, built
# decoder-only, single-threaded, no SIMD, as a standalone reactor module with
# native WebAssembly longjmp (AVM reports errors by longjmp). That leaves five
# imports, all stdio and memory plumbing, which the host supplies by hand (the
# site's av2-decode.js does), so no Emscripten JS glue ships. Native longjmp
# uses WebAssembly exception handling in its legacy encoding (Emscripten's
# default): Chrome 95, Firefox 100, Safari 15.2. av2dec.c is the small C
# surface the worker calls.
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
# On aadhar.sh, contract-av2-decoder-matches-avm.test.mjs holds the shipped
# samples to the plane hashes native AVM produced.
#
# COST. 1,760,696 B. Brotli q11 gets it to about 430 KB, but it ships with no
# precompressed twin, so the edge compresses it on the fly: measured on a branch
# preview 2026-10-13, 535,708 B br, 556,087 B zstd, 617,286 B gzip. Warm decode
# under Bun on an M3 Max: 80 to 130 ms per 600px tile; in Chrome on the
# preview, 39 to 92 ms. -msimd128 auto-vectorization measured no
# faster, so it stays off.
#
# EMSCRIPTEN DECIDES THE BYTES. emcc 6.0.12 minted av2dec.90fc76df.wasm, the
# file aadhar.sh serves (rebuilt byte-identical from a clean av2-tone checkout,
# 2026-10-14). Another emcc can write different bytes and so a new name, which
# re-mints the URL and needs the site's contract re-run; the build warns.
#
#   ./build.sh            needs emcc (brew install emscripten) and the AVM
#                         source from ../encoder/build.sh, or AVM_SRC=<dir>
#   DEST=<dir> ./build.sh also copy the wasm into <dir>
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
AVM_SRC="${AVM_SRC:-$HERE/../encoder/build/_deps/libavm-src}"
OUT="$HERE/build"
DEST="${DEST:-}"

command -v emcc >/dev/null || { echo "error: emcc not found (brew install emscripten)" >&2; exit 1; }
EMCC_PINNED="6.0.12"
emcc_version="$(emcc --version | sed -n '1s/.*) \([0-9][0-9.]*\).*/\1/p')"
[ "$emcc_version" = "$EMCC_PINNED" ] || echo "warning: emcc $emcc_version, not $EMCC_PINNED; the wasm may differ from the shipped one" >&2
[ -f "$AVM_SRC/av2/av2_dx_iface.c" ] || {
  echo "error: no AVM source at $AVM_SRC; run ../encoder/build.sh or set AVM_SRC" >&2; exit 1; }
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
rm -f "$OUT"/av2dec.*.wasm
cp "$OUT/av2dec.wasm" "$OUT/av2dec.$hash.wasm"
echo "wrote build/av2dec.$hash.wasm ($(stat -f %z "$OUT/av2dec.wasm") B)"
if [ -n "$DEST" ]; then
  rm -f "$DEST"/av2dec.*.wasm
  cp "$OUT/av2dec.wasm" "$DEST/av2dec.$hash.wasm"
  echo "copied to $DEST/av2dec.$hash.wasm"
fi
