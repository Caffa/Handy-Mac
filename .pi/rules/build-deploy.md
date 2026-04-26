---
name: build-deploy
description: "Build and deploy Handy to /Applications (or ~/Applications)"
---

# Build & Deploy Rules

## ⚠️ Critical Warning

`bun run tauri build` compiles a **production Rust build** with `lto = true`
and `codegen-units = 1` (see `src-tauri/Cargo.toml`). This is **very slow** —
expect **5-30+ minutes** on first run. Do NOT trigger on every commit.

## Option A: Local Update (In-App Update — Recommended)

Instead of replacing the .app bundle manually, use Handy's built-in
Tauri updater pointing at a local server. This builds, signs, and serves your
fork — then just click "Check for Updates" in the app.

**One-time setup (generate signing keypair):**

```bash
mkdir -p ~/.tauri
# Use a simple password — the script defaults to "handy"
echo "handy" | bunx tauri signer generate -w ~/.tauri/handy-fork.key
# The pubkey is already configured in tauri.conf.json for this fork
```

**Deploy via local update:**

```bash
# Build + serve — then click "Check for Updates" in the app
./scripts/local-update.sh

# Build + bump version so the updater sees it as new (recommended)
# (or use [reinstall] in commit message to trigger via post-commit hook)
./scripts/local-update.sh --bump

# Bump minor version
./scripts/local-update.sh --bump minor

# Re-serve last build without rebuilding
./scripts/local-update.sh --skip-build
```

The script:
1. Patches `tauri.conf.json` → endpoints point to `http://localhost:4321/latest.json`
2. Signs the build with your fork key
3. Generates `latest.json` with signature + version
4. Serves the update over HTTP
5. Restores config on exit

The app downloads the update in-place and relaunches — no .app swap needed.

## Option B: Standalone Script (Alternative)

For manual control (no commit required):

```bash
# Default to ~/Applications
./scripts/build-and-install.sh

# Or deploy to /Applications (may prompt for sudo)
INSTALL_DEST=/Applications ./scripts/build-and-install.sh
```

## Option C: Post-Commit Hook (Alternative)

The `.git/hooks/post-commit` hook does **nothing by default**. It only builds
when you explicitly opt in.

### How to trigger deployment

```bash
# Via environment variable
REINSTALL=1 git commit -m "feat(audio): add noise gate"

# Via commit message tag (for manual .app copy)
# (include [reinstall] anywhere in the message)
git commit -m "feat(audio): add noise gate [reinstall]"
```

### What the hook does

1. **Checks if deployment is requested** (`REINSTALL=1` or `[reinstall]` in message)
2. **Builds** via `bun run tauri build`
3. **Quits** running Handy gracefully (with AppleScript → fallback `pkill`)
4. **Cleans** old bundle: `rm -rf ~/Applications/Handy.app`
5. **Installs** new bundle: `cp -R .../Handy.app ~/Applications/`
6. **Prints** `open "~/Applications/Handy.app"` to restart

### Skipping deployment

Any normal commit (no `[reinstall]`, no `REINSTALL=1`) will skip silently:

```bash
git commit -m "wip: quick save"
```

A one-line hint is printed on interactive terminals.

### Disabling the hook entirely

```bash
chmod -x .git/hooks/post-commit
```

### Re-enabling

```bash
chmod +x .git/hooks/post-commit
```

## Local Deploy Target

| Destination | Pros | Cons |
|-------------|------|------|
| `~/Applications` | No sudo needed, user-owned | Not visible to other users |
| `/Applications` | System-wide | Usually requires sudo |

**Default: `~/Applications`**. Override via `INSTALL_DEST=/Applications`.

## Build Failure Recovery

If `bun run tauri build` fails:

```bash
# Check Rust / frontend errors in the output
# Common fix: ensure model is downloaded
curl -o src-tauri/resources/models/silero_vad_v4.onnx \
  https://blob.handy.computer/silero_vad_v4.onnx

# Re-try build
bun run tauri build
```

## CI vs Local

| Context | Where to build |
|---------|---------------|
| Local dev | `bun run tauri build` (slow) or `bun run tauri dev` (fast, hot reload) |
| CI/CD | Use `.github/workflows/build.yml` (already configured) |
| PRs | `bun run lint`, `bun run format:check`, `cargo clippy` |

## Safety Notes

- The hook (Option C) runs `set -euo pipefail` — any error aborts the deploy.
- Old bundle is **removed before copying** to avoid macOS cache corruption.
- Force-kill is a last resort; graceful quit is attempted first.

## macOS Permission Persistence (Critical)

When building with `signingIdentity: "-"` (ad-hoc signing), the app gets a
**cdhash-based designated requirement** that changes on every build:

```
# Before re-signing (ad-hoc):
designated => cdhash H"4272a9dd7cd73ae1596f0d8f6864987d3e86147c"
# ^ changes on EVERY build — macOS resets Accessibility, Microphone, etc.
```

macOS TCC (Transparency, Consent, and Control) ties permissions to the
designated requirement. A changed cdhash means macOS treats the updated
app as a completely different program, **resetting all granted permissions**.

The upstream Handy app avoids this because it uses a proper Apple Developer ID
certificate, which produces a stable DR like:
```
identifier "com.pais.handy" and anchor apple generic and certificate leaf[...]
```

**Fix:** The `local-update.sh` script automatically re-signs the .app with
a stable identifier-based DR after building:

```
# After re-signing:
designated => identifier "com.pais.handy"
# ^ stable across all builds — permissions persist
```

The standalone script `scripts/resign-stable-dr.sh` can also be run manually
after any build to fix the signature.

**To verify your build has the stable DR:**

```bash
codesign -d -r- src-tauri/target/release/bundle/macos/Handy.app
# Should show: designated => identifier "com.pais.handy"
# NOT: designated => cdhash H"..."
```

**Important:** If you skip the re-sign step, users will lose Accessibility and
Microphone permissions on every update.

## Known Caveats

### `git rebase` / `git commit --amend`

`post-commit` hooks fire during rebases and amends too. If a replayed commit
contains `[reinstall]`, the build will trigger again:

```bash
# During rebase of a [reinstall] commit:
git rebase -i main
# → you may see a long build mid-rebase
```

**Workaround:** Skip `[reinstall]` in commits you plan to rebase, or export
`REINSTALL=0` before rebasing:

```bash
REINSTALL=0 git rebase -i main
```

### Uncommitted changes during deploy

If you type `REINSTALL=1 git commit`, the build is clean against committed code.
But if you stash + deploy:

```bash
git stash push -m "deploy-stash"
REINSTALL=1 git commit --allow-empty -m "chore: deploy build [reinstall]"
git stash pop
```