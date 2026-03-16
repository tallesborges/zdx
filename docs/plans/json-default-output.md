# JSON Default Output for ZDX CLI

# Goals
- All finite ZDX CLI commands output JSON to stdout by default
- Structured errors in JSON to stderr
- Dates in ISO-8601 (RFC3339) format everywhere
- Agents (AI consumers) can reliably parse all finite CLI command output without flags
- List responses include `has_more` field so agents know if results were capped
- Rename `truncated` → `has_more` in tool outputs where semantics are "more results exist" (glob, grep), keep `truncated` where content is cut mid-stream (bash, read)

# Non-goals
- Human-readable format flag (`--human`, `--text`, etc.)
- Changing TUI output (remains interactive/human-facing)
- Defining a stable stdout API for long-running service commands (`bot`, `automations daemon`, future service runners)
- JSON schema versioning/OpenAPI spec generation

# Design principles
- User journey drives order
- Agent-first for finite commands: command results should be structured and deterministic
- Convention over configuration: no `--json` flag needed, it's always JSON
- Remove existing `--json` flags (they become redundant)
- Long-running services may emit logs/tracing, but logs are not the stable machine contract

# Operational taxonomy
- **Finite commands**: execute, return a result, and exit. These are the commands covered by this plan and should emit JSON by default.
- **Interactive modes**: keep a human session/UI alive (`resume`, chat TUI, future monitor TUI). These are not covered by the JSON stdout contract.
- **Service modes**: long-running background/service processes (`bot`, automation service runners). They may emit logs/tracing, but logs are not the stable machine contract.

# Output contracts
- **Structured result JSON**: finite commands that return a bounded result (`threads list`, `config path`, `worktree ensure`).
- **Structured streaming JSON**: finite commands that emit incremental progress/events during execution (`zdx exec`). Prefer compact JSONL with one event per line.
- **Logs / tracing**: observability output for service modes. Useful for humans/debugging, but not the stable machine contract.

# User journey
1. User runs `zdx threads list` → gets JSON array of threads on stdout
2. User runs `zdx automations runs` → gets JSON array of run records on stdout
3. User pipes output to `jq` for human reading: `zdx threads list | jq '.[] | .title'`
4. On error, stderr contains a JSON object with `ok: false` and structured error info
5. User runs `zdx config path` → gets JSON with the path
6. User runs any mutation command (`config init`, `threads rename`) → gets JSON confirmation

# Foundations / Already shipped (✅)

## JSON flag on select commands
- What exists: `zdx threads search --json` and `zdx automations runs --json` already emit JSON
- ✅ Demo: `zdx threads search --json` outputs pretty-printed JSON array
- Gaps: Only 2 commands support it; it's opt-in not default

## Serde derives on data types
- What exists: `ThreadSearchResult`, `AutomationRunRecord` already derive `Serialize`
- ✅ Demo: Used internally for JSONL persistence and `--json` output
- Gaps: Many command outputs are ad-hoc `println!` with no serializable struct

## Tool result envelope
- What exists: Tool results use `{ "ok": true, "data": ... }` / `{ "ok": false, "error": ... }` envelope
- ✅ Demo: Defined in SPEC.md §9
- Gaps: CLI commands don't use this envelope pattern

# MVP slices (ship-shaped, demoable)

## Slice 1: JSON output envelope + error helper
- **Goal**: Establish shared output types and helpers so all commands can emit consistent JSON
- **Scope checklist**:
  - [ ] Create `CliOutput<T: Serialize>` enum: `Success { data: T }` / `Error { code, message }` in `zdx-cli`
  - [ ] `CliOutput::print()` writes JSON to stdout (success) or stderr (error)
  - [ ] List responses include `has_more: bool` field — `true` when results were capped by a limit
  - [ ] Dates serialize as RFC3339/ISO-8601 strings
  - [ ] Top-level error handler in `main.rs` catches `anyhow::Error` and emits JSON error to stderr
  - [ ] Exit codes unchanged: 0 success, 1 error, 2 usage, 130 interrupted
- **✅ Demo**: `zdx threads show nonexistent` → stderr shows `{"ok":false,"error":{"code":"not_found","message":"..."}}`
- **Risks / failure modes**:
  - clap usage errors may bypass the handler → wrap clap error handling too

## Slice 2: Convert `exec` to structured streaming JSON by default
- **Goal**: `zdx exec` stays finite, but emits machine-readable JSONL events instead of mixed human text/log lines
- **Scope checklist**:
  - [ ] Replace text streaming on stdout with compact JSONL events (`assistant_delta`, `tool_started`, `tool_completed`, `turn_completed`, etc.)
  - [ ] Remove human-oriented debug/status lines from `stderr` in exec mode; emit structured events instead
  - [ ] Keep event ordering stable enough for agents/scripts to consume incrementally
  - [ ] Preserve thread persistence and tool loop behavior unchanged
  - [ ] Ensure the final event includes the accumulated result (`turn_completed` / final text)
- **✅ Demo**: `zdx exec -p "Say hello" | jq -c 'select(.type == "turn_completed") | .final_text'` prints the final assistant text from the JSONL stream
- **Risks / failure modes**:
  - Existing scripts/tests that grep raw assistant text or stderr status lines may need updates

## Slice 3: Convert `threads` subcommands to JSON output
- **Goal**: `threads list`, `threads show`, `threads search`, `threads rename`, `threads append` all emit JSON by default
- **Scope checklist**:
  - [ ] `threads list` → JSON array of `{ id, title, modified_at }` objects
  - [ ] `threads show` → JSON object with thread metadata + events array
  - [ ] `threads search` → JSON array (reuse existing `ThreadSearchResult`, remove `--json` flag)
  - [ ] `threads rename` → JSON `{ id, title }` confirmation
  - [ ] `threads append` → JSON `{ id, role, status }` confirmation
  - [ ] Remove `--json` flag from `threads search` (always JSON now)
- **✅ Demo**: `zdx threads list | jq '.[0].title'` returns the first thread's title
- **Risks / failure modes**:
  - `threads show` has a large transcript — ensure streaming/buffered write for big threads

## Slice 4: Convert `automations` subcommands to JSON output
- **Goal**: finite `automations` subcommands emit JSON by default
- **Scope checklist**:
  - [ ] `automations list` → JSON array of `{ name, source, schedule }` objects
  - [ ] `automations validate` → JSON array of `{ name, source, schedule, model, timeout_secs, max_retries, valid }` objects
  - [ ] `automations runs` → JSON array (reuse existing `AutomationRunRecord`, remove `--json` flag)
  - [ ] Remove `--json` flag from `automations runs` (always JSON now)
  - [ ] Explicitly leave `automations daemon` out of the JSON stdout contract (service mode; observability via tracing/logs/state)
- **✅ Demo**: `zdx automations list | jq '.[].name'` lists automation names
- **Risks / failure modes**:
  - `automations run <name>` currently routes through exec-like behavior; decide explicitly whether it becomes a finite command with a final JSON summary or stays outside this contract for now

## Slice 5: Convert remaining commands to JSON output
- **Goal**: All other non-interactive commands emit JSON
- **Scope checklist**:
  - [ ] `config path` → `{ "path": "..." }`
  - [ ] `config init` → `{ "path": "...", "created": true }`
  - [ ] `config generate` → `{ "content": "..." }` (TOML string in JSON)
  - [ ] `models update` → `{ "path": "...", "updated": true }`
  - [ ] `imagine` → `{ "files": ["path1", ...] }`
  - [ ] `login`/`logout` → `{ "provider": "...", "status": "..." }`
  - [ ] `telegram create-topic` → `{ "message_thread_id": N }`
  - [ ] `telegram send-message` / `send-document` → `{ "sent": true }`
  - [ ] `worktree ensure` → `{ "path": "..." }`
  - [ ] `worktree remove` → `{ "removed": "...", "branch_deleted": "..." }`
  - [ ] Explicitly leave interactive modes and service modes out of scope for this plan
- **✅ Demo**: `zdx config path | jq -r '.path'` prints the raw config path
- **Risks / failure modes**:
  - `login` has interactive prompts (stdin reads) — prompts go to stderr, only final result to stdout as JSON
  - `config generate` embeds TOML in a JSON string — may be awkward but is correct

## Slice 6: Structured JSON errors for all failure paths
- **Goal**: Every error path emits structured JSON to stderr, not bare text
- **Scope checklist**:
  - [ ] Wrap clap parse errors → `{ "ok": false, "error": { "code": "usage", "message": "..." } }` on stderr (exit 2)
  - [ ] Wrap panics with a panic hook → JSON error on stderr (exit 1)
  - [ ] Ensure SIGINT handler emits `{ "ok": false, "error": { "code": "interrupted" } }` on stderr (exit 130)
  - [ ] Audit all `eprintln!` / `warn!` calls in CLI commands — convert to JSON on stderr or remove
- **✅ Demo**: `zdx threads list --bad-flag` → stderr shows JSON error with code "usage"
- **Risks / failure modes**:
  - Panic hook JSON may fail if stderr is broken — acceptable edge case

## Slice 7: Rename `truncated` → `has_more` in glob and grep tool outputs
- **Goal**: Use correct semantics in tool outputs — `has_more` for "more results exist beyond the cap", keep `truncated` only for "content was cut mid-stream" (bash, read)
- **Scope checklist**:
  - [ ] `glob.rs`: rename `truncated` field to `has_more` in JSON output
  - [ ] `grep.rs`: rename `truncated` field to `has_more` in JSON output
  - [ ] Update all tests in glob.rs and grep.rs that assert on `truncated` → `has_more`
  - [ ] Update TUI cell.rs rendering if it reads these fields (grep/glob truncation display)
  - [ ] Update SPEC.md tool output docs if they reference the field name
  - [ ] `bash.rs` and `read.rs`: keep `truncated` (correct semantics — content cut mid-stream)
- **✅ Demo**: Run glob with many files → output has `"has_more": true` instead of `"truncated": true`
- **Risks / failure modes**:
  - System prompt or AGENTS.md references `truncated` for glob/grep — search and update
  - LLM tool descriptions mention `truncated` — update tool descriptions in SPEC.md

# Contracts (guardrails)
- All stdout from finite, non-interactive commands is valid JSON (parseable by `jq`)
- Error output on stderr is valid JSON with `{ "ok": false, "error": { "code": "...", "message": "..." } }`
- Exit codes unchanged: 0/1/2/130
- TUI mode (`zdx` with no subcommand) is unchanged
- `zdx exec` is a finite command and should emit structured streaming JSON on stdout
- Dates are RFC3339 UTC strings
- Service modes are outside this stdout JSON contract; they may emit logs via tracing/stderr for observability
- Interactive modes and service modes are out of scope for this plan

# Key decisions (decide early)
- **Envelope shape**: Use `{ "ok": true, "data": ... }` / `{ "ok": false, "error": { "code", "message" } }` matching existing tool envelope (SPEC.md §9) — consistent across the project
- **`zdx exec` contract**: Treat `exec` as a finite command with a structured streaming JSON contract (JSONL events), not as a special text-mode exception
- **Pretty vs compact**: Use compact JSON by default (single line). Agents don't need pretty-printing. Removes the `serde_json::to_string_pretty` calls
- **Warnings**: Warnings during finite command execution go to stderr as JSON `{ "ok": false, "error": { "code": "warning", "message": "..." } }` — or drop warnings entirely since agents don't need them
- **`has_more` vs `truncated`**: Use `has_more` when the output is a capped list of results (glob, grep, CLI list commands). Keep `truncated` only when content is literally cut mid-stream (bash stdout/stderr, read file content). This makes semantics clearer for agent consumers
- **Mode split**: Finite commands get the JSON default contract. Interactive modes and service modes each get their own behavior/contract and should not be forced into the finite-command JSON model.
- **Streaming output**: Streaming is an output behavior, not a lifecycle category. Finite commands may return either a bounded JSON result or a structured JSON event stream.
- **Long-running observability**: Service modes may log useful debug/runtime information, but those logs are not a stable machine contract. Service inspection/control should come from tracing + structured state + future TUI surfaces

# Testing
- Manual smoke demos per slice: pipe each command through `jq .` to verify valid JSON
- Integration tests in `crates/zdx-cli/tests/`: assert stdout is valid JSON for key commands
- Regression test: `zdx threads list` output parses as JSON array
- Regression test: invalid command → stderr is valid JSON with exit code 2

# Polish phases (after MVP)

## Phase 1: Consistent field naming audit
- Audit all JSON output for consistent naming conventions (snake_case throughout)
- Ensure no stale `println!` or `eprintln!` calls remain in CLI command paths
- ✅ Check-in demo: `grep -r 'println!' crates/zdx-cli/src/cli/commands/` returns zero hits

# Later / Deferred
- **JSON schema documentation**: Generate or document JSON schemas for each command's output. Revisit when a third-party consumer needs it.
- **`--format text` flag**: Add back human-readable output if users complain about raw JSON. Revisit if dogfooding reveals frequent pain.
- **JSON streaming (JSONL)**: For commands with large output, emit one JSON object per line instead of a single array. Revisit when performance is an issue.
- **Service control plane TUI**: Separate plan for a non-chat TUI focused on bot/automation service status, config inspection, logs, and start/stop/restart controls. Revisit as the next major TUI surface.
