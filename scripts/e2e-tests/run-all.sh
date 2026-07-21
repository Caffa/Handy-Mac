#!/usr/bin/env bash
# run-all.sh — Master E2E test runner for Handy-Mac
#
# Runs all E2E tests in sequence, reports pass/fail for each.
# Exit 0 if all pass, 1 if any fail.
#
# Usage:
#   ./scripts/e2e-tests/run-all.sh [--release] [--debug] [--skip-startup]
#                                   [--skip-settings] [--skip-transcribe]
#                                   [--skip-consecutive] [--skip-cli]
#                                   [--consecutive-runs N]
#
# Options:
#   --release            Use release binary (default)
#   --debug              Use debug binary
#   --skip-startup       Skip app startup test
#   --skip-settings      Skip settings persistence test
#   --skip-transcribe    Skip transcription test
#   --skip-consecutive   Skip consecutive runs test
#   --skip-cli           Skip CLI flags test
#   --consecutive-runs N Set number of consecutive runs (default: 5)
#   -h, --help           Show this help message

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Colors ──────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

# ── Defaults ─────────────────────────────────────────────────────────────
SKIP_STARTUP=false
SKIP_SETTINGS=false
SKIP_TRANSCRIBE=false
SKIP_CONSECUTIVE=false
SKIP_CLI=false
CONSECUTIVE_RUNS=5

# ── Parse arguments ───────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            export HANDY_BUILD_TYPE=release
            shift
            ;;
        --debug)
            export HANDY_BUILD_TYPE=debug
            shift
            ;;
        --skip-startup)
            SKIP_STARTUP=true
            shift
            ;;
        --skip-settings)
            SKIP_SETTINGS=true
            shift
            ;;
        --skip-transcribe)
            SKIP_TRANSCRIBE=true
            shift
            ;;
        --skip-consecutive)
            SKIP_CONSECUTIVE=true
            shift
            ;;
        --skip-cli)
            SKIP_CLI=true
            shift
            ;;
        --consecutive-runs)
            CONSECUTIVE_RUNS="${2:-5}"
            shift 2
            ;;
        -h|--help)
            head -n 18 "$0" | tail -n 15
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${RESET}" >&2
            exit 1
            ;;
    esac
done

# ── Test definitions ──────────────────────────────────────────────────────
# Use a simple string list instead of associative array for macOS bash 3.2 compat.
# Format: "name:STATUS" per line, stored in TEST_RESULTS_LIST.
TEST_RESULTS_LIST=""
TOTAL_TESTS=0
TOTAL_PASSED=0
TOTAL_FAILED=0
TOTAL_SKIPPED=0

# ── Helper: Record a test result ─────────────────────────────────────────
record_result() {
    local name="$1"
    local status="$2"
    TEST_RESULTS_LIST="${TEST_RESULTS_LIST}${name}:${status}
"
}

# ── Helper: Run a test script ─────────────────────────────────────────────
run_test_script() {
    local name="$1"
    local script="$2"
    shift 2
    local args=("${@:-}")

    TOTAL_TESTS=$((TOTAL_TESTS + 1))

    if [[ ! -x "$script" ]]; then
        echo -e "${RED}${BOLD}[FAIL]${RESET} ${name}: script not executable: $script"
        TOTAL_FAILED=$((TOTAL_FAILED + 1))
        record_result "$name" "FAIL"
        return 1
    fi

    echo ""
    echo -e "${CYAN}${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
    echo -e "${CYAN}${BOLD}  Running: ${name}${RESET}"
    echo -e "${CYAN}${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
    echo ""

    local rc=0
    "$script" "${args[@]}" || rc=$?

    case $rc in
        0)
            echo -e "\n${GREEN}${BOLD}[PASS]${RESET} ${name}\n"
            TOTAL_PASSED=$((TOTAL_PASSED + 1))
            record_result "$name" "PASS"
            ;;
        2)
            echo -e "\n${YELLOW}${BOLD}[SKIP]${RESET} ${name}\n"
            TOTAL_SKIPPED=$((TOTAL_SKIPPED + 1))
            record_result "$name" "SKIP"
            ;;
        *)
            echo -e "\n${RED}${BOLD}[FAIL]${RESET} ${name} (exit code: $rc)\n"
            TOTAL_FAILED=$((TOTAL_FAILED + 1))
            record_result "$name" "FAIL"
            ;;
    esac

    return $rc
}

# ── Skip helper ───────────────────────────────────────────────────────────
skip_test() {
    local name="$1"
    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    TOTAL_SKIPPED=$((TOTAL_SKIPPED + 1))
    record_result "$name" "SKIP"
    echo -e "${YELLOW}${BOLD}[SKIP]${RESET} ${name} (explicitly skipped)"
}

# ── Main ─────────────────────────────────────────────────────────────────
main() {
    echo -e "${BOLD}╔═══════════════════════════════════════════════════════╗${RESET}"
    echo -e "${BOLD}║          Handy-Mac E2E Test Suite                    ║${RESET}"
    echo -e "${BOLD}╚═══════════════════════════════════════════════════════╝${RESET}"
    echo ""
    echo -e "  Build type: ${BOLD}${HANDY_BUILD_TYPE:-release}${RESET}"
    echo -e "  Consecutive runs: ${BOLD}${CONSECUTIVE_RUNS}${RESET}"
    echo ""

    # Verify binary exists before running any tests
    local binary
    binary=$("$SCRIPT_DIR/common.sh" 2>/dev/null; find_binary 2>/dev/null) || true
    if [[ -z "$binary" || ! -x "$binary" ]]; then
        # Try sourcing common.sh and finding the binary properly
        source "$SCRIPT_DIR/common.sh"
        if ! binary=$(find_binary 2>/dev/null); then
            echo -e "${RED}${BOLD}ERROR:${RESET} Handy-Mac binary not found."
            echo -e "  Build with: ${CYAN}bun run tauri build${RESET} (from Handy-Mac/)"
            echo -e "  Or set ${CYAN}HANDY_BINARY${RESET} env var to the binary path."
            exit 1
        fi
    else
        source "$SCRIPT_DIR/common.sh"
        binary=$(find_binary 2>/dev/null) || true
    fi

    echo -e "  Binary: ${BOLD}${binary}${RESET}"
    echo ""

    # Kill any existing Handy processes before starting tests
    kill_all_handy

    # ── Run tests ──────────────────────────────────────────────────────

    if [[ "$SKIP_STARTUP" == true ]]; then
        skip_test "App Startup"
    else
        run_test_script "App Startup" "$SCRIPT_DIR/test-app-startup.sh"
    fi

    if [[ "$SKIP_SETTINGS" == true ]]; then
        skip_test "Settings Persistence"
    else
        run_test_script "Settings Persistence" "$SCRIPT_DIR/test-settings-persistence.sh"
    fi

    if [[ "$SKIP_TRANSCRIBE" == true ]]; then
        skip_test "Transcription"
    else
        run_test_script "Transcription" "$SCRIPT_DIR/test-transcribe-file.sh"
    fi

    if [[ "$SKIP_CONSECUTIVE" == true ]]; then
        skip_test "Consecutive Runs"
    else
        run_test_script "Consecutive Runs" "$SCRIPT_DIR/test-consecutive-runs.sh" "$CONSECUTIVE_RUNS"
    fi

    if [[ "$SKIP_CLI" == true ]]; then
        skip_test "CLI Flags"
    else
        run_test_script "CLI Flags" "$SCRIPT_DIR/test-cli-flags.sh"
    fi

    # ── Summary ─────────────────────────────────────────────────────────

    echo ""
    echo -e "${BOLD}╔═══════════════════════════════════════════════════════╗${RESET}"
    echo -e "${BOLD}║                E2E Test Suite Results                 ║${RESET}"
    echo -e "${BOLD}╚═══════════════════════════════════════════════════════╝${RESET}"
    echo ""

    echo "$TEST_RESULTS_LIST" | while IFS=: read -r name status; do
        [[ -z "$name" ]] && continue
        case $status in
            PASS) echo -e "  ${GREEN}${BOLD}✓ PASS${RESET}  $name" ;;
            FAIL) echo -e "  ${RED}${BOLD}✗ FAIL${RESET}  $name" ;;
            SKIP) echo -e "  ${YELLOW}${BOLD}⊘ SKIP${RESET}  $name" ;;
        esac
    done

    echo ""
    echo -e "  ${BOLD}Total:${RESET}  $TOTAL_TESTS"
    echo -e "  ${GREEN}${BOLD}Passed:${RESET} $TOTAL_PASSED"
    echo -e "  ${RED}${BOLD}Failed:${RESET} $TOTAL_FAILED"
    echo -e "  ${YELLOW}${BOLD}Skipped:${RESET} $TOTAL_SKIPPED"
    echo ""

    # Final cleanup — make sure no Handy processes are left
    kill_all_handy

    if [[ $TOTAL_FAILED -gt 0 ]]; then
        echo -e "${RED}${BOLD}Some tests failed.${RESET}"
        return 1
    fi

    echo -e "${GREEN}${BOLD}All tests passed!${RESET}"
    return 0
}

main "$@"