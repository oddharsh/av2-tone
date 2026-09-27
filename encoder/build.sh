#!/usr/bin/env bash
# build.sh — build an AV2-capable avifenc/avifdec from PINNED SOURCE.
#
# WHAT THIS IS FOR. Measurement, never shipping. No browser decodes AV2, and
# the file format is experimental, so nothing this build writes may reach
# public/. codec-knob-probe.ts's `--codec avm` arm is the consumer: it encodes
# AV2 stills onto the /pixel-peeper format axis's own AVIF budgets and scores
# them at matched bytes, which is how to watch where AV2 is going for photos.
#
# WHAT IT PRODUCES. libavif built with its experimental AVIF_CODEC_AVM (AVM is
# the AV2 reference software from AOMedia) plus the installed aom, so ONE
# binary encodes both: `-c avm` writes AV2, `-c aom` writes AV1. The file is
# still an AVIF container (ftyp avif/mif1/miaf) whose image item is `av02` with
# an `av2C` config box. brew's avifdec skips that item rather than misreading it
# ("Primary item not found"), and that is NOT the same as degrading gracefully
# in a browser. Measured 2026-09-27 in Chrome 154 and Canary 156: an AV2 file
# behind <picture><source type="image/avif"> with a PNG <img> fallback draws a
# BROKEN image (naturalWidth 0, currentSrc the AV2 source). The browser supports
# the declared type, commits to it, and fails the decode, which <picture> does
# not catch (CLAUDE.md gotcha 7). So these files cannot ship under the AVIF name.
#
# WHY A COMMIT AND NOT A TAG. No libavif release carries AVM v1.0.0, the first
# released AV2 (2026-05-28). v1.4.2 pins AVM research-v15.0.0; v1.0.0 reached
# libavif main in d8b4e042cd (2026-05-29). The commit below is main as of
# 2026-09-26, and libavif's cmake/Modules/LocalAvm.cmake pins AVM to v1.0.0
# from it, so this script pins one revision and inherits the second.
#
# THE av02 BITSTREAM IS NOT FROZEN FOR IMAGES. libavif's own configure prints
# "AV2 support with avm is experimental. Only use for testing." Treat a number
# measured here as a statement about THIS pin, and re-measure after moving it.
#
# CONTROLS, 2026-09-27, on a 320px crop: lossless (-l) decodes BIT-EXACT against
# the source (0 of 307,254 BMP bytes differ, 160,357 B against the PNG's
# 174,474). Lossy -q means something else here than for aom: libavif maps
# quality onto AV2's -48..255 quantizer, so -q 63 wrote 30,972 B where aom
# wrote 10,360. Compare the two only at matched bytes.
#
# COST. The first build fetches AVM plus its TensorFlow Lite, abseil, eigen and
# libyuv trees and takes about 2 minutes on 14 cores. Everything lands under
# src/ and build/, both gitignored, same as the sibling libavif/ build.
#
#   ./build.sh            build if missing
#   ./build.sh --force    rebuild from scratch
set -euo pipefail

LIBAVIF_REV="768b3dfe30da63265eb31dad6f79fc7662f65b42"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$SCRIPT_DIR/src"
OUT="$SCRIPT_DIR/build"
AVIFENC="$OUT/avifenc"

if [ "${1:-}" = "--force" ]; then rm -rf "$SRC" "$OUT"; fi

if [ -x "$AVIFENC" ] && [ -x "$OUT/avifdec" ]; then
  echo "AV2 avifenc already built: $AVIFENC"
  "$AVIFENC" --version | sed -n '1p'
  exit 0
fi

for cmd in cmake ninja git pkg-config; do
  command -v "$cmd" >/dev/null 2>&1 || {
    echo "error: $cmd not found (brew install cmake ninja pkgconf)" >&2; exit 1; }
done
# aom comes from the system so the AV1 arm of the same binary matches what the
# pipeline links; the AV2 codec is the only thing this build adds.
pkg-config --exists aom || {
  echo "error: aom development files not found (brew install aom)" >&2; exit 1; }

if [ ! -d "$SRC/.git" ]; then
  echo "fetching libavif $LIBAVIF_REV…" >&2
  rm -rf "$SRC"
  git init -q "$SRC"
  git -C "$SRC" remote add origin https://github.com/AOMediaCodec/libavif.git
  git -C "$SRC" fetch -q --depth 1 origin "$LIBAVIF_REV" >&2
  git -C "$SRC" checkout -q FETCH_HEAD
fi
if [ "$(git -C "$SRC" rev-parse HEAD)" != "$LIBAVIF_REV" ]; then
  echo "error: $SRC is not at $LIBAVIF_REV; rerun with --force" >&2; exit 1
fi

cmake -G Ninja -S "$SRC" -B "$OUT" \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=OFF \
  -DAVIF_CODEC_AVM=LOCAL \
  -DAVIF_CODEC_AOM=SYSTEM \
  -DAVIF_LIBYUV=LOCAL \
  -DAVIF_LIBSHARPYUV=OFF \
  -DAVIF_BUILD_APPS=ON \
  -DAVIF_JPEG=SYSTEM \
  -DAVIF_ZLIBPNG=SYSTEM >&2
cmake --build "$OUT" --target avifenc avifdec >&2

# A binary built WITHOUT avm is the silent failure here: `-c avm` would then be
# refused one encode at a time, deep inside a probe run. Assert it once instead.
# Plain grep rather than grep -q, so --version is read to EOF under pipefail.
if ! "$AVIFENC" --version 2>&1 | grep "avm \[enc/dec\]:1.0.0" >/dev/null; then
  echo "error: built avifenc does not report avm [enc/dec]:1.0.0" >&2
  "$AVIFENC" --version >&2
  exit 1
fi

echo "built: $AVIFENC (and avifdec beside it)"
"$AVIFENC" --version | sed -n '1p'
