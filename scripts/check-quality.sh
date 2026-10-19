#!/usr/bin/env bash
set -euo pipefail

cargo fmt --check
export DATABASE_URL=sqlite://target/sqlx-schema.sqlite
cargo sqlx database setup --source migrations
cargo sqlx prepare --check -- --all-targets
cargo clippy --all-targets -- -D warnings
cargo clippy --lib --no-default-features --features hydrate --target wasm32-unknown-unknown -- -D warnings
