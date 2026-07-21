#!/usr/bin/env bash
# test-transcribe-file.sh — E2E test: Can the app transcribe an audio file?
#
# 1. Generate a test WAV file (sine wave at 16kHz mono)
# 2. Check that at least one model is available
# 3. Run: Handy-Mac --transcribe-file test.wav --json
# 4. Verify exit code is 0
# 5. Verify JSON output contains expected fields
# 6. Test with --repeat 3 for benchmarking
# 7. Report timing
#
# Exit codes: 0=pass, 1=fail, 2=skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

# ── Cleanup ──────────────────────────────────────────────────────────────
TEMP_DIR=""
BINARY=""

cleanup() {
    if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
    fi
}
trap cleanup EXIT

# ── Test: Headless transcription produces output ─────────────────────────
test_transcribe_file_basic() {
    local binary="$1"
    local wav_file="$2"

    log_info "Running --transcribe-file with --json..."
    local output rc=0
    output=$("$binary" --transcribe-file "$wav_file" --json 2>&1) || rc=$?

    if [[ $rc -ne 0 ]]; then
        log_fail "--transcribe-file failed with exit code $rc"
        log_info "Output: $output"
        return 1
    fi

    # Verify JSON output is valid
    if ! assert_json_valid "$output"; then
        log_fail "Output is not valid JSON"
        log_info "Output: $output"
        return 1
    fi

    # Verify expected fields exist
    local has_text has_model
    has_text=$(echo "$output" | jq -r '.text' 2>/dev/null || echo "null")
    has_model=$(echo "$output" | jq -r '.model' 2>/dev/null || echo "null")

    if [[ "$has_text" == "null" ]]; then
        log_warn "JSON output missing 'text' field (may be expected for silence/sine wave)"
    else
        log_pass "JSON output contains 'text' field"
    fi

    if [[ "$has_model" == "null" ]]; then
        log_fail "JSON output missing 'model' field"
        return 1
    fi
    log_pass "JSON output contains 'model' field: $has_model"

    # Verify timing fields
    local transcribe_ms
    transcribe_ms=$(echo "$output" | jq -r '.transcribe_ms' 2>/dev/null || echo "null")
    if [[ "$transcribe_ms" != "null" ]]; then
        log_info "Transcription time: ${transcribe_ms}ms"
    fi

    log_pass "Headless transcription completed successfully"
    return 0
}

# ── Test: Transcription with --repeat 3 ──────────────────────────────────
test_transcribe_file_repeat() {
    local binary="$1"
    local wav_file="$2"

    log_info "Running --transcribe-file with --repeat 3 --json..."
    local output rc=0
    local start_time end_time elapsed_ms
    start_time=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")

    output=$("$binary" --transcribe-file "$wav_file" --repeat 3 --json 2>&1) || rc=$?

    end_time=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")
    elapsed_ms=$(( (end_time - start_time) / 1000000 ))

    if [[ $rc -ne 0 ]]; then
        log_fail "--transcribe-file --repeat 3 failed with exit code $rc"
        return 1
    fi

    local elapsed_sec
    elapsed_sec=$(echo "scale=2; $elapsed_ms / 1000" | bc 2>/dev/null || echo "${elapsed_ms}ms")
    log_info "Total --repeat 3 time: ${elapsed_sec}s"

    # Check JSON has transcribe_ms array with 3 entries
    if command -v jq &>/dev/null; then
        local count
        count=$(echo "$output" | jq '.transcribe_ms | length' 2>/dev/null || echo "0")
        if [[ "$count" == "3" ]]; then
            log_pass "Repeat transcription produced $count timing entries"
        else
            log_warn "Expected 3 timing entries, got $count"
        fi

        local best_ms
        best_ms=$(echo "$output" | jq -r '.best_ms' 2>/dev/null || echo "null")
        log_info "Best transcription time: ${best_ms}ms"

        local rtf
        rtf=$(echo "$output" | jq -r '.rtf' 2>/dev/null || echo "null")
        log_info "Real-time factor (RTF): $rtf"
    fi

    log_pass "Repeated transcription completed successfully"
    return 0
}

# ── Test: Transcription without --json (plain text) ──────────────────────
test_transcribe_file_plain() {
    local binary="$1"
    local wav_file="$2"

    log_info "Running --transcribe-file without --json..."
    local output rc=0
    output=$("$binary" --transcribe-file "$wav_file" 2>/dev/null) || rc=$?

    if [[ $rc -ne 0 ]]; then
        log_fail "--transcribe-file (plain) failed with exit code $rc"
        return 1
    fi

    # Plain output should contain "model=" and "text:" markers
    if echo "$output" | grep -q "model="; then
        log_pass "Plain output contains model info"
    else
        log_warn "Plain output does not contain model info"
    fi

    if echo "$output" | grep -q "text:"; then
        log_pass "Plain output contains text"
    else
        log_warn "Plain output does not contain 'text:' marker"
    fi

    return 0
}

# ── Main ─────────────────────────────────────────────────────────────────
main() {
    log_test "Handy-Mac Transcription E2E Test"
    echo ""

    # Find the binary
    BINARY=$(find_binary) || return 1
    log_info "Using binary: $BINARY"

    # Check for audio generation tools
    if ! command -v ffmpeg &>/dev/null && ! command -v sox &>/dev/null; then
        log_skip "Neither ffmpeg nor sox found. Cannot generate test audio."
        return 2
    fi

    # Create temp directory for test files
    TEMP_DIR=$(create_temp_dir)
    local wav_file="$TEMP_DIR/test_tone.wav"

    # Generate test WAV file
    log_info "Generating test WAV file: $wav_file"
    if ! generate_test_wav "$wav_file" 2 440; then
        log_fail "Failed to generate test WAV file"
        return 1
    fi

    # Verify the WAV file was created
    if [[ ! -f "$wav_file" ]]; then
        log_fail "Test WAV file was not created"
        return 1
    fi
    log_info "Test WAV file created: $(du -h "$wav_file" | cut -f1)"

    # Check that at least one model is available
    log_info "Checking for available models..."
    if ! has_models "$BINARY"; then
        log_skip "No models available for transcription. Download a model first."
        return 2
    fi
    log_pass "At least one model is available"

    # Run tests
    run_test "Basic headless transcription (--json)" test_transcribe_file_basic "$BINARY" "$wav_file" || true
    run_test "Plain text transcription output" test_transcribe_file_plain "$BINARY" "$wav_file" || true
    run_test "Repeated transcription (--repeat 3)" test_transcribe_file_repeat "$BINARY" "$wav_file" || true

    # Print summary
    print_summary "Transcription Tests"
    return $?
}

main "$@"