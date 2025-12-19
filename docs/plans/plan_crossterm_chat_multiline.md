# plan_crossterm_chat_multiline

> **Goal:** Replace the default chat UX with a `crossterm`-based prompt that supports multiline input, power-user editing (word-level navigation, bracketed paste, line deletion), session history navigation, and responsive interruption (Esc/Ctrl+C) during both input and execution phases.
>
> **Contract impact:** Changes interactive chat input behavior (TTY-only) and documents new keybindings; non-TTY behavior remains line-based and pipe-friendly.

---

## Status: SUPERSEDED

**Superseded by:** `plan_ratatui_minimal.md`

The original custom `InputState` + `TtyHandler` implementation was replaced with a simpler ratatui + tui-textarea approach. Key changes:

- **Input handling:** Now uses `tui-textarea` crate instead of custom `InputState`
- **Rendering:** Uses ratatui's `Viewport::Inline` for fixed input area at bottom
- **Interrupt handling:** Engine tool execution made interruptible via `tokio::select!` (kept from this plan)

See `plan_ratatui_minimal.md` for the current implementation.
