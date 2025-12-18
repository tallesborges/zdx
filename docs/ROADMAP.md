# Roadmap

> **Principle:** Build the engine once, then render it in CLI now and TUI later.
>
> **Note:** ROADMAP describes outcomes (what). `docs/SPEC.md` is the source of truth for contracts, and `docs/adr/` captures decision rationale (why).

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

## Next — v0.2.x (Engine-First Hardening + Authoring)

**Goal:** Make the engine/renderer split real, keep sessions durable, and unlock minimal authoring tools.

### v0.2.0 — Engine/renderer separation (SPEC-first)
- [ ] UI-agnostic engine module (agent loop emits events; no printing/formatting)
- [ ] Engine emits the required event types including `ToolRequested` (per SPEC §7)
- [ ] Renderer strictly enforces stdout/stderr rules (per SPEC §10)
- [ ] Ctrl+C reliably records an interruption event (per SPEC §11)
- [ ] Session JSONL includes `meta` (schema version), `tool_use`, `tool_result` events (per SPEC §8)
- [ ] All tool outputs use structured JSON envelope (per SPEC §6)

### v0.2.1 — Provider testability (offline)
- [ ] Provider base URL override (env or config) to enable local/fixture tests (per SPEC §5)
- [ ] Fixture-driven tests for streaming + tool loop parsing (no network)
- [ ] Clear, stable error shaping for provider failures (stderr-friendly)

### v0.2.2 — Tool: `write` (minimal authoring)
- [ ] `write` tool: create/overwrite files; auto-create parent directories (per SPEC §6 target)
- [ ] Deterministic tool result shape (easy to parse; good errors)

### v0.2.3 — Tool: `edit` (surgical edits)
- [ ] `edit` tool: exact text replacement with explicit failure modes (per SPEC §6 target)
- [ ] Deterministic tool result shape (easy to parse; good errors)

### v0.2.4 — Terminal UX polish
- [x] `AGENTS.md` hierarchical auto-inclusion (per SPEC §15):
  - Deterministic file name (`AGENTS.md`)
  - Hierarchical search: `ZDX_HOME` → `~` → ancestors → project root
  - Loaded once at session start
  - Surfaced to stderr (e.g., "Loaded AGENTS.md from ...")
- [ ] Cleaner, consistent error rendering to stderr (terminal-first)
- [ ] Improved transcript formatting (readable defaults; pipe-friendly)
- [ ] Optional system prompt profiles (only if it stays simple)

---

## Soon — v0.3.x (TUI MVP)

**Goal:** Minimal TUI powered by the same engine event stream.

### v0.3.0 — "Same engine" TUI baseline
- [ ] `zdx tui` command (MVP)
- [ ] Sessions list + resume
- [ ] Chat view with streaming output
- [ ] Tool activity panel
- [ ] Renderer parity (CLI + TUI consume the same engine events)

---

## Later — v0.4.x (Context + Optional Guardrails)

**Goal:** Enhanced context and optional mutation visibility.

### v0.4.0 — Explicit context attachments
- [ ] `--file <path>` context attachment (explicit, user-driven)
- [ ] Conservative project-aware context (only if predictable and low magic)

### v0.4.1 — Optional mutation visibility (still YOLO default)
- [ ] Diff preview for `write`/`edit`
- [ ] Optional `--confirm` for mutations (opt-in; YOLO default per SPEC §2)

---

## Later — v0.5.x (Automation Contracts)

**Goal:** Stable `exec` for scripts and tooling.

### v0.5.0 — Script-friendly outputs (versioned)
- [ ] `exec --format json` schema finalized and versioned (stable contract)
- [ ] `--tool-results=full|summary|omit`
- [ ] Exit codes locked down (per SPEC §10)
- [ ] Structured event stream export
- [ ] Note: `--format` flag exists from v0.2.x but json schema is reserved until v0.5

---

## Later — v0.6.x (Session Inspection)

**Goal:** Search and share past work.

- [ ] Session search and filtering
- [ ] `zdx session export --format html`
- [ ] Offline viewer

---

## Later — v0.7.x+ (Ergonomics + Optional Providers)

**Goal:** Deeper workflows and quality-of-life improvements without bloating the core.

### Providers (optional)
- [ ] Second provider (e.g., OpenAI) behind the same provider contract
- [ ] Local model support (e.g., Ollama) if it doesn’t bloat the core

### UX / Ergonomics
- [ ] Expanded prompt profiles
- [ ] Chat slash commands
- [ ] Shell completions (`zdx completion <shell>`)

---

## Far Future (post v1.0)

- [ ] Web UI for session browsing (out of scope for v0.x per SPEC)
