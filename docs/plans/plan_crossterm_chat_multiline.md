# plan_crossterm_chat_multiline

> **Goal:** Replace the default chat UX with a `crossterm`-based prompt that supports multiline input, power-user editing (word-level navigation, bracketed paste, line deletion), session history navigation, and responsive interruption (Esc/Ctrl+C) during both input and execution phases.
>
> **Contract impact:** Changes interactive chat input behavior (TTY-only) and documents new keybindings; non-TTY behavior remains line-based and pipe-friendly.

---

## Step 1: Specify multiline chat input and interrupt behavior in SPEC

**Commit:** `docs(spec): define multiline chat input and interrupt behavior`

**Goal:** Make multiline input, navigation, and keybindings unambiguous (see ADR-0004).

**Deliverable (SPEC):**
- Define TTY vs Non-TTY input behavior.
- **Keybindings (Input Phase):**
  - `Enter`: Submit message.
  - `Shift+Enter` / `Alt+Enter`: Insert newline.
  - `Arrows`, `Home/End`, `Ctrl+A/E`: Basic navigation.
  - `Option+Left` / `Option+Right`: Word-level navigation (back/forward).
  - `Backspace/Delete`: Character deletion.
  - `Option+Backspace`: Delete previous word.
  - `Cmd+Backspace` (or `Ctrl+U`): Delete entire current line.
  - `Ctrl+K`: Clear to end of line.
  - `Up/Down` at boundaries: Navigate previous user messages in session history.
  - `Ctrl+C` or `Esc`: Clear buffer.
  - `Ctrl+C` (twice): Exit chat.
- **Feature Support:**
  - **Bracketed Paste**: Enabled to allow pasting blocks of code without accidental submission.
- **Interrupt Behavior (Execution Phase):**
  - `Ctrl+C` or `Esc`: Signal interrupt to engine. This stops the current tool or streaming turn.
  - `Ctrl+C` (twice): Force exit.

**Technical Note (Interrupts):** In raw mode, the terminal usually does not send `SIGINT` on `Ctrl+C`. The TTY event loop must catch the `KeyEvent` and manually trigger the interrupt logic.

---

## Step 2: Add `crossterm` dependency

**Commit:** `chore(deps): add crossterm`

**Deliverable:**
- Add `crossterm` to `Cargo.toml`.
- Verify `is-terminal` availability (part of `std`).

---

## Step 3: Implement `InputState` (Pure & Testable Logic)

**Commit:** `feat(ui): add multiline input state machine`

**Goal:** Create a unit-testable state machine that handles text editing, word-level navigation, and history navigation without TTY dependency.

**Deliverable:**
- `src/ui/input_state.rs`:
  - `InputState` struct: `buffer: String`, `cursor: (row, col)`, `history: Vec<String>`, `history_index: Option<usize>`.
  - `handle_event(CrosstermEvent) -> Action` where `Action` is `None`, `Redraw`, `Submit(String)`, `Quit`, or `Clear`.
- Unit tests covering all keybindings, word-jump logic, and history boundary transitions.

---

## Step 4: Implement `TtyHandler` and Raw Mode Management

**Commit:** `feat(ui): add tty handler and raw mode guard`

**Goal:** Manage raw mode safely, enable bracketed paste, and provide the core TTY prompt loop.

**Deliverable:**
- `src/ui/tty.rs`:
  - `RawModeGuard`: RAII struct to ensure raw mode is disabled and bracketed paste is turned off on `Drop`.
  - `TtyHandler`:
    - `readline(history: &[String]) -> Result<Action>`: The main prompt loop.
    - Handles `Event::Paste` for bracketed paste support.
    - Handles terminal resizing (`Event::Resize`) by re-calculating wrapping.
    - Manages terminal cleanup on panics.

---

## Step 5: Implement Integrated TTY Renderer

**Commit:** `feat(ui): add interrupt-aware tty renderer`

**Goal:** Provide a renderer that can capture interrupt keys during engine execution while streaming assistant output.

**Deliverable:**
- `src/ui/renderer.rs` (or update existing):
  - Enhance `CliRenderer` to be TTY-aware.
  - Provide a way to poll for `Esc`/`Ctrl+C` while events are being processed.
  - Ensure assistant output (stdout) and tool status (stderr) are rendered correctly in raw mode (handling line wraps and cursor positions).
  - Update `interrupt.rs` to expose a `trigger_interrupt()` that handles the double-tap logic consistently between the TTY loop and the global `ctrlc` handler.

---

## Step 6: Improve Engine Responsiveness

**Commit:** `feat(engine): make tool execution and streaming responsive to interrupts`

**Goal:** Ensure that tool execution (especially `bash`) and the main engine loop respond immediately to the interrupt flag.

**Deliverable:**
- `src/engine.rs`:
  - Use `tokio::select!` in `execute_tools_async` to allow cancellation of tool futures if the interrupt flag is set.
  - Ensure the streaming loop checks the interrupt flag with high frequency.
- `src/tools/bash.rs`:
  - Ensure `run_command` can be cancelled gracefully (terminating the child process).

---

## Step 7: Wire into `chat.rs`

**Commit:** `feat(chat): use multiline editor and wire interrupts`

**Goal:** Toggle between the new TTY handler and the legacy `BufRead` loop.

**Deliverable:**
- `src/chat.rs`:
  - Use `TtyHandler` if `stdin().is_terminal()`.
  - Populate `InputState` history from the current `Session` (filtering for user messages).
  - Wire the event loop such that `Esc` or `Ctrl+C` during execution triggers the interrupt flag via the TTY renderer.

---

## Step 8 (optional): Calm status line

**Commit:** `feat(ui): add calm status line`

**Goal:** Add a single-line "HUD" that shows engine phase (e.g., `⚙ Reading file...`) while the user is waiting for a response.

**Deliverable:**
- `src/ui/status.rs`:
  - A utility to render a quiet spinner and the current `EngineEvent` phase on the bottom line.
  - Automatically clears itself before the next prompt is shown.