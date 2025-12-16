# Roadmap

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

## Next — v0.2.x (UX + Interop)

**Goal:** Better ergonomics + predictable outputs for automation.

- [ ] Streaming responses (lower latency, better UX)
- [ ] Stable machine output  
  - `exec --format json`  
  - Versioned envelope for other CLIs / scripts
- [ ] System prompt configuration
  - File-based
  - Config override
- [ ] `AGENTS.md` support
  - Auto-include when present in project root
- [ ] Extended / structured thinking support

---

## Soon — v0.3.x (Authoring & Context)

**Goal:** Move from “read-only assistant” to “safe code author”.

- [ ] `write` tool
- [ ] `edit` tool (exact replacement)
- [ ] Diff preview for write/edit
- [ ] Guardrails (visibility before mutation)
- [ ] Context attachment
  - `--file <path>`
- [ ] Project-aware context
  - Auto-include relevant files (heuristic-based, not magic)

---

## Later — v0.4.x (Sessions & Inspection)

**Goal:** Inspect, search, and share past work.

- [ ] Session discovery
  - Search and filter by content / metadata
- [ ] Session export
  - `session export --format html`
  - Offline viewer
  - Optional `--open`

---

## Later — v0.5.x+ (Extensibility)

**Goal:** Provider flexibility and deeper workflows.

- [ ] Multiple providers
  - OpenAI
  - Local models
- [ ] Prompt profiles / templates
- [ ] Cache support (responses, tool reads)
- [ ] Chat commands (slash / built-ins)
- [ ] TUI (only after CLI UX is excellent)
- [ ] Evaluate Zed’s `language_models` crate

---

## Far Future (Optional)

**Goal:** Visibility, not core workflow.

- [ ] Web UI for session browsing
