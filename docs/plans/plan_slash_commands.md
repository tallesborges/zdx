# Slash Commands Implementation Plan

**Status:** In Progress  
**Scope:** Add `/commands` popup to TUI with `/new` (alias `/clear`) and `/quit`

---

## Goals

1. Users can type `/` anywhere in input to open a command popup
2. Users can press `Ctrl+P` to open the command popup (command palette)
3. Popup shows available commands with fuzzy filtering
4. Commands execute immediately on selection
5. `/new` starts a new conversation (creates new session)
6. `/quit` exits the TUI cleanly

---

## Non-Goals / Deferred

- **Deferred:** Slash commands that take arguments (e.g., `/model gpt-4`)
- **Deferred:** Custom user-defined commands
- **Deferred:** Command history / recent commands
- **Deferred:** `/help` command (use popup descriptions instead)
- **Deferred:** Inline command execution (typing `/quit` + Enter without popup)
- **Deferred:** Autocomplete in input (only popup for now)

---

## User Journey

```
1. User is typing in input area
2. User presses `/` or `Ctrl+P` → popup appears above input
3. Popup shows: /new (aliases: /clear), /quit
4. User can:
   a. Type to filter (e.g., "ne" shows only /new)
   b. Arrow keys to navigate selection
   c. Enter to execute selected command
   d. Escape to close popup and insert "/" into input (only if opened via `/`)
5. Command executes, popup closes
6. Input state depends on command:
   - /new: input cleared, transcript cleared, new session started
   - /quit: TUI exits
```

---

## MVP Slices

### Slice 0: Terminal Safety (Foundation) ✅ DONE

**Goal:** Ensure popup can't corrupt terminal state.

- [x] Verify existing panic hook handles popup state (already exists in `install_panic_hook`)
- [x] Add `CommandPopup` to `TuiApp` state (Option<CommandPopupState>)
- [x] Popup state cleared on any terminal restore path

**Checklist:**
- [x] Popup state is `Option<T>` so it's trivially droppable
- [x] No new raw mode or alt-screen changes needed (reuses existing setup)
- [x] Test: Force panic while popup is open → terminal restores cleanly
- [x] Unit tests for `SlashCommand` and `CommandPopupState` (8 tests)

**✅ Demo:** Open popup, `panic!()` via debug key → terminal restores.

**Failure modes:**
- Panic leaves popup state orphaned → mitigated by Drop impl
- Raw mode not restored → existing panic hook handles this

---

### Slice 1: State + Trigger ✅ DONE

**Goal:** `/` opens popup, Escape closes it, state tracks visibility.

**Checklist:**
- [x] Add `SlashCommand` struct and `SLASH_COMMANDS` constant (done in Slice 0)
- [x] Add `CommandPopupState` struct (done in Slice 0)
- [x] Add `command_popup: Option<CommandPopupState>` to `TuiApp` (done in Slice 0)
- [x] Add `open_command_popup()` method
- [x] Add `close_command_popup(insert_slash: bool)` method
- [x] Add `handle_popup_key()` to route keys when popup open
- [x] Handle `/` key to open popup
- [x] Handle `Escape` in popup to close (insert "/" into input)
- [x] Handle `Ctrl+C` in popup to close (don't insert "/")
- [x] Add temporary status indicator "/ Commands (Esc to cancel)" in header

**✅ Demo:** Type `/` → status shows "/ Commands". Press `Escape` → popup closes, "/" appears in input.

**Failure modes:**
- Double `/` opens popup twice → guarded with `if self.command_popup.is_none()`
- Popup open during engine streaming → works (popup is overlay)

---

### Slice 2: Popup Rendering ✅ DONE

**Goal:** Render popup as floating box above input area.

**Checklist:**
- [x] Add `render_command_popup()` method
- [x] Calculate popup dimensions and position (centered, above input)
- [x] Render command list with `List` widget and selection highlight
- [x] Show aliases in parentheses: `/new (clear)`
- [x] Show description on same line
- [x] Render filter text at bottom when non-empty
- [x] Use `Clear` widget to clear area behind popup
- [x] Yellow border with "Commands" title
- [x] Selection indicator "▶ " with dark gray background

**✅ Demo:** Type `/` → see popup with both commands, first one selected with "▶".

**Failure modes:**
- Small terminal clips popup → capped at half terminal height
- Popup overlaps header → positioned relative to input area

---

### Slice 3: Navigation + Filtering ✅ DONE

**Goal:** Arrow keys navigate, typing filters, Enter/Tab selects. Input at top like Amp's command palette.

**Updated popup layout (Amp-style):**
```
┌ Commands ──────────────────────────────┐
│ > filter_text█                         │  ← Input at TOP
├────────────────────────────────────────┤
│▶ /new (clear)   Start a new convers... │
│  /quit (q, exit)  Exit ZDX             │
└────────────────────────────────────────┘
```

**Key handling in popup:**
- `Up/Down` → move selection
- `Enter/Tab` → execute selected command
- `Backspace` → remove last filter char
- `Char(c)` → append to filter
- `Escape` → close + insert "/"
- `Ctrl+C` → close (no insert)

**Checklist:**
- [x] Move filter input to TOP of popup (below title, above list)
- [x] Show `> ` prompt with filter text and cursor indicator
- [x] Up/Down arrows move selection (wrap around optional)
- [x] Enter executes selected command
- [x] Tab also executes (common shortcut)
- [x] Typing appends to filter
- [x] Backspace removes from filter (empty filter shows all)
- [x] Filter matches name OR aliases (case-insensitive)
- [x] Empty filter result shows "No matching commands"
- [x] Clamp selection when filter changes

**✅ Demo:** Type `/ne` → filter shows "ne", only `/new` shown. Press Enter → command executes.

**Failure modes:**
- Filter to empty → show "no matches", disable Enter
- Navigate past bounds → clamp or wrap selection index

---

### Slice 4: Command Execution ✅ DONE

**Goal:** `/new` and `/quit` work correctly.

**Implementation notes:**
- `/new` now starts a fresh session (creates new session file) rather than just clearing UI
- `/quit` interrupts any running engine before exiting

**Checklist:**
- [x] `/quit` sets `should_quit = true`
- [x] `/new` clears transcript, messages, resets scroll
- [x] `/new` starts a new session (if sessions enabled)
- [x] `/new` shows system message with new session ID
- [x] Engine state check: block `/new` while streaming (show message)
- [x] `/quit` during streaming: interrupt first, then quit

**✅ Demo:** 
1. Have a conversation, type `/new` → transcript empty, new session ID shown
2. Type `/quit` → TUI exits cleanly

**Failure modes:**
- New during streaming → block with "Cannot start new while streaming"
- Quit during streaming → interrupt first, then quit (existing Ctrl+C behavior)

---

### Slice 5: Polish + Edge Cases

**Goal:** Handle edge cases, improve UX.

**Edge cases:**
- [ ] `/` at end of text: "hello/" → popup opens, Escape adds "/" after "hello"
- [ ] Multiple `/` in input already: should still work
- [ ] Very long filter text: truncate display, don't crash
- [ ] Terminal resize while popup open: recalculate position
- [ ] Paste containing "/": don't trigger popup (only direct `/` keypress)

**UX polish:**
- [ ] Popup animation: instant (no animation needed for MVP)
- [ ] Keyboard hint in popup footer: "↑↓ navigate • Enter select • Esc cancel"
- [ ] Selected command shows full description (if space allows)
- [ ] Filter prefix shown: `> clear` (dimmed `>`)

**Checklist:**
- [ ] Test: resize while popup open
- [ ] Test: Escape inserts "/" at cursor position
- [ ] Test: Paste text with "/" doesn't trigger
- [ ] Add keyboard hints to popup
- [ ] Ensure popup doesn't steal focus from input cursor visually

**✅ Demo:** Resize terminal while popup open → popup repositions correctly.

**Failure modes:**
- Cursor position lost after Escape → track cursor, insert at correct position

---

## Contracts (Guardrails)

1. **Popup is ephemeral overlay:** Never affects terminal state beyond TuiApp
2. **Escape always closes:** User can always dismiss popup with Escape
3. **Commands are idempotent:** Running `/new` twice is safe
4. **No data loss without intent:** `/new` only affects UI state, not session files
5. **Input preserved on cancel:** Escape inserts "/" so user input isn't lost (if opened via `/`)

---

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Trigger | `/` anywhere in input OR `Ctrl+P` | More discoverable, consistent with other tools |
| Popup vs inline | Popup overlay | Shows all commands, easier discovery |
| Filter matching | Contains (case-insensitive) | Simple, good enough for 2 commands |
| Escape behavior | Close + insert "/" (if opened via `/`) | Preserves user intent (they typed "/") |
| New during streaming | Block with message | Safer, avoids race conditions |
| Quit during streaming | Allow (interrupts first) | Consistent with Ctrl+C |
| Command naming | `/new` primary, `/clear` alias | "new" is clearer about the action (starts fresh session) |

---

## Testing

### Unit tests (in `tui.rs` or new `commands.rs`)
- [ ] `SlashCommand` filtering logic
- [ ] Selection wrapping/clamping
- [ ] Filter matching (name and aliases)

### Integration tests (manual for now)
- [ ] Open/close popup cycle
- [ ] Execute each command
- [ ] Resize during popup
- [ ] Panic recovery with popup open

---

## Polish Phases

### Phase 1 (MVP - Slices 0-4)
- Basic popup, navigation, both commands work
- Minimum viable UX

### Phase 2 (Post-MVP)
- Keyboard hints in popup
- Better styling (rounded corners if terminal supports)
- Animation/fade (optional)

### Phase 3 (Future)
- More commands: `/model`, `/session`, `/help`
- Command arguments
- Fuzzy matching (fzf-style)

---

## Later / Deferred

- **Command arguments:** `/model sonnet-3.5` - needs input field in popup
- **Custom commands:** User-defined in config
- **Recent commands:** Show recently used at top
- **Inline execution:** Type `/quit` + Enter without popup
- **Tab completion:** Complete command inline without popup
- **Command palette:** Ctrl+Shift+P style (separate from `/`)
- **Confirmation dialogs:** "/new" confirm before clearing long conversation

---

## File Changes Summary

| File | Changes |
|------|---------|
| `src/ui/tui.rs` | Add popup state, key handling, rendering |
| `src/ui/mod.rs` | No changes (unless extracting to module) |
| `docs/SPEC.md` | Add §16 for slash commands contract (optional) |

---

## Implementation Order

```
Slice 0 → Slice 1 → Slice 2 → Slice 3 → Slice 4 → Slice 5
  ↓          ↓          ↓          ↓          ↓          ↓
Safety    State     Render     Navigate   Execute   Polish
          + Trigger            + Filter
```

Each slice is independently testable and demoable.
