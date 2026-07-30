> Stage: done. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals

- Fast mode becomes part of the model spec string: `openai:gpt-5.2@fast` instead of a provider-level config flag.
- The `@fast` marker is visible anywhere a model name is shown (input title, `/model` picker, `zdx models`, monitor, bot status/launcher), with no separate badge or hidden state.
- `@fast` composes with the existing thinking suffix (`model@high@fast`), parsed by one shared helper.
- Fast mode is selectable per model (favorites, thread overrides, subagents, automations) because it lives in the string that already flows everywhere.
- The provider-level `fast_mode` flag, the `/fast` command, and the `[F]` badge are all removed — the model picker is the only way to choose fast mode.

# Non-goals

- No new service tiers beyond the current `priority` mapping (no `flex` selection UI).
- No new per-model registry capability field to gate fast eligibility; eligibility stays provider-based (OpenAI + OpenAI Codex) as it is today.
- No backward-compat shim for existing `providers.*.fast_mode` values in user config (alpha stage; `AGENTS.md` forbids compat layers).
- No change to how thinking level is stored in `[config] thinking_level` — only the suffix grammar is extended.

# Design principles

- User journey drives order: the string must render correctly before it must be toggleable, and be toggleable before the picker offers it.
- One source of truth for spec syntax: a single parse/format pair in `crates/zdx-engine/src/models.rs` (today `split_model_thinking` / `format_model_thinking`, `models.rs:298-316`).
- Reuse the existing suffix machinery rather than adding a parallel mechanism; `resolve_provider` (`crates/zdx-providers/src/lib.rs:605`) stays the single provider-resolution choke point.
- Remove the old path in the same change that replaces it.

# User journey

1. I look at the model name in the TUI input title / `/model` list / monitor / bot and immediately see whether it is `@fast`.
2. I pick the `@fast` row in the model picker and the model spec itself changes to `openai:gpt-5.2@fast`.
3. I send a message and the request goes out with `service_tier: "priority"`.
4. I set a favorite, thread override, or subagent model to a `@fast` spec and it just works, because it is only a string.

# Foundations / Already shipped (✅)

## Suffix parsing for thinking level

- What exists: `split_model_thinking` / `format_model_thinking` in `crates/zdx-engine/src/models.rs:298-316`, using `ThinkingLevel::from_name` (`crates/zdx-types/src/config.rs:11-75`). Call sites: `read_thread.rs:111`, `handoff_generation.rs:140`, `prompt_builder_generation.rs:48`, `title_generation.rs:60`, `tldr_generation.rs:38`, `zdx-monitor/src/app.rs:2512`.
- ✅ Demo: `cargo nextest run -p zdx-engine split_model_thinking`.
- Gaps: only one `@` suffix is supported; `@high` and `@fast` cannot coexist.

## service_tier transport to the OpenAI Responses API

- What exists: `ProviderBuildContext.service_tier` (`crates/zdx-providers/src/lib.rs:125-153`) → `openai/api.rs:190`, `openai/codex.rs:318` → `openai/responses.rs:37,125` → request body field (`responses_types.rs:39`).
- ✅ Demo: current `/fast` toggle plus a real request already sends `"priority"`.
- Gaps: the value is derived from `provider_config.fast_mode` in `crates/zdx-engine/src/core/agent.rs:1105-1113`, not from the model spec.

## Provider resolution strips the `provider:` prefix

- What exists: `resolve_provider` (`crates/zdx-providers/src/lib.rs:605`) already handles `provider@account:model`; `agent.rs:1088` passes `selection.model` to the client.
- Gaps: it does not strip trailing modifiers, so a raw `@fast` would leak into the model ID sent to the API.

# MVP phases (ship-shaped, demoable)

## Phase 1: One spec parser + engine honors `@fast` — done 2026-07-30 ✅

- **Goal**: `model = "openai:gpt-5.2@fast"` in config produces `service_tier: "priority"` and a clean model ID on the wire.
- **Scope checklist**:
  - [x] Add `ModelSpec { base: &str, thinking: Option<ThinkingLevel>, fast: bool }` (landed in `crates/zdx-types/src/config.rs`, re-exported as `zdx_engine::models::ModelSpec`) with `parse` + `to_string`-style format in `crates/zdx-engine/src/models.rs`, replacing `split_model_thinking` / `format_model_thinking`.
  - [x] Grammar: consume trailing `@…` segments right-to-left, each matching either a `ThinkingLevel` name or the literal `fast`; stop at the first unknown segment. Canonical format order: `model[@thinking][@fast]`. Never consume the account `@` (it precedes `:`).
  - [x] Update all existing `split_model_thinking` / `format_model_thinking` call sites to the new API. `format_model_thinking` was kept as a thin helper (`ModelSpec::parse(model).with_thinking(level)`) because it now has to *preserve* `@fast`; the monitor uses it in six places.
  - [x] In `crates/zdx-engine/src/core/agent.rs`: parse `config.model` once, pass `spec.base` to `resolve_provider`, `effective_max_tokens_for`, and `model_supports_reasoning`; derive `service_tier` from `options.service_tier.or(spec.fast → "priority")`.
  - [x] Audit other raw uses of `config.model` / model-override strings in engine paths (subagent, automations, handoff, title/tldr generation) so a `@fast` spec never reaches a provider as part of the model ID.
- **✅ Demo**: set `model = "openai:gpt-5.2@fast"` in config, run `zdx exec "hi"`, confirm the request carries `service_tier: "priority"` and the model ID has no `@fast`; then remove the suffix and confirm no tier is sent.
- **Risks / failure modes**:
  - A missed raw `config.model` consumer sends `gpt-5.2@fast` to the API → 400. Mitigate by grepping every `config.model` / `model_id` use before finishing.
  - Ambiguity if a future thinking level is literally named `fast` — reserve the name.
- **Result**: `resolve_provider` strips modifiers and exposes `ProviderSelection::fast`; `build_run_turn_setup` parses the spec once and uses `spec.base` for the custom-provider lookup, `effective_max_tokens_for`, and `model_supports_reasoning`.

## Phase 2: Delete the whole provider-level fast-mode mechanism — done 2026-07-30 ✅

- **Goal**: no `fast_mode` field, no `/fast` command, no `[F]` badge anywhere; the spec suffix is the only representation.
- **Scope checklist**:
  - [x] Delete the `/fast` command: `build_fast_mode_toggle_actions` + `FAST_MODE_UNAVAILABLE_MSG` + dispatch (`crates/zdx-tui/src/features/input/update.rs:44-73, 1066-1103`), the command entry and availability gate (`common/commands.rs:219-267`), the palette branch (`overlays/command_palette.rs:296-308`), and the `mod.rs` re-export (`features/input/mod.rs:25`).
  - [x] Delete `ConfigMutation::SetFastMode` (`mutations.rs:125`), `UiEffect::PersistFastMode` (`effects.rs:149`) and their handlers (`update.rs:1137-1149`, `runtime/mod.rs:820-827`).
  - [x] Delete the `[F]` badge and `fast_mode_enabled_for_model` / `fast_mode_provider_for_model` (`features/input/render.rs:308-335`, `state.rs:668-678`); the title renders the spec string, which now contains `@fast`.
  - [x] Delete `Config::{fast_mode_for_provider, set_fast_mode_for_provider, save_fast_mode_for_provider}` + `provider_fast_mode_key` (`crates/zdx-engine/src/config.rs:1367-1414, 1812-1818`) and `ProviderConfig.fast_mode` (`:2100-2123`).
  - [x] Remove every `fast_mode = false` line from `crates/zdx-assets/default_config.toml` (~25 entries, `:98-303`); regenerate with `just update-config`.
  - [x] `crates/zdx-cli/src/cli/commands/imagine.rs:217-233, 260-275`: derive the tier from the parsed model spec and pass the stripped model to the image client.
  - [x] `crates/zdx-monitor/src/app.rs:183-254, 2512, 2650-2697`: use the shared parse/format so the monitor's model/thinking editor preserves `@fast`.
  - [x] `crates/zdx-bot/src/handlers/message/launcher.rs:194-216` and `status.rs:229`: keep the suffix visible in favorites/status display.
  - [x] Delete the two now-dead tests: `state.rs:758` `fast_mode_helpers_follow_the_active_provider` and the `/fast`-hidden assertion in `command_palette.rs:842`.
- **✅ Demo**: no `fast_mode` or `/fast` matches remain in the repo; `just ci-fast` and `just test` pass; `/fast` no longer appears in the TUI palette; a `@fast` model shows the suffix in its title with no `[F]` badge; monitor thinking change on a `@fast` model keeps the suffix.
- **Extra**: `handle_slash_commands` in the TUI composer only existed for `/fast`, so it was removed with its call site; the modal-precedence test now guards `$echo hi` instead of `/fast`. Bot `/model set` now compares the parsed `ModelSpec::base` so a `@fast` spec is accepted. README + `docs/SPEC.md` (Model routing) document the suffix grammar.
- **Risks / failure modes**:
  - Old user configs with `fast_mode = true` silently lose the setting (accepted per non-goals; the user re-selects `@fast` in the picker).
  - The thinking picker must round-trip through the shared formatter or it will drop `@fast`.

## Phase 3: Fast variants selectable in the model picker — done 2026-07-30 ✅

- **Goal**: I can pick a `@fast` model without typing the suffix — this is now the only entry point.
- **Scope checklist**:
  - [x] In `crates/zdx-tui/src/overlays/model_picker.rs`, rows are now `ModelRow { model, fast }`; eligible models get an extra `@fast` row rendered as `… @fast    priority tier · 2× cost`.
  - [x] Make current-selection matching suffix-aware so the active `@fast` spec highlights the right row (`:240-380`).
  - [x] `zdx models list` prints each eligible `@fast` variant under its base id (with the cost hint); `--json` rows carry `fast_id`. The monitor's model picker also lists `@fast` items so its model/thinking editor can select and preserve the suffix.
- **✅ Demo**: `/model`, filter `fast`, select the `@fast` row; the title and config both show the suffix and the next request uses the priority tier. `zdx models` lists the variant.
- **Risks / failure modes**:
  - Picker row count doubling for OpenAI models adds noise; keep variants adjacent to their base model.

# Contracts (guardrails)

- A model spec without `@fast` never sends `service_tier`.
- The model ID sent to any provider never contains an `@fast` or `@<thinking>` segment.
- `provider@account:model` account parsing is unaffected.
- `@fast` and a thinking suffix can coexist and round-trip through parse → format unchanged.
- Changing thinking level never changes fast mode, and vice versa.
- `@fast` rows are offered only for providers that support the priority tier.

# Key decisions (decide early)

- Suffix grammar and canonical order: `model[@thinking][@fast]`, parsed order-independently right-to-left. Decided — parsing must be settled before any call site is touched.
- Eligibility source: provider-based (OpenAI + OpenAI Codex), no new registry capability field. Revisit only if a non-OpenAI provider gains tiers.
- Provider `fast_mode` is deleted, not deprecated (workspace convention: no compat shims).

# Testing

- Manual smoke demo per phase as listed above.
- Regression tests limited to contracts: spec parse/format round-trip (`zdx-types`: order-independent modifiers, account form, unknown suffix, `without_thinking`/`with_fast`), `resolve_provider_strips_modifiers_from_model_id` (`zdx-providers`), `fast_variant` eligibility + `format_model_thinking` preserving `@fast` (`zdx-engine`), and the picker row spec/cost-hint test (`zdx-tui`).
- Not covered by an automated test: the `service_tier` mapping itself (`options.service_tier.or(selection.fast → "priority")`) — `build_run_turn_setup` builds a real provider client, so it needs credentials. Verified by reading the single call site.
- Delete the two now-dead fast-mode TUI tests instead of porting them.

# Polish rounds (after MVP)

## Polish round 1: labeling clarity — done 2026-07-30 ✅ (shipped with Phase 3)

- Picker `@fast` rows show `priority tier · 2× cost`; `zdx models list` appends `(priority tier, 2× cost)`.

# Later / Deferred

- Per-model registry capability flag for service tiers — revisit when a model in an eligible provider does not support `priority`, or when another provider adds tiers.
- `flex` (reduced-cost) tier as a second suffix — revisit if cost control becomes a goal.
- Migration of existing `providers.*.fast_mode = true` configs — revisit only if a non-alpha user reports losing the setting.
