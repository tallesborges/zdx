# v0.2.x Implementation Plan

> **Source of truth:** `docs/SPEC.md` (contracts) + `docs/ROADMAP.md` (outcomes).
>
> **Plan rule:** Each step is ~1 commit: small, observable, tested.

---

## Starting point (assumptions)

This plan assumes the codebase already has:

- Streaming via Anthropic SSE parsing (with fixtures / wiremock tests).
- Session JSONL persistence + `resume`.
- `read` + `bash` tools + timeout support.
- Config: `model`, `max_tokens`, `system_prompt` / `system_prompt_file`, `tool_timeout_secs`.
- Repo-local `AGENTS.md` auto-inclusion in effective system prompt (basic).

If any of the above is missing on your branch, land it first (or adapt the steps below).

---

## v0.2.0 — Engine/renderer separation (SPEC-first)

**Outcome:** A UI-agnostic engine that emits `EngineEvent`s; CLI becomes a renderer enforcing stdout/stderr rules.

### Commit 1: `refactor(engine): introduce engine runner that emits EngineEvent`

**Goal:** Establish a single entrypoint that runs a "turn" and emits events without printing.

**Deliverable:** New `engine` module that drives provider + tool loop and emits `EngineEvent` via callback/sink; no direct stdout/stderr writes in engine code.

**New/updated event types (per SPEC §7):**
- `ToolRequested { id, name, input }` — emitted when model decides to call a tool (before execution)
- `ToolStarted { id, name }` — emitted when tool execution begins
- `ToolFinished { id, result }` — emitted when tool execution completes

**Tool result envelope (per SPEC §6):**
All tool outputs must use the structured envelope:
```json
{ "ok": true, "data": { ... } }
{ "ok": false, "error": { "code": "...", "message": "..." } }
```

**CLI demo command(s):**
```bash
cargo test
```

**Files changed:**
- create `src/engine.rs` (or `src/engine/mod.rs`) — engine runner API (streaming + tool loop)
- modify `src/lib.rs` — export engine module
- modify `src/agent.rs` — reduce to a compatibility wrapper (or remove after migration)
- modify `src/events.rs` (or equivalent) — add `ToolRequested` event type
- modify `src/tools/read.rs` — return structured envelope `{ ok, data: { path, content, truncated, bytes } }`
- modify `src/tools/bash.rs` — return structured envelope `{ ok, data: { stdout, stderr, exit_code, timed_out } }`

**Tests added/updated:**
- `tests/tool_use_loop.rs` — assert behavior unchanged (still passes)
- new unit tests for engine runner: "text only" and "tool_use" paths emit expected events
- assert `ToolRequested` is emitted before `ToolStarted`
- assert tool results match the structured envelope format

**Edge cases covered:**
- tool loop continues until final assistant response
- errors emitted as `EngineEvent::Error` before returning `Err`
- tool errors return `{ ok: false, error: { code, message } }`

---

### Commit 2: `refactor(cli): add CLI renderer consuming EngineEvent`

**Goal:** Make CLI output purely a function of events.

**Deliverable:** `CliRenderer` that:
- writes assistant deltas/final text to **stdout** only
- writes tool status + diagnostics + errors to **stderr** only

**CLI demo command(s):**
```bash
zdx exec -p "hello" --no-save
```

**Files changed:**
- create `src/renderer.rs` (or `src/render/cli.rs`) — `CliRenderer`
- modify `src/main.rs` — use engine + renderer (no printing in engine)

**Tests added/updated:**
- `tests/exec_mock.rs` — assert stdout contains only assistant text
- `tests/tool_use_loop.rs` — assert tool indicators go to stderr

**Edge cases covered:**
- empty deltas don't emit output
- final newline behavior is stable (1 newline after completion)

---

### Commit 3: `feat(session): add schema versioning and tool event persistence`

**Goal:** Make sessions resumable with full tool history (per SPEC §8).

**Deliverable:** Session JSONL now includes:
- `meta` event as first line with `schema_version: 1`
- `tool_use` events when model requests a tool
- `tool_result` events with tool execution output

**Session schema (per SPEC §8):**
```json
{ "type": "meta", "schema_version": 1, "ts": "2025-12-17T03:21:09Z" }
{ "type": "message", "role": "user", "text": "...", "ts": "..." }
{ "type": "tool_use", "id": "...", "name": "read", "input": { "path": "..." }, "ts": "..." }
{ "type": "tool_result", "tool_use_id": "...", "output": { ... }, "ok": true, "ts": "..." }
{ "type": "message", "role": "assistant", "text": "...", "ts": "..." }
```

**Timestamp format:** RFC3339 UTC (e.g., `2025-12-17T03:21:09Z`)

**CLI demo command(s):**
```bash
zdx exec -p "read src/main.rs"
zdx sessions show <ID>
cat ~/.config/zdx/sessions/<ID>.jsonl
```

**Files changed:**
- modify `src/session.rs` — add `meta`, `tool_use`, `tool_result` event types
- modify `src/session.rs` — write `meta` as first line on new session
- modify engine/session integration — persist tool events during tool loop

**Tests added/updated:**
- `tests/session_schema.rs` — assert new sessions start with `meta` event
- `tests/session_schema.rs` — assert tool_use/tool_result are persisted
- `tests/resume.rs` — assert resume works with tool history in context

**Edge cases covered:**
- existing sessions without `meta` event still load (backward compatible)
- interrupted sessions mid-tool-call are resumable

---

### Commit 4: `fix(interrupt): make Ctrl+C a pure signal; renderer prints`

**Goal:** Keep stdout/stderr ownership in the renderer.

**Deliverable:** Ctrl+C handler sets an interrupt flag only; renderer prints a single interruption line and exit code remains `130` (per SPEC §10).

**CLI demo command(s):**
```bash
zdx exec -p "write a long story"
# press Ctrl+C
echo $?
```

**Files changed:**
- modify `src/interrupt.rs` — remove `eprintln!` from signal handler
- modify `src/main.rs` / renderer — print interruption message to stderr
- modify `src/session.rs` — ensure `interrupted` event still appended (best-effort)

**Tests added/updated:**
- `tests/exec_mock.rs` — simulate interrupted path by toggling interrupt flag (unit-style)

**Edge cases covered:**
- double Ctrl+C still exits immediately
- interruption during tool execution doesn't corrupt session file

---

## v0.2.1 — Provider testability (offline)

**Outcome:** Provider calls are testable without network; errors are shaped predictably for stderr.

### Commit 5: `feat(config): add optional Anthropic base URL in config`

**Goal:** Support test rigs without requiring env var.

**Deliverable:** `config.toml` can set an optional Anthropic base URL; resolution order:
`ANTHROPIC_BASE_URL` env > config key > default `https://api.anthropic.com`.

**CLI demo command(s):**
```bash
ZDX_HOME=$(mktemp -d) zdx config init
rg -n "anthropic" "$ZDX_HOME/config.toml" || true
```

**Files changed:**
- modify `src/config.rs` — add `anthropic_base_url: Option<String>`
- modify `src/providers/anthropic.rs` — construct config from `Config` + env

**Tests added/updated:**
- `src/config.rs` — unit tests for base URL parsing + precedence
- `tests/exec_mock.rs` — ensure configured base URL is used when env is absent

**Edge cases covered:**
- empty string treated as unset
- invalid URL yields a config error message (exit `2`)

---

### Commit 6: `feat(provider): stable, stderr-friendly error shaping`

**Goal:** Ensure provider failures surface cleanly and consistently.

**Deliverable:** Provider errors map into a small set of error kinds (http_status, timeout, parse, api_error) and renderer prints a one-liner + optional details (still stderr-only).

**CLI demo command(s):**
```bash
ANTHROPIC_BASE_URL=http://localhost:9999 zdx exec -p "hello" --no-save
```

**Files changed:**
- modify `src/providers/anthropic.rs` — structured error type
- modify engine/renderer — ensure errors flow via `EngineEvent::Error`

**Tests added/updated:**
- `tests/exec_mock.rs` — assert stderr contains stable prefix and exit code is `1`

**Edge cases covered:**
- non-JSON error bodies
- mid-stream error events

---

## v0.2.2 — Tool: `write` (minimal authoring)

**Outcome:** Model can create/overwrite files deterministically (YOLO, but predictable).

### Commit 7: `feat(tools): add write tool (create/overwrite + mkdir -p)`

**Goal:** Implement the minimal authoring capability (per ROADMAP v0.2.2).

**Deliverable:** New tool `write` that:
- creates/overwrites a file at `path`
- auto-creates parent dirs
- returns a deterministic JSON result using the structured envelope (per SPEC §6)

**Tool schema (per SPEC §6):**

Input:
```json
{ "path": "string", "content": "string" }
```

Output:
```json
{ "ok": true, "data": { "path": "...", "bytes_written": 123 } }
```

**CLI demo command(s):**
```bash
zdx exec -p "Create hello.txt with 'hi'." --no-save
```

**Files changed:**
- create `src/tools/write.rs`
- modify `src/tools/mod.rs` — register + execute `write`

**Tests added/updated:**
- `src/tools/write.rs` — unit tests (creates dirs, overwrites, relative paths)
- new `tests/tool_write.rs` — integration test: model tool_use -> file created -> assistant continues

**Edge cases covered:**
- path escapes (allowed; YOLO), but errors are clear
- binary-like content (treat as UTF-8 text; document limitation)
- errors return `{ ok: false, error: { code, message } }`

---

## v0.2.3 — Tool: `edit` (surgical edits)

**Outcome:** Model can apply small, explicit edits without rewriting whole files.

### Commit 8: `feat(tools): add edit tool (exact replace; explicit failure modes)`

**Goal:** Enable reliable, reviewable text edits.

**Deliverable:** New tool `edit` that:
- reads file
- replaces an exact `old` string with `new`
- fails if `old` is missing or replacements != `expected_replacements` (default: 1)
- returns deterministic JSON result using the structured envelope (per SPEC §6)

**Tool schema (per SPEC §6):**

Input:
```json
{
  "path": "string",
  "old": "string",
  "new": "string",
  "expected_replacements": 1
}
```
- `expected_replacements` is optional (default: 1)

Output:
```json
{ "ok": true, "data": { "path": "...", "replacements": 1 } }
```

**Failure modes:**
- `old` text not found
- replacements count != `expected_replacements`
- file not readable/writable

**CLI demo command(s):**
```bash
zdx exec -p "In src/main.rs, replace 'foo' with 'bar'." --no-save
```

**Files changed:**
- create `src/tools/edit.rs`
- modify `src/tools/mod.rs` — register + execute `edit`

**Tests added/updated:**
- `src/tools/edit.rs` — unit tests (0 matches, 1 match, >1 matches)
- new `tests/tool_edit.rs` — integration test: tool loop requests edit and succeeds/fails deterministically

**Edge cases covered:**
- files with CRLF vs LF (exact match rules documented)
- large files (cap read size or document behavior)
- errors return `{ ok: false, error: { code, message } }`

---

## v0.2.4 — Terminal UX polish

**Outcome:** Cleaner stderr, better transcripts, and project instructions that stay predictable.

### Commit 9: `refactor(context): move AGENTS.md warnings into renderer`

**Goal:** Keep engine/context UI-agnostic (no direct printing).

**Deliverable:** Reading `AGENTS.md` never prints directly; warnings become `EngineEvent::Error` (or `EngineEvent::Warning` if added additively) and renderer decides how to show them.

**AGENTS.md behavior (per ROADMAP v0.2.4):**
- Deterministic file name: `AGENTS.md`
- Deterministic search path: `--root` then cwd
- Loaded once at session start
- Surfaced to stderr (e.g., "Loaded AGENTS.md from ...")
- Optionally disableable via flag/config (without being a "guardrail")

**CLI demo command(s):**
```bash
zdx exec -p "hello" --root . --no-save
```

**Files changed:**
- modify `src/context.rs` — return structured warning instead of `eprintln!`
- modify engine/renderer — surface warning to stderr without polluting stdout

**Tests added/updated:**
- `src/context.rs` — unit test: unreadable AGENTS triggers warning signal (not panic)

**Edge cases covered:**
- truncate very large `AGENTS.md` with a warning (document cutoff)

---

### Commit 10: `feat(sessions): improve sessions show transcript formatting`

**Goal:** Make `sessions show` easy to read and pipe.

**Deliverable:** Transcript format:
- preserves message order
- clearly separates user vs assistant
- includes tool_use/tool_result events (summarized or detailed)
- doesn't include noise

**CLI demo command(s):**
```bash
zdx sessions show <ID> | less -R
```

**Files changed:**
- modify `src/session.rs` — improve `format_transcript`
- modify `tests/sessions_list_show.rs` — update expectations

**Tests added/updated:**
- `tests/sessions_list_show.rs` — asserts stable formatting

**Edge cases covered:**
- empty session and missing session remain friendly

---

### Commit 11: `chore(release): bump version to 0.2.x and align CLI metadata`

**Goal:** Make the shipped artifact match the v0.2.x contracts.

**Deliverable:** Cargo + `clap` version fields reflect `0.2.0` (or `0.2.1`, etc.) and help output is consistent.

**CLI demo command(s):**
```bash
zdx --help | rg -n "0\\.2\\."
```

**Files changed:**
- modify `Cargo.toml` — bump version
- modify `src/cli.rs` — use `#[command(version)]` or align literal version
- modify `docs/ROADMAP.md` — move shipped v0.2.x items under "Shipped" as appropriate

**Tests added/updated:**
- `tests/cli_help.rs` — assert help shows the right version string

**Edge cases covered:**
- none

---

## Optional (only if still simple in v0.2.4)

### Commit 12: `feat(prompts): optional system prompt profiles`

**Goal:** Keep system prompts tidy without adding complexity.

**Deliverable:** `--profile <name>` loads `~/.config/zdx/profiles/<name>.md` (or `$ZDX_HOME/profiles/...`) and applies it as the system prompt (CLI flag wins over config).

**CLI demo command(s):**
```bash
mkdir -p ~/.config/zdx/profiles
echo "You are a Rust expert." > ~/.config/zdx/profiles/rust.md
zdx exec -p "Explain ownership" --profile rust --no-save
```

**Files changed:**
- modify `src/cli.rs` — add `--profile`
- modify `src/config.rs` — optional `default_profile`
- modify `src/context.rs` — resolve effective system prompt via profile
- modify `docs/SPEC.md` — document profile resolution rules (if shipped)

**Tests added/updated:**
- add `tests/system_prompt_profiles.rs` — asserts resolution order and file loading

**Edge cases covered:**
- missing profile file yields config error (exit `2`)
