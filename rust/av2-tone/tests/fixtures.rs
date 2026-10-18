//! Four stills from rust/corpus.ts, frozen with what AVM made of them
//! (av2oracle's seq.*, frame.*, tile and frame_context lines, and
//! av2trace --digest's count and hash of every symbol AVM read), so `cargo
//! test` checks the header parser, the tile split, the starting CDFs and the
//! whole tile parse against AVM without AVM installed. The full corpus runs
//! through av2-check.
//!
//!   crop-10b444-shipped-qp180           aadhar.sh's flags: qmseg, TCQ, IST, CCTX, intra block copy
//!   crop-8b420-base-qp105-sb64          64px superblocks, 8-bit 4:2:0
//!   crop-8b420-base-qp180-sb64-tiles2x2 four tiles, so three tile-size fields
//!   crop-10b444-base-qp105-notcq        no TCQ, so parity hiding
//!
//! All four also feed the robustness sweeps: every truncation, and every
//! single bit flip of the headers and a sample of the tile data, must come
//! back from the full parse as Ok or Err, never a panic.

// Test code: a panic here is the assertion, not a decoder bug.
#![allow(clippy::panic, clippy::indexing_slicing)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use av2_tone::cdf::FrameContext;

const FIXTURES: [(&str, &[u8], &str); 4] = [
    (
        "crop-10b444-shipped-qp180",
        include_bytes!("fixtures/crop-10b444-shipped-qp180.avif"),
        include_str!("fixtures/crop-10b444-shipped-qp180.avm.txt"),
    ),
    (
        "crop-8b420-base-qp105-sb64",
        include_bytes!("fixtures/crop-8b420-base-qp105-sb64.avif"),
        include_str!("fixtures/crop-8b420-base-qp105-sb64.avm.txt"),
    ),
    (
        "crop-8b420-base-qp180-sb64-tiles2x2",
        include_bytes!("fixtures/crop-8b420-base-qp180-sb64-tiles2x2.avif"),
        include_str!("fixtures/crop-8b420-base-qp180-sb64-tiles2x2.avm.txt"),
    ),
    (
        "crop-10b444-base-qp105-notcq",
        include_bytes!("fixtures/crop-10b444-base-qp105-notcq.avif"),
        include_str!("fixtures/crop-10b444-base-qp105-notcq.avm.txt"),
    ),
];

/// av2trace --digest for each fixture, in FIXTURES order.
const TRACES: [&str; 4] = [
    include_str!("fixtures/crop-10b444-shipped-qp180.trace.txt"),
    include_str!("fixtures/crop-8b420-base-qp105-sb64.trace.txt"),
    include_str!("fixtures/crop-8b420-base-qp180-sb64-tiles2x2.trace.txt"),
    include_str!("fixtures/crop-10b444-base-qp105-notcq.trace.txt"),
];

fn avm_lines<'a>(avm: &'a str, prefix: &'a str) -> impl Iterator<Item = &'a str> {
    avm.lines().filter(move |l| l.starts_with(prefix))
}

#[test]
fn headers_match_avm() {
    for (name, file, avm) in FIXTURES {
        let (headers, pic) = av2_tone::decode_avif(file).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ours = av2_tone::dump::dump(&headers);
        let theirs: Vec<&str> = avm.lines().filter(|l| l.starts_with("seq.") || l.starts_with("frame.")).collect();
        for (i, (a, b)) in ours.lines().zip(&theirs).enumerate() {
            assert_eq!(a, *b, "{name}: line {}", i + 1);
        }
        assert_eq!(ours.lines().count(), theirs.len(), "{name}: field count");
        assert_eq!((pic.width, pic.height), (250, 186), "{name}");
    }
}

#[test]
fn tiles_and_starting_cdfs_match_avm() {
    for (name, file, avm) in FIXTURES {
        let obus = av2_tone::avif::primary_item(file).unwrap_or_default();
        let (headers, group) = av2_tone::parse_obus(&obus).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ours: Vec<String> = av2_tone::tile::tile_buffers(&headers, group.data)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .iter()
            .map(|t| format!("tile {} {} {} {}", t.row, t.col, group.offset + t.offset, t.data.len()))
            .collect();
        assert_eq!(ours, avm_lines(avm, "tile ").collect::<Vec<_>>(), "{name}: tiles");

        let fc = FrameContext::default_for(headers.frame.quant.base_qindex, headers.seq.reduced_tx_part_set).unwrap();
        let hex: String = fc.to_avm_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let theirs = avm_lines(avm, "frame_context ").next().unwrap_or_default().trim_start_matches("frame_context ");
        // AVM's two padding bytes happen to be zero too, so the whole struct compares.
        assert!(hex == theirs, "{name}: starting FRAME_CONTEXT differs from AVM's");
    }
}

#[test]
fn tile_parse_matches_avm_trace() {
    for ((name, file, _), avm) in FIXTURES.iter().zip(TRACES) {
        let obus = av2_tone::avif::primary_item(file).unwrap_or_default();
        let t = av2_tone::parse::trace_obus(&obus);
        if let Err(e) = &t.result {
            panic!("{name}: {e}");
        }
        let ours = format!("digest {} {:016x}", t.syms.len(), av2_tone::parse::trace_digest(&t.syms));
        assert_eq!(ours, avm.trim(), "{name}: symbols differ from AVM's; av2-check --trace names the first");
    }
}

/// Runs `f` and turns a panic into a test failure that names the input.
fn survives<T>(name: &str, what: &str, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| panic!("{name}: av2-tone panicked on {what}"))
}

/// Decodes the stream as far as av2-tone goes: headers and every tile's
/// symbols. Returns whether it parsed.
fn decode(obus: &[u8]) -> bool {
    av2_tone::decode_obus(obus).is_ok()
}

#[test]
fn every_truncation_is_an_error_not_a_panic() {
    for (name, file, _) in FIXTURES {
        let obus = av2_tone::avif::primary_item(file).unwrap_or_default();
        let (headers, group) = av2_tone::parse_obus(&obus).unwrap_or_else(|e| panic!("{name}: {e}"));
        let tiles = av2_tone::tile::tile_buffers(&headers, group.data).unwrap();
        let last_tile = group.offset + tiles.last().map_or(0, |t| t.offset);
        for n in 0..obus.len() {
            let parsed = survives(name, &format!("the first {n} bytes"), || decode(&obus[..n]));
            // Cut in the headers, or before the last tile's first byte: the
            // layout no longer fits, as AVM's get_tile_buffers finds too.
            if n <= last_tile {
                assert!(!parsed, "{name}: parsed with only {n} of {} bytes", obus.len());
            }
        }
        // Cutting the file anywhere also breaks the container.
        for n in (0..file.len()).step_by(7) {
            let r = catch_unwind(|| av2_tone::decode_avif(&file[..n]));
            assert!(matches!(r, Ok(Err(_))), "{name}: container cut at {n}");
        }
    }
}

#[test]
fn every_bit_flip_in_headers_and_tile_data_is_survived() {
    for (name, file, _) in FIXTURES {
        let obus = av2_tone::avif::primary_item(file).unwrap_or_default();
        let (_, group) = av2_tone::parse_obus(&obus).unwrap_or_else(|e| panic!("{name}: {e}"));
        // Every header bit and every bit of the first 64 bytes of tile data,
        // which hold the tile-size fields of a multi-tile frame; then every
        // 61st bit to the end, through the partition, mode and coefficient
        // syntax.
        let end = (group.offset + 64).min(obus.len());
        let bits = (0..end * 8).chain((end * 8..obus.len() * 8).step_by(61));
        let mut flipped = obus.clone();
        for bit in bits {
            flipped[bit / 8] ^= 0x80 >> (bit % 8);
            survives(name, &format!("bit {bit} flipped"), || decode(&flipped));
            flipped[bit / 8] ^= 0x80 >> (bit % 8);
        }
    }
}
