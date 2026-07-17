#!/usr/bin/env bash
# Run ThreadSanitizer tests on Linux with nightly Rust.
#
# ThreadSanitizer (TSan) detects data races, lock-order inversions, and other
# concurrency bugs at runtime. It only works on Linux with a nightly toolchain
# because it requires the unstable `-Zsanitizer=thread` flag.
#
# Prerequisites:
#   rustup toolchain install nightly
#
# Usage:
#   ./scripts/tsan-check.sh
#
# This script runs the tsan_concurrency test with TSan instrumentation.
# The tests are gated behind `#[cfg(all(target_os = "linux", feature = "tsan"))]`
# so they only compile on Linux with the `tsan` feature enabled.
set -euo pipefail

echo "Building with ThreadSanitizer (Linux only, requires nightly)..."
RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test --features tsan --test tsan_concurrency -- --test-threads=1
echo "ThreadSanitizer tests passed!"