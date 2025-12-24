# Thinking Feature Implementation Plan

**Status:** Ready for implementation  
**Validated by:** Gemini 3 Pro, Codex gpt-5.2  
**Created:** 2024-12-22

---

## Inputs

- **Project/feature:** Add thinking (extended thinking) support to zdx-cli, following pi-mono's implementation pattern
- **Existing state:** Anthropic provider with streaming, events system (`AssistantDelta`/`AssistantFinal`), transcript model (User/Assistant/Tool/System cells), TUI renderer, beta headers already include `interleaved-thinking-2025-05-14`
- **Constraints:** Rust/Anthropic API only, must preserve existing streaming behavior, thinking must work with tool loop
- **Success looks like:** User can enable thinking via config, see thinking blocks stream in TUI, thinking persists in sessions

---

## Goals

- User can enable thinking via config option
- Thinking blocks stream to TUI with distinct visual treatment
- Thinking blocks persist in sessions and replay correctly
- Thinking signature preserved for API continuity (required for conversation history)

## Non-goals

- Thinking budget exposed in UI (config-only)
- Real-time thinking summary/collapse UX
- Thinking for non-Anthropic providers

## Design principles

- **User journey drives order**: wire end-to-end first, then polish display
- **Ugly but functional**: stream raw thinking text before styling
- **Engine/UI separation**: engine emits thinking events; TUI renders them
- **Turn-grouped content**: thinking + text belong to same assistant turn for API compatibility

## User journey

1. User sets `thinking_enabled = true` in config
2. User sends a prompt in TUI
3. User sees thinking stream (visually distinct from response)
4. Thinking completes, response streams normally
5. Session persists thinking blocks; resume replays them correctly

---

## Foundations / Already shipped (✅)

| What exists | Location | Gaps |
|-------------|----------|------|
| SSE streaming parser | `anthropic.rs:520+` | No `thinking_delta`/`signature_delta` parsing |
| StreamEvent enum | `anthropic.rs:431` | No thinking variants |
| SseDelta struct | `anthropic.rs:660` | No `thinking`/`signature` fields |
| EngineEvent enum | `events.rs:16` | No `ThinkingDelta`/`ThinkingFinal` |
| Engine content block tracking | `engine.rs:246+` | Works by index ✓ — reuse for thinking |
| Turn grouping | `engine.rs:336` | Already groups blocks into single message ✓ |
| HistoryCell enum | `transcript.rs:70` | No `Thinking` cell variant |
| UI event handler | `update.rs:398+` | No thinking event handling |
| Config struct | `config.rs:51` | No `thinking_enabled` / `thinking_budget_tokens` |
| Beta headers | `anthropic.rs:31-34` | Already includes `interleaved-thinking-2025-05-14` ✓ |
| Default max_tokens | `config.rs` | 1024 — too low for thinking (API requires max_tokens > budget_tokens) |

---

## MVP slices (ship-shaped, demoable)

### Slice 1: Config options + validation ✅

**Goal:** User can set thinking config with safe defaults

**Scope checklist:**
- [x] Add `thinking_enabled: bool` to Config (default: false)
- [x] Add `thinking_budget_tokens: u32` to Config (default: 8000)
- [x] Add to `default_config.toml` template with comments
- [x] Add `effective_max_tokens(&self) -> u32` method:
  - When thinking enabled: return `max(max_tokens, thinking_budget_tokens + 4096)`
  - When disabled: return `max_tokens`
- [x] Log info message when max_tokens is auto-adjusted (via eprintln)

**✅ Demo:**
```bash
cargo run -- config path && cat ~/.config/zdx/config.toml
# Shows thinking_enabled and thinking_budget_tokens options

# Set thinking_enabled = true, run zdx
# Verify no API errors about token limits
```

**Risks / failure modes:**
- Config migration: existing configs without field → serde `#[serde(default)]` handles this
- User confusion if max_tokens auto-adjusted → log info message when adjusted

---

### Slice 2: API params + SSE parsing ✅

**Goal:** Send thinking params to API, parse thinking events from stream

**Scope checklist:**
- [x] Add `ThinkingConfig` struct for API request:
  ```rust
  #[derive(Serialize)]
  struct ThinkingConfig {
      #[serde(rename = "type")]
      thinking_type: &'static str,  // "enabled"
      budget_tokens: u32,
  }
  ```
- [x] Add `thinking: Option<ThinkingConfig>` to `StreamingMessagesRequest`
- [x] Pass config to `AnthropicClient` and set thinking param when enabled
- [x] Use `effective_max_tokens()` in request (not raw config value)
- [x] Add fields to `SseDelta` struct:
  ```rust
  #[serde(default)]
  thinking: Option<String>,      // for thinking_delta
  #[serde(default)]
  signature: Option<String>,     // for signature_delta
  ```
- [x] Add `StreamEvent` variants:
  ```rust
  ThinkingDelta { index: usize, thinking: String },
  SignatureDelta { index: usize, signature: String },
  ```
  Note: ThinkingBlockStart/Stop use existing ContentBlockStart/Stop with block_type="thinking"
- [x] Update `parse_sse_event()` to handle:
  - `content_block_start` with `type = "thinking"` (existing code works)
  - `content_block_delta` with `delta.type = "thinking_delta"`
  - `content_block_delta` with `delta.type = "signature_delta"`

**✅ Demo:**
```bash
# Add SSE fixture test with thinking response
cargo test -- sse_parser_thinking
# Test passes
```

**Risks / failure modes:**
- API rejects if model doesn't support thinking → let API error bubble (clear error message)

---

### Slice 3: Engine events + turn tracking ✅

**Goal:** Engine emits thinking events, tracks turn content for proper grouping

**Scope checklist:**
- [x] Add to `EngineEvent` enum:
  ```rust
  ThinkingDelta { text: String },
  ThinkingFinal { text: String, signature: String },
  ```
- [x] Add `ThinkingBlock` tracking in engine loop (similar to `ToolUseBuilder`):
  ```rust
  struct ThinkingBuilder {
      index: usize,
      text: String,
      signature: String,
  }
  ```
- [x] Handle `StreamEvent::ThinkingDelta` → accumulate text, emit `EngineEvent::ThinkingDelta`
- [x] Handle `StreamEvent::SignatureDelta` → accumulate signature
- [x] Handle `StreamEvent::ContentBlockStop` for thinking → emit `EngineEvent::ThinkingFinal`
- [x] Add `ChatContentBlock::Thinking { thinking: String, signature: String }` variant
- [x] Include thinking blocks in `assistant_blocks` for `TurnComplete.messages`
- [x] Add stub handlers in TUI update.rs (display deferred to Slice 4)
- [x] Add exec mode handlers in stream.rs (dim text output for thinking)

**✅ Demo:**
```bash
# Run with thinking enabled against real API
RUST_LOG=debug cargo run
# See ThinkingDelta events in logs
# Verify TurnComplete.messages includes thinking content block
```

**Risks / failure modes:**
- Thinking can interleave with text (interleaved thinking) → track by content block index (already works)
- Multiple thinking blocks per turn → accumulate per-index in Vec<ThinkingBuilder>

---

### Slice 4: Transcript cell + TUI display ✅

**Goal:** Thinking streams visibly in TUI

**Scope checklist:**
- [x] Add `HistoryCell::Thinking` variant:
  ```rust
  Thinking {
      id: CellId,
      created_at: DateTime<Utc>,
      content: String,
      signature: Option<String>,  // None while streaming
      is_streaming: bool,
  }
  ```
- [x] Add `HistoryCell::thinking_streaming()` constructor
- [x] Add `HistoryCell::append_thinking_delta()` method
- [x] Add `HistoryCell::finalize_thinking()` method
- [x] Update `handle_engine_event()` in `update.rs`:
  - `ThinkingDelta` → create or append to thinking cell
  - `ThinkingFinal` → finalize cell, store signature
- [x] Render thinking cells with distinct style:
  - Prefix: `💭 ` (or `[thinking]` for non-emoji terminals)
  - Style: dim/italic text (magenta prefix, dark gray content)
- [x] Add `display_lines()` implementation for Thinking cells
- [x] Add `Style::ThinkingPrefix` and `Style::Thinking` variants
- [x] Update `view.rs` to convert thinking styles to ratatui styles

**✅ Demo:**
```bash
cargo run
# Enable thinking in config
# Send a prompt
# See thinking stream with 💭 prefix before response
# Thinking text is visually distinct (dim)
```

**Risks / failure modes:**
- Long thinking overflows → scroll handles this (existing)
- Emoji rendering → fallback to text prefix if needed

---

### Slice 5: Session persistence + API reconstruction

**Goal:** Thinking persists in session JSONL, replays correctly, API messages properly grouped

**Scope checklist:**
- [ ] Add `SessionEvent::Thinking` variant:
  ```rust
  Thinking {
      content: String,
      signature: Option<String>,
      ts: String,
  }
  ```
- [ ] Update `SessionEvent::from_engine()` to convert `ThinkingFinal`
- [ ] Update session loading to reconstruct thinking cells
- [ ] Update `ChatMessage` / `ApiMessage` serialization:
  - Add `ApiContentBlock::Thinking` variant
  - Serialize with signature when present
  - **If signature missing (aborted):** convert to text block:
    ```rust
    ApiContentBlock::Text {
        text: format!("<thinking>\n{}\n</thinking>", thinking_content),
        cache_control: None,
    }
    ```
- [ ] Update `ApiMessage::from_chat_message()` to handle thinking blocks
- [ ] Verify turn grouping: thinking + text + tool_use → single assistant message (already works via `assistant_blocks`)

**✅ Demo:**
```bash
cargo run
# Send thinking prompt → see thinking + response
# Ctrl-C mid-conversation
zdx sessions list
zdx sessions resume <id>
# Transcript shows thinking blocks
# Send follow-up → conversation continues without API error
```

**Risks / failure modes:**
- Aborted thinking (no signature) → fallback to text block (per pi-mono pattern)
- Old sessions without thinking → loads fine, no thinking cells
- Turn grouping wrong → API rejects with "invalid message structure" (mitigated: reuse existing grouping)

---

## Contracts (guardrails)

1. **Thinking disabled by default**: `thinking_enabled = false` is default; no API changes when disabled
2. **Token limit safety**: `effective_max_tokens() >= thinking_budget_tokens + 1` always enforced
3. **Signature preservation**: aborted thinking blocks convert to text blocks to avoid API rejection
4. **Turn grouping**: thinking + text + tool_use from same turn → single assistant message to API
5. **Event ordering**: thinking events emitted before text events within a turn (matches API order)
6. **Session schema**: add `thinking` event type; older sessions remain readable (forward compatible)

---

## Key decisions (decide early)

| Decision | Options | Recommendation | Rationale |
|----------|---------|----------------|-----------|
| Thinking cell vs Assistant variant | Separate cell OR flag on Assistant | **Separate cell** | Cleaner state, distinct rendering, simpler pattern matching |
| Signature storage | Store in cell OR derive from API | **Store in cell** | Needed for session resume and API reconstruction |
| Missing signature handling | Error OR convert to text | **Convert to text** | Matches pi-mono pattern, API compatible |
| Turn association | Implicit (adjacent) OR explicit (turn_id) | **Implicit** | Engine already groups by turn; no need for explicit ID |
| Max tokens adjustment | Auto-adjust OR error | **Auto-adjust with log** | Better UX, user can still override explicitly |

---

## Testing

### Per-slice smoke tests
- **Slice 1:** Config loads with new fields, effective_max_tokens() returns safe value
- **Slice 2:** SSE fixture test parses thinking events
- **Slice 3:** Engine emits ThinkingDelta/ThinkingFinal events
- **Slice 4:** TUI displays thinking cells with distinct style
- **Slice 5:** Session round-trip preserves thinking, resume works

### Regression tests (protect contracts)
- [ ] `test_sse_parser_thinking_response` — fixture test for thinking stream
- [ ] `test_effective_max_tokens_with_thinking` — token limit safety
- [ ] `test_session_thinking_roundtrip` — thinking persists and loads
- [ ] `test_aborted_thinking_converts_to_text` — signature fallback

---

## Slice dependency graph

```
Slice 1 (Config)
    │
    ▼
Slice 2 (API + SSE parsing)
    │
    ▼
Slice 3 (Engine events)
    │
    ▼
Slice 4 (TUI display)
    │
    ▼
Slice 5 (Session persistence)
```

Each slice is independently demoable. Slices 4 and 5 can be developed in parallel after Slice 3.

---

## Polish phases (after MVP)

### Phase 1: Visual polish
- [ ] Collapsible thinking blocks (toggle expand/collapse with keybinding)
- [ ] Thinking token count display in status bar
- [ ] Distinct color theme for thinking text (configurable)
- **✅ Check-in:** thinking blocks visually distinct and scannable

### Phase 2: UX refinements
- [ ] Show thinking duration (time from start to final)
- [ ] Budget usage indicator during streaming
- [ ] Warning when thinking approaches budget limit
- **✅ Check-in:** user can monitor thinking cost/time

---

## Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Thinking budget in UI (model picker) | User feedback on needing runtime control |
| Per-prompt thinking toggle | User feedback on needing per-turn control |
| Thinking for other providers | When adding provider that supports extended thinking |
| Thinking summarization | User feedback on long thinking being noisy |
| Streaming thinking collapse | UX research on optimal display during stream |

---

## Reference: pi-mono implementation

Key patterns from `pi-mono/packages/ai/src/providers/anthropic.ts`:

```typescript
// API params
if (options?.thinkingEnabled && model.reasoning) {
    params.thinking = {
        type: "enabled",
        budget_tokens: options.thinkingBudgetTokens || 1024,
    };
}

// SSE event handling
} else if (event.delta.type === "thinking_delta") {
    block.thinking += event.delta.thinking;
    stream.push({ type: "thinking_delta", ... });
} else if (event.delta.type === "signature_delta") {
    block.thinkingSignature += event.delta.signature;
}

// Aborted thinking fallback
if (!block.thinkingSignature || block.thinkingSignature.trim().length === 0) {
    blocks.push({
        type: "text",
        text: `<thinking>\n${block.thinking}\n</thinking>`,
    });
} else {
    blocks.push({
        type: "thinking",
        thinking: block.thinking,
        signature: block.thinkingSignature,
    });
}
```
