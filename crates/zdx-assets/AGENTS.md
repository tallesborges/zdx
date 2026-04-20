# zdx-assets development guide

Scope: embedded asset content (prompts, instruction layers, default TOMLs, bundled skills, built-in subagents). No runtime logic.

## Where things are

- `src/lib.rs`: `&'static str` / `&'static [u8]` constants + `bundled_skill_assets()` accessor
- `build.rs`: generates the bundled-skill asset manifest from `bundled_skills/`
- `prompts/`: shared prompt templates (identity, system, handoff, thread title, read_thread)
- `instruction_layers/automation_harness.md`: built-in automation harness instruction layer
- `instruction_layers/exec_instruction_layer.md`: exec/terminal-specific output rules
- `instruction_layers/chat_instruction_layer.md`: interactive TUI chat output rules
- `instruction_layers/telegram_instruction_layer.md`: Telegram bot output rules
- `default_config.toml`: default configuration template (generated output; do not edit by hand)
- `default_models.toml`: default model registry fallback
- `bundled_skills/*/`: built-in bundled skill fallbacks; `build.rs` embeds every file under this tree
- `subagents/*.md`: built-in standalone subagent prompts (`explorer`, `oracle`)

## Conventions

- This crate must not depend on any other local crate.
- Keep this crate free of runtime logic, I/O, config parsing, and providers/tools dependencies.
- New bundled skills or prompts go here; runtime crates reference them via `zdx_assets::...` constants.

## Checks

- `cargo check -p zdx-assets`
- Final verification after a change that touches runtime consumers: `just ci`
