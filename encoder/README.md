# encoder

`build.sh` builds libavif with AVM 1.0.0 at a pinned commit, and with
`--tuned` a second pair carrying the 12 patches in `patches/` and AVM's AVX2
kernels compiled for arm64 through SIMDe. Every patch is encoder-only, and all but
0006 (a speed fix) are off by default. The reference decoder reads their output
pixel-identically.
`build.sh`'s header lists what each one does. `experiments/` holds patches
that change what the decoder reads, so they never join the build.

This file records what 0006 to 0013 measured.

## Method

Unless an entry says otherwise:

- **Timings:** an M3 Max with other work running, so read them as approximate.
- **Dense crops:** the 320px window with the most fine detail in each of 8
  public-domain photos.
- **Held out:** a dense 320px crop from each of 8 other photos (2 full-size
  camera photos, samsung5 and samsung6, and 6 web-size JPEGs), at speed 3.
- **Encodes:** AV2 at speed 6 with `tool-unlock=6`, 4 fixed qps per crop.
- **Score:** ssimulacra2 and butteraugli BD-rate against the shipping qmseg
  config. Positive means bigger files at equal score.

## 0006 fwd-txfm: encode speed

AVM cleared a 16 KB scratch buffer on every forward transform, 20% of the
encode. At speed 3, one core, a 600px tile: 37.9 s pristine (with SIMDe), 30.7 s
with 0006.

## 0007 tool-unlock (2026-10-16)

At speed 6 through libavif, AVM encodes with no 3-way or uneven 4-way
partitions, no TCQ, parity hiding, loop restoration or DIP, and never searches
the IST/CCTX transforms. 0007 turns each back on with one bit of
`-a tool-unlock=`.

Scored at matched bytes against aadhar.sh's shipped AVIF on 600px tiles: 20
tiles screened, then the two winners confirmed on 20 held-out tiles, weighted
to the 162 tiles the site ships. On the held-out set, plain speed 6 ties AVIF
(+0.03 ssimulacra2). TCQ alone adds +0.53 and the IST/CCTX search +0.55;
together (`tool-unlock=6`) they reach +1.02, and AV2 wins about 148 of 162
tiles, up from 116. Butteraugli agrees on 19 of 20.

Partitions, parity hiding, loop restoration, the intra-mode mask and DIP each
moved the screen by +0.07 or less. TCQ and IST/CCTX cost the decoder nothing
measurable; loop restoration costs it about 4x on one core. The encoder pays:
one 600px tile with qmseg took 12 s at speed 6 and 33 s with `tool-unlock=6`,
37 s at speed 3 and 145 s with it.

## 0008 iq-deltaq (2026-10-17)

libaom's tune=iq gives the AV1 inside AVIF two quantizer balances that an AV2
still gets neither of: DC finer than AC (AV1's DC table runs at 0.84x the AC
step around qindex 100) and coarser 4:4:4 chroma AC. AVM codes DC on its AC
table, and its `enable-chroma-deltaq` hook sets nothing. 0008 adds both as
frame-level deltas, read off libaom's tables at the AV1 qindex with the same
AC step.

This entry scores against AVIF instead, at aadhar.sh's shipping flags (libaom
3.15.1, `-q 50/63/80 -d 10 --speed 2 --yuv 444`). It ran on a 4-core x86
Linux box without SIMDe, on 24 crop-budget pairs from the dense crops. AV2 ran at speed 6 with qmseg's flags, bracketed on qp and
interpolated to the AVIF's exact bytes. Every file decoded at 16 bits and
rounded once.

| AV2 config | ssimulacra2 | wins | butteraugli |
|---|---|---|---|
| qmseg flags only | -0.47 | 8 of 24 | +0.083 |
| + iq-deltaq=1 | -0.53 | 6 of 24 | +0.112 |
| + tool-unlock=6 | +0.38 | 18 of 24 | -0.042 |
| + both | +0.38 | 18 of 24 | -0.042 |

0007 carries the result here too: from losing two thirds of these calls to
winning three quarters, at every budget tested (+0.82, +0.26 and +0.07 at
0.69, 1.07 and 1.80 bits per pixel). 0008 nets nothing. Luma's 8x8 block-mean
error falls on 23 of 24 crops, chroma's rises on all 24, and ssimulacra2 reads
the trade as a tie (-0.06, and -0.00 beside tool-unlock). Off by default.

## 0009 lambda-map (2026-10-17)

AVM decides every block by squared error. Its only perceptual weighting is
tune=ssim's per-16x16 RD multiplier curve, a fixed function of local variance
that libavif turns on for every AV2 encode. 0009 dumps that curve and the
jpegli masking field, and takes any map in its place. BD-rate here is against
tune=ssim's own curve:

| lambda map | ssimulacra2 BD-rate | crops better | butteraugli BD-rate |
|---|---|---|---|
| flat (no weighting) | +6.54% | 0 of 8 | +6.0% |
| tune=ssim curve, power 1.5 | +0.93% | 3 of 8 | +9.3% |
| tune=ssim curve, power 0.5 | +2.20% | 0 of 8 | +4.4% |
| jpegli field qf^-3 | +1.70% | 3 of 8 | -2.0% |
| jpegli field qf^-6 | +3.38% | 2 of 8 | +5.9% |
| jpegli field qf^-9 | +15.66% | 1 of 8 | +39.3% |
| tune=ssim times qf^-3 | +1.36% | 3 of 8 | +7.3% |

The curve is worth 6.5%, and the flat map lands exactly on tune=psnr's +6.54%
from the same crops, so the weighting is all tune=psnr gives up. Nothing beats
it on ssimulacra2: it sits at the optimum of its own strength. jpegli's field, about 6x flatter in log terms (0.07
against 0.41 standard deviation), loses at every strength. qf^-3 is the one
map butteraugli prefers (-2.0%), so the two metrics disagree about how much
texture masks. Off by default.

## 0010 seg-map (2026-10-17)

A segment id per 8x8 luma block, each segment with a qindex delta and a luma QM
level, so block-level quantization can be decided outside AVM. That allows up
to 8 segments, 16 with `enable-ext-seg=1`, within AV2's limit of 4 QM sets. A
map of qmseg's own split reproduces qmseg's files byte for byte on all 8
crops.

Building it found an AVM bug. The final encode of a block kept the quantizer
of the last RD candidate, so in a superblock mixing segment qindexes, a block
was quantized at one qindex and signalled at another (`avmenc --test-decode`:
mismatch at Y[2, 126], about 10 dB of luma PSNR on the fixture). AVM resets the
quantizer only for an AQ mode, and its variance AQ never mixed segments on
these crops. 0010 resets it for a seg-map too. qmseg's segments share a
qindex, so it was never affected (`--test-decode` passes at speeds 3 and 6).

## jpegli seg-maps on 0010 (2026-10-18)

jpegli scales each 8x8 block's quantizer step by 1/qf, its masking field. On
AV2's table (24 qindex an octave) that's an offset of
-24 * s * log2(qf / geomean(qf)): texture goes coarser, flat areas finer. The
field's log2 standard deviation runs 0.12 to 0.30 across the dense crops, so
at jpegli's own scaling (s = 1) the offsets span about +-5 to +-16 qindex.

A seg-map replaces qmseg, so each map also carries qmseg's QM split: levels
whose blocks are mostly textured (qf < 0.45) take qmseg's level-12 QM. Offsets
are quantized to a few evenly spaced levels, numbered finest first, since a
block takes the smallest id under it.

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

Moving qmseg's split from 64px units to 8x8 blocks costs 1.1% on its own, the
price of a segment id per block. The offsets buy some back, most at s = 2, and
fewer levels cost less, down to 3. At 2 levels most crops lose the QM split,
since no level is mostly textured. 3 levels reads best on ssimulacra2 (-0.87%,
6 of 8 better), but butteraugli reads it +3.5%. 4 levels is the one map
neither metric marks worse, and a rerun reproduced its -0.48% and -0.02%
exactly. Neither holds on the held-out photos:

| seg-map, held out | ssimulacra2 BD-rate | photos better | butteraugli BD-rate |
|---|---|---|---|
| s = 2, 3 levels | +0.49% | 2 of 8 | +0.4% |
| s = 2, 4 levels | +0.02% | 3 of 8 | +1.4% |

Single photos swing up to 3.5% on ssimulacra2 and 16% on butteraugli, more
than either average. samsung5 loses under both maps (+1.6% and +1.1%) and
samsung6 is flat (+0.1% and -0.6%). qmseg stays.

## 0011 qm-file (2026-10-18)

AV2, unlike AV1, can send its own quantization matrices in a QM OBU. AVM has
the control for it but no way to feed one from a file, so 0011 reads one. Per
level, the file holds the inverse weights (32 is flat) of the Y, U and V 8x8,
8x4 and 4x8 matrices. Three things bound it:

- A custom matrix shapes only transforms up to 8x8. Larger ones keep AVM's own
  for that level.
- AV2 derives a 4x4 matrix by downsampling the 8x8, and AVM's built-in 4x4
  tables differ from that at 521 of 720 entries. So even AVM's own matrices
  sent back change the coding (level 8 on a 320px crop: 44 B smaller,
  ssimulacra2 -0.13).
- The OBU costs 29 to 150 B a level on that crop (about 1.3 KB for all 15, 5%
  of the file), so a file should carry only the levels the frame uses.
  Replacing an unused level adds the OBU and leaves every pixel alone, which
  the build gate checks.

`avmenc --test-decode` passes on AVM's own matrices and on a steeper set.

## jpegli matrices in a qm-file (2026-10-18)

jpegli's base YCbCr tables give one step per 8x8 DCT coefficient and plane. As
AV2 inverse weights they become 32 * (Q[k] / Q[0])^alpha, where alpha 1 is
jpegli's own shape. 8x4 and 4x8 are sampled from the 8x8 at equal frequency
(the 4-point index v is the 8-point index 2v, as AV2 derives 4x4). Each file
carries only the levels its encode uses. qm-curve=1 puts the frame on level 9
at qp 80 and 95 and level 8 at 110 and 125; qmseg's textured segments use
level 12.

| qm-file | ssimulacra2 BD-rate | crops better | butteraugli BD-rate |
|---|---|---|---|
| AVM's own matrices (null) | +1.41% | 0 of 8 | +4.7% |
| jpegli, alpha 0.5 | +5.62% | 0 of 8 | +7.5% |
| jpegli, alpha 0.75 | +7.20% | 0 of 8 | +12.4% |
| jpegli, alpha 1 | +7.37% | 0 of 8 | +12.2% |
| jpegli, alpha 0.5, its distance-3 shape | +5.02% | 0 of 8 | +9.2% |

Sending AVM's own matrices back costs 1.4%, almost all of it the QM OBU (118 B
for one level, 182 B for two, against 6 to 35 KB crops). Every jpegli shape
loses beyond that, more the steeper it is.

The reason shows when AVM's matrices are read as a function of frequency
(index k of an N-point transform at k / 2N cycles a pixel). Its 8x8 and its
32x32 agree to 0.07 octaves rms, one curve across sizes, and transforms of
16x16 and up keep that curve because a qm-file can't reach them. Fitted to
the curve, jpegli's shape needs alpha of only 0.30 (luma, level 8), 0.22
(level 9) and 0.02 (level 12, which AVM keeps nearly flat). It still misses by
0.28 octaves rms on luma at level 8, because jpegli rises early and plateaus
while AVM rises late. So alpha 0.5 and up weights small transforms 2 to 3x
more steeply than large ones at the same frequency. Matrices at the fitted
alpha, per level and plane:

| qm-file | ssimulacra2 BD-rate | crops better | butteraugli BD-rate |
|---|---|---|---|
| fitted, levels 8/9 and 12 | +1.53% | 1 of 8 | +2.2% |
| fitted to the distance-3 shape | +1.93% | 0 of 8 | +5.9% |
| fitted, frame level (8/9) only | +1.73% | 0 of 8 | +4.1% |
| fitted, level 12 only | +0.04% | 2 of 8 | +0.6% |
| alpha 0.5, level 12 only | +1.68% | 0 of 8 | +5.0% |
| alpha 0.5, frame level only | +3.64% | 0 of 8 | +8.2% |

Fitting closes most of the gap to the null file (+1.53% against +1.41%) but
never passes AVM's own matrices. Level 12 alone ties shipping because its
fitted luma alpha (0.02) makes it nearly AVM's level 12, and it only touches
the 3 crops qmseg splits (cat -0.15%, geese -0.08%, iced coffee +0.52%).
Steeper level-12 matrices cost the cat 11%. Held out:

| qm-file, held out | ssimulacra2 BD-rate | photos better | butteraugli BD-rate |
|---|---|---|---|
| AVM's own matrices (null) | +2.01% | 0 of 8 | +1.5% |
| fitted, level 12 only | +0.56% | 1 of 8 | -1.0% |
| fitted, levels 8/9 and 12 | +2.65% | 1 of 8 | +0.3% |

No jpegli shape beats AVM's matrices, and none recovers the cost of sending a
matrix at all. The null file costs samsung5 8.3%, the most of any photo. The
next entry traces that to its OBU, on a file of a few KB at qp 125. Nothing
ships from this.

## What a qm-file costs: 0012, experiment 0013 (2026-10-19)

Besides the OBU, two things separated a qm-file of AVM's own matrices from
AVM's built-in ones:

- **Forward weights.** For a user matrix AVM computed the encoder's forward
  weights as 1024 / iwt truncated, while every predefined weight is that
  rounded (162,000 of 162,000). So 38% of entries came out different, and
  AVM's own matrices quantized differently. 0012 rounds. The decoder
  dequantizes with iwt alone, so the output stays standard AV2 and the
  reference decoder reads it pixel-identically. `qmround.c` checks it in
  `build.sh` (2,125 of 5,760 weights differ without 0012, none with it).
- **The 4x4.** The decoder derives it from the 8x8's even rows and columns,
  which misses AVM's own 4x4 by up to 0.23 octaves at levels 8 and 9, and the
  QM OBU has no room for one. `experiments/0013` adds an optional 4x4 per
  level to the OBU and the qm-file (y4x4, u4x4, v4x4). It changes what the
  decoder reads, so it stays out of the build.

With both, a qm-file of AVM's own matrices decodes pixel-identically to the
shipping encode on all 64 encodes below, which leaves the OBU as the only
cost:

| qm-file of AVM's own matrices | ssimulacra2 BD-rate | butteraugli BD-rate | held out (s2, bu) |
|---|---|---|---|
| as sent before (null) | +1.41% | +4.7% | +2.01%, +1.5% |
| with 0012 | +1.52% | +2.0% | +1.89%, +0.5% |
| with 0012, AVM's 4x4 in the 8x8 entries it samples | +1.42% | +3.9% | +2.37%, +4.2% |
| with 0012 and 0013's 4x4 line | +1.34% | +1.4% | +2.05%, +2.2% |

The last row is bytes alone: 110 to 189 B a file, 1.3% of these crops on
average. It's more on the small files at qp 125, which BD-rate weighs as
heavily as the large ones (samsung5's +8.8%). Fixing the forward weights and
the 4x4 moves ssimulacra2 by under 0.1%, inside the noise. Butteraugli moves
by up to 3 points on the training crops but either way held out. So the OBU
carries the whole cost, and its size is fixed.

aadhar.sh ships every photo at 600x600. Its full-tier AVIFs (AV1 today) run 24
KB at the median, 10 KB at the 10th percentile and 48 KB at the 90th. So 118
to 182 B is 0.5 to 0.8% of a median photo and up to 1.8% of a small one, about
half its share of these crops. A matrix shape has to beat that. 0012 stays in
the build because it makes a qm-file quantize as its matrices say.

## jpegli matrices at full size (2026-10-19)

The same qm-files on 8 of the site's own 600px photos (their shipped JPEG
tiers as the source), at speed 6 with 0012. This entry doesn't record whether
`tool-unlock=6` was on. AVM's own matrices sent the same way carry
the same OBU, so read each jpegli row against that one:

| qm-file, 600px photos | ssimulacra2 BD-rate | photos better | butteraugli BD-rate | bytes against shipping |
|---|---|---|---|---|
| AVM's own matrices | +0.48% | 1 of 8 | -1.2% | +176 B |
| fitted, levels 8/9 and 12 | +0.60% | 1 of 8 | -2.6% | -152 B |
| fitted, level 12 only | +0.35% | 0 of 8 | 0.0% | -16 B |
| alpha 0.5 | +4.55% | 0 of 8 | +1.0% | +971 B |

These encodes average 47 KB across the 4 qps, so the OBU costs 0.5% here
against 1.5% on the 320px crops. The fitted shape reads +0.12% against AVM's
own matrices on ssimulacra2 and 1.5 points better on butteraugli, while single
photos swing 1.5% and 10%: a tie. Level 12 alone comes closest to shipping
because two of the photos have no textured units and encode exactly as
shipping does; it trails on the other six. Alpha 0.5 loses on every photo.

The same arms at speed 3 and on the two 12 MP camera originals (samsung5 and
samsung6) started on 2026-10-19 and aren't in yet.
