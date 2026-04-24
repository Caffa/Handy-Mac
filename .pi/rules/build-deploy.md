---
name: build-deploy
description: "Build and deploy Handy to /Applications (or ~/Applications)"
---

# Build & Deploy Rules

## ⚠️ Critical Warning

`bun run tauri build` compiles a **production Rust build** with `lto = true`
and `codegen-units = 1` (see `src-tauri/Cargo.toml`). This is **very slow** —
expect **5-30+ minutes** on first run. Do NOT trigger on every commit.

## Local Deploy Target

| Destination | Pros | Cons |
|-------------|------|------|
| `~/Applications` | No sudo needed, user-owned | Not visible to other users |
| `/Applications` | System-wide | Usually requires sudo |

**Default: `~/Applications`**. Override via `INSTALL_DEST=/Applications`.

## Option A: Post-Commit Hook (Opt-In Per Commit)

The `.git/hooks/post-commit` hook does **nothing by default**. It only builds
when you explicitly opt in.

### How to trigger deployment

```bash
# Via environment variable
DEPLOY=1 git commit -m "feat(audio): add noise gate"

# Via commit message tag
# (include [deploy] anywhere in the message)
git commit -m "feat(audio): add noise gate [deploy]"
```

### What the hook does

1. **Checks if deployment is requested** (`DEPLOY=1` or `[deploy]` in message)
2. **Builds** via `bun run tauri build`
3. **Quits** running Handy gracefully (with AppleScript → fallback `pkill`)
4. **Cleans** old bundle: `rm -rf ~/Applications/Handy.app`
5. **Installs** new bundle: `cp -R .../Handy.app ~/Applications/`
6. **Prints** `open "~/Applications/Handy.app"` to restart

### Skipping deployment

Any normal commit (no `[deploy]`, no `DEPLOY=1`) will skip silently:

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

## Option B: Standalone Script

For manual control (no commit required):

```bash
# Default to ~/Applications
./scripts/build-and-install.sh

# Or deploy to /Applications (may prompt for sudo)
INSTALL_DEST=/Applications ./scripts/build-and-install.sh
```

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

- The hook runs `set -euo pipefail` — any error aborts the deploy.
- Old bundle is **removed before copying** to avoid macOS cache corruption.
- Force-kill is a last resort; graceful quit is attempted first.

## Known Caveats

### `git rebase` / `git commit --amend`

`post-commit` hooks fire during rebases and amends too. If a replayed commit
contains `[deploy]`, the build will trigger again:

```bash
# During rebase of a [deploy] commit:
git rebase -i main
# → you may see a long build mid-rebase
```

**Workaround:** Skip `[deploy]` in commits you plan to rebase, or export
`DEPLOY=0` before rebasing:

```bash
DEPLOY=0 git rebase -i main
```

### Uncommitted changes during deploy

If you type `DEPLOY=1 git commit`, the build is clean against committed code.
But if you stash + deploy:

```bash
git stash push -m "deploy-stash"
DEPLOY=1 git commit --allow-empty -m "chore: deploy build [deploy]"
git stash pop
```
