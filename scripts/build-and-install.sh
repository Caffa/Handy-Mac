#!/usr/bin/env bash
#
# scripts/build-and-install.sh — Build Handy and install to Applications
#
# Usage:
#   ./scripts/build-and-install.sh
#
# Options:
#   INSTALL_DEST=/Applications ./scripts/build-and-install.sh
#
# Defaults to ~/Applications (avoids sudo).

set -euo pipefail

APP_NAME="Handy"
APP_BUNDLE="${APP_NAME}.app"

# Default to user-local Applications to avoid permission issues
INSTALL_DEST="${INSTALL_DEST:-$HOME/Applications}"
mkdir -p "$INSTALL_DEST"

echo "[build-and-install] Building Handy for macOS..."
echo "[build-and-install] Install destination: $INSTALL_DEST"

if ! bun run tauri build; then
  echo "[build-and-install] ❌ Tauri build failed."
  exit 1
fi

BUNDLE_SRC="src-tauri/target/release/bundle/macos/$APP_BUNDLE"

if [[ ! -d "$BUNDLE_SRC" ]]; then
  echo "[build-and-install] ❌ Expected bundle not found: $BUNDLE_SRC"
  exit 1
fi

DEST_APP="$INSTALL_DEST/$APP_BUNDLE"

# Quit running instance (if any)
if pgrep -x "$APP_NAME" > /dev/null 2>&1; then
  echo "[build-and-install] Quitting running $APP_NAME..."
  osascript -e "tell application \"$APP_NAME\" to quit" 2>/dev/null || true

  # Wait up to 5s for graceful exit
  for i in {1..10}; do
    if ! pgrep -x "$APP_NAME" > /dev/null 2>&1; then
      echo "[build-and-install] ✅ Quit gracefully."
      break
    fi
    sleep 0.5
  done

  # Force kill if still running (SIGKILL)
  if pgrep -x "$APP_NAME" > /dev/null 2>&1; then
    echo "[build-and-install] ⚠️  Force killing $APP_NAME..."
    pkill -9 -x "$APP_NAME" || true
    for i in {1..6}; do
      if ! pgrep -x "$APP_NAME" > /dev/null 2>&1; then
        echo "[build-and-install] ✅ Terminated after force kill."
        break
      fi
      sleep 0.5
    done
  fi
fi

# Safety gate: refuse to rm while app is still running
if pgrep -x "$APP_NAME" > /dev/null 2>&1; then
  echo "[build-and-install] ❌ $APP_NAME is still running. Aborting to avoid corrupting the bundle."
  echo "[build-and-install] 💡 Kill the process manually, then re-run."
  exit 1
fi

# Install
if [[ -d "$DEST_APP" ]]; then
  echo "[build-and-install] Removing old bundle..."
  rm -rf "$DEST_APP"
fi

echo "[build-and-install] Installing new bundle..."
cp -R "$BUNDLE_SRC" "$INSTALL_DEST/"

echo "[build-and-install] ✅ Installed to $DEST_APP"
echo "[build-and-install] ▶️  To start: open \"$DEST_APP\""
