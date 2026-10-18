//! Two stills from rust/corpus.ts, frozen with what AVM parsed out of them
//! (av2oracle's seq.* and frame.* lines), so `cargo test` checks the header
//! parser against AVM without AVM installed. The full corpus runs through
//! av2-check.
//!
//!   crop-10b444-shipped-qp180  aadhar.sh's flags: qmseg, TCQ, intra block copy
//!   crop-8b420-base-qp105-sb64 64px superblocks, 8-bit 4:2:0
//!
//! Both also feed the robustness sweeps: every truncation and every single
//! bit flip of the headers must come back as Ok or Err, never a panic.

// Test code: a panic here is the assertion, not a decoder bug.
#![allow(clippy::panic, clippy::indexing_slicing)]

use std::panic::{AssertUnwindSafe, catch_unwind};

const FIXTURES: [(&str, &[u8], &str); 2] = [
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
];

#[test]
fn headers_match_avm() {
    for (name, file, avm) in FIXTURES {
        let (headers, pic) = av2_tone::decode_avif(file).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ours = av2_tone::dump::dump(&headers);
        for (i, (a, b)) in ours.lines().zip(avm.lines()).enumerate() {
            assert_eq!(a, b, "{name}: line {}", i + 1);
        }
        assert_eq!(ours.lines().count(), avm.lines().count(), "{name}: field count");
        assert_eq!((pic.width, pic.height), (250, 186), "{name}");
    }
}

/// Runs the header parser and turns a panic into a test failure that names
/// the input, rather than a crash.
fn parse_survives(name: &str, what: &str, obus: &[u8]) -> bool {
    match catch_unwind(AssertUnwindSafe(|| av2_tone::parse_obus(obus).is_ok())) {
        Ok(ok) => ok,
        Err(_) => panic!("{name}: av2-tone panicked on {what}"),
    }
}

#[test]
fn every_truncation_is_an_error_not_a_panic() {
    for (name, file, _) in FIXTURES {
        let obus = av2_tone::avif::primary_item(file).unwrap_or_default();
        let (_, tiles) = av2_tone::parse_obus(&obus).unwrap_or_else(|e| panic!("{name}: {e}"));
        let headers_end = obus.len() - tiles.len();
        for n in 0..headers_end {
            assert!(!parse_survives(name, &format!("the first {n} bytes"), &obus[..n]), "{name}: {n} bytes parsed");
        }
        // Cutting the file anywhere also breaks the container.
        for n in (0..file.len()).step_by(7) {
            let r = catch_unwind(|| av2_tone::decode_avif(&file[..n]));
            assert!(matches!(r, Ok(Err(_))), "{name}: container cut at {n}");
        }
    }
}

#[test]
fn every_bit_flip_in_the_headers_is_survived() {
    for (name, file, _) in FIXTURES {
        let obus = av2_tone::avif::primary_item(file).unwrap_or_default();
        let (_, tiles) = av2_tone::parse_obus(&obus).unwrap_or_else(|e| panic!("{name}: {e}"));
        let headers_end = obus.len() - tiles.len();
        let mut flipped = obus.clone();
        for bit in 0..headers_end * 8 {
            flipped[bit / 8] ^= 0x80 >> (bit % 8);
            parse_survives(name, &format!("bit {bit} flipped"), &flipped);
            flipped[bit / 8] ^= 0x80 >> (bit % 8);
        }
    }
}
