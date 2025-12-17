# ZDX Specification

**Product:** ZDX (terminal-first agentic coding CLI)  
**Spec version:** v0.2.x (living document)  
**Status:** Source of truth for *values + contracts*. ROADMAP.md must not contradict SPEC.md.

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

These are explicitly *out of scope* for v0.x unless the roadmap promotes them later:

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

* Primary provider for v0.2.x: **Anthropic Claude**
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

### Tool set (target)

ZDX aims for a minimal tool set:

* `read` (filesystem)
* `bash` (shell)
* later: `write`, `edit` (authoring)

### Tool definitions (current / v0.2.x)

#### `read`

* **Purpose:** Read file contents.
* **Input schema:**

  * `{ "path": "string" }`
* **Output:** plain text string (file contents) OR structured error.
* **Path behavior:**

  * Paths are resolved relative to the current execution context.
  * If `--root` exists, it is treated as a **working directory context**, not a security boundary (YOLO).
  * Path canonicalization is allowed for correctness, not for sandboxing guarantees.

#### `bash`

* **Purpose:** Execute a shell command.
* **Input schema:**

  * `{ "command": "string" }`
* **Output schema (recommended):**

  * `{ "stdout": "string", "stderr": "string", "exit_code": number }`
* **Execution context:**

  * Runs in the current directory or `--root` directory if provided.
* **Timeout:**

  * Controlled by `tool_timeout_secs` (config)

### Tool loop correctness requirements

* When the model requests a tool, ZDX must:

  1. Execute the tool
  2. Return a `tool_result` that corresponds to the correct `tool_use_id`
  3. Continue until the model ends the turn

---

## 7) Engine event stream contract

The engine emits events for renderers (CLI now, TUI later).

### Required event types (v0.2.x)

* `AssistantDelta { text }` — incremental text chunk
* `AssistantFinal { text }` — completed message
* `ToolStarted { id, name }`
* `ToolFinished { id, result }`
* `Error { message }`
* `Interrupted`

### Renderer rules

* The renderer must be able to:

  * render streaming text as it arrives
  * render tool activity indicators
  * render errors cleanly
  * resume sessions from persisted events/messages

### Persistence mapping

* Important engine events should be persistable as JSONL session events
  (at minimum: messages, interruptions; later: tool events).

---

## 8) Session and persistence contract

### Storage location

* Default base directory: `~/.config/zdx/`
* Override base directory: `$ZDX_HOME`
* Sessions directory:

  * `~/.config/zdx/sessions/` or `$ZDX_HOME/sessions/`

### File format

* Sessions are **JSONL (append-only)** event logs.

### Minimum event schema (current)

* Message:

  ```json
  { "type": "message", "role": "user", "text": "...", "ts": "..." }
  { "type": "message", "role": "assistant", "text": "...", "ts": "..." }
  ```
* Interrupted:

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

### Current keys (v0.2.x)

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

### Output channel rules (terminal-first)

* **stdout**:

  * assistant text (and only assistant text) in default `--format text`
  * machine output if `--format json`
* **stderr**:

  * logs, tool status lines, diagnostics, warnings, errors (human-readable)

### Exit codes (recommended contract)

* `0` success
* `1` general failure (provider/tool/parse)
* `2` config error (missing/invalid config)
* `130` interrupted (Ctrl+C)

(Exact codes may evolve, but once committed in ROADMAP v0.5+, they should be treated as stable.)

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

* be announced in ROADMAP.md
* include a migration note in release notes

---

## 13) How SPEC, ROADMAP, and PLAN documents relate

* **SPEC.md**: values + contracts + non-goals (this document)
* **ROADMAP.md**: high-level versions and outcomes (what's next and why)
* **PLAN_vX.Y.md**: concrete, commit-level delivery plan (how to build it)

**Rule:** ROADMAP and PLAN must not violate SPEC values (KISS/YAGNI, terminal-first, engine-first, YOLO default).

---

## 14) Glossary

* **Engine**: core agent loop producing events (no UI)
* **Renderer**: CLI/TUI that consumes engine events and displays them
* **Session (JSONL)**: append-only record of conversation and events
* **Tool loop**: model requests tool → tool runs → tool_result returned → model continues
* **YOLO mode**: permissive operation prioritizing speed and flow over guardrails
