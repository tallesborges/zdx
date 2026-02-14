# zdx-cli development guide

Scope: CLI argument parsing/router, subcommands, and interactive/exec mode entrypoints.

## Where things are

- `src/main.rs`: binary entrypoint
- `src/cli/`: argument structs + command dispatch
- `src/cli/commands/automations.rs`: automations commands (`list`, `validate`, `run`)
- `src/cli/commands/daemon.rs`: scheduled automations daemon loop
- `src/cli/commands/telegram.rs`: Telegram utility commands
- `src/cli/commands/worktree.rs`: worktree command handler
- `src/modes/exec.rs`: non-interactive streaming mode
- `src/modes/mod.rs`: mode exports (exec + feature-gated TUI)
- `tests/`: CLI integration tests (`assert_cmd`, fixtures)

## Conventions

- Keep CLI glue thin; shared behavior belongs in `zdx-core`.
- Prefer integration tests in `crates/zdx-cli/tests/` for CLI behavior.

## Checks

- Targeted: `cargo test -p zdx-cli`
- Workspace lint/test: use `just lint` / `just test` from repo root

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Command behavior contract changes: update `docs/SPEC.md` as needed.