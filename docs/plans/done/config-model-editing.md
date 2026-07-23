> Stage: **done**. All three phases shipped (Config-tab model editing for core/helpers/audio/telegram, favorites CRUD, and skills-style subagent overrides); tests + `just ci-fast` green. Keep the existing `zdx monitor` **Config** tab, but reorganize it so every model-bearing setting is grouped clearly and editable in place via the picker that already exists. Source: user request — "a better and easy to control/change the models on the ZDX monitor… models for the helpers, favorites, sub agents; everything that has models" + follow-up "keep the config, maybe just organize better, so we change the configs if we want".

# Models on the Config tab

> Don't add a new tab. Improve the existing `Section::Config` tab so all models (main, helpers, audio, telegram bot, favorites, per-subagent) are grouped together and each one is editable with `Enter` → picker, so changing any model is a few keystrokes instead of hand-editing `config.toml` / frontmatter.

# Goals
- Reorganize the Config tab so model settings are grouped and scannable, and every model row is editable via the existing `ModelPickerState` overlay (model + thinking two-step).
- Make **everything with a model** editable from Config:
  - Core: `model` (+ `thinking_level`)
  - Helpers: `title_model`, `tldr_model`, `handoff_model`, `prompt_builder_model`, `read_thread_model`
  - Audio: `transcription.model`, `speech.model`
  - Telegram bot: `telegram.model` (+ `telegram.thinking_level`)
  - Favorites: the `favorites` presets (alias + model + thinking) — add / edit / remove
  - Subagents: per-subagent `model` (+ `thinking_level`) override for user/project agents
- Every edit persists to the right place (config.toml keys vs. subagent frontmatter) and reloads live in the tab.
- Keep the current keybindings the Config tab teaches: `↑↓ select · Enter edit · PgUp/PgDn scroll · Tab switch · q quit`.

# Non-goals
- No new tab — this stays inside `Section::Config`.
- No model *registry* editing (adding/removing providers or model definitions) — that's the `model-provider-mgmt` skill / `default_models.toml`. This only assigns *which existing model* each role uses.
- No editing of non-model config (verbose, language, provider base URLs, mcp, skills). Those stay visible but read-only in the Config tab, as today.
- No TUI-side changes — the interactive TUI has its own favorites Tab-cycle; this is the monitor surface only.
- No bulk "set all to X" / profile switching in the MVP (possible Later item).

# Design decisions
- **Keep the Config tab; reorganize + extend it.** The tab already exists mostly to edit models (`editable_model_fields`, `open_model_picker`, `commit_model_picker` in `crates/zdx-monitor/src/app.rs`). We keep it as-is structurally, but: (a) order the model groups first and clearly labeled, (b) make the missing model groups editable (telegram, favorites, subagents), (c) leave the remaining non-model config visible below as read-only rows.
- **Grouping/order** in `build_config_lines`: emit model groups in this order — `core`, `helpers`, `audio`, `telegram`, `favorites`, `subagents` — then the rest of the flattened config (`providers`, `mcp`, `skills`, …) as today. Only model rows are selectable/editable; the rest render dimmed/non-selectable.
- **Reuse the picker wholesale.** `ModelPickerState` + `ModelFieldKind` (`Chat`/`Transcription`/`Speech`) already handle model source, the thinking step, and the current-value marker. New targets plug into the same `commit_model_picker`.
- **Generalize the field descriptor.** Extend `editable_model_fields()` to return targets beyond config keys: an enum `target` of `ConfigKey(String)`, `Favorite(index)`, or `Subagent(name, path, source)`. `config_selected` indexes this unified list. Commit routes on `target`.
- **Persistence** stays in `zdx-engine::config` for config keys (existing `save_model_field` / `save_thinking_level` / `save_telegram_model` / `save_telegram_thinking_level`), gains a favorites writer, and a subagent frontmatter writer in `zdx-engine::subagents`.

# User journey
1. `just monitor` → `Tab` to **Config** (unchanged location).
2. Top of the tab now shows model groups in order: `core`, `helpers`, `audio`, `telegram`, `favorites`, `subagents`, each row `role → provider:model@thinking`. Non-model config follows below, dimmed.
3. `↑↓` to any model row, `Enter` opens the picker, type-to-filter, pick model, pick thinking, it saves and the row updates.
4. Favorites: an `[+ add favorite]` row + per-row edit/delete. Subagents: each discovered agent shows its effective model with an override/source marker.

# Foundations / Already shipped (reuse, don't rebuild) ✅
- **Model picker overlay**: `ModelPickerState`, `PickerPhase::{Model,Thinking}`, `render_model_picker` (`crates/zdx-monitor/src/ui.rs`), filter + current-value marker; chat via `available_models()`, curated STT/TTS via `zdx_engine::audio::{transcribe,speak}`. ✅
- **Editable field resolution**: `editable_model_fields()` maps `(section, key)` → `(path, ModelFieldKind)`; `commit_model_picker()` persists per-kind (`crates/zdx-monitor/src/app.rs`). ✅ core/helpers/audio already work.
- **Config-side savers**: `Config::save_model_field`, `save_thinking_level`, `save_telegram_model`, `save_telegram_thinking_level` (`crates/zdx-engine/src/config.rs`) — TOML-preserving. ✅
- **Subagent discovery**: `subagents::discover(root)` → `SubagentDefinition { name, path, source, model, thinking_level, … }`, precedence project > user > built-in (`crates/zdx-engine/src/subagents.rs`). ✅ read-only today.
- **Config grouping**: `build_config_lines` / `HELPER_MODEL_KEYS` already pull helper models into their own group (`crates/zdx-monitor/src/app.rs`). ✅ pattern to extend.

# Gaps to close
- `telegram.model` is **not** in `editable_model_fields` (savers exist but aren't wired).
- `favorites` has **no** editor and **no** TOML writer (`save_favorites`) — it's a `[[favorites]]` array-of-tables.
- Subagents are parse-only: no frontmatter *writer*, and built-in agents are embedded assets (no on-disk file) — need an override-file path.
- Groups aren't ordered model-first, and non-model rows aren't visually separated from editable ones.

---

# Phase 1 — Reorganize + Telegram (config-backed models) ✅ shipped
Goal: model groups ordered and clearly labeled at the top of Config, and the currently-missing Telegram bot model editable.

- In `build_config_lines`, order emitted groups `core → helpers → audio → telegram → favorites(stub) → subagents(stub)` then the rest; add a `telegram` model group carrying `telegram.model` (+ inline `@thinking`). Keep non-model rows below.
- Extend the editable-field resolver to add `telegram.model` as `ModelFieldKind::Chat`; route commit to `save_telegram_model` + `save_telegram_thinking_level` (switch on `target` instead of string-matching `field == "model"`).
- (Optional polish) render non-model config rows dimmed/non-selectable so `↑↓` only lands on editable model rows.
- Tests: extend the `editable_model_fields` test for the new group order incl. `telegram.model → Chat`; commit test for the telegram path using `save_*_to(tmp)`.
- ✅ Demo: `just monitor` → `Config` → edit main, a helper, transcription, speech, and the telegram bot model; values persist across reload.
- Verify: `cargo nextest run -p zdx-monitor` + `just ci-fast`.

# Phase 2 — Favorites editor ✅ shipped
Goal: create/edit/delete favorite presets from the Config tab.

- Add `Config::save_favorites(&[ModelFavorite])` (+ `_to(path)`) in `config.rs` rewriting the `[[favorites]]` array-of-tables while preserving the rest (toml_edit), mirroring existing preserving savers. Round-trip test.
- Render a `favorites` group: one row per favorite (`alias → provider:model@thinking`) + a trailing `[+ add favorite]` action row.
- Target `Favorite(index)`: `Enter` opens the picker seeded with current model+thinking; commit writes that index. `Enter` on `[+ add]` appends a new favorite (auto alias `fav{N}`) then opens the picker.
- Delete: `d`/`Del` on a favorite row removes it via `save_favorites` (status-line feedback; no confirm — local preset).
- Alias rename is Later polish (small text-input overlay).
- Tests: `save_favorites` round-trip (add/edit/remove) preserves other keys; resolver lists favorites in order with the add-row last.
- ✅ Demo: add a favorite, change its model, delete it — from the tab; TUI Tab-cycle reflects it.

# Phase 3 — Subagent model overrides ✅ shipped
Goal: set each subagent's model (+ thinking) from the Config tab.

Decision: **handle built-ins the "skills way" via a config override map** (confirmed). Built-in subagents stay managed/live by zdx (embedded/materialized, auto-refreshed on update); the model choice is a small `config.toml` key layered on top — never a forked copy of the prompt. This means built-in prompt updates always flow through and nothing goes stale. Only user/project subagents (files you authored) are edited by writing their own frontmatter, exactly like user/project skills.

> Shipped deviation: for simplicity the override map is applied **uniformly to every subagent** (built-in *and* user/project) — no frontmatter writer was added. A user/project agent's frontmatter `model` still acts as its default; the `[subagents.overrides.<name>]` entry layers on top, and `d`/reset removes the entry to fall back to that default. This keeps one code path and consistent reset semantics; frontmatter editing can be added later if per-file portability is wanted.

- Config shape: a `[subagents]` map holding per-subagent overrides, e.g.
  - `[subagents.explorer] model = "provider:id"` and optional `thinking_level = "high"`.
  - Add a `subagents: BTreeMap<String, SubagentOverride>` field to `Config` (`config.rs`), with `SubagentOverride { model: Option<String>, thinking_level: Option<ThinkingLevel> }`.
  - Add `Config::save_subagent_model(name, model, thinking)` (+ `_to(path)`) and `Config::clear_subagent_override(name)` (reset), toml_edit-preserving.
- Apply overrides in `subagents::discover()`: after loading a definition (built-in or file), if `config.subagents[name]` has a `model`/`thinking_level`, apply it on top of the live definition. Config override wins over the built-in default but is expressed as a key, not a file.
- Render a `subagents` group from `discover(root)`: one row per agent `name → effective provider:model@thinking` with a `builtin`/`user`/`project` marker and an "overridden" marker when a config entry applies.
- Target `Subagent { name, source }`: on commit —
  - **built-in** → write `[subagents.<name>]` in `config.toml` (skills-style, no file copy).
  - **user/project** (files you authored) → write the model into their `.md` frontmatter via a targeted line-edit of `model:`/`thinking_level:` (preserve body + other keys + comments).
  - Reset key removes the config override / clears the frontmatter key.
- Tests: `save_subagent_model` round-trip (set/reset) preserves other config keys; `discover` applies a config override on top of a built-in without altering the embedded prompt; frontmatter line-edit round-trip for a user agent.
- ✅ Demo: set `explorer`'s model from the tab (writes `[subagents.explorer]`), confirm `discover` reports the new model with the built-in prompt unchanged; edit a user agent and see its frontmatter updated; reset restores defaults.

# Later / Deferred
- Inline alias rename for favorites (text-input overlay).
- "Reset to default" per row (revert a helper/subagent to its `default_*` / built-in model).
- Bulk actions / model profiles (swap a whole set of roles at once).
- Per-subagent thinking-only edit without changing the model.
- Cost/context hints (from the model registry) inline in the picker.

# Risks / open questions
- **Non-model rows staying selectable**: today `↑↓` moves over editable model rows only (`editable_model_fields`); keep that so the extra visible config doesn't get in the way. Decide whether to render the rest dimmed vs. collapse it.
- **Frontmatter round-trip fidelity** (Phase 3): for user/project agents, use a **targeted line-edit** of the `model:`/`thinking_level:` keys rather than a full YAML re-serialize. Built-in agents are not file-edited at all — they use the `[subagents]` config override, so there's no prompt copy to keep in sync.
- **toml_edit for `[[favorites]]`**: appending/removing array-of-tables while preserving formatting — verify with a round-trip test before building the UI.
- **Selection index generalization**: generalize `config_selected` to index the unified `EditableModelField` list (config + favorites + subagents) and keep it clamped after reloads.

# Verification (whole feature)
- `cargo nextest run -p zdx-monitor` and `-p zdx-engine` for new savers/writers.
- `just ci-fast` during iteration; `just ci` before marking done.
- Manual: `just monitor` → Config tab → exercise every group end-to-end; confirm `config.toml` and subagent files on disk.
