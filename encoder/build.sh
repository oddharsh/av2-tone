#!/usr/bin/env bash
# build.sh: build an AV2-capable avifenc/avifdec from PINNED SOURCE.
#
# WHAT THIS IS FOR. Measurement and test files, never serving as an image. No
# browser decodes AV2, and the file format is experimental. aadhar.sh uses
# these binaries to encode AV2 stills onto its own AVIF byte budgets and score
# them at matched bytes (its av2-tile-probe.ts and codec-knob-probe.ts), and the
# few AV2 files it publishes reach the page only as DATA for the WebAssembly
# decoder that ../wasm builds.
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
# not catch (docs/GOTCHAS.md gotcha 7). So these files cannot ship under the AVIF name.
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
# src/ and build/, both gitignored.
#
#   ./build.sh            build if missing
#   ./build.sh --force    rebuild from scratch
#   ./build.sh --tuned    ALSO build build/tuned: the same pin plus patches/ and
#                         the SIMDe kernels (see "THE TUNED BUILD" below)
#
# THE TUNED BUILD. patches/ holds twelve encoder-only changes to AVM, each off by
# default and each decoded pixel-identically by the untouched reference decoder:
#   0001 base-delta-q    the sequence DC/chroma offsets the encoder hard-zeroes
#   0002 variance-boost  deltaq-mode=3, libaom/SVT-AV1 Variance Boost on AV2 q
#   0003 jpegli-mask     deltaq-mode=4, jpegli adaptive-quantization field
#   0004 qm-curves       qm-curve / qm-chroma-curve, libaom still-image QM levels
#   0005 qm-per-segment  qmseg: two QM sets split by texture, an AV2-only syntax
#   0006 fwd-txfm        speed only, no option: the forward transform zeroed a
#                        16 KB scratch per call, 20% of a speed-3 encode (1 core,
#                        600px: 37.9 s to 30.7 s; speed 6 about 5%)
#   0007 tool-unlock     a bitmask that turns back on AV2 tools the speed level
#                        or libavif switches off: 1 3-way and uneven 4-way
#                        partitions, 2 IST/CCTX search, 4 TCQ, 8 parity hiding,
#                        16 loop restoration, 32 every intra mode, 64 DIP
#   0008 iq-deltaq       libaom tune=iq's quantizer balance as frame deltas: DC
#                        finer than AC as AV1's DC table runs, 4:4:4 chroma AC
#                        coarser, at the AV1 qindex with the same step. Measured
#                        a tie on ssimulacra2 (README), so it stays off
#   0009 lambda-map      lambda-map=<file> replaces tune=ssim's per-16x16 RD
#                        multiplier curve with any map, and field-dump=<file>
#                        writes that curve and the jpegli field, so block-level
#                        perceptual weighting is decided outside AVM
#   0010 seg-map         seg-map=<file>: a segment per 8x8 luma block, each with
#                        a qindex delta and luma QM level (up to 16 segments
#                        with enable-ext-seg, 4 QM sets), so block-level
#                        quantization is decided outside AVM. Also fixes the
#                        final encode keeping another segment's quantizer
#   0011 qm-file         qm-file=<file>: user-defined quantization matrices per
#                        level, sent in AV2's QM OBU (they shape transforms up
#                        to 8x8; AV2 keeps its own for larger ones)
#   0012 qm-round        a qm-file's forward weights round 1024 / iwt, as AVM's
#                        predefined ones do, instead of truncating it (38% of
#                        entries moved, so AVM's own matrices sent back
#                        quantized differently); qmround.c checks it
# experiments/ holds patches that change what the decoder reads, so they never
# join this build: 0013 qm-4x4 adds an optional 4x4 matrix to the QM OBU.
# Then simde.ts compiles AVM's own AVX2 kernels for arm64 through SIMDe. The build
# refuses to finish unless the tuned binary, with every new option off, writes
# BYTE-IDENTICAL files to the pristine one, and the pristine decoder reads a
# qmseg file pixel-identically. build/ itself stays pristine on purpose: it is
# the reference decoder every one of those controls is judged by.
set -euo pipefail

LIBAVIF_REV="768b3dfe30da63265eb31dad6f79fc7662f65b42"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$SCRIPT_DIR/src"
OUT="$SCRIPT_DIR/build"
AVIFENC="$OUT/avifenc"

if [ "${1:-}" = "--force" ]; then rm -rf "$SRC" "$OUT"; fi

if [ -x "$AVIFENC" ] && [ -x "$OUT/avifdec" ] && [ "${1:-}" != "--tuned" ]; then
  echo "AV2 avifenc already built: $AVIFENC"
  "$AVIFENC" --version | sed -n '1p'
  exit 0
fi

if [ ! -x "$AVIFENC" ] || [ ! -x "$OUT/avifdec" ]; then

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
fi
[ "${1:-}" = "--tuned" ] || exit 0

# ---- the tuned build ---------------------------------------------------------
# SIMDe v0.8.2, pinned by commit like everything else here.
SIMDE_REV="71fd833d9666141edcd1d3c109a80e228303d8d7"
TUNED="$OUT/tuned"
TSRC="$OUT/tuned-src"
SIMDE_DIR="$OUT/simde"
PRISTINE_AVM="$OUT/_deps/libavm-src"
[ -d "$PRISTINE_AVM" ] || { echo "error: $PRISTINE_AVM missing; rerun with --force" >&2; exit 1; }

rm -rf "$TUNED" "$TSRC"
cp -R "$PRISTINE_AVM" "$TSRC"
for p in "$SCRIPT_DIR"/patches/*.patch; do
  patch -p1 -s -d "$TSRC" < "$p" || { echo "error: $(basename "$p") does not apply" >&2; exit 1; }
done

if [ ! -d "$SIMDE_DIR/.git" ]; then
  echo "fetching SIMDe $SIMDE_REV…" >&2
  rm -rf "$SIMDE_DIR"
  git init -q "$SIMDE_DIR"
  git -C "$SIMDE_DIR" remote add origin https://github.com/simd-everywhere/simde.git
  git -C "$SIMDE_DIR" fetch -q --depth 1 origin "$SIMDE_REV" >&2
  git -C "$SIMDE_DIR" checkout -q FETCH_HEAD
fi

cmake -G Ninja -S "$SRC" -B "$TUNED" \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=OFF \
  -DAVIF_CODEC_AVM=LOCAL \
  -DAVIF_CODEC_AOM=SYSTEM \
  -DAVIF_LIBYUV=LOCAL \
  -DAVIF_LIBSHARPYUV=OFF \
  -DAVIF_BUILD_APPS=ON \
  -DAVIF_JPEG=SYSTEM \
  -DAVIF_ZLIBPNG=SYSTEM \
  -DFETCHCONTENT_SOURCE_DIR_LIBAVM="$TSRC" >&2
cmake --build "$TUNED" --target avifenc avifdec >&2
bun "$SCRIPT_DIR/simde.ts" --src "$TSRC" --build "$TUNED" --simde "$SIMDE_DIR" --work "$OUT/simde-work" >&2

# THE GATE. Its input is ../fixture's synthetic frame (gradients, noise, edges,
# stripes), deterministic and licence-free. AV2_GATE_IMAGE=<file> swaps in a
# real photo; aadhar.sh passes one of its own 600px tiers.
GATE="$OUT/gate"
rm -rf "$GATE"; mkdir -p "$GATE"
IMG="${AV2_GATE_IMAGE:-$(bun "$SCRIPT_DIR/../fixture/gen.ts")}"
[ -f "$IMG" ] || { echo "error: gate image $IMG not found" >&2; exit 1; }
BASE=(-c avm -d 10 --speed 6 --yuv 444 -a sb-size=128 -a qp=105 --jobs 8)
"$AVIFENC" "${BASE[@]}" "$IMG" "$GATE/pristine.avif" >/dev/null
"$TUNED/avifenc" "${BASE[@]}" "$IMG" "$GATE/tuned.avif" >/dev/null
cmp -s "$GATE/pristine.avif" "$GATE/tuned.avif" || {
  echo "error: the tuned build is not byte-identical with its options off" >&2; exit 1; }
"$TUNED/avifenc" "${BASE[@]}" -a enable-qm=1 -a qm-curve=1 -a qmseg=1 -a qmseg-thresh=40 "$IMG" "$GATE/qmseg.avif" >/dev/null
"$OUT/avifdec" "$GATE/qmseg.avif" "$GATE/qmseg.pristine.y4m" >/dev/null
"$TUNED/avifdec" "$GATE/qmseg.avif" "$GATE/qmseg.tuned.y4m" >/dev/null
cmp -s "$GATE/qmseg.pristine.y4m" "$GATE/qmseg.tuned.y4m" || {
  echo "error: the reference decoder reads a qmseg file differently" >&2; exit 1; }
# 0007's tool-unlock must change the stream (a knob that silently does nothing
# would score as a tie), and the reference decoder must read what it writes.
"$TUNED/avifenc" "${BASE[@]}" -a tool-unlock=127 "$IMG" "$GATE/unlock.avif" >/dev/null
cmp -s "$GATE/tuned.avif" "$GATE/unlock.avif" && {
  echo "error: tool-unlock=127 wrote the same bytes as no options" >&2; exit 1; }
"$OUT/avifdec" "$GATE/unlock.avif" "$GATE/unlock.pristine.y4m" >/dev/null
"$TUNED/avifdec" "$GATE/unlock.avif" "$GATE/unlock.tuned.y4m" >/dev/null
cmp -s "$GATE/unlock.pristine.y4m" "$GATE/unlock.tuned.y4m" || {
  echo "error: the reference decoder reads a tool-unlock file differently" >&2; exit 1; }
# 0008's iq-deltaq writes frame-level DC and chroma deltas the reference
# encoder never sends: the stream must change, and the reference decoder must
# read it as the tuned build does.
"$TUNED/avifenc" "${BASE[@]}" -a iq-deltaq=1 "$IMG" "$GATE/iq.avif" >/dev/null
cmp -s "$GATE/tuned.avif" "$GATE/iq.avif" && {
  echo "error: iq-deltaq=1 wrote the same bytes as no options" >&2; exit 1; }
"$OUT/avifdec" "$GATE/iq.avif" "$GATE/iq.pristine.y4m" >/dev/null
"$TUNED/avifdec" "$GATE/iq.avif" "$GATE/iq.tuned.y4m" >/dev/null
cmp -s "$GATE/iq.pristine.y4m" "$GATE/iq.tuned.y4m" || {
  echo "error: the reference decoder reads an iq-deltaq file differently" >&2; exit 1; }
# 0009's field-dump must not touch the stream; its tune=ssim grid, read back
# as a lambda-map, must give the same bytes (the path is exact); a flat map
# must change them; and a map for the wrong grid must fail, never fall back.
"$TUNED/avifenc" "${BASE[@]}" -a field-dump="$GATE/fields.txt" "$IMG" "$GATE/dump.avif" >/dev/null
cmp -s "$GATE/tuned.avif" "$GATE/dump.avif" || {
  echo "error: field-dump changed the bytes" >&2; exit 1; }
grep -q '^jpegli ' "$GATE/fields.txt" || { echo "error: field-dump wrote no jpegli field" >&2; exit 1; }
awk '/^jpegli /{exit} /^ssim /{$1="lambda"} {print}' "$GATE/fields.txt" > "$GATE/ssim.map"
"$TUNED/avifenc" "${BASE[@]}" -a lambda-map="$GATE/ssim.map" "$IMG" "$GATE/roundtrip.avif" >/dev/null
cmp -s "$GATE/tuned.avif" "$GATE/roundtrip.avif" || {
  echo "error: tune=ssim's own grid as a lambda-map changed the bytes" >&2; exit 1; }
awk 'NR==1{print; next} {for (i = 1; i <= NF; i++) $i = 1; print}' "$GATE/ssim.map" > "$GATE/flat.map"
"$TUNED/avifenc" "${BASE[@]}" -a lambda-map="$GATE/flat.map" "$IMG" "$GATE/flat.avif" >/dev/null
cmp -s "$GATE/tuned.avif" "$GATE/flat.avif" && {
  echo "error: a flat lambda-map wrote the same bytes as tune=ssim" >&2; exit 1; }
# Refused means exit 1 with the reason, so a crash can't pass for a refusal.
printf 'lambda 1 1\n1\n' > "$GATE/wrong.map"
set +e
"$TUNED/avifenc" "${BASE[@]}" -a lambda-map="$GATE/wrong.map" "$IMG" "$GATE/wrong.avif" >"$GATE/wrong.log" 2>&1
st=$?
set -e
[ "$st" = 1 ] && grep -q "lambda-map: not a" "$GATE/wrong.log" || {
  echo "error: a lambda-map for the wrong grid was not refused cleanly (exit $st)" >&2; exit 1; }
# 0010's seg-map. A map whose one segment changes nothing is no map (same
# bytes). A split inside a superblock must decode as well as the same split on
# a superblock edge: AVM's final encode kept the last RD candidate's quantizer,
# so a block was quantized at another segment's qindex and signalled at its
# own, about 10 dB down (the patch resets it, as AVM does for an AQ mode). The
# reference decoder must read the file as the tuned build does, and a map for
# the wrong grid must exit 1 with the reason.
read -r W H < <(head -c 200 "$IMG" | head -1 | sed -E 's/.* W([0-9]+) H([0-9]+).*/\1 \2/')
COLS=$(( (W + 7) / 8 )); ROWS=$(( (H + 7) / 8 ))
segmap() {  # segmap <8x8 blocks in segment 0, from the left> <segment 1's qindex delta>
  awk -v c="$COLS" -v r="$ROWS" -v s="$1" -v d="$2" 'BEGIN {
    print "segmap", c, r
    for (y = 0; y < r; y++) { l = ""; for (x = 0; x < c; x++) l = l (x < s ? 0 : 1) (x < c - 1 ? " " : ""); print l }
    print "seg 0 0 -1"; if (d != "none") print "seg 1", d, -1 }'
}
segmap "$COLS" none > "$GATE/noop.map"
"$TUNED/avifenc" "${BASE[@]}" -a seg-map="$GATE/noop.map" "$IMG" "$GATE/noop.avif" >/dev/null
cmp -s "$GATE/tuned.avif" "$GATE/noop.avif" || {
  echo "error: a no-op seg-map changed the bytes" >&2; exit 1; }
segmap 32 40 > "$GATE/edge.map"   # 256 px: a superblock edge at 64 and 128
segmap 40 40 > "$GATE/mid.map"    # 320 px: inside a superblock either way
for m in edge mid; do
  "$TUNED/avifenc" "${BASE[@]}" -a seg-map="$GATE/$m.map" "$IMG" "$GATE/$m.avif" >/dev/null
  "$OUT/avifdec" "$GATE/$m.avif" "$GATE/$m.pristine.y4m" >/dev/null
done
"$TUNED/avifdec" "$GATE/mid.avif" "$GATE/mid.tuned.y4m" >/dev/null
cmp -s "$GATE/mid.pristine.y4m" "$GATE/mid.tuned.y4m" || {
  echo "error: the reference decoder reads a seg-map file differently" >&2; exit 1; }
EDGE=$(bun "$SCRIPT_DIR/psnr.ts" "$IMG" "$GATE/edge.pristine.y4m")
MID=$(bun "$SCRIPT_DIR/psnr.ts" "$IMG" "$GATE/mid.pristine.y4m")
awk -v e="$EDGE" -v m="$MID" 'BEGIN { exit !(m > e - 2) }' || {
  echo "error: a seg-map split inside a superblock decodes at $MID dB against $EDGE dB on its edge" >&2; exit 1; }
printf 'segmap 1 1\n0\nseg 0 0 -1\n' > "$GATE/wrongseg.map"
set +e
"$TUNED/avifenc" "${BASE[@]}" -a seg-map="$GATE/wrongseg.map" "$IMG" "$GATE/wrongseg.avif" >"$GATE/wrongseg.log" 2>&1
st=$?
set -e
[ "$st" = 1 ] && grep -q "seg-map: not a" "$GATE/wrongseg.log" || {
  echo "error: a seg-map for the wrong grid was not refused cleanly (exit $st)" >&2; exit 1; }
# 0011's qm-file. With the QM level pinned at 8, a file that replaces only
# level 9 must leave the pixels alone (it adds the QM OBU and nothing else), a
# steeper level 8 must change them, the reference decoder must read that file
# as the tuned build does, and a value under 5 must exit 1 with the reason.
QM=(-a enable-qm=1 -a qm-min=8 -a qm-max=8)
qmfile() {  # qmfile <level> <the first value, 5..255>
  awk -v l="$1" -v first="$2" 'BEGIN {
    print "level", l
    split("y u v", pl, " "); split("8 8 8 4 4 8", wh, " "); split("8x8 8x4 4x8", nm, " ")
    for (p = 1; p <= 3; p++) for (t = 1; t <= 3; t++) {
      w = wh[2 * t - 1]; h = wh[2 * t]; line = pl[p] nm[t]
      for (y = 0; y < h; y++) for (x = 0; x < w; x++) {
        v = int(32 * 2 ^ (0.18 * (x * 8 / w + y * 8 / h)) + 0.5); if (v > 255) v = 255
        if (p == 1 && t == 1 && x == 0 && y == 0) v = first
        line = line " " v }
      print line } }'
}
qmfile 9 32 > "$GATE/l9.qm"; qmfile 8 32 > "$GATE/l8.qm"; qmfile 8 4 > "$GATE/bad.qm"
"$TUNED/avifenc" "${BASE[@]}" "${QM[@]}" "$IMG" "$GATE/qm.avif" >/dev/null
for q in l9 l8; do
  "$TUNED/avifenc" "${BASE[@]}" "${QM[@]}" -a qm-file="$GATE/$q.qm" "$IMG" "$GATE/$q.avif" >/dev/null
  "$OUT/avifdec" "$GATE/$q.avif" "$GATE/$q.pristine.y4m" >/dev/null
done
"$OUT/avifdec" "$GATE/qm.avif" "$GATE/qm.pristine.y4m" >/dev/null
cmp -s "$GATE/qm.avif" "$GATE/l9.avif" && {
  echo "error: a qm-file for level 9 wrote no QM OBU" >&2; exit 1; }
cmp -s "$GATE/qm.pristine.y4m" "$GATE/l9.pristine.y4m" || {
  echo "error: a qm-file for an unused level changed the pixels" >&2; exit 1; }
cmp -s "$GATE/qm.pristine.y4m" "$GATE/l8.pristine.y4m" && {
  echo "error: a qm-file for the level in use left the pixels alone" >&2; exit 1; }
"$TUNED/avifdec" "$GATE/l8.avif" "$GATE/l8.tuned.y4m" >/dev/null
cmp -s "$GATE/l8.pristine.y4m" "$GATE/l8.tuned.y4m" || {
  echo "error: the reference decoder reads a qm-file file differently" >&2; exit 1; }
set +e
"$TUNED/avifenc" "${BASE[@]}" "${QM[@]}" -a qm-file="$GATE/bad.qm" "$IMG" "$GATE/bad.avif" >"$GATE/bad.log" 2>&1
st=$?
set -e
[ "$st" = 1 ] && grep -q "qm-file: level 8 y8x8 needs" "$GATE/bad.log" || {
  echo "error: a qm-file value under 5 was not refused cleanly (exit $st)" >&2; exit 1; }
# 0012: AVM's own matrices loaded as a qm-file must give AVM's own forward
# weights at every size but 4x4 (which AV2 derives from the 8x8).
cc -O1 -c -I"$TSRC" -I"$TUNED/_deps/libavm-build" "$SCRIPT_DIR/qmround.c" -o "$GATE/qmround.o"
c++ -o "$GATE/qmround" "$GATE/qmround.o" "$TUNED/_deps/libavm-build/libavm.a" -lm -lpthread
"$GATE/qmround" >&2 || {
  echo "error: a qm-file of AVM's own matrices quantizes differently from them" >&2; exit 1; }
echo "built: $TUNED/avifenc (gate: identical with options off; qmseg, tool-unlock and iq-deltaq decode identically; lambda-map round-trips; seg-map decodes inside superblocks; qm-file changes only the level it replaces and quantizes AVM's own matrices as AVM does)"
