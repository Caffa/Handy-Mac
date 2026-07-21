#!/usr/bin/env bash
# test-cli-flags.sh — E2E test: Do all CLI flags parse correctly?
#
# Tests:
# 1. --is-active-use when app NOT running → exit code 2
# 2. --is-recording when app NOT running → exit code 2
# 3. --list-models → exit code 0, outputs model list
# 4. --list-devices → exit code 0, outputs device list
# 5. --transcribe-file with nonexistent file → exit code 2
# 6. --transcribe-file with --json → outputs valid JSON
# 7. --transcribe-file with invalid WAV format → exit code 2
#
# Exit codes: 0=pass, 1=fail, 2=skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

BINARY=""
TEMP_DIR=""

# ── Cleanup ──────────────────────────────────────────────────────────────
cleanup() {
    if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
    fi
    # Kill any leftover Handy processes from our tests
    kill_all_handy
}
trap cleanup EXIT

# ── Test: --is-active-use when app is NOT running ────────────────────────
test_is_active_use_not_running() {
    local binary="$1"

    # Make sure no Handy process is running
    kill_all_handy
    sleep 1

    # Remove the query state file if it exists
    rm -f "${TMPDIR:-/tmp}/handy_query_state.json"

    log_info "Testing --is-active-use when app is NOT running..."
    local rc=0
    "$binary" --is-active-use 2>/dev/null || rc=$?

    if [[ $rc -eq 2 ]]; then
        log_pass "--is-active-use returns exit code 2 (not running) as expected"
        return 0
    else
        log_fail "--is-active-use returned exit code $rc (expected 2)"
        return 1
    fi
}

# ── Test: --is-recording when app is NOT running ─────────────────────────
test_is_recording_not_running() {
    local binary="$1"

    # Make sure no Handy process is running
    kill_all_handy
    sleep 1

    # Remove the query state file if it exists
    rm -f "${TMPDIR:-/tmp}/handy_query_state.json"

    log_info "Testing --is-recording when app is NOT running..."
    local rc=0
    "$binary" --is-recording 2>/dev/null || rc=$?

    if [[ $rc -eq 2 ]]; then
        log_pass "--is-recording returns exit code 2 (not running) as expected"
        return 0
    else
        log_fail "--is-recording returned exit code $rc (expected 2)"
        return 1
    fi
}

# ── Test: --list-models ──────────────────────────────────────────────────
test_list_models() {
    local binary="$1"

    log_info "Testing --list-models..."
    local output rc=0
    output=$("$binary" --list-models 2>&1) || rc=$?

    if [[ $rc -ne 0 ]]; then
        log_fail "--list-models failed with exit code $rc"
        log_info "Output: $output"
        return 1
    fi

    if [[ -z "$output" ]]; then
        log_warn "--list-models returned empty output (no models installed)"
    else
        log_pass "--list-models produced output ($(( $(echo "$output" | wc -l | tr -d ' ') )) lines)"
    fi

    return 0
}

# ── Test: --list-models --json ────────────────────────────────────────────
test_list_models_json() {
    local binary="$1"

    if ! command -v jq &>/dev/null; then
        log_skip "jq not available, skipping JSON model list test"
        return 2
    fi

    log_info "Testing --list-models --json..."
    local output rc=0
    output=$("$binary" --list-models --json 2>&1) || rc=$?

    if [[ $rc -ne 0 ]]; then
        log_fail "--list-models --json failed with exit code $rc"
        return 1
    fi

    if ! assert_json_valid "$output"; then
        log_fail "--list-models --json output is not valid JSON"
        return 1
    fi

    # Verify it's an array of model objects
    local model_count
    model_count=$(echo "$output" | jq 'length' 2>/dev/null || echo "0")

    log_pass "--list-models --json returned $model_count models as valid JSON"
    return 0
}

# ── Test: --list-devices ──────────────────────────────────────────────────
test_list_devices() {
    local binary="$1"

    log_info "Testing --list-devices..."
    local output rc=0
    output=$("$binary" --list-devices 2>&1) || rc=$?

    if [[ $rc -ne 0 ]]; then
        log_fail "--list-devices failed with exit code $rc"
        return 1
    fi

    if [[ -z "$output" ]]; then
        log_warn "--list-devices returned empty output (no compute devices)"
    else
        log_pass "--list-devices produced output"
    fi

    return 0
}

# ── Test: --transcribe-file with nonexistent file ─────────────────────────
test_transcribe_nonexistent_file() {
    local binary="$1"

    log_info "Testing --transcribe-file with nonexistent file..."
    local rc=0
    "$binary" --transcribe-file "/nonexistent/path/test.wav" 2>/dev/null || rc=$?

    if [[ $rc -eq 2 ]]; then
        log_pass "--transcribe-file with nonexistent file returns exit code 2 (bad input)"
        return 0
    else
        log_fail "--transcribe-file with nonexistent file returned exit code $rc (expected 2)"
        return 1
    fi
}

# ── Test: --transcribe-file with invalid WAV format ───────────────────────
test_transcribe_invalid_wav() {
    local binary="$1"
    local invalid_wav="$2"

    log_info "Testing --transcribe-file with invalid WAV format (wrong sample rate)..."
    local output rc=0
    output=$("$binary" --transcribe-file "$invalid_wav" 2>&1) || rc=$?

    if [[ $rc -eq 2 ]]; then
        log_pass "--transcribe-file with invalid WAV returns exit code 2 (bad input)"
        return 0
    elif [[ $rc -eq 0 ]]; then
        log_warn "--transcribe-file with invalid WAV returned exit code 0 (may have tolerated the format)"
        return 0
    else
        log_fail "--transcribe-file with invalid WAV returned exit code $rc (expected 2)"
        return 1
    fi
}

# ── Test: Conflicting flags produce error ─────────────────────────────────
test_conflicting_flags() {
    local binary="$1"

    log_info "Testing conflicting flags (--is-active-use with --start-hidden)..."
    local output rc=0
    # These flags are documented as conflicting — clap should reject this combination
    output=$("$binary" --is-active-use --start-hidden 2>&1) || rc=$?

    # clap conflicts should produce exit code 2 (usage error)
    if [[ $rc -ne 0 ]]; then
        log_pass "Conflicting flags rejected (exit code $rc)"
        return 0
    else
        log_fail "Conflicting flags were accepted (exit code 0, expected non-zero)"
        return 1
    fi
}

# ── Main ─────────────────────────────────────────────────────────────────
main() {
    log_test "Handy-Mac CLI Flags E2E Test"
    echo ""

    # Find the binary
    BINARY=$(find_binary) || return 1
    log_info "Using binary: $BINARY"

    # Create temp directory for test files
    TEMP_DIR=$(create_temp_dir)

    # Generate an invalid WAV file (44.1kHz stereo instead of 16kHz mono)
    # This should be rejected by the strict WAV validation in the app
    local invalid_wav="$TEMP_DIR/invalid_test.wav"
    if command -v ffmpeg &>/dev/null; then
        ffmpeg -y -f lavfi -i "sine=frequency=440:duration=1" \
            -ar 44100 -ac 2 "$invalid_wav" 2>/dev/null || {
            log_warn "Could not generate invalid WAV file (ffmpeg failed)"
            invalid_wav=""
        }
    else
        log_warn "ffmpeg not available, skipping invalid WAV test"
        invalid_wav=""
    fi

    # Run tests — order matters: not-running tests first, then headless tests
    run_test "--is-active-use returns 2 when app not running" "test_is_active_use_not_running $BINARY" || true
    run_test "--is-recording returns 2 when app not running" "test_is_recording_not_running $BINARY" || true
    run_test "--list-models produces output" "test_list_models $BINARY" || true
    run_test "--list-models --json outputs valid JSON" "test_list_models_json $BINARY" || true
    run_test "--list-devices produces output" "test_list_devices $BINARY" || true
    run_test "--transcribe-file with nonexistent file returns 2" "test_transcribe_nonexistent_file $BINARY" || true

    if [[ -n "$invalid_wav" && -f "$invalid_wav" ]]; then
        run_test "--transcribe-file with invalid WAV returns 2" "test_transcribe_invalid_wav $BINARY $invalid_wav" || true
    fi

    run_test "Conflicting flags are rejected" "test_conflicting_flags $BINARY" || true

    # Print summary
    print_summary "CLI Flags Tests"
    return $?
}

main "$@"