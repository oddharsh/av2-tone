# rust

`av2-tone`, a safe-Rust AV2 still-image decoder, and `av2-check`, the harness
that holds it to pristine AVM 1.0.0. The [top-level README](../README.md) has
the crate's rules and status; this file covers what's built and how it's
checked.

```bash
rust/ci.sh                    # fmt, cargo test, clippy -D warnings
rust/oracle/build.sh          # pristine AVM, decoder only, plus av2oracle and av2trace
bun rust/corpus.ts            # 57 stills into rust/build/corpus (needs encoder/build.sh --tuned)
cargo run --release --manifest-path rust/Cargo.toml -p av2-check -- --headers rust/build/corpus
cargo run --release --manifest-path rust/Cargo.toml -p av2-check -- --entropy 300
cargo run --release --manifest-path rust/Cargo.toml -p av2-check -- --trace --coverage rust/build/corpus
bun rust/gen/cdf.ts           # regenerate the CDF tables (after oracle/build.sh)
bun rust/gen/defs.ts          # regenerate AVM's constants and lookup tables
```

## Scope

One intra key frame in a single-picture sequence: what this repo's encoder
writes for a still at speed 6, with `tool-unlock=6` and aadhar.sh's qmseg
flags. Anything outside it is refused as `Unsupported` with the feature named:
multi-frame headers, film grain, loop restoration,
quantization-matrix OBUs, more than one tile group, `TX_MODE_SELECT`,
signalled 3-way and uneven 4-way partitions, delta q, lossless segments and
intra DIP. Speed 6 writes none of these.

## What's built, in pipeline order

- **Headers:** the AVIF container, OBU framing, the sequence header and the key
  frame's header, tile grid included.
- **The entropy decoder:** AVM's multi-symbol arithmetic decoder (`entdec.c`),
  its bool, literal, bypass and unary reads, `tell` and `tell_frac`, and AV2's
  CDF adaptation. In AV2, unlike AV1, each CDF carries its own counter and
  three per-interval rates.
- **The default CDFs:** `gen/cdf.ts` reads `FRAME_CONTEXT`'s layout out of
  AVM's headers and has a C probe run AVM's own `av2_setup_past_independence`
  for every base_qindex bucket and `reduced_tx_part_set`. The generated tables
  are checked in, and the generator refuses an AVM tree that's dirty or not at
  v1.0.0. `gen/defs.ts` takes the enums, `#define`s and lookup tables the
  parse needs (scan orders included) out of AVM the same way.
- **Tiles:** each tile's bytes (tile-size fields included) and the frame
  context it starts from.
- **The tile parse:** every symbol in a still. That covers partitions (with
  the semi-decoupled luma and chroma trees AV2 uses at 64x64) and mode info
  (intra modes, CFL and MHCCP, MRL, FSC, palette, intra block copy and its
  vector, segment ids, CDEF, CCSO and GDF flags). It also covers transform
  types (with the IST secondary transform and CCTX) and every coefficient: end
  of block, base and low-range levels, signs, Golomb-Rice high ranges, TCQ
  states, parity hiding, and the forward-skip path for identity transforms.
  AVM interleaves coefficients with modes block by block, so the parse can't
  stop short of them.

The pixels don't match yet, because nothing is reconstructed.

## How it's checked

**`av2oracle`** decodes with AVM as built. It borrows the decoder through
AVM's public thread-worker hook, so AVM needs no patch, and reads the parsed
headers, tile buffers and starting `FRAME_CONTEXT` out of AVM's own structs.
It also writes the planes as raw 16-bit samples. `av2-check` diffs all of it
and prints the first pixel difference as (plane, x, y). `--headers` lets pixel
differences through while reconstruction is being built; every other check
still fails the run.

**`av2trace`** is the same pristine source built with `CONFIG_ACCOUNTING`, a
stock build option that logs every symbol AVM reads with its value, coding
mode, cost in 1/65536 bits and block. `av2-check --trace` names the first
symbol where the two parses part ways by the AVM function and line that read
it, and `--coverage` lists every read site the corpus reached. Four stills keep
`av2trace --digest`'s symbol count and hash in `av2-tone/tests/fixtures`, so
`cargo test` checks the whole parse without AVM installed.

**`av2-check --entropy`** has AVM's own encoder (`entenc.c`) write random
streams over every alphabet size from 2 to 16, with adaptation on and off. The
Rust decoder must recover every symbol, match AVM's `tell` after every
operation, and end with AVM's CDFs.

**The corpus** is the fixture frame at 8 and 10 bits, 4:2:0 and 4:4:4, and qp
40, 105 and 180. Each runs at speed 6 four ways: plain, with `tool-unlock=6`, with qmseg,
and with both. Four full frames add 2x2 tiles at 64, 128 and 256px
superblocks. Three crops add 64px superblocks (alone and tiled) and
`reduced-tx-part-set`. Two more switch trellis quantization off, which drops
TCQ from the sequence header: for a still, AVM forces TCQ on whenever the
sequence allows it, and parity hiding only runs without it. A rerun of
`corpus.ts` reproduces every file byte for byte.
