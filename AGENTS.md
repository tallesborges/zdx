# zdx workspace development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is the workspace index.

## AGENTS.md index

This monorepo now uses scoped `AGENTS.md` files per crate.

- `AGENTS.md` (this file): workspace-level conventions + index
- `crates/zdx-assets/AGENTS.md`: embedded assets (prompts, default TOMLs, bundled skills, built-in subagents)
- `crates/zdx-types/AGENTS.md`: pure shared value types and pure helper logic for providers, tools, events
- `crates/zdx-providers/AGENTS.md`: LLM provider implementations (Anthropic, OpenAI, Gemini, etc.)
- `crates/zdx-engine/AGENTS.md`: core engine — runtime, config, agent orchestration, tools
- `crates/zdx-tools/AGENTS.md`: leaf tool implementations (bash, edit, read, write, glob, grep, web_search, fetch_webpage, apply_patch)
- `crates/zdx-cli/AGENTS.md`: CLI routing/modes/commands map + CLI testing guidance
- `crates/xtask/AGENTS.md`: maintainer task crate guidance
- `apps/web/AGENTS.md`: Svelte Mini App frontend (design system + views, consumes the gateway's Mini App HTTP API)
- `apps/tui/AGENTS.md`: Go chat TUI `zdx-go tui` (every turn runs in the orchestrator service over its socket)
- `apps/monitor/AGENTS.md`: Go monitor `zdx-go monitor`: the read-only dashboard `zdx monitor` runs (reads the orchestrator socket views and read-only CLI JSON)
- `apps/gateway/AGENTS.md`: Go gateway `zdx-go gateway`: the Telegram bot and Mini App server (talks to Rust only through the orchestrator socket and CLI)
- `apps/shared/AGENTS.md`: shared Go packages the gateway, the monitor, and the chat TUI import (socket transport, wire types, transcript layout, quota, jobs, Git views, version), and the `zdx-go` commands `quota`, `jobs`, and `git` that `zdx quota`, `zdx jobs` (all but `history`), and `zdx git` exec

### Scope and precedence

- `AGENTS.md` applies to its directory tree.
- For each changed file, follow every in-scope `AGENTS.md`.
- When rules conflict: deeper `AGENTS.md` wins; system/developer/user instructions win over all `AGENTS.md`.
- Scope-specific style rules stay within scope unless explicitly stated otherwise.
- If an in-scope `AGENTS.md` requires checks, run them after changes and make a best effort to pass.

## Workspace layout

- `docs/SPEC.md`: behavior contracts
- `docs/ARCHITECTURE.md`: architecture and data flow
- `docs/plans/`: commit-sized implementation plans, staged by lifecycle: `drafts/` (exploratory) → `active/` (building) → `done/` + `archived/`
- `apps/go.mod`, `apps/cmd/zdx-go/`, `apps/build.sh`: the one Go module and its one binary `zdx-go`, with a command per Go program (`gateway`, `monitor`, `tui`, `quota`, `jobs`, `git`). Rust `zdx` stays the command people type and execs `zdx-go <command> --root ROOT` (`crates/zdx-cli/src/cli/commands/go_program.rs`); the bot service runs `zdx-go gateway` directly. `apps/build.sh` builds it into `apps/bin/zdx-go` (gitignored) with the Mini App embedded (needs `bun`), or without it (`--no-web`)
- `apps/web/`: Svelte Mini App frontend, embedded into `zdx-go` by `apps/build.sh` (see `apps/web/AGENTS.md`)
- `apps/gateway/`: Go Telegram + Mini App gateway (see `apps/gateway/AGENTS.md`)
- `apps/monitor/`, `apps/tui/`: Go monitor and chat TUI (see their `AGENTS.md`)
- `apps/shared/`: shared Go packages and the `quota`, `jobs`, and `git` commands (see `apps/shared/AGENTS.md`)
- `.github/workflows/`: CI/release workflows
- `.cargo/config.toml`: cargo aliases, lld linker, and incremental builds for workspace crates (overrides a global `incremental = false`; works with a global sccache wrapper, which still caches dependencies)
- `justfile`: common development tasks

## Build / run

All common tasks are available via `just` (see `justfile`). Run `just` to list all recipes.

- `just run` (builds `apps/bin/zdx-go` without the Mini App, then `zdx` from this checkout execs its chat TUI; pass extra args: `just run --help`). Bare `zdx` on a terminal runs `zdx-go tui`. Every Go command `zdx` starts uses `$ZDX_GO` (absolute) or the installed `~/.local/bin/zdx-go`, and fails when neither exists; piped stdin runs `exec`. The TUI needs a running orchestrator service built with `chat_submit` and `subscribe_thread`
- `just tui [ARGS]` (runs `apps/bin/zdx-go tui` directly, without `zdx`)
- `just monitor [-- ARGS]` / `just quota [ARGS]` / `just git [ARGS]` / `just jobs [ARGS]` (build `apps/bin/zdx-go` without the Mini App, then run that `zdx` command from this checkout)
- `just bot [-- ARGS]` (Telegram bot: builds `zdx-go` with the Mini App and `zdx` from this checkout, then `zdx bot` execs `zdx-go gateway` on the global `[telegram]` config. The gateway's `bot.pid` refuses a second bot on one `ZDX_HOME`, so stop the bot service first)
- `just orchestrator` (orchestrator service; orchestrator topics in the bot and `zdx orchestrator send` need it)
- `just deploy` (build release → build and install `~/.local/bin/zdx-go` with the Mini App (needs Go and `bun`; signed as `dev.zdx.go`) → install `~/.local/bin/zdx` last (signed under a temp name, then renamed), because the new `zdx` execs `zdx-go` (a scheduled job runs `zdx jobs run`), and a failed build leaves the old `zdx` → guarded `zdx service restart all`; the standard way to ship a change to the running bot/scheduler/orchestrator service). `all` includes the `orchestrator` service: `restart all` checks that every service is installed before it restarts any, and the bot answers orchestrator topics only while that service runs. The restart refuses to interrupt active agent runs; wait for them, queue it with `zdx service restart --queue` (the bot restarts once no run is active — use this from inside an agent run, which counts as active), or explicitly run `zdx service restart all --force`. A bot plist whose command is not the current one (the old `~/.local/bin/zdx-gateway --root ROOT bot`) is installed again by `zdx service restart bot`/`all`, not by the bot's own exit. Set `ZDX_CODESIGN_ID` in your shell env to a code-signing identity so `install` and `go-install` sign `zdx` and `zdx-go` with stable identifiers and macOS TCC grants survive rebuilds; unset, they stay ad-hoc signed and macOS re-prompts after every build.
- `just safe-deploy` (the same build and install order as `deploy`, `zdx` last → `zdx service restart all --queue`: the bot restarts once no agent run, worker, or orchestrator `[worker update]` turn is pending. Use this from inside an agent run, or whenever workers may be busy. A queued restart cannot install a changed bot plist: it says so, and `zdx service restart bot` applies it.)
- `just ci` (full local CI: lint + test; matches GHA's Rust job on push/PR — optional pre-push gate. GHA also vets and builds the Go module and runs `just shared-check`'s tests on Linux; the gateway, monitor, and TUI tests run only locally)
- `just ci-fast` (fast inner-loop check: fmt + clippy on default features, lib+bins only — single cargo mode, avoids mode-switching tax)
- `just lint` (format + clippy)
- `just fmt` (nightly rustfmt)
- `just clippy` (lint only)
- `just test` (workspace tests; skips doctests, which are empty and cost ~15s to build)
- `just install-hooks` (enable pre-commit formatting and change-aware pre-push Clippy)
- `just update-defaults` (maintainer: refresh both default_models.toml + default_config.toml)
- `just update-models` (maintainer: refresh default_models.toml)
- `just update-config` (maintainer: refresh default_config.toml)
- `just codebase` (generate codebase.txt for entire workspace)
- `just codebase crates/zdx-engine` (generate codebase.txt for specific crate)
- `just build-release` (build the release `zdx` binary, not incremental; Rust only: it needs neither Go nor the Mini App build. The engine and the `zdx` CLI crate build at `opt-level = 1` in release, see `Cargo.toml`)
- `just go-check` (whole Go module: `gofmt`, `go vet`, `go test -race`) / `just gateway-check` / `just monitor-check` / `just tui-check` / `just shared-check` (the same for one part)
- `just go-build [OUT]` (`zdx-go` with the Mini App embedded; runs the cached web build) / `just go-build-dev [OUT]` (no Mini App, no `bun`) / `just go-install` (build and sign it into `~/.local/bin/zdx-go`, what `zdx` and the bot service run). Builds pass `-ldflags "$(apps/shared/ldflags.sh)"` for the zdx version
- `just web-demo` / `just web-dev` / `just web-check` / `just web-build` (Svelte Mini App in `apps/web`; requires `bun`)
- `bun test apps/web/build-cached.test.mjs` (web-build cache/invalidation regression tests)
- `bun test apps/web/annotate.test.mjs` (artifact annotation bridge and message-validation tests)
- `bun test apps/web/image-marks.test.mjs` (image annotation geometry, reveal/badge placement, and payload tests)
- `bun test apps/web/artifact-link.test.mjs` (`Open artifact` start-parameter parsing and key lookup)
- `bun test apps/web/artifact-list.test.mjs` (Artifacts tab order and day groups)
- `bun test apps/web/version-match.test.mjs` (artifact Update: which notes move to a newer version)
- `bun test apps/web/text-preview.test.mjs` (text artifact preview: UTF-8 cut, binary detection, line splitting)
- `bun test apps/web/tool-pairing.test.mjs` (transcript tool pairing: result, interrupted, live, and orphaned calls and their shared views)

## Verification

- During iteration, prefer `just ci-fast` for quick feedback (clippy on default features, lib+bins only).
- When behavior changes, also run `just test` (or a narrower `cargo test -p <crate> --lib --bins --tests`).
- `just ci` matches what GHA runs on push/PR; treat it as an optional pre-push gate rather than the default after every change. GHA will catch what local `ci-fast` misses (all-features, all-targets, tests).
- Use `just lint` or `just test` only when intentionally running one half of CI.

## Conventions

- Rust edition: 2024 (see `Cargo.toml`)
- Formatting: rustfmt defaults
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries
- Product stage: early-stage/iterative (alpha). Prefer simple, explicit implementations over compatibility/fallback layers.
  - Do not add backward-compatibility shims, defensive fallback logic, or keep dead code paths unless explicitly requested or required by a documented contract in `docs/SPEC.md`.
  - Remove deprecated code immediately rather than marking it deprecated.

## Cross-surface features

ZDX exposes the same capabilities through several surfaces (CLI/exec, TUI, Monitor, bot and Mini
App, agents and scripts). A feature belongs to the system, not to the surface that first asked for
it:

- Own the capability once, in the shared layer, derived from canonical sources. Domain logic,
  derivation, and naming live there; surfaces do not re-derive them from their own display types.
- Give every capability a machine-readable, programmatic path, not only a human-facing one. Agents
  and scripts are first-class consumers.
- Keep surfaces as thin adapters: presentation, layout, and interaction only. A rule implemented
  twice in two languages is a bug.
- Preserve uncertainty in the shared layer. Missing or incomplete data stays explicitly unknown;
  a surface must not infer a stronger claim than the shared layer states.
- Reuse-ready is not the same as built everywhere. Making the capability directly consumable by
  another surface is required; building that surface's UI is only in scope when requested.
- Test behavior once where it lives, then add narrow adapter tests per exposed surface.

## Derived caches and hot paths

For any disposable store rebuilt from files on disk (SQLite caches, search indexes, generated artifacts):

- Never put corpus-sized work on a per-call path. Whole-store checks, full-table counts, and directory re-scans belong on index/maintenance commands, on write paths, or behind a freshness window.
- Don't scan to prove a store is healthy. Let the next real operation fail.
- Anything a user waits on is hot, even when it runs async.
- Filter, order, and limit in the store, not in memory. Add the index that query needs.
- When you add something the indexer stores, capture it in the indexer and bump the schema version. It rebuilds on next start. Prefer one mechanism over per-field migrations and backfills: no trivial lookup should ever fall back to opening every file.
- Know what the bump costs before you make it. On a 13.7k-thread, 3.2GB store the full rebuild is ~2.5-4min depending on load, and it runs inside `sync()` holding the process-wide connection lock — so it blocks whoever triggers it. `just install` runs `zdx threads reindex` to pay that there instead of inside the first Telegram message after a restart, which lists recent threads to build the orchestrator prompt. Warm, that command is ~0.2s.
- Reuse expensive handles only when you have measured the saving.
- Test against real data volume. Fixtures are too small to show scaling bugs.
- Keep tests off the developer's real `ZDX_HOME`. A test that reaches thread listing will index the live store; take `zdx_engine::test_support::temp_zdx_home()` (feature `testing` from other crates).

## Tests (keep it light)

- Add tests only to protect a user-visible contract or a real regression.
- Prefer integration tests in `crates/zdx-cli/tests/` over unit tests for CLI/output/persistence behavior.
- Avoid mutating process-global env vars in-process; set env on spawned CLI commands instead.
- When a unit test seems to need an `env_lock`/`EnvVarGuard` pattern, prefer refactoring the code under test to accept explicit inputs or expose a pure helper instead of serializing env mutation in tests. Only test process-global env mutation directly when that behavior is itself the thing being verified.
- **Process-global test isolation**:
  - Assume Rust unit tests run concurrently in one process under `cargo test`. Environment variables, current directory, and static handles are shared between test threads.
  - Never cache a value derived from `ZDX_HOME`, another environment variable, or the current directory in an unkeyed `OnceLock`/`LazyLock`/`static`. Prefer instance-owned state or caches keyed by the explicit path.
  - Never call `std::env::set_var`/`remove_var` directly in unit tests without an RAII guard that restores previous environment state and clears dependent caches on `Drop`.
  - Use UUIDs (`uuid::Uuid::new_v4()`) or atomic counters for test IDs and temporary markers; do not use system clock nanoseconds or sleeps for uniqueness.

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
