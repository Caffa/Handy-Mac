#!/usr/bin/env bash
# Manual smoke test for the Handy voice-insert HTTP API (Phase 1).
#
# Prerequisites: Handy is RUNNING with voice_insert_enabled = true
# (Settings → Integrations). The script exercises:
#   health → unauthenticated 401 → state → 409 while idle → start recording
#   (CLI toggle) → two spaced inserts → CANCEL via --cancel (a real CLI flag,
#   see cli.rs; it routes to utils::cancel_current_operation) → 409 after
#   cancel → state back to idle.
#
# Note: cancel leaves the registry buffer unfinalized by design; the server's
# is_recording gate is what makes late inserts answer 409 in that case.
#
# Env overrides:
#   HANDY_TOKEN  bearer token (default: read from the settings store)
#   HANDY_PORT   server port (default: 48213)
#   HANDY_BIN    Handy binary used for --toggle-transcription
#                (default: /Applications/Handy.app/Contents/MacOS/Handy)
#   HANDY_STORE  settings store json path (default below)

set -euo pipefail

BASE="http://127.0.0.1:${HANDY_PORT:-48213}"
HANDY_BIN="${HANDY_BIN:-/Applications/Handy.app/Contents/MacOS/Handy}"
HANDY_STORE="${HANDY_STORE:-$HOME/Library/Application Support/com.pais.handy/settings_store.json}"

# ── Token: env override, else read from the settings store ────────────
# Store format: {"settings": { "voice_insert_token": "...", ... }}
TOKEN="${HANDY_TOKEN:-}"
if [[ -z "$TOKEN" ]]; then
  TOKEN="$(python3 - "$HANDY_STORE" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    settings = data.get("settings") or {}
    print(settings.get("voice_insert_token") or "")
except FileNotFoundError:
    print("settings store not found: %s" % sys.argv[1], file=sys.stderr)
    print("")
except (json.JSONDecodeError, OSError) as exc:
    print("cannot read settings store: %s" % exc, file=sys.stderr)
    print("")
PY
)"
  if [[ -z "$TOKEN" ]]; then
    echo "error: no voice_insert_token found in $HANDY_STORE." >&2
    echo "Enable the Voice Insert API in Settings → Integrations, click Regenerate," >&2
    echo "or pass the token explicitly: HANDY_TOKEN=<token> $0" >&2
    exit 1
  fi
fi

# Assert an HTTP status code; prints the actual code either way.
expect_code() {
  local expected="$1" actual="$2" label="$3"
  if [[ "$actual" == "$expected" ]]; then
    echo "   ✓ $label → $actual"
  else
    echo "   ✗ $label → expected $expected, got $actual" >&2
    exit 1
  fi
}

echo "1) health (no auth required):"
curl -s --max-time 5 "$BASE/health"; echo

echo "2) state without token (expect 401):"
code="$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' "$BASE/state")"
expect_code 401 "$code" "state unauthenticated"

echo "3) state with token:"
curl -s --max-time 5 -H "Authorization: Bearer $TOKEN" "$BASE/state"; echo

echo "4) insert while idle (expect 409):"
code="$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"text":"[[Idle Token]]"}' "$BASE/insert")"
expect_code 409 "$code" "insert while idle"

echo "5) recording: toggling via $HANDY_BIN"
"$HANDY_BIN" --toggle-transcription
sleep 3
code="$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"text":"[[Note A]]"}' "$BASE/insert")"
expect_code 200 "$code" "insert Note A"
echo "   (speak anything, or stay silent — tokens paste regardless)"
sleep 3
code="$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"text":"[[Note B]]"}' "$BASE/insert")"
expect_code 200 "$code" "insert Note B"
sleep 2

echo "6) cancel the recording via --cancel (real CLI flag, routes to"
echo "   utils::cancel_current_operation — NOT the toggle stop):"
"$HANDY_BIN" --cancel
sleep 1

echo "7) state after cancel:"
state="$(curl -s --max-time 5 -H "Authorization: Bearer $TOKEN" "$BASE/state")"
echo "   $state"

echo "8) insert after cancel (expect 409):"
code="$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"text":"[[Late Token]]"}' "$BASE/insert")"
expect_code 409 "$code" "insert after cancel"

echo
echo "Done. Check the pasted text for the tokens (as colored [[wikilinks]]"
echo "in live captions, and spliced into the final transcription). The"
echo "cancel step intentionally drops this recording's tokens — late"
echo "inserts must 409 via the recording gate."
