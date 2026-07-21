#!/usr/bin/env bash
# common.sh — Shared utilities for Handy-Mac E2E tests
# Source this file from other test scripts:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "$SCRIPT_DIR/common.sh"

set -euo pipefail

# ── Colors ──────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

# ── Project paths ────────────────────────────────────────────────────────
# Resolve the Handy-Mac project root (parent of scripts/e2e-tests/)
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY_NAME="handy"  # The actual binary name (matches productName "Handy" → binary "handy")

# ── Configuration ─────────────────────────────────────────────────────────
BUILD_TYPE="${HANDY_BUILD_TYPE:-release}"  # release or debug
STARTUP_TIMEOUT="${HANDY_STARTUP_TIMEOUT:-10}"  # seconds to wait for app startup
APP_DATA_DIR="${HOME}/Library/Application Support/com.pais.handy"

# ── Logging ──────────────────────────────────────────────────────────────

log_test() {
    local test_name="$1"
    echo -e "${CYAN}${BOLD}[TEST]${RESET} ${test_name}"
}

log_pass() {
    local msg="$1"
    echo -e "${GREEN}${BOLD}[PASS]${RESET} ${msg}"
}

log_fail() {
    local msg="$1"
    echo -e "${RED}${BOLD}[FAIL]${RESET} ${msg}"
}

log_skip() {
    local msg="$1"
    echo -e "${YELLOW}${BOLD}[SKIP]${RESET} ${msg}"
}

log_info() {
    local msg="$1"
    echo -e "${CYAN}[INFO]${RESET} ${msg}"
}

log_warn() {
    local msg="$1"
    echo -e "${YELLOW}[WARN]${RESET} ${msg}"
}

# ── Binary discovery ─────────────────────────────────────────────────────

# Find the Handy-Mac binary.
# Uses HANDY_BINARY env var if set, otherwise searches standard locations.
# Prints the path to stdout. Returns 1 if not found.
find_binary() {
    # Explicit override
    if [[ -n "${HANDY_BINARY:-}" ]]; then
        if [[ -x "$HANDY_BINARY" ]]; then
            echo "$HANDY_BINARY"
            return 0
        else
            log_fail "HANDY_BINARY is set but not executable: $HANDY_BINARY"
            return 1
        fi
    fi

    local candidates=(
        "$PROJECT_ROOT/src-tauri/target/$BUILD_TYPE/$BINARY_NAME"
        "$PROJECT_ROOT/src-tauri/target/$BUILD_TYPE/Handy-Mac"
    )

    # Also check for capitalized variant
    if [[ "$BUILD_TYPE" == "release" ]]; then
        candidates+=("$PROJECT_ROOT/src-tauri/target/release/Handy")
    else
        candidates+=("$PROJECT_ROOT/src-tauri/target/debug/Handy")
    fi

    for candidate in "${candidates[@]}"; do
        if [[ -x "$candidate" ]]; then
            echo "$candidate"
            return 0
        fi
    done

    log_fail "Handy-Mac binary not found. Build with 'bun run tauri build' or set HANDY_BINARY."
    echo "  Searched:" >&2
    for candidate in "${candidates[@]}"; do
        echo "    - $candidate" >&2
    done
    return 1
}

# ── App lifecycle ────────────────────────────────────────────────────────

# Wait for the app to become ready (responds to --is-active-use with exit code 0 or 1).
# Arguments:
#   $1 — path to the binary
#   $2 — timeout in seconds (default: $STARTUP_TIMEOUT)
# Returns 0 if the app becomes ready, 1 if timeout.
wait_for_app() {
    local binary="$1"
    local timeout="${2:-$STARTUP_TIMEOUT}"
    local elapsed=0

    log_info "Waiting up to ${timeout}s for app to become ready..."

    while [[ $elapsed -lt $timeout ]]; do
        if "$binary" --is-active-use 2>/dev/null; then
            # exit code 0 = active use (running)
            log_info "App is ready (active)."
            return 0
        fi
        local rc=$?
        if [[ $rc -eq 1 ]]; then
            # exit code 1 = idle (running but not active) — still ready
            log_info "App is ready (idle)."
            return 0
        fi
        # exit code 2 = not running yet
        sleep 1
        ((elapsed++)) || true
    done

    log_fail "App did not become ready within ${timeout}s."
    return 1
}

# Kill the Handy-Mac app process gracefully.
# Sends SIGTERM, waits 3 seconds, then SIGKILL.
kill_app() {
    local pid
    pid=$(pgrep -f "Handy|handy" | head -1 || true)

    if [[ -z "$pid" ]]; then
        log_info "No Handy process found to kill."
        return 0
    fi

    log_info "Sending SIGTERM to Handy process (PID: $pid)..."
    kill -TERM "$pid" 2>/dev/null || true

    local wait_count=0
    while [[ $wait_count -lt 30 ]]; do  # 3 seconds in 0.1s steps
        if ! kill -0 "$pid" 2>/dev/null; then
            log_info "Handy process terminated gracefully."
            return 0
        fi
        sleep 0.1
        ((wait_count++)) || true
    done

    log_warn "Process did not exit after SIGTERM, sending SIGKILL..."
    kill -KILL "$pid" 2>/dev/null || true
    sleep 0.5

    if kill -0 "$pid" 2>/dev/null; then
        log_fail "Could not kill Handy process (PID: $pid)."
        return 1
    fi

    log_info "Handy process killed."
    return 0
}

# Kill ALL Handy-Mac processes (for cleanup).
kill_all_handy() {
    pkill -f "Handy|handy" 2>/dev/null || true
    sleep 1
    # Force kill any remaining
    pkill -9 -f "Handy|handy" 2>/dev/null || true
}

# ── Audio file generation ────────────────────────────────────────────────

# Generate a test WAV file (16kHz mono 16-bit PCM, as required by Handy).
# Arguments:
#   $1 — output path
#   $2 — duration in seconds (default: 2)
#   $3 — frequency in Hz (default: 440)
# Returns 0 on success, 1 on failure.
generate_test_wav() {
    local output="$1"
    local duration="${2:-2}"
    local frequency="${3:-440}"

    # Ensure the output directory exists
    mkdir -p "$(dirname "$output")"

    if command -v ffmpeg &>/dev/null; then
        ffmpeg -y -f lavfi -i "sine=frequency=${frequency}:duration=${duration}" \
            -ar 16000 -ac 1 -sample_fmt s16 "$output" 2>/dev/null
        return $?
    elif command -v sox &>/dev/null; then
        sox -n "$output" synth "$duration" sine "$frequency" rate 16000 channels 1
        return $?
    else
        log_fail "Neither ffmpeg nor sox found. Install one to generate test audio."
        return 1
    fi
}

# ── Assertions ────────────────────────────────────────────────────────────

# Assert that a command exits with the expected code.
# Arguments:
#   $1 — expected exit code
#   $2... — command and args to run
# Returns 0 if assertion passes, 1 otherwise.
assert_exit_code() {
    local expected="$1"
    shift
    local cmd=("$@")

    log_info "Running: ${cmd[*]} (expecting exit code $expected)"

    local actual=0
    "${cmd[@]}" &>/dev/null || actual=$?

    if [[ $actual -eq $expected ]]; then
        log_pass "Exit code $actual matches expected $expected"
        return 0
    else
        log_fail "Exit code $actual (expected $expected)"
        return 1
    fi
}

# Assert that a JSON output contains a field with an expected value.
# Arguments:
#   $1 — JSON string
#   $2 — field name (dot notation supported, e.g., "model")
#   $3 — expected value
# Returns 0 if assertion passes, 1 otherwise.
assert_json_field() {
    local json="$1"
    local field="$2"
    local expected="$3"

    if ! command -v jq &>/dev/null; then
        log_warn "jq not found, skipping JSON field assertion."
        return 2
    fi

    local actual
    actual=$(echo "$json" | jq -r ".$field" 2>/dev/null) || {
        log_fail "Could not extract field '$field' from JSON"
        return 1
    }

    if [[ "$actual" == "$expected" ]]; then
        log_pass "JSON field '$field' = '$actual' (expected '$expected')"
        return 0
    else
        log_fail "JSON field '$field' = '$actual' (expected '$expected')"
        return 1
    fi
}

# Assert that JSON output is valid (parseable).
# Arguments:
#   $1 — JSON string
# Returns 0 if valid, 1 otherwise.
assert_json_valid() {
    local json="$1"

    if ! command -v jq &>/dev/null; then
        log_warn "jq not found, skipping JSON validity assertion."
        return 2
    fi

    if echo "$json" | jq . &>/dev/null; then
        log_pass "JSON output is valid"
        return 0
    else
        log_fail "JSON output is not valid"
        return 1
    fi
}

# ── Timing ───────────────────────────────────────────────────────────────

# Measure the time a command takes and report it.
# Arguments:
#   $1... — command and args to run
# Prints timing info and returns the command's exit code.
measure_time() {
    local start end elapsed
    start=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")

    local cmd=("$@")
    log_info "Timing: ${cmd[*]}"

    local rc=0
    "${cmd[@]}" || rc=$?

    end=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")

    # Calculate elapsed in milliseconds
    local elapsed_ms=$(( (end - start) / 1000000 ))
    local elapsed_sec=$(echo "scale=2; $elapsed_ms / 1000" | bc 2>/dev/null || echo "${elapsed_ms}ms")

    if [[ $rc -eq 0 ]]; then
        log_pass "Completed in ${elapsed_sec}s (exit code 0)"
    else
        log_fail "Failed with exit code $rc after ${elapsed_sec}s"
    fi

    return $rc
}

# ── Model availability ───────────────────────────────────────────────────

# Check if at least one model is available for transcription.
# Arguments:
#   $1 — path to the binary
# Returns 0 if models available, 1 if not.
has_models() {
    local binary="$1"
    local output
    output=$("$binary" --list-models 2>/dev/null) || {
        log_warn "Could not list models (app may not be fully initialized)."
        return 1
    }

    if [[ -z "$output" ]] || echo "$output" | grep -q "No models available"; then
        return 1
    fi

    return 0
}

# ── Temp file management ─────────────────────────────────────────────────

# Create a temp directory for the test. Cleans up on EXIT via trap.
# Prints the directory path to stdout.
create_temp_dir() {
    local tmpdir
    tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/handy-e2e-XXXXXX")
    echo "$tmpdir"
}

# ── Test runner helpers ──────────────────────────────────────────────────

# Counters for test results
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0

# Summary function — call at the end of each test script.
print_summary() {
    local script_name="$1"
    echo ""
    echo -e "${BOLD}──────────────────────────────────────────${RESET}"
    echo -e "${BOLD}  Results: ${script_name}${RESET}"
    echo -e "  ${GREEN}Passed: ${TESTS_PASSED}${RESET}  ${RED}Failed: ${TESTS_FAILED}${RESET}  ${YELLOW}Skipped: ${TESTS_SKIPPED}${RESET}  Total: ${TESTS_RUN}"
    echo -e "${BOLD}──────────────────────────────────────────${RESET}"

    if [[ $TESTS_FAILED -gt 0 ]]; then
        return 1
    fi
    return 0
}

# Run a single test case.
# Arguments:
#   $1 — test name
#   $2 — test function name
# Returns 0 on pass, 1 on fail, 2 on skip.
run_test() {
    local name="$1"
    local func="$2"

    ((TESTS_RUN++)) || true
    log_test "$name"

    local rc=0
    "$func" || rc=$?

    case $rc in
        0) ((TESTS_PASSED++)) || true ;;
        2) ((TESTS_SKIPPED++)) || true ;;
        *) ((TESTS_FAILED++)) || true ;;
    esac

    return $rc
}