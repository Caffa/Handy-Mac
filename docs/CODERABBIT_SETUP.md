# CodeRabbit Setup Guide

Status: Active

## What is CodeRabbit?

[CodeRabbit](https://coderabbit.ai) is an AI-powered code review bot for GitHub pull requests. It automatically reviews PRs, provides line-by-line feedback, catches bugs, suggests improvements, and summarizes changes — all without manual configuration.

Key features for Handy:

- Reviews both **Rust** and **TypeScript/React** code
- Learns codebase patterns over time
- Catches issues CI can't (logic errors, missing edge cases, inconsistent patterns)
- Free for open-source/public repositories

## Setup

CodeRabbit is free for public repositories. Setting it up takes under 2 minutes:

1. **Go to** [https://coderabbit.ai](https://coderabbit.ai)
2. **Sign in with GitHub** (authorizes the CodeRabbit GitHub App)
3. **Install the CodeRabbit GitHub App** on the repository (or organization)
   - During installation, select which repos to enable (or "All repositories")
4. **Done** — CodeRabbit automatically reviews every new pull request

No config file required. CodeRabbit starts reviewing PRs immediately.

## Optional: Configuration File

For finer control, create `.coderabbit.yaml` in the repository root:

```yaml
# .coderabbit.yaml
language: en-US
enable_free_tier: true

reviews:
  profile: assertive          # chill, balanced, or assertive
  request_changes_workflow: false
  high_level_summary: true
  poem: false
  review_status: true
  collapse_walkthrough: false

  auto_review:
    enabled: true
    drafts: false

  path_instructions:
    - path: src-tauri/src/**/*.rs
      instructions: |
        Focus on unsafe code, error handling with ? and .map_err(),
        thread safety, and Tauri command correctness.
        The project uses Rust with Tauri 2.x patterns.
    - path: src/**/*.{ts,tsx}
      instructions: |
        All user-facing strings must use i18next (t() function).
        The project uses React 18 + TypeScript + Tailwind CSS 4 + Zustand.
        Check for untranslated literal strings in JSX.
    - path: src/i18n/**
      instructions: |
        Translation files — check for missing keys vs English source,
        and consistent interpolation syntax.

tools:
  biome:
    enabled: true
  eslint:
    enabled: true
```

## What CodeRabbit Reviews

| Area | Details |
|------|---------|
| Rust (`src-tauri/`) | Unsafe blocks, error propagation, Tauri command patterns, thread safety |
| TypeScript/React (`src/`) | Component patterns, hook usage, type safety, i18next compliance |
| CI workflows (`.github/`) | YAML correctness, action versions, dependency caching |
| Config files | `biome.json`, `eslint.config.js`, `Cargo.toml`, `package.json` |

## How It Complements CI

| Tool | Catches | Runs on |
|------|---------|---------|
| ESLint | Missing i18next translations | CI (code-quality.yml) |
| Biome | Formatting, unused imports, suspicious code | CI (code-quality.yml + ci.yml) |
| Prettier | Formatting consistency | CI (code-quality.yml) |
| Clippy | Rust code smells | CI (ci.yml) |
| rustfmt | Rust formatting | CI (ci.yml) |
| CodeRabbit | Logic errors, missing edge cases, pattern inconsistencies | Every PR (GitHub App) |

## Troubleshooting

- **CodeRabbit not reviewing PRs**: Check that the GitHub App is installed on the repo (Settings → Integrations → Applications).
- **Too many suggestions**: Set `profile: chill` in `.coderabbit.yaml`.
- **Want to skip review on a PR**: Add `@coderabbitai ignore` in the PR description.