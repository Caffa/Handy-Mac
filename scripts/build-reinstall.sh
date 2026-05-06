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

# ─── Step 5: Install via Rapidmg ─────────────────────────────────────────────
echo "5/6 🚀 Opening DMG with Rapidmg for auto-install..."

RAPIDMG_APP="/Applications/Rapidmg.app"
if [[ ! -d "$RAPIDMG_APP" ]]; then
    echo "   ❌ Rapidmg not found at $RAPIDMG_APP"
    echo "   Install from https://rapidmg.app or use Option C (direct copy):"
    echo "     INSTALL_DEST=$INSTALL_DEST ./scripts/build-and-install.sh"
    exit 1
fi

open -a Rapidmg "$DMG_PATH"

# Wait for install to complete using a three-phase approach.
#
# The .app directory can appear on disk before Rapidmg has finished writing
# all files. If we re-sign while Rapidmg is still copying, the signature
# gets overwritten — this is the Rapidmg race condition. The three phases
# below eliminate it:
#
#   Phase 1: Wait for the .app bundle to appear on disk.
#   Phase 2: Wait for the DMG volume to be ejected — Rapidmg auto-ejects
#            after install, which is the strongest signal that all writes
#            are complete.
#   Phase 3: Wait for the entire bundle (not just the main binary) to
#            stabilize — file count and total size must be steady for 5
#            consecutive checks. This catches lingering writes even if
#            the DMG eject signal was missed (e.g. manual DMG mount).
#
DEST_BIN="$DEST_APP/Contents/MacOS/handy"
DMG_VOLUME="/Volumes/$APP_NAME"

# Phase 1: Wait for app to appear
echo "   Waiting for Rapidmg to install..."
APP_APPEARED=false
for i in {1..60}; do
    if [[ -d "$DEST_APP" ]] && [[ -f "$DEST_BIN" ]]; then
        APP_APPEARED=true
        break
    fi
    sleep 0.5
done

if [[ "$APP_APPEARED" != true ]]; then
    echo "   ⚠️  $DEST_APP not found after 30s — Rapidmg may still be processing."
    echo "   Check Rapidmg manually. The DMG is at: $DMG_PATH"
fi

# Phase 2: Wait for the DMG volume to be ejected.
# Rapidmg auto-ejects the DMG after finishing the install — this is the
# strongest signal that all file writes are complete.
if [[ -d "$DMG_VOLUME" ]]; then
    echo "   Waiting for DMG volume to be ejected (Rapidmg finishing)..."
    for i in {1..60}; do
        if [[ ! -d "$DMG_VOLUME" ]]; then
            echo "   ✅ DMG ejected — Rapidmg install complete."
            break
        fi
        sleep 0.5
    done
    if [[ -d "$DMG_VOLUME" ]]; then
        echo "   ⚠️  DMG still mounted after 30s — falling through to stability check."
    fi
fi

# Phase 3: Wait for entire bundle to stabilize.
# Rapidmg may write Frameworks and resources after the main binary is done,
# so checking only the binary size is insufficient. We track file count and
# total size recursively across the whole .app bundle.
if [[ -d "$DEST_APP" ]]; then
    echo "   Waiting for bundle to stabilize..."
    PREV_INFO=""
    STABLE_COUNT=0
    for i in {1..40}; do
        FILE_COUNT=$(find "$DEST_APP" -type f 2>/dev/null | wc -l | tr -d ' ') || FILE_COUNT=0
        TOTAL_SIZE=$(du -sk "$DEST_APP" 2>/dev/null | cut -f1) || TOTAL_SIZE=0
        CURR_INFO="${FILE_COUNT}:${TOTAL_SIZE}"
        if [[ "$CURR_INFO" == "$PREV_INFO" ]] && [[ "$FILE_COUNT" -gt 0 ]]; then
            STABLE_COUNT=$((STABLE_COUNT + 1))
            if [[ $STABLE_COUNT -ge 5 ]]; then
                echo "   ✅ Bundle stable (${FILE_COUNT} files, ${TOTAL_SIZE}KB)."
                break
            fi
        else
            STABLE_COUNT=0
        fi
        PREV_INFO="$CURR_INFO"
        sleep 0.5
    done
fi

# ─── Step 6: Re-sign with stable DR ──────────────────────────────────────────
# Re-sign in a verification loop. Even with the three-phase wait above,
# there can be edge cases where a write completes after our stability
# check (e.g. APFS CoW filesystem timing, or Rapidmg finalizing metadata).
# The loop catches this: sign, verify the DR is correct on disk, and
# retry if the signature was overwritten.
if [[ -d "$DEST_APP" ]]; then
    echo "6/6 🔐 Re-signing with stable designated requirement..."
    echo "   DR: identifier \"$BUNDLE_ID\""

    MAX_SIGN_ATTEMPTS=5
    SIGN_CONFIRMED=false

    for ATTEMPT in $(seq 1 $MAX_SIGN_ATTEMPTS); do
        if [[ $ATTEMPT -gt 1 ]]; then
            echo "   Retry $((ATTEMPT-1))/$((MAX_SIGN_ATTEMPTS-1))..."
            # Wait a beat before retrying — gives any lingering
            # writes time to settle
            sleep 2
        fi

        # Use || true to prevent `set -e` from aborting the script on
        # non-zero exit — we rely on the verification step below to determine
        # whether signing actually succeeded.
        if [[ -f "$ENTITLEMENTS" ]]; then
            codesign --force -s - \
                -r="designated => identifier \"$BUNDLE_ID\"" \
                --entitlements "$ENTITLEMENTS" \
                --options runtime \
                "$DEST_APP" 2>&1 || true
        else
            # No entitlements file — sign without it
            codesign --force -s - \
                -r="designated => identifier \"$BUNDLE_ID\"" \
                "$DEST_APP" 2>&1 || true
        fi

        # Verify — must anchor to ^designated => to avoid false positive from
        # the Executable= line which also contains the bundle identifier.
        # A line starting with "# designated =>" is the derived (computed) DR;
        # we need the explicit one (without #) to confirm our identifier-based
        # requirement was written to disk.
        ACTUAL_DR=$(codesign -d -r- "$DEST_APP" 2>&1 | grep "^designated =>" || true)

        if echo "$ACTUAL_DR" | grep -q "^designated => identifier \"$BUNDLE_ID\""; then
            echo "   ✅ Stable DR confirmed — permissions will persist across updates."
            SIGN_CONFIRMED=true
            break
        fi
    done

    if [[ "$SIGN_CONFIRMED" != true ]]; then
        echo "   ❌ DR is not identifier-based after $MAX_SIGN_ATTEMPTS attempts (got: $ACTUAL_DR)."
        echo "   This means macOS permissions (Accessibility, etc.) may reset on next build."
        echo "   Run manually: scripts/resign-stable-dr.sh"
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