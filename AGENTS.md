# zdx workspace development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is the workspace index.

## AGENTS.md index

This monorepo now uses scoped `AGENTS.md` files per crate.

- `AGENTS.md` (this file): workspace-level conventions + index
- `crates/zdx-core/AGENTS.md`: core engine/providers/tools map + core-specific conventions
- `crates/zdx-tui/AGENTS.md`: TUI architecture map + runtime/features conventions
- `crates/zdx-cli/AGENTS.md`: CLI routing/modes/commands map + CLI testing guidance
- `crates/zdx-bot/AGENTS.md`: Telegram bot flow map + bot-specific conventions
- `crates/xtask/AGENTS.md`: maintainer task crate guidance

### Scope and precedence

- `AGENTS.md` applies to its directory tree.
- For each changed file, follow every in-scope `AGENTS.md`.
- When rules conflict: deeper `AGENTS.md` wins; system/developer/user instructions win over all `AGENTS.md`.
- Scope-specific style rules stay within scope unless explicitly stated otherwise.
- If an in-scope `AGENTS.md` requires checks, run them after changes and make a best effort to pass.

## Workspace layout

- `docs/SPEC.md`: behavior contracts
- `docs/ARCHITECTURE.md`: architecture and data flow
- `docs/plans/`: commit-sized implementation plans
- `tools/scripts/`: optional repo scripts
- `.github/workflows/`: CI/release workflows
- `.cargo/config.toml`: cargo aliases/shared target dir config
- `justfile`: common development tasks

## Build / run

All common tasks are available via `just` (see `justfile`). Run `just` to list all recipes.

- `just run` (interactive TUI; pass extra args: `just run --help`)
- `just bot` (Telegram bot; requires config telegram.\* keys)
- `just bot-loop` (Telegram bot with auto-restart on exit code 42)
- `just automations` (automation subcommands; e.g. `just automations list`)
- `just ci` (full local CI: lint + test)
- `just lint` (format + clippy)
- `just fmt` (nightly rustfmt)
- `just clippy` (lint only)
- `just test` (fast path; skips doc tests)
- `just update-defaults` (maintainer: refresh both default_models.toml + default_config.toml)
- `just update-models` (maintainer: refresh default_models.toml)
- `just update-config` (maintainer: refresh default_config.toml)
- `just codebase` (generate codebase.txt for entire workspace)
- `just codebase crates/zdx-tui` (generate codebase.txt for specific crate)
- `just build-release` (build release binary)
- Release automation: `.github/workflows/release-please.yml` (config in `release-please-config.json`)

## Conventions

- Rust edition: 2024 (see `Cargo.toml`)
- Formatting: rustfmt defaults
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries

## Tests (keep it light)

- Add tests only to protect a user-visible contract or a real regression.
- Prefer integration tests in `crates/zdx-cli/tests/` over unit tests for CLI/output/persistence behavior.
- Avoid mutating process-global env vars in-process; set env on spawned CLI commands instead.

## Docs

- `docs/SPEC.md`: contracts (what/behavior)
- `docs/ARCHITECTURE.md`: system architecture and design (Elm/MVU patterns, key patterns)
- `docs/plans/`: optional commit-sized plans (how)

## ⚠️ IMPORTANT: Keep this file up to date

**This is mandatory, not optional.** When you:

- **Add/move/delete a crate-level file** → Update the corresponding `crates/*/AGENTS.md`
- **Add/remove/rename a crate AGENTS file** → Update the "AGENTS.md index" section here
- **Change build/run/test workflows** → Update "Build / run" here
- **Add workspace-wide conventions** → Document here (or in the relevant scoped crate file)
- **Change system architecture** → Update `docs/ARCHITECTURE.md` (module relationships, data flow, component boundaries, or design patterns)
