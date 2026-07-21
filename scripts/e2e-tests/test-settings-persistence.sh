#!/usr/bin/env bash
# test-settings-persistence.sh — E2E test: Do settings save and load correctly?
#
# 1. Start app with --start-hidden --no-tray
# 2. Wait for startup
# 3. Modify the settings JSON file on disk
# 4. Kill app gracefully
# 5. Restart app
# 6. Verify settings file still contains the changes
# 7. Kill app
#
# Exit codes: 0=pass, 1=fail, 2=skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

SETTINGS_FILE="$APP_DATA_DIR/settings_store.json"

# ── Cleanup ──────────────────────────────────────────────────────────────
BINARY=""
SETTINGS_BACKUP=""

cleanup() {
    # Restore settings backup if we made one
    if [[ -n "$SETTINGS_BACKUP" && -f "$SETTINGS_BACKUP" ]]; then
        mv "$SETTINGS_BACKUP" "$SETTINGS_FILE" 2>/dev/null || true
    fi
    if [[ -n "$BINARY" ]]; then
        kill_app || true
    fi
}
trap cleanup EXIT

# ── Test: Settings persist across restarts ────────────────────────────────
test_settings_persist() {
    local binary="$1"

    # Check if jq is available
    if ! command -v jq &>/dev/null; then
        log_skip "jq not available, cannot test settings persistence"
        return 2
    fi

    # Back up existing settings
    if [[ -f "$SETTINGS_FILE" ]]; then
        SETTINGS_BACKUP="${SETTINGS_FILE}.e2e-backup"
        cp "$SETTINGS_FILE" "$SETTINGS_BACKUP"
        log_info "Backed up existing settings to $SETTINGS_BACKUP"
    fi

    # Make sure the app data directory exists
    mkdir -p "$(dirname "$SETTINGS_FILE")"

    log_info "Starting app to generate default settings..."
    "$binary" --start-hidden --no-tray &>/dev/null &

    if ! wait_for_app "$binary" "$STARTUP_TIMEOUT"; then
        log_fail "App did not start"
        return 1
    fi

    # Wait a bit for settings to be written
    sleep 2

    # Check that settings file was created
    if [[ ! -f "$SETTINGS_FILE" ]]; then
        log_fail "Settings file not found at $SETTINGS_FILE"
        kill_app || true
        return 1
    fi

    log_pass "Settings file created at $SETTINGS_FILE"

    # Read a setting value to verify structure
    local current_debug_mode
    current_debug_mode=$(jq -r '.debug_mode // false' "$SETTINGS_FILE" 2>/dev/null || echo "null")
    log_info "Current debug_mode setting: $current_debug_mode"

    # Modify a safe setting (debug_mode) that won't break the app
    local new_value="true"
    if [[ "$current_debug_mode" == "true" ]]; then
        new_value="false"
    fi

    log_info "Setting debug_mode to $new_value..."
    local tmp_settings="${SETTINGS_FILE}.tmp"
    jq --arg v "$new_value" '.debug_mode = ($v == "true")' "$SETTINGS_FILE" > "$tmp_settings" && \
        mv "$tmp_settings" "$SETTINGS_FILE"

    # Verify the file was written correctly
    local verify_value
    verify_value=$(jq -r '.debug_mode' "$SETTINGS_FILE" 2>/dev/null || echo "null")
    if [[ "$verify_value" != "$new_value" ]]; then
        log_fail "Settings file was not written correctly (expected $new_value, got $verify_value)"
        kill_app || true
        return 1
    fi
    log_info "Verified settings file has debug_mode=$verify_value"

    # Kill the app gracefully
    log_info "Stopping app..."
    kill_app || true

    # Restart the app
    log_info "Restarting app..."
    "$binary" --start-hidden --no-tray &>/dev/null &

    if ! wait_for_app "$binary" "$STARTUP_TIMEOUT"; then
        log_fail "App did not restart"
        return 1
    fi

    # Wait for settings to be loaded
    sleep 2

    # Verify the setting persisted
    local persisted_value
    persisted_value=$(jq -r '.debug_mode' "$SETTINGS_FILE" 2>/dev/null || echo "null")

    if [[ "$persisted_value" == "$new_value" ]]; then
        log_pass "Setting debug_mode=$persisted_value persisted across restart"
    else
        log_fail "Setting did not persist: expected $new_value, got $persisted_value"
        kill_app || true
        return 1
    fi

    # Restore the original value by flipping it back
    local restore_value
    if [[ "$new_value" == "true" ]]; then
        restore_value="false"
    else
        restore_value="true"
    fi

    log_info "Restoring debug_mode to $restore_value..."
    jq --arg v "$restore_value" '.debug_mode = ($v == "true")' "$SETTINGS_FILE" > "$tmp_settings" && \
        mv "$tmp_settings" "$SETTINGS_FILE"

    kill_app || true
    return 0
}

# ── Test: Settings file structure is valid ────────────────────────────────
test_settings_structure() {
    local binary="$1"

    if ! command -v jq &>/dev/null; then
        log_skip "jq not available, cannot validate settings structure"
        return 2
    fi

    # Start the app to ensure settings exist
    "$binary" --start-hidden --no-tray &>/dev/null &

    if ! wait_for_app "$binary" "$STARTUP_TIMEOUT"; then
        log_fail "App did not start"
        return 1
    fi

    sleep 2

    if [[ ! -f "$SETTINGS_FILE" ]]; then
        log_fail "Settings file not found at $SETTINGS_FILE"
        kill_app || true
        return 1
    fi

    # Validate JSON structure
    local keys
    keys=$(jq 'keys' "$SETTINGS_FILE" 2>/dev/null) || {
        log_fail "Settings file is not valid JSON"
        kill_app || true
        return 1
    }

    local key_count
    key_count=$(echo "$keys" | jq 'length' 2>/dev/null)

    if [[ "$key_count" -lt 1 ]]; then
        log_fail "Settings file has no keys"
        kill_app || true
        return 1
    fi

    log_pass "Settings file is valid JSON with $key_count keys"

    # Check for expected key
    if echo "$keys" | jq -e '. | index("selected_model")' &>/dev/null; then
        log_pass "Settings contains 'selected_model' key"
    else
        log_warn "Settings does not contain 'selected_model' key (may use different schema)"
    fi

    kill_app || true
    return 0
}

# ── Main ─────────────────────────────────────────────────────────────────
main() {
    log_test "Handy-Mac Settings Persistence E2E Test"
    echo ""

    # Find the binary
    BINARY=$(find_binary) || return 1
    log_info "Using binary: $BINARY"

    # Check if settings file location is accessible
    if [[ ! -d "$(dirname "$SETTINGS_FILE")" ]]; then
        log_skip "App data directory does not exist yet. Run the app once first."
        return 2
    fi

    # Make sure no Handy process is already running
    kill_all_handy

    # Run tests
    run_test "Settings file structure is valid" "test_settings_structure $BINARY" || true
    run_test "Settings persist across restarts" "test_settings_persist $BINARY" || true

    # Final cleanup
    kill_all_handy

    # Print summary
    print_summary "Settings Persistence Tests"
    return $?
}

main "$@"