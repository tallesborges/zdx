# `[[model_modes]]` — Design Proposal

> Stage: **draft**. Research + design, not implementation.
> Supersedes `subagent-favorite-refs.md` (the `fav:` ref idea), which is removed. Line references are against the current working tree, which has uncommitted changes.

**TL;DR:** Replace `[[favorites]]` with `[[model_modes]]`: named tiers (`fast`, `smart`, `ultra`), each with a `primary` model spec, a short `description`, and `alternatives`. Modes become the single place a model is named — subagent overrides point at `mode:<name>`, the TUI cycles them, the bot launcher lists them, and the system prompt renders them so agents pick a tier by task instead of memorizing ids.

---

## 1. What exists today

- **Favorites** (`crates/zdx-engine/src/config.rs:726-755`): `[[favorites]]` = `{ alias, model }`. `thinking` is no longer a config key — it lives in the model spec's `@<level>` suffix, `ModelFavorite::thinking` is `skip_deserializing` and derived at load, and a leftover `favorites[i].thinking` key is rejected loudly by `reject_removed_thinking_keys` (`config.rs:265-302`, contract in `docs/SPEC.md:323`).
- **Consumers:** TUI Tab/Shift+Tab cycle (`crates/zdx-tui/src/features/input/update.rs:679-760`) and status alias (`features/input/render.rs:315` → `Config::active_favorite_alias`, `config.rs:985`); Telegram launcher buttons (`crates/zdx-bot/src/handlers/message/launcher.rs:40-52,187,230`); monitor `favorites` group + editor (`crates/zdx-monitor/src/tabs/config.rs:274-289,316,452-458,739`); `favorite_count` in the Mini App payload (`crates/zdx-bot/src/server.rs:366,1964` → `apps/web/src/views/MonitorView.svelte:272`); writers `Config::save_favorites{,_to}` (`config.rs:1404,1412`).
- **Subagent model resolution** (`crates/zdx-engine/src/tools/subagent.rs:243-289`): a layered `apply_model_spec` chain — config/parent → definition frontmatter → `[subagents.overrides.<name>].model` → per-invocation `model` argument — then `validate_model_supported` (`:397`) against `Config::subagent_available_models()` (`config.rs:1613`). Thinking rides along in each spec.
- **System prompt**: MiniJinja template at `crates/zdx-assets/prompts/system_prompt_template.md`, vars built in `crates/zdx-engine/src/core/context.rs:240-260,498-556`. No model information is rendered today.
- **`zdx models list`** (`crates/zdx-cli/src/cli/mod.rs:800-812`, `crates/zdx-cli/src/cli/commands/models.rs:201-261`): flags `--provider`, `--all`, `--json`. `ProviderKind::is_subscription` already exists (`crates/zdx-providers/src/lib.rs:510`) but is not exposed as a filter.

**Problem.** A model id is written in many places (favorites, subagent overrides, subagent frontmatter, automations) with no shared vocabulary, and nothing tells an agent *which tier* to use for a task. Favorites are a TUI keyboard convenience, not a concept the rest of the system can reference.

---

## 2. Config shape

User-confirmed modes and picks, with every id verified against `zdx models list` (see "Verified ids" below):

```toml
[[model_modes]]
name = "fast"
description = "quick searches, cheap checks, exploration"
primary = "google-antigravity:gemini-3.8-flash-high"
alternatives = ["claude-cli:claude-sonnet-5@low"]

[[model_modes]]
name = "smart"
description = "deep reasoning, architecture, oracles, default work"
primary = "claude-cli:claude-opus-5@high"
alternatives = ["openai-codex:gpt-5.6-sol@xhigh"]

[[model_modes]]
name = "ultra"
description = "hardest open-ended tasks, maximum capability"
primary = "claude-cli:claude-fable-5-1@high"
alternatives = ["openai-codex:gpt-6-astra@xhigh"]
```

```rust
pub struct ModelMode {
    pub name: String,
    pub description: String,
    pub primary: String,              // model spec, thinking in the @<level> suffix
    pub alternatives: Vec<String>,    // same
    #[serde(skip_deserializing)]
    pub thinking: ThinkingLevel,      // derived from `primary`, mirrors ModelFavorite
}
```

**Deviation from the sketch: no `thinking` key.** The sketch had `thinking = "low"` per mode. That contradicts the shipped contract (`SPEC.md:323`) that thinking exists only inside a model spec, and `reject_removed_thinking_keys` already fails configs that keep such a key. Thinking therefore goes in `primary`/`alternatives` (`…@low`, `…@high`), and that function gains a case rejecting `model_modes[i].thinking` with the same message. Each alternative carries its own level, which is what you want when the equivalent pick needs a different level to match the primary's quality.

- `name` is free-form. `fast`/`smart`/`ultra` are a convention, not an enum: adding a fourth mode is one TOML block, no code change.
- Names are matched trimmed and case-insensitively; first match wins.
- `description` is prose aimed at the agent reading the prompt — it is the mode's routing rule.
- An empty/missing `[[model_modes]]` is valid: no prompt section, no cycling, refs fail as in §4.
- `crates/zdx-assets/default_config.toml` gains commented examples only. Concrete ids go stale, and the shipped default must not pin models.

### Verified ids

Checked with `zdx models list` / `--all` / `--provider <p> --json` against the live registry, plus `is_subscription` from `crates/zdx-providers/src/lib.rs:219-300`. Subscription-backed providers: `claude-cli`, `openai-codex`, `google-antigravity`, `opencode-go`, `grok-build`, `xiaomi-plan`, `qwen-code`.

| Spec | Registry entry | Notes |
|---|---|---|
| `google-antigravity:gemini-3.8-flash-high` | Gemini 3.8 Flash High | `reasoning = false` |
| `claude-cli:claude-sonnet-5@low` | Claude Sonnet 5 | `reasoning = true` |
| `claude-cli:claude-opus-5@high` | Claude Opus 5 | `reasoning = true` |
| `openai-codex:gpt-5.6-sol@xhigh` | GPT-5.6 Sol | `reasoning = true` |
| `claude-cli:claude-fable-5-1@high` | Claude Fable 5.1 | `reasoning = true` |
| `openai-codex:gpt-6-astra@xhigh` | GPT-6 Astra | `reasoning = true` |

**`fast` primary carries no `@` suffix.** For `google-antigravity:*`, the reasoning tier is baked into the model variant: the registry lists `gemini-3.8-flash-low`, `-medium`, and `-high` as separate ids, and all three report `reasoning = false`, so they take no thinking level (only `gemini-3.7-flash-tiered` and `gemini-pro-agent` report `reasoning = true` on that provider). The `-high` is therefore the **variant**, not a thinking level, and appending `@high` would be a no-op the TUI thinking picker refuses. The current `$ZDX_HOME/config.toml` favorite carries exactly that redundant suffix (`google-antigravity:gemini-3.8-flash-high@high`); the migration drops it. The `fast` alternative keeps `@low`, since `claude-cli:claude-sonnet-5` does report `reasoning = true`.

**Only main-company models.** Every id above comes from Anthropic, OpenAI, or Google through the user's own subscription (`claude-cli`, `openai-codex`, `google-antigravity`). Aggregators and second-party carriers (`opencode-go`, `deepseek`, `parity:*`) are deliberately excluded from the modes.

### The `medium`/`default` question — settled

**Three modes, no `medium`** (user-confirmed). The default worker model is already the `smart` tier, so a middle mode would be the default under a second name, and every routing decision (prompt text, subagent override, launcher button) gets a fuzzier boundary for no new capability. The former `Medium` favorite (`openai-codex:gpt-5.6-sol@medium`) survives as the `smart` alternative at `@xhigh`, which is what it is in practice: the same tier, a different vendor. Modes are a free-form list, so a genuine middle tier later is one TOML block with no code change.

### `alternatives` are not fallbacks

`alternatives` are equivalent picks at the same tier, for a human or an agent to choose deliberately (vendor preference, provider outage, a task that suits one model better). Runtime resolution uses `primary` only; there is no automatic failover, retry chain, or load balancing. They surface in the prompt, the monitor row, and the picker, and nothing else reads them.

---

## 3. Resolution (`mode:<name>`)

A model field may hold `mode:<name>` instead of a spec. One resolver in `config.rs`, next to `ModelMode`:

```rust
/// Strips a `mode:` prefix and resolves it to that mode's `primary` spec.
/// `None` → not a ref. `Some(Err(name))` → ref with no matching mode.
pub fn resolve_mode_ref(&self, value: &str) -> Option<Result<&str, &str>>
```

Accepted in exactly three places:

1. `[subagents.overrides.<name>].model` — e.g. `explorer = { model = "mode:fast" }`, `oracle = { model = "mode:smart" }`. Resolved in `resolve_execution_model` (`subagent.rs:243-289`) before the `apply_model_spec` call for the override.
2. `invoke_subagent`'s `model` argument, so the prompt can say "delegate this on `mode:ultra`" without pasting an id. Resolved before the tool-argument `apply_model_spec` call; the resolved id still goes through `validate_model_supported`.
3. Subagent definition frontmatter `model:` — same resolver, so a user-authored subagent file can be tier-bound too. Built-in frontmatter keeps literal ids as its default.

`mode:` is reserved as a ref prefix and is not a provider id; resolution runs before `resolve_provider`. Thinking comes from the resolved spec's suffix, so a mode ref sets model *and* level together; layers after it still win, unchanged.

**Missing mode.** `tracing::warn!` once naming the ref site and the unknown name, then treat the field as unset and continue down the existing layer chain (definition → parent/config model). A stale name is a config typo, not a reason to fail a delegation mid-run. The monitor renders it as `mode:<name> (missing)`. A mode whose resolved model is unavailable keeps today's `model_not_supported` failure.

---

## 4. System prompt

New MiniJinja var `model_modes: Vec<PromptTemplateModelMode { name, description, primary, alternatives }>`, built in `context.rs` next to `skills_list` (`:240-260,498-556`) from the effective config, rendered only when non-empty. The block lives in the reserved orchestrator profile (`crates/zdx-assets/subagents/orchestrator.md`), not the shared system prompt template: the orchestrator is the only profile that routes work across tiers, and workers resolve `mode:<name>` through their subagent override with no prompt section.

Template block:

```jinja
{% if model_modes %}
# Model Modes

Named model tiers from the user's config. When work is delegated or spawned, choose the mode that fits the task and pass that mode's primary id — or `mode:<name>` — as the `model` argument. This does not change the model for the current run.
{% for mode in model_modes %}
- `{{ mode.name }}` — {{ mode.description }}. Primary: `{{ mode.primary }}`.{% if mode.alternatives %} Alternatives: {% for alt in mode.alternatives %}`{{ alt }}`{% if not loop.last %}, {% endif %}{% endfor %}.{% endif %}
{% endfor %}
Alternatives are equivalent picks at the same tier, not automatic fallbacks: use one only when the primary is unavailable or the task specifically calls for it.
Prefer models on a provider the user already subscribes to; they carry no per-token cost. When no mode fits, run `zdx models list --plan-only` (add `--all` for the full registry) rather than guessing an id.
{% endif %}
```

**`zdx models list --plan-only`**: worth adding, cheap — a `--plan-only` flag filtering on `ProviderKind::is_subscription` (`crates/zdx-providers/src/lib.rs:510`), composable with `--provider`/`--json`, plus a `subscription: bool` field in the `--json` output. Without it the prompt's advice is unactionable, since nothing in the current output marks which providers are subscription-backed.

---

## 5. Migration

**Replacement, not deprecation.** `[[favorites]]` and `ModelFavorite` are deleted in the same change that adds `[[model_modes]]`, per the workspace rule against compatibility shims. A config that still has `[[favorites]]` fails to load with a message pointing at `[[model_modes]]`, reusing the `reject_removed_thinking_keys` pattern (`config.rs:265-302`) — silent ignoring would leave Tab cycling mysteriously dead.

The five current favorites collapse into the three confirmed modes:

| Favorite | Model | Lands as |
|---|---|---|
| `Low` | `google-antigravity:gemini-3.8-flash-high@high` | `fast` primary, redundant `@high` dropped |
| `Medium` | `openai-codex:gpt-5.6-sol@medium` | `smart` alternative, raised to `@xhigh` |
| `High · Claude` | `claude-cli:claude-opus-5@high` | `smart` primary, unchanged |
| `High · Codex` | `openai-codex:gpt-6-astra@medium` | `ultra` alternative, raised to `@xhigh` |
| `Ultra` | `claude-cli:claude-fable-5-1@high` | `ultra` primary, unchanged |

The only model not carried over from a favorite is the `fast` alternative `claude-cli:claude-sonnet-5@low`, which is new.

The user's three existing `[subagents.overrides]` entries become `explorer = { model = "mode:fast" }`, `oracle = { model = "mode:smart" }`, `orchestrator = { model = "mode:smart" }`. Hand-edited once in `$ZDX_HOME/config.toml`; no automatic rewriter.

**Consumer fallout:**
- **TUI Tab-cycle** keeps working, over modes: Tab/Shift+Tab cycles each mode's `primary` in config order (`update.rs:679-760`, `next_favorite_index` → `next_mode_index`). Alternatives are not in the cycle — they would make the cycle long and ambiguous. The status line shows the mode name (`render.rs:315`, `active_favorite_alias` → `active_mode_name`, matching on the primary spec). Cycling still writes no config, per `SPEC.md:325`.
- **Bot launcher** renders one button per mode with its name and description in the header (`launcher.rs:40-52,187,230`); callbacks already carry the alias string, so `nt:p:{name}` needs no protocol change. The availability filter (`filter_available_favorites`) keeps its behavior on `primary`.
- **Mini App**: `favorite_count` → `mode_count` (`server.rs:366,1964`, `MonitorView.svelte:272`). Mechanical rename only.
- **Docs**: `SPEC.md:323,325,326,372` and `README.md:99` say "favorites"; update the wording. `crates/zdx-bot/AGENTS.md:22` and `crates/zdx-monitor/AGENTS.md:16` name the favorites paths.

---

## 6. Monitor and TUI

**Monitor Config tab** (`crates/zdx-monitor/src/tabs/config.rs`): the `favorites` group becomes `model_modes` (`:274-289`, `MODEL_SECTION_ORDER:313`, `ADD_FAVORITE_LABEL:316` → `[+ add mode]`).
- Row per mode: `<name> → <primary> (+N alt)`; `Enter` opens the existing two-phase picker on the primary; the add-row appends a mode named `mode{N}` with an empty description.
- `alternatives` and `description` are not editable in v1 — TOML only. Editing a list of specs needs a different overlay than the single-value picker, and that is not worth building before the modes are in use.
- `d` deletes the mode (`delete_or_reset_selected:764`), `save_favorite:739` → `save_mode`, `Config::save_favorites{,_to}` → `save_model_modes{,_to}` rewriting the `[[model_modes]]` array of tables.
- Subagent rows show `mode:<name> → <resolved>` for refs, `mode:<name> (missing)` when unresolvable, literal specs otherwise.

**Pickers** (monitor `ModelPickerState:630`, TUI model picker): for fields that accept a ref — subagent overrides — list one `mode:<name>` pseudo-entry per configured mode above the concrete models, and write the ref verbatim on commit. Concrete ids stay selectable for one-off pins. Non-ref fields (`model`, helper models, transcription, speech) do not offer them.

---

## 7. Out of scope

- No frontend/Mini App mode selector. The only web change is the `favorite_count` → `mode_count` rename.
- No mode refs in `model`, `title_model`, `handoff_model`, or other helper-model keys.
- No automatic fallback to `alternatives`, no cost-based or capability-based auto-selection.
- No editing of `description`/`alternatives` from the monitor.
- No automations mode refs (they take a model spec today; can follow later through the same resolver).

---

## 8. Acceptance criteria

1. `[[model_modes]]` loads with `name`, `description`, `primary`, `alternatives`; a `thinking` key inside a mode is rejected with the model-spec message; `ModelMode::thinking` is derived from `primary`.
2. A config containing `[[favorites]]` fails to load with an error naming `[[model_modes]]`; `ModelFavorite` and `save_favorites` no longer exist in the tree.
3. `explorer = { model = "mode:fast" }` runs explorer on the `fast` primary at that spec's level; changing the mode's `primary` repoints it with no other edit.
4. `invoke_subagent` accepts `model: "mode:ultra"` and resolves it before `validate_model_supported`; an unavailable resolved model still fails with `model_not_supported`.
5. An unknown `mode:<name>` logs a warning and falls through to the next layer; the delegation completes.
6. The system prompt contains a `# Model Modes` section listing each mode with description, primary, and alternatives, plus the subscription-preference and `zdx models list` lines; the section is absent when no modes are configured.
7. `zdx models list --plan-only` lists only models from subscription providers and composes with `--provider`/`--json`.
8. TUI Tab/Shift+Tab cycles mode primaries, the status line shows the active mode name, and cycling writes no config.
9. The Telegram launcher shows one button per mode; the monitor shows a `model_modes` group whose rows are editable (primary) and deletable, and subagent rows display `mode:<name> → resolved`.
10. `just ci-fast` plus `cargo nextest run -p zdx-engine -p zdx-monitor -p zdx-tui -p zdx-bot` pass; `SPEC.md`, `README.md`, and the two `AGENTS.md` files no longer describe favorites.
11. After the config edit, `$ZDX_HOME/config.toml` holds exactly the three modes from §2, each `primary`/`alternative` id appears in `zdx models list`, and no spec carries an `@<level>` suffix on a model whose registry entry reports `reasoning = false`.
