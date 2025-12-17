# Roadmap

> **Current focus:** UX-first (make daily use feel great) while doing the *right* foundational work to make a future TUI straightforward (UI-agnostic core + event stream).
>
> **Principle:** Build the "engine" once, then render it in CLI and later TUI.

---

## Shipped — v0.1.x (Foundation + Early UX)

**Goal:** A fast, reliable, terminal-first agent with durable sessions and streaming output.

### Core commands
- [x] `exec` — non-interactive execution
- [x] Default chat mode (interactive REPL)
- [x] `sessions list` / `sessions show`
- [x] `resume` — resume previous session
- [x] `config init` / `config path`

### CLI flags (per SPEC §10)
- [x] `--root <ROOT>` — working directory context for tools
- [x] `--system-prompt <PROMPT>` — inline override
- [x] `--session <ID>` — append to existing session
- [x] `--no-save` — disable session persistence

### Session persistence
- [x] JSONL event log format
- [x] Resume existing sessions with full history
- [x] Interruption recorded as structured event

### Provider integration
- [x] Anthropic Claude integration
- [x] Streaming responses (SSE)
- [x] Tool calling loop (`tool_use` → execute → `tool_result`)

### Tools
- [x] `read` (filesystem)
- [x] `bash` (shell)

### Streaming UX
- [x] Output streaming for text mode (tokens printed as they arrive)
- [x] Basic tool activity indicators (`⚙ Running...` to stderr)
- [x] stdout/stderr separation (assistant text → stdout, status → stderr)

### Configuration (per SPEC §9)
- [x] `system_prompt` (inline)
- [x] `system_prompt_file` (file wins if both present)
- [x] `model`, `max_tokens`, `tool_timeout_secs`

### Engine events (per SPEC §7)
- [x] `EngineEvent` types defined: `AssistantDelta`, `AssistantFinal`, `ToolStarted`, `ToolFinished`, `Error`, `Interrupted`

---

## Next — v0.2.x (Engine Extraction + Polish)

**Goal:** Extract UI-agnostic engine so TUI becomes "just a renderer." Polish remaining UX gaps.

### Engine / UI separation (the #1 TUI enabler)
- [ ] Extract UI-agnostic core ("engine") module/crate
  - Agent loop lives in core (no printing)
  - CLI becomes a renderer/adapter over core
- [ ] Wire `EngineEvent` as actual communication layer
  - Engine emits events; CLI renders them
  - Currently: agent.rs prints directly (needs refactor)

### Provider testability (per SPEC §5)
- [ ] Base URL override (env var or config)
- [ ] Fixture-driven stream parsing tests

### Prompting ergonomics
- [ ] System prompt profile library
  - `--profile rust-expert`, `--profile bash-minimal`, etc.
  - Profiles compose cleanly with project conventions

### Project conventions
- [ ] `AGENTS.md` support
  - Auto-include when present in project root
- [ ] Caching of `AGENTS.md`
  - Memoize per session (content + mtime) to avoid repeated reads

### Reliability polish
- [ ] Clean error rendering (per SPEC §7 renderer rules)
  - Structured error display to stderr
- [ ] Improve transcript formatting for terminal readability
  - Clear separation: user / assistant / tool blocks
- [ ] (Optional) `zdx completion <bash|zsh|fish>`

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
  - Minimal shortcuts (don't over-design)
- [ ] Renderer parity
  - No forked logic: TUI consumes the same `EngineEvent` stream as CLI

> **Note:** This is intentionally a *viewer + chat* first. No file tree, no diff UI, no IDE ambitions yet.

---

## Later — v0.4.x (Authoring & Context)

**Goal:** Move from read-only to safe code authoring (visibility-before-mutation).

- [ ] `write` tool
- [ ] `edit` tool (exact replacement)
- [ ] Diff preview (default) for write/edit
- [ ] Optional guardrails (visibility before mutation)
  - Interactive apply flow (opt-in)
  - `--confirm` flag for mutation approval prompts
- [ ] YOLO remains the default (per SPEC §2)
  - All tools (`read`, `bash`, `write`, `edit`) work without confirmation by default
  - Guardrails are opt-in, not opt-out
- [ ] Explicit context attachment
  - `--file <path>`
- [ ] Project-aware context (conservative heuristics)
  - Auto-include a *small* relevant set (hard caps; predictable)

---

## Later — v0.5.x (Exec Interop + Automation Contracts)

**Goal:** Make `exec` a stable building block for scripts/other CLIs (when you're ready to lock the contract).

- [ ] Stable machine output
  - `exec --format json`
  - Versioned envelope (`schema_version`)
  - Deterministic fields (session_id, final_text, events optional)
- [ ] Tool result filtering in JSON output
  - `--tool-results=full|summary|omit`
- [ ] Output channels + exit codes locked down (per SPEC §10)
  - stdout/stderr rules; exit codes: 0 success, 1 failure, 2 config error, 130 interrupted
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
  - Consider evaluating Zed's `language_models` crate

---

## Far Future (Optional)

**Goal:** Visibility, not core workflow.

- [ ] Web UI for session browsing
