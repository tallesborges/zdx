# zdx-cli development guide

`docs/SPEC.md` is the source of truth for product behavior (contracts). This file is about working in the repo.

## Where things are

- `src/main.rs`: CLI entrypoint; wires commands to implementations
- `src/cli/`: `clap` CLI definitions
- `src/engine/`: engine loop + event stream (no terminal I/O)
- `src/renderers/`: stdout/stderr renderer + exec wrapper
- `src/ui/`: terminal UI app + chat loop
- `src/session/`: JSONL sessions (read/write)
- `src/tools/`: tool implementations + schemas
- `src/providers/`: provider clients (Anthropic, etc.)
- `src/config/`: config loading + paths
- `src/core/`: shared types (events, context, interrupt)
- `tests/`: integration tests (`assert_cmd`, fixtures)

## Keep this file up to date

- If you add/move a major module, update **Where things are** so future-you can find it fast.
- If you change workflows (build/run/test), update **Build / run**.
- If you add new conventions/boundaries (e.g., new renderer/UI split), add a one-liner here or in scoped `AGENTS.md` files.

## Build / run

- `cargo run -- --help`
- `cargo run --` (interactive; needs provider key via env)
- `cargo test`
- `cargo fmt`
- `cargo clippy`

## Conventions

- Rust edition: 2024 (see `Cargo.toml`)
- Formatting: rustfmt defaults
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries
- Keep the engine UI-agnostic: terminal I/O belongs in renderers/UI only

## Tests (keep it light)

- Add tests only to protect a user-visible contract or a real regression.
- Prefer integration tests in `tests/` over unit tests for CLI/output/persistence behavior.
- Avoid mutating process-global env vars in-process; set env on spawned CLI commands instead.

## Docs

- `docs/SPEC.md`: contracts (what/behavior)
- `docs/adr/`: durable decisions (why)
- `docs/plans/`: optional commit-sized plans (how)
