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

### Slice 5: Session persistence + API reconstruction ✅

**Goal:** Thinking persists in session JSONL, replays correctly, API messages properly grouped

**Scope checklist:**
- [x] Add `SessionEvent::Thinking` variant:
  ```rust
  Thinking {
      content: String,
      signature: Option<String>,
      ts: String,
  }
  ```
- [x] Update `SessionEvent::from_engine()` to convert `ThinkingFinal`
- [x] Update session loading to reconstruct thinking cells
- [x] Update `ChatMessage` / `ApiMessage` serialization:
  - Add `ApiContentBlock::Thinking` variant (already existed)
  - Serialize with signature when present
  - **If signature missing (aborted):** convert to text block:
    ```rust
    ApiContentBlock::Text {
        text: format!("<thinking>\n{}\n</thinking>", thinking_content),
        cache_control: None,
    }
    ```
- [x] Update `ApiMessage::from_chat_message()` to handle thinking blocks (already worked, added aborted handling)
- [x] Verify turn grouping: thinking + text + tool_use → single assistant message (already works via `assistant_blocks`)

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

1. **Thinking disabled by default**: `thinking_level = off` is default; no API changes when disabled
2. **Token limit safety**: `effective_max_tokens() >= thinking_budget_tokens + 1` always enforced
3. **Signature preservation**: aborted thinking blocks convert to text blocks to avoid API rejection
4. **Turn grouping**: thinking + text + tool_use from same turn → single assistant message to API
5. **Event ordering**: thinking events emitted before text events within a turn (matches API order)
6. **Session schema**: add `thinking` event type; older sessions remain readable (forward compatible)
7. **Thinking level display**: when thinking enabled, level shown in model title bar (Phase 2)
8. **Level persistence**: thinking level persists to config file immediately on change (Phase 2)
9. **Config migration**: old `thinking_enabled` + `thinking_budget_tokens` format auto-migrates (Phase 2)

---

## Key decisions (decide early)

| Decision | Options | Recommendation | Rationale |
|----------|---------|----------------|-----------|
| Thinking cell vs Assistant variant | Separate cell OR flag on Assistant | **Separate cell** | Cleaner state, distinct rendering, simpler pattern matching |
| Signature storage | Store in cell OR derive from API | **Store in cell** | Needed for session resume and API reconstruction |
| Missing signature handling | Error OR convert to text | **Convert to text** | Matches pi-mono pattern, API compatible |
| Turn association | Implicit (adjacent) OR explicit (turn_id) | **Implicit** | Engine already groups by turn; no need for explicit ID |
| Max tokens adjustment | Auto-adjust OR error | **Auto-adjust with log** | Better UX, user can still override explicitly |

### Phase 2 key decisions

| Decision | Options | Recommendation | Rationale |
|----------|---------|----------------|-----------|
| Budget representation | Exact tokens OR named levels | **Named levels** | Simpler UX, covers 90% of use cases, maps to common patterns |
| Level names | off/low/med/high OR off/minimal/low/med/high | **5 levels** | Minimal (~1k) useful for simple tasks, more granularity |
| Config migration | Break old format OR auto-migrate | **Auto-migrate** | No friction for existing users |
| Picker keybinding | Ctrl+T OR Ctrl+Shift+T | **Ctrl+T** | Available, memorable (T for Thinking) |
| Level indicator style | Full name OR abbreviated | **Abbreviated** | `[💭med]` fits better, model names can be long |
| Indicator position | After model OR after auth | **After auth** | Logical grouping: model + auth + thinking config |

---

## Testing

### Per-slice smoke tests
- **Slice 1:** Config loads with new fields, effective_max_tokens() returns safe value ✅
- **Slice 2:** SSE fixture test parses thinking events ✅
- **Slice 3:** Engine emits ThinkingDelta/ThinkingFinal events ✅
- **Slice 4:** TUI displays thinking cells with distinct style ✅
- **Slice 5:** Session round-trip preserves thinking, resume works ✅
- **Slice 6:** ThinkingLevel enum maps to correct budget values ✅
- **Slice 7:** Model title shows thinking indicator when enabled ✅
- **Slice 8:** Thinking picker opens/closes, selection updates state
- **Slice 9:** Ctrl+T and /thinking command both open picker
- **Slice 10:** Thinking level persists to config file, survives restart

### Regression tests (protect contracts)
- [x] `test_sse_parser_thinking_response` — fixture test for thinking stream
- [x] `test_effective_max_tokens_with_thinking` — token limit safety
- [x] `test_session_thinking_roundtrip` — thinking persists and loads
- [x] `test_aborted_thinking_converts_to_text` — signature fallback
- [x] `test_thinking_level_budget_mapping` — each level maps to expected tokens
- [ ] `test_save_thinking_level_preserves_config` — toml_edit preserves comments

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

## Phase 2: Runtime Thinking Control

### Slice 6: Thinking level enum + config mapping ✅

**Goal:** Replace boolean `thinking_enabled` with a thinking level that maps to budget tokens

**Scope checklist:**
- [x] Define `ThinkingLevel` enum in `config.rs`:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
  #[serde(rename_all = "lowercase")]
  pub enum ThinkingLevel {
      #[default]
      Off,      // No reasoning
      Minimal,  // Very brief reasoning (~1k tokens)
      Low,      // Light reasoning (~2k tokens)
      Medium,   // Moderate reasoning (~8k tokens)
      High,     // Deep reasoning (~16k tokens)
  }
  ```
- [x] Add `ThinkingLevel::budget_tokens(&self) -> Option<u32>` method:
  - `Off` → `None` (disabled)
  - `Minimal` → `Some(1024)`
  - `Low` → `Some(2048)`
  - `Medium` → `Some(8192)`
  - `High` → `Some(16384)`
- [x] Add `ThinkingLevel::description(&self) -> &'static str` method:
  - `Off` → "No reasoning"
  - `Minimal` → "Very brief reasoning (~1k tokens)"
  - `Low` → "Light reasoning (~2k tokens)"
  - `Medium` → "Moderate reasoning (~8k tokens)"
  - `High` → "Deep reasoning (~16k tokens)"
- [x] Add `ThinkingLevel::display_name(&self) -> &'static str` method (short: "off", "minimal", etc.)
- [x] Add `ThinkingLevel::all() -> &'static [ThinkingLevel]` for picker
- [x] Replace `thinking_enabled: bool` + `thinking_budget_tokens: u32` with `thinking_level: ThinkingLevel`
- [x] Update `effective_max_tokens()` to use `thinking_level.budget_tokens()`
- [x] Update `default_config.toml` template with `thinking_level`
- [x] Update `engine.rs` to translate `ThinkingLevel` to raw API values

**Demo:**
```bash
# Edit config: thinking_level = "medium"
cargo run
# Verify thinking works with ~8000 token budget
```

**Risks / failure modes:**
- Config migration for existing users → serde `#[serde(alias)]` or custom deserialize

---

### Slice 7: Show thinking level in model title ✅

**Goal:** Display current thinking level next to model name in status bar

**Scope checklist:**
- [x] Update `render_input()` in `view.rs` to include thinking level:
  ```rust
  // Format: " claude-sonnet-4 (api-key) [💭medium] "
  if state.config.thinking_level != ThinkingLevel::Off {
      title_spans.push(Span::styled(
          format!(" [💭{}]", state.config.thinking_level.display_name()),
          thinking_style,
      ));
  }
  ```
- [x] Use dim style for thinking indicator to distinguish from model name
- [x] Keep indicator compact: `[💭medium]` or `[💭high]`

**Demo:**
```bash
cargo run
# See "[💭medium]" after model name in input border
# Change config to thinking_level = "off"
# Restart - indicator disappears
```

**Risks / failure modes:**
- Long model names overflow border → truncate model name, not indicator

---

### Slice 8: Thinking level picker overlay

**Goal:** User can change thinking level via overlay (like model picker)

**Scope checklist:**
- [ ] Create `src/ui/overlays/thinking_picker.rs`:
  - [ ] `ThinkingPickerState { selected: usize }` struct
  - [ ] `ThinkingPickerState::new(current: ThinkingLevel)` - select current level
  - [ ] `open_thinking_picker(state)` - opens overlay
  - [ ] `close_thinking_picker(state)` - closes overlay
  - [ ] `handle_thinking_picker_key(state, key)` - up/down/enter/esc
  - [ ] `render_thinking_picker(frame, picker, area, input_top_y)` - render list with:
    - Level name (left column, cyan)
    - Description with token count (right column, dimmed)
- [ ] Add `OverlayState::ThinkingPicker(ThinkingPickerState)` variant
- [ ] Add accessor methods: `as_thinking_picker()`, `as_thinking_picker_mut()`
- [ ] Wire up in `update.rs`:
  - Handle key events when ThinkingPicker overlay active
  - On selection: update `state.config.thinking_level`, emit `PersistThinking` effect
- [ ] Wire up in `view.rs`: render thinking picker when active
- [ ] Add `UiEffect::PersistThinking { level: ThinkingLevel }` effect
- [ ] Implement effect in `tui.rs`: call `Config::save_thinking_level()`

**Demo:**
```bash
cargo run
# Press Ctrl+T (or /thinking command)
# See thinking level picker with Off/Minimal/Low/Medium/High
# Select "High" → config persisted, indicator updates to [💭high]
```

**Risks / failure modes:**
- Overlay positioning conflicts with model picker → same centering logic, exclusive

---

### Slice 9: Keybinding + slash command for thinking picker

**Goal:** User can open thinking picker via keyboard shortcut or command

**Scope checklist:**
- [ ] Add `Ctrl+T` keybinding in `update.rs` to open thinking picker
- [ ] Add `/thinking` slash command in `commands.rs`:
  ```rust
  SlashCommand {
      name: "thinking",
      aliases: &["think", "t"],
      description: "Change thinking level",
      action: CommandAction::OpenThinkingPicker,
  }
  ```
- [ ] Add `CommandAction::OpenThinkingPicker` variant
- [ ] Handle action in command execution to open overlay
- [ ] Add system message on level change: "Thinking level set to {level}"

**Demo:**
```bash
cargo run
# Press Ctrl+T → thinking picker opens
# Or type /thinking → picker opens
# Select level → "Thinking level set to high" appears in transcript
```

---

### Slice 10: Config persistence for thinking level

**Goal:** Thinking level persists to config file like model selection

**Scope checklist:**
- [ ] Add `Config::save_thinking_level(level: ThinkingLevel)` method
- [ ] Add `Config::save_thinking_level_to(path, level)` for testability
- [ ] Use `toml_edit` to preserve comments (same pattern as `save_model`)
- [ ] Update runtime effect handler to call `save_thinking_level()`
- [ ] Add integration test: change level via picker, restart, verify persisted

**Demo:**
```bash
cargo run
# Change thinking level via picker
# Quit and restart
# Verify thinking level indicator shows persisted value
```

---

## Updated Slice Dependency Graph

```
Slice 1-5 (MVP - Complete) ✅
    │
    ▼
Slice 6 (Thinking level enum) ✅
    │
    ├─────────────────┐
    ▼                 ▼
Slice 7 (Model title) Slice 8 (Picker overlay)
                      │
                      ▼
                  Slice 9 (Keybinding + command)
                      │
                      ▼
                  Slice 10 (Config persistence)
```

Slices 7 and 8 can be developed in parallel after Slice 6.

---

## Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| ~~Thinking budget in UI (model picker)~~ | ✅ Replaced by thinking level picker (Slice 8) |
| Per-prompt thinking toggle | User feedback on needing per-turn control |
| Thinking for other providers | When adding provider that supports extended thinking |
| Thinking summarization | User feedback on long thinking being noisy |
| Streaming thinking collapse | UX research on optimal display during stream |
| Custom budget in picker | User feedback on needing fine-grained control beyond levels |

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
