#!/usr/bin/env bash
# Rust static analysis check script for Handy
# Runs formatting, linting, security audit, and dependency checks
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC_TAURI="$PROJECT_ROOT/src-tauri"

echo "=== Rust Static Analysis ==="
echo "Project: $PROJECT_ROOT"
echo ""

# cargo fmt check
echo "Running cargo fmt check..."
(cd "$SRC_TAURI" && cargo fmt -- --check)
echo "  ✓ cargo fmt passed"
echo ""

# cargo clippy
echo "Running cargo clippy..."
(cd "$SRC_TAURI" && cargo clippy --all-targets -- -D warnings)
echo "  ✓ cargo clippy passed"
echo ""

# cargo audit (optional — warn if not installed)
echo "Running cargo audit (if installed)..."
if command -v cargo-audit &>/dev/null; then
    (cd "$SRC_TAURI" && cargo audit)
    echo "  ✓ cargo audit passed"
else
    echo "  ⚠ cargo-audit not installed. Install with: cargo install cargo-audit"
fi
echo ""

# cargo deny (optional — warn if not installed)
echo "Running cargo deny check (if installed)..."
if command -v cargo-deny &>/dev/null; then
    (cd "$SRC_TAURI" && cargo deny check)
    echo "  ✓ cargo deny passed"
else
    echo "  ⚠ cargo-deny not installed. Install with: cargo install cargo-deny"
fi
echo ""

echo "=== All Rust static analysis checks completed! ==="