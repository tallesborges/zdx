# plan_ratatui_minimal

> **Goal:** Add ratatui for a simple split layout: scrollable messages (top) + fixed input (bottom).
>
> **YAGNI:** No status line, no fancy formatting, no colors. Just the layout.

---

## Status: ✅ IMPLEMENTED

Implemented with `ratatui` + `tui-textarea` (like Codex).

**Key design:**
- Uses `Viewport::Inline(3)` - NO alternate screen, input area fixed at bottom
- Messages print normally above (standard terminal scrolling)
- `tui-textarea` handles all text editing (multiline, cursor, selection)
- History navigation (Up/Down arrows at buffer boundaries)
- Double Ctrl+C to quit, single Ctrl+C/Esc to clear

**Files changed:**
- `Cargo.toml`: added `ratatui = "0.29"`, `tui-textarea = "0.7"`
- `src/ui/tui.rs`: new TuiApp using tui-textarea
- `src/ui/mod.rs`: exports `TuiApp`, `InputResult`
- `src/chat.rs`: uses `TuiApp` instead of `TtyHandler`

**Legacy files (unused but kept):**
- `src/ui/input_state.rs`
- `src/ui/tty.rs`

---

## Original Plan (for reference)

### Step 1: Add ratatui dependency ✅
### Step 2: Create minimal TUI app ✅
### Step 3: Wire TUI into chat ✅
