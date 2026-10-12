#!/usr/bin/env bash
set -euo pipefail

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --lib --no-default-features --features hydrate --target wasm32-unknown-unknown -- -D warnings
