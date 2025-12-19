# ZDX Specification

**Product:** ZDX (terminal-first agentic coding CLI)  
**Spec version:** living document  
**Status:** Source of truth for *values + contracts*. If `docs/ROADMAP.md` exists, it must not contradict `docs/SPEC.md`.
**Notation:** Sections labeled **Current (v0.1)** describe shipped behavior in this repo; sections labeled **Planned** describe intended future behavior and are not shipped yet.

---

## 0) Scope (CLI-first)

ZDX is a **CLI product first**.

- The primary UX is `zdx ...` commands in a terminal.
- Anything that does not improve the CLI’s day-to-day usefulness is out of scope for the current spec version.

### Success criteria (current)

ZDX is “working” when I can:
- run a prompt and stream output reliably
- use tools (`read`, `write`, `bash` now; `edit` planned) to inspect/modify files in a repo
- save/resume sessions in a predictable format
- pipe/redirect output like a normal UNIX CLI

### Non-goals (for now)

- Building a full TUI (ratatui/crossterm) or IDE-like UI
- Plugin systems / provider marketplace
- “Safety sandbox” / heavy permission systems beyond a simple root dir context
- Premature abstractions beyond the minimal multi-provider/tool interface

### Design constraints

- Engine is UI-agnostic, but **CLI is the only shipped UI** right now.
- Prefer fewer flags and fewer modes; add only when real usage demands it.

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

### CLI-first shipping
Ship CLI features that I will use this week. Everything else waits.

### YOLO default
- ZDX prioritizes speed and flow.
- By default, ZDX assumes the user is operating on their own machine and accepts risk.
- ZDX does not attempt to be a “safety sandbox”; any guardrails must remain **opt-in** and **low friction**.

### Engine-first (UI-agnostic)
- ZDX has a core "engine" that emits events.
- CLI is just a renderer over the event stream.
- Future TUI must consume the same engine events (no forked logic).

### UX inspiration
ZDX aims for an “iOS-like” CLI UX: strong defaults, consistent patterns, minimal configuration, and progressive disclosure of advanced options.

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

**Current (v0.1):** Anthropic Claude.

**Planned:** OpenAI, Gemini, OpenRouter.

### Provider interface (contract)

Regardless of provider, ZDX requires:

  * Non-streaming responses (baseline)
  * Streaming responses (SSE / incremental tokens)
  * Tool calling loop (`tool_use` → execute tool → `tool_result`)

### Key management

* API keys or access tokens are **never stored** in config files.

**Current (v0.1):** API keys are provided via environment variables:

  * `ANTHROPIC_API_KEY`

**Planned:** provider API keys use environment variables:

  * `OPENAI_API_KEY`
  * `GEMINI_API_KEY`
  * `OPENROUTER_API_KEY`

**Planned:** subscription-based login (OpenAI / Gemini / Claude) stores access tokens in an OS credential store (never in config/session files).

### Provider configuration surface (user-visible)

**Current (v0.1):**
* `model`
* `max_tokens`

**Planned:**
* `provider`
* `temperature`

Tool calling mode is always allowed if tools are enabled.

### Testability requirement

* Provider calls must be testable without network by allowing:

  * a base URL override (env or config)
  * deterministic fixture-driven stream parsing tests

**Current (v0.1):** Anthropic base URL override precedence is:
1) `ANTHROPIC_BASE_URL` env var (if set and non-empty)
2) `anthropic_base_url` in `config.toml` (if set and non-empty)

---

## 6) Tools contract

### Tool philosophy

* Tools are intentionally few, stable, and predictable.
* Tools are exposed to the model as JSON-schema definitions.
* Tool results must be deterministic and easy to parse.

### Tool set (current)

* `read` (filesystem)
* `write` (filesystem)
* `bash` (shell)
* `edit` (filesystem)

### Path resolution rules

* If a `path` is **absolute**, use it as-is.
* If a `path` is **relative**, resolve relative to `--root` (default: current working directory).
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
  { "path": "string" }
  ```

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

* **Path:** `data.path` is the canonicalized absolute path on disk.
* **Truncation (v0.1):** If the file content exceeds 50 KiB (51200 bytes), `content` is truncated to the first 50 KiB and `truncated: true`.

#### `write`

* **Purpose:** Write content to a file, creating or overwriting.
* **Input schema:**

  ```json
  { "path": "string", "content": "string" }
  ```

* **Output schema:**

  ```json
  {
    "ok": true,
    "data": {
      "path": "...",
      "bytes": 12345,
      "created": true
    }
  }
  ```

* **Path:** `data.path` is the resolved absolute path on disk.
* **Behavior:** Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories if needed (mkdir -p).
* **`created`:** `true` if the file did not exist before, `false` if overwritten.
* **Error codes:**
  - `invalid_input`: Missing or malformed input fields.
  - `mkdir_error`: Failed to create parent directories.
  - `write_error`: I/O or permission failure.

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

* **Shell invocation:** Commands run via `sh -c "<command>"` for POSIX portability.
* **Execution context:** Runs in the `--root` directory (default: current directory).
* **Timeout:** Controlled by `tool_timeout_secs` (config). If exceeded, `timed_out: true`, `exit_code: -1`, and `stderr` contains a one-line timeout message.
* **Output limits (v0.1):** stdout/stderr are not truncated.

#### `edit`

* **Purpose:** Edit an existing file by performing an exact string replacement.
* **Input schema:**

  ```json
  {
    "path": "string",
    "old": "string",
    "new": "string",
    "expected_replacements": 1
  }
  ```

  - `expected_replacements` defaults to `1` if omitted.

* **Output schema:**

  ```json
  {
    "ok": true,
    "data": {
      "path": "...",
      "replacements": 1
    }
  }
  ```

* **Path:** `data.path` is the resolved absolute path on disk.
* **Behavior:**
  - Reads the file as UTF-8 text (no newline normalization).
  - `old` must be non-empty.
  - Counts non-overlapping occurrences of `old` in the file.
  - If `count == 0`, returns `old_not_found`.
  - If `count != expected_replacements`, returns `replacement_count_mismatch`.
  - Otherwise replaces the text (exact match) and writes the updated file back.
* **Error codes:**
  - `invalid_input`: Missing/malformed fields; `old` is empty; or `expected_replacements < 1`.
  - `path_error`: Path does not exist (or cannot be resolved/canonicalized).
  - `read_error`: I/O failure reading the file (including non-UTF-8).
  - `write_error`: I/O failure writing the file.
  - `old_not_found`: No occurrences of `old` were found.
  - `replacement_count_mismatch`: Found occurrences != `expected_replacements`.

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
* `Error { kind, message, details? }` — structured error for renderers
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

* Session IDs are UUID v4 in hyphenated lowercase form (e.g., `550e8400-e29b-41d4-a716-446655440000`).

### Session UX requirements

* Resuming a session must continue from the full prior message history.
* Sessions must remain readable even if the process is interrupted mid-stream.

---

## 9) Configuration contract

### Location

* `~/.config/zdx/config.toml` or `$ZDX_HOME/config.toml`

### Format

* TOML

### Current keys (v0.1)

* `model` (string)
* `max_tokens` (int)
* `tool_timeout_secs` (int; `0` disables timeouts)
* `system_prompt` (string, optional)
* `system_prompt_file` (string, optional)
* `anthropic_base_url` (string, optional; empty string treated as unset)

### Planned keys

* `provider` (string)
* `temperature` (float)
* `openai_base_url` / `gemini_base_url` / `openrouter_base_url` (string, optional)

### Prompt resolution rules

* If both `system_prompt` and `system_prompt_file` exist:

  * **file wins** (explicitly)
* CLI flags override config file values.
* API key is env-only.

---

## 10) CLI contract

### Commands (v0.1)

* `zdx` — interactive chat (default when no subcommand is provided)
* `zdx exec -p, --prompt <PROMPT>` — run one prompt non-interactively
* `zdx sessions list`
* `zdx sessions show <SESSION_ID>`
* `zdx sessions resume [SESSION_ID]` — resume by id, or latest if omitted
* `zdx config path`
* `zdx config init`

### Global options (v0.1)

* `--root <ROOT>` (default: `.`): working directory context for tools and engine
* `--system-prompt <PROMPT>`: overrides config system prompt; an empty string clears both `system_prompt` and `system_prompt_file` for that run
* `--session <ID>`: append to an existing session id
* `--no-save`: disable session persistence

### Planned commands (not shipped)

* `zdx auth login` — login for subscription-based provider access
* `zdx handoff` — emit a handoff bundle for continuing work elsewhere

### Planned options (not shipped)

* `--provider <anthropic|openai|gemini|openrouter>`
* `--model <MODEL>`
* `--format <text|json>`

### Output channel rules (terminal-first)

**Agent commands** (`zdx`, `zdx exec`, `zdx sessions resume`):
* **stdout**: assistant text only (streaming)
* **stderr**: REPL UI, tool status lines, diagnostics, warnings, errors (human-readable)

**Utility commands** (`zdx sessions list/show`, `zdx config path/init`):
* **stdout**: command output
* **stderr**: errors/warnings

**Tool status details (v0.1):**
* Tool completion lines include duration: `Done. (X.XXs)`
* Bash tool debug lines:
  - On request: `Tool requested: bash command="<command>"`
  - On finish: `Tool finished: bash exit=<code>` or `Tool finished: bash timed_out=true`

### Planned: JSON output mode

* `--format json` emits a versioned stream of structured events to stdout suitable for piping and scripting.

### Planned: Handoff bundles

Handoff is the replacement for “compaction”: it does not overwrite history with a lossy summary.
Instead, it creates a focused starter prompt for the next thread/session.

* **Command:** `zdx handoff --goal "<GOAL>" [--from-session <SESSION_ID>] [--out <PATH>]`
* **Purpose:** Extract the minimum context needed to achieve `<GOAL>` and package it for starting fresh.
* **Default output:** Markdown to stdout (pipeable); `--out <PATH>` writes the same markdown to a file.
* **Source selection (contract):**
  - If `--from-session` is provided, handoff uses that saved session as the source.
  - Otherwise, handoff uses the latest saved session.
* **Bundle contents (contract):**
  - `Goal`: the exact goal string passed by the user
  - `Starter prompt`: a single prompt suitable to paste into a new `zdx exec -p ...` / new chat session
  - `Relevant files`: a short, explicit list of paths relative to `--root`
  - `Open tasks`: concrete next actions (checklist style)
  - `Repro commands`: copy/paste shell commands that reproduce current state or continue work
* **Reviewability:** The output is intended to be edited by the user before use (draft-first).

### Exit codes (v0.1)

* `0` success
* `1` runtime error (provider/tool/session)
* `2` CLI usage error (argument parsing)
* `130` interrupted (Ctrl+C)

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

* Tool timeouts must return clean, structured results and allow the agent loop to continue or stop gracefully.
* Timeout semantics are tool-specific (v0.1):
  - `bash`: `ok: true` with `timed_out: true`
  - `read`: `ok: false` with `error.code: "timeout"`

---

## 12) Versioning and compatibility promises

### v0.x rule

* ZDX may change quickly, but should avoid breaking user workflows without reason.

### What should remain stable as early as possible

* Session JSONL event types (or at least backward-readable)
* Core CLI commands (`exec`, default chat, `sessions`, `config`)
* Output channel rules (stdout vs stderr)
* Engine event stream types (additive changes preferred)

Breaking changes, if necessary, should:

* be called out in release notes (with a migration note where feasible)
* optionally be reflected in `docs/ROADMAP.md` (if the project is using it)

---

## 13) How SPEC, ROADMAP, and PLAN documents relate

* **SPEC.md**: values + contracts + non-goals (this document)
* **docs/ROADMAP.md**: optional priorities list (what's next and why)
* **docs/plans/plan_<short_slug>.md**: concrete, commit-level delivery plan (how to build it)
* **docs/adr/NNNN-*.md**: decision rationale over time (the “why”)

**Rule:** If present, `docs/ROADMAP.md` and `docs/plans/plan_<short_slug>.md` must not violate SPEC values (KISS/YAGNI, terminal-first, engine-first, YOLO default).
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
