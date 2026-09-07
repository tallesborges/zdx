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
- `--subagent <NAME>` runs the prompt under a named subagent (`explorer`, `oracle`, or any discovered subagent): the subagent's rendered prompt becomes the run's system prompt, and its `model`, `thinking_level`, and `tools` apply as defaults. Explicit `-m`/`-t`/`--tools`/`--no-tools` still win. The reserved `task` name resolves to default exec behavior. Conflicts with `--no-system-prompt`; unknown names fail the run.

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
- Telegram bot chat profiles live under `telegram.profiles.<name>` in `config.toml` with `chat_id` and `cwd`; matching chats run agent turns from the profile cwd, and unprofiled allowed chats keep using the bot root fallback.
- Each Telegram profile gets its own layered config anchored at the profile `cwd`, so a workspace `.zdx/config.toml` applies to chats bound to that profile. Profile configs are built once at startup; unprofiled chats use the bot-level config. Runtime `/model` and `/thinking` changes in a General topic are workspace-scoped: they write the chat root's overlay and update only that chat's config.
- Every bot-created Telegram forum topic starts with a status-style thread header. The bot pins it silently when it has `can_pin_messages`, keeps the topic usable if pinning fails, and provides a refresh action plus an Open Thread button when the Threads Mini App is configured. Resumed topics display and open their effective source thread.

### Format

- First line is `meta` with `schema_version`, optional `title`, and optional lineage fields (`origin_kind`, `parent_thread_id`, `subagent_name`) for threads spawned by another agent run.
- Timestamps are RFC3339 UTC.
- Event types: `meta`, `message`, `tool_use`, `tool_result`, `interrupted`, `reasoning`, `usage`, `notice`.
- `tool_use` events carry `id_origin` (`real` when the provider emitted the id, `synthesized` when zdx generated one because the provider omitted it; default `synthesized` for old transcripts) and an optional `replay` token (e.g. Gemini per-part `thoughtSignature`). Replay metadata is preserved verbatim so multi-turn provider caches (e.g. Gemini's implicit prompt cache) can hit on subsequent turns.
- `tool_result` events carry optional `duration_ms`, the client-observed execution duration for a real tool call. It is absent on older transcripts and synthetic results that did not execute a tool. Parallel results remain persisted in request order; summing their durations measures tool work, not wall time.
- `usage` events carry optional `model` and `provider` fields recording which model/provider produced that usage, so token/cost can be attributed per provider even when the model is switched mid-thread. Both default to absent on older transcripts (attribution then falls back to the thread's model). A request's terminal `usage` event also carries optional `duration_ms` (wall-clock request time) and `ttft_ms` (time-to-first-token) for latency/throughput stats; both are absent on interim/failed usage and on older transcripts. Adding these fields is additive and does not bump `schema_version`.
- `message` and `reasoning` events also carry an optional `replay` token for the same reason.
- `notice` events (e.g. model `refusal`, `model_context_window_exceeded`) are persisted for UI replay and MUST NOT be rehydrated as conversation messages sent back to providers.
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
  - tool call: `tool_timeout_secs` when configured, otherwise 30s for MCP tools

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
- Built-in `Todo_Write` tracks a flat per-thread todo list for multi-step work and keeps at most one active `in_progress` todo while unfinished work remains.

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
- **Main config thinking precedence:** after config layers merge, an explicit legacy `thinking_level` (including `off`, even inherited from a lower layer) wins. Only when that key is absent does the reader use the main `model`'s thinking suffix, otherwise defaulting to `off`. Readers do not rewrite files or change helper-model resolution.
- **Monitor model saves:** chat model selections persist a single `model@thinking[@fast]` string. Saving the main model removes its separate `thinking_level`; favorite and subagent-override saves likewise omit their legacy `thinking`/`thinking_level` keys. Subsequent template-merging saves preserve the absence of the main legacy key instead of reintroducing the default `off`. Legacy config fields remain accepted; other legacy model/thinking controls are unchanged. When migrating layered config, bare-model overlays must retain their previously inherited thinking level in their own model string.
- **Favorite thinking precedence:** a favorite's explicit legacy `thinking` wins; absent that key, readers use the favorite model's suffix, then `off`. Account qualifiers and `@fast` survive model saves.
- **`@fast`:** selects the priority service tier (`service_tier: "priority"`, premium per-token rate) and is offered only for providers that accept it (OpenAI, OpenAI Codex). A spec without `@fast` sends no service tier. Acceptance is not a grant — the `ChatGPT` Codex backend takes the field and may still serve `service_tier: "default"` — so whenever the served tier differs from the requested one, the downgrade is logged. `@fast` is selected from the model picker (or typed into any model field: config `model`, favorites, thread overrides, subagents, automations) and stays visible wherever the model name is shown.

### Provider-level config

- Each provider may expose `base_url` and `tools` overrides under `[providers.<id>]` in config.
- Provider implementations live in `zdx-providers`; the models registry (`models.toml`) tracks available models per provider.

### Anthropic adaptive thinking

- Adaptive thinking (`thinking.type: "adaptive"`) is used on Claude Opus 4.7, Opus 4.6, and Sonnet 4.6.
- We always send `thinking.display: "summarized"` so visible thinking text is preserved. This is required on Opus 4.7 (where the API default silently became `"omitted"`) and is a no-op on older Claude 4 models where `"summarized"` is already the default.

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
- Writes (`zdx config`, favorite/Telegram saves, monitor edits) target `$ZDX_HOME/config.toml`.
- Exception: interactive model/thinking selections (TUI `/model`, `/thinking`, and the Telegram bot's General-topic equivalents) are workspace-scoped. They are written to `<project root>/.zdx/config.toml`, where the project root is the nearest layered directory (cwd first, never above home) that already contains a `.zdx` directory. `.zdx` is an opt-in marker: a directory with only `.git` is not a project root. Outside any project they fall back to `$ZDX_HOME/config.toml`.
- Workspace writes are minimal: only the changed key is written, and a missing overlay file is created empty rather than seeded from the default template.

### MCP configuration

- MCP server configuration is not stored in `config.toml` for this slice.
- The authoritative supported MCP source is a project-local `.mcp.json` file using the standard `mcpServers` JSON shape.
- Missing `.mcp.json` is normal and does not affect startup.
- Invalid `.mcp.json` or server-specific MCP failures are warnings/non-fatal conditions rather than startup errors.

### Contracts

- Config is the single source of truth for user preferences (model, tokens, timeouts, prompt customization, memory paths, skill sources, subagent settings).
- Adding a new config key or provider section should not require a spec update — the config struct in code (`zdx-engine`) is authoritative for the full schema.
- `max_tokens` is optional; when unset, providers that support omitted limits use provider defaults. Providers that require a limit use an internal fallback from model metadata.
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
- `invoke_subagent` accepts `subagent: <name>`. When omitted, it uses the default/base system prompt behavior.
- `invoke_subagent` also accepts optional per-invocation `model` and `thinking_level` overrides. Resolution precedence is explicit invocation override → named subagent profile → parent/default configuration; profile prompt, tools, and context behavior remain unchanged.
- For each `[subagents.overrides.<name>]`, config readers resolve thinking after layer merging: an explicit legacy `thinking_level`, including `off`, wins over the override model's `@thinking` suffix. When neither exists, the override leaves thinking unset so the existing definition/caller fallback remains. Readers preserve model strings and do not rewrite files; override saves persist the chosen thinking level in `model` and remove the separate key.
- Reserved runtime alias `task` explicitly selects that same default delegated-worker behavior using the normal base prompt + context pipeline.
- The `task` alias is intended for complex multi-step, output-heavy, or independently parallelizable delegated work; direct execution should stay the default for small tasks.
- Delegated child runs should be prompted self-sufficiently: the parent should include the goal, relevant context, constraints/non-goals, expected output, and verification when relevant rather than assuming the child inherits its implicit reasoning state.
- When a named subagent is selected, its body is rendered with the same prompt-template syntax/vars as the main prompt pipeline, then used as the child run's system prompt directly; it does not inherit the default ZDX prompt/context pipeline unless that text is written into the subagent body.
- Named subagents may declare `skills:` (allowed on-demand skills) and `auto_loaded_skills:` (skills whose `SKILL.md` contents are injected directly into the subagent prompt). Auto-loaded skills should be treated as already in context for that run.
- A subagent definition may declare `allowed_subagents`, restricting which subagents it can reach via `invoke_subagent`. The restriction is enforced twice: the tool schema advertises only the listed subagents (dropping the `task` alias unless listed) and marks `subagent` required, and execute-time resolution rejects any unlisted name. When restricted, an omitted `subagent` argument is an error rather than the implicit `task` fallback, so a restricted caller cannot reach the default coding agent by leaving the argument out. `allowed_subagents` may not list the declaring agent itself or the reserved `orchestrator` profile. Omitting the field leaves the caller unrestricted.
- Explicit subagent skill dependencies are resolved from enabled sources even if global `include_skills` / `ignored_skills` filters would otherwise hide them.
- Built-in subagents currently include:
  - `explorer`: a read-only local exploration specialist for open-ended multi-step discovery across the current workspace, broader machine-local filesystem paths, and saved thread history.
  - `oracle`: a read-only deep reasoning advisor for code review, difficult debugging, planning, and architecture decisions. Its output is advisory and should be independently validated by the parent agent.

### Models registry

- Path: `<base>/models.toml` (falls back to `default_models.toml` when missing).
- Persistent user metadata overrides live at `<base>/model_overrides.toml`. `[[override]]` entries use a provider-qualified `id` and may override display name, pricing, context/output limits, reasoning, image input, and API routing metadata. An override may also define metadata for a custom-provider model absent from `models.toml`; it survives `zdx models update` because the generated registry and user overrides are separate files.
- Tracks available models per provider. Entries support `*` wildcards for `zdx models update`.
- `zdx models list` prints models from enabled providers as `provider:model` ids (the exact value accepted by `-m`), with `--all` to include disabled providers, `--provider <id>` to filter by provider, and `--json` for machine-readable output.

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
- Slash commands that act on setup/status do not auto-create topics from `General`; they run in place instead (for example `/model`, `/thinking`, `/status`, `/worktree`).
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
  - if generation or topic creation fails, the staging session stays open: sending another message retries, Discard / `/cancel` aborts
  - Discard (or `/cancel`) deletes the staging messages — the bot's own always, the user's best-effort (needs `can_delete_messages`) — and leaves the source thread untouched
  - `/handoff` outside a forum topic (DM or `General`) does not start staging; the bot explains it needs a topic
  - stale staging sessions expire; a message after expiry runs as a normal agent turn
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

- A topic created from an ordinary `General` message in a forum chat is initialized as an orchestrator topic before its first turn **only when the chat's Telegram profile opts in** with `telegram.profiles.<name>.orchestrator = true` (default `false`; unprofiled chats never opt in). Chats without the flag keep classic behavior (General → normal coding topic). `/new`, the launcher, `/handoff`, and `/btw` topics keep the default profile regardless.
- With BotFather **Threaded Mode** enabled, each brand-new thread in the bot's private chat is likewise initialized as an orchestrator thread on its first sighting: the message must carry a client-created `message_thread_id`, not be a known slash command, and map to a thread file that does not exist yet. It also gets the same pinned orchestrator card a General-created topic gets, posted right after the user's first message (pinning is best-effort in private chats). Plain unthreaded DMs and pre-existing DM threads keep their current profile.
- Orchestrator turns use the built-in profile's rendered prompt — which composes a ZDX operating manual, the full discovered skills catalog, project context, memory, and a bounded recent-activity snapshot (most recently active projects and top-level threads) — plus the Telegram instruction layer, and exactly its declared tool list: `read`, `grep`, `glob`, `ask_media`, `telegram`, `git`, `gh_api`, `thread_search`, `read_thread`, `todo_write`, `memory_search`, `web_search`, `fetch_webpage`, plus the six thread controls below. `bash`, `edit`, `write`, `apply_patch`, `invoke_subagent`, and the background tools are excluded.
- In the Telegram bot, orchestrator turns also receive a **Telegram Workspaces** block: every bound profile (name, chat id, root, orchestrator flag) and, under each, the project-level skills a worker in that root will discover (`.zdx/skills`, `.claude/skills`, `.agents/skills` of the root and its ancestors; bundled and user skills are excluded since the orchestrator already has them). A skill reachable from nested workspaces is listed once, under the deepest root containing it. Descriptions are cut to their first line (≤140 chars). This is how the orchestrator knows what a workspace can do without running there.
- The orchestrator's read-only posture is structural: its declared tool list contains nothing that can mutate local or remote state, so mutations and anything needing a shell must go to a worker.
- The orchestrator has no subagent access. `invoke_subagent` is excluded so every unit of delegated work is a visible, mirrored worker thread the user can follow and interrupt, rather than a hidden child run blocking the middle of a reply.
- The reserved `orchestrator` profile is not invokable as a subagent by anyone. Beyond being hidden from the advertised catalog, subagent name resolution rejects it, so `invoke_subagent`, `zdx exec --subagent`, and automation `subagent:` frontmatter all fail rather than starting a second home base. The bot resolves the profile from the embedded definition instead.
- The orchestrator inherits the chat's configured model and thinking level.
- An optional personal overlay at `$ZDX_HOME/orchestrator.md` is appended to orchestrator prompts only (never to workers or other agents). A missing or empty file is a no-op; read failures are logged and skipped.
- Orchestrator topics get the same async LLM title on their first message as other topics (Threaded Mode DM threads are otherwise left as "New Thread" or a truncated first line), and do not post retry buttons on failed turns.
- The orchestrator topic's pinned header (and `/status`) uses an orchestrator card: model/thinking, thread, profile, context/usage/pricing, plus a live worker summary (running/queued/settled counts and the first few workers by title and status; each worker title links to its mirror topic, or to the worker thread in the Mini App when the mirror has no topic link). Root and branch lines are omitted as fixed noise for the home base. The bot re-renders a card it posted in the current process whenever one of that orchestrator's workers is created, prompted, starts its first tool call, or finishes, so the pinned card is a live status; `↻ Refresh` remains for cards posted before a restart.

### Worker threads

- A worker is an ordinary visible thread bound to one existing project root, created by the orchestrator. Its meta records the owning orchestrator as `parent_thread_id` (lineage only; no `origin_kind`, so it stays a visible top-level thread and `zdx threads show <orchestrator>` lists it as a child). Worker turns run through a child `zdx --thread <id> exec` process in that root, so the worker resumes its own persisted JSONL history on every prompt and can also be resumed manually.
- Prompts for one worker run strictly serially through an in-memory FIFO; different workers run concurrently with no global cap.
- Cancellation stops the current worker turn by terminating the child's entire process group (TERM, short grace, then KILL), reaps it before any queued successor could start, and clears the queue. The thread survives and a later message resumes it.

### Thread-control tools

Available only where a live worker manager exists (the Telegram bot); elsewhere the tools fail with `orchestrator_unavailable`:

- `create_thread(root, prompt, title?, model?, thinking_level?)` — validates the root exists, creates a visible worker thread, queues the first prompt, and returns the thread id plus the mirror topic's `mirror_url` when the surface opened one. The tool waits a bounded moment (≤5s) for the asynchronously created mirror to resolve — the worker is already queued and never waits on it; a surface that opens no mirror resolves the wait immediately with `null`.
- `send_thread_message(thread_id, message)` — queues another prompt; re-attaches an unmanaged existing thread from its persisted root.
- `get_thread_status(thread_id?)` — status (`queued`/`running`/`completed`/`failed`/`cancelled`), queue depth, bounded latest final text, and `mirror_url`; without an id, lists all workers owned by the calling orchestrator. A managed worker also reports `current_tool` (the tool in flight, from live activity), `seconds_since_last_activity`, and `turn_elapsed_seconds`, so a worker doing slow work can be told from one that is hung. For a thread this process does not manage but whose file exists, the call succeeds with `managed: false` and only `seconds_since_last_write` (the thread file's mtime): live tool activity exists solely in the owning process, the JSONL has no `tool_started` event, and a running tool's `tool_use` is not flushed until its turn checkpoints, so `current_tool` is null rather than guessed.
- Managed status also includes `current_tool_input`: the shared primary-command/target extractor's preview, whitespace collapsed and capped at 200 Unicode characters including any truncation ellipsis. Name and preview are correlated by tool-use id and describe the most recently started unfinished call; finishing it falls back to any earlier unfinished call. The preview is null before input arrives, when no primary argument exists, or when idle, and is omitted for unmanaged threads. Input activity never attaches to a different id. Activity age is a hint, not proof of a hang.
- `get_thread_status` includes nullable `context` for managed and unmanaged threads: `input_tokens` is the latest recorded input-bearing request's noncached input + cache-read + cache-write tokens, never cumulative thread usage and never output tokens. `model`, `provider`, and `recorded_at` come from that record, not current thread overrides. A positive registry `context_limit` matched by provider and model yields `percent_used`; unknown attribution or limit leaves those two fields null while retaining the token count. No usable usage within the bounded final 256 KiB (or unreadable history) yields `context: null`, not zero. This is a last-request estimate (`basis: last_recorded_request_input`), not an exact forecast of resumed context; trailing output-only usage does not replace it.
- `wait_for_threads(thread_ids, timeout_seconds?)` — waits until all listed workers are idle or a bounded timeout (default 60s, max 600s) expires.
- `update_thread(thread_id, title)` — title only; refused while that managed worker is mid-turn (the atomic rewrite must not race the worker's own appends).
- `cancel_thread(thread_id)` — cancels the current turn and clears the queue; the thread is preserved.

### Worker mirror topics

- When an orchestrator creates a worker, the bot also opens a **mirror topic** for it, named after the worker. The host chat is the group whose Telegram profile `cwd` contains the worker's project root (deepest match), falling back to the orchestrator's own chat — including a Threaded Mode DM home, where bots may create topics — so project workers surface in their project's group even when orchestrated from a DM. The topic's thread is a thin pointer: it aliases the worker thread (`alias_to`) and is marked `worker_topic` in its meta.
- A supergroup mirror has a tappable link (`https://t.me/c/<internal_chat_id>/<topic_id>`), registered on the worker manager as the worker's `mirror_url` and surfaced through `create_thread`, `get_thread_status`, and the `[worker update]` callback (`Mirror topic: <url>`). DM-hosted mirrors have no link form and report `null`.
- The orchestrator's reply automatically ends with one `🛠 <worker title>` link per worker the turn created or messaged (successful `Create_Thread`/`Send_Thread_Message` results, first-touch order, placed above `↗ Open thread`), so the user reaches the mirror without relying on the model to link it. A worker whose mirror has no topic link (DM-hosted) links to the worker thread in the Mini App instead; the line is skipped only when neither exists.
- While a worker turn runs, its mirror shows one live status line naming the tool currently running (`🔧 Running `bash`...`, `⏳ Waiting for model...`), the same shape as a normal turn's status: no arguments, no history (the thread itself has the detail). It is edited at most every 3 seconds and deleted when the turn's result posts.
- A mirror topic opens with the **same pinned status card as any other topic** (`post_thread_header`), computed for the worker thread itself: model and thinking (a worker created with `model`/`thinking_level` overrides has them persisted on its thread meta, so the card and `zdx threads` report what it really runs with), thread id, profile, root, branch, context/usage/pricing, plus the `💬 Open Thread`, `🎛 Open orchestrator` (the owning orchestrator's topic, or its thread in the Mini App for DM homes; derived from the worker's persisted parent, so it survives restarts), and `↻ Refresh` buttons. Refresh resolves the topic's alias, so it always refreshes the worker thread.
- Each finished worker turn posts its final text (rendered from Markdown, bounded, followed by the same `↗ Open thread` Mini App link normal answers carry, so a truncated result is one tap from its full transcript) — or its failure/cancellation — into the mirror topic, in addition to the orchestrator callback. Orchestrator-sent prompts are posted there too as `📤` messages rendered from Markdown (the first prompt right under the pinned card, follow-ups as they are sent), so the topic reads as a full prompt → activity → result conversation; topic-originated prompts are not re-posted since the user's message is already visible.
- Only the live status message carries inline buttons (prompts and results have none; the pinned card has Open Thread + Refresh): `⏹ Cancel worker` (callback `wk:c`; the worker is resolved from the topic's persisted alias, never from callback data) cancels the current turn and clears the queue exactly like `cancel_thread`, answering "nothing to cancel" when the worker is not managed; `💬 Open Thread` opens the worker thread in the Mini App (`{mini_app_url}?startapp={worker_thread_id}`) and is omitted when the Mini App is not configured for the host chat.
- Messages sent in a mirror topic never run an in-process turn (the worker's child process owns the aliased JSONL); **every** mirror-topic message — including slash commands and staged flows — is checked before any local interpretation, queued verbatim into the worker's FIFO, and acknowledged. Retry callbacks are also refused in mirror topics. After a restart, a mirror-topic message re-attaches the worker owning itself, so results keep landing in the topic even though orchestrator callbacks are gone.
- Managed workers are excluded from the General launcher's `🔄 Continue` picker while managed, so a resume topic can never become a second writer on a running worker's thread.
- Mirror-topic creation is best-effort: when no candidate chat accepts the topic it is skipped (the worker still runs and its `mirror_url` resolves to `null`).
- **Restart recovery:** at startup the bridge scans persisted thread metadata once for `worker_topic` + `alias_to` pairs and rebuilds the worker→mirror map (and each worker's `mirror_url`) from it, so mirror feeds and buttons resume automatically for every worker re-attached after a restart — nobody has to message the topic first. Recovery is derived purely from the canonical JSONL meta lines; there is no separate store.

### Completion callbacks and restart semantics

- After every finished worker prompt, the bot dispatches one best-effort synthetic `[worker update]` message (worker id, status, mirror link, bounded final text) into the owning orchestrator topic through that topic's normal queue, using the same synthetic-message mechanism as `/goal` continuations. Because the synthetic message itself is invisible in chat, a one-line notice is posted in the orchestrator topic first (`✅ Worker 🛠 <title> finished · reviewing…`, with the title linked to the mirror topic when it has one; `🚫 … was cancelled` / `❌ … failed`), so the user can tell which worker woke the orchestrator and jump to it.
- All manager state is in-memory by design: worker ownership, queues, routes, and pending callbacks are lost on bot restart. Orchestrator and worker JSONL transcripts survive; workers can be re-attached with `send_thread_message` or a mirror-topic message, and their mirror associations are recovered at startup as described above. There is no database, scheduler, durable job store, or delivery guarantee.
