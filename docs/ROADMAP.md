# Roadmap

> **Current focus:** UX-first (make daily use feel great) while doing the *right* foundational work to make a future TUI straightforward (UI-agnostic core + event stream).
>
> **Principle:** Build the “engine” once, then render it in CLI and later TUI.

---

## Shipped — v0.1.0 (Foundation)

**Goal:** A fast, reliable, terminal-first agent with durable sessions.

- [x] Core commands: `exec`, `chat`
- [x] Session persistence (JSONL event log)
- [x] Resume existing sessions
- [x] Configuration management
- [x] Anthropic Claude integration
- [x] Tool execution loop
  - [x] `read` (filesystem)
  - [x] `bash` (shell)

---

## Next — v0.2.x (UX-First + TUI-Ready Core)

**Goal:** Make the CLI feel excellent *now* while shaping the core so a TUI becomes “just a renderer.”

### Engine / UI separation (the #1 TUI enabler)
- [ ] Extract a UI-agnostic core (“engine”) module/crate
  - Agent loop lives in core (no printing)
  - CLI becomes a renderer/adapter over core
- [ ] Define a minimal internal event stream emitted by core
  - `AssistantDelta`, `AssistantFinal`
  - `ToolStarted`, `ToolFinished` (+ optional chunked tool output)
  - `SessionPersisted`, `Error`, `Interrupted`

### Streaming UX (immediate user-visible win)
- [ ] Output streaming for text mode
  - Stream assistant text to stdout as it arrives (no buffering)
- [ ] Provider streaming integrated into the event stream
- [ ] Tool activity indicators
  - One-line status (“Running bash…”, “Reading file…”)
  - Optional verbose tool trace toggle

### Session navigation (daily usability + TUI landing screen later)
- [ ] `zdx sessions list`
- [ ] `zdx sessions show <id>`
- [ ] Improve transcript formatting for terminal readability
  - Clear separation: user / assistant / tool blocks

### Prompting ergonomics (high ROI, low complexity)
- [ ] System prompt configuration
  - File-based system prompt
  - Config override
- [ ] System prompt profile library
  - `--profile rust-expert`, `--profile bash-minimal`, etc.
  - Profiles compose cleanly with project conventions
- [ ] Config override via CLI flags (command-level knobs)
  - `--system-prompt "..."` (inline override)
  - `--budget-tokens 5000` (token budget / cap)
  - Keep flags minimal and consistent across `chat`/`exec`

### Project conventions (useful now; can be cached)
- [ ] `AGENTS.md` support
  - Auto-include when present in project root
- [ ] Caching of `AGENTS.md`
  - Memoize per session (content + mtime) to avoid repeated reads

### Reliability polish (UX-critical)
- [ ] Cancellation (Ctrl+C) that leaves sessions consistent
  - Persist interruption as a structured event
- [ ] Tool timeouts + failure shaping
  - Structured errors; prevent “stuck” loops
- [ ] (Optional polish) `zdx completion <bash|zsh|fish>`

---

## Soon — v0.3.x (TUI MVP)

**Goal:** Build a minimal TUI that is powered entirely by the same core event stream.

- [ ] `zdx tui` (thin slice)
  - Sessions list (open/resume/new)
  - Chat view (scrollback + input box)
  - Streaming assistant output
  - Tool activity panel (basic)
- [ ] Keyboard ergonomics
  - Scroll, jump to bottom, copy transcript block
  - Minimal shortcuts (don’t over-design)
- [ ] Renderer parity
  - No forked logic: TUI consumes the same `EngineEvent` stream as CLI

> **Note:** This is intentionally a *viewer + chat* first. No file tree, no diff UI, no IDE ambitions yet.

---

## Later — v0.4.x (Authoring & Context)

**Goal:** Move from read-only to safe code authoring (visibility-before-mutation).

- [ ] `write` tool
- [ ] `edit` tool (exact replacement)
- [ ] Diff preview (default) for write/edit
- [ ] Guardrails (visibility before mutation)
  - Interactive apply flow
  - Optional non-interactive `--apply` for scripts
- [ ] Optional “YOLO” toggle (recommended compromise)
  - Default: `read` allowed
  - `--yolo`: enables mutation (`write/edit`) and/or shell (`bash`)
- [ ] Explicit context attachment
  - `--file <path>`
- [ ] Project-aware context (conservative heuristics)
  - Auto-include a *small* relevant set (hard caps; predictable)

---

## Later — v0.5.x (Exec Interop + Automation Contracts)

**Goal:** Make `exec` a stable building block for scripts/other CLIs (when you’re ready to lock the contract).

- [ ] Stable machine output
  - `exec --format json`
  - Versioned envelope (`schema_version`)
  - Deterministic fields (session_id, final_text, events optional)
- [ ] Tool result filtering in JSON output
  - `--tool-results=full|summary|omit`
- [ ] Output channels + exit codes locked down
  - stdout/stderr rules; non-zero exit codes on failures
- [ ] (Optional) Structured export of event stream
  - JSON event dump for debugging and external tooling

---

## Later — v0.6.x (Sessions & Inspection)

**Goal:** Inspect, search, and share past work.

- [ ] Session discovery
  - Search and filter by content / metadata
- [ ] Session export
  - `zdx session export --format html`
  - Offline viewer
  - Optional `--open`

---

## Later — v0.7.x+ (Extensibility + Performance)

**Goal:** Provider flexibility, deeper workflows, and performance improvements.

- [ ] Multiple providers
  - OpenAI
  - Local models
- [ ] Cache support
  - Tool reads (content hash / mtime)
  - Provider responses (opt-in; careful with secrets)
- [ ] Prompt profiles / templates (expanded)
- [ ] Chat commands (slash / built-ins)
- [ ] TUI exploration (richer experience)
  - After CLI/TUI event model is stable
  - Consider evaluating Zed’s `language_models` crate

---

## Far Future (Optional)

**Goal:** Visibility, not core workflow.

- [ ] Web UI for session browsing
