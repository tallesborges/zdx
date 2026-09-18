# ZDX Specification

**Product:** ZDX (TUI-first terminal assistant for developers)  
**Status:** Source of truth for *vision + user-visible contracts*.

---

## 1) Vision

ZDX is a **daily-driver terminal app** you can keep open all day: calm, fast, and focused on developer productivity.

The TUI is the product. A CLI mode exists to support automation and scripting.

---

## 2) Why

**Learning by building.** This project exists to explore how agentic coding tools work by implementing one from scratch.

**Developer UX is the priority.** Every feature should reduce friction and help developers move faster. If it doesn't improve the daily workflow, it doesn't belong.

Terminal AI tools often break the parts that matter daily:
- flicker/jank from naive redraw
- resize bugs and "lost" history
- mixed stdout/stderr corrupting the screen
- weak transcript UX (scroll/select/copy)
- no durable history you can trust

ZDX solves this with a boring, reliable core:
- own the viewport (TUI)
- transcript as the source of truth
- UI-agnostic agent (events)
- deterministic tools
- append-only thread log

---

## 3) Goals

### Primary: `zdx` (interactive)

- Full-screen terminal chat UI that stays stable under resizes, overlays, long threads, and continuous streaming.
- Transcript UX: scroll, select, copy.
- Threads persist and replay deterministically.
- **Queued prompts:** when a turn is streaming, submitting a normal prompt enqueues it. The next queued prompt auto-sends when the turn ends. A small queue panel appears between transcript and input (first 3 prompts, 30-char summaries). Queue is in-memory only.
- **Side questions (`/btw`):** the user can open a popup, ask a side question from the latest stable thread context, and ZDX runs it in a background forked thread without interrupting the current run. The result is available later in thread history.

### Secondary: `zdx exec ...` (non-interactive)

- Script-friendly execution with clean stdout/stderr separation.
- Same agent, different renderer.

---

## 4) Non-goals

- Cooperating with terminal scrollback while the TUI is running.
- Guaranteeing stdout piping while the TUI is active (use `exec` for that).
- Terminal-dependent rendering tricks (scroll regions / partial clears) as a correctness mechanism.
- IDE ambitions (file tree, refactor UI, indexing) in early versions.
- Safety sandboxing as a primary product goal (YOLO default).

---

## 5) Principles

- **Developer UX is the priority:** features that improve daily workflow win. Ship fast unless it degrades core UX.
- **TUI-first UX:** optimize for reading/navigation/editing in a full-screen terminal app.
- **KISS/YAGNI:** ship the smallest daily-driver value; refactor only after usage proves shape.
- **Ship-first:** get it working, ship it, learn from usage. Refactor when the shape is proven.
- **User journey drives order:** build in the order the user experiences it: start → input → submit → see output → stream → scroll/navigate → follow-up interactions → polish.
- **Learn by doing:** explore TUI tech hands-on; accept messy code as part of the learning process.
- **YOLO default:** no guardrails, prioritize speed and flow.

---

## 6) Product Surface (CLI)

**Shipped commands (v0.1):**
- `zdx` — interactive chat (TTY)
- `zdx bot` — run the global Telegram bot from `[telegram]` in `$ZDX_HOME/config.toml`
- `zdx bot init` — create/update global Telegram bot settings in `$ZDX_HOME/config.toml`
- `zdx bot profile add <NAME> <CHAT_ID> <CWD>` — map a Telegram chat to a project cwd via `telegram.profiles.<NAME>`
- `zdx exec -p, --prompt <PROMPT> [--no-system-prompt] [--subagent NAME]` — run one prompt non-interactively
- `zdx imagine -p, --prompt <PROMPT> [--out PATH] [--model MODEL] [--aspect RATIO] [--size SIZE]` — generate images with Gemini image models
- `zdx mcp servers|auth <SERVER>|logout <SERVER>|tools <SERVER>|schema <SERVER> <TOOL>|call <SERVER> <TOOL> --json '{...}'` — inspect, authenticate, and call configured MCP servers through the helper CLI
- `zdx automations list|validate|daemon|runs [NAME] [--date*] [--json]|run <NAME>`
- `zdx service install|uninstall|start|stop [bot|daemon|all]`, `zdx service restart [bot|daemon|all] [--force]`, `zdx service status [--json]`, `zdx service logs [bot|daemon|all] [--lines N] [--err]` — manage the long-lived `bot`/`daemon` services under launchd (macOS)
- `zdx threads list [--all]|show <ID>|resume [ID]|search [QUERY] [--date*] [--limit N] [--json]|tools [TOOL] [--failed] [--date*] [--limit N] [--json]`
- `zdx config init|path`

**Exit codes:** `0` success, `1` runtime error, `2` CLI usage error, `130` interrupted.

---

## 7) Output Contracts

### `zdx exec` (non-interactive, scriptable)

- **stdout:** assistant text only (or JSON if/when `--format json` ships).
- **stderr:** diagnostics, warnings, tool status, errors.
- `--no-system-prompt` disables all system/context composition for that run (config system prompt, `AGENTS.md`/`CLAUDE.md`, memory, skills).
- `--subagent <NAME>` runs the prompt under a named subagent (`explorer`, `oracle`, or any discovered subagent): the subagent's rendered prompt becomes the run's system prompt, and its `model` spec and `tools` apply as defaults. An explicit `-m provider:model[@thinking][@fast]` or tool override still wins. Omitting the flag runs default exec behavior. Conflicts with `--no-system-prompt`; unknown names fail the run.

### `zdx imagine` (non-interactive, scriptable)

- **stdout:** generated image file path(s), one per line.
- **stderr:** diagnostics and errors.

### `zdx mcp ...` (non-interactive helper, scriptable)

- **stdout:**
  - `servers`, `tools`, and `schema` print structured JSON inspection data.
  - `call` prints the normal ZDX `ToolOutput` JSON envelope.
  - `auth` and `logout` print human-readable status/instructions.
- `servers` may report MCP server states such as `loaded`, `auth_required`, or `failed`.
- **stderr:** CLI usage/runtime errors.

### `zdx service ...` (macOS/launchd)

- launchd owns the lifetime of the `bot` and `daemon` services: start at login, restart on crash. Telegram `/restart` restarts the daemon first, then exits the bot so launchd restarts it. `/restart f` (also `force`, `--force`) explicitly allows interrupting active runs; `/restart q` (also `queue`) waits until no agent run is active anywhere and then performs the gated restart automatically, announcing it in the topic that queued it. Only one queued restart is pending at a time; a second `/restart q` reports that. `/restart` in any mode bypasses the per-topic queue, so it is answered while a turn runs.
- Agents run `~/.local/bin/zdx` (the `just install` target), never the calling binary, so `restart` always picks up the currently installed build.
- Agents are launched via `zsh -c 'exec …'` so `~/.zshenv` is sourced; launchd sources no shell startup files, and provider API keys live there.
- `install` refuses when the service is already running outside launchd; PID-file uniqueness continues to prevent duplicate instances.
- `stop` is durable: the service stays stopped until an explicit `start`, across reboots.
- `restart` waits for the old process to exit before the replacement starts, and reports `PID old → new`.
- `restart` refuses while any agent run is active across ZDX surfaces. A multi-service restart checks once before changing either service. CLI `--force`, Telegram `/restart f`, and Monitor `R` bypass the guard and may interrupt those runs; Monitor `r` remains guarded. A blocked Telegram `/restart` lists `/restart q` and `/restart f` as the two ways forward.
- Service stdout/stderr are captured to `$ZDX_HOME/run/logs/{bot,daemon}.{out,err}`.
- Plists set `ZDX_SERVICE_SUPERVISOR=launchd`; the bot uses this to self-mark as supervised so `/restart` is honored with no monitor running.
- `zdx monitor` is a control panel over the same operations, not an independent supervisor; it never spawns service processes itself.

### `zdx` (interactive)

- Full-screen alt-screen TUI; **does not print transcript to stdout while active**.
- Any diagnostics are shown in the UI; optional file logging is acceptable.

### Provider retries

- Before visible assistant output or tool activity begins, ZDX automatically retries transient provider failures up to three times with exponential backoff.
- Typed transport failures, timeouts, HTTP `408`, HTTP `429`, HTTP `500..=599`, and known provider overload/rate-limit codes are transient.
- Request construction, parsing/protocol failures, authentication, permission, quota, billing, and usage-limit failures are terminal and are not automatically retried.
- Structured transport kind, HTTP status, and provider code/type take precedence. Text matching is used only for unknown or unstructured provider/gateway errors.
- Once visible output or tool activity begins, provider failures stop the turn instead of transparently retrying and risking duplicate output or tool execution.

---

## 8) Threads

Threads are append-only **JSONL** event logs (thread events are never modified or deleted).

### Storage

- Base dir: `$ZDX_HOME` (if set) else `~/.zdx`
- Threads dir: `<base>/threads/`
- OAuth cache: `<base>/oauth.json` (0600 perms)
- MCP OAuth cache: `<base>/mcp_oauth.json` (0600 perms)
- `zdx bot` resolves Telegram credentials/settings from `[telegram]` in `config.toml`. `[telegram]` carries identity, routing, and the optional Mini App server (`bot_token`, allowlists, `profiles`, `server`); the bot's model and thinking level come from the layered config like every other surface.
- Telegram bot chat profiles live under `telegram.profiles.<name>` in `config.toml` with `chat_id` and `cwd`; matching chats run agent turns from the profile cwd, and unprofiled allowed chats keep using the bot root fallback. A profile plays one of two roles. A **workspace** profile (the default) owns its `cwd` for worker-mirror routing and workspace skill attribution. An **orchestrator** profile (`orchestrator = true`) is a management home base: it keeps the same chat→root binding for its own turns but is excluded from both, and takes an optional `worker_root` (defaulting to `cwd`) used as the default root for workers it creates without one. A management group and a workspace group may therefore share a `cwd`, with only the workspace owning it. Two chats sharing a `cwd` also resolve the same workspace-layer model defaults; per-topic `/model` and the `[subagents.overrides.orchestrator]` override apply on top, unchanged.
- `telegram.profiles` is rejected at load when: two profiles share a `chat_id`; two **workspace** profiles resolve to the same canonical root (mirror routing and skill attribution would depend on profile name order); a `worker_root` is blank or is not an existing directory; or a configured orchestrator group's effective `worker_root` is not covered by any workspace profile (every worker without a project root of its own would silently lose its mirror). Legacy DM orchestrators have no profile and are unaffected. `zdx bot profile add` and any profile re-save write `orchestrator` and `worker_root` back, so neither is dropped on rewrite.
- Each Telegram profile gets its own layered config anchored at the profile `cwd`, so a workspace `.zdx/config.toml` applies to chats bound to that profile. Profile configs are built once at startup; unprofiled chats use the bot-level config. Runtime `/model` changes in a General topic are workspace-scoped: they write the unified model spec to the chat root's overlay and update only that chat's config.
- Every bot-created Telegram forum topic starts with a status-style thread header. The bot pins it silently when it has `can_pin_messages`, keeps the topic usable if pinning fails, and provides a refresh action plus an Open Thread button when the Threads Mini App is configured. Resumed topics display and open their effective source thread.

### Format

- First line is `meta` with `schema_version`, optional `title`, and optional lineage fields (`origin_kind`, `parent_thread_id`, `subagent_name`) for threads spawned by another agent run.
- Timestamps are RFC3339 UTC.
- Event types: `meta`, `message`, `tool_use`, `tool_result`, `interrupted`, `reasoning`, `usage`, `notice`.
- `tool_use` events carry `id_origin` (`real` when the provider emitted the id, `synthesized` when zdx generated one because the provider omitted it; default `synthesized` for old transcripts) and an optional `replay` token (e.g. Gemini per-part `thoughtSignature`). Replay metadata is preserved verbatim so multi-turn provider caches (e.g. Gemini's implicit prompt cache) can hit on subsequent turns.
- `tool_result` events carry optional `duration_ms`, the client-observed execution duration for a real tool call. It is absent on older transcripts and synthetic results that did not execute a tool. Parallel results remain persisted in request order; summing their durations measures tool work, not wall time.
- `usage` events carry optional `model` and `provider` fields recording which model/provider produced that usage, so token/cost can be attributed per provider even when the model is switched mid-thread. Both default to absent on older transcripts (attribution then falls back to the thread's model). A request's terminal `usage` event also carries optional `duration_ms` (wall-clock request time) and `ttft_ms` (time-to-first-token) for latency/throughput stats; both are absent on interim/failed usage and on older transcripts. Adding these fields is additive and does not bump `schema_version`.
- `message` and `reasoning` events also carry an optional `replay` token for the same reason.
- Every request builder (Anthropic wire, chat-completions, OpenAI Responses, Gemini) applies one shared reasoning-replay rule: a thread's reasoning is replayed only for the in-flight turn — the assistant messages at or after the last genuine user turn (tool-result carriers use the `user` role and do not start a turn). Older turns' reasoning is dropped from the request; an assistant message left with no content at all is dropped with it. The rule is single-sourced in `zdx_types::ReasoningReplay` and matches each provider's own documented scope (OpenAI: "since the last `user` message"; Gemini: "only current turn is required; we don't validate on previous turns"; Anthropic: prior-turn `thinking` blocks may be omitted). The one exception is a redacted thinking block, whose opaque payload is always echoed back verbatim. Replaying every prior turn's reasoning grows a request without bound on backends that do not filter prior-turn thinking server-side, and can push a long thread past its context limit. Per-part replay tokens that are not reasoning — a Gemini `thoughtSignature` on a `functionCall`/`text` part, a per-part Gemini signature — are unaffected, since providers require those on the current turn's parts and dropping earlier ones is not validated. Because the drop is deterministic and applied on every request, a kept block's prefix still matches the request that produced it, so prompt-cache prefixes stay stable across turns.
- A reasoning block persisted above 128k characters is truncated head-and-tail with a marker, and its replay token is dropped (a truncated block no longer matches its signature).
- `notice` events (e.g. model `refusal`, `model_context_window_exceeded`, `max_tokens`, runaway reasoning) are persisted for UI replay and MUST NOT be rehydrated as conversation messages sent back to providers.
- Child runs spawned by another agent — user-visible subagents (`invoke_subagent`) and internal helpers (title, tldr, handoff, prompt-builder, `read_thread`) — persist their own thread JSONL tagged with `origin_kind` (e.g. `subagent`, `helper:title`) plus `parent_thread_id`/`subagent_name`. These threads are hidden by default from `zdx threads list`, the TUI thread picker, `thread_search`, the monitor dashboard, and native memory export (use `zdx threads list --all` to include them), but their token usage IS counted by `zdx stats`. `zdx threads show <id>` displays lineage: a parent-link header when the thread is itself a child, and a "Child runs" section listing each spawned child with its tokens and cost.
- Threads remain readable even if interrupted mid-stream.

### Durability

- The persistence layer flushes ordered batches at tool-turn boundaries (after each `process_tool_turn` completes) and at terminal `TurnFinished`. Streaming text/reasoning/tool-input deltas are no longer persisted directly.
- A hard crash can lose any assistant content streamed since the last checkpoint or terminal flush; completed prior tool turns are durable. Long tool loops persist incrementally between turns, so an interrupted multi-tool run keeps everything up to the last completed tool turn.
- Within a flushed batch, blocks are persisted in the exact stream order the provider produced them. This preserves Gemini's per-part replay fidelity and is required for implicit-cache hits on the next turn.

### Metadata Updates

The `meta` line (first line only) may be rewritten atomically to update thread metadata (e.g., `title`). This uses write-to-temp-then-rename for safety. Thread events after the meta line are never modified.

### Automation sessions

- Manual and daemon runs persist to timestamped thread IDs by default: `automation-<name>-<YYYYMMDD-HHMM>`.
- `zdx automations run <name> --thread <ID>` uses the explicit thread ID instead.
- `--no-thread` disables persistence for that run.
- Automation frontmatter may include `subagent: <name>` to run with a named subagent prompt/tool/model configuration.

---

## 9) Tools

Tools are intentionally few, stable, and machine-parseable.

### `Bash` execution modes

Three intents, each with its own input and lifetime. They never silently substitute for one another.

- **Foreground (default).** The command is waited on inline and its output streams to the surface as it arrives.
- **`background: true`.** Detached at spawn in its own session; outlives the turn and the zdx process. Rejects a positive `timeout_secs`. Unchanged by the foreground bound.
- **`timeout_secs > 0`.** An explicit hard kill deadline. The command is killed at the deadline and the result carries `timed_out: true`. This is the only way to ask for a kill, and it takes precedence over the foreground bound.

**Auto-background handoff (Unix).** A foreground command runs under `bash_foreground_bound_secs` (default 120; `0` disables it and restores an unbounded wait). The bound is a relocation bound, not a deadline: when it expires the command **keeps running and is moved to the background**, never killed.

The handoff applies only when no kill deadline is in force and the run is on a surface that can keep the job alive:

- A per-call `timeout_secs` suppresses it: a kill the caller asked for is never silently converted into a relocation. There is no global tool-timeout setting; the only other source of a tool-call deadline is an automation's `timeout_secs` frontmatter, which bounds tool calls for that automation run only.
- An adopted job is held by its owning process's supervisor lease, so that process must not exit while the job runs. A surface qualifies either by outliving the run — the interactive TUI and the Telegram bot daemon — or by **draining**: `zdx exec`, which is also how every `invoke_subagent` child and orchestrator worker runs, waits for adopted jobs to finish before the process exits. Unknown surfaces are excluded and keep the unbounded foreground wait.

**Draining (`zdx exec`).** When a turn ends with adopted jobs still running, the process stays alive until they finish. It holds the lease throughout, the jobs' reader tasks keep appending to their logs, and the process exits only once every job has exited and its log is complete. Draining never kills a job and has no deadline, so a legitimately long build is waited out in full. While waiting it logs the pending job count and pids at `warn` level, so a human watching a process that has not exited can see why. Interrupting during the drain is an explicit request to stop: the jobs are torn down through their leases — TERM → grace → KILL → group sweep — so nothing is orphaned, and a second interrupt force-exits, after which the supervisor's own lease cleanup applies.

**Limitation: a parent cannot reach a child's job.** `background_output` and `background_kill` resolve a `bg_id` only within the thread that created it, and subagents are not given those tools at all. A job adopted inside a subagent or orchestrator worker is therefore invisible to the parent: the child waits it out at exit, but nobody can poll or stop it in the meantime. Changing that requires a cross-thread ownership model for background jobs and is deliberately out of scope.

Behavior at the boundary:

- Completion is resolved before the bound, so a command that finishes at the bound is reported as completed rather than relocated.
- That race is settled on observation, not wall-clock truth: a command finishing microseconds before the bound may still be reported as `backgrounded` with `exit_code: null`. It is not lost — the job is registered, its exit is recorded, and the real exit code and full output surface on the first `background_output` poll, which will show `status: "exited"`.

- The handoff retains the invocation's existing supervisor, session, process group, and lease — nothing is re-parented, re-spawned, or signalled — so a long build or test run completes unharmed. Only the owner of the handle moves.
- The tool call returns a **success** carrying `backgrounded: true`, a `bg_id`, `pid`, `pgid`, `status: "running"`, `exit_code: null`, `elapsed_secs`, the output captured before the handoff, `stdout_log`/`stderr_log`, and a message instructing the model not to re-run the command.
- Output capture continues into the background logs. Bytes captured before the handoff are flushed into the log first, so the log holds the complete stream and the partial output appears in both places. Streaming deltas stop at the handoff, so no output event can arrive after the tool result.
- The job is registered with `mode: "adopted"` and is visible to `background_output` and `background_kill` like any other background job. Killing it closes its lease, which runs the supervisor's TERM → grace → KILL → group-sweep path; the lease is authoritative about identity, so no PID-reuse guard applies.
- Adopted jobs are **session-scoped**: they belong to the zdx process that adopted them, unlike `background: true`, which detaches at spawn and outlives zdx. A long-lived surface simply keeps them; a one-shot run drains them before exiting. Either way the process never exits out from under a running job. If the owning process dies anyway (crash, `SIGKILL`, second interrupt), the lease closes and the supervisor terminates the job, and the registry's liveness scan reaps the record to a tombstone.
- Degraded bookkeeping never kills the command. If the registry marker cannot be written, or the output logs cannot be opened, the job keeps running and the result reports `tracking_failed: true` (plus `logging_failed: true` when output is no longer being captured) alongside its `pid`.
- `background_output` stamps every read with a strictly increasing `read_seq`, so polling an unchanged job never looks like a repeated identical tool call to the turn's loop detector.
- Non-Unix builds have no supervisor or lease; auto-backgrounding does not apply there and `timeout_secs` remains the only bound.

### `telegram`

- Posts into a Telegram chat or forum topic other than the current conversation. One tool with an `action` enum: `send_message`, `send_document`, `create_topic`.
- Requires `chat_id`; `message_thread_id` selects a forum topic and is omitted for the group's General topic. `create_topic` returns the new topic id.
- `send_message` defaults to `parse_mode: html` and sends text raw — the Markdown conversion on the normal reply path does not apply. Body is capped at 4096 characters and captions at 1024; both are rejected locally with a clear message rather than by the Telegram API.
- `send_document` resolves paths like other file tools. An optional `bot_token` overrides the configured bot; otherwise the token resolves from `telegram.bot_token`, then `ZDX_TELEGRAM_BOT_TOKEN`, then `TELEGRAM_BOT_TOKEN`.
- Needs no shell, so the orchestrator can post to other topics; it previously could not.
- Shares its implementation with the `zdx telegram` CLI subcommands, which are unchanged apart from `--parse-mode markdown` now sending Markdown instead of HTML.
- Posting is externally visible and cannot be cleanly unsent; the tool description instructs confirming the destination unless the user named it in the current turn.

### `gh_api`

- Read-only GitHub REST access for repositories that are not checked out locally, taking a single `path` (e.g. `repos/OWNER/REPO/pulls/123`) plus an optional `media_type`.
- GET only. There is no method or body parameter and `--method` is never passed, so `gh api` stays on its GET default and mutations are structurally unreachable; creating issues, comments, or reviews remains worker work.
- Implemented by shelling out to `gh api` rather than calling the REST API directly, because `gh` stores its token in the OS keyring and `~/.config/gh/hosts.yml` frequently contains no `oauth_token`. Delegating inherits `gh`'s own precedence (`GH_TOKEN`/`GITHUB_TOKEN` over the keyring) and its enterprise host handling, and means private repositories visible to `gh` work here too.
- `path` is rejected if it starts with `-` or contains `://`, so it cannot become a flag or redirect the request to another host.
- JSON file responses are base64-decoded in place: `content` is replaced with readable text and `encoding` set to `utf-8`, keeping surrounding metadata without carrying the blob twice. Content that is not valid UTF-8 stays encoded. `media_type` of `diff`/`patch`/`raw`/`text`/`html`/`full` sets the corresponding `Accept` header, so a pull request path can return an actual diff.
- Output is truncated at 30000 characters.
- Needs no shell, so the orchestrator can honor the project rule that GitHub URLs are inspected with `gh`.

### `git`

- Read-only repository inspection with a fixed `action` enum: `status`, `log`, `diff`, `show`.
- Read-only by construction, not by convention: the subcommand comes from that fixed set, every flag is built in code, and no caller-supplied string is ever used as a flag. `revision` and `path` are rejected if they start with `-`, pathspecs are passed after `--`, and the command is spawned directly with no shell, so no input turns it into a mutating or shell-executing command. `--no-ext-diff` also blocks repository config from invoking an external diff program.
- `repo` defaults to the working root and resolves like other file tools. `stat` returns a per-file summary instead of a patch, `staged` selects the index for `diff`, and `log` takes a `limit` clamped to 1..=200.
- Output is truncated at 20000 characters with a `truncated` flag.
- Needs no shell, so the orchestrator can inspect repository state directly.

### `ask_media`

- One-shot understanding of a local image, PDF, audio, or video file: takes a `file` path and a `prompt`, sends the file inline to a Gemini model, and returns the model's text answer.
- Read-only: it reads one file and returns text. It never modifies the file and needs no shell, so agents without `bash` (including the orchestrator profile) can use it.
- Stateless — each call re-sends the file; a follow-up question is another call.
- Optional `model` override must be a Gemini model; anything else fails clearly. Inline size cap is 15 MiB.
- Paths resolve like other file tools (relative to the working root, with `$VAR`/`~` expansion).
- Shares its implementation with the `zdx ask-media` CLI, which is unchanged.
- Models with `input_images = false` have image blocks replaced by a note pointing at this tool (see §Image fallback).

### `list_models`

- Read-only listing of the model ids this config can run, mirroring `zdx models list`: enabled providers (account-qualified ids) plus models declared under `[providers.custom.<name>]`.
- Each entry carries the exact `provider:model` id, its display name, and whether the provider is subscription-backed. An optional `provider` argument filters by provider id or its account-qualified form, case-insensitively.
- Also returns the configured `[[model_modes]]` in config order (name, description, primary, alternatives), so a shell-less agent can weigh a tier's picks — including deliberately choosing an alternative when a primary is unavailable — without reading config.
- Read-only and shell-free, so agents without `bash` — the orchestrator profile — verify an id instead of guessing one. Output is capped at 200 model entries with a `truncated` flag and a note to narrow by provider.

### MCP-backed tools

- MCP support is an internal engine backed by a project-local `.mcp.json` file.
- MCP tools are **not** added to the model-visible tool list by default in `zdx exec`, the TUI, or the Telegram bot.
- The supported user/skill-facing MCP surface in this slice is `zdx mcp ...`.
- Supported MCP config source for this slice: `<project-root>/.mcp.json`
- Supported config shape:

```json
{
  "mcpServers": {
    "xcode": {
      "type": "stdio",
      "command": "xcrun",
      "args": ["mcpbridge"]
    },
    "figma": {
      "url": "https://mcp.figma.com/mcp",
      "oauth": {
        "clientId": "your-oauth-client-id",
        "redirectUri": "http://127.0.0.1:8787/callback",
        "tokenEndpointAuthMethod": "none",
        "scopes": ["mcp:connect"]
      }
    }
  }
}
```

- Supported transports: `stdio` and streamable `http`.
- Optional fields:
  - `stdio`: `env`
  - `http`: `type` (defaults to `http` when `url` is present), `headers`, `oauth`
- `zdx mcp servers|auth|logout|tools|schema|call` is the preferred way for skills and operators to inspect/call MCP servers.
- Discovered MCP tools still get stable, collision-safe internal names in the form `mcp__<server>__<tool>`, which are surfaced in helper metadata/output.
- Discovery lifecycle for the shipped helper CLI: each `zdx mcp ...` invocation loads the current root's MCP workspace and prints diagnostics/status in JSON.
- MCP tool execution through the helper CLI uses the same ZDX `ToolOutput` envelope as built-in tools.
- MCP server failures are isolated per server. A broken or unreachable MCP server does not disable built-in tools or healthy MCP servers.
- OAuth-protected HTTP MCP servers should surface `auth_required` status and auth metadata in `zdx mcp servers` instead of degrading to a generic load failure when auth requirements can be discovered.
- OAuth-protected HTTP MCP servers may authenticate with cached bearer tokens from `<base>/mcp_oauth.json`.
- `zdx mcp auth <SERVER>` performs OAuth discovery and login for HTTP MCP servers. It may use configured OAuth client settings from `.mcp.json` or dynamic client registration when the authorization server allows it.
- `zdx mcp logout <SERVER>` removes cached OAuth credentials for that HTTP MCP server.
- Timeout behavior:
  - connect: 10s
  - discovery (`tools/list`): 15s
  - tool call: 30s

### Envelope

Success:
```json
{ "ok": true, "data": { ... } }
```

Error:
```json
{ "ok": false, "error": { "code": "...", "message": "..." } }
```

### Semantics

- Tool results are deterministic and correspond to the correct `tool_use_id`.
- Relative paths resolve against `--root` (default `.`).
- `--root` is a working directory context, not a security boundary (YOLO).
- `Glob` accepts one pattern or an OR-array, can match full paths or entry names case-insensitively, returns files and directories by default (narrowed by `entry_type`), can bound the walk with `max_depth` so a single level is listable on its own, and can explicitly include ignored entries. `Grep` can likewise include ignored files. Both keep their shared traversal deadline and report `truncated: true` with a warning when absence was not proven; continuation is by narrowing or splitting `path`, not an unstable cursor over a partial parallel walk.
- `Grep` reports matching lines with optional context, unique captured values, or only the paths of files containing a match; those reporting modes are mutually exclusive and all paginate with `offset`/`max_count`.
- Built-in `Todo_Write` tracks a flat todo list for multi-step work. Each call is a whole-list snapshot: `todos` replaces the previous list, `[]` clears it, and every item carries `content` plus an explicit `status` (`pending`, `in_progress`, `completed`, `abandoned`). The tool is a pure function of its input — no ids, no per-item mutations, and no server-held state to reconcile. Statuses are validated and stored exactly as sent: empty content is rejected, and the number of `in_progress` items is never adjusted, so parallel work can hold several active items and a list may have none.

---

## 10) Providers

Providers are the bridge between the agent and LLM APIs. New providers can be added without updating this spec as long as they follow these contracts.

### Auth contracts

- **API-key providers:** keys come from environment variables (`<PROVIDER>_API_KEY`), never stored in config.
- **OAuth providers:** tokens are cached in `<base>/oauth.json` (0600 perms). Login via `zdx login --<provider-slug>`.
- **Multiple OAuth accounts:** `zdx login|logout --<provider-slug> --account <name>` manages a named account stored under the `<provider>@<name>` key in `oauth.json`. Omitting `--account` targets the default account, stored under the bare `<provider>` key. Account names cannot contain `@`, `:` or `/`.

### Model routing

- **Explicit prefix** (canonical): `<provider>:<model>` (e.g., `anthropic:claude-sonnet-4-5`). Always wins.
- **Account-qualified prefix:** `<provider>@<account>:<model>` (e.g., `claude-cli@work:claude-fable-5`) routes the request through that named OAuth account.
- **Account model expansion:** every named account in `oauth.json` automatically mirrors its provider's full model list as account-qualified entries, so a second subscription needs no `models.toml` edits and survives `zdx models update`. Explicit account rows in `models.toml` take precedence over the mirrored entry.
- **Heuristic fallback:** when no prefix is given, the model name is matched against provider-specific patterns (e.g., `claude-*` → Anthropic). Heuristics are implementation details and may change.
- **Spec modifiers:** a model spec may carry trailing `@` modifiers — a thinking level (`@high`) and/or `@fast`. They are order-independent and render canonically as `model[@thinking][@fast]`. Modifiers are stripped before the model id reaches a provider, and an unknown segment ends modifier parsing (so `provider@account:model` is never consumed).
- **Thinking source of truth:** config, model modes, subagent definitions/overrides, automations, CLI overrides, and agent-tool inputs carry thinking only in the model spec. Removed standalone config keys are rejected rather than ignored. A suffixless persisted config model means `off`; a suffixless runtime override inherits the currently effective level and is canonicalized with that level before it crosses a process boundary or is newly persisted.
- **Historical threads:** the old transcript `thinking_override` field remains readable. When present it is applied after that thread's model override to preserve historical behavior. Any new model-override write stores one unified model spec and atomically clears the legacy field.
- **TUI selection is not a config write:** `/model`, the standalone `/thinking` (Ctrl+T), and Tab/Shift+Tab model-mode cycling change the active tab's effective selection and, when the thread already exists on disk, that thread's model override. They never touch config, so the selection follows the thread and `/new` starts from the configured default again. `/thinking` keeps the current model and changes only the level; it is a no-op with a notice on models that cannot reason. `/model-save` is the only TUI path that writes the workspace config; it commits the current spec, reports the actual write result, and adopts the saved value as the tab's default.
- **Selecting a model for the thread a command will open (TUI):** while the `/handoff` composer is open, `/model` (Ctrl+L), `/thinking` (Ctrl+T), and model-mode cycling apply to the thread the handoff will create, not the current one. The pick is held in the composer (shown in its title, and the level keys operate on it), the source tab and thread keep their model and override, and cancelling the handoff at any stage drops it. On submit the pick is pinned on the new thread and used for its system prompt; without one, the new thread inherits the source thread's model as before. `/btw` needs nothing extra: it opens its own tab up front, inheriting the source thread's model, and a pick there is already local to it — pinned on the side thread when the first message creates it.
- **`@fast`:** selects the priority service tier (`service_tier: "priority"`, premium per-token rate) and is offered only for providers that accept it (OpenAI, OpenAI Codex). A spec without `@fast` sends no service tier. Acceptance is not a grant — the `ChatGPT` Codex backend takes the field and may still serve `service_tier: "default"` — so whenever the served tier differs from the requested one, the downgrade is logged. `@fast` is selected from the model picker (or typed into any model field: config `model`, model modes, thread overrides, subagents, automations) and stays visible wherever the model name is shown.

### Model modes

- **Definition:** `[[model_modes]]` entries carry `name`, `description`, `primary`, and `alternatives`. `primary` is a model spec whose `@<level>` suffix is the mode's thinking level; a `thinking` key inside a mode is rejected. `alternatives` are equivalent picks at the same tier, surfaced for deliberate choice and never resolved automatically.
- **References:** any subagent model field — `[subagents.overrides.<name>].model`, subagent frontmatter `model:`, and the `invoke_subagent` `model` argument — may hold `mode:<name>` instead of a spec. Lookup is trim + case-insensitive. A reference naming an unconfigured mode is logged and falls through to the next layer instead of failing the delegation; the resolved model is still validated against the available subagent models.
- **Surfaces:** modes are cycled with Tab/Shift+Tab in the TUI (primaries only), listed by the Telegram launcher, editable in the monitor Config tab (primary only; `description`/`alternatives` stay TOML-only), and rendered as a `# Model Modes` catalog in the reserved orchestrator profile only, so it can pick a tier per worker. Ordinary runs and worker subagents carry no such section; they resolve `mode:<name>` through their subagent override instead.
- **Removed:** `[[favorites]]` no longer exists. A config that still defines it fails to load with an error pointing at `[[model_modes]]`.

### Provider-level config

- Each provider may expose `base_url` and `tools` overrides under `[providers.<id>]` in config.
- Provider implementations live in `zdx-providers`; the models registry (`models.toml`) tracks available models per provider.
- **Custom providers:** `[providers.custom.<name>]` defines a user endpoint (base URL, API key, model allow-list) reached as `<name>:<model>`. The wire protocol is `chat-completions` (default; OpenAI-compatible, `<base_url>/chat/completions`) or `anthropic` (Anthropic Messages via the same client as the first-party provider, `<base_url>/v1/messages`, `x-api-key` auth); the registry spellings `openai-completions`/`anthropic-messages` are accepted aliases. Resolution: an explicit `api` on a matching `[[override]]` in `model_overrides.toml` wins, else the provider's `api`; a model entry's implicit metadata default never counts as a selection, and an unsupported override value fails the turn with a clear error. Both protocols accept the same `base_url` convention (`https://llm.example.com/v1`): in `anthropic` mode one trailing `/v1` is dropped before the client appends `/v1/messages`, so switching protocols never produces `/v1/v1/messages`. Thinking levels: in `chat-completions` mode the level is forwarded as a top-level `reasoning_effort` string carrying the level name (`low`/`medium`/`high`/`xhigh`/`max`), and `off` sends `none` because proxied reasoning backends think by default when the field is omitted. Custom endpoints are proxies that pass the value to their backend, so `@max` reaches backends that distinguish it (e.g. a vLLM-hosted DeepSeek V4). In `anthropic` mode non-off levels map exactly as for the first-party Anthropic provider (adaptive thinking + `output_config.effort` for non-legacy model ids), and `off` sends an explicit `thinking: {"type": "disabled"}`; first-party Anthropic clients keep omitting the field.

### Anthropic adaptive thinking

- Adaptive thinking (`thinking.type: "adaptive"`) is used on Claude Opus 4.7, Opus 4.6, and Sonnet 4.6.
- We always send `thinking.display: "summarized"` so visible thinking text is preserved. This is required on Opus 4.7 (where the API default silently became `"omitted"`) and is a no-op on older Claude 4 models where `"summarized"` is already the default.

### Z.AI GLM-5.3 forced thinking

- The GLM-5.3 family (`glm-5.3`, `glm-5.3-flash`) always reasons: it rejects `thinking: {"type": "disabled"}` with HTTP 400 (code 1210), and its `reasoning_effort` vocabulary is `low`/`high`/`max` only (`medium`/`minimal`/`xhigh` are rejected too). Those models therefore receive no `thinking` field (there is nothing to toggle) and carry the level as a top-level `reasoning_effort` (`medium`/`high`/`xhigh` collapse to `high`, `max` stays `max`, `off` omits it). Verified against `api.z.ai` 2026-09-15.
- Other GLM models keep the documented toggle: `thinking: {"type": "enabled"}` when the model reasons, `{"type": "disabled"}` when it does not.

---

## 11) Environment Variables (Runtime Context)

ZDX exposes runtime context to agent processes via `ZDX_*` environment variables.
These are the canonical source of truth for paths and session context — skills, automations, and bash commands reference these env vars directly.

### Mechanism

`set_runtime_env()` in `zdx-engine/src/core/context.rs` sets all `ZDX_*` env vars once at agent startup (TUI, exec, bot). Child processes (bash tool, subagents) inherit them automatically.

### System prompt `<environment>` block

The `<environment>` block in the system prompt contains current-session metadata (for example current directory and date) plus a short list of high-signal runtime env vars the model may need without running commands (for example `ZDX_MEMORY_ROOT`). It does not enumerate every derived path; the model uses `$ZDX_*` env vars directly in bash commands.

### Subagent inheritance

Child `zdx exec` processes inherit all `ZDX_*` env vars from the parent automatically. No explicit forwarding needed.

---

## 12) Configuration

- Location: `<base>/config.toml`
- Format: TOML

### Layering

- Config is layered: `$ZDX_HOME/config.toml` is the base, then every `.zdx/config.toml` from the user's home directory down to the current working directory is merged over it. The directory closest to cwd wins.
- When cwd is outside the user's home directory, only `<cwd>/.zdx/config.toml` is layered.
- Merge is a deep merge: tables merge recursively; scalars and arrays (including arrays of tables) are replaced wholesale by the higher-precedence layer.
- Any key may be overridden by a workspace layer; there is no restricted subset.
- Missing layers are skipped. If no layer exists, defaults apply.
- Monitor Config display loads and reloads layers for the monitor's supplied root, not the process CWD. Workspace-supplied rows carry a dim source-directory basename (for example `[zdx]` or inherited `[example]`); global/default-only rows are untagged. Equal-value overrides still belong to the overriding layer, and the panel title names the viewed root. These tags are display metadata only: picker values and write destinations are unchanged.
- Writes (`zdx config`, mode/Telegram saves, monitor edits) target `$ZDX_HOME/config.toml`.
- Exception: workspace-scoped model writes commit one unified model spec to `<project root>/.zdx/config.toml`, where the project root is the nearest layered directory (cwd first, never above home) that already contains a `.zdx` directory. `.zdx` is an opt-in marker: a directory with only `.git` is not a project root. Outside any project they fall back to `$ZDX_HOME/config.toml`. The Telegram bot's interactive `/model` selection writes here; in the TUI only the explicit `/model-save` command does.
- Workspace writes are minimal: only the changed key is written, and a missing overlay file is created empty rather than seeded from the default template.

### MCP configuration

- MCP server configuration is not stored in `config.toml` for this slice.
- The authoritative supported MCP source is a project-local `.mcp.json` file using the standard `mcpServers` JSON shape.
- Missing `.mcp.json` is normal and does not affect startup.
- Invalid `.mcp.json` or server-specific MCP failures are warnings/non-fatal conditions rather than startup errors.

### Contracts

- Config is the single source of truth for user preferences (model, tokens, timeouts, prompt customization, memory paths, skill sources, subagent settings).
- Adding a new config key or provider section should not require a spec update — the config struct in code (`zdx-engine`) is authoritative for the full schema.
- `max_tokens` is optional; when unset, providers that support omitted limits use provider defaults. Providers that require a limit use an internal fallback from model metadata, capped at 64k and clamped to the space left in the model's context window (the request's estimated input plus a fixed headroom). A model's declared output limit is a ceiling on one response, not a per-request size: deriving `max_tokens` from it asks for the whole window on every turn and can push the request past the context limit. An explicit config value is only ever reduced by the context clamp, never raised.
- A turn that loops instead of finishing is stopped rather than left to burn its budget. Three shapes are detected: reasoning past 160k characters with no text or tool call yet, reasoning or answer text that degenerates into the same few short lines, and three identical tool calls (same tool, same input, same result) within one turn. Before giving up, the engine retries the turn once at the next thinking level down and says so in the notice; if the retry loops too, or there is no lower level, the turn closes normally with a `notice` and the repeated content is truncated head-and-tail. A detected loop never fails the turn. The answer-text check is deliberately stricter than the reasoning one (a full 4k window of short lines, none repeating fewer than ten times) so tables, log dumps, and generated code pass.
- Provider base URLs and tool overrides live under `[providers.<id>]`.

### Prompt templating

- Template syntax: MiniJinja (`{{ var }}`, `{% if %}`, `{% for %}`).
- `[prompt_template].file` — optional template path (relative paths resolve from `ZDX_HOME`).
- The built-in fallback/default prompt is `prompts/system_prompt_template.md`. On custom template load/render failure, ZDX warns and falls back to that built-in template.
- Providers consume the caller-composed prompt; they do not prepend hidden coding system prompts.

### Prompt layers

- Prompt layers are additive MiniJinja-rendered prompt fragments appended after the base system prompt.
- The same mechanism is used for surface-specific constraints (for example Telegram or exec output guidance) and harness-style behavior layers (for example automation/headless execution instructions).
- Prompt layers modify behavior without creating a separate subagent identity.

### Named subagents

- Named subagents are markdown files with YAML frontmatter plus a standalone prompt body.
- Discovery order/override precedence: built-in → `~/.zdx/subagents/` → project `.zdx/subagents/` (later sources override earlier by name).
- `invoke_subagent` requires `subagent: <name>`: delegation always targets a named subagent, and there is no unnamed/default delegated agent. Omitting the argument is an error.
- `invoke_subagent` accepts an optional per-invocation `model = "provider:model[@thinking][@fast]"` override. Model layers resolve parent/default configuration → named subagent definition → `[subagents.overrides.<name>]` → explicit invocation. A suffixless runtime layer inherits the preceding level; an explicit suffix replaces it. Profile prompt, tools, and context behavior remain unchanged.
- Subagent definition frontmatter and `[subagents.overrides.<name>]` each use only `model` for model and thinking selection; standalone `thinking_level` is rejected.
- Delegation is read-only: every delegable subagent researches, reads, and analyzes without mutating local or remote state, so implementation stays in the calling run. Mutating work is delegated by creating a worker thread (§18), which exists only where a live worker manager runs.
- Delegated child runs should be prompted self-sufficiently: the parent should include the goal, relevant context, constraints/non-goals, expected output, and verification when relevant rather than assuming the child inherits its implicit reasoning state.
- When a named subagent is selected, its body is rendered with the same prompt-template syntax/vars as the main prompt pipeline, then used as the child run's system prompt directly; it does not inherit the default ZDX prompt/context pipeline unless that text is written into the subagent body.
- Named subagents may declare `skills:` (allowed on-demand skills) and `auto_loaded_skills:` (skills whose `SKILL.md` contents are injected directly into the subagent prompt). Auto-loaded skills should be treated as already in context for that run.
- A subagent definition may declare `allowed_subagents`, restricting which subagents it can reach via `invoke_subagent`. The restriction is enforced twice: the tool schema advertises only the listed subagents, and execute-time resolution rejects any unlisted name. `allowed_subagents` may not list the declaring agent itself or the reserved `orchestrator` profile. Omitting the field leaves the caller unrestricted.
- Explicit subagent skill dependencies are resolved from enabled sources even if global `include_skills` / `ignored_skills` filters would otherwise hide them.
- Built-in subagents currently include:
  - `explorer`: a read-only local exploration specialist for open-ended multi-step discovery across the current workspace, broader machine-local filesystem paths, and saved thread history.
  - `oracle`: a read-only deep reasoning advisor for code review, difficult debugging, planning, and architecture decisions. Its output is advisory and should be independently validated by the parent agent.

### Models registry

- Path: `<base>/models.toml` (falls back to `default_models.toml` when missing).
- Persistent user metadata overrides live at `<base>/model_overrides.toml`. `[[override]]` entries use a provider-qualified `id` and may override display name, pricing, context/output limits, reasoning, image input, and API routing metadata. An override may also define metadata for a custom-provider model absent from `models.toml`; it survives `zdx models update` because the generated registry and user overrides are separate files.
- Tracks available models per provider. Entries support `*` wildcards for `zdx models update`.
- `zdx models list` prints models from enabled providers as `provider:model` ids (the exact value accepted by `-m`), with `--all` to include disabled providers, `--provider <id>` to filter by provider, `--plan-only` to keep only providers covered by a subscription plan, and `--json` for machine-readable output (each entry carries a `subscription` flag).

---

## 13) Project Context + Memory (`AGENTS.md`, `CLAUDE.md`, `MEMORY.md`)

ZDX loads project/user context inputs in this order before template rendering:

1. Base/system prompt from config (`system_prompt` / `system_prompt_file`)
2. Hierarchical project context: prefer `AGENTS.md`, fall back to `CLAUDE.md` per directory (global + user + project ancestry)
3. Optional memory index from the configured memory root (default: `$ZDX_HOME/memory/Notes/MEMORY.md`)

### Memory configuration

```toml
[memory]
# root = "~/Notes"  # default: $ZDX_HOME/memory
```

- The configured memory root supports `~` expansion.
- `memory.root` must be an absolute path or use `~/...`; other relative values are rejected.
- Defaults place memory under `$ZDX_HOME/memory/`, with notes in `Notes/`, calendar notes in `Calendar/`, and the index at `Notes/MEMORY.md`.
- The configured memory root is a container directory. Notes should live under `Notes/` or `Calendar/`; tools/skills should not create ad-hoc markdown files directly under the root.
- The `memory` skill provides full guidelines for working with memory notes (NotePlan-compatible conventions).

### Native memory search/index contracts

- `zdx memory index` exports changed top-level thread transcripts to Markdown, refreshes derived thread metadata in `$ZDX_HOME/cache/threads.sqlite`, and builds `$ZDX_HOME/cache/memory.sqlite` over exported thread Markdown, `Notes/**/*.md`, and `Calendar/**/*.md`.
- `threads.sqlite` and `memory.sqlite` are disposable caches. Canonical sources remain thread JSONL, exported thread Markdown, Notes Markdown, and Calendar Markdown.
- Native memory docids use the disjoint grammar `zdxmem:v1:<source>:<hex16>`, where `<source>` is `thread`, `note`, or `calendar`. They are internal index identity and are not returned to callers.
- `Memory_Search` and `zdx memory search` default to native lexical search when `strategy` is omitted; explicit `keyword` is also lexical. Explicit `vector`/`hybrid` require a complete configured embedding profile and fail clearly when embeddings are unavailable or coverage is incomplete. Agent searches never trigger corpus embedding; vector/hybrid queries embed only the query (plus optional intent) text and surface a warning that it was sent to the configured provider.
- Every search hit carries the absolute canonical `path` of its source file, plus `thread_id` for thread hits. Callers read current canonical content — `Read` for notes/calendar paths, `Read_Thread` for thread IDs — rather than an indexed snapshot, which lags its sources between index runs.
- Notes and Calendar indexing excludes `@Archive` and `@Trash` by default and rejects paths that escape the configured memory roots.
- Hosted corpus embedding is opt-in only through `zdx memory index --embed` with a complete `[memory.embeddings]` profile (provider, model, source allowlist, `usd_per_million_tokens` pricing source, hard `max_run_tokens` budget); `--dry-run` preflights pending/cached inputs, token estimate, and cost without provider calls or writes. Runs whose conservative estimate exceeds the token budget refuse to upload. Vectors are stored per `(input_hash, profile_fingerprint)` so unchanged inputs are never re-purchased and interrupted runs resume; estimated cost is always reported and actual tokens/cost when the provider reports usage.

### Contracts

- At each directory scope, ZDX loads `AGENTS.md` if present; otherwise it loads `CLAUDE.md`.
- At each directory scope (home, every ancestor between home and the project root, and the project root itself), ZDX additionally loads `<dir>/.zdx/AGENTS.md` (or `CLAUDE.md` fallback) when present, after the scope's regular `AGENTS.md`. This is intended for personal rules that live alongside other `.zdx/` per-project assets and are typically gitignored. Loading order means the `.zdx/` file wins over the committed file at the same scope ("deeper wins").
- Relative file references mentioned inside an `AGENTS.md`/`CLAUDE.md` block resolve from that context file's directory, not from the session cwd/root, unless the file explicitly says otherwise.
- Memory is optional. Missing `MEMORY.md` does not fail startup and does not inject memory blocks.
- `MEMORY.md` load failures are warnings (non-fatal).
- `MEMORY.md` content is capped at 16 KiB with truncation warning.
- Only `MEMORY.md` index content is injected. Detailed memory lives under the configured memory root (`Notes/` + `Calendar/`) and is accessed on-demand via the `memory` skill.
- Runtime exposes the configured memory root to tools/skills via `ZDX_MEMORY_ROOT`.
- Built-in template emits a `## Memory` section (with `<memory_contract>` and `<memory_index>` blocks) only when memory index content is present.
- Proactive memory-save suggestion instructions are surface-gated: enabled for TUI and Telegram sessions, disabled for exec mode, automations, and subagent runs.
- Explicit `remember X` still means immediate save regardless of proactive suggestion mode.
- When proactive suggestions are enabled, memory instructions are note-first: save full detail in memory notes, and only promote durable/reusable items into `MEMORY.md`.
- `MEMORY.md` entries should be concise routing pointers; updates should prefer upsert/merge over append-only duplication.

---

## 14) Skills (SKILL.md)

Skills are folders containing a `SKILL.md` file with YAML frontmatter (`name`, `description`) and Markdown instructions. At startup, only metadata is loaded. The model uses the `read` tool to load full instructions when a task matches a skill.

### Discovery & sources

- **Bundled skills:** ZDX includes built-in bundled skill fallbacks (currently `deepwiki-cli`, `memory`, `imagine`, and `skill-creator`) shipped inside the crate under `crates/zdx-assets/bundled_skills/`. At build time, ZDX embeds every file under that tree into the binary. At runtime, it materializes the bundle on demand under `$ZDX_HOME/bundled-skills/` and rewrites that directory only when the materialized bundle stamp is missing or differs from the embedded bundled-skill manifest hash; it does not verify individual bundled files on each startup.
- **Recursive sources:** `~/.zdx/skills/`, project `.zdx/skills/`, `~/.codex/skills/`, `~/.agents/skills/`, and project `.agents/skills/` are scanned recursively for `SKILL.md`. Project `.zdx/skills/` is also discovered in every ancestor between the current working directory (inclusive) and the user's home directory (exclusive), mirroring `AGENTS.md` discovery; ancestors closer to the cwd take precedence on name collisions.
- **Claude sources (one-level):** `~/.claude/skills/` and project `.claude/skills/` only scan `dir/*/SKILL.md`.
- **Priority:** zdx-user → zdx-project → codex-user → claude-user → claude-project → agents-user → agents-project → built-in (first wins on name collision, so user/project skills override bundled fallbacks).

### Validation & warnings

- **Name:** required, ≤64 chars, lowercase alphanumeric + hyphens, no leading/trailing/consecutive hyphens.
- **Description:** required, ≤1024 chars.
- **Directory match:** name should match parent directory; mismatch emits a warning but still loads.
- Invalid skills are skipped with warnings; startup never fails.

### Prompt integration

- Skill metadata is appended to the system prompt as an `<available_skills>` XML block.
- Each skill includes `name`, `description`, and `path`.
- Bundled skills use `${ZDX_HOME}/bundled-skills/...` prompt paths, so the same path works in `read`, `bash`, subagents, and relative bundled references.

### Filtering

- `include_skills`: optional glob allowlist (empty = all).
- `ignored_skills`: optional glob blocklist (wins over include).

## Related Documentation

- `docs/ARCHITECTURE.md` — TUI implementation patterns, code organization
- `docs/adr/` — Architecture Decision Records (the "why" behind decisions)
- `AGENTS.md` — Development guide and conventions

---

## 15) Telegram bot media response (`zdx-bot`)

When using the Telegram bot runtime, assistant turns may include media file directives.

Contracts:

- Text replies continue to work unchanged.
- If assistant output contains explicit media directives, the bot may send media in addition to text.
  - Supported entry formats:
    - `<media>/absolute/path/to/file</media>`
    - `<medias><media>/absolute/path/to/file1</media><media>/absolute/path/to/file2</media></medias>`
- Any `<media>` directives are stripped from the user-visible reply text.
- Routing by file type:
  - Image-like extensions (`.png`, `.jpg`, `.jpeg`, `.webp`) are sent via Telegram `sendPhoto`.
  - Other files (including `.pdf`) are sent via Telegram `sendDocument`.
- When multiple valid media paths are present, the bot attempts to send each one in order.
- Bot only uses local absolute file paths for this flow (no URL fetch in this slice).
- Preflight upload size checks:
  - photos > 10 MB are rejected before upload
  - documents > 50 MB are rejected before upload

---

## 16) Telegram forum topic flow (`zdx-bot`)

When the Telegram bot is used in a forum-enabled supergroup:

- A normal user message sent in `General` creates a new topic and routes that message into the topic before the agent replies. Right after the pinned header, the original message is **forwarded** into the new topic (text, photo + caption, voice, and every item of an album alike, keeping the "Forwarded from" attribution), so the topic reads from the question onward instead of starting at the answer. The forward is silent and best-effort; the turn runs even if it fails.
- Slash commands that act on setup/status do not auto-create topics from `General`; they run in place instead (for example `/model`, `/status`, `/worktree`).
- `/new` sent in `General` creates an empty topic only:
  - no prompt is routed into the new topic
  - no agent turn starts
  - no bot message is posted into the topic as part of creation
- Topic title generation rules:
  - if the topic was created from a normal message in `General`, the bot may auto-generate the topic title from that first routed message
  - if the topic was created by `/new` in `General`, the bot waits and auto-generates the topic title from the first later in-topic message that contains usable text (plain text or audio transcript)
- `/handoff` (inside a topic) starts a staged, memory-only handoff flow:
  - the next message (text or voice transcript) is consumed as the handoff input — it never runs an agent turn and is never persisted to the topic's thread
  - that input completes the command with no confirmation step: the bot generates the handoff context and immediately creates a new topic whose thread records `handoff_from` (source thread), then runs the first agent turn there with the generated handoff prompt
  - the generated context is posted into the new topic before that turn starts, because the handoff prompt itself is dispatched synthetically and never appears as a chat message
  - the new thread continues the source thread's **mode**: a persistent profile (the reserved `orchestrator` home base) is copied onto it, so an orchestrated thread hands off to another orchestrator and an ordinary thread to an ordinary one. When the profile cannot be recorded the handoff fails instead of falling back to the default coding agent
  - nothing moves off the source thread: it keeps its workers and stays usable, so both threads can continue side by side. The successor is a fresh conversation in the same mode, not a takeover — it can adopt a worker on purpose with `Send_Thread_Message`, which re-attaches that thread to whoever messaged it
  - if generation or topic creation fails, the staging session stays open: sending another message retries, Discard / `/cancel` aborts
  - Discard (or `/cancel`) deletes the staging messages — the bot's own always, the user's best-effort (needs `can_delete_messages`) — and leaves the source thread untouched
  - `/handoff` outside a forum topic (DM or `General`) does not start staging; the bot explains it needs a topic
  - stale staging sessions expire; a message after expiry runs as a normal agent turn
- The model of a topic opened by `/handoff` or `/btw`:
  - by default it continues the **source thread's** effective model and thinking level (its own `/model` override, not the chat default)
  - the staged command's card carries a `🎛 Model` button next to Discard (only for the commands that open a topic — not `/prompt_builder` or `/goal`). It opens the provider → model → thinking picker in place, and returns to the card showing the pick; `← Back` / `✖ Cancel` return without changing it
  - a pick is resolved against the source thread's effective config (so a suffixless model keeps the current thinking level) and is written only to the new thread when it is created — the current thread's model is never touched, and the pick lasts for that one command
  - the picker is in-memory like the rest of staging: after a bot restart the buttons report the expired session instead of acting
- `/commands` (inside a topic or DM) posts a context-dependent command picker:
  - lists only the custom `.md` commands the TUI would show for the chat's bound project (bundled + `$ZDX_HOME/commands` + project `.zdx/commands`); agent built-ins live in the native `/` menu instead
  - custom `.md` commands are picker-only on the bot: they are not typed commands and are not registered in the native `/` menu
  - tapping a custom command dispatches its prompt content as a normal agent turn in the current topic
  - the picker is one-shot: a tap consumes it; Dismiss deletes it
  - the picker itself bypasses the queue and posts immediately even while a turn is running; a tapped command runs as a normal queued turn
- `/tldr` (typed, native menu) posts a recap of the current thread (read-only, `tldr_model`); like `/status` it bypasses the queue and does not auto-create topics from `General`
- `/threads` (with `/thread` accepted) posts a named Mini App link for the current thread when `[telegram.server]` is enabled and `mini_app_url` is configured.
- The embedded Mini App server is opt-in. It serves one unified shell at canonical route `/app`; `/threads` and `/monitor` remain compatibility aliases. The shell navigates between Monitor, Threads, and Git without a page reload. Threads preserves event order and shows messages plus collapsed reasoning, tool calls/results, usage, notices, and interruptions; private persistence metadata and provider replay tokens are never exposed. Monitor covers managed services, active agents, background processes, live subscription quota windows, 30-day usage, an explicit safe config summary, and automations. Git is a read-only inspector for the current branch and ahead/behind state, worktrees, staged/unstaged/untracked files, recent commits, and lazy per-file diffs capped at 256 KiB. Git resolves the selected thread's persisted project root when it belongs to a repository, then falls back to the bot root. Every `/api/*` route requires fresh, bot-token-signed `Telegram.WebApp.initData` from an allowlisted Telegram user. Local monitor snapshots run off the async request worker and are cached for 30 seconds. Subscription quota endpoints use stored credentials read-only, never refresh tokens, run concurrently, and are cached for five minutes. The Mini App exposes no service control, process control, Git write, or destructive Git actions.
- `/prompt_builder` (typed, native menu; `/prompt-builder` also accepted) starts the same staged flow as `/handoff` with the intent as input:
  - works inside topics and DMs (not `General`); the generated prompt is previewed with Accept / Discard buttons and regenerates on a new message — it is the one staged command that gates on confirmation, because accepting it writes to the current thread
  - Accept runs the generated prompt as the user's real message in the current topic (a normal agent turn); the preview message is kept (edited) as the turn's reply anchor
  - Discard / `/cancel` delete the staging messages and leave the thread untouched

---

## 17) Telegram turn status and final reply (`zdx-bot`)

- While a turn runs, the bot keeps one live status message carrying the current activity and the Cancel button, edited in place with a debounce.
- The status message also offers an `Open Thread` button that deep-links the Mini App to that turn's effective thread, on the same terms as the topic header: only when `[telegram.server]` is enabled with `mini_app_url` set, otherwise Cancel stands alone. Audio turns show it once transcription hands over to the agent, since the thread is not known while transcribing.
- When the turn completes, the status message is deleted and the assistant reply is sent as a new message. The reply's Telegram timestamp is the turn's end time, it carries no `edited` marker, and it raises a normal message notification (Telegram does not notify on edits).
- The new reply keeps the same reply target the status message used; an invalid reply target falls back to sending without one.
- A turn that produces no text, media, or follow-ups deletes the status message and posts nothing.
- Cancelled and failed turns still resolve in place: the status message is edited to `Cancelled ✓` or to the error text (failures then post the retry buttons as a separate message).

---

## 18) Orchestrator profile and worker threads (`zdx-bot`)

The reserved built-in `orchestrator` profile turns a Telegram topic into a persistent home base that coordinates work across projects by delegating to worker threads.

### Persistent top-level profiles

- A thread whose meta has `origin_kind = None` and `subagent_name` set is a **persistent top-level profile** thread. It stays visible in default listings (unlike child runs, which set `origin_kind`).
- Existing child-run lineage semantics (`origin_kind` set) are unchanged.
- Only the reserved built-in `orchestrator` profile exists. An unknown persistent profile name fails the turn instead of silently falling back to the default coding toolset.
- The `orchestrator` name is reserved: user/project subagent files may not define or override it, and it is loaded directly from the embedded built-in definition.

### Orchestrator topics

- A topic created from an ordinary `General` message in a forum chat is initialized as an orchestrator topic before its first turn **only when the chat's Telegram profile opts in** with `telegram.profiles.<name>.orchestrator = true` (default `false`; unprofiled chats never opt in). Chats without the flag keep classic behavior (General → normal coding topic). `/new`, the launcher, and `/btw` topics keep the default profile regardless — a side question is an ordinary read-only thread that reads the source with `read_thread`, never a second home base. `/handoff` is the exception: it continues the source thread's mode, so handing off an orchestrator produces another orchestrator (see §16).
- With BotFather **Threaded Mode** enabled, each brand-new thread in the bot's private chat is likewise initialized as an orchestrator thread on its first sighting: the message must carry a client-created `message_thread_id`, not be a known slash command, and map to a thread file that does not exist yet. It also gets the same pinned orchestrator card a General-created topic gets, posted right after the user's first message (pinning is best-effort in private chats). Plain unthreaded DMs and pre-existing DM threads keep their current profile.
- Orchestrator turns use the built-in profile's rendered prompt — which composes a ZDX operating manual, the full discovered skills catalog, project context, memory, and a bounded recent-activity snapshot (most recently active projects and top-level threads) — plus the Telegram instruction layer, and exactly its declared tool list: `read`, `grep`, `glob`, `ask_media`, `telegram`, `git`, `gh_api`, `list_models`, `thread_search`, `read_thread`, `todo_write`, `memory_search`, `web_search`, `fetch_webpage`, plus the six thread controls below. `bash`, `edit`, `write`, `apply_patch`, `invoke_subagent`, and the background tools are excluded.
- In the Telegram bot, orchestrator turns also receive a **Telegram Workspaces** block: every bound **workspace** profile (name, chat id, root) and, under each, the project-level skills a worker in that root will discover (`.zdx/skills`, `.claude/skills`, `.agents/skills` of the root and its ancestors; bundled and user skills are excluded since the orchestrator already has them). A skill reachable from nested workspaces is listed once, under the deepest root containing it. Descriptions are cut to their first line (≤140 chars). This is how the orchestrator knows what a workspace can do without running there. Orchestrator profiles are **not** listed and are skipped during skill attribution, so a management group sharing a workspace's root can neither be offered as a worker destination nor silently claim that root's skills. When the current chat is a configured orchestrator group, the block ends by naming its default worker root.
- The orchestrator's read-only posture is structural: its declared tool list contains nothing that can mutate local or remote state, so mutations and anything needing a shell must go to a worker.
- The orchestrator has no subagent access. `invoke_subagent` is excluded so every unit of delegated work is a visible, mirrored worker thread the user can follow and interrupt, rather than a hidden child run blocking the middle of a reply.
- The reserved `orchestrator` profile is not invokable as a subagent by anyone. Beyond being hidden from the advertised catalog, subagent name resolution rejects it, so `invoke_subagent`, `zdx exec --subagent`, and automation `subagent:` frontmatter all fail rather than starting a second home base. The bot resolves the profile from the embedded definition instead.
- The orchestrator's model and thinking level resolve through the shared model-spec resolver as: the chat's configured model → `[subagents.overrides.orchestrator]` → the topic's own `/model` override. An override without an `@level` suffix keeps the preceding level. The status card reflects the same resolved config and labels model/thinking provenance independently as `workspace/global default`, `orchestrator override`, or `topic override`. This is the only place the `orchestrator` override key takes effect, since the profile is never reachable through `invoke_subagent`.
- An optional personal overlay at `$ZDX_HOME/orchestrator.md` is appended to orchestrator prompts only (never to workers or other agents). A missing or empty file is a no-op; read failures are logged and skipped.
- Orchestrator topics get the same async LLM title on their first message as other topics (Threaded Mode DM threads are otherwise left as "New Thread" or a truncated first line), and do not post retry buttons on failed turns.
- The orchestrator topic's pinned header (and `/status`) uses an orchestrator card: model/thinking, thread, profile, context/usage/pricing, plus a live worker summary (running/queued/settled counts and the first few workers by title and status; each worker title links to its mirror topic, or to the worker thread in the Mini App when the mirror has no topic link). Root and branch lines are omitted as fixed noise for the home base. The bot re-renders a card it posted in the current process whenever one of that orchestrator's workers is created, prompted, starts its first tool call, or finishes, so the pinned card is a live status; `↻ Refresh` remains for cards posted before a restart.

### Worker threads

- A worker is an ordinary visible thread bound to one existing project root, created by the orchestrator. Its meta records the owning orchestrator as `parent_thread_id` (lineage only; no `origin_kind`, so it stays a visible top-level thread and `zdx threads show <orchestrator>` lists it as a child). Worker turns run through a child `zdx --thread <id> exec` process in that root, so the worker resumes its own persisted JSONL history on every prompt and can also be resumed manually.
- Prompts for one worker run strictly serially through an in-memory FIFO; different workers run concurrently with no global cap. Each queued prompt gets a process-unique integer `prompt_id` when it is enqueued (from any path: orchestrator tools or mirror-topic messages); ids are never reused within a process and are lost on restart with the queue.
- A single waiting prompt can be removed by id without affecting the running turn or the other queued prompts. Removing the last waiting prompt makes the worker idle once its current turn ends. The running prompt has already left the queue and cannot be removed; cancellation is the only way to stop it.
- Cancellation stops the current worker turn by terminating the child's entire process group (TERM, short grace, then KILL), reaps it before any queued successor could start, and clears the queue. The thread survives and a later message resumes it.

### Thread-control tools

Available only where a live worker manager exists (the Telegram bot); elsewhere the tools fail with `orchestrator_unavailable`. None of them blocks on worker progress: the orchestrator delegates, ends its turn, and is resumed by the completion callback below. `get_thread_status` is on-demand inspection, not a polling primitive.

- `create_thread(root?, prompt, title?, model?)` — `model` is a unified `provider:model[@thinking][@fast]` override. `root` is optional: an explicit value always wins and keeps the handling it has always had, and omitting it resolves the owning orchestrator group's configured default worker root. The resolved root is what gets persisted on the worker and drives mirror routing, and the result reports it as `root` plus a `root_source` of `explicit` or `orchestrator_default`, so a default is never invisible. Omitting `root` from a thread with no configured default is an `invalid_input` error rather than a guess. The tool validates the root exists, creates a visible worker thread, queues the first prompt, and returns the thread id plus the mirror topic's `mirror_url` when the surface opened one. It waits a bounded moment (≤5s) for the asynchronously created mirror to resolve — the worker is already queued and never waits on it; a surface that opens no mirror resolves the wait immediately with `null`.
- `send_thread_message(thread_id, message)` — queues another prompt; re-attaches an unmanaged existing thread from its persisted root. The returned snapshot's `queue` includes the new prompt (last) with its `prompt_id`.
- `get_thread_status(thread_id?)` — status (`queued`/`running`/`completed`/`failed`/`cancelled`), queue depth, `queue` (waiting prompts in run order as `{prompt_id, prompt}`, the prompt text capped at 300 characters; the running prompt is not listed), bounded latest final text, and `mirror_url`; without an id, lists all workers owned by the calling orchestrator. A managed worker also reports `current_tool` (the tool in flight, from live activity), `seconds_since_last_activity`, and `turn_elapsed_seconds`, so a worker doing slow work can be told from one that is hung. For a thread this process does not manage but whose file exists, the call succeeds with `managed: false` and only `seconds_since_last_write` (the thread file's mtime): live tool activity exists solely in the owning process, the JSONL has no `tool_started` event, and a running tool's `tool_use` is not flushed until its turn checkpoints, so `current_tool` is null rather than guessed.
- Managed status also includes `current_tool_input`: the shared primary-command/target extractor's preview, whitespace collapsed and capped at 200 Unicode characters including any truncation ellipsis. Name and preview are correlated by tool-use id and describe the most recently started unfinished call; finishing it falls back to any earlier unfinished call. The preview is null before input arrives, when no primary argument exists, or when idle, and is omitted for unmanaged threads. Input activity never attaches to a different id. Activity age is a hint, not proof of a hang.
- `get_thread_status` includes nullable `context` for managed and unmanaged threads: `input_tokens` is the latest recorded input-bearing request's noncached input + cache-read + cache-write tokens, never cumulative thread usage and never output tokens. `model`, `provider`, and `recorded_at` come from that record, not current thread overrides. A positive registry `context_limit` matched by provider and model yields `percent_used`; unknown attribution or limit leaves those two fields null while retaining the token count. No usable usage within the bounded final 256 KiB (or unreadable history) yields `context: null`, not zero. This is a last-request estimate (`basis: last_recorded_request_input`), not an exact forecast of resumed context; trailing output-only usage does not replace it.
- `update_thread(thread_id, title)` — title only; refused while that managed worker is mid-turn (the atomic rewrite must not race the worker's own appends).
- `remove_thread_prompt(thread_id, prompt_id)` — removes one waiting prompt; returns the removed `{prompt_id, prompt}` and the worker snapshot. Fails with `remove_thread_prompt_failed` when the worker is unmanaged or the id is not waiting (already running, finished, or never queued). Never touches the running turn.
- `cancel_thread(thread_id)` — cancels the current turn and clears the queue; the thread is preserved.

### Worker mirror topics

- When an orchestrator creates a worker, the bot also opens a **mirror topic** for it, named after the worker. Host resolution is: (1) the **workspace** group whose Telegram profile `cwd` contains the worker's root (deepest match); (2) the workspace group covering the owning orchestrator group's configured default worker root; (3) a Threaded Mode DM home, private chats only, where the DM is the sole possible host. A configured orchestrator group is **never** a host at any step — keeping the management space free of worker topics is the point of the split. In either failure case the worker still runs with `mirror_url` `null` and the topic is never placed in the management group instead; the two are reported differently, because they need different fixes. When **no chat is eligible**, the warning posted in the orchestrator topic names the unroutable root and points at adding a workspace profile for it or repointing `worker_root`. When hosts resolved but **every `create_forum_topic` call failed**, the warning says routing succeeded, lists each attempted destination with the Telegram error it returned, and points at the destination's topic setup and the bot's topic rights — without asserting a cause the API error did not establish. The topic's thread is a thin pointer: it aliases the worker thread (`alias_to`) and is marked `worker_topic` in its meta.
- A supergroup mirror has a tappable link (`https://t.me/c/<internal_chat_id>/<topic_id>`), registered on the worker manager as the worker's `mirror_url` and surfaced through `create_thread`, `get_thread_status`, and the `[worker update]` callback (`Mirror topic: <url>`). DM-hosted mirrors have no link form and report `null`.
- The orchestrator's reply automatically ends with one `🛠 <worker title>` link per worker the turn created or messaged (successful `Create_Thread`/`Send_Thread_Message` results, first-touch order, placed above `↗ Open thread`), so the user reaches the mirror without relying on the model to link it. A worker whose mirror has no topic link (DM-hosted) links to the worker thread in the Mini App instead; the line is skipped only when neither exists.
- While a worker turn runs, its mirror shows one live status line naming the tool currently running (`🔧 Running `bash`...`, `⏳ Waiting for model...`), the same shape as a normal turn's status: no arguments, no history (the thread itself has the detail). It is edited at most every 3 seconds and deleted when the turn's result posts.
- A mirror topic opens with the **same pinned status card as any other topic** (`post_thread_header`), computed for the worker thread itself: the resolved model and thinking from its unified model override, thread id, profile, root, branch, context/usage/pricing, plus the `💬 Open Thread`, `🎛 Open orchestrator` (the owning orchestrator's topic, or its thread in the Mini App for DM homes; derived from the worker's persisted parent, so it survives restarts), and `↻ Refresh` buttons. Refresh resolves the topic's alias, so it always refreshes the worker thread.
- Each finished worker turn posts its final text (rendered from Markdown, bounded, followed by the same `↗ Open thread` Mini App link normal answers carry, so a truncated result is one tap from its full transcript) — or its failure/cancellation — into the mirror topic, in addition to the orchestrator callback. Orchestrator-sent prompts are posted there too as `📤` messages rendered from Markdown (the first prompt right under the pinned card, follow-ups as they are sent), so the topic reads as a full prompt → activity → result conversation; topic-originated prompts are not re-posted since the user's message is already visible.
- Only the live status message carries inline buttons (prompts and results have none; the pinned card has Open Thread + Refresh): `⏹ Cancel worker` (callback `wk:c`; the worker is resolved from the topic's persisted alias, never from callback data) cancels the current turn and clears the queue exactly like `cancel_thread`, answering "nothing to cancel" when the worker is not managed; `💬 Open Thread` opens the worker thread in the Mini App (`{mini_app_url}?startapp={worker_thread_id}`) and is omitted when the Mini App is not configured for the host chat.
- Messages sent in a mirror topic never run an in-process turn (the worker's child process owns the aliased JSONL); **every** mirror-topic message — including slash commands and staged flows — is checked before any local interpretation, queued verbatim into the worker's FIFO, and acknowledged. Retry callbacks are also refused in mirror topics. After a restart, a mirror-topic message re-attaches the worker owning itself, so results keep landing in the topic even though orchestrator callbacks are gone.
- Managed workers are excluded from the General launcher's `🔄 Continue` picker while managed, so a resume topic can never become a second writer on a running worker's thread.
- Mirror-topic creation is best-effort: when no candidate chat accepts the topic it is skipped (the worker still runs and its `mirror_url` resolves to `null`).
- **Restart recovery:** at startup the bridge scans persisted thread metadata once for `worker_topic` + `alias_to` pairs and rebuilds the worker→mirror map (and each worker's `mirror_url`) from it, so mirror feeds and buttons resume automatically for every worker re-attached after a restart — nobody has to message the topic first. Recovery is derived purely from the canonical JSONL meta lines; there is no separate store.

### Completion callbacks and restart semantics

- Worker completion is the only mechanism that resumes an orchestrator; there is no tool that holds a turn open until a worker goes idle, and therefore no suppression or replay of callbacks claimed by a waiter. Every finished prompt dispatches its callback.
- After every finished worker prompt, the bot dispatches one best-effort synthetic `[worker update]` message (worker id, status, mirror link, bounded final text) into the owning orchestrator topic through that topic's normal queue, using the same synthetic-message mechanism as `/goal` continuations. Because the synthetic message itself is invisible in chat, a one-line notice is posted in the orchestrator topic first (`✅ Worker 🛠 <title> finished · reviewing…`, with the title linked to the mirror topic when it has one; `🚫 … was cancelled` / `❌ … failed`), so the user can tell which worker woke the orchestrator and jump to it.
- **Batched updates:** worker updates that pile up while a turn is running are handled together. When the queue starts a worker update, it also takes every worker update already waiting behind it and runs them as one turn carrying all of their text, in arrival order. No update is dropped or replaced by the last one, the running turn is never interrupted, and nothing waits for an update that has not arrived yet. Batching stops at the first queued user message or command — those keep their own turn and run next, so ordering is preserved. One notice per completion is still posted, so five finished workers show five notices and one review turn. A queued update cancelled before it runs is skipped, as before.
- All manager state is in-memory by design: worker ownership, queues, routes, and pending callbacks are lost on bot restart. Orchestrator and worker JSONL transcripts survive; workers can be re-attached with `send_thread_message` or a mirror-topic message, and their mirror associations are recovered at startup as described above. There is no database, scheduler, durable job store, or delivery guarantee.

---

## 19) Telegram incoming message content (`zdx-bot`)

Applies to every chat the bot reads (DM, group, forum topic).

- A **rich message** (Bot API 10.1: structured text with headings, lists, tables, quotes, code, media blocks) carries no `text` field — its content is in `rich_message.blocks`. The bot reads those blocks, so a rich message runs a turn exactly like a plain-text one instead of being ignored as empty.
- Blocks are flattened to Markdown-ish plain text: headings to `#`-prefixed lines, lists to `-`/label-prefixed items (task items to `- [x]`/`- [ ]`), preformatted blocks to fenced code with their language, quotations to `>` lines with their credit, tables to `|`-separated rows (with a separator line under a header row), dividers to `---`, details blocks to summary plus content, LaTeX to its expression.
- Inline styling (bold, italic, spoiler, …) is dropped and its text kept. A link keeps its text plus `(url)` when the two differ; a custom emoji becomes its alternative emoji.
- Media inside a rich message is **not** downloaded as an attachment; each media block is named (`[photo]`, `[voice note]`, …) followed by its caption, so nothing is silently lost.
- Unknown block and inline types are not an error: their nested text is kept, and an update is never rejected for containing a type this version does not model.
- Everything downstream of ingest is unchanged: a rich message is subject to the same allowlist, queueing, staging, and topic-creation rules, and a topic auto-created from `General` takes its provisional name from the flattened text.
