#!/usr/bin/env bash
# build.sh: the decoders the demo races against, built in build/ (gitignored).
#
#   build/dav2d/      VideoLAN's dav2d at a pinned commit, the fastest AV2
#                     decoder outside this repo. It reads 4:2:0 only: its OBU
#                     parser refuses profile 4 (4:4:4), so run.ts races it on a
#                     4:2:0 copy of the photo.
#   build/dav1dbench  davbench.c against Homebrew's dav1d, for the same photo
#                     as AV1 (what browsers decode today)
#   build/dav2dbench  davbench.c against build/dav2d
#
# Both benches decode in process and hash the planes the way AVM's own bench
# does, so run.ts can check dav2d's pixels against AVM's.
#
#   ./build.sh    needs meson, ninja, clang, pkgconf, git and brew's dav1d
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/build"
DAV2D_REPO=https://code.videolan.org/videolan/dav2d.git
DAV2D_COMMIT=492be09f5c3629a5bce90a420e6824dbd99301d8

for cmd in meson ninja clang pkg-config git; do
  command -v "$cmd" >/dev/null || { echo "error: $cmd not found" >&2; exit 1; }
done
pkg-config --exists dav1d || { echo "error: no dav1d; brew install dav1d" >&2; exit 1; }
mkdir -p "$OUT"

if [ ! -d "$OUT/dav2d/src/.git" ]; then
  rm -rf "$OUT/dav2d/src"
  git clone --quiet "$DAV2D_REPO" "$OUT/dav2d/src"
fi
git -C "$OUT/dav2d/src" fetch --quiet origin "$DAV2D_COMMIT" 2>/dev/null || true
git -C "$OUT/dav2d/src" checkout --quiet --detach "$DAV2D_COMMIT"
if [ ! -f "$OUT/dav2d/build/build.ninja" ]; then
  meson setup "$OUT/dav2d/build" "$OUT/dav2d/src" --buildtype release \
    --default-library static -Denable_tools=false -Denable_tests=false >/dev/null
fi
ninja -C "$OUT/dav2d/build" >/dev/null

clang -O2 -DDAV=1 "$HERE/davbench.c" $(pkg-config --cflags --libs dav1d) -o "$OUT/dav1dbench"
clang -O2 -DDAV=2 "$HERE/davbench.c" -I"$OUT/dav2d/src/include" -I"$OUT/dav2d/build/include" \
  "$OUT/dav2d/build/src/libdav2d.a" -lpthread -o "$OUT/dav2dbench"
echo "built $OUT/dav1dbench and $OUT/dav2dbench (dav2d ${DAV2D_COMMIT:0:9})"
