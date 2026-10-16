# av2-tone

AV2 still images, encoded and decoded fast on a Mac, and checked bit for bit
against AVM, the AV2 reference software. This code grew up inside
[aadhar.sh](https://aadhar.sh/garage/av2) and moved here so it can stand on its own.

AV2 is the successor to AV1. AVM 1.0.0 shipped in May 2026, and no browser
decodes it yet. The image format is experimental: libavif's own build calls AV2
support "only for testing", so every number below describes the pinned AVM and
can move when the pin does.

## What's here

| dir | what it does |
|---|---|
| `encoder/` | builds libavif with AVM 1.0.0 at a pinned commit, plus a tuned build: seven encoder patches and AVM's AVX2 kernels compiled for arm64 through [SIMDe](https://github.com/simd-everywhere/simde) |
| `fast-decode/` | a native AV2 decoder for this machine: AVM plus NEON GDF, GDF across cores, and deblocking behind reconstruction, checked bit for bit against pristine AVM |
| `wasm/` | compiles AVM's decoder to a standalone WebAssembly module with no Emscripten glue (1.76 MB, about 430 KB Brotli) |
| `metal/` | AV2's GDF loop filter on the Apple GPU, checked against AVM's own C kernels |
| `fixture/` | a deterministic synthetic test frame, so nothing here depends on a photo |
| `demo/` | encodes your photo as AV1 and AV2 at the same byte count, races the encoders and the decoders (AVM, the fast decoder, dav2d, dav1d) on it, and shows both images on a local page |

Coming: a safe-Rust still-image decoder. It's the `av2-tone` crate this repo
is named for, and it hasn't started yet.

## Build

You need macOS on Apple Silicon, `brew install cmake ninja pkgconf aom`, and
[Bun](https://bun.sh). `wasm/` also needs `brew install emscripten`.

```bash
encoder/build.sh --tuned
```

That fetches libavif and AVM, builds the pristine encoder and decoder in
`encoder/build/`, then the tuned pair in `encoder/build/tuned/`. It refuses to
finish unless the tuned encoder, with every new option off, writes files
byte-identical to the pristine one. The first run takes a few minutes.

```bash
metal/run.sh
wasm/build.sh
```

```bash
fast-decode/build.sh
bun fast-decode/check.ts <dir of AV2 .avif files>
```

`fast-decode/build.sh` builds a pristine and a fast decoder from the source
`encoder/build.sh` fetched, and `check.ts` refuses any byte of difference
between them, at one thread and at every core.

`metal/run.sh` encodes the fixture at four quantizers, decodes them, and
requires the GPU's GDF to match AVM's on every pixel for all 36 filter table
sets. `wasm/build.sh` writes `wasm/build/av2dec.<sha8>.wasm`.

## Numbers so far

Measured on an M3 Max with other work running, so read them as approximate.

- **Encoder, speed 3, one core, 600px tile:** 37.9 s pristine-with-SIMDe to
  30.7 s with patch 0006. AVM cleared a 16 KB scratch buffer on every forward
  transform, which was 20% of the encode.
- **GDF on a 1600x2400 frame:** 211 ms in AVM's C on one core, 3.7 ms in NEON
  across all cores, about 0.8 ms on the GPU.
- **AV2 tools speed 6 switches off, 2026-10-16.** At speed 6 through libavif,
  AVM encodes with no 3-way or uneven 4-way partitions, no TCQ, parity hiding,
  loop restoration or DIP, and it never searches the IST/CCTX transforms.
  Patch 0007 turns each back on with one bit of `-a tool-unlock=`. Scored at
  matched bytes against aadhar.sh's shipped AVIF on 600px tiles: 20 tiles
  screened, then the two winners confirmed on 20 held-out tiles, weighted to
  the 162 tiles that site ships. On the held-out set, plain speed 6 ties AVIF
  (+0.03 ssimulacra2). TCQ alone adds +0.53 and the IST/CCTX search +0.55;
  together (`tool-unlock=6`) they reach +1.02, and AV2 wins about 148 of
  162 tiles, up from 116. Butteraugli agrees on 19 of 20. Partitions, parity
  hiding, loop restoration, the intra-mode mask and DIP each moved the screen
  by +0.07 or less. TCQ and IST/CCTX cost the decoder nothing measurable;
  loop restoration costs it about 4x on one core. The encoder pays: one 600px
  tile with qmseg on took 12 s at speed 6 and 33 s with `tool-unlock=6`, 37 s
  at speed 3 and 145 s with it.
- **Measured and dropped:** `-mcpu=native` (no change) and more than four
  encoder threads per tile (no faster).

## Licence

This repo's own files are BSD-2-Clause (see `LICENSE`). Nothing upstream is
vendored. The build scripts fetch libavif (BSD-2-Clause), AVM
(BSD-3-Clause-Clear plus the Alliance for Open Media Patent License 1.0) and
SIMDe (MIT) at pinned commits. The patches in `encoder/patches/` modify AVM,
so they carry AVM's terms.
