#!/usr/bin/env bash
# build.sh: the oracle the safe-Rust decoder is checked against. Pristine AVM
# 1.0.0, decoder only, built out of tree from the source ../../encoder/build.sh
# fetched (never patched, never copied), and av2oracle.c linked against it,
# with AVM's entropy encoder (avm_dsp/entenc.c) for the --entropy mode. A
# second build of the same source turns on CONFIG_ACCOUNTING, a stock AVM
# option that records every symbol a decode reads, for av2trace.c.
#
#   rust/oracle/build.sh     writes rust/build/av2oracle, av2trace and (with an ELF
#                            linker) av2xform
#
# Needs cmake, ninja, clang, and the AVM source (../../encoder/build.sh, or
# AVM_SRC=<dir>). The first build takes about two minutes.
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
# av2xform reaches AVM's per-block inverse transform through the linker's
# --wrap, which ELF linkers have and Apple's ld does not; on macOS it's skipped.
if echo 'int f(void); int __real_f(void); int __wrap_f(void) { return __real_f(); } int f(void) { return 0; }
          int main(void) { return f(); }' | clang -x c - -Wl,--wrap=f -o "$OUT/.wrapcheck" 2>/dev/null; then
  clang -O2 -g -Wall -Wextra -Wno-unused-parameter "$HERE/av2xform.c" "$OUT/avm/libavm.a" \
    -Wl,--wrap=av2_inverse_transform_block -I"$AVM_SRC" -I"$OUT/avm" -o "$OUT/av2xform" -lm -lpthread
  rm -f "$OUT/.wrapcheck"
else
  echo "note: this linker has no --wrap, so av2xform (the per-block reconstruction oracle) isn't built" >&2
fi

cmake -S "$AVM_SRC" -B "$OUT/avm-acct" -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DCONFIG_AV2_ENCODER=0 -DCONFIG_TENSORFLOW_LITE=0 -DCONFIG_MULTITHREAD=1 -DCONFIG_ACCOUNTING=1 \
  -DENABLE_EXAMPLES=0 -DENABLE_TESTS=0 -DENABLE_TOOLS=0 -DENABLE_DOCS=0 >"$OUT/avm-acct.log" 2>&1 ||
  { tail -20 "$OUT/avm-acct.log" >&2; exit 1; }
ninja -C "$OUT/avm-acct" avm >>"$OUT/avm-acct.log" 2>&1 || { tail -40 "$OUT/avm-acct.log" >&2; exit 1; }
clang -O2 -g -Wall -Wextra -Wno-unused-parameter "$HERE/av2trace.c" "$OUT/avm-acct/libavm.a" \
  -I"$AVM_SRC" -I"$OUT/avm-acct" -o "$OUT/av2trace" -lm -lpthread
echo "built $OUT/av2oracle, $OUT/av2trace$( [ -x "$OUT/av2xform" ] && echo " and $OUT/av2xform")"
