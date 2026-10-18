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
| `encoder/` | builds libavif with AVM 1.0.0 at a pinned commit, plus a tuned build: eleven encoder patches and AVM's AVX2 kernels compiled for arm64 through [SIMDe](https://github.com/simd-everywhere/simde) |
| `fast-decode/` | a native AV2 decoder for this machine: AVM plus NEON GDF, GDF across cores, and deblocking behind reconstruction, checked bit for bit against pristine AVM |
| `wasm/` | compiles AVM's decoder to a standalone WebAssembly module with no Emscripten glue (1.76 MB, about 430 KB Brotli) |
| `metal/` | AV2's GDF loop filter on the Apple GPU, checked against AVM's own C kernels |
| `fixture/` | a deterministic synthetic test frame, so nothing here depends on a photo |
| `demo/` | encodes your photo as AV1 and AV2 at the same byte count, races the encoders and the decoders (AVM, the fast decoder, dav2d, dav1d) on it, and shows both images on a local page |
| `rust/` | `av2-tone`, a safe-Rust still-image decoder, and the harness that checks it against pristine AVM on every file of a 57-still corpus. It parses every header and every tile symbol, matching AVM's own trace on all 57; reconstruction comes next |

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

## The safe-Rust decoder

`rust/av2-tone` is the crate this repo is named for: an AV2 still-image
decoder with no dependencies beyond std, checked against AVM 1.0.0 the same
way everything else here is. It has three rules.

- **No unsafe.** `#![forbid(unsafe_code)]` sits at the crate root and in the
  workspace lints, so nothing can opt back in.
- **Every bitstream read is bounds-checked and returns a `Result`.** The bit
  reader, the OBU walker and the container parser all return `Error` where
  AVM would longjmp out. Clippy refuses unchecked indexing, `unwrap`, `expect`
  and `panic!` in the decoder.
- **A panic is a bug.** Malformed input ends in `Truncated`, `Invalid` or
  `Unsupported`, never a crash. `cargo test` holds it to that on four real
  stills: it cuts each at every length, and flips every header bit and every
  61st bit of tile data, then parses every tile in full. The harness reports
  any panic as a failure. A one-off run over all 57 corpus stills (148,029
  flips, about 3,000 per file) found none.

The scope is one intra key frame in a single-picture sequence: what this
repo's encoder writes for a still at speed 6, with `tool-unlock=6` and
aadhar.sh's qmseg flags. Anything outside it is refused as `Unsupported`
with the feature named (multi-frame headers, film grain, loop restoration,
quantization-matrix OBUs, more than one tile group, `TX_MODE_SELECT`,
signalled 3-way and uneven 4-way partitions, delta q, lossless segments,
intra DIP) rather than misread. Speed 6 writes none of these.

Built so far, in pipeline order:

- **Headers.** The AVIF container, OBU framing, the sequence header and the
  key frame's header, tile grid included. On all 57 stills in the corpus,
  every parsed field matches AVM's, down to the header's length in bits.
- **The entropy decoder.** AVM's multi-symbol arithmetic decoder (`entdec.c`),
  its bool, literal, bypass and unary reads, `tell` and `tell_frac`, and AV2's
  CDF adaptation. In AV2, unlike AV1, each CDF carries its own counter and
  three per-interval rates. `av2-check --entropy` has AVM's own encoder
  (`entenc.c`) write random streams over every alphabet size from 2 to 16,
  with adaptation on and off. The Rust decoder must recover every symbol,
  match AVM's `tell` after every operation, and end with AVM's CDFs. On 300
  streams (1.2 million operations), it does.
- **The default CDFs.** `rust/gen/cdf.ts` reads `FRAME_CONTEXT`'s layout out of
  AVM's headers and has a C probe run AVM's own `av2_setup_past_independence`
  for every base_qindex bucket and `reduced_tx_part_set`. The generated tables
  (171 CDF fields, 17,513 values) are checked in. The generator refuses an AVM
  tree that is dirty or not at v1.0.0.
- **Tiles.** Each tile's bytes (tile-size fields included) and the frame
  context it starts from. On all 57 stills, the tiles sit where AVM puts them,
  and the starting context matches AVM's `FRAME_CONTEXT` byte for byte.
- **The tile parse.** Every symbol in a still: partitions (with the
  semi-decoupled luma and chroma trees AV2 uses at 64x64), mode info (intra
  modes, CFL and MHCCP, MRL, FSC, palette, intra block copy and its vector,
  segment ids, CDEF, CCSO and GDF flags), transform types (with the IST
  secondary transform and CCTX), and every coefficient (end of block, base
  and low-range levels, signs, Golomb-Rice high ranges, TCQ states, parity
  hiding, and the forward-skip path for identity transforms). AVM interleaves
  coefficients with modes block by block, so this step can't stop short of
  them. `av2-check --trace` diffs it against pristine AVM built with
  `CONFIG_ACCOUNTING`, which logs every symbol AVM reads with its value,
  coding mode, cost in 1/65536 bits and block. On all 57 stills, all 5,614,124
  symbols match, across 107 of AVM's read sites. `rust/gen/defs.ts` takes
  the enums, `#define`s and lookup tables the parse needs (scan orders
  included) out of AVM the way `cdf.ts` takes the CDFs.

The pixels don't match yet, because nothing is reconstructed. Next comes
dequantization, the inverse transforms (IST and CCTX included), intra
prediction with CFL and intra block copy, then deblocking, CDEF, CCSO, GDF
and loop restoration.

```bash
rust/ci.sh                    # fmt, cargo test, clippy -D warnings
rust/oracle/build.sh          # pristine AVM, decoder only, plus av2oracle and av2trace
bun rust/corpus.ts            # 57 stills into rust/build/corpus (needs --tuned)
cargo run --release --manifest-path rust/Cargo.toml -p av2-check -- --headers rust/build/corpus
cargo run --release --manifest-path rust/Cargo.toml -p av2-check -- --entropy 300
cargo run --release --manifest-path rust/Cargo.toml -p av2-check -- --trace --coverage rust/build/corpus
bun rust/gen/cdf.ts           # regenerate the CDF tables (after oracle/build.sh)
bun rust/gen/defs.ts          # regenerate AVM's constants and lookup tables
```

`av2oracle` decodes with AVM as built. It reads the parsed headers, tile
buffers and starting `FRAME_CONTEXT` out of AVM's own structs: it borrows the
decoder through AVM's public thread-worker hook, so AVM needs no patch. It
also writes the planes as raw 16-bit samples. `av2-check` diffs all of it, and
prints the first pixel difference as (plane, x, y). `--headers` lets pixel
differences through while reconstruction is being built; every other check
still fails the run. The corpus is the fixture frame at 8 and 10 bits,
4:2:0 and 4:4:4, qp 40, 105 and 180, at speed 6 plain, with `tool-unlock=6`,
with qmseg and with both. Four full frames add 2x2 tiles at 64, 128 and 256px
superblocks, and three more crops add 64px superblocks (alone and tiled) and
`reduced-tx-part-set`. Two more switch trellis quantization off, which drops
TCQ from the sequence header: for a still, AVM forces TCQ on whenever the
sequence allows it, and parity hiding only runs without it. A rerun of
`corpus.ts` reproduces every file byte for byte.

`av2trace` is the second oracle: the same pristine source with
`CONFIG_ACCOUNTING`, a stock build option. `av2-check --trace` names the
first symbol where the two parses part ways by the AVM function and line that
read it, and `--coverage` lists every read site the corpus reached. Four
stills keep `av2trace --digest`'s symbol count and hash in
`rust/av2-tone/tests/fixtures`, so `cargo test` checks the whole parse
without AVM installed.

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
- **qm-file (patch 0011), 2026-10-18.** AV2 can send its own quantization
  matrices in a QM OBU, which AV1 cannot, and AVM has the control for it but
  no way to feed one from a file. Patch 0011 reads one: per level, the
  inverse weights (32 flat) of the Y, U and V 8x8, 8x4 and 4x8 matrices. Three
  things bound what it can do. A custom matrix shapes only transforms up to
  8x8; larger ones keep AVM's own for that level. AV2 derives a 4x4 matrix by
  downsampling the 8x8 one, and AVM's built-in 4x4 tables differ from that at
  521 of 720 entries, so even AVM's own matrices sent back change the coding
  (level 8 on a 320px crop: 44 B smaller, ssimulacra2 -0.13). And the OBU
  costs 29 to 150 B a level on that crop (about 1.3 KB for all 15, 5% of the
  file), so a file should carry only the levels the frame uses: replacing an
  unused level adds the OBU and leaves every pixel alone, which the build
  gate checks. `avmenc --test-decode` passes on AVM's own matrices and on a
  steeper set.
- **jpegli seg-maps (patch 0010), 2026-10-18.** jpegli scales each 8x8
  block's quantizer step by 1/qf, its masking field, which on AV2's table (24
  qindex an octave) is an offset of -24 * s * log2(qf / geomean(qf)): texture
  goes coarser, flat areas finer. The 8x8 field spreads 0.12 to 0.30 in log2
  standard deviation across the 8 dense crops, so at jpegli's own scaling
  (s = 1) the offsets span about +-5 to +-16 qindex. A seg-map replaces qmseg,
  so each map also carries qmseg's QM split: levels whose blocks are mostly
  textured (qf < 0.45) take qmseg's level-12 QM. Offsets are quantized to a
  few evenly spaced levels, numbered finest first since a block takes the
  smallest id under it. Same crops, 4 qps, speed 6 with `tool-unlock=6`,
  BD-rate against the shipping qmseg config (positive is bigger files):

  | seg-map | ssimulacra2 BD-rate | crops better | butteraugli BD-rate |
  |---|---|---|---|
  | qmseg's split at 8x8, no offsets | +1.11% | 1 of 8 | +7.3% |
  | s = 0.5, 8 levels | +2.15% | 1 of 8 | +5.8% |
  | s = 1, 8 levels | +1.04% | 2 of 8 | +5.1% |
  | s = 1, 8 levels, no QM split | +1.85% | 2 of 8 | +3.0% |
  | s = 1, 16 levels (`enable-ext-seg`) | +2.48% | 1 of 8 | +7.1% |
  | s = 2, 8 levels | +0.12% | 5 of 8 | +4.6% |
  | s = 3, 8 levels | +1.35% | 3 of 8 | +3.4% |
  | s = 4, 8 levels | +4.85% | 2 of 8 | +11.0% |
  | s = 2, 4 levels | -0.48% | 3 of 8 | -0.0% |
  | s = 2, 3 levels | -0.87% | 6 of 8 | +3.5% |
  | s = 2, 2 levels | +2.04% | 2 of 8 | +7.8% |
  | s = 2, 4 levels, QM split by qmseg's 64px units | +0.34% | 3 of 8 | +2.4% |

  Moving qmseg's split from 64px units to 8x8 blocks costs 1.1% on its own,
  the price of a segment id per block. The offsets buy some of it back, most
  at twice jpegli's own strength, and fewer levels cost less, down to 3: at
  2, most crops lose qmseg's QM split, since no level is mostly textured.
  3 levels is the best ssimulacra2 reading of the series (-0.87%, 6 of 8
  crops better), but butteraugli reads it +3.5%; 4 levels is the one map
  neither metric marks worse. A rerun of 4 levels reproduced -0.48% and
  -0.02% exactly. Neither holds on 8 held-out photos at speed 3 (a dense
  320px crop each: 2 full-size camera photos and 6 web-size JPEGs):

  | seg-map, held out | ssimulacra2 BD-rate | photos better | butteraugli BD-rate |
  |---|---|---|---|
  | s = 2, 3 levels | +0.49% | 2 of 8 | +0.4% |
  | s = 2, 4 levels | +0.02% | 3 of 8 | +1.4% |

  Single photos swing up to 3.5% on ssimulacra2 and 16% on butteraugli,
  more than either average. Of the two camera photos, samsung5 loses under
  both maps (+1.6% and +1.1%) and samsung6 is flat (+0.1% and -0.6%).
  qmseg stays.
- **jpegli matrices in a qm-file (patch 0011), 2026-10-18.** jpegli's base
  YCbCr tables give one step per 8x8 DCT coefficient and plane; as AV2
  inverse weights they are 32 * (Q[k] / Q[0])^alpha, alpha 1 being jpegli's
  own shape, with 8x4 and 4x8 sampled from the 8x8 at equal frequency (the
  4-point index v is the 8-point index 2v, as AV2 derives 4x4). Each file
  carries only the levels its encode uses: qm-curve=1 puts the frame on level
  9 at qp 80 and 95 and level 8 at 110 and 125, and qmseg's textured
  segments use level 12. Same crops and method, against the shipping qmseg
  config:

  | qm-file | ssimulacra2 BD-rate | crops better | butteraugli BD-rate |
  |---|---|---|---|
  | AVM's own matrices (null) | +1.41% | 0 of 8 | +4.7% |
  | jpegli, alpha 0.5 | +5.62% | 0 of 8 | +7.5% |
  | jpegli, alpha 0.75 | +7.20% | 0 of 8 | +12.4% |
  | jpegli, alpha 1 | +7.37% | 0 of 8 | +12.2% |
  | jpegli, alpha 0.5, its distance-3 shape | +5.02% | 0 of 8 | +9.2% |

  Sending AVM's own matrices back costs 1.4%: the QM OBU (about 100 to 250 B
  against 10 to 25 KB crops) and the 4x4 matrices AV2 derives from the 8x8,
  which differ from AVM's built-in ones. Every jpegli shape loses beyond
  that, more the steeper it is. The reason shows when AVM's matrices are
  read as a function of frequency (index k of an N-point transform at
  k / 2N cycles a pixel): its 8x8 and its 32x32 agree to 0.07 octaves rms,
  one curve across sizes, and since a qm-file reaches only the transforms up
  to 8x8, 16x16 and up keep it. Fitted to that curve where the two overlap,
  jpegli's shape needs alpha of only 0.30 (luma, level 8), 0.22 (level 9)
  and 0.02 (level 12, which AVM keeps nearly flat), and still misses it by
  0.28 octaves rms on luma at level 8: jpegli rises early and plateaus, AVM
  rises late. So alpha 0.5 and up weights small transforms 2 to 3x more
  steeply than large ones at the same frequency. Matrices at the fitted
  alpha, per level and plane, against the same baseline:

  | qm-file | ssimulacra2 BD-rate | crops better | butteraugli BD-rate |
  |---|---|---|---|
  | fitted, levels 8/9 and 12 | +1.53% | 1 of 8 | +2.2% |
  | fitted to the distance-3 shape | +1.93% | 0 of 8 | +5.9% |
  | fitted, frame level (8/9) only | +1.73% | 0 of 8 | +4.1% |
  | fitted, level 12 only | +0.04% | 2 of 8 | +0.6% |
  | alpha 0.5, level 12 only | +1.68% | 0 of 8 | +5.0% |
  | alpha 0.5, frame level only | +3.64% | 0 of 8 | +8.2% |

  Fitting closes most of the gap to the null file (+1.53% against +1.41%)
  but never passes AVM's own matrices. Level 12 alone ties shipping because
  its fitted luma alpha (0.02) makes it nearly AVM's level 12, and it only
  touches the 3 crops qmseg splits (cat -0.15%, geese -0.08%, iced coffee
  +0.52%). Steeper level-12 matrices cost the cat 11%. On the 8 held-out
  photos at speed 3:

  | qm-file, held out | ssimulacra2 BD-rate | photos better | butteraugli BD-rate |
  |---|---|---|---|
  | AVM's own matrices (null) | +2.01% | 0 of 8 | +1.5% |
  | fitted, level 12 only | +0.56% | 1 of 8 | -1.0% |
  | fitted, levels 8/9 and 12 | +2.65% | 1 of 8 | +0.3% |

  The null file costs the samsung5 camera photo 8.3%, the most of any
  photo, which points at the derived 4x4 matrices: a detailed crop codes
  many 4x4 transforms. No jpegli shape beats AVM's matrices, and the cost of
  sending any matrix at all (that 4x4 mismatch plus the OBU) is larger than
  what a shape could buy here. Letting a qm-file set the 4x4 matrix too
  would isolate the shape from that cost; nothing ships from this.
- **Measured and dropped:** `-mcpu=native` (no change) and more than four
  encoder threads per tile (no faster).

## Licence

This repo's own files are BSD-2-Clause (see `LICENSE`). Nothing upstream is
vendored. The build scripts fetch libavif (BSD-2-Clause), AVM
(BSD-3-Clause-Clear plus the Alliance for Open Media Patent License 1.0) and
SIMDe (MIT) at pinned commits. The patches in `encoder/patches/` modify AVM,
so they carry AVM's terms.
