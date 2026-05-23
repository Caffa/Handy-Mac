# Build & Deploy Handy

When the user says "build and deploy" or "build and reinstall", run:

```bash
./scripts/build-reinstall.sh --launch
```

From the project root directory.

## What it does (6 steps)

1. **Quits Handy** — sends AppleScript quit, waits up to 5s, force-kills if needed
2. **Deletes old app** — removes `/Applications/Handy.app`
3. **Builds** — `CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build` (3-10 min on incremental)
4. **Creates DMG** — stages `.app` + Applications symlink, runs `hdiutil create`
5. **Installs via Rapidmg** — opens DMG with `/Applications/Rapidmg.app`, then uses a three-phase wait:
   - Phase 1: Wait for `.app` bundle to appear on disk
   - Phase 2: Wait for DMG volume (`/Volumes/Handy`) to be ejected — this signals Rapidmg is done writing
   - Phase 3: Wait for entire bundle (file count + total size) to stabilize for 5 consecutive checks
6. **Re-signs with stable DR** — verification loop: `codesign` then verify the DR is `identifier "com.pais.handy"`. Retries up to 5 times if Rapidmg overwrote the signature.

The `--launch` flag opens the app after install completes.

## Known issue: Rapidmg race condition

Rapidmg writes files asynchronously. The old script only checked the main binary size, which could stabilize before Frameworks/resources were done copying — causing the re-sign to be overwritten. The fix uses:

1. DMG volume ejection detection (strongest signal Rapidmg is done)
2. Full bundle stability (file count + total size, not just binary size)
3. Re-sign verification loop (up to 5 retries with 2s backoff)

## Flags

- `--launch` — open Handy after install
- `--skip-build` — skip the build step, reinstall from existing `.app` bundle
- `--help` — usage info

## Environment variables

- `INSTALL_DEST` — install destination (default: `/Applications`)

## Key paths

- Built `.app`: `src-tauri/target/release/bundle/macos/Handy.app`
- DMG: `src-tauri/target/release/bundle/Handy_<version>_<arch>.dmg`
- Entitlements: `src-tauri/Entitlements.plist`
- Bundle ID: `com.pais.handy`

## Prerequisites

- Bun
- Rust (rustup)
- Rapidmg installed at `/Applications/Rapidmg.app`
