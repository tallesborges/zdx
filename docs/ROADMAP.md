# Roadmap

> **Principle:** Build the "engine" once, then render it in CLI and later TUI.

---

## Shipped — v0.1.x (Foundation + Early UX)

**Goal:** A fast, reliable, terminal-first agent with durable sessions and streaming output.

### Commands
- [x] `exec` — non-interactive execution
- [x] Default chat mode — interactive REPL
- [x] `sessions list` / `sessions show`
- [x] `resume` — resume previous session
- [x] `config init` / `config path`

### CLI Options (per SPEC §10)
- [x] `--root` — working directory context
- [x] `--system-prompt` — inline override
- [x] `--session` — append to existing session
- [x] `--no-save` — disable session persistence

### Capabilities
- [x] Anthropic Claude provider with streaming
- [x] Tool calling loop (`read`, `bash`)
- [x] JSONL session persistence
- [x] Streaming output to stdout
- [x] Tool activity indicators to stderr
- [x] stdout/stderr separation
- [x] System prompt from config (inline or file)
- [x] `EngineEvent` types defined (per SPEC §7)

---

## Next — v0.2.x (Engine Extraction + Polish)

**Goal:** Extract UI-agnostic engine so TUI becomes "just a renderer."

### Foundational
- [ ] UI-agnostic engine module (agent loop emits events, no printing)
- [ ] `EngineEvent` as communication layer between engine and renderer
- [ ] Provider testability (base URL override for testing)

### UX Improvements
- [ ] System prompt profiles (`--profile <name>`)
- [ ] `AGENTS.md` auto-inclusion
- [ ] Clean error rendering to stderr
- [ ] Improved transcript formatting

### Optional
- [ ] Shell completions (`zdx completion <shell>`)

---

## Soon — v0.3.x (TUI MVP)

**Goal:** Minimal TUI powered by the same engine event stream.

- [ ] `zdx tui` command
- [ ] Sessions list view
- [ ] Chat view with streaming output
- [ ] Tool activity panel
- [ ] Keyboard navigation
- [ ] Renderer parity (TUI and CLI consume same events)

---

## Later — v0.4.x (Authoring & Context)

**Goal:** Code authoring with visibility-before-mutation.

### New Tools
- [ ] `write` tool
- [ ] `edit` tool (exact replacement)

### UX
- [ ] Diff preview for write/edit
- [ ] Optional `--confirm` for mutation approval (opt-in; YOLO default per SPEC §2)
- [ ] `--file <path>` context attachment
- [ ] Project-aware context (conservative auto-include)

---

## Later — v0.5.x (Automation Contracts)

**Goal:** Stable `exec` for scripts and tooling.

- [ ] `exec --format json` with versioned schema
- [ ] `--tool-results=full|summary|omit`
- [ ] Exit codes locked down (per SPEC §10)
- [ ] Structured event stream export

---

## Later — v0.6.x (Session Inspection)

**Goal:** Search and share past work.

- [ ] Session search and filtering
- [ ] `zdx session export --format html`
- [ ] Offline viewer

---

## Later — v0.7.x+ (Extensibility)

**Goal:** Provider flexibility and deeper workflows.

- [ ] Multiple providers (OpenAI, local models)
- [ ] Tool/provider response caching
- [ ] Expanded prompt profiles
- [ ] Chat slash commands

---

## Far Future

- [ ] Web UI for session browsing
