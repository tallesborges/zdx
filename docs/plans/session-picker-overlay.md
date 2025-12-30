# Session Picker Overlay Implementation Plan

> Ship-first plan for adding a session picker overlay to the TUI.

## Goals
- User can open a session picker overlay to browse saved sessions
- User can navigate sessions with arrow keys (with scroll support for long lists)
- User can select a session to switch to it (loads that session's full history including tools/thinking)
- User can dismiss the overlay with Esc (no change)

## Non-goals
- Live preview while navigating (deferred to polish)
- Session deletion from overlay
- Session search/filtering
- Session renaming

## Design principles
- **User journey drives order**: Get basic list → navigate → select working first
- **Leverage existing patterns**: Follow model_picker.rs structure exactly
- **Ship ugly but functional**: Basic session list with ID + timestamp before any preview
- **Reducer purity**: All I/O happens in effect handlers, never in reducer

## User journey
1. User is in chat mode (existing or fresh session)
2. User opens session picker (via command palette or keyboard shortcut)
3. User sees list of sessions (most recent first) with ID + timestamp
4. User navigates with ↑/↓ keys (list scrolls if needed)
5. User presses Enter to select → session loads with full history, overlay closes
6. OR User presses Esc → overlay closes, no change

## Foundations / Already shipped (✅)

### Session persistence
- What exists: `Session`, `list_sessions()`, `load_session()`, `SessionSummary`, `SessionEvent`
- ✅ Demo: `cargo run -- sessions list` shows sessions
- Gaps: None

### Overlay system
- What exists: `OverlayState` enum, `model_picker.rs` as template, render patterns
- ✅ Demo: Open command palette with `/`, close with Esc
- Gaps: None

### Command palette commands
- What exists: Command registration in `commands.rs`, palette dispatch
- ✅ Demo: Type `/model` to see model picker command
- Gaps: Need to add "Sessions" command

---

## MVP Slices (ship-shaped, demoable)

### Slice 1: Session picker state + render (with effect-driven loading)

**Goal**: Show a scrollable list of sessions in an overlay, loaded via effect handler

**Scope checklist**:
- [ ] Add `SessionPickerState` struct in new `src/ui/chat/overlays/session_picker.rs`:
  ```rust
  pub struct SessionPickerState {
      pub sessions: Vec<SessionSummary>,
      pub selected: usize,
      pub offset: usize,  // scroll offset for long lists
  }
  ```
- [ ] Add `SessionPicker(SessionPickerState)` variant to `OverlayState` enum
- [ ] Add `as_session_picker()` and `as_session_picker_mut()` helpers to `OverlayState`
- [ ] Add `UiEffect::OpenSessionPicker` variant
- [ ] Implement effect handler in `TuiRuntime::execute_effect`:
  - Call `list_sessions()` (I/O happens here, not in reducer)
  - If empty, add system message "No sessions found" and don't open overlay
  - If error, add system message with error and don't open overlay
  - Otherwise, create `SessionPickerState` and set overlay
- [ ] Add `render_session_picker()` that displays sessions (ID truncated + timestamp)
  - Handle scroll offset for visible window
  - Show "No sessions" if list is empty (shouldn't happen due to effect handler)
- [ ] Add `close_session_picker()` function
- [ ] Wire up Esc key to close in `handle_session_picker_key()`
- [ ] Export from `overlays/mod.rs`
- [ ] Add render call in `view.rs` overlay match

**✅ Demo**: Trigger `UiEffect::OpenSessionPicker` from reducer, see list appear, press Esc to close

**Risks / failure modes**:
- `list_sessions()` is blocking I/O — acceptable since it's fast (dir read)
- Effect handler pattern is more complex but keeps reducer pure

---

### Slice 2: Navigation (↑/↓ with scroll)

**Goal**: User can move selection up/down with automatic scroll offset management

**Scope checklist**:
- [ ] Implement Up/Down key handling in `handle_session_picker_key()`
- [ ] Bounds clamping (don't go negative, don't exceed `sessions.len() - 1`)
- [ ] Scroll offset management:
  - If selected goes above visible window, decrease offset
  - If selected goes below visible window, increase offset
- [ ] Render highlight style on selected item (match model_picker pattern)
- [ ] Add keyboard hints at bottom (↑↓ navigate • Enter select • Esc cancel)

**✅ Demo**: Open picker with 10+ sessions, press ↓ repeatedly, see list scroll, selection stays visible

**Risks / failure modes**:
- Need to calculate visible height from render area — pass through or hardcode reasonable default

---

### Slice 3: Selection (Enter to switch with full state reset)

**Goal**: User can select a session and the conversation switches to it with full fidelity

**Scope checklist**:
- [ ] Add `build_transcript_from_events()` helper to `TuiState` or `TranscriptState`:
  ```rust
  fn build_transcript_from_events(events: &[SessionEvent]) -> Vec<HistoryCell>
  ```
  - Map `SessionEvent::Message` → `HistoryCell::User` or `HistoryCell::Assistant`
  - Map `SessionEvent::ToolUse` + `SessionEvent::ToolResult` → `HistoryCell::Tool`
  - Map `SessionEvent::Thinking` → `HistoryCell::Thinking`
  - Skip `SessionEvent::Meta`, `SessionEvent::Interrupted`
- [ ] Add `UiEffect::LoadSession { session_id: String }` variant
- [ ] On Enter key in `handle_session_picker_key()`:
  - If agent is running, show system message "Stop the current task first" and return (no switch)
  - Otherwise emit `LoadSession` effect with selected session ID
- [ ] Implement effect handler in `TuiRuntime::execute_effect`:
  - Load events via `session::load_session(id)`
  - Build transcript via `build_transcript_from_events()`
  - Build messages via `session::events_to_messages()` (for API context)
  - Build input history from user messages
  - Reset state facets:
    - `conversation.session` = new session
    - `conversation.messages` = loaded messages
    - `conversation.usage` = reset to new
    - `transcript.cells` = loaded transcript
    - `transcript.scroll.reset()` (or set to FollowLatest)
    - `transcript.wrap_cache.clear()`
    - `input.history` = loaded command history
  - Add system message "Switched to session {id}"
  - Close overlay
- [ ] Handle load errors gracefully (system message, don't crash)

**✅ Demo**: Open picker, select different session, see transcript change with tools/thinking preserved

**Risks / failure modes**:
- Tool events need pairing (ToolUse + ToolResult) — handle incomplete pairs gracefully
- Large sessions may have slight load delay — acceptable for MVP

---

### Slice 4: Command palette integration

**Goal**: User can open session picker from command palette

**Scope checklist**:
- [ ] Add "Sessions" command to `COMMANDS` in `commands.rs`:
  ```rust
  Command {
      name: "sessions",
      aliases: &["history"],
      description: "Browse and switch sessions",
  }
  ```
- [ ] Handle "sessions" in `execute_command()` — return `vec![UiEffect::OpenSessionPicker]`
- [ ] (Optional) Add direct keyboard shortcut if a good one is available

**✅ Demo**: Type `/ses`, see "Sessions" command, select it, picker opens

**Risks / failure modes**:
- Shortcut conflicts — skip shortcut for MVP, add in polish

---

## Contracts (guardrails)

- Esc always closes overlay without side effects
- Session switch must not corrupt current session file
- Overlay must not block agent streaming (if agent is running, picker still works but Enter is blocked)
- `list_sessions()` errors shown gracefully, not panic
- Switching sessions preserves full history fidelity (tools, thinking blocks)
- All I/O happens in effect handlers, never in reducer

## Key decisions (decide early)

1. **Session list is snapshot on open**: Don't refresh while overlay is open (simplicity)
2. **Loading happens in effect handler**: `load_session()` called in runtime, not reducer
3. **No confirmation dialog**: Enter immediately switches (matches model picker pattern)
4. **Block switch during agent run**: Don't interrupt automatically, require user to stop first
5. **Use SessionEvent for transcript**: Not ChatMessage, to preserve tool/thinking fidelity

## Testing

- Manual smoke demos per slice (as described in ✅ Demo sections)
- Unit tests:
  - `SessionPickerState::new()` with empty list
  - Navigation bounds clamping
  - `build_transcript_from_events()` preserves all cell types
- Integration test (optional):
  - Create session, switch away, switch back, verify content

---

## Polish Phases (after MVP)

### Phase 1: Preview on navigate
- Replace current transcript content while navigating (temporary)
- Store original session state to restore on Esc
- ✅ Check-in: Navigate sessions, see preview, Esc restores original

### Phase 2: Visual improvements
- Show first message preview in picker (truncated)
- Relative date formatting ("2 hours ago" vs timestamp)
- Session count in title bar
- Current session indicator (• or highlight)
- ✅ Check-in: Picker shows richer information

### Phase 3: Quick access shortcut
- Add dedicated shortcut (e.g., Ctrl+H for history)
- ✅ Check-in: Single keypress opens picker

### Phase 4: Session metadata
- Show message count per session
- Show token usage summary
- ✅ Check-in: Picker helps user choose based on context

---

## Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Session deletion | Users request cleanup capability |
| Session search/filter | Session count becomes pain point (>50) |
| Session renaming | UUIDs prove too confusing |
| Live preview | MVP feedback requests it |
| Async session loading | Large sessions cause noticeable delay |

---

## File Changes Summary

| File | Changes |
|------|---------|
| `src/ui/chat/overlays/session_picker.rs` | **New file** - state, handlers, render |
| `src/ui/chat/overlays/mod.rs` | Add module + re-exports |
| `src/ui/chat/state/mod.rs` | Add `SessionPicker` variant + accessors to `OverlayState` |
| `src/ui/chat/effects.rs` | Add `OpenSessionPicker`, `LoadSession` variants |
| `src/ui/chat/mod.rs` | Implement effect handlers |
| `src/ui/chat/reducer.rs` | Wire up key handling dispatch |
| `src/ui/chat/view.rs` | Add render call for session picker |
| `src/ui/chat/commands.rs` | Add "sessions" command |
| `src/ui/chat/state/transcript.rs` | Add `build_transcript_from_events()` helper |

---

## Review Notes

Plan reviewed by Gemini and Codex. Key feedback incorporated:
- ✅ Effect-driven I/O (not in reducer)
- ✅ Full state reset on session switch
- ✅ Transcript fidelity via SessionEvent mapping
- ✅ Scroll offset for long lists
- ✅ Empty list / error handling
- ✅ Block switch during agent run
