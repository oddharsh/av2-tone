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
| `encoder/` | builds libavif with AVM 1.0.0 at a pinned commit, plus a tuned build: ten encoder patches and AVM's AVX2 kernels compiled for arm64 through [SIMDe](https://github.com/simd-everywhere/simde) |
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
- **iq-deltaq (patch 0008), and 0007 on dense crops, 2026-10-17.** AVM codes
  DC on its AC table and its `enable-chroma-deltaq` hook sets nothing, so an
  AV2 still gets neither of the quantizer balances libaom's tune=iq gives the
  AV1 inside AVIF: DC finer than AC (AV1's DC table runs at 0.84x the AC step
  around qindex 100) and 4:4:4 chroma AC coarser. Patch 0008 adds them as
  frame-level deltas, read off libaom's own tables at the AV1 qindex with the
  same AC step. Scored on a 4-core x86 Linux box without SIMDe, against AVIF
  at aadhar.sh's shipping flags (libaom 3.15.1, `-q 50/63/80 -d 10 --speed 2
  --yuv 444`), on the 320px window with the most fine detail in each of 8
  public-domain photos: 24 crop-budget pairs, AV2 at speed 6 with that
  site's qmseg flags, bracketed on qp and interpolated to the AVIF's exact
  bytes, every file decoded at 16 bits and rounded once. Against AVIF:

  | AV2 config | ssimulacra2 | wins | butteraugli |
  |---|---|---|---|
  | qmseg flags only | -0.47 | 8 of 24 | +0.083 |
  | + iq-deltaq=1 | -0.53 | 6 of 24 | +0.112 |
  | + tool-unlock=6 | +0.38 | 18 of 24 | -0.042 |
  | + both | +0.38 | 18 of 24 | -0.042 |

  0007 carries the result here as it did on the 600px tiles: from losing two
  thirds of these calls to winning three quarters, at every budget tested
  (+0.82, +0.26 and +0.07 at 0.69, 1.07 and 1.80 bits per pixel). 0008 does
  what it says and nets nothing: luma's 8x8 block-mean error falls on 23 of
  24 crops, chroma's rises on all 24, and ssimulacra2 reads the trade as a
  tie (-0.06, and -0.00 beside tool-unlock). It stays in, off by default, as
  the record of that.
- **lambda-map (patch 0009), 2026-10-17.** AVM decides every block by
  squared error, and tune=ssim's per-16x16 RD multiplier curve (a fixed
  function of local variance, which libavif turns on for every AV2 encode)
  is its only perceptual weighting. Patch 0009 dumps that curve and the
  jpegli masking field, and takes any map in its place. Screened at speed 6
  with `tool-unlock=6` on the same 8 dense crops, 4 fixed qps each, as
  ssimulacra2 BD-rate against tune=ssim's own curve (positive is bigger
  files at equal score):

  | lambda map | ssimulacra2 BD-rate | crops better | butteraugli BD-rate |
  |---|---|---|---|
  | flat (no weighting) | +6.54% | 0 of 8 | +6.0% |
  | tune=ssim curve, power 1.5 | +0.93% | 3 of 8 | +9.3% |
  | tune=ssim curve, power 0.5 | +2.20% | 0 of 8 | +4.4% |
  | jpegli field qf^-3 | +1.70% | 3 of 8 | -2.0% |
  | jpegli field qf^-6 | +3.38% | 2 of 8 | +5.9% |
  | jpegli field qf^-9 | +15.66% | 1 of 8 | +39.3% |
  | tune=ssim times qf^-3 | +1.36% | 3 of 8 | +7.3% |

  tune=ssim's curve is worth 6.5%, and the flat map lands exactly on
  tune=psnr's +6.54% from the same crops, so the weighting is all tune=psnr
  gives up. Nothing beats the curve on ssimulacra2: it sits at the optimum
  of its own strength, and jpegli's field, about 6x flatter in log terms
  (0.07 against 0.41 standard deviation), loses at every strength. qf^-3 is
  the one map butteraugli prefers (-2.0%): the two metrics disagree about
  how much texture masks. Off by default; the options stay so a map can be
  built outside AVM.
- **seg-map (patch 0010), 2026-10-17.** A segment id per 8x8 luma block,
  each segment with a qindex delta and a luma QM level, so block-level
  quantization can be decided outside AVM (up to 8 segments, 16 with
  `enable-ext-seg=1`, and AV2's limit of 4 QM sets). A map of qmseg's own
  split reproduces qmseg's files byte for byte on all 8 crops. Building it
  found an AVM bug: the final encode of a block keeps the quantizer of the
  last RD candidate, so inside a superblock that mixes segment qindexes a
  block was quantized at one qindex and signalled at another (`avmenc
  --test-decode`: mismatch at Y[2, 126]; about 10 dB of luma PSNR on the
  fixture). AVM resets the quantizer only for an AQ mode, and its variance
  AQ never mixed segments on these crops, so nothing had tripped it. 0010
  resets it for a seg-map too; qmseg, whose segments share a qindex, was
  never affected (`--test-decode` passes at speeds 3 and 6).
- **Measured and dropped:** `-mcpu=native` (no change) and more than four
  encoder threads per tile (no faster).

## Licence

This repo's own files are BSD-2-Clause (see `LICENSE`). Nothing upstream is
vendored. The build scripts fetch libavif (BSD-2-Clause), AVM
(BSD-3-Clause-Clear plus the Alliance for Open Media Patent License 1.0) and
SIMDe (MIT) at pinned commits. The patches in `encoder/patches/` modify AVM,
so they carry AVM's terms.
