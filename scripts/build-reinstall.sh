#!/usr/bin/env bash
#
# scripts/build-reinstall.sh — Full clean reinstall of Handy via Rapidmg
#
# This is the recommended build+deploy workflow for AI agents.
# It builds first, THEN quits Handy, deletes the old app, creates a DMG,
# opens it with Rapidmg for auto-install, re-signs with a stable DR,
# and launches the app automatically.
#
# This ordering ensures the working app remains available if the build fails.
#
# NOTE: This script runs in a non-interactive shell by default, so ~/.zshrc
# won't be loaded. If you get "command not found" errors for bun/cargo,
# either:
#   1. Run with:  zsh -i -c "./scripts/build-reinstall.sh"
#   2. Or add your PATH exports below:
#
# export PATH="$HOME/.bun/bin:$HOME/.cargo/bin:$PATH"
#
# Prerequisites:
#   - Bun (https://bun.sh)
#   - Rust (https://rustup.rs)
#   - Rapidmg installed at /Applications/Rapidmg.app
#
# Usage:
#   ./scripts/build-reinstall.sh              # Full build + reinstall (auto-launches)
#   ./scripts/build-reinstall.sh --skip-build # Reinstall last build only
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

# ─── Parse args ───────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) DO_SKIP_BUILD=true ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --skip-build   Skip build, reinstall from existing .app bundle"
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

# ─── Step 1: Build ────────────────────────────────────────────────────────────
# Build FIRST, before quitting the app, so the working app remains available
# if the build fails.
if [[ "$DO_SKIP_BUILD" == true ]]; then
    echo "1/9 ⏩ Skipping build (--skip-build)"
    if [[ ! -d "$BUNDLE_DIR/$APP_BUNDLE" ]]; then
        echo "   ❌ No built .app found at $BUNDLE_DIR/$APP_BUNDLE"
        echo "   Run without --skip-build first."
        exit 1
    fi
else
    echo "1/9 🔨 Building Handy (production)..."
    echo "   This takes 3-10 minutes on incremental builds."
    echo ""

    CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build 2>&1 || {
        echo "   ❌ Tauri build failed."
        echo "   The working app remains installed and operational."
        echo "   Check the error output above."
        exit 1
    }

    if [[ ! -d "$BUNDLE_DIR/$APP_BUNDLE" ]]; then
        echo "   ❌ Build succeeded but no .app found at $BUNDLE_DIR/$APP_BUNDLE"
        exit 1
    fi

    echo "   ✅ Build complete."
fi

# ─── Step 2: Create DMG ───────────────────────────────────────────────────────
echo "2/9 📦 Creating DMG..."

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

# ─── Step 3: Wait for active use to finish ────────────────────────────────────
# Before quitting, check if Handy is actively in use and wait if needed.
# Uses --is-active-use CLI flag for reliable detection.
#
# "Active use" includes:
# - Recording (user speaking, audio being captured)
# - Processing (transcription, post-processing, router filing)
# - Pronunciation recording (model training)
#
# Always-on mode (mic stream open but NOT recording) does NOT count as active use.
# The script will NOT wait in that case.
DEST_APP="$INSTALL_DEST/$APP_BUNDLE"

if pgrep -xi "$APP_NAME" > /dev/null 2>&1; then
    # Check if Handy is in active use using the CLI flag
    # Exit codes: 0 = active use (recording/transcribing/processing), 1 = idle, 2 = error/unknown flag
    # Use || to capture exit code without triggering set -e exit
    ACTIVE_EXIT=0
    "$DEST_APP/Contents/MacOS/handy" --is-active-use 2>&1 || ACTIVE_EXIT=$?
    
    if [[ $ACTIVE_EXIT -eq 0 ]]; then
        echo "3/9 ⏸️  $APP_NAME is in active use. Waiting for it to finish..."
        echo "   (Active use includes: recording, transcribing, post-processing, router filing)"
        echo "   (Always-on mode does not count as active use)"
        
        # Wait loop: check every 5 seconds until idle
        # (suppress detailed status during loop to reduce noise)
        ACTIVE_CHECK_COUNT=0
        while "$DEST_APP/Contents/MacOS/handy" --is-active-use >/dev/null 2>&1; do
            ACTIVE_CHECK_COUNT=$((ACTIVE_CHECK_COUNT + 1))
            echo "   Still in active use... waiting (attempt $ACTIVE_CHECK_COUNT)"
            sleep 5
        done
        
        echo "   ✅ Active use finished. Proceeding with reinstall."
    elif [[ $ACTIVE_EXIT -eq 2 ]]; then
        # Flag not supported in this version - fall back to --is-recording
        echo "3/9 ⏸️  $APP_NAME: --is-active-use not supported, checking recording state..."
        RECORDING_EXIT=0
        "$DEST_APP/Contents/MacOS/handy" --is-recording 2>&1 || RECORDING_EXIT=$?
        
        if [[ $RECORDING_EXIT -eq 0 ]]; then
            echo "   $APP_NAME has an active recording session. Waiting for it to finish..."
            echo "   (Always-on mode does not block - only active transcriptions do)"
            RECORDING_CHECK_COUNT=0
            while "$DEST_APP/Contents/MacOS/handy" --is-recording >/dev/null 2>&1; do
                RECORDING_CHECK_COUNT=$((RECORDING_CHECK_COUNT + 1))
                echo "   Recording session still active... waiting (attempt $RECORDING_CHECK_COUNT)"
                sleep 5
            done
            echo "   ✅ Recording session finished. Proceeding with reinstall."
        else
            echo "3/9 ✅ $APP_NAME is not recording (legacy check)."
        fi
    else
        echo "3/9 ✅ $APP_NAME is idle (no active use)."
    fi
else
    echo "3/9 ✅ $APP_NAME is not running."
fi

# ─── Step 4: Quit Handy ───────────────────────────────────────────────────────
# Now that the build succeeded and recording (if any) finished, quit the app.
# Binary is named "handy" (lowercase) inside the bundle, but the app process
# may show as either. Use case-insensitive match to catch both.
if pgrep -xi "$APP_NAME" > /dev/null 2>&1; then
    echo "4/9 🛑 Quitting $APP_NAME..."
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
        echo "   The new build is ready at: $BUNDLE_DIR/$APP_BUNDLE"
        echo "   The DMG is at: $DMG_PATH"
        exit 1
    fi
else
    echo "4/9 ✅ $APP_NAME not running."
fi

# ─── Step 5: Delete old app ───────────────────────────────────────────────────
# Only delete AFTER the build completes and app is quit.
if [[ -d "$DEST_APP" ]]; then
    echo "5/9 🗑️  Removing $DEST_APP..."
    rm -rf "$DEST_APP"
else
    echo "5/9 ✅ No existing $DEST_APP to remove."
fi

# ─── Step 6: Install via Rapidmg ─────────────────────────────────────────────
echo "6/9 🚀 Opening DMG with Rapidmg for auto-install..."

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
for i in {1..180}; do
    if [[ -d "$DEST_APP" ]] && [[ -f "$DEST_BIN" ]]; then
        APP_APPEARED=true
        break
    fi
    sleep 0.5
done

if [[ "$APP_APPEARED" != true ]]; then
    echo "   ⚠️  $DEST_APP not found after 90s — Rapidmg may have failed."
    echo "   Falling back to manual DMG install..."
    # ─── Fallback: mount DMG and copy .app manually ───
    DMG_MOUNTED=false
    hdiutil attach "$DMG_PATH" -nobrowse -quiet 2>/dev/null || true
    # Find the mounted volume (volume name is "$APP_NAME" = "Handy")
    VOL_PATH="/Volumes/$APP_NAME"
    if [[ -d "$VOL_PATH" ]] && [[ -d "$VOL_PATH/$APP_BUNDLE" ]]; then
        DMG_MOUNTED=true
        cp -R "$VOL_PATH/$APP_BUNDLE" "$INSTALL_DEST/" 2>&1
        hdiutil detach "$VOL_PATH" -quiet 2>/dev/null || true
        if [[ -d "$DEST_APP" ]] && [[ -f "$DEST_BIN" ]]; then
            APP_APPEARED=true
            echo "   ✅ Manual install succeeded from DMG fallback."
        else
            echo "   ❌ Manual fallback failed — .app not found after copy."
        fi
    else
        echo "   ❌ Could not mount or find $APP_BUNDLE in DMG."
        echo "   The DMG is at: $DMG_PATH"
        # Try to detach if partially mounted
        [[ -d "$VOL_PATH" ]] && hdiutil detach "$VOL_PATH" -quiet 2>/dev/null || true
    fi
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

# ─── Step 7: Re-sign with stable DR ──────────────────────────────────────────
# Re-sign in a verification loop. Even with the three-phase wait above,
# there can be edge cases where a write completes after our stability
# check (e.g. APFS CoW filesystem timing, or Rapidmg finalizing metadata).
# The loop catches this: sign, verify the DR is correct on disk, and
# retry if the signature was overwritten.
if [[ -d "$DEST_APP" ]]; then
    echo "7/9 🔐 Re-signing with stable designated requirement..."
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
    echo "7/9 ⏭️  Skipping re-sign (app not yet installed)."
fi

# ─── Step 8: Reset icon cache ─────────────────────────
# Clear macOS icon cache so the new app icon shows up correctly.
# This fixes the "missing cover image" issue where Finder caches old icons.
echo "8/9 🧹 Resetting icon cache..."
if [[ -d "$DEST_APP" ]]; then
    # Wait for Rapidmg to fully finish writing all files (including icon.icns)
    echo "   Waiting for Rapidmg to fully finish..."
    for i in {1..20}; do
        if [[ -f "$DEST_APP/Contents/Resources/icon.icns" ]]; then
            ICON_SIZE=$(stat -f%z "$DEST_APP/Contents/Resources/icon.icns" 2>/dev/null || echo "0")
            if [[ "$ICON_SIZE" -gt 1000 ]]; then
                break
            fi
        fi
        sleep 0.5
    done

    # Touch the app to update its modification date
    touch "$DEST_APP"

    # Clear user-level icon caches
    rm -rf ~/Library/Caches/com.apple.iconservices* 2>/dev/null || true
    rm -rf ~/Library/Caches/com.apple.IconServices* 2>/dev/null || true

    # Restart services that cache icons
    killall Finder 2>/dev/null || true
    killall Dock 2>/dev/null || true
    killall iconservicesagent 2>/dev/null || true

    echo "   ✅ Icon cache cleared, Finder/Dock restarted."
    echo "   (Icon will rebuild automatically in a few seconds)"
else
    echo "8/9 ⏭️  Skipping cache reset (app not installed)."
fi

# ─── Step 9: Reload launchd agent ───────────────────────────────────
# If a Handy launch agent exists, reload it so launchd picks up the new
# binary path. Without this, launchd caches the old binary path and the
# "RunAtLoad" or "launchctl kickstart" will try to run the stale copy
# from the build directory, causing "Abort trap: 6" crashes.
LAUNCHD_LABEL="Handy"
PLIST_PATH="$HOME/Library/LaunchAgents/${LAUNCHD_LABEL}.plist"
LAUNCHD_ID="gui/$(id -u)/${LAUNCHD_LABEL}"

if [[ -f "$PLIST_PATH" ]]; then
    echo "9/9 🔄 Reloading launchd agent..."
    # Unload the old job (ignore errors if not loaded)
    launchctl bootout "$LAUNCHD_ID" 2>/dev/null || true
    # Reload from the plist
    if launchctl bootstrap "$LAUNCHD_ID" "$PLIST_PATH" 2>/dev/null; then
        echo "   ✅ Launch agent reloaded — Handy will auto-start on login."
    else
        echo "   ⚠️  Bootstrap failed (may already be loaded). Trying kickstart..."
        launchctl kickstart -k "$LAUNCHD_ID" 2>/dev/null || true
        echo "   ✅ Launch agent restarted via kickstart."
    fi
else
    echo "9/9 ⏭️  No launch agent plist found at $PLIST_PATH — skipping."
fi

# ─── Launch Handy ─────────────────────────────────────────────────
echo "🚀 Launching Handy..."
open "$DEST_APP"

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  ✅ Build + Reinstall complete!"
echo "═══════════════════════════════════════════════════════════"
echo ""
echo "  App:  $DEST_APP"
echo "  DMG:  $DMG_PATH"
echo ""