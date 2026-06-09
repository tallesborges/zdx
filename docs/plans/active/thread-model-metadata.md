# Plan: Record Model in Thread JSONL

**Feature:** Persist which model ran in a thread so any saved thread can answer "which model did I use", and so token usage can be attributed to a model.

**Existing state:**
- Threads are append-only JSONL (`crates/zdx-engine/src/core/thread_persistence.rs`); event types: `meta`, `message`, `tool_use`, `tool_result`, `reasoning`, `usage`, `notice`, `interrupted` (SPEC §8, `docs/SPEC.md:124`).
- The model is **not** recorded. The only model-ish field is `meta.model_override`, populated only on an explicit `/model` override, and rewritten in place so mid-thread switches are lost.
- `usage` events (`ThreadEvent::Usage`) carry token counts but **no model**.
- The live run registry (`agent_activity`) tracks `model` per run but markers are deleted on `Drop` — useless for past threads.
- The only model name in old JSONL is buried in `ReplayToken::Gemini { model }` — Gemini-only.
- The model is known at the write site: `setup.model` → `consume_stream(..., &setup.model)` (`agent.rs:1012`) → `StreamState.turn.model` (`agent.rs:355`).

**Constraints:**
- Rust edition 2024; alpha-stage, prefer simple/explicit over compat shims.
- Back-compatible, additive change — no `schema_version` bump (old threads simply have no model).

**Success looks like:** Run a turn, then `zdx threads show <id>` displays the model used. After a mid-thread `/model` switch, later `usage` lines show the new model.

# Goals

- Record the model on each `usage` event in thread JSONL.
- Surface the model in `zdx threads show`.
- Single source of truth (no dual-write to keep consistent).

# Non-goals

- Per-token cost computation/UI (separate concern; this just enables it).
- `schema_version` bump or thread migration.
- Backfilling model into existing threads.

# Design

- The per-`usage` event is the single source of truth. The model is known at the emit site and survives mid-thread `/model` switches.
- Model rides `AgentEvent::UsageUpdate` (runtime/wire) → copied onto `ThreadEvent::Usage` (disk) by `UsagePersistor`. The persistor only sees the event stream, so it must read the model off the event.
- Provider/disk layering stays clean: `turn.model` (bare id) keeps feeding Gemini replay untouched; the metadata field is separate.

# Phase 1 — carry model end-to-end (core)

- `crates/zdx-types/src/events.rs:137` — add `model: String` to `AgentEvent::UsageUpdate`.
- `crates/zdx-engine/src/core/agent.rs:1831` — `StreamState::flush_pending_usage` is the only production emit site; set `model` from `self.turn.model.clone()`.
- `crates/zdx-engine/src/core/thread_persistence.rs`:
  - `ThreadEvent::Usage` — add `model: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `ThreadEvent::usage(...)` constructor — accept the model and set it.
  - `UsagePersistor` — add `current_model: Option<String>`; set it in the `UsageUpdate` arm (`~ln 1030`); use it in `flush_pending`/output paths (`~ln 1052`, `1054`, `1114`).

# Phase 2 — surface it

- `crates/zdx-engine/src/core/thread_persistence.rs:2239` — `format_transcript` currently skips `Usage` events. Derive a "Models used: …" line (or render model on the schema header at `~ln 2184`) so `zdx threads show` displays it.

# Phase 3 — fast listing (optional, deferred)

- Add `meta.model` mirroring the existing `model_override` plumbing (`Meta` variant, `meta_with_root*`, `ThreadMeta`, `read_meta`, a `read_thread_model` helper) so `threads list`/picker can show a model badge without scanning events. Skip unless list-level display is needed — `threads show` already answers the question.

# Match-site & test updates (blast radius)

- Exhaustive `UsageUpdate` destructures that must add the field: `crates/zdx-tui/src/features/transcript/update.rs:173` and the agent unit test at `agent.rs:3068`. `exec.rs:297` uses `{ .. }` (fine); the bot does not destructure it (verified).
- `ThreadEvent::usage(...)` call sites: 3 production (in the persistor) + ~8 tests in `thread_persistence.rs` (3113, 3148–3166, 3197) — update to pass a model (tests can pass `None`/a literal).
- New round-trip test locking the JSONL contract: emit `UsageUpdate { model }` → persist → reload → assert `ThreadEvent::Usage.model` is preserved (mirror persist tests at `thread_persistence.rs:3216`).

# Docs

- Update `docs/SPEC.md:124` (§8) to document that `usage` events carry an optional `model` (and `meta.model` if Phase 3 lands).

# Verification

- `just ci-fast` during iteration.
- `cargo nextest run -p zdx-engine` for persistence/round-trip tests; `just test` before wrapping up.
- Manual: run a turn → `zdx threads show <id>` shows the model; do a `/model` switch mid-thread → later usage lines show the new model.

# Open decisions

1. **Model value:** bare id (`claude-opus-4-6`, zero plumbing via `turn.model`) vs provider-qualified (`claude-cli:claude-opus-4-6`, needs passing `config.model` into `consume_stream`). Default: **bare** for Phase 1; registry ids are effectively unique. Upgrade only if the provider prefix is wanted.
2. **Phase 3 (`meta.model`):** include now for list/picker, or defer? Default: **defer**.
