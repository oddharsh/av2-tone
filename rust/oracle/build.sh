#!/usr/bin/env bash
# build.sh: the oracle the safe-Rust decoder is checked against. Pristine AVM
# 1.0.0, decoder only, built out of tree from the source ../../encoder/build.sh
# fetched (never patched, never copied), and av2oracle.c linked against it,
# with AVM's entropy encoder (avm_dsp/entenc.c) for the --entropy mode.
#
#   rust/oracle/build.sh     writes rust/build/av2oracle
#
# Needs cmake, ninja, clang, and the AVM source (../../encoder/build.sh, or
# AVM_SRC=<dir>). The first build takes about a minute.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
AVM_SRC="${AVM_SRC:-$HERE/../../encoder/build/_deps/libavm-src}"
OUT="$(cd "$HERE/.." && pwd)/build"
mkdir -p "$OUT"

for cmd in cmake ninja clang; do
  command -v "$cmd" >/dev/null || { echo "error: $cmd not found" >&2; exit 1; }
done
[ -f "$AVM_SRC/av2/av2_dx_iface.c" ] || {
  echo "error: no AVM source at $AVM_SRC; run encoder/build.sh or set AVM_SRC" >&2; exit 1; }
# The oracle is only worth its name if AVM is untouched.
if git -C "$AVM_SRC" rev-parse --git-dir >/dev/null 2>&1 &&
   [ -n "$(git -C "$AVM_SRC" status --porcelain --untracked-files=no)" ]; then
  echo "error: $AVM_SRC has local changes; the oracle needs pristine AVM" >&2; exit 1
fi

cmake -S "$AVM_SRC" -B "$OUT/avm" -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DCONFIG_AV2_ENCODER=0 -DCONFIG_TENSORFLOW_LITE=0 -DCONFIG_MULTITHREAD=1 \
  -DENABLE_EXAMPLES=0 -DENABLE_TESTS=0 -DENABLE_TOOLS=0 -DENABLE_DOCS=0 >"$OUT/avm.log" 2>&1 ||
  { tail -20 "$OUT/avm.log" >&2; exit 1; }
ninja -C "$OUT/avm" avm >>"$OUT/avm.log" 2>&1 || { tail -40 "$OUT/avm.log" >&2; exit 1; }
# entenc.c, AVM's entropy encoder, is not in a decoder-only build; the
# --entropy mode compiles it in from the same pristine source.
clang -O2 -g -Wall -Wextra -Wno-unused-parameter "$HERE/av2oracle.c" "$AVM_SRC/avm_dsp/entenc.c" "$OUT/avm/libavm.a" \
  -I"$AVM_SRC" -I"$OUT/avm" -o "$OUT/av2oracle" -lm -lpthread
echo "built $OUT/av2oracle"
