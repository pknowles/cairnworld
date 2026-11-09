#!/usr/bin/env bash
# Runs the whole test suite. Test databases and logs hold the telemetry of every
# test, so they are kept after the run, pass or fail, and only the next whole
# run replaces them.
set -euo pipefail

rm -rf target/tmp/test-databases
mkdir -p target/tmp
cargo test --no-run 2>&1 | tee target/tmp/build.log
cargo test 2>&1 | tee target/tmp/test.log
