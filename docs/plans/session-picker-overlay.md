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
- ✅ Gaps: None (sessions command added)

---

## MVP Slices (ship-shaped, demoable) ✅ COMPLETE

### Slice 1: Session picker state + render (with effect-driven loading) ✅

**Goal**: Show a scrollable list of sessions in an overlay, loaded via effect handler

**Scope checklist**:
- [x] Add `SessionPickerState` struct in new `src/ui/chat/overlays/session_picker.rs`
- [x] Add `SessionPicker(SessionPickerState)` variant to `OverlayState` enum
- [x] Add `as_session_picker()` and `as_session_picker_mut()` helpers to `OverlayState`
- [x] Add `UiEffect::OpenSessionPicker` variant
- [x] Implement effect handler in `TuiRuntime::execute_effect` (`open_session_picker()` method)
- [x] Add `render_session_picker()` that displays sessions (ID truncated + timestamp)
- [x] Add `close_session_picker()` function
- [x] Wire up Esc key to close in `handle_session_picker_key()`
- [x] Export from `overlays/mod.rs`
- [x] Add render call in `view.rs` overlay match

**✅ Demo**: VERIFIED - Session picker opens, shows sessions, closes with Esc

---

### Slice 2: Navigation (↑/↓ with scroll) ✅

**Goal**: User can move selection up/down with automatic scroll offset management

**Scope checklist**:
- [x] Implement Up/Down key handling in `handle_session_picker_key()` (also j/k vim keys)
- [x] Bounds clamping (don't go negative, don't exceed `sessions.len() - 1`)
- [x] Scroll offset management (via `session_picker_select_prev/next` helpers)
- [x] Render highlight style on selected item (match model_picker pattern)
- [x] Add keyboard hints at bottom (↑↓ navigate • Enter select • Esc cancel)

**✅ Demo**: VERIFIED - Navigation works with scroll offset management

---

### Slice 3: Selection (Enter to switch with full state reset) ✅

**Goal**: User can select a session and the conversation switches to it with full fidelity

**Scope checklist**:
- [x] Add `build_transcript_from_events()` helper (in `mod.rs` module-level function)
  - Maps `SessionEvent::Message` → `HistoryCell::User` or `HistoryCell::Assistant`
  - Maps `SessionEvent::ToolUse` + `SessionEvent::ToolResult` → `HistoryCell::Tool`
  - Maps `SessionEvent::Thinking` → `HistoryCell::Thinking`
  - Skips `SessionEvent::Meta`, `SessionEvent::Interrupted`
- [x] Add `UiEffect::LoadSession { session_id: String }` variant
- [x] On Enter key: block if agent is running, otherwise emit `LoadSession` effect
- [x] Implement effect handler (`load_session()` method in `TuiRuntime`)
  - Loads events, builds transcript, builds messages, builds input history
  - Resets all state facets with loaded data
  - Shows confirmation message
- [x] Handle load errors gracefully (system message, don't crash)

**✅ Demo**: VERIFIED - Session switching works with full transcript fidelity

---

### Slice 4: Command palette integration ✅

**Goal**: User can open session picker from command palette

**Scope checklist**:
- [x] Add "Sessions" command to `COMMANDS` in `commands.rs` (with "history" alias)
- [x] Handle "sessions" in `execute_command()` → returns `vec![UiEffect::OpenSessionPicker]`
- [ ] (Optional) Add direct keyboard shortcut — deferred to polish phase

**✅ Demo**: VERIFIED - `/sessions` or `/history` opens picker

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

## Testing ✅ COMPLETE

- Manual smoke demos per slice (as described in ✅ Demo sections)
- Unit tests implemented:
  - [x] `SessionPickerState::new()` with empty list (`test_session_picker_state_new_empty`)
  - [x] `SessionPickerState::new()` with sessions (`test_session_picker_state_new_with_sessions`)
  - [x] Navigation bounds clamping (`test_navigation_bounds`)
  - [x] Scroll offset down (`test_scroll_offset_down`)
  - [x] Scroll offset up (`test_scroll_offset_up`)
  - [x] `build_transcript_from_events()` empty (`test_build_transcript_from_events_empty`)
  - [x] `build_transcript_from_events()` with messages (`test_build_transcript_from_events_messages`)
  - [x] `build_transcript_from_events()` with tool use (`test_build_transcript_from_events_tool_use`)
  - [x] `build_transcript_from_events()` with thinking (`test_build_transcript_from_events_thinking`)
  - [x] `build_transcript_from_events()` mixed events (`test_build_transcript_from_events_mixed`)
- Integration test (optional):
  - [ ] Create session, switch away, switch back, verify content — deferred

---

## Polish Phases (after MVP)

### Phase 1: Preview on navigate ✅

**Goal**: Show live preview of session transcript when navigating the picker

**Scope checklist**:
- [x] Add `original_cells` field to `SessionPickerState` for restore on Esc
- [x] Update `SessionPickerState::new()` to take and store original cells snapshot
- [x] Add `UiEffect::PreviewSession { session_id }` effect
- [x] Update `open_session_picker()` to pass cells snapshot and trigger initial preview
- [x] Update navigation handlers to emit `PreviewSession` effect on selection change
- [x] Implement `preview_session()` effect handler (loads events, updates transcript display only)
- [x] Update Esc handler to restore original cells from snapshot
- [x] Update Ctrl+C handler to restore original cells from snapshot
- [x] Add test for original cells storage

**✅ Demo**: Navigate sessions with ↑/↓, see preview update, Esc restores original

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

## File Changes Summary ✅ COMPLETE

| File | Changes | Status |
|------|---------|--------|
| `src/ui/chat/overlays/session_picker.rs` | **New file** - state, handlers, render | ✅ Done |
| `src/ui/chat/overlays/mod.rs` | Add module + re-exports | ✅ Done |
| `src/ui/chat/state/mod.rs` | Add `SessionPicker` variant + accessors to `OverlayState` | ✅ Done |
| `src/ui/chat/effects.rs` | Add `OpenSessionPicker`, `LoadSession` variants | ✅ Done |
| `src/ui/chat/mod.rs` | Implement effect handlers + `build_transcript_from_events()` | ✅ Done |
| `src/ui/chat/reducer.rs` | Wire up key handling dispatch | ✅ Done |
| `src/ui/chat/view.rs` | Add render call for session picker | ✅ Done |
| `src/ui/chat/commands.rs` | Add "sessions" command | ✅ Done |

**Note**: `build_transcript_from_events()` was placed in `mod.rs` rather than `transcript.rs` as originally planned, which is fine since it needs access to session types and is only used by the effect handler.

---

## Review Notes

Plan reviewed by Gemini and Codex. Key feedback incorporated:
- ✅ Effect-driven I/O (not in reducer)
- ✅ Full state reset on session switch
- ✅ Transcript fidelity via SessionEvent mapping
- ✅ Scroll offset for long lists
- ✅ Empty list / error handling
- ✅ Block switch during agent run
