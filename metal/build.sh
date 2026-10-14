#!/usr/bin/env bash
# build.sh: build gdfbench, which runs AV2's GDF loop filter on the Apple GPU
# and checks it bit for bit against AVM's own C kernels.
#
# WHY GDF. In AVM 1.0.0's decoder GDF (the learned Guided Detail Filter) was
# 71% of a single-thread still decode before NEON, and it is the one decode
# stage shaped like GPU work: every output pixel is independent. The serial
# entropy parse is not, so a GPU can only ever take GDF's share.
#
# THE ORACLE is gdf_block.c compiled straight from the pinned AVM source that
# tools/photos/libavif-avm/build.sh fetched, so the reference kernels and the
# tables are the decoder's own, never a copy.
#
# The shader compiles at run time through newLibraryWithSource, so this needs
# no Metal toolchain download, only the macOS SDK.
#
#   tools/photos/av2-metal/build.sh
#   tools/photos/av2-metal/run.sh          decode the samples, check, time
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
AVM_BUILD="${AVM_BUILD:-$DIR/../libavif-avm/build}"
AVM_SRC="$AVM_BUILD/_deps/libavm-src"
AVM_CFG="$AVM_BUILD/_deps/libavm-build"
OUT="$DIR/build"

[ -f "$AVM_SRC/av2/common/gdf_block.c" ] || {
  echo "error: $AVM_SRC has no AVM source; run tools/photos/libavif-avm/build.sh first" >&2; exit 1; }
[ -f "$AVM_CFG/config/avm_config.h" ] || {
  echo "error: $AVM_CFG/config/avm_config.h missing; run tools/photos/libavif-avm/build.sh first" >&2; exit 1; }

mkdir -p "$OUT"
# -fwrapv: the C oracle relies on int32 wrap where GDF_NORM_IDX scales a large
# sum, and Metal's int wraps; without it clang may assume no overflow.
CFLAGS=(-O2 -fwrapv -I"$AVM_SRC" -I"$AVM_CFG" -Wno-everything)
clang "${CFLAGS[@]}" -c "$AVM_SRC/av2/common/gdf_block.c" -o "$OUT/gdf_block.o"
clang "${CFLAGS[@]}" -fobjc-arc -c "$DIR/gdfbench.m" -o "$OUT/gdfbench.o"
clang "$OUT/gdfbench.o" "$OUT/gdf_block.o" -framework Foundation -framework Metal -o "$OUT/gdfbench"
echo "built: $OUT/gdfbench"
