#!/usr/bin/env bash
# ci.sh: the gate every change to the Rust decoder passes. Unit tests,
# including the frozen AVM fixtures and the truncation and bit-flip sweeps,
# then clippy with every warning an error (the workspace lints refuse unsafe,
# unchecked indexing, unwrap, expect and panic in the decoder).
#
# The oracle checks run outside this script because they need AVM built:
#   rust/oracle/build.sh && bun rust/corpus.ts
#   cargo run --release -p av2-check -- --headers rust/build/corpus
#   cargo run --release -p av2-check -- --entropy 300
#   cargo run --release -p av2-check -- --trace build/corpus
set -euo pipefail
cd "$(dirname "$0")"
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
