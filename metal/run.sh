#!/usr/bin/env bash
# run.sh: check gdf.metal against AVM's C kernels and time a 1600x2400 frame.
#
# The inputs are real AV2 decodes, so GDF sees reconstructed pixels. By default
# it makes them: the ../fixture frame encoded with ../encoder's pristine
# avifenc at four quantizers (light to heavy filtering), then decoded. Pass a
# directory of AV2 .avif files to use those instead; aadhar.sh passes its
# /garage/av2 samples. Exits non-zero on any mismatch.
#
# Time it on a quiet machine: the C column doubles under load and the GPU
# column moves less, which flatters the GPU.
#
#   metal/run.sh [dir of .avif] [-- gdfbench args, e.g. --size 1600x2400]
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
ENC_BUILD="${ENC_BUILD:-$DIR/../encoder/build}"
[ -x "$ENC_BUILD/avifdec" ] || { echo "error: no $ENC_BUILD/avifdec; run ../encoder/build.sh first" >&2; exit 1; }
[ -x "$DIR/build/gdfbench" ] || "$DIR/build.sh"

CORPUS="$DIR/build/corpus"
if [ $# -gt 0 ] && [ "$1" != "--" ]; then
  SRC="$1"; shift
else
  SRC="$CORPUS"
  if [ ! -f "$CORPUS/.done" ]; then
    rm -rf "$CORPUS"; mkdir -p "$CORPUS"
    fixture="$(bun "$DIR/../fixture/gen.ts")"
    for qp in 60 105 160 220; do
      "$ENC_BUILD/avifenc" -c avm -d 10 --yuv 444 --speed 6 --jobs 4 -a sb-size=128 -a qp=$qp \
        "$fixture" "$CORPUS/fixture-qp$qp.avif" >/dev/null
    done
    touch "$CORPUS/.done"
  fi
fi
[ "${1:-}" = "--" ] && shift

mkdir -p "$DIR/build/y4m"
rm -f "$DIR"/build/y4m/*.y4m
for f in "$SRC"/*.avif; do
  "$ENC_BUILD/avifdec" "$f" "$DIR/build/y4m/$(basename "$f" .avif).y4m" >/dev/null
done
exec "$DIR/build/gdfbench" --shader "$DIR/gdf.metal" "$@" "$DIR"/build/y4m/*.y4m
