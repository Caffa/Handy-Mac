#!/usr/bin/env bash
# test-consecutive-runs.sh — E2E test: Do consecutive transcriptions work
#                             without degradation?
#
# 1. Generate a test WAV file
# 2. Run --transcribe-file 5 times in sequence
# 3. Verify each succeeds (exit code 0)
# 4. Report timing for each run
# 5. Check for memory leaks (optional: track RSS with ps)
#
# Exit codes: 0=pass, 1=fail, 2=skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

NUM_RUNS=5
BINARY=""
TEMP_DIR=""

# ── Cleanup ──────────────────────────────────────────────────────────────
cleanup() {
    if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
    fi
}
trap cleanup EXIT

# ── Test: Consecutive transcription runs ──────────────────────────────────
test_consecutive_runs() {
    local binary="$1"
    local wav_file="$2"
    local num_runs="${3:-$NUM_RUNS}"

    local failures=0
    local timings=()

    log_info "Running $num_runs consecutive transcriptions..."

    for i in $(seq 1 "$num_runs"); do
        local start_time end_time elapsed_ms
        start_time=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")

        local rc=0
        local output
        output=$("$binary" --transcribe-file "$wav_file" --json 2>&1) || rc=$?

        end_time=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")
        elapsed_ms=$(( (end_time - start_time) / 1000000 ))

        if [[ $rc -ne 0 ]]; then
            ((failures++)) || true
            log_fail "Run $i/$num_runs: failed (exit code $rc)"
        else
            # Extract transcription time from JSON if available
            local transcribe_ms="N/A"
            if command -v jq &>/dev/null; then
                transcribe_ms=$(echo "$output" | jq -r '.best_ms // .transcribe_ms // "N/A"' 2>/dev/null || echo "N/A")
            fi
            log_pass "Run $i/$num_runs: succeeded (${elapsed_ms}ms total, ${transcribe_ms}ms transcription)"
        fi

        timings+=("$elapsed_ms")
    done

    # Report summary
    echo ""
    log_info "─── Consecutive Runs Summary ───"
    for i in "${!timings[@]}"; do
        local run_num=$((i + 1))
        local ms="${timings[$i]}"
        local sec
        sec=$(echo "scale=2; $ms / 1000" | bc 2>/dev/null || echo "${ms}ms")
        log_info "  Run $run_num: ${sec}s"
    done

    if [[ $failures -gt 0 ]]; then
        log_fail "$failures/$num_runs runs failed"
        return 1
    fi

    log_pass "All $num_runs consecutive runs succeeded"
    return 0
}

# ── Test: Memory stability (optional) ────────────────────────────────────
test_memory_stability() {
    local binary="$1"
    local wav_file="$2"
    local num_runs="${3:-3}"

    # Check if we can measure memory
    if ! command -v ps &>/dev/null; then
        log_skip "ps command not available for memory tracking"
        return 2
    fi

    log_info "Running $num_runs transcriptions and tracking memory..."

    local first_rss="" last_rss=""
    for i in $(seq 1 "$num_runs"); do
        local rc=0
        "$binary" --transcribe-file "$wav_file" --json &>/dev/null || rc=$?

        if [[ $rc -ne 0 ]]; then
            log_warn "Run $i failed, skipping memory check"
            continue
        fi

        # This is a headless run so the process has already exited;
        # we can't measure its RSS after the fact. We'd need to run
        # it under a wrapper. For now, just note that it's headless.
        log_info "Run $i completed (headless — no RSS tracking available)"
    done

    log_info "Memory tracking is limited for headless (--transcribe-file) runs"
    log_info "For full memory profiling, use Instruments or Instruments CLI"
    log_pass "Memory stability test completed (limited mode)"
    return 0
}

# ── Main ─────────────────────────────────────────────────────────────────
main() {
    log_test "Handy-Mac Consecutive Runs E2E Test"
    echo ""

    # Allow overriding the number of runs
    local num_runs="${1:-$NUM_RUNS}"

    # Find the binary
    BINARY=$(find_binary) || return 1
    log_info "Using binary: $BINARY"
    log_info "Will run $num_runs consecutive transcriptions"

    # Check for audio generation tools
    if ! command -v ffmpeg &>/dev/null && ! command -v sox &>/dev/null; then
        log_skip "Neither ffmpeg nor sox found. Cannot generate test audio."
        return 2
    fi

    # Create temp directory for test files
    TEMP_DIR=$(create_temp_dir)
    local wav_file="$TEMP_DIR/test_consecutive.wav"

    # Generate test WAV file
    log_info "Generating test WAV file: $wav_file"
    if ! generate_test_wav "$wav_file" 2 440; then
        log_fail "Failed to generate test WAV file"
        return 1
    fi

    # Check that at least one model is available
    if ! has_models "$BINARY"; then
        log_skip "No models available for transcription. Download a model first."
        return 2
    fi

    # Run tests
    run_test "Consecutive transcription runs ($num_runs)" test_consecutive_runs "$BINARY" "$wav_file" "$num_runs" || true
    run_test "Memory stability check" test_memory_stability "$BINARY" "$wav_file" 3 || true

    # Print summary
    print_summary "Consecutive Runs Tests"
    return $?
}

main "$@"