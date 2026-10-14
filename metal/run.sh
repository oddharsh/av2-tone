#!/usr/bin/env bash
# run.sh: decode /garage/av2's AV2 samples with the pristine AVM decoder, then
# check gdf.metal against AVM's C kernels on each and time a 1600x2400 frame.
# Exits non-zero on any mismatch. Time it on a quiet machine: the C column
# doubles under load and the GPU column moves less, which flatters the GPU.
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$DIR/../../.."
AVIFDEC="${AVIFDEC:-$DIR/../libavif-avm/build/avifdec}"
[ -x "$DIR/build/gdfbench" ] || "$DIR/build.sh"
mkdir -p "$DIR/build/y4m"
for f in "$ROOT"/public/garage/av2/*-av2.avif; do
  "$AVIFDEC" "$f" "$DIR/build/y4m/$(basename "$f" .avif).y4m" >/dev/null
done
exec "$DIR/build/gdfbench" --shader "$DIR/gdf.metal" "$@" "$DIR"/build/y4m/*.y4m
