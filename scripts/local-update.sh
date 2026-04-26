#!/usr/bin/env bash
#
# scripts/local-update.sh — Build Handy, serve as a local update via the built-in updater
#
# This script repurposes Handy's existing Tauri updater to serve local builds.
# Instead of manually replacing the .app bundle, you just build, serve, and
# click "Check for Updates" in the running app.
#
# How it works:
#   1. Reads the signing key from ~/.tauri/handy-fork.key
#   2. (Optionally) bumps the version in tauri.conf.json so the updater detects it
#   3. Patches tauri.conf.json endpoints to point at localhost
#   4. Builds the app with updater artifacts (tar.gz + .sig)
#   5. Generates latest.json from the build output
#   6. Serves the update directory on a local HTTP server
#   7. Restores tauri.conf.json when done (Ctrl+C)
#
# Prerequisites (one-time setup):
#   mkdir -p ~/.tauri
#   echo "handy" | bunx tauri signer generate -w ~/.tauri/handy-fork.key
#   # Then paste the pubkey from ~/.tauri/handy-fork.key.pub into
#   # src-tauri/tauri.conf.json → plugins.updater.pubkey
#
# IMPORTANT: Your installed Handy.app must be built with the fork's pubkey.
# If you previously installed from the upstream Handy release, you need to
# run this script once (or scripts/build-and-install.sh) to get a fork-signed
# build installed first. After that, all future updates work via the updater.
#
# Usage:
#   ./scripts/local-update.sh                  # Build + serve on port 4321
#   ./scripts/local-update.sh --bump            # Bump patch version, then build + serve
#   ./scripts/local-update.sh --bump minor       # Bump minor version
#   ./scripts/local-update.sh --skip-build      # Serve only (skip build)
#   ./scripts/local-update.sh --permanent       # Don't restore config on exit
#   PORT=8080 ./scripts/local-update.sh         # Custom port
#
# Environment variables:
#   TAURI_SIGNING_PRIVATE_KEY_PATH  Path to signing key (default: ~/.tauri/handy-fork.key)
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  Password for the key (default: "handy")
#   PORT    Local server port (default: 4321)

set -euo pipefail

# ─── Configuration ────────────────────────────────────────────────────────────
APP_NAME="Handy"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TAURI_DIR="$PROJECT_ROOT/src-tauri"
CONF_FILE="$TAURI_DIR/tauri.conf.json"
CARGO_TOML="$TAURI_DIR/Cargo.toml"
PACKAGE_JSON="$PROJECT_ROOT/package.json"
KEY_FILE="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.tauri/handy-fork.key}"
KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-handy}"
LOCAL_PORT="${PORT:-4321}"
SKIP_BUILD=false
PERMANENT=false
BUMP_LEVEL=""  # empty = no bump, "patch", "minor", "major"

# Platform key for Tauri updater JSON
# Maps macOS arch → Tauri platform string
case "$(uname -m)" in
    arm64)  PLATFORM="darwin-aarch64" ;;
    x86_64) PLATFORM="darwin-x86_64" ;;
    *)      echo "⚠️  Unsupported arch: $(uname -m)"; exit 1 ;;
esac

# ─── Parse args ────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=true ;;
        --permanent)  PERMANENT=true ;;
        --bump)
            shift
            BUMP_LEVEL="${1:-patch}"
            ;;
        --bump=*)
            BUMP_LEVEL="${1#--bump=}"
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --skip-build     Skip build step, serve existing artifacts"
            echo "  --permanent      Don't restore tauri.conf.json on exit"
            echo "  --bump [LEVEL]   Bump version before building (patch|minor|major, default: patch)"
            echo "  --help           Show this help message"
            echo ""
            echo "Environment variables:"
            echo "  PORT                             Local server port (default: 4321)"
            echo "  TAURI_SIGNING_PRIVATE_KEY_PATH   Path to signing key (default: ~/.tauri/handy-fork.key)"
            echo "  TAURI_SIGNING_PRIVATE_KEY_PASSWORD  Password for the key (default: handy)"
            echo ""
            echo "Examples:"
            echo "  $0                          # Build and serve"
            echo "  $0 --bump                   # Bump patch version, build and serve"
            echo "  $0 --bump minor             # Bump minor version, build and serve"
            echo "  $0 --skip-build             # Re-serve last build"
            echo "  $0 --permanent              # Keep localhost endpoint in config permanently"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Run $0 --help for usage."
            exit 1
            ;;
    esac
    shift
done

# ─── Preflight checks ─────────────────────────────────────────────────────────
if [[ ! -f "$KEY_FILE" ]]; then
    echo "❌ Signing key not found at: $KEY_FILE"
    echo ""
    echo "Generate one with:"
    echo "  mkdir -p ~/.tauri"
    echo '  echo "" | bunx tauri signer generate -w ~/.tauri/handy-fork.key'
    echo ""
    echo "Then update the pubkey in $CONF_FILE with the output from:"
    echo "  cat ~/.tauri/handy-fork.key.pub"
    exit 1
fi

# ─── Helper: bump semver ──────────────────────────────────────────────────────
bump_version() {
    local current="$1"
    local level="$2"
    local major minor patch
    IFS='.' read -r major minor patch <<< "$current"

    case "$level" in
        patch) patch=$((patch + 1)) ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        major) major=$((major + 1)); minor=0; patch=0 ;;
        *) echo "❌ Unknown bump level: $level"; exit 1 ;;
    esac

    echo "${major}.${minor}.${patch}"
}

# ─── Read current version ──────────────────────────────────────────────────────
VERSION=$(grep '"version"' "$CONF_FILE" | head -1 | sed 's/.*: "//;s/".*//;s/\s*,//')
if [[ -z "$VERSION" ]]; then
    echo "❌ Could not read version from $CONF_FILE"
    exit 1
fi

# ─── Bump version if requested ─────────────────────────────────────────────────
if [[ -n "$BUMP_LEVEL" ]]; then
    NEW_VERSION=$(bump_version "$VERSION" "$BUMP_LEVEL")
    echo "📈 Bumping version: $VERSION → $NEW_VERSION ($BUMP_LEVEL)"

    # Update all version locations
    if [[ "$(uname -s)" == "Darwin" ]]; then
        sed -i '' "s/\"version\": \"$VERSION\"/\"version\": \"$NEW_VERSION\"/g" "$CONF_FILE"
        sed -i '' "s/^version = \"$VERSION\"/version = \"$NEW_VERSION\"/" "$CARGO_TOML"
        sed -i '' "s/\"version\": \"$VERSION\"/\"version\": \"$NEW_VERSION\"/" "$PACKAGE_JSON"
    else
        sed -i "s/\"version\": \"$VERSION\"/\"version\": \"$NEW_VERSION\"/g" "$CONF_FILE"
        sed -i "s/^version = \"$VERSION\"/version = \"$NEW_VERSION\"/" "$CARGO_TOML"
        sed -i "s/\"version\": \"$VERSION\"/\"version\": \"$NEW_VERSION\"/" "$PACKAGE_JSON"
    fi

    VERSION="$NEW_VERSION"
fi

echo "📋 Version: $VERSION"
echo "🍎 Platform: $PLATFORM"

# ─── Backup tauri.conf.json ────────────────────────────────────────────────────
CONF_BACKUP="$CONF_FILE.local-update-backup"

if [[ "$PERMANENT" == false ]]; then
    cp "$CONF_FILE" "$CONF_BACKUP"
    echo "📦 Backed up tauri.conf.json (will restore on exit)"
else
    echo "📌 Permanent mode — config changes will NOT be reverted"
fi

# ─── Patch endpoints to localhost ────────────────────────────────────────────
LOCAL_ENDPOINT="http://localhost:$LOCAL_PORT/latest.json"

if [[ "$(uname -s)" == "Darwin" ]]; then
    # Replace any endpoint URL with our local one
    sed -i '' -E "s|https?://[^\"]+/latest\.json|$LOCAL_ENDPOINT|g" "$CONF_FILE"
else
    sed -i -E "s|https?://[^\"]+/latest\.json|$LOCAL_ENDPOINT|g" "$CONF_FILE"
fi

echo "🔗 Patched endpoint → $LOCAL_ENDPOINT"

# Ensure Tauri allows HTTP connections for local updates.
# Tauri v2 updater uses the `dangerous_remote_update_interval` or the
# `allow_downgrades` fields. For local dev over HTTP, we need to check
# if there's a transport security config. Since tauri-plugin-updater v2
# allows HTTP for localhost by default, this should just work.

# ─── Restore on exit ──────────────────────────────────────────────────────────
cleanup() {
    EXIT_CODE=$?
    echo ""
    if [[ "$PERMANENT" == false && -f "$CONF_BACKUP" ]]; then
        echo "🧹 Restoring tauri.conf.json..."
        mv "$CONF_BACKUP" "$CONF_FILE"
        echo "✅ Restored."
    fi
    # Kill the server if it's running
    if [[ -n "${SERVER_PID:-}" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        echo "🛑 Server stopped."
    fi
    exit "$EXIT_CODE"
}
trap cleanup EXIT INT TERM

# ─── Build ────────────────────────────────────────────────────────────────────
BUNDLE_DIR="$TAURI_DIR/target/release/bundle/macos"
TAR_GZ="$BUNDLE_DIR/$APP_NAME.app.tar.gz"
SIG_FILE="$TAR_GZ.sig"

if [[ "$SKIP_BUILD" == true ]]; then
    echo "⏩ Skipping build (--skip-build)"
    if [[ ! -f "$TAR_GZ" ]]; then
        echo "❌ No build artifacts found at $TAR_GZ"
        echo "   Run without --skip-build first."
        exit 1
    fi
else
    echo ""
    echo "🔨 Building Handy with updater artifacts..."
    echo "   This may take several minutes on the first run."
    echo ""

    export TAURI_SIGNING_PRIVATE_KEY_PATH="$KEY_FILE"
    export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD"

    # Build with cmake workaround for macOS
    CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build

    if [[ ! -f "$TAR_GZ" ]]; then
        echo "❌ Build succeeded but no .tar.gz found at $TAR_GZ"
        echo "   Check that createUpdaterArtifacts is true in tauri.conf.json"
        exit 1
    fi
fi

# ─── Re-sign with stable DR ──────────────────────────────────────────────────────
# Ad-hoc signing (signingIdentity: "-") produces a cdhash-based designated requirement
# that changes on every build. macOS TCC ties Accessibility, Microphone, and other
# permissions to the DR, so a changing cdhash causes permission resets after update.
# Re-signing with identifier-based DR ensures permissions persist across updates.
BUNDLE_ID="com.pais.handy"
APP_PATH="$BUNDLE_DIR/$APP_NAME.app"

if [[ -d "$APP_PATH" ]]; then
    echo ""
    echo "🔐 Re-signing with stable designated requirement..."
    echo "   DR: identifier \"$BUNDLE_ID\""

    codesign --force -s - \
        -r="designated => identifier \"$BUNDLE_ID\"" \
        --entitlements "$TAURI_DIR/Entitlements.plist" \
        --options runtime \
        "$APP_PATH"

    # Re-create the updater tar.gz with the re-signed app
    echo "📦 Re-creating updater tar.gz with re-signed app..."
    rm -f "$TAR_GZ"
    cd "$BUNDLE_DIR"
    tar -czf "$APP_NAME.app.tar.gz" "$APP_NAME.app"

    echo "✅ Re-signed with stable DR"
fi

# ─── Verify/sign if no .sig file ───────────────────────────────────────────────
if [[ ! -f "$SIG_FILE" ]]; then
    echo ""
    echo "⚠️  No .sig file found at $SIG_FILE"
    echo "   Signing manually with your key..."
    TAURI_SIGNING_PRIVATE_KEY_PATH="$KEY_FILE" \
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$KEY_PASSWORD" \
    bunx tauri signer sign "$TAR_GZ"
fi

# ─── Generate latest.json ────────────────────────────────────────────────────
SIGNATURE=$(cat "$SIG_FILE")
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
TAR_FILENAME="$(basename "$TAR_GZ")"
LATEST_JSON="$BUNDLE_DIR/latest.json"

# Generate platform entries.
# For macOS we provide both the base target and the -app variant (Tauri convention).
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

echo ""
echo "✅ Build complete!"
echo "   📦 $TAR_GZ ($(du -h "$TAR_GZ" | cut -f1))"
echo "   🔑 $SIG_FILE"
echo "   📜 $LATEST_JSON"
echo ""
echo "─────────────────────────────────────────────────────────────"
echo "🚀  Local update server starting on http://localhost:$LOCAL_PORT"
echo "─────────────────────────────────────────────────────────────"
echo ""
echo "   To update your running Handy app:"
echo "     1. Make sure Handy is running (this build or an earlier one)"
echo "     2. Open Handy → Settings (or click the tray icon)"
echo "     3. Click 'Check for Updates' in the footer"
echo "     4. Click the update button to download and install"
echo ""
if [[ "$PERMANENT" == false ]]; then
    echo "   ⚠️  Config will be restored when you press Ctrl+C"
fi
echo ""
echo "   Press Ctrl+C to stop the server"

# ─── Serve ────────────────────────────────────────────────────────────────────
cd "$BUNDLE_DIR"

# Use python for the HTTP server (available on all macOS systems)
python3 -m http.server "$LOCAL_PORT" &
SERVER_PID=$!

# Wait for the server — Ctrl+C triggers cleanup
wait "$SERVER_PID"