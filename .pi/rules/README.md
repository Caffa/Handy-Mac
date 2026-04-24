# Handy Development Rules

These are project-specific rules for developing the Handy speech-to-text application.

## Rule Files

| Priority | File | When to Read |
|----------|------|--------------|
| **HIGH** | `handy-dev.md` | Any code change — backend (Rust) or frontend (TS/React) |
| **HIGH** | `build-deploy.md` | Building, bundling, or installing the app locally |
| **MEDIUM** | `commit.md` | Before committing changes |

## Quick Reference

- **Stack**: Tauri 2.x (Rust backend + Vite/React/TypeScript frontend)
- **Package Manager**: Bun (not npm)
- **Dev Command**: `bun run tauri dev`
- **Build Command**: `bun run tauri build`
- **Lint**: `bun run lint`, `bun run format`
- **Model Setup**: `curl -o src-tauri/resources/models/silero_vad_v4.onnx https://blob.handy.computer/silero_vad_v4.onnx`
- **macOS Build Fix**: `CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev`
