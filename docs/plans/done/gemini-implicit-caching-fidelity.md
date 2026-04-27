# Gemini Implicit Prompt Caching Fidelity

## Project Summary
Make zdx replay Gemini assistant turns byte-identically to what Google originally streamed back, so Gemini's **implicit prompt cache** actually hits across multi-turn conversations. The fix spans four layers — SSE parser, engine assistant builder, on-disk `ThreadEvent` persistence, and request builder — because today each layer flattens or reorders parts in a way the next layer cannot recover.

## Existing State

### Replay loses per-part fidelity at four layers
1. **SSE parser** (`crates/zdx-providers/src/gemini/sse.rs:235-415`): three-pass scan merges all `thought:true` parts into one reasoning block, all non-thought text parts into one text block, then iterates function calls. Single `pending_signature: Option<String>` (`sse.rs:36`) keeps at most one signature per turn, with a "function-call signature wins" cross-part heuristic (`sse.rs:240-260`). Synthesizes a local id when Gemini omits `functionCall.id` (`sse.rs:368-386`).
2. **Engine builder** (`crates/zdx-engine/src/core/agent.rs:304-335`): `AssistantTurnBuilder::into_blocks` emits **fixed category order** `[reasoning…, text?, tool_use…]`. Original part order is gone before persistence.
3. **Persistence** (`crates/zdx-engine/src/core/thread_persistence.rs:148-193, 333-365`): streaming `AgentEvent`s become individual `ThreadEvent::{Reasoning, ToolUse, ToolResult}` records as they complete. Text is only flushed at turn end via `AssistantCompleted` / `TurnFinished` (`agent.rs:1907-1975`). **Walking persisted events does not recover original Gemini part order** because text completions are not emitted per-part. `ThreadEvent::{Message, ToolUse}` also drop signatures and id-origin metadata. `take_assistant_blocks` (`thread_persistence.rs:2026-2034`) re-buckets reasoning before tool_use on rehydration.
4. **Request builder** (`crates/zdx-providers/src/gemini/shared.rs:268-318`): `gemini_signature` (`shared.rs:380-396`) returns the **first** `ReasoningBlock` signature only; `append_assistant_blocks` attaches that one signature to the first text or first tool_use part. Synthetic `SYNTHETIC_THOUGHT_SIGNATURE` (`shared.rs:205`) fallback applied to text parts and to all models including Gemini 2.5. `Reasoning` blocks are dropped (`shared.rs:313`). FunctionCall always serializes `id` (`shared.rs:300-306`); paired `functionResponse.id` is also always serialized (`shared.rs:343-350`), even when the underlying id was synthesized rather than emitted by Gemini.

### What's already correct (do not change)
- Cache token math: `input = prompt - cached`, `cache_read_input_tokens = cached`, `cacheTokensDetails` fallback (`sse.rs:170-200`).
- `toolUsePromptTokenCount` added to `input_tokens` matches Vertex docs (additive). **Do not "fix" this.**
- System prompt trim (`crates/zdx-providers/src/shared.rs:81-85`) is stable across turns.
- Tool order is stable from `&[ToolDefinition]` slice order (`shared.rs:405-423`).
- `sanitize_gemini_function_schema` iterates `serde_json::Map` (BTreeMap-backed), deterministic.
- Empty text/thinking blocks already skipped in `AssistantTurnBuilder` (`agent.rs:321-324`).
- `SignatureProvider` enum already exists (`crates/zdx-types/src/messages.rs:60-63`) — reuse for the cross-provider stream-event signature carrier.
- `TurnFinished.messages` (`agent.rs:1907`) already carries the assembled ordered `ChatMessage`s. We add a sibling `AgentEvent::TurnCheckpoint { messages, prior_message_count }` emitted after each `process_tool_turn` completes, so persistence flushes incrementally between tool turns instead of only at terminal `TurnFinished`. Both events carry a `prior_message_count: usize` cursor so the persistence consumer can slice the new turn-suffix.

### What pi-mono does differently (the reference)
- Per-part `thoughtSignature` (`packages/ai/src/providers/google-shared.ts:131-169`).
- Sentinel **only** on Gemini-3 `functionCall` parts when no real signature.
- Re-emits `thought: true` parts only when `msg.provider === model.provider && msg.model === model.id` (exact match).
- `isValidThoughtSignature` (length % 4 == 0, base64 charset) before replaying any real signature.

## Constraints
- Implicit caching only. No `cachedContents.create`.
- `ChatContentBlock` and `ReplayToken` live in `crates/zdx-types` per AGENTS.md "pure shared value types." Provider-specific replay metadata stays in `ReplayToken` variants — no new parallel enums.
- Alpha conventions: no compatibility shims. New fields use `#[serde(default)]` so old transcripts load cleanly; do not maintain dual readers.
- Cannot regress Anthropic / OpenAI replay. `AssistantTurnBuilder` and persistence are shared.
- `toolUsePromptTokenCount` math, explicit `cachedContents` API, Cloud Code Assist `user_prompt_id` analysis, `parametersJsonSchema` migration are all **out of scope**.
- Crash-safety: persistence batches per `TurnCheckpoint` (after each completed tool turn) and per `TurnFinished`, not per streamed event. Long tool loops still persist incrementally between tool turns; only the in-flight tool turn is lost on hard crash. Document in `docs/SPEC.md` if not already covered.

## Success Looks Like
- A non-circular golden test (`crates/zdx-engine/tests/gemini_replay_fidelity.rs`) parses a captured raw Gemini SSE stream, runs it through the full pipeline (SSE parser → engine builder → `ThreadEvent` round-trip → request builder), and asserts the replayed assistant `Content` deep-equals a hand-curated `expected_content.json`.
- `SYNTHETIC_THOUGHT_SIGNATURE` only ever appears on the wire when model contains `gemini-3` AND the part is a `functionCall` AND no real signature exists.
- `functionCall.id` and matching `functionResponse.id` are emitted together (both real or both omitted).
- Manual smoke test on Gemini 3 Pro Preview (4096-token implicit-cache floor) shows non-zero `cachedContentTokenCount` from turn 2 onward.
- `just ci` green.

---

# Goals
- Preserve original assistant part order from Gemini stream → persistence → next request.
- Carry per-part Gemini signature + source model end-to-end via the existing `ReplayToken` mechanism.
- Track id origin (real vs synthesized) on `ChatContentBlock::ToolUse` and use it symmetrically for `functionCall.id` and `functionResponse.id` emission.
- Re-emit `thought: true` only on exact-model match; drop on mismatch.
- Restrict synthetic sentinel to Gemini-3 `functionCall` only; validate real signatures.
- **Persistence becomes turn-batched and order-preserving**: stop emitting `ThreadEvent::{Reasoning, ToolUse, ToolResult}` from streaming events; emit them all from the per-turn `messages` slice on `TurnCheckpoint` (between tool turns) and `TurnFinished` (terminal).

# Non-goals
- `cachedContents` API surface.
- Token-count refactoring.
- Cloud Code Assist `user_prompt_id` rework.
- `parametersJsonSchema` migration.
- Model-family-permissive replay.
- Per-tool-step incremental persistence is preserved via `TurnCheckpoint` events between tool turns (durability matches today's behavior at tool-turn boundaries; only the in-flight tool turn is at risk on hard crash).

# Design principles
- **Storage shape mirrors wire shape.** Persist what Gemini emitted in the order it was emitted. Use `TurnFinished.messages[prior_count..]` (already-ordered) as the persistence source; streaming events become UI-only.
- **One replay metadata path.** Extend `ReplayToken::Gemini`; no parallel enums.
- **Symmetric id handling.** `functionCall.id` and `functionResponse.id` emitted/omitted together based on `IdOrigin`.
- **Cache-friendly migration defaults.** `IdOrigin::Synthesized` is the serde default — old transcripts that synthesized ids automatically omit them on replay (better for cache hits than perpetuating bogus ids).
- **Synthetic sentinel is a documented escape hatch.** Apply only where pi applies it; exempt from base64 validation.
- **Permissive empty-model gate.** Old transcripts with empty `model` field replay normally; only mismatched-model is gated.
- **Atomic persistence contract.** `ToolUse`, `ToolResult`, `Reasoning`, and text persistence all change in one slice (2.4) — no half-state where some are streamed and others batched.

# User journey
1. Multi-turn Gemini 3 conversation begins.
2. Turn 1: model returns `[thought, text, functionCall_a (real id), text, functionCall_b (no id)]` with per-part signatures. zdx persists each part with its own metadata in original order at `TurnFinished`.
3. Turn 2 request: assistant message replays parts in original order with original per-part signatures and `thought: true`. `functionCall_a` emits `id`; `functionCall_b` omits it; matching `functionResponse_a` emits `id`, `functionResponse_b` omits it.
4. Google's implicit cache matches the prefix. `cachedContentTokenCount > 0` in usage.
5. Same flow on Gemini 2.5: signatures stored if returned, sentinel never used.

---

# Slice 1 — Failing golden fixture (lands first, blocks merge)

**Demoable change**: `cargo test -p zdx-engine --test gemini_replay_fidelity` exists and **fails** against the current code. The diff IS the spec for slice 2.

## Why first
Pure additive test that captures the contract before touching production code.

## Test location decision (decided now to avoid slice-2 churn)
- **Pipeline golden test**: `crates/zdx-engine/tests/gemini_replay_fidelity.rs` — `zdx-engine` already depends on `zdx-providers` (`crates/zdx-engine/Cargo.toml:35-41`). Reverse direction is forbidden.
- **Fixtures**: `crates/zdx-engine/tests/fixtures/gemini/`.
- **Layer-local unit tests** (slice 2 sub-gates) live in their own crates.

## Fixtures
- `crates/zdx-engine/tests/fixtures/gemini/multipart_turn.sse` — raw captured SSE bytes from a real Gemini 3 Pro Preview generation. Capture by enabling `ZDX_DEBUG_TRACE` (already supported in `gemini/api.rs:120-130`) and copying the trace.
- `crates/zdx-engine/tests/fixtures/gemini/multipart_turn_expected.json` — the **expected `candidates[0].content` object**, hand-copied from the raw SSE's final aggregated state. **Not** produced by zdx code.
- `crates/zdx-engine/tests/fixtures/gemini/multipart_turn_request.json` — the user message + tools + system prompt that produced this turn.

The fixture must include:
- 1 part with `thought: true` and a `thoughtSignature`.
- 2 separate text parts with distinct `thoughtSignature` values, interleaved with function calls.
- 2 `functionCall` parts: **one with a real `id` field, one without**. Each with its own `thoughtSignature`.
- ≥ 5 distinct `thoughtSignature` values across the parts.

## Test
```text
crates/zdx-engine/tests/gemini_replay_fidelity.rs
  test_byte_identity:
    1. Read fixture SSE bytes.
    2. Feed through GeminiSseParser → drain StreamEvents.
    3. Run StreamEvents through engine AssistantTurnBuilder → ChatContentBlocks.
    4. Persist via the same TurnFinished code path (write ThreadEvents),
       then rehydrate (round-trip).
    5. Build ChatMessage, call build_contents with model "gemini-3-pro-preview".
    6. Deep-compare contents[1] (assistant message) with multipart_turn_expected.json
       using serde_json::Value equality (order-independent for object keys, strict
       for array order — matches Gemini's wire contract).

  test_function_response_id_symmetry:
    Build a turn-2 message that combines the captured assistant turn with a
    user tool_result for both function calls. Assert functionResponse_a emits
    "id" and functionResponse_b omits it.
```

## Success criteria
- Test compiles and runs.
- Test **fails** with a clear diff.

## Risk
Low. Read-only test.

---

# Slice 2 — Core fidelity slice (closes slice-1 test)

**One atomic PR**, structured as **six sequential gates** (not independently mergeable). Each gate has a local test that must pass before the next gate's work begins. A failure at gate 2.6 (the golden) is bisectable via gates 2.1–2.5.

## Sequential gates

| # | Layer | Local test gate | Files |
|---|---|---|---|
| 2.1 | Replay metadata types + `IdOrigin` | `cargo test -p zdx-types` | `crates/zdx-types/src/messages.rs`, `crates/zdx-types/src/providers.rs` |
| 2.1.B | Cross-provider `Text` variant smoke (checkpoint within 2.1) | `cargo test --workspace --no-run` + provider tests | `crates/zdx-providers/src/anthropic/types.rs`, `openai/responses.rs`, `openai/chat_completions.rs` |
| 2.2 | SSE parser → ordered per-part `StreamEvent`s | `cargo test -p zdx-providers gemini::sse` | `crates/zdx-providers/src/gemini/sse.rs` |
| 2.3 | Engine builder + model threading + `TurnCheckpoint`/`TurnFinished` cursor | `cargo test -p zdx-engine core::agent` | `crates/zdx-engine/src/core/agent.rs`, `crates/zdx-engine/src/core/events.rs` |
| 2.4 | Persistence write/read switches to checkpoint-batched | `cargo test -p zdx-engine core::thread_persistence` | `crates/zdx-engine/src/core/thread_persistence.rs` |
| 2.5 | Gemini request builder → byte-identical replay | `cargo test -p zdx-providers gemini::shared` | `crates/zdx-providers/src/gemini/shared.rs` |
| 2.6 | Final pipeline | `cargo test -p zdx-engine --test gemini_replay_fidelity` | (slice 1 golden) |

Per-PR rule: all six gates must pass at submission time. Internal commit history may show them landing one at a time, but they ship together.

## 2.1 — Replay metadata types

**File**: `crates/zdx-types/src/messages.rs`

- Extend `ReplayToken::Gemini`:
  ```text
  Gemini {
      signature: String,
      #[serde(default)]
      model: String,
  }
  ```
  Old serialized tokens deserialize with `model: ""`. The replay gate is permissive on empty (see 2.5).
- Add `IdOrigin` enum and put it directly on `ChatContentBlock::ToolUse` (it's an id property, not a signature property):
  ```text
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
  #[serde(rename_all = "snake_case")]
  enum IdOrigin {
      #[default]
      Synthesized,
      Real,
  }

  ToolUse {
      id: String,
      name: String,
      input: Value,
      #[serde(default)]
      id_origin: IdOrigin,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      replay: Option<ReplayToken>,
  }
  ```
  **`Synthesized` is the default** — old transcripts with synthesized ids will correctly omit them on Gemini replay (cache-friendly per oracle review). New code paths in 2.2 must explicitly set `Real` when Gemini emits an id, otherwise the default applies.
- Convert `ChatContentBlock::Text(String)` → `Text { text: String, replay: Option<ReplayToken> }`:
  ```text
  Text {
      text: String,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      replay: Option<ReplayToken>,
  }
  ```
- Add a constructor helper for the common case:
  ```text
  impl ChatContentBlock {
      pub fn text(s: impl Into<String>) -> Self {
          Self::Text { text: s.into(), replay: None }
      }
  }
  ```

**File**: `crates/zdx-types/src/providers.rs`

- Extend `StreamEvent::ContentBlockCompleted` with a generalized signature carrier (per oracle's K — Gemini-specific naming inside a cross-provider event would muddle the abstraction):
  ```text
  ContentBlockCompleted {
      index: usize,
      signature: Option<(SignatureProvider, String)>,
  }
  ```
  `SignatureProvider` already exists at `crates/zdx-types/src/messages.rs:60-63`. Anthropic and OpenAI signatures continue to flow through their existing dedicated events (`ReasoningSignatureDelta`); the new field is for per-part Gemini signatures on text/tool_use blocks where no dedicated event exists. **Tuple is fine because `StreamEvent` is internal and not serde-derived** (`crates/zdx-types/src/providers.rs:234`); no serde attributes needed.

### Migration of `Text(String)` call sites

26 call sites verified via `grep` — split roughly **10 constructions** + **16 pattern matches**:

**Constructions** (replace with `ChatContentBlock::text(...)`):
- `crates/zdx-engine/src/core/agent.rs:323`
- `crates/zdx-types/src/messages.rs:153, 163`
- `crates/zdx-bot/src/agent/mod.rs:60`
- `crates/zdx-engine/src/core/thread_persistence.rs:1953, 1992`
- Tests: `gemini/shared.rs:825, 954`, `openai/responses.rs:419`, `openai/chat_completions.rs:1171`

**Pattern matches** (rewrite to `ChatContentBlock::Text { text, .. }`):
- `crates/zdx-tui/src/state.rs:494, 511`
- `crates/zdx-providers/src/gemini/shared.rs:285, 333`
- `crates/zdx-providers/src/anthropic/types.rs:263`
- `crates/zdx-providers/src/openai/responses.rs:192, 219`
- `crates/zdx-providers/src/openai/chat_completions.rs:380, 434`
- `crates/zdx-engine/src/core/thread_persistence.rs:1786, 1814, 2524, 2705, 2798, 3353`
- `crates/zdx-engine/src/core/agent.rs:3061`

Migration verification: **`cargo test --workspace --no-run`** (or `just ci`).

### Test gate
- `test_replay_token_gemini_with_model_roundtrip` — old `{ signature }` deserializes with `model: ""`, new `{ signature, model }` round-trips.
- `test_id_origin_default_is_synthesized` — `ToolUse` deserialized from old JSON has `id_origin: Synthesized`.
- `test_chat_content_block_text_constructor_helper` — `ChatContentBlock::text("x")` produces `Text { text: "x", replay: None }`.

## 2.1.B — Cross-provider `Text` variant smoke (checkpoint within 2.1, per oracle's W/BB)

This is a **checkpoint within 2.1**, not an independently mergeable gate — it depends on 2.1's variant migration already being applied to the workspace. Run the existing provider request-builder tests immediately after 2.1's type changes to catch any pattern-match regression early — before 2.2's SSE work begins. These tests don't depend on per-part signatures or `IdOrigin`; they only verify the `Text { text, replay: None }` migration didn't break existing serialization.

### Test gate
- `cargo test --workspace --no-run` (compile-wide migration check).
- `cargo test -p zdx-providers anthropic::types` — assistant message with `Text { text, replay: None }` blocks still serializes to Anthropic API.
- `cargo test -p zdx-providers openai::responses` — same for Responses API; ordered `[Reasoning, Text, ToolUse]` blocks produce ordered Responses items.
- `cargo test -p zdx-providers openai::chat_completions` — assistant message with mixed `Text` + `ToolUse` blocks produces a single chat-completions message with `content` + `tool_calls` populated (`chat_completions.rs:375-421` aggregator still works).
- `cargo test -p zdx-tui` and `cargo test -p zdx-bot` — pattern-match call sites (`zdx-tui/src/state.rs:494,511`, `zdx-bot/src/agent/mod.rs:60`) still compile and pass.

If any of these fail, fix the variant migration in 2.1 before proceeding.

## 2.2 — SSE parser → ordered per-part events

**File**: `crates/zdx-providers/src/gemini/sse.rs`

- Replace `pending_signature: Option<String>` and `signature_from_function_call: bool` with per-part attribution.
- Replace the three-pass merge (`sse.rs:262-348`) with a single in-order walk emitting one `ContentBlock*` event sequence per Gemini part.
- Each text part → its own `Text` block. On `ContentBlockCompleted`, populate `signature: Some((SignatureProvider::Gemini, sig))` when the part had a `thoughtSignature`.
- Each thought part → its own `Reasoning` block with its own `thoughtSignature` via existing `ReasoningSignatureDelta` path (already per-block).
- Each function call part → `ToolUse` event with its own `thoughtSignature` AND new `id_origin: IdOrigin` field on the relevant `StreamEvent` variant. `Real` when Gemini's part has a non-empty `id`; `Synthesized` when synthesized via `format!("{}-{}-{}", prefix, run_id, index)` (`sse.rs:380-385`). Extend `StreamEvent::ContentBlockStart` with `id_origin: Option<IdOrigin>` (None for non-tool-use blocks).
- Drop the `combined_text` / `combined_reasoning` accumulators.
- Stop the "function-call signature wins" cross-part heuristic.

### Test gate
- `test_per_part_signatures_emitted_in_order` — two text parts each with own signature → two `ContentBlockCompleted` events with distinct `signature` values.
- `test_function_call_id_origin_real` — part with `id` → `ContentBlockStart` with `id_origin: Some(Real)`.
- `test_function_call_id_origin_synthesized` — part without `id` → `id_origin: Some(Synthesized)`.
- `test_thought_part_emits_own_block` — single thought part → single `Reasoning` block (not merged with following thoughts).
- `test_no_more_pending_signature_field` — confirms removal via compile.

## 2.3 — Engine builder + model threading + `TurnFinished` cursor

**File**: `crates/zdx-engine/src/core/agent.rs`, `crates/zdx-engine/src/core/events.rs`

### Builder ordering
- Replace category-bucketed `AssistantTurnBuilder` (`thinking_blocks: Vec`, `text: String`, `tool_uses: Vec`) with a single ordered `parts: Vec<AssistantPart>` keyed by stream block index.
- `AssistantPart` enum: `Reasoning { text, replay }`, `Text { text, replay }`, `ToolUse { id, name, input, id_origin, replay }`.
- `into_blocks` walks `parts` in original index order; no category re-sort.

### Model threading (per oracle's L — concrete injection point)
- `RunTurnSetup.model: String` already exists (`agent.rs:857-915` builds it). Pass it explicitly:
  - `consume_stream(stream, prior_messages, sender, cancel, model: &str)`
  - `StreamState::new(model: String)` stores it as `model: String`
  - `AssistantTurnBuilder::new(model: String)` stores it
- When `AssistantTurnBuilder` constructs a `ReplayToken::Gemini` from a `signature: Some((SignatureProvider::Gemini, sig))` on `ContentBlockCompleted`, populate `ReplayToken::Gemini { signature: sig, model: self.model.clone() }`.
- For `ReasoningSignatureDelta { provider: SignatureProvider::Gemini, signature, .. }` (from `gemini/sse.rs:436-443`), the builder applies the same model.
- Anthropic and OpenAI `ReplayToken` variants do NOT change — model is only relevant for Gemini's exact-model gate. Asymmetry is justified.

### `TurnCheckpoint` + `TurnFinished` cursor (per oracle's H/Q/R — Option A with checkpoints)

- Add a new event `AgentEvent::TurnCheckpoint { messages: Vec<ChatMessage>, prior_message_count: usize }` for incremental persistence between tool turns. **Standalone variant** (not a new `TurnStatus::Checkpoint` on `TurnFinished`) — non-terminal events should not be muxed through the terminal event.

  **Consumer updates required (compile-breaking match exhaustiveness):**
  - `crates/zdx-types/src/events.rs:14-127` — define the new variant.
  - `crates/zdx-tui/src/features/transcript/update.rs:38-204` — add explicit no-op arm (TUI gets live state from streaming events, doesn't need `TurnCheckpoint`).
  - `crates/zdx-tui/src/runtime/mod.rs:1034-1085` — non-terminal forward via existing `_` arm; verify it doesn't accidentally treat the variant as terminal.
  - `crates/zdx-cli/src/modes/exec.rs:277-294` — add `"turn_checkpoint"` name to `event_type_name`.
  - `crates/zdx-cli/src/modes/exec.rs:298-316` — `sanitize_exec_event` returns `None` for `TurnCheckpoint` to prevent emitting full message snapshots in JSONL output.
  - `crates/zdx-engine/src/core/thread_persistence.rs` — handle in `UsagePersistor` (calls `flush_messages`).
  - All `TurnFinished` construction sites in tests must be updated for the new `prior_message_count` field.
- Extend `AgentEvent::TurnFinished` with `prior_message_count: usize`:
  ```text
  TurnFinished {
      status: TurnStatus,
      final_text: String,
      messages: Vec<ChatMessage>,
      prior_message_count: usize,  // NEW
  }
  ```
- **Cursor capture point** (per oracle's Q): capture `let initial_message_count = messages.len();` in `run_turn_with_cancel` (`agent.rs:693-714`) **before** `messages` is moved into `run_turn_inner`. This makes the cursor available to both the success path and the error/interrupt helpers (`emit_turn_error`, `emit_turn_error_with_messages` at `agent.rs:519-591`).
- Pass `initial_message_count` into:
  - `run_turn_inner` as a parameter (or carry through `RunTurnSetup`).
  - All six `TurnFinished` emission sites (`agent.rs:534, 541, 552, 563, 579, 1907`).
  - `emit_turn_error` / `emit_turn_error_with_messages` signatures (extend with `prior_message_count: usize`).
- Emit `TurnCheckpoint` after each `process_tool_turn` completion (`agent.rs:1880-1888` after `messages.push(ChatMessage::tool_results(...))`):
  ```text
  sender.send(AgentEvent::TurnCheckpoint {
      messages: messages.clone(),
      prior_message_count: initial_message_count,
  });
  ```
- **Do not** use `prior_messages.len()` from `build_interrupted_messages(prior_messages, turn)` (`agent.rs:1915-1960`) as the persistence cursor — that would drop messages appended during the same agent turn before the interrupt. Always use the run-entry count.
- Persistence consumer (2.4) tracks its own last-persisted index; on each `TurnCheckpoint`/`TurnFinished`, persists `messages[max(prior_message_count, last_persisted)..]`. Idempotent across repeated checkpoints; survives a checkpoint+terminal sequence without duplicates.
- Crash-safety: long tool loops persist incrementally between tool turns. Only an in-flight tool turn is at risk on hard crash. Better than today (where streaming-event ordering may already be inconsistent), much better than checkpoint-free Option A.

### Test gate
- `test_assistant_turn_preserves_part_order` — feed `[reasoning, text, tool_use, text, tool_use]` events → blocks come out in the same order.
- `test_assistant_turn_per_part_replay_metadata` — each block carries its own `replay` field.
- `test_assistant_turn_gemini_signature_includes_model` — Gemini signature in `ContentBlockCompleted` produces `ReplayToken::Gemini { signature, model }` populated from setup.
- `test_turn_finished_includes_prior_message_count` — initial 3 messages, turn appends 2, `TurnFinished` carries `prior_message_count: 3`.
- `test_turn_checkpoint_emitted_after_each_tool_turn` — multi-tool-turn run emits `TurnCheckpoint` between turns plus `TurnFinished` at end; all carry the same `prior_message_count` value (initial), and `messages.len()` grows monotonically.
- `test_turn_finished_cursor_in_provider_error_path` — provider error after some committed messages: emitted `TurnFinished` carries the run-entry `prior_message_count`, not the post-error message count.
- `test_turn_finished_cursor_in_interrupted_path` — interrupt after at least one tool cycle: `prior_message_count` matches run entry, not the local `prior_messages.len()` from `build_interrupted_messages`.
- `test_turn_finished_cursor_with_empty_setup_failure` — setup failure before any work: `prior_message_count == initial messages.len()`.

## 2.4 — Persistence: checkpoint-batched, atomic contract

**File**: `crates/zdx-engine/src/core/thread_persistence.rs`

This is the most invasive change. Persistence stops emitting any `ThreadEvent` from streaming `AgentEvent`s and instead writes ordered batches on `TurnCheckpoint` (after each tool turn) and `TurnFinished` (terminal). **All of `ToolUse`, `ToolResult`, `Reasoning`, and text persistence change atomically** — no half-state.

**Atomic-commit rule (per oracle's X)**: within 2.4, add the new checkpoint/`TurnFinished` write-path AND remove the streaming write branches in the **same commit**. Tests `test_streaming_events_no_longer_persisted` and `test_persistence_round_trip_preserves_order` guard against duplicates and missed events at the boundary.

### Schema changes
- `ThreadEvent::Message` extends with `replay`:
  ```text
  Message {
      role: String,
      text: String,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      phase: Option<String>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      replay: Option<ReplayToken>,
      ts: String,
  }
  ```
- `ThreadEvent::ToolUse` extends with `id_origin` and `replay`:
  ```text
  ToolUse {
      id: String,
      name: String,
      input: Value,
      #[serde(default)]
      id_origin: IdOrigin,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      replay: Option<ReplayToken>,
      ts: String,
  }
  ```
- `ThreadEvent::Reasoning` already has `replay` — no schema change.
- `ThreadEvent::ToolResult` unchanged.

### Write-path rewrite
- **Delete** the streaming branches in `ThreadEvent::from_agent` (`thread_persistence.rs:333-365`):
  - `AgentEvent::ToolInputCompleted` no longer produces a `ThreadEvent`.
  - `AgentEvent::ToolCompleted` no longer produces a `ThreadEvent`.
  - `AgentEvent::ReasoningCompleted` no longer produces a `ThreadEvent`.
  - These remain as live UI events consumed by the TUI (`crates/zdx-tui/src/features/transcript/update.rs:80-168`).
- **Add** `TurnCheckpoint` and `TurnFinished` handlers that share an idempotent write helper. Persistence consumer (`UsagePersistor` at `thread_persistence.rs:991-1021`) gains `last_persisted_index: usize` initialized to `0`. Rehydrated history is **not** re-persisted because `prior_message_count` (passed by the agent at run entry) covers the historical prefix:
  ```text
  fn flush_messages(&mut self, full: &[ChatMessage], prior_count: usize) {
      let start = std::cmp::max(prior_count, self.last_persisted_index);
      for msg in &full[start..] {
          // emit ThreadEvents for each block in order (see below)
      }
      self.last_persisted_index = full.len();
  }
  ```
  - On `TurnCheckpoint`: call `flush_messages(&messages, prior_message_count)`.
  - On `TurnFinished`: call `flush_messages(&messages, prior_message_count)`, then forward usage aggregation from `UsageUpdate` (no change to existing usage path).
  - **`prior_count < last_persisted_index` is expected** after the first checkpoint — the second checkpoint or terminal `TurnFinished` arrives with the same `prior_message_count` (the run-entry value) but `last_persisted_index` is already past it. Do **not** add `debug_assert!(prior_count >= last_persisted)` — it would fire after every successful checkpoint. The `max()` is the correct pattern. Optionally: `debug_assert!(start <= full.len())`.
  - Idempotent: a checkpoint flushing `0..3` followed by `TurnFinished` carrying messages `0..5` flushes only `3..5`, no duplicates.
- For each `ChatMessage` in the new suffix, handle **both** `MessageContent` variants (per oracle's AA — the plan must not assume only `Blocks`):
  - `MessageContent::Text(text)` → emit one `ThreadEvent::Message { role, text, phase, replay: None, ... }`. Used by `ChatMessage::user(text)` and `ChatMessage::assistant_text(...)` paths.
  - `MessageContent::Blocks(blocks)` → walk blocks in order:
    - `ChatContentBlock::Text { text, replay }` → `ThreadEvent::Message { role, text, replay, ... }` (one per text block, preserving order)
    - `ChatContentBlock::ToolUse { id, name, input, id_origin, replay }` → `ThreadEvent::ToolUse { ... }`
    - `ChatContentBlock::Reasoning(block)` → `ThreadEvent::Reasoning { text, replay, ... }`
    - `ChatContentBlock::ToolResult(result)` → `ThreadEvent::ToolResult { ... }` (existing handling)
    - `ChatContentBlock::Image { ... }` → existing handling

### Remove duplicate persistence paths (per oracle's AA/CC)

The existing terminal-text and partial-content paths must be removed in the same commit as the new flush helper, otherwise terminal assistant text will be persisted twice (once from `flush_messages`, once from the old paths):

- `persist_completed_messages` final-text branch (`thread_persistence.rs:1064-1072`) — **delete or disable** for `TurnFinished::Completed`. Final text is now persisted as part of the message blocks.
- `ThreadEvent::Interrupted` partial-content emission (`thread_persistence.rs:342-347`) — **suppress** the `partial_content` field for new batched flushes. The interrupted path's partial assistant blocks are now in `messages[..]` and flushed by `flush_messages`. The `Interrupted` event itself can stay as a marker, but without duplicating the text payload.

### Read-path rewrite (per oracle's U — replace, don't delete)
- Replace `pending_reasoning` and `pending_tool_uses` (which bucketed by category) with a single ordered `pending_assistant_blocks: Vec<ChatContentBlock>` buffer that preserves arrival order.
- Preserve adjacent-tool-result grouping: keep `pending_tool_results: Vec<ToolResult>` (tool-result events still need to be coalesced into a single user message), but tool-results coalescing is independent of assistant-side ordering.
- `take_assistant_blocks` (`thread_persistence.rs:2026-2034`) drains `pending_assistant_blocks` in arrival order — events from disk are now in original part order, so this is a no-op reorder.

### Tests to update
- Existing tests at `thread_persistence.rs:2654-2798, 3339-3417` assume bucketed order — update to assert original-emission order.
- Tests that drove the streaming-event persistence path (search for `AgentEvent::ToolInputCompleted` and `AgentEvent::ReasoningCompleted` in tests) must be rewritten to drive `AgentEvent::TurnCheckpoint` / `AgentEvent::TurnFinished` with constructed `messages` vecs.

### Test gate
- `test_persistence_round_trip_preserves_order` — `TurnFinished` with `[reasoning, text, tool_use, text, tool_use]` ordered blocks → JSONL on disk → rehydrate → same order.
- `test_persistence_carries_per_part_signatures` — text and tool_use signatures survive round-trip.
- `test_persistence_carries_id_origin` — `ToolUse` `id_origin` survives.
- `test_old_transcript_loads_with_synthesized_default` — fixture with old shape rehydrates with `id_origin: Synthesized`, `replay: None`.
- `test_streaming_events_no_longer_persisted` — `AgentEvent::ToolInputCompleted` followed by `AgentEvent::TurnFinished` produces exactly one `ThreadEvent::ToolUse` (from the batched flush), not two.
- `test_tool_result_persisted_with_tool_use_in_order` — `TurnFinished` containing assistant→tool_use→user→tool_result produces ThreadEvents in that order.
- `test_checkpoint_then_turn_finished_idempotent` — `TurnCheckpoint` flushes messages 0..3, then `TurnFinished` with messages 0..5 flushes only 3..5 (no re-write of 0..3).
- `test_checkpoint_persistence_survives_crash_simulation` — emit `TurnCheckpoint` after tool turn 1, drop the consumer (simulating crash), reload from disk; messages from tool turn 1 are present, in-flight tool turn 2 is not.

## 2.5 — Request builder

**File**: `crates/zdx-providers/src/gemini/shared.rs`

- `append_assistant_blocks` (`shared.rs:268-318`) reads signature from each block's own `replay` field; never scans for a leading `ReasoningBlock`. Walks blocks in order.
- For `Text { replay: Some(ReplayToken::Gemini { signature, model }) }`:
  - Attach `thoughtSignature` only when `(model.is_empty() || model == self.model)` AND `is_valid_thought_signature(&signature)`.
  - Empty-model permissive gate is the migration safety net.
- For `ToolUse { id, id_origin, replay }`:
  - Attach `thoughtSignature` from `replay` under same gate.
  - If no valid real signature AND `is_gemini_3(self.model)`, fall back to `SYNTHETIC_THOUGHT_SIGNATURE`. Otherwise emit no `thoughtSignature`.
  - **Emit `"id": id` only when `id_origin == IdOrigin::Real`**. Track `tool_use_id → id_origin` in `GeminiContentsBuilder` (extend `tool_name_map` at `shared.rs:240` to `tool_meta_map: HashMap<String, ToolMeta>` carrying `{ name, id_origin }`).
- For `Reasoning(ReasoningBlock { replay: Some(ReplayToken::Gemini { signature, model }), .. })`:
  - Re-emit `{ "thought": true, "text": ..., "thoughtSignature": signature }` only when same gate passes AND signature valid.
  - On mismatch or invalid: drop the block.
- `append_user_blocks` functionResponse builder (`shared.rs:343-350`):
  - Look up `tool_use_id` in the builder's `tool_meta_map`.
  - **Emit `"id": tool_use_id` only when matching `IdOrigin::Real`**. Otherwise omit. Closes the asymmetry oracle flagged.
- Add `is_valid_thought_signature(sig: &str) -> bool` (length % 4 == 0, base64 charset). Treat `SYNTHETIC_THOUGHT_SIGNATURE` as an explicit exception.
- Add `is_gemini_3(model: &str) -> bool` next to `capabilities_for_model`.
- Delete `gemini_signature(blocks: &[ChatContentBlock]) -> Option<String>` (`shared.rs:380-396`).

### Test gate (per-section)
- `test_text_part_carries_own_signature`
- `test_tool_use_carries_own_signature`
- `test_signature_not_moved_across_parts`
- `test_synthetic_only_on_gemini_3_function_call`
- `test_synthetic_never_on_text_parts`
- `test_invalid_base64_signature_dropped`
- `test_synthesized_id_omitted_on_function_call`
- `test_synthesized_id_omitted_on_function_response` (the symmetry test)
- `test_real_id_preserved_on_both`
- `test_thought_reemitted_on_same_model`
- `test_thought_dropped_on_different_model`
- `test_empty_model_replays_as_same_model` (migration permissiveness)

### Cross-provider regression tests
Cross-provider regression tests (Anthropic, OpenAI Responses, Chat Completions) for the `Text` variant migration ran early at gate 2.1b. By 2.5 they should still be passing; if anything in 2.5 touches shared types, re-run them as a smoke check.

## 2.6 — Final pipeline

The slice 1 golden test now passes. No code changes; just verification that 2.1–2.5 compose correctly.

## Risk
- High. Cross-cutting change to `ChatContentBlock`, `ReplayToken`, `StreamEvent`, `AssistantTurnBuilder`, `AgentEvent::TurnFinished`, `ThreadEvent` schema and write/read paths, three provider request builders, plus 26 call sites for the `Text` variant migration.
- Mitigation: each gate has its own test; gates 2.1–2.5 must all pass before the golden 2.6 is run. Crash-safety tradeoff is documented and accepted.

---

# Slice 3 — Cleanup (audit, no expected deletions)

**Demoable change**: `rg "gemini_signature\(|pending_signature|signature_from_function_call|pending_reasoning|pending_tool_uses|pending_tool_results" crates/` returns nothing.

By design, slice 2 deletes everything atomically (per oracle's P). Slice 3 is now an **audit** that confirms no dead paths remain:
- `rg` checks above.
- `cargo +nightly udeps --workspace` if available.
- Inline doc comments referencing old behavior are updated.

If anything is still alive after slice 2, this slice catches and removes it.

## Risk
Low.

---

# Slice 4 — Verification

**Demoable change**: `just ci` green. Manual smoke artifact captured.

## Automated
- `just ci` (full lint + test).
- Slice 1 golden test passes.
- All updated persistence tests pass.
- Cross-provider regression tests from 2.5 pass.

## Manual smoke (caching)
- Use Gemini 3 Pro Preview (implicit-cache floor 4096 tokens).
- Multi-turn coding session with system prompt + tools loaded such that turn-1 prompt > 4096 tokens.
- Enable debug metrics; observe `cachedContentTokenCount` and `cacheTokensDetails`:
  - **Pass**: `cachedContentTokenCount > 0` on turn 2 and growing on turn 3.
  - **Fail**: iterate.
- Capture metrics output for the PR.

## Implicit-cache floors (reference)
| Model | Min tokens for implicit cache |
|---|---|
| Gemini 3 Pro Preview | 4096 |
| Gemini 3 Flash Preview | 1024 |
| Gemini 2.5 Pro | 4096 |
| Gemini 2.5 Flash | 1024 |

## Escalation triggers (out-of-scope investigation only if hit)
- API-key Gemini hits cache but Cloud Code Assist OAuth path doesn't → investigate `user_prompt_id` (`gemini/shared.rs:561-568`).
- Cache hits on first followup but misses on third → investigate `serde_json::Map` ordering and `parametersJsonSchema` vs `parameters`.

---

# Out-of-scope reminders
- `toolUsePromptTokenCount` math change.
- Explicit `cachedContents.create` API.
- Cloud Code Assist `user_prompt_id` rework.
- `parametersJsonSchema` migration.
- Model-family-permissive replay.
- Per-streaming-event durability (replaced by `TurnCheckpoint`-batched flush after each tool turn — see `docs/SPEC.md` "Threads / Durability"; an in-flight tool turn is the unit at risk on a hard crash).
