# Subagent model refs to `[[favorites]]` — Design Proposal

> Stage: **draft**. Research + design, not implementation.
> Date: 2026-09-03.

**TL;DR:** Let `[subagents.overrides.<name>].model` hold `fav:<alias>` instead of a literal `provider:id`. The alias resolves against the existing top-level `[[favorites]]` list at delegation time, so changing one favorite repoints every role that references it. Thinking level stays per-subagent. Literal model ids keep working unchanged.

---

## 1. Current state

**Favorites** (`crates/zdx-engine/src/config.rs:665-690`): `[[favorites]]` is a `Vec<ModelFavorite>` of `{ alias, model, thinking }`. Consumed by the TUI Tab-cycle (`crates/zdx-tui/src/features/input/update.rs:658-710`), the Telegram launcher (`crates/zdx-bot/src/handlers/message/launcher.rs:37-52`), and the monitor Config tab. `ModelFavorite::matches` and `models_equivalent` already treat bare and `provider:`-prefixed ids as equivalent. Aliases are free-form strings and today contain spaces and punctuation (`Low`, `Medium`, `High · Claude`, `High · Codex`, `Ultra` in the user config).

**Subagent model selection** resolves in this order at `crates/zdx-engine/src/tools/subagent.rs:85-115` and `:235-266`:

1. per-invocation `model` argument from the tool call,
2. `[subagents.overrides.<name>].model` (layered onto the definition at `:87-96`),
3. definition frontmatter `model:` (`crates/zdx-assets/subagents/{explorer,oracle,orchestrator}.md`),
4. parent thread model, then `config.model`.

Any model coming from 1–3 is checked by `validate_model_supported` (`:394-426`) against `Config::subagent_available_models()` (`crates/zdx-engine/src/config.rs:1427`); an unavailable id fails the tool call with `model_not_supported`. Thinking level is separate: override `thinking_level` → definition `thinking_level` → parent (`:322-328`).

**Config shape** (`crates/zdx-engine/src/config.rs:106-143`): `SubagentOverride { model: Option<String>, thinking_level: Option<ThinkingLevel> }`, keyed by subagent name. Written by `save_subagent_override_to` (`:1263-1296`) as two plain keys; removed by `clear_subagent_override_to` (`:1310-1332`). Config layers merge per-key via `merge_items` in `load_layered` (`:1026-1050`), so a workspace `.zdx/config.toml` can override single keys.

**Monitor Config tab** (`crates/zdx-monitor/src/tabs/config.rs`): `build_config_lines` (`:138`) renders a `favorites` group (one row per preset plus `[+ add favorite]`) and a `subagents` group built by `build_subagent_lines` (`:268-299`), which shows override model → definition model → `(default)`. `Enter` opens the two-phase model picker (`open_model_picker:476`, `ModelPickerState::new:569`), which lists only concrete `provider:id` entries; `commit_model_picker` (`:740-790`) routes `subagents.<name>` to `save_subagent_override`, and `d` calls `clear_subagent_override`.

**The problem.** Role → model bindings are duplicated literals in three places (built-in frontmatter, `[subagents.overrides]`, favorites), so switching "the fast model" means editing every site. Amp's Modes page is the reference shape: each role (Search, Oracle, Librarian, Read Thread) is bound to a model *class*, and one mode switch moves them together.

---

## 2. Goals / non-goals

**Goals**
- `[subagents.overrides.<name>].model` can name a favorite alias instead of a model id.
- Changing `[[favorites]]` once repoints every subagent referencing that alias.
- Config-driven and user-level: no prompt/frontmatter edits to change a model.
- Editable from the monitor Config tab.
- Existing literal overrides keep working with no migration.

**Non-goals**
- Changing `crates/zdx-assets/default_config.toml` or the repo's `.zdx/config.toml` (workspace default untouched; the shipped default has no `[subagents.overrides]` and gains none).
- Editing `crates/zdx-assets/subagents/*.md` frontmatter. Their `model:` stays as the last-resort default.
- Favorite refs for `model`, `title_model`, `handoff_model`, or other role models. Subagents only for v1.
- Inheriting a favorite's `thinking` into subagents (see §4).
- Any compatibility shim beyond honoring literal ids, which are simply "not a ref".

---

## 3. Config shape

```toml
[[favorites]]
alias = "Low"
model = "google-antigravity:gemini-3.8-flash-high"
thinking = "high"

[subagents.overrides]
explorer = { model = "fav:Low", thinking_level = "low" }
oracle   = { model = "fav:High · Claude", thinking_level = "high" }
task     = { model = "openai-codex:gpt-5.6-sol" }   # literal, still valid
```

- Sentinel prefix: `fav:`. Everything after it, trimmed, is the alias; aliases may contain spaces and punctuation, so no further parsing.
- `fav:` is reserved as a model-ref prefix and is not a provider id. Resolution runs *before* `resolve_provider`, so a hypothetical provider named `fav` would be shadowed — accepted, and documented in `default_config.toml` comments.
- Alias match: exact after `trim()`, case-insensitive. First match wins if duplicates exist.
- No new struct or field. `SubagentOverride.model` stays `Option<String>`; the ref lives in the string. `save_subagent_override_to` needs no change since it writes the model string verbatim.

---

## 4. Resolution order

At `crates/zdx-engine/src/tools/subagent.rs:87-96`, where the override is layered onto the definition:

1. Per-invocation `model` argument — unchanged, wins over everything, never a ref (the tool schema keeps requiring an available model id).
2. Override `model`:
   - starts with `fav:` → look up the alias in `config.favorites`; on hit, substitute `favorite.model`;
   - otherwise → used literally, exactly as today.
3. Definition frontmatter `model:` when no override.
4. Parent/config model.

Then the existing `validate_model_supported` check runs on the resolved id, so a favorite pointing at a disabled provider produces today's `model_not_supported` failure with no new code.

**Thinking stays per-subagent.** The favorite's `thinking` is deliberately ignored for subagents: the reference supplies a model only. Thinking resolves as today (override `thinking_level` → definition → parent). This keeps `explorer = { model = "fav:Low", thinking_level = "low" }` fast even when the `Low` favorite carries `thinking = "high"` for interactive use.

**Implementation shape.** One helper on `Config` in `crates/zdx-engine/src/config.rs`, next to `ModelFavorite`:

```rust
/// Strips a `fav:` prefix and resolves the alias against `[[favorites]]`.
/// `None` → not a ref. `Some(Err(alias))` → ref with no matching favorite.
pub fn resolve_model_ref(&self, value: &str) -> Option<Result<String, String>>
```

Two call sites: `subagent.rs` override application, and `build_subagent_lines` in the monitor for display. Nothing else reads `SubagentOverride.model`.

---

## 5. Missing alias

An unresolvable `fav:<alias>` is a config typo, not a runtime fault, and a subagent run is one-shot. Behavior:

- `tracing::warn!` once per resolution with the subagent name and the missing alias.
- Treat the override model as unset and fall back to the definition's frontmatter model (step 3), then the parent model. The delegation still runs.
- The monitor renders the row as `fav:<alias> (missing)` so the typo is visible where it is fixed.

Rejected alternative: failing the tool call with `invalid_config`. It matches the strictness of `model_not_supported`, but it turns a stale alias into a hard delegation failure in the middle of a run, where the definition default is a correct and safe answer.

---

## 6. Monitor UI

All changes are in `crates/zdx-monitor/src/tabs/config.rs`.

- **Display** (`build_subagent_lines:268-299`): when the override model is a ref, show `fav:<alias> → <resolved>@<thinking>`; when it does not resolve, show `fav:<alias> (missing)`. Literal and `(default)` rows are unchanged.
- **Picker** (`ModelPickerState::new:569`): for fields starting with `subagents.`, prepend one pseudo-item per configured favorite, rendered `fav:<alias>` and sorted above the concrete model list. Concrete ids remain selectable, so an intentional one-off pin stays a one-key choice.
- **Commit** (`commit_model_picker:740`): when the chosen item is a `fav:` pseudo-item, pass the raw `fav:<alias>` string to `save_subagent_override` and keep the thinking phase (thinking is stored in its own key, so no `@level` suffix is appended). Non-subagent fields must not offer refs.
- **Reset**: `d` still calls `clear_subagent_override`; no ref-specific path.
- **Favorites group**: unchanged. Editing a favorite there implicitly repoints every subagent that references it; the subagent rows show the new resolved model on the next `reload_config_lines`.

No changes to the TUI, the Telegram launcher, or `zdx-bot`: none of them read subagent overrides.

---

## 7. Migration

- Existing `[subagents.overrides.<name>].model` values without the `fav:` prefix are literal ids and behave exactly as today. No rewrite, no deprecation, no dual-read shim.
- No config version bump; the field type is unchanged.
- Opting in is manual: edit the value in `$ZDX_HOME/config.toml` or pick a `fav:` row in the monitor.
- `crates/zdx-assets/default_config.toml` gains only comment lines documenting the `fav:` prefix under `[subagents]`. Shipped defaults, the repo `.zdx/config.toml`, and built-in subagent frontmatter are untouched.

---

## 8. Acceptance criteria

1. With `explorer = { model = "fav:Low", thinking_level = "low" }` and a `Low` favorite, `invoke_subagent(subagent: "explorer")` runs on that favorite's model at thinking `low`; the favorite's own `thinking` has no effect.
2. Editing the `Low` favorite's model repoints explorer with no other config edit and no prompt edit.
3. A ref whose alias has no favorite logs a warning and falls back to the definition's frontmatter model; the delegation still completes.
4. A ref resolving to a model from a disabled provider fails with the existing `model_not_supported` error, same as a literal id would.
5. A literal `[subagents.overrides]` model id behaves identically to before the change (regression test over the current `explorer`/`oracle` entries).
6. Monitor Config tab: the subagent row shows `fav:<alias> → resolved@thinking`, the picker offers `fav:` entries above concrete models for `subagents.*` fields only, committing writes `model = "fav:<alias>"` plus `thinking_level`, and `d` clears the override.
7. `crates/zdx-assets/default_config.toml`, the repo `.zdx/config.toml`, and `crates/zdx-assets/subagents/*.md` show no model-value changes in the diff.
8. Alias matching is trim + case-insensitive, verified by a unit test on the resolver including a spaced/punctuated alias (`High · Claude`).
9. `just ci-fast` and `cargo nextest run -p zdx-engine -p zdx-monitor` pass.
