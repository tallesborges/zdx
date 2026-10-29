# zdx-assets development guide

Scope: embedded asset content (prompts, instruction layers, default TOMLs, bundled skills, bundled commands, built-in subagents). No runtime logic.

## Where things are

- `src/lib.rs`: `&'static str` / `&'static [u8]` constants + `bundled_skill_assets()` / `bundled_command_assets()` accessors
- `build.rs`: generates the bundled-skill and bundled-command asset manifests from `bundled_skills/` and `bundled_commands/`
- `prompts/`: shared prompt templates (identity, system, handoff, thread title, read_thread)
- `prompts/partials/*.md`: named MiniJinja partials (`project_instructions`, `runtime_layers`, `safety_rules`) registered by the engine renderer and pulled into templates with `{% include "<name>" %}`. The default system prompt and `prompts/non_interactive_subagent_suffix.md` (appended to a subagent prompt when a job runs it) both use them, so edit these blocks only in the partials. Register a new partial in `PROMPT_PARTIALS` in `src/lib.rs`.
- `instruction_layers/job_instruction_layer.md`: job-specific rules, applied on top of the exec layer (keep shared one-shot rules in the exec layer only)
- `instruction_layers/exec_instruction_layer.md`: non-interactive `zdx exec` run rules (direct CLI, orchestrator workers, zbar)
- `instruction_layers/chat_instruction_layer.md`: interactive TUI chat output rules
- `instruction_layers/telegram_instruction_layer.md`: Telegram bot output rules
- `default_config.toml`: default configuration template (generated output; do not edit by hand)
- `default_models.toml`: default model registry fallback
- `model_overrides.toml`: pinned post-fetch overrides applied by `zdx models update` (corrects stale/promo upstream pricing and limits). Exception: `input_audio`/`audio_formats` entries are shipped `ask_media` audio defaults, read at runtime and ignored by the updater (contract: `docs/SPEC.md` §Models registry)
- `audio_providers.toml`: shipped `ask_media` audio request settings per built-in provider (payload shape, formats, limits), the only source of those defaults; loaded by `zdx-engine` `config::shipped_provider_audio`
- `bundled_skills/*/`: built-in bundled skill fallbacks; `build.rs` embeds every file under this tree
- `bundled_skills/model-dispatch/`: named-model dispatch, fan-out, and reasoning-level comparisons
- `bundled_skills/frontend-design/`: product-UI design skill adapted from `anthropics/skills` (Apache-2.0; keep `LICENSE.txt` with its modification notice there, not in `SKILL.md`); also auto-loaded by `html-artifact-specialist` for visual craft
- `bundled_skills/html-artifacts/`: the HTML artifact skill (one `SKILL.md`; its craft notes on page, SVG, charts, and prototypes are optional know-how); auto-loaded by the built-in `html-artifact-specialist`
- `bundled_commands/*.md`: built-in bundled custom slash commands; `build.rs` embeds every file under this tree
- `subagents/*.md`: built-in standalone subagent prompts (`explorer`, `oracle`, and the write-capable `html-artifact-specialist`, registered in `zdx-engine` `built_in_definitions`) plus the reserved persistent `orchestrator` profile (Telegram home base; cannot be overridden by user/project subagent files)

## Conventions

- This crate must not depend on any other local crate.
- Keep this crate free of runtime logic, I/O, config parsing, and providers/tools dependencies.
- New bundled skills or prompts go here; runtime crates reference them via `zdx_assets::...` constants.

## Checks

- `cargo check -p zdx-assets`. For changes that touch runtime consumers, follow the root verification rules.
