# Commit Rules for Handy

## Commit Format

Follow **Conventional Commits**:

```
<type>(<scope>): <imperative summary>
```

### Types

| Type | Use for |
|------|---------|
| `feat` | New user-facing functionality |
| `fix` | Bug fixes (top priority during feature freeze) |
| `refactor` | Code restructuring with no behavior change |
| `perf` | Performance improvements |
| `test` | Adding or updating tests |
| `docs` | Documentation updates |
| `chore` | Build, deps, tooling |

### Scopes

| Scope | Affected area |
|-------|-------------|
| `frontend` | React/TS components, hooks, stores |
| `rust` | All Rust backend code |
| `audio` | Audio recording, VAD, playback |
| `transcription` | Whisper/ONNX inference pipeline |
| `settings` | Settings UI or persistence |
| `overlay` | Recording overlay window |
| `bindings` | Tauri command bindings (`src/bindings.ts`) |
| `i18n` | Translations or i18n setup |
| `ci` | GitHub Actions workflows |
| `deps` | Dependency updates |

### Examples

```
feat(audio): add noise gate threshold slider
fix(rust): prevent crash when mic permission denied
refactor(frontend): extract useRecording hook
chore(deps): bump tauri to 2.10.2
```

## Feature Freeze Awareness

**Bug fixes are the top priority.** New features will be rejected unless there is explicit community support (see `CONTRIBUTING.md`). When in doubt, choose `fix` over `feat`.

## Triggering Auto-Build

To build and install the Handy app to Applications after the commit:

```bash
# Option 1: Environment variable
DEPLOY=1 git commit -m "feat(audio): add noise gate"

# Option 2: Commit message tag
git commit -m "feat(audio): add noise gate [deploy]"
```

See `build-deploy.md` for full details.

## Rules

- Summary must be imperative: `add`, `fix`, `refactor` — not `added`, `fixes`, `refactored`
- No trailing period in summary
- Keep subject line under 72 characters
- Body is optional; use it for complex changes to explain *why* (not *what*)
- Run `bun run lint:fix` and `bun run format` **before** committing
