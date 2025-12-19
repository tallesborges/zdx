# ADR-0004: Crossterm Raw Mode for Multiline Input

## Status
Accepted

## Context
The current chat interface uses `std::io::stdin().lines()`, which is limited to single-line input and provides no control over cursor navigation, history, or interrupt (Esc) handling. To support multiline prompts (essential for coding tasks) and responsive tool cancellation, we need raw terminal access.

## Decision
We will implement a custom input loop using `crossterm`.
1. Use **Raw Mode** during the input phase to intercept every keypress.
2. Maintain raw mode (or a "passive" variant) during the execution phase to capture `Esc` and `Ctrl+C` for immediate tool/stream cancellation.
3. Fall back to standard line-buffered input when `stdin` is not a TTY (preserving pipe/test compatibility).

## Consequences
- **Pros:** Full control over UI, multiline support, responsive interrupts, standard macOS/Zed shortcuts.
- **Cons:** Increased complexity in terminal state management; potential for "mangled" terminal if cleanup fails (mitigated by RAII guards).
