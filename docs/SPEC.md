# ZDX Specification

**Product:** ZDX (terminal-first agentic coding CLI)  
**Spec version:** living document  
**Status:** Source of truth for *values + contracts*. If `docs/ROADMAP.md` exists, it must not contradict `docs/SPEC.md`.

---

## 1) Purpose

ZDX is a **terminal-first** agentic coding CLI that you can use daily for real work.

ZDX optimizes for:
- Fast iteration and "ship small" delivery
- A great terminal UX (streaming, resumable sessions, clean output)
- A core architecture that can later power a TUI without rewriting the engine

ZDX is **not** trying to be an IDE, a framework, or a safety sandbox product.

---

## 2) Core values and principles

### KISS / YAGNI
- Implement the smallest thing that creates user value.
- Prefer **simple data structures + explicit contracts** over "smart" abstractions.
- Refactor only after usage proves the shape.

### Terminal-first
- Designed around UNIX expectations:
  - stdout/stderr separation
  - piping
  - exit codes
  - predictable output formats

### YOLO default
- ZDX prioritizes speed and flow.
- By default, ZDX assumes the user is operating on their own machine and accepts risk.
- Optional guardrails may exist later, but must remain **low friction** and **opt-in**.

### Engine-first (UI-agnostic)
- ZDX has a core "engine" that emits events.
- CLI is just a renderer over the event stream.
- Future TUI must consume the same engine events (no forked logic).

---

## 3) Non-goals

These are explicitly *out of scope* for now:

- Web UI / server mode
- Multi-agent orchestration (agent graphs, swarms, etc.)
- Plugin system with dynamic loading (shared objects, runtime extensions)
- IDE ambitions (file tree, refactor UI, project indexing) in early versions
- Security sandboxing as a primary product goal

---

## 4) Architecture overview

ZDX follows a layered design:

```text
[Config] -> [Session store (JSONL)]  (durable)
                  ^
                  |
[User] -> [CLI Renderer] <-> [Agent Engine] <-> [Provider (Anthropic)]
                                   ^
                                   |
                                 [Tools]
```

### Separation rule

* **Engine**: no printing, no terminal formatting.
* **Renderer** (CLI/TUI): displays events, reads user input, chooses output format.

---

## 5) Provider contract

### Provider(s)

* Primary provider (current): **Anthropic Claude**
* The provider must support:

  * Non-streaming responses (baseline)
  * Streaming responses (SSE / incremental tokens)
  * Tool calling loop (`tool_use` → execute tool → `tool_result`)

### Key management

* API keys are **never stored** in config files.
* API keys are provided via environment variables:

  * `ANTHROPIC_API_KEY`

### Provider configuration surface (user-visible)

* `model`
* `max_tokens`
* `temperature` (optional; may exist later)
* Tool calling mode: always allowed if tools are enabled

### Testability requirement

* Provider calls must be testable without network by allowing:

  * a base URL override (env or config)
  * deterministic fixture-driven stream parsing tests

---

## 6) Tools contract

### Tool philosophy

* Tools are intentionally few, stable, and predictable.
* Tools are exposed to the model as JSON-schema definitions.
* Tool results must be deterministic and easy to parse.

### Tool set (current)

* `read` (filesystem)
* `bash` (shell)

### Path resolution rules

* If a `path` is **absolute**, use it as-is.
* If a `path` is **relative**, resolve relative to `--root` if provided, else current working directory.
* Path canonicalization is allowed for correctness, not for sandboxing guarantees.
* `--root` is treated as a **working directory context**, not a security boundary (YOLO).

### Tool result envelope (all tools)

All tool outputs use a consistent JSON envelope:

**Success:**
```json
{ "ok": true, "data": { ... } }
```

**Error:**
```json
{ "ok": false, "error": { "code": "ENOENT", "message": "File not found" } }
```

This ensures tool results are deterministic and parseable.

### Tool definitions (current)

#### `read`

* **Purpose:** Read file contents.
* **Input schema:**

  ```json
  { "path": "string", "max_bytes": 262144 }
  ```

  * `max_bytes` is optional (default: 256KB)

* **Output schema:**

  ```json
  {
    "ok": true,
    "data": {
      "path": "...",
      "content": "...",
      "truncated": false,
      "bytes": 12345
    }
  }
  ```

* **Truncation:** If file exceeds `max_bytes`, content is truncated and `truncated: true`.

#### `bash`

* **Purpose:** Execute a shell command.
* **Input schema:**

  ```json
  { "command": "string" }
  ```

* **Output schema:**

  ```json
  {
    "ok": true,
    "data": {
      "stdout": "...",
      "stderr": "...",
      "exit_code": 0,
      "timed_out": false
    }
  }
  ```

* **Shell invocation:** Commands run via `sh -lc "<command>"` for POSIX portability.
* **Execution context:** Runs in the current directory or `--root` directory if provided.
* **Timeout:** Controlled by `tool_timeout_secs` (config). If exceeded, `timed_out: true`.
* **Output limits:** stdout/stderr are truncated to a reasonable limit (e.g., 256KB each) with truncation indicated.

### Tool loop correctness requirements

* When the model requests a tool, ZDX must:

  1. Execute the tool
  2. Return a `tool_result` that corresponds to the correct `tool_use_id`
  3. Continue until the model ends the turn

---

## 7) Engine event stream contract

The engine emits events for renderers (CLI now, TUI later). See [ADR-0002](./adr/0002-engine-emits-events-to-renderer-sink.md).

### Required event types (current)

* `AssistantDelta { text }` — incremental text chunk
* `AssistantFinal { text }` — completed message
* `ToolRequested { id, name, input }` — model decided to call a tool
* `ToolStarted { id, name }` — tool execution begins
* `ToolFinished { id, result }` — tool execution complete
* `Error { message }`
* `Interrupted`

### Renderer rules

* The renderer must be able to:

  * render streaming text as it arrives
  * render tool activity indicators
  * render errors cleanly
  * resume sessions from persisted events/messages

### Persistence mapping

* Engine events that affect model context or user-visible history must be persistable as JSONL session events.
  * Minimum persisted set (current):
    - `meta` (schema header)
    - `message`
    - `tool_use`
    - `tool_result`
    - `interrupted`
  * Additional event types may be added later, but backward readability should be preserved where possible.

---

## 8) Session and persistence contract

### Storage location

* Default base directory: `$XDG_CONFIG_HOME/zdx/` if set, otherwise `~/.config/zdx/`
* Override base directory: `$ZDX_HOME` (takes precedence over XDG)
* Sessions directory:

  * `<base>/sessions/`
  * Note: Sessions are technically state/data, but kept under config dir for simplicity.

### File format

* Sessions are **JSONL (append-only)** event logs. See [ADR-0001](./adr/0001-session-format-jsonl.md).

### Timestamp format

* All timestamps (`ts`) are **RFC3339 UTC** (e.g., `2025-12-17T03:21:09Z`).

### Schema versioning

* First line of every session file must be a meta event:

  ```json
  { "type": "meta", "schema_version": 1, "ts": "..." }
  ```

* Schema version increments when event shapes change incompatibly.

### Minimum event schema (current)

* **Meta** (first line):

  ```json
  { "type": "meta", "schema_version": 1, "ts": "..." }
  ```

* **Message:**

  ```json
  { "type": "message", "role": "user", "text": "...", "ts": "..." }
  { "type": "message", "role": "assistant", "text": "...", "ts": "..." }
  ```

* **Tool use** (model requests a tool):

  ```json
  { "type": "tool_use", "id": "...", "name": "read", "input": { "path": "..." }, "ts": "..." }
  ```

* **Tool result** (tool execution output):

  ```json
  { "type": "tool_result", "tool_use_id": "...", "output": { ... }, "ok": true, "ts": "..." }
  ```

* **Interrupted:**

  ```json
  { "type": "interrupted", "role": "system", "text": "Interrupted", "ts": "..." }
  ```

### Session IDs

* Session IDs are UUID v4 (or equivalent stable unique id).

### Session UX requirements

* Resuming a session must continue from the full prior message history.
* Sessions must remain readable even if the process is interrupted mid-stream.

---

## 9) Configuration contract

### Location

* `~/.config/zdx/config.toml` or `$ZDX_HOME/config.toml`

### Format

* TOML

### Current keys (current)

* `model` (string)
* `max_tokens` (int)
* `tool_timeout_secs` (int)
* `system_prompt` (string, optional)
* `system_prompt_file` (string, optional)

### Prompt resolution rules

* If both `system_prompt` and `system_prompt_file` exist:

  * **file wins** (explicitly)
* CLI flags override config file values.
* API key is env-only.

---

## 10) CLI contract

### Commands (stable surface)

* `exec` — non-interactive execution
* `chat` — interactive REPL (if present)
* `sessions` — list/show sessions
* `resume` — resume last or specific session
* `config` — init/path/etc.
* `help`

### Options (current)

* `--system-prompt <PROMPT>`
* `--session <ID>`
* `--no-save`
* `--root <ROOT>` (if present):

  * interpreted as a working directory context (not a sandbox)
* `--format <text|json>`:

  * `text` (default): human-readable output
  * `json`: machine-readable output (schema versioned; may evolve in v0.x)

### Output channel rules (terminal-first)

* **stdout**:

  * assistant text (and only assistant text) in `--format text`
  * structured JSON events in `--format json`
* **stderr**:

  * logs, tool status lines, diagnostics, warnings, errors (human-readable)

### Exit codes (recommended contract)

* `0` success
* `1` general failure (provider/tool/parse)
* `2` config error (missing/invalid config)
* `130` interrupted (Ctrl+C)

Exact codes may evolve in v0.x; avoid breaking changes without a clear reason.

---

## 11) Reliability and UX requirements

### Streaming UX

* When streaming is enabled:

  * tokens must be printed as they arrive
  * flushing behavior should make the output feel immediate

### Cancellation

* Ctrl+C must not corrupt session files:

  * interruption should be recorded as a structured session event

### Timeouts

* Tool timeouts must return clean structured errors and allow the agent loop to continue or stop gracefully.

---

## 12) Versioning and compatibility promises

### v0.x rule

* ZDX may change quickly, but should avoid breaking user workflows without reason.

### What should remain stable as early as possible

* Session JSONL event types (or at least backward-readable)
* Core CLI commands (`exec`, `resume`, `sessions`)
* Output channel rules (stdout vs stderr)
* Engine event stream types (additive changes preferred)

Breaking changes, if necessary, should:

* be called out in release notes (with a migration note where feasible)
* optionally be reflected in `docs/ROADMAP.md` (if the project is using it)

---

## 13) How SPEC, ROADMAP, and PLAN documents relate

* **SPEC.md**: values + contracts + non-goals (this document)
* **docs/ROADMAP.md**: optional priorities list (what's next and why)
* **PLAN_vX.Y.md**: concrete, commit-level delivery plan (how to build it)
* **docs/adr/NNNN-*.md**: decision rationale over time (the “why”)

**Rule:** If present, `docs/ROADMAP.md` and `docs/PLAN_vX.Y.md` must not violate SPEC values (KISS/YAGNI, terminal-first, engine-first, YOLO default).
**Rule:** When a notable decision changes, add a new ADR that supersedes the old one; avoid rewriting past ADRs.

---

## 14) Glossary

* **Engine**: core agent loop producing events (no UI)
* **Renderer**: CLI/TUI that consumes engine events and displays them
* **Session (JSONL)**: append-only record of conversation and events
* **Tool loop**: model requests tool → tool runs → tool_result returned → model continues
* **YOLO mode**: permissive operation prioritizing speed and flow over guardrails

---

## 15) Project context (AGENTS.md)

### Purpose

ZDX automatically loads `AGENTS.md` files to provide project-specific guidelines to the model.
This enables per-project customization without modifying the global config.

### Loading order

AGENTS.md files are loaded hierarchically and concatenated in this order:

1. `$ZDX_HOME/AGENTS.md` — global user guidelines (always checked)
2. `~/AGENTS.md` — user home (only if project root is under home)
3. Ancestor directories from `~` to project root (only if project root is under home)
4. Project root (`--root` or cwd) — most specific

### Behavior

* Empty files are skipped silently.
* Unreadable files log a warning to stderr but don't fail.
* Loaded file paths are logged to stderr (per §10 output channel rules).
* Content is concatenated with path headers for clarity:

  ```markdown
  # Project Context

  ## /path/to/AGENTS.md

  <content>

  ## /another/path/AGENTS.md

  <content>
  ```

### Integration with system prompt

* Project context is appended to the system prompt (config or flag).
* If no system prompt is configured, project context becomes the system prompt.
* Content order: system prompt first, then project context.
