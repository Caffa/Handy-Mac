#!/usr/bin/env bash
#
# scripts/resign-stable-dr.sh — Re-sign the Handy.app with a stable designated requirement
#
# Problem: Tauri builds with `signingIdentity: "-"` produce ad-hoc signatures whose
# designated requirement is a cdhash. The cdhash changes on every build, which causes
# macOS TCC (Transparency, Consent, and Control) to treat the updated app as a
# completely different program and reset all permissions (Accessibility, Microphone, etc.).
#
# Solution: Re-sign with a designated requirement based on the bundle identifier,
# which is stable across builds. This means:
#   Before: designated => cdhash H"4272a9dd7cd73ae1596f0d8f6864987d3e86147c"
#   After:  designated => identifier "com.pais.handy"
#
# macOS TCC then recognizes the updated app as "the same program" and preserves
# Accessibility, Microphone, and other permissions across updates.
#
# This script:
#   1. Re-signs the .app bundle with a stable DR
#   2. Re-creates the updater tar.gz
#   3. Re-signs the tar.gz with the Tauri updater signing key
#   4. Generates latest.json for the update server
#
# Usage:
#   ./scripts/resign-stable-dr.sh
#
# Environment variables:
#   BUNDLE_DIR          Path to bundle output (default: src-tauri/target/release/bundle/macos)
#   TAURI_SIGNING_PRIVATE_KEY_PATH   Path to updater signing key (default: ~/.tauri/handy-fork.key)
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  Password for the signing key (default: handy)
#   PORT                Update server port (default: 4321)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TAURI_DIR="$PROJECT_ROOT/src-tauri"
BUNDLE_DIR="${BUNDLE_DIR:-$TAURI_DIR/target/release/bundle/macos}"
APP_PATH="$BUNDLE_DIR/Handy.app"
TAR_GZ="$BUNDLE_DIR/Handy.app.tar.gz"
SIG_FILE="$TAR_GZ.sig"
ENTITLEMENTS="$TAURI_DIR/Entitlements.plist"
BUNDLE_ID="com.pais.handy"
KEY_FILE="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.tauri/handy-fork.key}"
KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-handy}"
LOCAL_PORT="${PORT:-4321}"

# Read version from tauri.conf.json
VERSION=$(grep '"version"' "$TAURI_DIR/tauri.conf.json" | head -1 | sed 's/.*: "//;s/".*//;s/\s*,//')

echo "🔐 Re-signing Handy.app with stable designated requirement..."
echo "   App: $APP_PATH"
echo "   DR:  identifier \"$BUNDLE_ID\""

# Step 1: Re-sign the .app with stable DR
codesign --force -s - \
  -r="designated => identifier \"$BUNDLE_ID\"" \
  --entitlements "$ENTITLEMENTS" \
  --options runtime \
  "$APP_PATH"

# Verify the new DR
NEW_DR=$(codesign -d -r- "$APP_PATH" 2>&1 | grep "^designated" || true)
echo "   New DR: $NEW_DR"

# Step 2: Re-create the updater tar.gz
echo "📦 Creating updater tar.gz..."
rm -f "$TAR_GZ"
cd "$BUNDLE_DIR"
tar -czf Handy.app.tar.gz Handy.app
echo "   $(du -h Handy.app.tar.gz | cut -f1)"

# Step 3: Sign the tar.gz with the Tauri updater key
echo "🔑 Signing updater tar.gz..."
TAURI_SIGNING_PRIVATE_KEY_PATH="$KEY_FILE" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD" \
bunx tauri signer sign "$TAR_GZ"

# Step 4: Generate latest.json
SIGNATURE=$(cat "$SIG_FILE")
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
TAR_FILENAME="$(basename "$TAR_GZ")"
LATEST_JSON="$BUNDLE_DIR/latest.json"

# Determine platform
case "$(uname -m)" in
    arm64)  PLATFORM="darwin-aarch64" ;;
    x86_64) PLATFORM="darwin-x86_64" ;;
    *)      echo "⚠️  Unsupported arch: $(uname -m)"; exit 1 ;;
esac

cat > "$LATEST_JSON" << HEREDOC
{
  "version": "$VERSION",
  "notes": "Local fork build ($VERSION)",
  "pub_date": "$TIMESTAMP",
  "platforms": {
    "$PLATFORM": {
      "signature": "$SIGNATURE",
      "url": "http://localhost:$LOCAL_PORT/$TAR_FILENAME"
    },
    "${PLATFORM}-app": {
      "signature": "$SIGNATURE",
      "url": "http://localhost:$LOCAL_PORT/$TAR_FILENAME"
    }
  }
}
HEREDOC

echo "✅ Done!"
echo "   📦 $TAR_GZ ($(du -h "$TAR_GZ" | cut -f1))"
echo "   🔑 $SIG_FILE"
echo "   📜 $LATEST_JSON"
echo ""
echo "The .app is now signed with DR: identifier \"$BUNDLE_ID\""
echo "macOS will preserve Accessibility, Microphone, and other permissions across updates."