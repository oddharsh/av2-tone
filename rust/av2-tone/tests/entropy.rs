//! The arithmetic decoder against AVM's own encoder. Each fixture is an
//! `av2oracle --entropy <seed> 1500` run: a stream AVM's entenc.c wrote over
//! every alphabet size from 2 to 16 (plus bools, literals, bypass bits and
//! unary codes), with AVM's tell after each operation and the CDFs AVM's
//! reader ended with. Seed 3 adapts every CDF, seed 1 none, seed 2 a mix.
//! av2-check --entropy runs the same replay on fresh seeds.

// Test code: indexing a parsed script and panicking on a bad one are fine.
#![allow(clippy::indexing_slicing)]

mod common;

use common::entropy_script::{Coverage, replay};

const SCRIPTS: [(&str, &str); 3] = [
    ("seed 1, no adaptation", include_str!("fixtures/entropy-seed1.txt")),
    ("seed 2, mixed", include_str!("fixtures/entropy-seed2.txt")),
    ("seed 3, always adapting", include_str!("fixtures/entropy-seed3.txt")),
];

#[test]
fn recovers_every_symbol_and_matches_avm_tell() {
    let mut all = Coverage::default();
    for (name, script) in SCRIPTS {
        let cov = replay(script).unwrap_or_else(|e| panic!("{name}: {e}"));
        all.add(&cov);
    }
    // Every alphabet size, adapting and not, came up.
    for n in 2..=16 {
        assert!(all.symbols[n][0] > 0 && all.symbols[n][1] > 0, "alphabet {n}: {:?}", all.symbols[n]);
    }
    assert!(all.other > 0);
}

#[test]
fn a_corrupted_stream_never_panics() {
    // Flip each byte of a real stream in turn and decode it to the end: the
    // values change, but every read must return, Ok or Err.
    use av2_tone::entropy::SymbolReader;
    let data: Vec<u8> = SCRIPTS[2].1.lines().find_map(|l| l.strip_prefix("data ")).map_or(Vec::new(), |h| {
        (0..h.len()).step_by(2).filter_map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok()).collect()
    });
    assert!(!data.is_empty());
    for i in 0..data.len().min(256) {
        let mut d = data.clone();
        d[i] ^= 0xff;
        let mut cdfs: Vec<Vec<u16>> = (2..=16usize)
            .map(|n| {
                let mut c: Vec<u16> = (1..n).map(|k| (32768 - 32768 * k / n) as u16).collect();
                c.extend([0, 0, 2, 3, 4]);
                c
            })
            .collect();
        let mut r = SymbolReader::new(&d, true);
        for k in 0..3000 {
            let n = 2 + k % 15;
            let _ = r.read_symbol(&mut cdfs[n - 2], n);
            let _ = r.read_literal((k % 9) as u32);
        }
        assert!(r.has_overflowed());
    }
}
