#!/usr/bin/env bash
# test-app-startup.sh — E2E test: Does the app start and not crash?
#
# 1. Build the binary if needed
# 2. Start the app with --start-hidden --no-tray
# 3. Wait up to 10 seconds for it to become ready
# 4. Check --is-active-use returns exit code 1 (idle, not recording)
# 5. Kill the app
# 6. Report startup time
#
# Exit codes: 0=pass, 1=fail, 2=skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

# ── Cleanup ──────────────────────────────────────────────────────────────
TEMP_DIR=""
BINARY=""

cleanup() {
    if [[ -n "$BINARY" ]]; then
        kill_app || true
    fi
    if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
    fi
}
trap cleanup EXIT

# ── Test: App starts and is queryable ─────────────────────────────────────
test_app_starts() {
    local binary="$1"

    log_info "Starting app with --start-hidden --no-tray..."
    local start_time end_time elapsed_ms
    start_time=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")

    # Start the app in the background
    "$binary" --start-hidden --no-tray --debug &>/dev/null &
    local app_pid=$!
    log_info "App PID: $app_pid"

    # Wait for the app to become ready
    if ! wait_for_app "$binary" "$STARTUP_TIMEOUT"; then
        log_fail "App did not become ready within ${STARTUP_TIMEOUT}s"
        kill_app || true
        return 1
    fi

    end_time=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")
    elapsed_ms=$(( (end_time - start_time) / 1000000 ))
    local elapsed_sec
    elapsed_sec=$(echo "scale=2; $elapsed_ms / 1000" | bc 2>/dev/null || echo "${elapsed_ms}ms")

    log_pass "App started in ${elapsed_sec}s"
    return 0
}

# ── Test: --is-active-use returns idle (1) when app is running but idle ─
test_is_active_use_idle() {
    local binary="$1"

    log_info "Checking --is-active-use returns idle (exit code 1)..."
    local rc=0
    "$binary" --is-active-use 2>/dev/null || rc=$?

    if [[ $rc -eq 1 ]]; then
        log_pass "--is-active-use correctly reports idle (exit code 1)"
        return 0
    elif [[ $rc -eq 0 ]]; then
        log_fail "--is-active-use returned active (exit code 0) but app should be idle"
        return 1
    else
        log_fail "--is-active-use returned unexpected exit code $rc (expected 1)"
        return 1
    fi
}

# ── Test: --is-recording returns not-recording (1) ───────────────────────
test_is_not_recording() {
    local binary="$1"

    log_info "Checking --is-recording returns not-recording (exit code 1)..."
    local rc=0
    "$binary" --is-recording 2>/dev/null || rc=$?

    if [[ $rc -eq 1 ]]; then
        log_pass "--is-recording correctly reports not-recording (exit code 1)"
        return 0
    else
        log_fail "--is-recording returned unexpected exit code $rc (expected 1)"
        return 1
    fi
}

# ── Main ─────────────────────────────────────────────────────────────────
main() {
    log_test "Handy-Mac App Startup E2E Test"
    echo ""

    # Find the binary
    BINARY=$(find_binary) || return 1
    log_info "Using binary: $BINARY"

    # Make sure no Handy process is already running
    kill_all_handy

    # Run tests
    run_test "App starts without crashing" "test_app_starts $BINARY" || true
    run_test "--is-active-use returns idle" "test_is_active_use_idle $BINARY" || true
    run_test "--is-recording returns not-recording" "test_is_not_recording $BINARY" || true

    # Cleanup
    kill_app || true

    # Print summary
    print_summary "App Startup Tests"
    return $?
}

main "$@"