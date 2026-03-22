# zdx-cli development guide

Scope: CLI argument parsing/router, subcommands, and interactive/exec mode entrypoints.

## Where things are

- `src/main.rs`: binary entrypoint
- `src/cli/`: argument structs + command dispatch
- `src/cli/commands/automations.rs`: automations commands (`list`, `validate`, `run`)
- `src/cli/commands/daemon.rs`: scheduled automations daemon loop
- `src/cli/commands/imagine.rs`: image generation command handler (`zdx imagine`)
- `src/cli/commands/telegram.rs`: Telegram utility commands
- `src/cli/commands/worktree.rs`: worktree command handler
- `prompts/exec_instruction_layer.md`: exec/terminal-specific output rules
- `src/modes/exec.rs`: non-interactive streaming mode
- `src/modes/mod.rs`: mode exports (exec + feature-gated TUI)
- `tests/`: CLI integration tests (`assert_cmd`, fixtures)

## Conventions

- Keep CLI glue thin; shared behavior belongs in `zdx-core`.
- Prefer integration tests in `crates/zdx-cli/tests/` for CLI behavior.

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo test -p zdx-cli`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Add/move/delete prompt layer files in this crate: update this file.
- Command behavior contract changes: update `docs/SPEC.md` as needed.