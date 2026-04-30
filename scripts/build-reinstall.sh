#!/usr/bin/env bash
#
# scripts/build-reinstall.sh — Full clean reinstall of Handy via Rapidmg
#
# This is the recommended build+deploy workflow for AI agents.
# It quits Handy, deletes the old app, builds, creates a DMG,
# opens it with Rapidmg for auto-install, and re-signs with a stable DR.
#
# Prerequisites:
#   - Bun (https://bun.sh)
#   - Rust (https://rustup.rs)
#   - Rapidmg installed at /Applications/Rapidmg.app
#
# Usage:
#   ./scripts/build-reinstall.sh              # Full build + reinstall
#   ./scripts/build-reinstall.sh --skip-build # Reinstall last build only
#   ./scripts/build-reinstall.sh --launch      # Also launch after install
#
# Environment variables:
#   INSTALL_DEST  Where to install (default: /Applications)
#   SKIP_BUILD    Set to "1" to skip the build step

set -euo pipefail

# ─── Configuration ────────────────────────────────────────────────────────────
APP_NAME="Handy"
APP_BUNDLE="${APP_NAME}.app"
BUNDLE_ID="com.pais.handy"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TAURI_DIR="$PROJECT_ROOT/src-tauri"
BUNDLE_DIR="$TAURI_DIR/target/release/bundle/macos"
ENTITLEMENTS="$TAURI_DIR/Entitlements.plist"
INSTALL_DEST="${INSTALL_DEST:-/Applications}"
DO_SKIP_BUILD=false
DO_LAUNCH=false

# ─── Parse args ───────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) DO_SKIP_BUILD=true ;;
        --launch)     DO_LAUNCH=true ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --skip-build   Skip build, reinstall from existing .app bundle"
            echo "  --launch       Launch Handy after install"
            echo "  --help         Show this help message"
            echo ""
            echo "Environment variables:"
            echo "  INSTALL_DEST   Install destination (default: /Applications)"
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

echo "═══════════════════════════════════════════════════════════════"
echo "  Handy Build + Reinstall"
echo "═══════════════════════════════════════════════════════════════"
echo ""

# ─── Step 1: Quit Handy ───────────────────────────────────────────────────────
# Binary is named "handy" (lowercase) inside the bundle, but the app process
# may show as either. Use case-insensitive match to catch both.
if pgrep -xi "$APP_NAME" > /dev/null 2>&1; then
    echo "1/6 🛑 Quitting $APP_NAME..."
    osascript -e "tell application \"$APP_NAME\" to quit" 2>/dev/null || true

    # Wait up to 5s for graceful exit
    for i in {1..10}; do
        if ! pgrep -xi "$APP_NAME" > /dev/null 2>&1; then
            echo "   ✅ Quit gracefully."
            break
        fi
        sleep 0.5
    done

    # Force kill if still running
    if pgrep -xi "$APP_NAME" > /dev/null 2>&1; then
        echo "   ⚠️  Force killing $APP_NAME..."
        pkill -9 -x "$APP_NAME" 2>/dev/null || true
        pkill -9 -x "handy" 2>/dev/null || true
        for i in {1..6}; do
            if ! pgrep -xi "$APP_NAME" > /dev/null 2>&1; then
                echo "   ✅ Terminated after force kill."
                break
            fi
            sleep 0.5
        done
    fi

    # Safety: refuse to proceed if still running
    if pgrep -xi "$APP_NAME" > /dev/null 2>&1; then
        echo "   ❌ $APP_NAME is still running. Aborting."
        exit 1
    fi
else
    echo "1/6 ✅ $APP_NAME not running."
fi

# ─── Step 2: Delete old app ───────────────────────────────────────────────────
DEST_APP="$INSTALL_DEST/$APP_BUNDLE"
if [[ -d "$DEST_APP" ]]; then
    echo "2/6 🗑️  Removing $DEST_APP..."
    rm -rf "$DEST_APP"
else
    echo "2/6 ✅ No existing $DEST_APP to remove."
fi

# ─── Step 3: Build ─────────────────────────────────────────────────────────────
if [[ "$DO_SKIP_BUILD" == true ]]; then
    echo "3/6 ⏩ Skipping build (--skip-build)"
    if [[ ! -d "$BUNDLE_DIR/$APP_BUNDLE" ]]; then
        echo "   ❌ No built .app found at $BUNDLE_DIR/$APP_BUNDLE"
        echo "   Run without --skip-build first."
        exit 1
    fi
else
    echo "3/6 🔨 Building Handy (production)..."
    echo "   This takes 3-10 minutes on incremental builds."
    echo ""

    CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build 2>&1 || {
        echo "   ❌ Tauri build failed."
        echo "   The .app bundle may still exist from a previous build."
        echo "   Check the error output above."
        exit 1
    }

    if [[ ! -d "$BUNDLE_DIR/$APP_BUNDLE" ]]; then
        echo "   ❌ Build succeeded but no .app found at $BUNDLE_DIR/$APP_BUNDLE"
        exit 1
    fi

    echo "   ✅ Build complete."
fi

# ─── Step 4: Create DMG ───────────────────────────────────────────────────────
echo "4/6 📦 Creating DMG..."

# Read version for DMG filename
VERSION=$(grep '"version"' "$TAURI_DIR/tauri.conf.json" | head -1 | sed 's/.*: "//;s/".*//;s/\s*,//')
ARCH=$(uname -m)
DMG_NAME="Handy_${VERSION}_${ARCH}.dmg"
DMG_PATH="$TAURI_DIR/target/release/bundle/$DMG_NAME"

# Stage the DMG contents
DMG_STAGING="/tmp/handy-dmg-staging-$$"
rm -rf "$DMG_STAGING"
mkdir -p "$DMG_STAGING"
cp -R "$BUNDLE_DIR/$APP_BUNDLE" "$DMG_STAGING/"
ln -sf /Applications "$DMG_STAGING/Applications"

# Remove old DMG if exists
rm -f "$DMG_PATH"

hdiutil create -volname "$APP_NAME" -srcfolder "$DMG_STAGING" -ov -format UDZO "$DMG_PATH" 2>&1
rm -rf "$DMG_STAGING"

if [[ ! -f "$DMG_PATH" ]]; then
    echo "   ❌ DMG creation failed."
    exit 1
fi

echo "   ✅ DMG created: $DMG_PATH ($(du -h "$DMG_PATH" | cut -f1))"

# ─── Step 5: Install via Rapidmg ─────────────────────────────────────────────────
echo "5/6 🚀 Opening DMG with Rapidmg for auto-install..."

RAPIDMG_APP="/Applications/Rapidmg.app"
if [[ ! -d "$RAPIDMG_APP" ]]; then
    echo "   ❌ Rapidmg not found at $RAPIDMG_APP"
    echo "   Install from https://rapidmg.app or use Option C (direct copy):"
    echo "     INSTALL_DEST=$INSTALL_DEST ./scripts/build-and-install.sh"
    exit 1
fi

open -a Rapidmg "$DMG_PATH"

# Wait for install to complete.
# IMPORTANT: The .app directory can appear before Rapidmg has finished
# writing all files. If we re-sign while Rapidmg is still copying, the
# signature gets overwritten. We must wait for the main binary to exist
# and its size to stabilize.
DEST_BIN="$DEST_APP/Contents/MacOS/handy"
echo "   Waiting for Rapidmg to install..."
APP_APPEARED=false
for i in {1..30}; do
    if [[ -d "$DEST_APP" ]] && [[ -f "$DEST_BIN" ]]; then
        APP_APPEARED=true
        break
    fi
    sleep 0.5
done

if [[ "$APP_APPEARED" != true ]]; then
    echo "   ⚠️  $DEST_APP not found after 15s — Rapidmg may still be processing."
    echo "   Check Rapidmg manually. The DMG is at: $DMG_PATH"
fi

# Wait for the binary size to stabilize (Rapidmg finishes writing)
if [[ -f "$DEST_BIN" ]]; then
    echo "   Waiting for file copy to complete..."
    PREV_SIZE=0
    STABLE_COUNT=0
    for i in {1..20}; do
        CURR_SIZE=$(stat -f%z "$DEST_BIN" 2>/dev/null || echo 0)
        if [[ "$CURR_SIZE" -eq "$PREV_SIZE" ]] && [[ "$CURR_SIZE" -gt 0 ]]; then
            STABLE_COUNT=$((STABLE_COUNT + 1))
            if [[ $STABLE_COUNT -ge 3 ]]; then
                echo "   ✅ $DEST_APP installed and stable."
                break
            fi
        else
            STABLE_COUNT=0
        fi
        PREV_SIZE=$CURR_SIZE
        sleep 0.5
    done
fi

# ─── Step 6: Re-sign with stable DR ──────────────────────────────────────────────
if [[ -d "$DEST_APP" ]]; then
    echo "6/6 🔐 Re-signing with stable designated requirement..."
    echo "   DR: identifier \"$BUNDLE_ID\""

    if [[ -f "$ENTITLEMENTS" ]]; then
        codesign --force -s - \
            -r="designated => identifier \"$BUNDLE_ID\"" \
            --entitlements "$ENTITLEMENTS" \
            --options runtime \
            "$DEST_APP" 2>&1
    else
        # No entitlements file — sign without it
        codesign --force -s - \
            -r="designated => identifier \"$BUNDLE_ID\"" \
            "$DEST_APP" 2>&1
    fi

    # Verify — must anchor to ^designated => to avoid false positive from
    # the Executable= line which also contains the bundle identifier
    ACTUAL_DR=$(codesign -d -r- "$DEST_APP" 2>&1 | grep "^designated =>" || true)
    echo "   Signature: $ACTUAL_DR"

    if echo "$ACTUAL_DR" | grep -q "^designated => identifier \"$BUNDLE_ID\""; then
        echo "   ✅ Stable DR confirmed — permissions will persist across updates."
    else
        echo "   ❌ DR is not identifier-based (got: $ACTUAL_DR)."
        echo "   This means macOS permissions (Accessibility, etc.) will reset on next build."
        echo "   Retrying re-sign..."
        # Retry: Rapidmg may have overwritten after the first attempt
        sleep 1
        if [[ -f "$ENTITLEMENTS" ]]; then
            codesign --force -s - \
                -r="designated => identifier \"$BUNDLE_ID\"" \
                --entitlements "$ENTITLEMENTS" \
                --options runtime \
                "$DEST_APP" 2>&1
        else
            codesign --force -s - \
                -r="designated => identifier \"$BUNDLE_ID\"" \
                "$DEST_APP" 2>&1
        fi
        ACTUAL_DR=$(codesign -d -r- "$DEST_APP" 2>&1 | grep "^designated =>" || true)
        echo "   Signature (retry): $ACTUAL_DR"
        if echo "$ACTUAL_DR" | grep -q "^designated => identifier \"$BUNDLE_ID\""; then
            echo "   ✅ Stable DR confirmed on retry."
        else
            echo "   ❌ DR still not identifier-based after retry. Permissions may reset."
            echo "   Run manually: scripts/resign-stable-dr.sh"
        fi
    fi
else
    echo "6/6 ⏭️  Skipping re-sign (app not yet installed)."
fi

# ─── Done ─────────────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "  ✅ Build + Reinstall complete!"
echo "═══════════════════════════════════════════════════════════════"
echo ""
echo "  App:  $DEST_APP"
echo "  DMG:  $DMG_PATH"
echo ""

if [[ "$DO_LAUNCH" == true ]]; then
    echo "🚀 Launching Handy..."
    open "$DEST_APP"
else
    echo "  To launch: open \"$DEST_APP\""
fi