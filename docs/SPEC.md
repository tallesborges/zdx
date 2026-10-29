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
- `zdx jobs list [--json] [--daemon|--archived]|validate|daemon|runs [NAME] [--date*] [--json]|set-model <NAME> (<SPEC>|--clear)|run <NAME> [--keep]|migrate [--dry-run]`
- `zdx service install|uninstall|start|stop [bot|daemon|all]`, `zdx service restart [bot|daemon|all] [--force|--queue]`, `zdx service status [--json]`, `zdx service logs [bot|daemon|all] [--lines N] [--err]` — manage the long-lived `bot`/`daemon` services under launchd (macOS)
- `zdx threads list [--all|--workers]|show <ID>|resume [ID]|search [QUERY] [--date*] [--limit N] [--json]|tools [TOOL] [--failed] [--date*] [--limit N] [--json]`
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
- `openai:gpt-image-2.5-flare` and `openai:gpt-image-2.5-sunburst` (and their dated snapshots) run through the Responses `image_generation` tool with the exact ID in the tool's `model` field. This is the route for explicit Flare/Sunburst selection.
- `openai-codex:*` image requests are allowed but the subscription backend is server-managed. `zdx imagine` prints a warning on stderr for every such request: the requested image model or variant cannot be guaranteed, the model that ran is not reported, and the requested size and quality were not honored in probes (it chose its own size, for example 1536x1024 for `--size 1K`). Codex responses carry no effective-model field; output-token counts matched the `gpt-image-2.5-flare` tiers, which is suggestive and not proof of the model.

### `zdx mcp ...` (non-interactive helper, scriptable)

- **stdout:**
  - `servers`, `tools`, and `schema` print structured JSON inspection data.
  - `call` prints the normal ZDX `ToolOutput` JSON envelope.
  - `auth` and `logout` print human-readable status/instructions.
- `servers` may report MCP server states such as `loaded`, `auth_required`, or `failed`.
- **stderr:** CLI usage/runtime errors.

### `zdx service ...` (macOS/launchd)

- launchd owns the lifetime of the `bot` and `daemon` services: start at login, restart on crash. Telegram `/restart` restarts the daemon first, then exits the bot so launchd restarts it. `/restart f` (also `force`, `--force`) explicitly allows interrupting active runs; `/restart q` (also `queue`) waits until the machine is idle (defined below) and then performs the gated restart automatically, announcing it in the topic that queued it. `zdx service restart [target] --queue` queues the same restart from the CLI, so an agent can request it from inside its own run; the bot announces it in the caller's Telegram thread (`ZDX_THREAD_ID`) when there is one. Both write one persisted request that the bot service performs. Besides no active agent run, the bot also waits until no worker is running or has a prompt queued, no worker completion is still being delivered, and no bot turn is queued or running (this covers the orchestrator's `[worker update]` turn), so a queued restart never drops a worker callback. `--queue` refuses when no supervised bot is running. `just safe-deploy` is install plus this queued restart. Only one queued restart is pending at a time: a second request (from either surface) merges its services into it and reports that one was already queued. A pending request is dropped once every requested service runs a process started after it, so a forced or manual restart in the meantime satisfies it. `zdx service status` (text and `--json` `queued_restart`) shows the pending request. `/restart` in any mode bypasses the per-topic queue, so it is answered while a turn runs.
- Agents run `~/.local/bin/zdx` (the `just install` target), never the calling binary, so `restart` always picks up the currently installed build.
- Agents are launched via `zsh -c 'exec …'` so `~/.zshenv` is sourced; launchd sources no shell startup files, and provider API keys live there.
- `install` refuses when the service is already running outside launchd; PID-file uniqueness continues to prevent duplicate instances.
- `stop` is durable: the service stays stopped until an explicit `start`, across reboots.
- `restart` waits for the old process to exit before the replacement starts, and reports `PID old → new`.
- `restart` refuses while any agent run is active across ZDX surfaces. A multi-service restart checks once before changing either service. CLI `--force`, Telegram `/restart f`, and Monitor `R` bypass the guard and may interrupt those runs; Monitor `r` remains guarded. A blocked Telegram `/restart` lists `/restart q` and `/restart f` as the two ways forward; a blocked CLI restart names `--queue` and `--force`.
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

### Stalled streams

- A provider request that makes no content progress for `stream_idle_timeout_secs` (default 300; `0` disables) fails with a `timeout` provider error: "Provider stream stalled: no output for Ns". The budget starts when the request is sent, so a stream that never opens counts too.
- Any stream event except a keepalive ping is progress and resets the budget. Pings do not, so a proxy that keeps pinging cannot hide a silent generation. The limit bounds silence, not total duration: a long answer that keeps streaming is never cut off.
- The stall error follows the retry rules above: before visible output it is retried transparently (each attempt gets a fresh budget, at most three retries); after visible output it ends the turn.
- Streaming HTTP clients also set a 30s connect timeout and a 20-minute per-read timeout as a transport backstop. There is no total request timeout.

---

## 8) Threads

Threads are append-only **JSONL** event logs (thread events are never modified or deleted).

### Storage

- Base dir: `$ZDX_HOME` (if set) else `~/.zdx`
- Threads dir: `<base>/threads/`
- OAuth cache: `<base>/oauth.json` (0600 perms)
- MCP OAuth cache: `<base>/mcp_oauth.json` (0600 perms)
- `zdx bot` resolves Telegram credentials/settings from `[telegram]` in `config.toml`. `[telegram]` carries identity, routing, and the optional Mini App server (`bot_token`, allowlists, `profiles`, `server`); the bot's model and thinking level come from the layered config like every other surface.
- Telegram bot chat profiles live under `telegram.profiles.<name>` in `config.toml` with `chat_id` and `cwd`; matching chats run agent turns from the profile cwd, and unprofiled allowed chats keep using the bot root fallback. A profile plays one of two roles. A **workspace** profile (the default) lists its `cwd`'s project skills and is listed to orchestrators as a worker destination. An **orchestrator** profile (`orchestrator = true`) is a management home base: it keeps the same chat→root binding for its own turns but is excluded from both, and takes an optional `worker_root` (defaulting to `cwd`) used as the default root for workers it creates without one. Any profiles may share a `cwd`: workspace groups on one root each list that root's skills, and an orchestrator home on a workspace's root lists none. Two chats sharing a `cwd` also resolve the same workspace-layer model defaults; per-topic `/model` and the `[subagents.overrides.orchestrator]` override apply on top, unchanged.
- `telegram.profiles` is rejected at load when: two profiles share a `chat_id`, or a `worker_root` is blank or is not an existing directory. A `worker_root` does not need a covering workspace profile, since workers open no Telegram topic. `zdx bot profile add` and any profile re-save write `orchestrator` and `worker_root` back, so neither is dropped on rewrite.
- Each Telegram profile gets its own layered config anchored at the profile `cwd`, so a workspace `.zdx/config.toml` applies to chats bound to that profile. Profile configs are built once at startup; unprofiled chats use the bot-level config. Runtime `/model` changes in a General topic are workspace-scoped: they write the unified model spec to the chat root's overlay and update only that chat's config.
- Every bot-created Telegram forum topic starts with a status-style thread header. The bot pins it silently when it has `can_pin_messages`, keeps the topic usable if pinning fails, and provides a refresh action plus an Open Thread button when the Threads Mini App is configured. Resumed topics display and open their effective source thread.

### Format

- First line is `meta` with `schema_version`, optional `title`, and optional lineage fields (`origin_kind`, `parent_thread_id`, `subagent_name`) for threads spawned by another agent run.
- Timestamps are RFC3339 UTC. New events retain millisecond precision; older second-precision events remain valid.
- Event types: `meta`, `message`, `tool_use`, `tool_result`, `interrupted`, `reasoning`, `usage`, `notice`, `delivery`.
- `tool_use` events carry `id_origin` (`real` when the provider emitted the id, `synthesized` when zdx generated one because the provider omitted it; default `synthesized` for old transcripts) and an optional `replay` token (e.g. Gemini per-part `thoughtSignature`). Replay metadata is preserved verbatim so multi-turn provider caches (e.g. Gemini's implicit prompt cache) can hit on subsequent turns.
- `tool_result` events carry optional `duration_ms`, the client-observed execution duration for a real tool call, plus optional `started_at`/`completed_at` wall-clock boundaries. The boundaries preserve real overlap between parallel tools. These fields are absent on older transcripts and synthetic results without an honest execution span. Parallel results remain persisted in request order; summing durations measures tool work, while the union of explicit spans measures wall time.
- `usage` events carry optional `model` and `provider` fields recording which model/provider produced that usage, so token/cost can be attributed per provider even when the model is switched mid-thread. Both default to absent on older transcripts (attribution then falls back to the thread's model). A successful request's terminal `usage` event also carries optional `duration_ms` (wall-clock request time), `ttft_ms` (time-to-first-token), and explicit `started_at`/`completed_at` boundaries. Interim/failed usage and older transcripts omit request boundaries. Adding these fields is additive and does not bump `schema_version`.
- `message` and `reasoning` events also carry an optional `replay` token for the same reason.
- Every request builder (Anthropic wire, chat-completions, OpenAI Responses, Gemini) applies one shared reasoning-replay rule: a thread's reasoning is replayed only for the in-flight turn — the assistant messages at or after the last genuine user turn (tool-result carriers use the `user` role and do not start a turn). Older turns' reasoning is dropped from the request; an assistant message left with no content at all is dropped with it. The rule is single-sourced in `zdx_types::ReasoningReplay` and matches each provider's own documented scope (OpenAI: "since the last `user` message"; Gemini: "only current turn is required; we don't validate on previous turns"; Anthropic: prior-turn `thinking` blocks may be omitted). The one exception is a redacted thinking block, whose opaque payload is always echoed back verbatim. Replaying every prior turn's reasoning grows a request without bound on backends that do not filter prior-turn thinking server-side, and can push a long thread past its context limit. Per-part replay tokens that are not reasoning — a Gemini `thoughtSignature` on a `functionCall`/`text` part, a per-part Gemini signature — are unaffected, since providers require those on the current turn's parts and dropping earlier ones is not validated. Because the drop is deterministic and applied on every request, a kept block's prefix still matches the request that produced it, so prompt-cache prefixes stay stable across turns.
- A reasoning block persisted above 128k characters is truncated head-and-tail with a marker, and its replay token is dropped (a truncated block no longer matches its signature).
- `notice` events (e.g. model `refusal`, `model_context_window_exceeded`, `max_tokens`, runaway reasoning) are persisted for UI replay and MUST NOT be rehydrated as conversation messages sent back to providers.
- `delivery` events record what a chat surface actually delivered for the preceding final reply: `id`, `surface`, optional `chat_id`/`topic_id`, and one item per part (`text`, or `file` with `path`) with `status` `pending`/`sent`/`failed`/`unknown`, an optional `message_id`, and an optional error excerpt. A `pending` record is written before the sends and the outcome after them with the same `id`; the later event wins, and a lone `pending` record means the delivery is still running or the process stopped mid-send (unconfirmed either way). Delivery is never a conversation turn: replay prepends one `<delivery_status>` text block listing the undelivered items to the next user message, so the model does not claim a failed file was sent; fully delivered replies add nothing. Transcripts and the Mini App show undelivered items as a warning; superseded pending records are not shown. Known limit: a Telegram `🔄 Try again` retry records no user message, so the note reaches the model at the next user message, not in the retry turn; and because retries run outside the topic queue, a user message recorded while a retry is still delivering carries a "not confirmed" note for that delivery even if it later succeeds.
- Child runs spawned by another agent — user-visible subagents (`invoke_subagent`) and internal helpers (title, tldr, handoff, prompt-builder, `read_thread`) — persist their own thread JSONL tagged with `origin_kind` (e.g. `subagent`, `helper:title`) plus `parent_thread_id`/`subagent_name`. These threads are hidden by default from `zdx threads list`, the TUI thread picker, `thread_search`, the monitor dashboard, and native memory export (use `zdx threads list --all` to include them), but their token usage IS counted by `zdx stats`. `zdx threads show <id>` displays lineage: a parent-link header when the thread is itself a child, and a "Child runs" section listing each spawned child with its tokens and cost.
- **Worker threads** (§18) are identified by one canonical rule, derived from existing metadata rather than a stored type: a top-level thread (`origin_kind` unset) that records a `parent_thread_id`. Subagent and helper runs also record a parent but set `origin_kind`, so they never match; mirror-topic alias threads record no parent. Every surface filters with this same rule: `zdx threads list --workers` (labelled `[worker ← <parent>]`; the default list labels workers the same way), the Mini App thread list (`GET /api/threads?kind=workers`, plus an `is_worker` flag on every row), and the monitor's thread kind filter.
- Threads remain readable even if interrupted mid-stream.
- **Live run status.** While a turn runs, its process keeps a marker in `$ZDX_HOME/run/agents/` with the model, phase (`waiting`, `thinking`, `answering`, `retrying`), the calls of the current tool round (command or target summary, input up to 16 KiB, the last 8 KiB of streamed output), `last_activity_at` (any agent event, model streaming included), and `heartbeat_at` (rewritten every 5 s). A finished call stays listed, with `completed_at`, `ok`, and duration, until the next round starts; surfaces hide a call once its `tool_use` is in the thread file, so a call is never invisible between finishing and the checkpoint, and never listed twice. The shared `RunStatus` derived from the marker (`state` `tool` while a call runs, else the phase; one-line `label`; turn, activity, and heartbeat ages; running and finished call counts) is what the Mini App, the Monitor, and `get_thread_status` show. Ages are observations: a quiet tool or a long model stream can be healthy, and a fresh heartbeat shows only that the process is alive, so no surface labels a run stuck. Reasoning text is never stored in the marker. The marker is removed when the turn ends; a marker whose process is gone is ignored and removed. The marker records its process's OS start time (`birth_id`), so a marker left by a killed process is also ignored once its PID belongs to another process (a start time that does not match; for older markers without it, on macOS, a process that started more than 60 s after the marker was written). Heartbeat and activity ages never decide this: a sleeping Mac writes no heartbeat. Queued restarts and subagent continuation read the same list.
- `zdx threads inspect <THREAD_ID>` renders the shared trajectory projection used by Monitor and the Mini App. `--json` emits one machine-readable document containing source-event count, observation time, per-turn spans/tracks, timing quality, observed interval aggregates, and bottleneck ranking. Recorded execution duration remains distinct from explicit wall-boundary width; legacy duration-only spans can rank but are never positioned on an inferred timeline.

### Durability

- The persistence layer flushes ordered batches at tool-turn boundaries (after each `process_tool_turn` completes) and at terminal `TurnFinished`. Streaming text/reasoning/tool-input deltas are no longer persisted directly.
- A hard crash can lose any assistant content streamed since the last checkpoint or terminal flush; completed prior tool turns are durable. Long tool loops persist incrementally between turns, so an interrupted multi-tool run keeps everything up to the last completed tool turn.
- Within a flushed batch, blocks are persisted in the exact stream order the provider produced them. This preserves Gemini's per-part replay fidelity and is required for implicit-cache hits on the next turn.

### Metadata Updates

The `meta` line (first line only) may be rewritten atomically to update thread metadata (e.g., `title`). This uses write-to-temp-then-rename for safety. Thread events after the meta line are never modified.

### Jobs

A job is a saved prompt that runs unattended: by hand, on a recurring schedule, or once at a set time.

- **File:** one markdown file per job, `$ZDX_HOME/jobs/<name>.md` (not recursive; the name is the file stem), with YAML frontmatter and a non-empty prompt body. Keys: `schedule`, `run_at`, `grace_secs`, `model`, `subagent`, `root`, `timeout_secs`, `max_retries`. Unknown keys are rejected.
- **Schedule:** `schedule` (recurring) and `run_at` (once) are exclusive; neither means manual only. `schedule` is 5-field cron (minute hour day-of-month month day-of-week; each field `*`, `N`, `*/N`, `A-B`, or a comma list; day-of-week 0-6, with 7 also Sunday in values and ranges such as `1-7`; day-of-month and day-of-week must both match), evaluated in the daemon's local time. `run_at` is RFC 3339 with an explicit offset or `Z` and whole seconds; a value without an offset, or with fractional seconds, is rejected. `grace_secs` (60 to 604800) is how late an occurrence may still start; defaults: once 6 h, recurring 10 min; rejected on a manual job.
- **Invalid files fail alone:** a file that does not parse (YAML, unknown key, bad cron or `run_at`, both schedule keys, bad `grace_secs`, empty prompt, duplicate name) is listed as `invalid` with its error and never runs. The other jobs list and run as usual.
- **Occurrences and claims:** each scheduled run is an occurrence with a key: the UTC instant for once jobs (`YYYYMMDDTHHMMSSZ`), the local wall-clock minute for recurring jobs (`YYYYMMDDTHHMM`, so a local minute repeated by a DST change runs once and a skipped one never matches). A run takes the job's lock, then creates the occurrence's claim exclusively (`$ZDX_HOME/state/jobs/<name>/claims/<key>.json`) before it starts; the claim later records the final outcome. The scheduler therefore starts each occurrence at most once, across ticks, restarts, and manual runs. Side effects are not exactly-once: a retry or a manual rerun runs the whole prompt again.
- **One live run per job:** every run holds `$ZDX_HOME/state/jobs/<name>/lock` (advisory `flock`) for its whole duration. The OS releases it when the process dies. A run that finds the lock held fails with "already running" and exit code 75.
- **Scheduler:** `zdx jobs daemon` reads the job files on every tick, so adding, editing, or removing a job needs no restart; a run uses the file as it was when the run started. It wakes at the next due instant or after `--poll-interval-secs` (default 30). Each due occurrence runs as a child process, `zdx --root <daemon root> jobs run <name>` in the daemon's working directory with `--system-prompt` and `--no-thread` passed through, so each run loads config as it is at run time and has its own runtime environment. At most 3 runs are live at once. An occurrence that cannot start yet (job busy, limit reached) stays due until its grace ends.
- **Missed runs:** a once job starts on the first tick at or after `run_at` while within grace; past grace without a claim, it is recorded `missed` and archived. A recurring job starts only its latest occurrence within grace; earlier occurrences that leave grace unstarted are recorded as one `missed` summary per tick with their count and the latest scheduled time, looking back at most 7 days. The summary is written under the job's lock, then the job's checkpoint (`$ZDX_HOME/state/jobs/<name>/missed.json`) moves to that latest time, and later ticks skip occurrences up to it. When the summary cannot be written (lock busy, write error), the tick is not saved, so the next tick records it; a crash between the summary and the checkpoint can repeat that one summary. The scheduler's last check time never moves backward, so a clock change does not repeat them. Recurring occurrences at or before `since` in `$ZDX_HOME/state/jobs/scheduler.json` never run or count as missed. `since` is the scheduler's first start on this `ZDX_HOME` (so the first start after a migration does not repeat runs the old daemon did), raised to the pruning cutoff before finished recurring claims older than 8 days are pruned (so a clock moved back cannot rerun an occurrence whose claim is gone). Once-job claims are kept.
- **Failure and retry:** a failed attempt is retried up to `max_retries` times, waiting 60 s times the attempt number before each retry. All attempts share one `run_id` and one thread. A run that cannot be prepared (missing root, bad subagent) fails without retry, is recorded as failed, and never falls back to another root. An interrupted attempt ends the run as `interrupted`. A scheduled run process that exits before taking its occurrence (other than for a busy lock) records the occurrence as `failed`.
- **Interrupted runs:** a claim without an outcome whose lock is free means its run died (crash, kill, restart). The daemon marks it `interrupted` at start, when its child exits, and on a rescan every 10 minutes; a claim whose lock is still held by another process is watched until that run ends. An interrupted occurrence is never rerun automatically.
- **Archive:** when a once occurrence reaches a final outcome (`succeeded`, `failed`, `missed`, `interrupted`), its file moves to `$ZDX_HOME/jobs/archive/<name>--<key>[-N].md`, but only if it still declares the same `run_at` instant; a rescheduled file stays. Job files have no writer lock, so the move never replaces a file and loses no version when the file is edited meanwhile: the archive name is hard-linked and checked again; whatever is at the job path is then taken aside with one rename, and put back if it is a newer replacement. Archives are kept. `zdx jobs list --archived [--json]` and the Monitor Jobs tab (`a`) list them newest first with each occurrence's final outcome. A restored file with the same `run_at` reports `finished` and does not run again.
- **Manual runs:** `zdx jobs run <name>` of a once job consumes its occurrence (claim, outcome, archive); `--keep` runs it as a test and leaves the occurrence pending. A consumed occurrence cannot run again: change `run_at`, or use `--keep`. Manual runs of recurring and manual jobs take no occurrence.
- **Records:** `$ZDX_HOME/jobs_runs.jsonl` (appends serialized by `jobs_runs.lock`, one write per record; a damaged line is skipped with a warning when read) has one line per attempt, plus one per missed or interrupted occurrence: `job`, `run_id`, `trigger` (`manual`/`daemon`), `schedule_kind`, `scheduled_for`, `lateness_secs`, `outcome` (`succeeded`, `failed`, `retrying`, `missed`, `interrupted`), `attempt`/`max_attempts`, start and finish times, `error`, `schedule`, the resolved `model` and `root` (null when the run could not start), `subagent`, and `thread_id`. `$ZDX_HOME/state/jobs/<name>/last.json` holds the latest final outcome. A run writes its final claim before `last.json`; a final claim newer than `last.json` wins in a once job's status, and the daemon rewrites a stale `last.json` from the claims at start and on its 10-minute rescan. A crash after a daemon-written final claim but before its run-log line loses that one history line; the claim and `last.json` still hold the outcome.
- **Status:** shared in `zdx_engine::jobs` (`job_state`, `summaries`, `overview`): `manual`, `scheduled`, `pending`, `due`, `expired`, `running`, `interrupted`, `finished`, or `invalid`, plus `next_run_at` and `last_run`. `zdx jobs list [--json]`, `zdx jobs list --daemon`, the Monitor Jobs tab, and the Mini App Jobs section show it.
- **Threads:** manual and daemon runs persist to timestamped thread IDs by default: `job-<name>-<YYYYMMDD-HHMMSS>` (UTC). `zdx jobs run <name> --thread <ID>` uses the explicit thread ID instead; `--no-thread` disables persistence for that run.
- Job frontmatter may include `subagent: <name>` to run with a named subagent prompt/tool/model configuration.
- Job frontmatter may include `root: <path>` to run in a different workspace. The path is absolute or starts with `~/` (`~` alone is the home directory); empty and relative paths are rejected. These root errors fail only that job at resolution. The directory must exist when the run starts, otherwise the run fails, is recorded as failed, and is not retried. It never falls back to another root. Without `root`, the run uses the caller's root (`--root` of `zdx jobs run`/`daemon`). With `root`, project instructions, skills, subagent lookup, and tools use that root, and config layers are loaded for it, so its workspace `.zdx/config.toml` applies; a global `--system-prompt` is applied again on top of that config. A declared `root` takes precedence over `--root` and `--worktree`.
- Model layers resolve config (inherited) → subagent definition → `[subagents.overrides.<name>]` → job `model`. A suffixless spec inherits the previous thinking level. The global `--model` flag is rejected for `zdx jobs` commands with an error rather than ignored; set `model:` in the job file instead.
- Every job run is one-shot and non-interactive. Its prompt is the exec instruction layer plus the job layer (§ Prompt layers), and the first user turn carries the same runtime-context snapshot as other surfaces (skills catalog, memory index, capability catalog, working-directory tree). With `subagent:`, the subagent body replaces the default system prompt, so the canonical project-instructions block, the base Safety and Git rules, and both layers are appended to it (`prompts/non_interactive_subagent_suffix.md`, built from the same partials as the default template). Project instructions appear once even if the body renders `project_context`. The profile's tools and skills stay as declared: the runtime context carries the memory index and working-directory tree but no skills catalog (only skills the profile allows, as its body renders them), and no delegation catalog when the profile's tools omit `invoke_subagent`. `zdx exec --subagent` is unchanged. The job prompt counts as the request: actions it names count as requested, and running non-interactively grants no other consent.
- Resolution is shared: `zdx_engine::jobs::resolve_run_settings`/`overview` return the effective root and model for each job with an origin per value (`job`, `subagent`, `subagent_override`, `config`, `caller`; plus `thinking_origin` for the thinking level) and the provider, account, bare model id, and thinking level. `zdx jobs list --json` prints these overviews with the schedule and status; a job whose settings cannot be resolved carries `settings: null` and an `error`, and does not hide the others. Inherited values describe the caller's config and root, which can differ from the daemon's.
- `zdx_engine::jobs::set_model` changes only a job's `model:` line: it replaces it (keeping an inline comment), removes it (`--clear`, so the job inherits again), or adds it as the last frontmatter line. Other keys, comments, line endings, and the prompt stay byte for byte. The edited file is parsed first and must differ only in `model`, otherwise nothing is written; a caller can require the declared model it saw (the Monitor does), and the write is an atomic rename that is abandoned if the file changed meanwhile. `zdx jobs set-model` and the Monitor Jobs tab (`Enter` opens the Config tab's model and thinking picker seeded with the job's resolved model; `d`/`Delete` removes the override) both use it. The picker lists models enabled in the config the job resolves with (its own `root:`, else the daemon's config; the global config when neither is known).
- The daemon's own context is shared: `zdx_engine::job_daemon` reports where the scheduled daemon resolves jobs and resolves them that way, independent of the asking process's root. On start the daemon writes `run/daemon.context.json` (PID, `--root`, config directory, start time) and removes it on exit. The context is `running` when that record's PID owns the daemon PID file; otherwise `installed`, read from the launchd plist (applies at the next start, with a note saying whether a daemon is running without a record or none is running); otherwise `unavailable` with a reason (no daemon and no service, unreadable plist, or a service `ZDX_HOME` different from the caller's). Inherited roots then report origin `daemon`. When the context is unavailable or its config fails to load, jobs without their own `root` stay unresolved with that reason; they never fall back to the caller's root. `zdx jobs list --daemon [--json]` prints this view (`{daemon, jobs, error}`), and the Monitor Jobs tab shows it.
- **Migration from Automations (temporary):** while `$ZDX_HOME/automations/` holds `*.md` files, every jobs surface fails with a pointer to `zdx jobs migrate`, and the daemon does not start. `zdx jobs migrate [--dry-run]` refuses while a daemon is running, stops before changing anything on a name clash or an existing `automations_runs.jsonl.migrated`, never overwrites a job file, converts `automations_runs.jsonl` into `jobs_runs.jsonl` (the `automation` key becomes `job`; existing `jobs_runs.jsonl` lines are kept after the old ones) and keeps the original as `automations_runs.jsonl.migrated`, then moves the job files. It leaves `automations_daemon_state.json` in place, unused. The guard and the command are removed one release after the rename.

---

## 9) Tools

Tools are intentionally few, stable, and machine-parseable.

### Background execution: `Bash` modes and background subagents

Three intents, each with its own input and lifetime. They never silently substitute for one another.

- **Foreground (default).** The command is waited on inline and its output streams to the surface as it arrives.
- **`background: true`.** Detached at spawn in its own session; outlives the turn and the zdx process. Rejects a positive `timeout_secs`. Unchanged by the foreground bound.
- **`timeout_secs > 0`.** With background execution on (see the execution policy below), a foreground **wait** budget for one call, not a deadline. It replaces `bash_foreground_bound_secs` for that command; when it elapses the command is relocated exactly as it would be at the default bound, or simply waited on further where the handoff is unavailable. Either way it never terminates the command: with background execution on, a caller cannot ask for a kill through the tool's arguments at all — the ways to stop a running command are `background_kill` and interrupting the turn. With background execution off there is nothing to relocate into, so it is the caller's own kill limit for that command instead (see below).

**Execution policy.** Each run resolves one policy, background execution on or off, from an explicit permission. The tool list, the `Bash` and `invoke_subagent` schemas and descriptions, and runtime enforcement all derive from it; no surface or tool restates it.

- **On** (the TUI, the bot, plain `zdx exec`, jobs without a subagent, orchestrator workers, and a subagent whose definition sets `background_commands: true`): the three intents above apply, `invoke_subagent` accepts `background: true`, and a slow normal `invoke_subagent` call is handed off at the bound (see Background subagents below). A run that has `bash` or `invoke_subagent` also always has `background_output` and `background_kill`, even when its tool selection omits them, so it can read and stop every job it starts.
- **Off** (every other subagent, a job whose subagent does not opt in, and `zdx exec --no-background-commands`): no background execution of any kind.
  - The `Bash` schema has no `background` property, and its description says there is no implicit time limit. `invoke_subagent` has no `background` property, and a normal call blocks until the subagent finishes (no handoff). `background_output` and `background_kill` are removed even when selected.
  - `background: true` fails with `background_disabled` before anything runs, in both tools.
  - The auto-background handoff never applies, so nothing is adopted and `zdx exec` has nothing to drain.
  - A `Bash` command runs in the foreground until it finishes. `bash_foreground_bound_secs` does not apply, and there is no implicit time limit. It is stopped, with its process group, only by: the caller's `timeout_secs` (`0` or omitted means none, and it is not capped); the run's deadline (a job's `timeout_secs`), whichever is earlier; or the turn being interrupted, cancelled, or ended (for a subagent, by its parent's `background_kill` or turn end).
  - A kill at a limit returns a success envelope with `timed_out: true`, exit code -1, and the output captured so far. Its message names the limit that stopped the command (the caller's `timeout_secs` or the job's deadline) and tells the model not to rerun the command unchanged.
  - Because nothing bounds a command implicitly, a subagent that waits on a hung command is managed from outside: its caller's normal `invoke_subagent` call hands it off at the caller's bound, and the caller can then see the running command with `background_output` and stop the subagent with `background_kill`.

**Auto-background handoff (Unix).** A foreground command runs under `bash_foreground_bound_secs` (default 120; `0` disables it and restores an unbounded wait). The bound is a relocation bound, not a deadline: when it expires the command **keeps running and is moved to the background**, never killed.

The handoff applies only when no kill deadline is in force and the run is on a surface that can keep the job alive:

- A per-call `timeout_secs` tunes the bound rather than suppressing it. Omitted and `0` keep the configured bound; a positive value replaces it, up or down. When there is no handoff to relocate into — auto-backgrounding disabled, an excluded surface, or a non-Unix build — the command keeps waiting in the foreground; a wait budget never degrades into a kill.
- With background execution on, the only kill deadline is a job's `timeout_secs` frontmatter, which bounds tool calls for that job run only. There is no global tool-timeout setting and no model-facing route to one. That deadline suppresses the handoff so it still kills, and a caller's `timeout_secs` can neither raise, lower, nor disable it.
- An adopted job is held by its owning process's supervisor lease, so that process must not exit while the job runs. A surface qualifies either by outliving the run — the interactive TUI and the Telegram bot daemon — or by **draining**: `zdx exec`, which is also how orchestrator workers and any `invoke_subagent` child with background execution on run, waits for adopted jobs to finish before the process exits. Unknown surfaces are excluded and keep the unbounded foreground wait.

**Draining (`zdx exec`).** When a turn ends with adopted jobs still running, the process stays alive until they finish. It holds the lease throughout, the jobs' reader tasks keep appending to their logs, and the process exits only once every job has exited and its log is complete. Draining never kills a job and has no deadline, so a legitimately long build is waited out in full. While waiting it logs the pending job count and pids at `warn` level, so a human watching a process that has not exited can see why. Interrupting during the drain is an explicit request to stop: the jobs are torn down through their leases — TERM → grace → KILL → group sweep — so nothing is orphaned, and a second interrupt force-exits, after which the supervisor's own lease cleanup applies.

**Background subagents.** `invoke_subagent` has three modes, all chosen by the run's execution policy:

- **Blocking.** With background execution off, or on but without a handoff bound (a surface that cannot keep relocated work alive, `bash_foreground_bound_secs = 0`, a non-Unix build, or no owning agent turn), a normal call blocks until the child exits and returns its answer as `result` with its `child_thread_id` (§ Named subagents, Result and continuation).
- **Automatic handoff.** With background execution on and a handoff bound, a normal call (no `background`) waits like a blocking call for up to the bound, which is `bash_foreground_bound_secs` (default 120): one setting bounds the foreground wait of both `Bash` and `invoke_subagent`. A child that exits within the bound returns its answer exactly as a blocking call does, and leaves no job, registry record, or log behind. A child still running at the bound is **handed off, never stopped**: the same child keeps running (it is not restarted), the live relay of its tool activity to the caller stops, and the call returns a success with `backgrounded: true`, `bg_id`, `child_thread_id`, `elapsed_secs` (the measured wait), `deadline_secs`, and the same progress fields as `background_output`, plus a message telling the model not to invoke it again. The message follows the child's `turn`: a child still working is followed with `background_output`; a child whose turn already ended (its `result`, `error`, or `partial_result` is in the output) but whose process had not exited yet has nothing more to give, so the model is told not to wait for it. Progress made before the handoff stays in the job.
- **Explicit background.** `background: true` hands off at once (requires background execution on; otherwise `background_disabled`).

From the handoff on, a subagent is a background job exactly like one started with `background: true`; the rules below apply to both.

- An explicit `background: true` call returns at once with a `bg_id`, `status: "running"`, the subagent name, `child_thread_id` (the child's thread, created before the child starts and tagged as a subagent run of the caller), and `deadline_secs` (the run's deadline, or null). A normal call that may hand off creates its child thread the same way.
- During the foreground wait of a normal call, the child is already owned by the agent turn. Cancelling the call returns `canceled` at once and stops the child; a call that is abandoned (its future dropped or aborted), or that fails before the handoff, stops it too. In every such case, once the child has exited, the call's logs are deleted and so is its child thread if the child never wrote to it; a child thread with any history is kept. No registry record exists before the handoff.
- A run deadline's timer ends as soon as the child exits.
- A call that cannot start creates nothing: without an owning turn it fails with `no_owner` before the child thread is created, and when a later step fails (logs, spawn, registration) the new child thread is deleted again as long as it still holds only its meta line.
- The child is the same `zdx exec` run as a blocking call, with the same prompt, model, tools, and background policy (its own commands run with background execution off unless its definition opts in). It is registered as an adopted job of kind `subagent` and supervised by the same lease as a command.
- `background_output` on it reports `kind: "subagent"`, `status` (`running` until the child process exits, then `exited`), and `turn`:
  - `running`: the child is still working. `current_tool` and `current_tool_input` name its running tool call; `tool_calls` and `seconds_since_activity` show progress.
  - `completed`: the child's turn finished; `result` holds its final answer. The answer is recorded the moment the child reports the end of its turn, not when its process exits. `status` can still be `running` after that (for example while it waits for its own background commands), so a result alone does not mean the process has exited. The answer stays readable after the child is stopped.
  - `interrupted`: the child's turn was interrupted; `partial_result` holds its partial text. It is never reported as `completed`.
  - `failed`: the child's turn failed; `error` holds the message.
  - `timed_out`: the run's deadline stopped the child before its turn ended; there is no result.
  - `stopped`: the child exited without finishing its turn (it was stopped or crashed); there is no result, the tail of its stderr is included, and its partial transcript stays in `child_thread_id`.
  - `wait_secs` and `wait_for` work as for commands, with one difference: a wait on a subagent, including `wait_for: "exit"`, also ends as soon as its turn ends, with `waited_for: "result"`, even while its process is still running. So `wait_for: "exit"` waits for the result, not necessarily for the process to exit. A child's tool events count as output for an output wait.
- **Run deadline.** When the run has a tool deadline (a job's `timeout_secs`), a managed subagent is stopped at that deadline, measured from when it started, as a blocking call is. A deadline earlier than the handoff bound stops it during the foreground wait, and the call fails like a blocking one (`Subagent timed out after N seconds`); a later deadline still stops it after the handoff. The handoff never stops a subagent and never extends a deadline. The view reports `timed_out` only when the deadline itself stopped it: if `background_kill` or the owner's cleanup had already started stopping it, or it had already ended, the deadline does nothing and the view keeps the real cause. A turn that had already ended keeps its result. No deadline applies otherwise.
- `background_kill` stops the child and, through its leases, every command and subagent it is running.
- **Owner lifetime.** Background means non-blocking, not detached. Every managed subagent (explicit background, or a normal call that may hand off) belongs to the agent turn that started it from the moment it starts. When that turn ends — finished, failed, or cancelled — the engine stops each of its managed subagents that is still running, without the model having to collect or kill it, and before `zdx exec` drains its Bash jobs. A worker turn therefore cannot leave a subagent behind. If the owning process dies instead, the lease stops the child. Bash jobs are not affected by this rule and keep their own lifetimes.
- A result stays readable while its record is kept: until the turn ends, and at most 5 minutes after the child exits. The child's thread keeps the full transcript.
- Unix only. Descendants that leave their process group (for example with `setsid`) are outside this containment.

**Limitation: a parent cannot reach a child's job.** `background_output` and `background_kill` resolve a `bg_id` only within the thread that created it. A job adopted inside an orchestrator worker, or inside a subagent that opted into background execution, is therefore invisible to the parent: the child can poll and stop it with its own tools and waits it out at exit, but the parent cannot poll or stop it in the meantime. Subagents run with background execution off by default, so by default they create no such jobs. Changing that requires a cross-thread ownership model for background jobs and is deliberately out of scope.

Behavior at the boundary:

- Completion is resolved before the bound, so a command that finishes at the bound is reported as completed rather than relocated.
- That race is settled on observation, not wall-clock truth: a command finishing microseconds before the bound may still be reported as `backgrounded` with `exit_code: null`. It is not lost — the job is registered, its exit is recorded, and the real exit code and full output surface on the first `background_output` poll, which will show `status: "exited"`.

- The handoff retains the invocation's existing supervisor, session, process group, and lease — nothing is re-parented, re-spawned, or signalled — so a long build or test run completes unharmed. Only the owner of the handle moves.
- The tool call returns a **success** carrying `backgrounded: true`, a `bg_id`, `pid`, `pgid`, `status: "running"`, `exit_code: null`, `elapsed_secs`, the output captured before the handoff, `stdout_log`/`stderr_log`, and a message instructing the model not to re-run the command.
- Output capture continues into the background logs. Bytes captured before the handoff are flushed into the log first, so the log holds the complete stream and the partial output appears in both places. Streaming deltas stop at the handoff, so no output event can arrive after the tool result.
- The job is registered with `mode: "adopted"` and is visible to `background_output` and `background_kill` like any other background job. Killing it closes its lease, which runs the supervisor's TERM → grace → KILL → group-sweep path; the lease is authoritative about identity, so no PID-reuse guard applies.
- Adopted jobs are **session-scoped**: they belong to the zdx process that adopted them, unlike `background: true`, which detaches at spawn and outlives zdx. A long-lived surface simply keeps them; a one-shot run drains them before exiting. Either way the process never exits out from under a running job. If the owning process dies anyway (crash, `SIGKILL`, second interrupt), the lease closes and the supervisor terminates the job, and the registry's liveness scan reaps the record to a tombstone.
- Degraded bookkeeping never kills the command. If the registry marker cannot be written, or the output logs cannot be opened, the job keeps running and the result reports `tracking_failed: true` (plus `logging_failed: true` when output is no longer being captured) alongside its `pid`.
- `background_output` stamps every read with a strictly increasing `read_seq`, so polling an unchanged job never looks like a repeated identical tool call to the turn's loop detector. A read that waited is still a read and still advances it.
- `background_output` takes an optional `wait_secs`, which blocks until the job appends output, exits, or the budget elapses — whichever comes first — and adds `waited_for: "output" | "exit" | "timeout"` to the usual envelope (plus `"result"` for a background subagent whose turn ended; see Background subagents). It applies to both adopted and detached jobs, since both record their exit in the registry marker. `wait_secs` is clamped to `bash_foreground_bound_secs` (120s by default, and to 120s when that is `0`), so a wait can never recreate the blocked-turn problem the bound exists to prevent. With `wait_for: "exit"`, new output does not end the wait: on a command it returns only when the command exits or the budget elapses; on a background subagent it returns as soon as the subagent's result is ready (`waited_for: "result"`, which can happen while its process is still running), when it exits, or when the budget elapses. The budget is clamped to a fixed 1800s instead of the foreground bound, so a job the model chose to wait out does not cost a model call per bound. An unknown `wait_for` value is rejected as `invalid_input`; without a positive `wait_secs`, `wait_for` has no effect. Waiting is purely an observer: it never signals the job, and a wait that times out or is cancelled leaves it running untouched. Cancelling the turn ends the wait promptly.
- Non-Unix builds have no supervisor or lease; auto-backgrounding does not apply there, so a per-call `timeout_secs` has nothing to bound and is inert. Only a job's deadline still kills.

### `telegram`

- Posts into a Telegram chat or forum topic other than the current conversation. One tool with an `action` enum: `send_message`, `send_document`, `create_topic`.
- Requires `chat_id`; `message_thread_id` selects a forum topic and is omitted for the group's General topic. `create_topic` returns the new topic id.
- `send_message` and `send_document` return the receipt from Telegram's answer: `chat_id`, `message_id`, `message_thread_id` (set only when Telegram marks the message as a forum-topic message), and `url`, the message link (`https://t.me/c/<internal_chat_id>/<topic_id>/<message_id>` in a topic, `https://t.me/c/<internal_chat_id>/<message_id>` otherwise; `null` for private chats and basic groups, which have no such link). An accepted answer without a `message_id` is reported as possibly delivered, never as a made-up id. Each call sends exactly one message, so there is one receipt.
- `send_message` defaults to `parse_mode: html` and sends text raw — the Markdown conversion on the normal reply path does not apply. Body is capped at 4096 characters and captions at 1024; both are rejected locally with a clear message rather than by the Telegram API.
- `send_document` resolves paths like other file tools. A file over Telegram's 50 MB bot upload limit is refused locally as not sent, from its size before it is read (and again on the bytes read, in case it grew). An optional `bot_token` overrides the configured bot; otherwise the token resolves from `telegram.bot_token`, then `ZDX_TELEGRAM_BOT_TOKEN`, then `TELEGRAM_BOT_TOKEN`.
- A Telegram flood refusal (HTTP 429 with `retry_after`) is retried after the full wait, at most three attempts within two minutes. A timeout or unreadable answer is reported as possibly delivered and is never resent automatically. This client has no pacing gate: it runs outside the bot process and cannot see the bot's per-chat state.
- Needs no shell, so the orchestrator can post to other topics; it previously could not.
- Shares its implementation with the `zdx telegram` CLI subcommands. `zdx telegram send-message` and `send-document` print `Sent message to Telegram (message_id <id>): <url>` (or `(message_id <id>).` with no link), or with `--json` one line `{"chat_id":…,"message_thread_id":…,"message_id":…,"url":…}`. `create-topic` prints the new topic id. `--parse-mode markdown` sends Markdown, not HTML.
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

- One-shot understanding of a local image, PDF, audio, or video file: takes a `file` path and a `prompt`, sends the file inline to a model, and returns the model's text answer. The default model is `gemini:gemini-3.5-flash-lite`.
- Read-only: it reads one file and returns text. It never modifies the file and needs no shell, so agents without `bash` (including the orchestrator profile) can use it.
- Stateless — each call re-sends the file; a follow-up question is another call.
- Optional `model` override is an exact `provider:model` id. Any Gemini model takes every supported media type. For audio files only, it can also be another provider's model when both hold:
  - the provider has audio settings. A built-in provider has them when it is listed in the embedded `audio_providers.toml` (today Xiaomi, Alibaba, StepFun, Meta, Muse Code, Mistral); its credential is the provider's usual API key as a Bearer token, or Muse Code's minted key. A custom provider (`[providers.custom.<name>]`, protocol `chat-completions`) has them when it has an `audio` block; it uses its `base_url` and `api_key`/`api_key_env`. A built-in provider not in `audio_providers.toml` is refused even with an `audio` block in config, because built-in providers use their own auth schemes; add another OpenAI-compatible endpoint as a custom provider instead. A different request protocol or auth scheme needs code.
  - the model has `input_audio = true` on a provider-qualified `[[override]]` entry in `model_overrides.toml` (see §Models registry). Shipped defaults: Xiaomi `mimo-v2.6-flash`/`-pro`/`-pro-ultraspeed`, Alibaba `qwen3.8-omni-flash`, StepFun `stepaudio-2.5-chat`, Meta and Muse Code `muse-wonder-1.0` and `muse-spark-1.2` (plus `muse-wonder-full-1.0` on Muse Code), Mistral `voxtral-small-latest`.
  Audio support is never inferred from a model name, OpenAI compatibility, or upstream modality data. Any other provider or model, a declaration for a provider without audio settings, and any non-audio file sent to one of those models fail with an error that names the declared models and how to declare one.
- Audio in a format the model does not take is converted with `ffmpeg` to mono 16-bit PCM WAV at the decoder's sample rate (48 kHz for Telegram's OGG/Opus voice notes, which every shipped provider except Xiaomi gets converted). Only the container and codec change; stereo is mixed to mono. How a provider resamples the audio after it arrives is not documented. A missing `ffmpeg` fails clearly. Gemini always gets the original file.
- Inline size cap is 15 MiB, for the file and for converted audio (about 2.5 minutes of 48 kHz PCM); a provider's own base64 limit (Alibaba 10 MB, about 75 s of PCM) is checked before sending.
- Provider audio settings: `[providers.<name>.audio]` (built-in) or `[providers.custom.<name>.audio]` in `config.toml` (global or workspace layer, merged like other config):
  - `payload`: the `input_audio` shape. `data-uri` sends `{"data": "data:<mime>;base64,…"}`; `data-uri-with-format` adds `"format"`; `base64-with-format` sends `{"data": "<base64>", "format": "<fmt>"}`; `base64` sends the bare base64 string.
  - `formats`: formats sent without conversion; must include `wav`. Other audio is converted to WAV.
  - `max_base64_bytes` (optional, > 0): the provider's limit on the encoded audio, checked before sending.
  - `max_tokens` (optional, > 0): sent as `max_tokens`.
  - `text_modalities` (default `false`): sends `modalities = ["text"]`.
  - Built-in defaults live only in `crates/zdx-assets/audio_providers.toml`, embedded in the binary; they are not copied into `default_config.toml` or the user's config. Each field resolves as: the config value if any layer sets it, else that shipped default. An explicit `false` overrides a shipped `true`. A custom provider has no defaults: `payload` and `formats` are required.
  - Values are checked before the request; an unknown payload or format, a list without `wav`, a zero limit, missing settings, a non-chat-completions custom provider, or a missing credential fails that call with the section or provider name.
  - `zdx ask-media` reads config on each run. The bot reloads changed config files before a turn, so a change applies to its next turn; other long-running processes use the config they loaded at start. A change to `audio_providers.toml` needs a release.
  - Example, built-in field override (StepFun without `modalities`, and a smaller output budget):
    ```toml
    [providers.stepfun.audio]
    text_modalities = false
    max_tokens = 2000
    ```
  - Example, a new OpenAI-compatible audio provider and model, with no code change (`config.toml`, then `model_overrides.toml`):
    ```toml
    [providers.custom.acme-audio]
    base_url = "https://llm.example.com/v1"
    api_key_env = "ACME_AUDIO_API_KEY"
    models = []                      # chat models for the picker; not needed for audio

    [providers.custom.acme-audio.audio]
    payload = "base64-with-format"
    formats = ["wav", "mp3"]
    max_tokens = 4000
    ```
    ```toml
    [[override]]
    id = "acme-audio:listen-1"
    input_audio = true
    ```
- Paths resolve like other file tools (relative to the working root, with `$VAR`/`~` expansion).
- Shares its implementation with the `zdx ask-media` CLI.
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
- **Thinking source of truth:** config, model modes, subagent definitions/overrides, jobs, CLI overrides, and agent-tool inputs carry thinking only in the model spec. Removed standalone config keys are rejected rather than ignored. A suffixless persisted config model means `off`; a suffixless runtime override inherits the currently effective level and is canonicalized with that level before it crosses a process boundary or is newly persisted.
- **Historical threads:** the old transcript `thinking_override` field remains readable. When present it is applied after that thread's model override to preserve historical behavior. Any new model-override write stores one unified model spec and atomically clears the legacy field.
- **TUI selection is not a config write:** `/model`, the standalone `/thinking` (Ctrl+T), and Tab/Shift+Tab model-mode cycling change the active tab's effective selection and, when the thread already exists on disk, that thread's model override. They never touch config, so the selection follows the thread and `/new` starts from the configured default again. `/thinking` keeps the current model and changes only the level; it is a no-op with a notice on models that cannot reason. `/model-save` is the only TUI path that writes the workspace config; it commits the current spec, reports the actual write result, and adopts the saved value as the tab's default.
- **Selecting a model for the thread a command will open (TUI):** while the `/handoff` composer is open, `/model` (Ctrl+L), `/thinking` (Ctrl+T), and model-mode cycling apply to the thread the handoff will create, not the current one. The pick is held in the composer (shown in its title, and the level keys operate on it), the source tab and thread keep their model and override, and cancelling the handoff at any stage drops it. On submit the pick is pinned on the new thread and used for its system prompt; without one, the new thread inherits the source thread's model as before. `/btw` needs nothing extra: it opens its own tab up front, inheriting the source thread's model, and a pick there is already local to it — pinned on the side thread when the first message creates it.
- **`@fast`:** selects the priority service tier (`service_tier: "priority"`, premium per-token rate) and is offered only for providers that accept it (OpenAI, OpenAI Codex). A spec without `@fast` sends no service tier. Acceptance is not a grant — the `ChatGPT` Codex backend takes the field and may still serve `service_tier: "default"` — so whenever the served tier differs from the requested one, the downgrade is logged. `@fast` is selected from the model picker (or typed into any model field: config `model`, model modes, thread overrides, subagents, jobs) and stays visible wherever the model name is shown.

### Model modes

- **Definition:** `[[model_modes]]` entries carry `name`, `description`, `primary`, and `alternatives`. `primary` is a model spec whose `@<level>` suffix is the mode's thinking level; a `thinking` key inside a mode is rejected. `alternatives` are equivalent picks at the same tier, surfaced for deliberate choice and never resolved automatically.
- **References:** any subagent model field — `[subagents.overrides.<name>].model`, subagent frontmatter `model:`, and the `invoke_subagent` `model` argument — may hold `mode:<name>` instead of a spec. Lookup is trim + case-insensitive. A reference naming an unconfigured mode is logged and falls through to the next layer instead of failing the delegation; the resolved model is still validated against the available subagent models.
- **Surfaces:** modes are cycled with Tab/Shift+Tab in the TUI (primaries only), listed by the Telegram launcher, editable in the monitor Config tab (primary only; `description`/`alternatives` stay TOML-only), and rendered as a `# Model Modes` catalog in the reserved orchestrator profile only, so it can pick a tier per worker. Ordinary runs and worker subagents carry no such section; they resolve `mode:<name>` through their subagent override instead.
- **Removed:** `[[favorites]]` no longer exists. A config that still defines it fails to load with an error pointing at `[[model_modes]]`.

### Provider-level config

- Each provider may expose `base_url` and `tools` overrides under `[providers.<id>]` in config.
- Provider implementations live in `zdx-providers`; the models registry (`models.toml`) tracks available models per provider.
- **Custom providers:** `[providers.custom.<name>]` defines a user endpoint (base URL, API key, model allow-list) reached as `<name>:<model>`. An optional `audio` block lets `ask_media` send audio to it (see `ask_media`). The wire protocol is `chat-completions` (default; OpenAI-compatible, `<base_url>/chat/completions`) or `anthropic` (Anthropic Messages via the same client as the first-party provider, `<base_url>/v1/messages`, `x-api-key` auth); the registry spellings `openai-completions`/`anthropic-messages` are accepted aliases. Resolution: an explicit `api` on a matching `[[override]]` in `model_overrides.toml` wins, else the provider's `api`; a model entry's implicit metadata default never counts as a selection, and an unsupported override value fails the turn with a clear error. Both protocols accept the same `base_url` convention (`https://llm.example.com/v1`): in `anthropic` mode one trailing `/v1` is dropped before the client appends `/v1/messages`, so switching protocols never produces `/v1/v1/messages`. Thinking levels: in `chat-completions` mode the level is forwarded as a top-level `reasoning_effort` string carrying the level name (`low`/`medium`/`high`/`xhigh`/`max`), and `off` sends `none` because proxied reasoning backends think by default when the field is omitted. Custom endpoints are proxies that pass the value to their backend, so `@max` reaches backends that distinguish it (e.g. a vLLM-hosted DeepSeek V4). In `anthropic` mode non-off levels map exactly as for the first-party Anthropic provider (adaptive thinking + `output_config.effort` for non-legacy model ids), and `off` sends an explicit `thinking: {"type": "disabled"}`; first-party Anthropic clients keep omitting the field.

### Anthropic adaptive thinking

- Adaptive thinking (`thinking.type: "adaptive"`) is used on Claude Opus 4.7, Opus 4.6, and Sonnet 4.6.
- We always send `thinking.display: "summarized"` so visible thinking text is preserved. This is required on Opus 4.7 (where the API default silently became `"omitted"`) and is a no-op on older Claude 4 models where `"summarized"` is already the default.

### Z.AI GLM-5.3 forced thinking

- The GLM-5.3 family (`glm-5.3`, `glm-5.3-flash`) always reasons: it rejects `thinking: {"type": "disabled"}` with HTTP 400 (code 1210), and its `reasoning_effort` vocabulary is `low`/`high`/`max` only (`medium`/`minimal`/`xhigh` are rejected too). Those models therefore receive no `thinking` field (there is nothing to toggle) and carry the level as a top-level `reasoning_effort` (`medium`/`high`/`xhigh` collapse to `high`, `max` stays `max`, `off` omits it). Verified against `api.z.ai` 2026-09-15.
- Other GLM models keep the documented toggle: `thinking: {"type": "enabled"}` when the model reasons, `{"type": "disabled"}` when it does not.

---

## 11) Environment Variables (Runtime Context)

ZDX exposes runtime context to agent processes via `ZDX_*` environment variables.
These are the canonical source of truth for paths and session context — skills, jobs, and bash commands reference these env vars directly.

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
- **Workspaces.** A workspace is a directory whose own overlay is `<root>/.zdx/config.toml`. The workspace list is every Telegram profile `cwd` (used as-is; profiles sharing a `cwd` are one workspace) plus the caller's current root (resolved to its nearest project root, else itself), current first. Telegram profiles are bindings to workspaces, not a separate config concept: they carry no model settings.
- The shared workspace model-settings view (`zdx_engine::workspace_config`) covers `model`, the helper models, `transcription.model`, `speech.model`, `model_modes`, `subagents.enabled`, and every `subagents.overrides.<name>.model` (orchestrator first). Each setting reports its effective value, the layer that supplied it (`default`, `global`, `workspace` = this workspace's own overlay, `ancestor` = a parent directory's overlay), and the inherited value and layer it would fall back to without this workspace's overlay. Missing values stay explicitly unset. Each workspace also reports `overlay_is_global` (its overlay is the global config file). Exposed read-only by `zdx config workspaces [--json]` and the Mini App `GET /api/workspaces` (Monitor → Workspaces section).
- Monitor Config views one scope at a time and loads only that scope's layers, never the process CWD: a workspace (default: the monitor root) through all its layers, or the global config on its own (`global · all workspaces`, the first `p` entry; `a` toggles between the viewed workspace and global on the same row). Rows set in the viewed workspace's own overlay carry a highlighted `[<name>]` tag; rows inherited from an ancestor overlay carry a dim `[<ancestor>]` tag; global/default rows are untagged. Equal-value overrides still belong to the overriding layer. A strip under the title names the destination file; the second line shows the selected row's origin, what a reset of the destination falls back to (the built-in default for global), and which listed workspaces an edit reaches or skips (`workspace_config::reach`: a workspace reads the file unless a layer above it sets the key, so a parent's edit reaches nested workspaces).
- Monitor Config edits always write the viewed scope's own file: the workspace overlay (created on first write) or `$ZDX_HOME/config.toml`. There is no separate write target. Every write and reset of the global config file waits for `y` (Enter does nothing; Esc cancels; other keys and the mouse do nothing), with the file, old and new value, and reach shown; `y` writes the file captured when the write was requested. A workspace whose overlay is the global file (`overlay_is_global`, e.g. home with `$ZDX_HOME` at `~/.zdx`; detected through symlinks, also before the file exists) is labelled as the global config and asks the same way. Picker titles name the destination (`Set <field> in <workspace|GLOBAL>`). `d` resets the selected setting in the destination (on a mode row it deletes that mode instead; `D` resets the whole mode list); a key the destination does not set is reported, not written. Mode edits/deletes and the subagents toggle start from the viewed chain, which is the destination's chain, so a workspace's modes never reach global; a workspace's first mode edit copies its inherited list into its overlay, and the picker says so; deleting an inherited mode does the same with the remaining modes, so it also waits for `y` with that warning. The selected-row lines wrap to the terminal width, and the footer names what `a` does from the current view. When `p` re-discovers and the viewed workspace is gone, the view switches to the first workspace at once, even if the picker is cancelled.
- Other writes (`zdx config`, mode/Telegram saves) target `$ZDX_HOME/config.toml`.
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
- A turn that loops instead of finishing is stopped rather than left to burn its budget. Three shapes are detected: reasoning past 160k characters with no text or tool call yet, reasoning or answer text that degenerates into the same few short lines, and three identical tool calls (same tool, same input, same result) with no progress between them. A tool round that produces any tool/input/result combination not seen earlier in the turn counts as progress and restarts the count, so an edit-then-verify cycle whose check prints the same output each time is not a loop; replaying only earlier combinations (the same call, or calls that alternate) still is. Before giving up, the engine retries the turn once at the next thinking level down, with a fresh count, and says so in the notice; if the retry loops too, or there is no lower level, the turn closes normally with a `notice` and the repeated content is truncated head-and-tail. A detected loop never fails the turn. The answer-text check is deliberately stricter than the reasoning one (a full 4k window of short lines, none repeating fewer than ten times) so tables, log dumps, and generated code pass.
- Provider base URLs and tool overrides live under `[providers.<id>]`.

### Prompt templating

- Template syntax: MiniJinja (`{{ var }}`, `{% if %}`, `{% for %}`).
- `[prompt_template].file` — optional template path (relative paths resolve from `ZDX_HOME`).
- The built-in fallback/default prompt is `prompts/system_prompt_template.md`. On custom template load/render failure, ZDX warns and falls back to that built-in template.
- Providers consume the caller-composed prompt; they do not prepend hidden coding system prompts.

### Prompt layers

- Prompt layers are additive MiniJinja-rendered prompt fragments appended after the base system prompt.
- The same mechanism is used for surface-specific constraints (for example Telegram or exec output guidance) and harness-style behavior layers (for example job/headless execution instructions).
- Prompt layers modify behavior without creating a separate subagent identity.

### Named subagents

- Named subagents are markdown files with YAML frontmatter plus a standalone prompt body.
- Discovery order/override precedence: built-in → `~/.zdx/subagents/` → project `.zdx/subagents/` (later sources override earlier by name).
- `invoke_subagent` requires `subagent: <name>`: delegation always targets a named subagent, and there is no unnamed/default delegated agent. Omitting the argument is an error.
- `invoke_subagent` accepts an optional per-invocation `model = "provider:model[@thinking][@fast]"` override. Model layers resolve parent/default configuration → named subagent definition → `[subagents.overrides.<name>]` → explicit invocation. A suffixless runtime layer inherits the preceding level; an explicit suffix replaces it. Profile prompt, tools, and context behavior remain unchanged.
- Subagent definition frontmatter and `[subagents.overrides.<name>]` each use only `model` for model and thinking selection; standalone `thinking_level` is rejected.
- **Result and continuation.** A child run that finishes with an answer returns `{ "result": <final answer>, "child_thread_id": <id> }`; the background handle and the handoff output carry the same `child_thread_id`. A failure carries the id only as described below, and a cancelled call returns `canceled` with no id. Passing that id back as `thread_id` continues the same child instead of starting a new one: the child runs `zdx exec` against that thread, so it loads the thread's full history and keeps its `ZDX_THREAD_ID` and artifact directory. The prompt then carries only the new request. Every other rule is unchanged per call: the profile prompt, tools, background policy, and model layers are resolved again, so a continued child uses the current definition and override (an invocation `model` applies to that call only).
  - Before anything runs, the thread must exist (`thread_not_found`), be a thread an `invoke_subagent` call created (`origin_kind: subagent`) for the same subagent name and in the same working directory (`thread_mismatch`), and the id may contain only ASCII letters, digits, `-`, and `_` (`invalid_input`). The caller need not be the thread that started it.
  - One thread runs at most one child at a time. Every child run, new or continued, holds an exclusive advisory lock on its thread file until the child process exits, in every mode: after a handoff, and when a call is cancelled or dropped while its lease is still stopping the child. A continuation of a thread that is locked, or that has an active-run marker from another surface (a TUI session, a manual `zdx exec --thread`), fails with `thread_busy` and runs nothing.
  - A failed call names its thread in the error details (`child_thread_id: <id>`) only while that thread exists. A thread created for the call that the child never wrote to is deleted (§9), so no id is reported for it. A continued thread is never deleted. Cancellation returns `canceled` without an id.
- Delegation runs each child with the tools and rules its definition declares. The built-in `explorer` and `oracle` are read-only, and so is any subagent whose definition gives it no tool that can change state. A subagent may declare write-capable tools (the built-in `html-artifact-specialist` writes its HTML artifact); its description says what it writes. Long-running mutating work still belongs in a worker thread (§18), which exists only where a live worker manager runs.
- Delegated child runs should be prompted self-sufficiently: the parent should include the goal, relevant context, constraints/non-goals, expected output, and verification when relevant rather than assuming the child inherits its implicit reasoning state.
- When a named subagent is selected, its body is rendered with the same prompt-template syntax/vars as the main prompt pipeline, then used as the child run's system prompt directly; it does not inherit the default ZDX prompt/context pipeline unless that text is written into the subagent body.
- Named subagents may declare `skills:` (allowed on-demand skills) and `auto_loaded_skills:` (skills whose `SKILL.md` contents are injected directly into the subagent prompt). Auto-loaded skills should be treated as already in context for that run.
- A subagent definition may declare `allowed_subagents`, restricting which subagents it can reach via `invoke_subagent`. The restriction is enforced twice: the tool schema advertises only the listed subagents, and execute-time resolution rejects any unlisted name. `allowed_subagents` may not list the declaring agent itself or the reserved `orchestrator` profile. Omitting the field leaves the caller unrestricted.
- A subagent definition may declare `background_commands: true` to run with background execution on (§9, Background execution, execution policy). Omitting it, or `false`, runs the subagent with it off: an `invoke_subagent` child gets `--no-background-commands` from its parent, and `zdx exec --subagent` and a job's `subagent:` apply the definition directly. The built-in `explorer` and `oracle` do not opt in. This setting is about the subagent's own commands; whether a caller may start it with `invoke_subagent` `background: true` follows the caller's policy.
- Explicit subagent skill dependencies are resolved from enabled sources even if global `include_skills` / `ignored_skills` filters would otherwise hide them.
- Built-in subagents currently include:
  - `explorer`: a read-only local exploration specialist for open-ended multi-step discovery across the current workspace, broader machine-local filesystem paths, and saved thread history.
  - `oracle`: a read-only deep reasoning advisor for code review, difficult debugging, planning, and architecture decisions. Its output is advisory and should be independently validated by the parent agent.
  - `html-artifact-specialist`: builds one HTML artifact (explainer, comparison, review, or working mockup) from the sources it is given. It is not read-only: it writes the page under its artifact directory, or edits the page it is given, and returns the absolute path. It auto-loads the bundled `html-artifacts` and `frontend-design` skills; its model comes from `[subagents.overrides.html-artifact-specialist]` or the caller. A revision continues the same child with `thread_id`.

### Models registry

- Path: `<base>/models.toml` (falls back to `default_models.toml` when missing).
- Persistent user metadata overrides live at `<base>/model_overrides.toml`. `[[override]]` entries use a provider-qualified `id` and may override display name, pricing, context/output limits, reasoning, image input, and API routing metadata. An override may also define metadata for a custom-provider model absent from `models.toml`; it survives `zdx models update` because the generated registry and user overrides are separate files. `models.toml` is generated: edits to it are lost on the next update, so durable changes go in `model_overrides.toml`.
- Audio input for `ask_media` is declared only in override files, never in `models.toml`:
  - `input_audio = true|false` enables or disables the model for audio through its provider's audio settings. `audio_formats = ["wav", "mp3", …]` (optional; known: wav, mp3, ogg, flac, m4a, aac; must include `wav`, the conversion target) replaces the provider's `formats` for that model.
  - Layers: the embedded `model_overrides.toml` shipped in the binary, then `<base>/model_overrides.toml`. For each field the last matching entry wins, so a user entry can change one field and keep the rest, or set `input_audio = false` to turn off a shipped default. Only provider-qualified ids count (aliases and `provider@account:` accepted); a bare id never enables audio.
  - The user file is read on each `ask_media` call: a change applies to the next call with no rebuild or restart. A new shipped default needs a release.
  - `zdx models update` ignores these fields and never writes them, so regeneration cannot drop them. An entry that only declares audio does not add a model to the picker or `zdx models list`, and does not hide a custom-provider model.
  - Example: to use a new StepFun audio model that StepFun documents for audio input:
    ```toml
    [[override]]
    id = "stepfun:<model-id>"
    input_audio = true
    # audio_formats = ["wav"]   # only if the model takes fewer formats than the provider's `formats`
    ```
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
- The shared prompt has no separate memory-save suggestion block. The TUI and Telegram instruction layers put memory-save offers in their `<followups>` suggested replies; other surfaces make no proactive offer.
- Explicit `remember X` means an immediate save on every surface.
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
  - `.ogg`/`.oga`/`.opus` go via `sendVoice`, `.mp3`/`.m4a`/`.wav`/`.aac`/`.flac` via `sendAudio`.
  - Other files (including `.pdf`) are sent via Telegram `sendDocument`.
  - HTML (`.html`, `.htm`) with an `Open artifact` link (Mini App configured, link fits; see File actions) is not uploaded: it goes out as a file card instead, whatever its size. Without that link it is sent via `sendDocument` like other files.
- When multiple valid media paths are present, the bot attempts to send each one in order, after the reply text.
- Reply text longer than one Telegram message (4096 characters) is split into several messages, sent in order before the files: each part ends after a line break, else a space, else between characters, never inside a tag or entity, so the parts read in order give the exact text; formatting open at a break is closed and reopened in the next part. If formatting cannot fit in a part at all (a tag longer than a message, or nesting too deep to reopen), the rest is sent without its tags: the words stay, that formatting is lost. A part with no visible text is not sent (Telegram refuses empty messages). Parts are measured with tags and entities counted, so each also fits if Telegram refuses its HTML and the plain-text fallback sends it raw. The follow-up buttons ride on the last part. Each part is its own `delivery` item (`reply text part 2 of 3`), so a refused part is reported on its own; the parts and files after it are still sent, except after an unabsorbed flood refusal.
- Every file is its own message. Documents are never merged into one message (when several documents rode in one rich message, every one of them was observed to open the same file in the Telegram client) and never take the reply text as their caption: the text goes first, with the follow-up buttons, then each document. A reply with no text sends no filler message; its files go out alone.
- A reply with text and exactly one valid photo, voice note, or audio file whose full rendered caption (reply text plus appended links) fits Telegram's 1024-character caption limit is sent as one message: the file with the text as its HTML caption, carrying both its `Open artifact` button and the follow-up buttons. If Telegram refuses that file for a reason other than flood control, the text is still sent on its own. Otherwise text and files are separate messages.
- File actions: when the Mini App is configured for the chat (`[telegram.server]` enabled with `mini_app_url`), each file message carries its own `↗ Open artifact` URL button: `{mini_app_url}?startapp=a_<key>_<thread>`. `key` is the file's `key` in the shared artifact projection (`zdx_engine::core::artifacts::artifact_key`: the first 16 hex characters of the SHA-256 of its normalized absolute path), and `thread` is the session id of the receiving chat or topic (before any `alias_to` hop), so feedback sent from the preview returns to that topic, and a worker's file sent by its orchestrator is found in the orchestrator topic's list. The value uses Telegram's start-parameter alphabet (`A-Z a-z 0-9 _ -`) and is kept to at most 64 characters, a conservative local limit (Telegram states 64 only for bot `start` parameters and no length for `startapp`); a file whose link would not fit, or whose listed path is not the file sent (a `..` stepping out of a symlinked directory; `artifacts::lexical_path_is_physical`), gets no button. The bot and the projection read `<media>` values with the same rule (`artifacts::clean_media_path`). The key does not embed the raw path; it is an opaque lookup identifier, not an authorization. The Mini App reads `startapp` from initData (`start_param`) or, when that is absent, from the `tgWebAppStartParam` query parameter, opens the thread's Artifacts tab, looks the key up in that thread's artifact list with its normal authenticated requests, and opens the same preview (and annotation, when the file type supports it) as a tap on that row. A key that is not in the list (the file is no longer part of the thread, or is past the list's 500-file cap), or a file that no longer exists, shows a notice instead; a key is never matched to another row. A pending link is taken once; leaving the tab before the list loads drops it, and a failed list load keeps it for a reload. Buttons go out with the message they belong to; no request is sent only to add one. Without a configured Mini App, files go out with no button.
- File cards: a file card is one plain `sendMessage` that names the file (name, size when sent, `not uploaded`) and says that both buttons use the file as it is now on the Mac, with one row: `↗ Open artifact` (the link above) and `⬇ Send file` (callback `sf:<key>`, the same artifact key). Its delivery item is `link_only: true`, and `Sent` means the card arrived. Every delivery posts new messages: a new version of a file at the same path gets a new card, and earlier cards (and uploaded documents) are never edited, deleted, marked as replaced, or refreshed, so nothing is spent keeping them current. Because Open and Send file read the file at its path when used, an old card opens and sends the current bytes, not a snapshot; only uploaded documents keep the bytes sent at the time. To keep an earlier version, the agent saves the new one under a new name.
- `⬇ Send file`: the tap is resolved from the card alone (its chat and topic name the receiving thread; the key is looked up in that thread's artifact list through the same alias and path checks as the Mini App's download route), so it needs no stored state and works after a restart. The bot answers the tap first (`Sending the file…`), then uploads the current bytes via `sendDocument` as a reply to the card, in the card's topic, with no buttons; the card is left as it is. A second tap while that upload runs answers `Already sending this file`. A successful upload is recorded as a `Delivery` event on the thread (one file item with the document's message id). A key that is not in the list, a file that is gone, or a refused upload posts a short notice as a reply to the card instead (an ambiguous failure says the file may have arrived); failures are not recorded as deliveries. Stale taps: a callback update carries no time, so the bot cannot measure tap age; when Telegram refuses the answer as too old (`query is too old`, observed about 10–15 s after the tap, not documented), the tap waited (for example while the Mac slept), nothing is uploaded, and one notice per card within 10 minutes asks the user to tap Send file again. Open and Send file both need the Mac and the bot running at the moment they are used.
- Bot only uses local absolute file paths for this flow (no URL fetch in this slice).
- Preflight upload size checks:
  - photos > 10 MB are rejected before upload
  - documents > 50 MB are rejected before upload; documents, voice notes, and audio over that limit are rejected from the file size, before the file is read
- Delivery truth: every part of the reply (text, each file, including invalid paths) gets a `delivery` record (§8). After a partial delivery the bot posts one plain summary (`⚠️ Part of this reply was not delivered: …`); a failure of that summary is only logged.
- Flood control (policy, not a documented Telegram rule set): the bot gates calls per chat, keyed by its bot token and chat, shared by all forum topics of that chat.
  - Telegram documents about one message per second per chat, at most 20 messages per minute per group, and `retry_after` on HTTP 429. It does not document whether a 429 is chat-, method-, or bot-wide, per-topic budgets, or how edits, typing, deletes, and albums count. The gate therefore paces and counts new messages only.
  - After a 429, the chat is in a flood wait for `retry_after`. By default calls to that chat are skipped locally during the wait without contacting Telegram (typing, status edits, header refreshes, callback edits, notices). Turn status messages and the status delete wait it out instead, so Cancel stays available. Reply text, files, and the delivery summary wait it out and also wait for a pacing slot (≥1 s between new messages in the chat; ≤20 per rolling minute in groups), then retry explicit flood refusals at most three attempts within two minutes, never shortening Telegram's wait. Waiting is bounded by the same two minutes: a call whose admission would come later fails at once without contacting Telegram. The budget is per request and bounds the start of the last attempt, not its completion (the 35 s HTTP timeout still applies); an HTML-repair or plain-text fallback and an invalid-reply resend are new requests with their own budget. When an item fails this way, later items of the same reply are not attempted. Admission is checked and recorded at send time, so senders released together by the end of a flood wait still go out one slot apart.
  - A timeout or unreadable answer is recorded as `unknown` and never resent automatically, since the message may already be in the chat. A connection that never opened is `failed`.
  - Pacing delays only the waiting sends listed above. Other new messages (worker completion notices, command replies, headers, retry buttons) go out at once and are counted toward the chat's pacing, so the 1 s / 20 per minute policy is not a guarantee for the whole chat. Worker notices stay immediate on purpose: the worker bridge must not stall, and a notice skipped during a flood wait is lost, as a refused one was before.
  - The gate is process-local: the `telegram` tool and `zdx telegram` send with their own client and bypass it, and a bot-wide flood limit can still refuse other chats.
- Every Telegram call is logged (`Telegram call` / `Telegram call failed`) with client (`bot` or `outbound`), process id, numeric bot id, method, chat, topic, attempt, outcome, message id, `retry_after`, wait, and duration; new messages log at `info`, other calls at `debug`. Each finished reply delivery logs one `Telegram delivery` line (`Telegram delivery incomplete` at `warn`) with the record id, thread, chat, topic, and per-item status and message id, which ties it to the per-call lines. Calls carry no record id or calling thread themselves; outbound tool/CLI calls are identified only by client, process, chat, and topic. Tokens and message bodies are never logged, and transport errors are stripped of URLs.

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
  - only the user's own message counts as that input. A bot-generated prompt that reaches the topic meanwhile (a `[worker update]`) runs as its normal turn and leaves the session waiting. The same rule holds for every staged command (`/btw`, `/prompt_builder`)
  - that input completes the command with no confirmation step: the bot generates the handoff context and immediately creates a new topic whose thread records `handoff_from` (source thread), then runs the first agent turn there with the generated handoff prompt
  - the generated context is posted into the new topic as a quote under a `Handoff context` heading before that turn starts, because the handoff prompt itself is dispatched synthetically and never appears as a chat message. A `/btw` topic gets the same treatment: the user's question is posted there as a quoted `Side question` before the turn starts
  - seed format (shared with the TUI, built in `zdx-engine`): a `[Handoff from thread <id>. …]` header with the ancestor lineage when there is one, then the user's input verbatim under `User message:` (omitted when empty), then the helper's output under `Generated background from thread <id> (written by a helper model; it can be incomplete or wrong):`. Each part is labelled with where it came from; generated text is never presented as the user's words. A `/btw` seed has a `[Side question from thread <id>. …]` header and the labelled user message, and no generated part
  - the new thread continues as the **role** shown on the staged card's `Continue as:` button, next to `🎛 Model`. It defaults to the source thread's role: `Orchestrator` when the source carries the reserved `orchestrator` persistent profile, `Agent` otherwise. Tapping it switches between `Continue as: Agent` and `Continue as: Orchestrator` in place, for this one command
    - `Orchestrator` creates the new thread with the existing reserved `orchestrator` profile (the topic is named like other orchestrator home bases), so an ordinary thread can hand off into a new orchestrator. The handoff context and the source-thread lineage (`handoff_from`, readable with `Read_Thread`) are the same as for any handoff. When the profile cannot be recorded the handoff fails instead of falling back to the default coding agent
    - `Agent` creates an ordinary thread with no persistent profile, also when the source is an orchestrator
    - the role is independent of the model: `🎛 Model` and the model inheritance rules below apply unchanged to either role
  - nothing moves off the source thread: its profile is never changed, it keeps its workers, and it stays usable, so both threads can continue side by side. The successor is a fresh conversation, not a takeover. Workers are not transferred; the successor can adopt one on purpose with `Send_Thread_Message`, which re-attaches that thread to whoever messaged it
  - if generation or topic creation fails, the staging session stays open: sending another message retries, Discard / `/cancel` aborts
  - Discard (or `/cancel`) deletes the staging messages — the bot's own always, the user's best-effort (needs `can_delete_messages`) — and leaves the source thread untouched
  - `/handoff` outside a forum topic (DM or `General`) does not start staging; the bot explains it needs a topic
  - stale staging sessions expire; a message after expiry runs as a normal agent turn
- The model of a topic opened by `/handoff` or `/btw`:
  - by default it continues the **source thread's** effective model and thinking level when the source pinned one with `/model` (that pin is copied onto the new thread). When the source pinned nothing, the new topic starts on the default model for its **own** role (the `orchestrator` override applies only to an orchestrator topic), so a `/handoff` that changes the role can start on a different model than the source runs
  - the staged command's card carries a `🎛 Model` button next to Discard (only for the commands that open a topic — not `/prompt_builder`; on `/handoff` it sits next to `Continue as:`, with Discard below). It opens the provider → model → thinking picker in place, shows the model the new topic starts on without a pick (following the card's current role), and returns to the card showing the pick; `← Back` / `✖ Cancel` return without changing it
  - a pick is resolved against that starting config (so a suffixless model keeps its thinking level) and is written only to the new thread when it is created — the current thread's model is never touched, and the pick lasts for that one command. Switching `Continue as:` after a pick keeps the pick
  - the picker and the `Continue as:` choice are in-memory like the rest of staging: after a bot restart the buttons report the expired session instead of acting
- `/commands` (inside a topic or DM) posts a context-dependent command picker:
  - lists only the custom `.md` commands the TUI would show for the chat's bound project (bundled + `$ZDX_HOME/commands` + project `.zdx/commands`); agent built-ins live in the native `/` menu instead
  - custom `.md` commands are picker-only on the bot: they are not typed commands and are not registered in the native `/` menu
  - tapping a custom command dispatches its prompt content as a normal agent turn in the current topic
  - the picker is one-shot: a tap consumes it; Dismiss deletes it
  - the picker itself bypasses the queue and posts immediately even while a turn is running; a tapped command runs as a normal queued turn
- `/tldr` (typed, native menu) posts a recap of the current thread (read-only, `tldr_model`); like `/status` it bypasses the queue and does not auto-create topics from `General`
- `/threads` (with `/thread` accepted) posts a named Mini App link for the current thread when `[telegram.server]` is enabled and `mini_app_url` is configured.
- The embedded Mini App server is opt-in. It serves one unified shell at canonical route `/app`; `/threads` and `/monitor` remain compatibility aliases. The shell navigates between Monitor, Threads, and Git without a page reload. Threads lists recent top-level threads with an `All` / `Workers` filter (`GET /api/threads?kind=workers`) and a `worker` badge on worker rows; a thread view preserves event order and shows messages plus collapsed reasoning, tool calls/results, usage, notices (including undelivered reply parts, kind `delivery`), and interruptions; while a reply delivery for the thread is running in the bot process (`delivering`, from its pending record until its outcome is persisted; a pending record left by a crash never counts), the view keeps polling; while a turn runs on the thread in any process (`run`, the shared live run status), the view polls every 4 s for its whole length, not only while a tool runs, shows a status line with the label, turn age, last-event age, and heartbeat age, and lists the calls of the current round (finished ones with their outcome and duration) until the thread file holds them; the thread list, the Workers pane, and Monitor active agents show the same `run` status; a thread opened by an alias id (resumed or mirror topic) shows its source thread's transcript and delivery state under the requested id and topic link; and an outcome that replaces a shown pending record arrives as a full, non-partial response; private persistence metadata and provider replay tokens are never exposed. Its Trajectory tab renders the shared engine projection from the authenticated read-only `GET /api/threads/{id}/trajectory` route: a mobile-first per-turn Input/Model/Tools timeline, exact parallel overlap when explicit span boundaries exist, ranked bottlenecks, and tap-open span details. Legacy events may contribute recorded durations to the ranking but are never positioned from inferred boundaries. Monitor covers managed services, active agents, background processes, live subscription quota windows, 30-day usage, an explicit safe config summary, and jobs (the shared job summaries: schedule, status, next run, last outcome). The menu (left drawer) opens with a card for the open thread (only while a thread view is on screen), labelled "This thread": run state and turn age, model and context occupancy from the thread's own usage, branch and changed-file count when the view already fetched git, the live run's label and state while a turn runs (following the thread view's poll), and when the thread has workers its first three (failed first, then running and queued) with a link to all of them. Below it come the three most recent other threads, then "All of zdx": one full-width row per Monitor section, in order Overview, Active agents (count), Services (running/total), Quota (the peak quota window), Jobs (the job count, or "N failed" in red when a last run failed), Workspaces, and Config. Each row opens its section. Workspaces and Config show no figure, so the menu needs no extra request for them. Every Monitor figure covers all of zdx, not one thread. Opening the menu reads the thread list and the Monitor snapshot once (and the workers once when the view has not loaded them); nothing polls while it is open. The thread tabs are in the thread view's tab strip, not in the menu. The Artifacts tab lists files newest first by file modified time, as the projection sends them, under local day headers (Today, Yesterday, a weekday and date, the year when it differs); each row shows the time, and a tap on it shows the full date. A sort chip offers Newest first, Oldest first, and Name A–Z (one list without day headers); rows without a time (the file is gone) come last. Download stays on each row. The preview (from the tab or from a message) shows Markdown artifacts rendered, with the same sanitizer as messages (no raw HTML, images, or scripts; only `http`, `https`, `mailto`, `#`, and `/` links) and a Source view, and shows plain text, data, and source code (the shared projection's `markdown` and `text` kinds, decided by file extension) as UTF-8 text with line numbers and approximate syntax colours. It renders at most the first 256 KiB and says so when it cuts; text files over 10 MiB are not fetched, and a file with a NUL byte is reported as not text. Both offer Download. Text artifacts cannot be annotated. Git is a read-only inspector for the current branch and ahead/behind state, worktrees, staged/unstaged/untracked files, recent commits, and lazy per-file diffs capped at 256 KiB. Git resolves the selected thread's persisted project root when it belongs to a repository, then falls back to the bot root. Every `/api/*` route requires fresh, bot-token-signed `Telegram.WebApp.initData` from an allowlisted Telegram user. Local monitor snapshots run off the async request worker and are cached for 30 seconds, except active agents, which are read from the run markers on every request; while any agent is active the Monitor view polls every 5 s instead of 30 s. Subscription quota endpoints use stored credentials read-only, never refresh tokens, run concurrently, and are cached for five minutes. The Mini App exposes no service control, process control, Git write, or destructive Git actions.
- Artifact feedback is the Mini App's one write path. In the HTML artifact preview (Artifacts tab and inline transcript attachments alike) the user switches between Browse (the page behaves as before and scrolls), Tap (select an element; a drag scrolls the part of the page under the finger), and Draw (finger strokes); in Tap and Draw the app covers the page, so page links and handlers never fire. Selecting and commenting are separate steps: a tap outlines the element and dims the rest of the page, and a bottom sheet shows its tag, label, size, and text, plus a local tree (its nearest ancestors up to 8, and up to 12 children) with Select parent, so a container such as a `div` or `section` can be chosen; picking a tree row moves the highlight and scrolls the page to it, and when the sheet or the note editor shrinks the page view the selected element is scrolled back into view (only the page's own scroll positions, never the app around it). The dimming covers the whole page view. Only Comment on this opens the note editor, and the element stays highlighted while the note is written. A selection whose element is removed from the page, or that has no box of its own (hidden, or `display: contents`), says so and cannot be commented on; Notes with a selection open ends the selection and shows the notes list; leaving Tap mode, Cancel, a send, or a lost bridge end it. The tree comes from the page and is shown as plain, bounded text, each node at most once. Each note has an intent (Change / Keep / Question / Note), a comment, voice notes, and images; notes can be edited and deleted, and only an explicit Send posts them to `POST /api/threads/{id}/artifacts/feedback`. While a send is in flight the notes are locked, and success removes exactly the notes that were sent. Closing the preview with an open, unsaved note asks first. The preview keeps its opaque-origin `sandbox="allow-scripts"` frame. A bridge script injected right after the doctype (comments before it allowed) answers the app's requests (pick, probe, scroll, marks, text selection) over `postMessage`. In Browse the page keeps native text selection: while Browse is open (and no note is being edited) the app asks the bridge for the current selection every 400 ms, and a non-empty one shows a docked bar at the bottom of the preview with the quote and Add note, clear of the phone's own selection menu above the text. Only Add note opens the note editor; it clears the page's selection, and the note's target is the exact quote (at most 2000 characters), up to 64 characters of the element's text before and after it (cuts never split a character; page text that holds a broken character, a lone UTF-16 surrogate, is sent with U+FFFD in its place, because the server's JSON parser refuses the whole submission otherwise; the same applies to every page string, and drafts saved earlier are repaired when sent), the smallest element holding the whole selection, and the selection's box in page coordinates. Its numbered mark follows that element. Image artifacts have no text selection; they keep Tap pins and Draw. While an HTML or image preview is open and the Mini App is visible, it asks `GET /api/threads/{id}/artifacts/version?path=…` (same login and path rules as `/artifacts/file`; answers `{sha256, size_bytes, modified_at}`, the SHA-256 of the file bytes, cached per path while length and modified time stay the same; a read of the bot host, never a Telegram call) every 30 s and when the app comes back to the front. When the hash differs from the version on screen, a "New version available" bar offers Update; nothing reloads by itself. Update waits while a note is being edited, a voice note records, a drawing is unfinished, or a send runs. It loads the new bytes and names them by their own hash (an image by the server's source hash), not by the advertised one; bytes equal to the version on screen just close the bar. Without notes, or with notes already made on an earlier version, it shows the new version and those notes keep their version. Notes made on the version on screen are checked first, in a hidden frame of the new page while the old page stays on screen. The check starts only after that frame's load event (styles, scripts, and images loaded) and its web fonts are ready, not when the injected bridge first runs: an element note needs exactly one element for its selector with the same tag, text, label, role, and captured markup; a text note needs its holding element with the same tag and the exact quote with the same words before and after it exactly once in its full text, and then the new page measures where those words are, which becomes the note's box (a note whose words cannot be measured, or whose measured words are not the quote because the page changed its text in between, does not move; the old box is never carried over); a drawing needs every element under it unchanged, in the same box, at the same page width, and fewer elements than the capture lists at most (12); an image note needs a byte-identical normalized copy; a whole-page note always passes. A page that does not finish loading within 8 s, or does not answer a request within 5 s, fails every check but whole-page notes. A page that changes its layout after it finished loading (late scripts, charts) is measured as it was then. These checks are mechanical and do not prove a note means the same thing. When every note passes, all of them move to the new version together, each keeping `carried_from` (the hash it was written on), which the prompt reports. Otherwise a sheet lists the notes that did not match and why, with Stay on this version (keeps the page on screen and every note; the bar stays) and Update anyway (shows the new version and keeps every note as a note on the earlier version: marks hidden, Send anyway or Discard). No branch removes a note. Everything it reports is untrusted page data: the page's scripts share its window and can forge element details; a per-load nonce and schema checks only filter stray messages. Reports become drafts the user sees before Send, nothing the user wrote is sent into the page, and no page message can start a send. Numbered marks follow their element through page and inner scrolling; a mark whose element is gone is shown as not found. When the page navigates away or the bridge does not answer, Tap and Draw are off until the artifact is reloaded. Unsent drafts, with their media, are kept on the device per user, thread, and artifact path (IndexedDB) through closing, tab switches, network failures, and an expired authorization; the UI says "saving" until the IndexedDB write commits and "saved on this device" only after it does (a reload before that loses the last change), and when device storage fails it says the notes are kept only until the Mini App closes. A submission names the artifact version by the SHA-256 of the bytes annotated: if the file changed since, the server answers `412` and the user must choose Send anyway (`allow_stale`), which tells the agent the notes describe an earlier version; drafts reopened on a changed file are shown as stale with Send anyway or Discard. The engine (`zdx_engine::core::artifact_feedback`) validates limits (attached images at most 40 megapixels, checked from the header before decoding), checks the version, saves a bundle (`$ZDX_HOME/artifact-feedback/<thread>/<submission_id>/`: `feedback.json`, media, drawing sketches, and a copy of the annotated version written from the same bytes that were hashed) and writes the prompt. Only threads of a Telegram chat or topic accept feedback: it becomes a turn through that chat's normal per-topic queue (it waits behind a running turn, keeps the thread's profile and model, follows alias routing) plus a short notice in the chat. A legacy worker mirror topic is such a topic, and the queued message takes the same path a typed one does there (forwarded to the worker, acknowledged and answered in the topic). Worker threads opened directly, and TUI/CLI threads, are refused (`409`) with the drafts kept; they are never rerouted to another chat. The sender must be allowed to write into that chat. Voice notes go through the usual transcription and images through the usual inline-image path, each with its note label next to it in the prompt. The agent gets element selectors, text, role, label, nearest heading, short markup, and positions; a text note comes as the quoted selection with the text just before and after it and the element holding it; each drawing comes as a schematic sketch (the stroke over numbered outlines of the elements under it) plus those elements and the smallest element holding the stroke. No screenshot is captured: the app cannot read the sandboxed page's pixels, so canvas, SVG, and image content is known only by position, and the user can attach a screenshot. `submission_id` is bound to the sender, destination, and content: a retry after the feedback was queued answers `duplicate`, a retry after a failed queue queues it, and the same id with other content is refused (`409`). The bundle is marked queued just before it is put on the queue, so a failed marker write queues nothing. `queued` means the in-memory queue accepted it, not that a model read it: a restart before the turn starts loses it, and a retry then still answers `duplicate`. The version hash covers the file bytes, not what the page builds at runtime. `active` is refused for this write, and a legacy `?id=active` view pins the first concrete thread it resolved.
- Image artifacts in PNG, JPEG, or WebP (typed by their bytes, not the declared MIME type) are annotated in the same preview, with the same notes, intents, media, drafts, version binding, and explicit Send. The original file is never changed. The engine (`zdx_engine::core::artifact_feedback::canonical`) makes one canonical copy of it: EXIF orientation applied in every format the same way (WebP included, with or without the `Exif\0\0` header), at most 1568 px on the long edge (never enlarged), within 768 KiB, a PNG when the image uses transparency (else JPEG, quality 90 down to 70, then halved until it fits), with no EXIF left. `GET /api/threads/{id}/artifacts/image?path=…` serves it to the Mini App (same auth and artifact resolution as `/artifacts/file`) with `X-Zdx-Source-Sha256` (the original's hash: the artifact version), `X-Zdx-Canonical-Sha256`, and `-Width`/`-Height`; at most two copies are made at once in the whole process, previews and feedback saves together (a slot is held until the decode and shrink finish, even when the client has gone; a request that waits more than 60 s for one gets `503` and the drafts are kept), and copies are cached under `$ZDX_HOME/cache/artifact-images/` by source hash and normalization version (a tampered cache file is made again). The app shows that copy as is, never re-encoding or re-orienting it, and refuses it if it does not decode at the named size. There is no page and no element tree: Tap places a pin (a second tap while its note is open moves it), Draw adds finger strokes that become one marked-area note, and a note can cover the whole image. Pinch zooms, one finger pans, a double tap in Browse zooms in and back out, and Fit returns to the fitted view; the image is fitted with letterbox or pillarbox bands. Zoom shows the copy's pixels, not the original's. Marks are fractions of the copy, so they stay on the same spot through zoom, pan, resize, and rotation. While a pin or area is being edited it is kept in view: when the editor opening or a rotation reshapes the image area, the view pans just enough to show it, keeping the zoom. A mark's number sits beside its mark, inside the image, never pinned to the edge of the screen; a tap outside the image places nothing, strokes are clamped to the image edge, and a stroke drawn only outside the image is dropped. The submission carries the copy's hash and size (`image`), and the drafts remember them: drafts reopened on another copy (the file or the normalization changed) are stale, with marks hidden, never moved onto the new frame. The engine refuses a submission whose frame is not exactly the copy it draws on: for the current file its copy; for a changed file sent anyway the cached copy of the version the user saw (refused when that copy is gone, and when the current file is now over the 25 MiB size limit: that check runs from the file length before anything else, whichever version the notes were made on). The bundle gets the original (`annotated-version-…`), the clean copy (`annotated-canonical.*`), the copy with every mark drawn on it, numbered, at the copy's own size and format (PNG keeps transparency) within 1 MiB, which the bot passes to the model byte for byte, and a close-up per mark that covers a small part of the image (a crop of the copy, at most 1024 px on its long edge, enlarged up to 4× for small regions). The prompt gives positions in copy pixels and percentages, the factor to the original's upright pixels, the original's format, size, and EXIF orientation, and the copy's path and hash. Large originals, camera photos included, are annotated automatically on the optimized copy, and the preview says so (`Optimized copy for annotation: W×H of W×H px`; `X-Zdx-Source-Width`/`-Height` carry the original's upright size). The limits on originals only bound what is read and decoded: at most 25 MiB (checked from the file's metadata before it is read or hashed) and at most 40,000,000 pixels (checked from the header before anything is decoded, whatever the shape or orientation), then the decoded output size (at most 160 MB, so wide-colour images near the pixel limit are refused). These limits apply to image-annotation originals only, not to images attached to a note, HTML artifacts, Telegram attachments, or `read`/`ask_media`. A cached copy is used only when the original it was made from meets them too (the cache record carries the original's byte length and size), so a stale send never draws on a copy made under other limits. An image over a limit answers `422` with the reason and the limits and stays a passive preview of the original; unsent notes on it stay on the device, the preview says so, and they come back when the image can be annotated again. That limit is on the decoded output, not on peak memory: decoders add working buffers on top. The shrink is a box filter that writes the small copy directly (no full-size intermediate), and it runs before the image is turned upright, so no full-size rotated buffer is made; an EXIF block that cannot be read is refused. Measured in release builds for 40-megapixel originals, one copy peaks at about 150–170 MB of heap for baseline JPEG and RGBA PNG, about 225–270 MB for lossy WebP and progressive 4:2:0 JPEG, and up to about 480 MB of heap (about 0.5 GB RSS) for some encodings (progressive 4:4:4 or CMYK JPEG, lossy WebP with alpha); with the two-copy limit that is about 1 GB for normalization alone. The whole-route worst case with feedback uploads in flight (about 1.2 GB of heap) is an estimate, not measured. Images attached to a note are decoded by the bot's usual inline-image path when the turn is ingested, outside this limit, like photos sent in Telegram. `CANONICAL_VERSION` 2 marks the box-filter output. Drawings are also limited by how long their lines are once drawn, not only by point counts; a drawing over that limit is refused with its note number. Strokes not yet turned into a note keep Send disabled until the user taps Comment or Clear, and leaving Draw with such strokes asks before discarding them (declining keeps them). When zoomed in, the Fit button moves to the other top corner if a mark or its number is under it. GIF, SVG, other image formats, and images over the limits answer `422` with the reason and stay a passive preview of the original that says why. PDF artifacts cannot be annotated yet.
- `/prompt_builder` (typed, native menu; `/prompt-builder` also accepted) starts the same staged flow as `/handoff` with the intent as input:
  - works inside topics and DMs (not `General`); the generated prompt is previewed with Accept / Discard buttons and regenerates on a new message — it is the one staged command that gates on confirmation, because accepting it writes to the current thread
  - Accept runs the generated prompt as the user's real message in the current topic (a normal agent turn); the preview message is kept (edited) as the turn's reply anchor
  - Discard / `/cancel` delete the staging messages and leave the thread untouched

---

## 17) Telegram turn status and final reply (`zdx-bot`)

- While a turn runs, the bot keeps one live status message carrying the current activity and the Cancel button, edited in place at most once per 3 s. A change inside that interval is not dropped: the newest activity is shown when the interval ends, even if no further event arrives, and a pending change is discarded when the turn ends. During a known flood wait the edit is skipped and tried again one interval later.
- The status message also offers an `Open Thread` button that deep-links the Mini App to that turn's effective thread, on the same terms as the topic header: only when `[telegram.server]` is enabled with `mini_app_url` set, otherwise Cancel stands alone. Audio turns show it once transcription hands over to the agent, since the thread is not known while transcribing.
- When the turn completes, the status message is deleted and the assistant reply is sent as a new message. The reply's Telegram timestamp is the turn's end time, it carries no `edited` marker, and it raises a normal message notification (Telegram does not notify on edits).
- The new reply keeps the same reply target the status message used. Only a refusal of the reply target itself (`message to be replied not found` / `REPLY_MESSAGE_ID_INVALID`) falls back to sending without one; any other failure is not repeated, because it may have been delivered. The status message follows the same rule.
- Suggested replies (`<followups>`) are an inline keyboard on the reply message itself (or on the captioned file, §15). Only a reply with no text posts a separate `💡 Suggested replies` message. A tap replaces the suggestions with the chosen option and starts the turn; Dismiss removes the suggestions. Link buttons on the same message (a captioned file's `Open artifact`) stay in both cases. Neither changes or deletes the reply text.
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

- A topic created from an ordinary `General` message in a forum chat is initialized as an orchestrator topic before its first turn **only when the chat's Telegram profile opts in** with `telegram.profiles.<name>.orchestrator = true` (default `false`; unprofiled chats never opt in). Chats without the flag keep classic behavior (General → normal coding topic). `/new`, the launcher, and `/btw` topics keep the default profile regardless — a side question is an ordinary read-only thread that reads the source with `read_thread`, never a second home base. `/handoff` is the exception: its card's `Continue as:` choice decides the role, defaulting to the source thread's, so handing off an orchestrator produces another orchestrator and any thread can be handed off into a new one (see §16). This explicit choice is not gated by the `orchestrator` opt-in flag, which only controls what a `General` message creates.
- With BotFather **Threaded Mode** enabled, each brand-new thread in the bot's private chat is likewise initialized as an orchestrator thread on its first sighting: the message must carry a client-created `message_thread_id`, not be a known slash command, and map to a thread file that does not exist yet. It also gets the same pinned orchestrator card a General-created topic gets, posted right after the user's first message (pinning is best-effort in private chats). Plain unthreaded DMs and pre-existing DM threads keep their current profile.
- Orchestrator turns use the built-in profile's rendered prompt — which composes a ZDX operating manual, the full discovered skills catalog, project context, memory, and a bounded recent-activity snapshot (most recently active projects and top-level threads) — plus the Telegram instruction layer, and exactly its declared tool list: `read`, `grep`, `glob`, `ask_media`, `telegram`, `git`, `gh_api`, `list_models`, `thread_search`, `read_thread`, `todo_write`, `memory_search`, `web_search`, `fetch_webpage`, plus the six thread controls below. `bash`, `edit`, `write`, `apply_patch`, `invoke_subagent`, and the background tools are excluded.
- In the Telegram bot, orchestrator turns also receive a **Telegram Workspaces** block: every bound **workspace** profile (name, chat id, root) and, under each, the project-level skills a worker in that root will discover (`.zdx/skills`, `.claude/skills`, `.agents/skills` of the root and its ancestors; bundled and user skills are excluded since the orchestrator already has them). A skill reachable from nested workspaces is listed once per root, under the deepest root containing it; workspaces sharing that root each list it. Descriptions are cut to their first line (≤140 chars). This is how the orchestrator knows what a workspace can do without running there. Orchestrator profiles are **not** listed and are skipped during skill attribution, so a management group sharing a workspace's root can neither be offered as a worker destination nor silently claim that root's skills. When the current chat is a configured orchestrator group, the block ends by naming its default worker root.
- The orchestrator's read-only posture is structural: its declared tool list contains nothing that can mutate local or remote state, so mutations and anything needing a shell must go to a worker.
- The orchestrator has no subagent access. `invoke_subagent` is excluded so every unit of delegated work is a visible worker thread the user can open in the Mini App and the orchestrator can interrupt, rather than a hidden child run blocking the middle of a reply.
  - `invoke_subagent` holds the calling turn open until the child exits. The orchestrator must stay fast and available for the next message and for `[worker update]` callbacks, so this is a deliberate decision (2026-09), not a missing feature: quick lookups use the orchestrator's direct read tools, and investigations or longer work go to asynchronous workers. Do not propose re-enabling subagents for the orchestrator (for example through `allowed_subagents`) unless the delegation no longer blocks the turn.
- The reserved `orchestrator` profile is not invokable as a subagent by anyone. Beyond being hidden from the advertised catalog, subagent name resolution rejects it, so `invoke_subagent`, `zdx exec --subagent`, and job `subagent:` frontmatter all fail rather than starting a second home base. The bot resolves the profile from the embedded definition instead.
- The orchestrator's model and thinking level resolve through the shared model-spec resolver as: the chat's configured model → `[subagents.overrides.orchestrator]` → the topic's own `/model` override. An override without an `@level` suffix keeps the preceding level. The status card reflects the same resolved config and labels model/thinking provenance independently as `workspace/global default`, `orchestrator override`, or `topic override`. This is the only place the `orchestrator` override key takes effect, since the profile is never reachable through `invoke_subagent`.
- An optional personal overlay at `$ZDX_HOME/orchestrator.md` is appended to orchestrator prompts only (never to workers or other agents). A missing or empty file is a no-op; read failures are logged and skipped.
- Orchestrator topics get the same async LLM title on their first message as other topics (Threaded Mode DM threads are otherwise left as "New Thread" or a truncated first line), and do not post retry buttons on failed turns.
- The orchestrator topic's pinned header (and `/status`) uses an orchestrator card: model/thinking, thread, profile, context/usage/pricing, plus a live worker summary (running/queued/settled counts and the five most recently touched workers — last created, prompted, or started/finished a turn, newest first — by title and status; each worker title links to the worker thread in the Mini App, or to its legacy mirror topic when it still has one). Root and branch lines are omitted as fixed noise for the home base. The bot re-renders a card it posted in the current process whenever one of that orchestrator's workers is created, prompted, starts its first tool call, or finishes, so the pinned card is a live status; a re-render identical to the last one Telegram accepted sends no edit, and a refresh skipped by a flood wait is caught up by the next one. `↻ Refresh` remains for cards posted before a restart.

### Worker threads

- A worker is an ordinary visible thread bound to one existing project root, created by the orchestrator. Its meta records the owning orchestrator as `parent_thread_id` (lineage only; no `origin_kind`, so it stays a visible top-level thread and `zdx threads show <orchestrator>` lists it as a child). Worker turns run through a child `zdx --thread <id> exec` process in that root, so the worker resumes its own persisted JSONL history on every prompt and can also be resumed manually.
- Prompts for one worker run strictly serially through an in-memory FIFO; different workers run concurrently with no global cap. Each queued prompt gets a process-unique integer `prompt_id` when it is enqueued (from any path: orchestrator tools or legacy mirror-topic messages); ids are never reused within a process and are lost on restart with the queue.
- A single waiting prompt can be removed by id without affecting the running turn or the other queued prompts. Removing the last waiting prompt makes the worker idle once its current turn ends. The running prompt has already left the queue and cannot be removed; cancellation is the only way to stop it.
- Cancellation stops the current worker turn by terminating the child's entire process group (TERM, short grace, then KILL), reaps it before any queued successor could start, and clears the queue. The thread survives and a later message resumes it.

### Thread-control tools

Available only where a live worker manager exists (the Telegram bot); elsewhere the tools fail with `orchestrator_unavailable`. None of them blocks on worker progress: the orchestrator delegates, ends its turn, and is resumed by the completion callback below. `get_thread_status` is on-demand inspection, not a polling primitive.

- `create_thread(root?, prompt, title?, model?)` — `model` is a unified `provider:model[@thinking][@fast]` override. `root` is optional: an explicit value always wins and keeps the handling it has always had, and omitting it resolves the owning orchestrator group's configured default worker root. The resolved root is what gets persisted on the worker, and the result reports it as `root` plus a `root_source` of `explicit` or `orchestrator_default`, so a default is never invisible. Omitting `root` from a thread with no configured default is an `invalid_input` error rather than a guess. The tool validates the root exists, creates a visible worker thread, queues the first prompt, and returns the thread id at once.
- Prompts sent through `create_thread` and `send_thread_message` reach the worker after one `[Sent by the orchestrator in thread <id>]` line. The line claims only the sender; any `User request:` / `Orchestrator notes:` split inside is written by the orchestrator. Prompts typed in a legacy mirror topic are queued without it.
- `send_thread_message(thread_id, message)` — queues another prompt; re-attaches an unmanaged existing thread from its persisted root. The caller becomes the owner at once, even mid-turn. The returned snapshot's `queue` includes the new prompt (last) with its `prompt_id`.
- `get_thread_status(thread_id?, include_last_answer?)` — status (`queued`/`running`/`completed`/`failed`/`cancelled`), queue depth, `queue` (waiting prompts in run order as `{prompt_id, prompt}`, the prompt text capped at 300 characters; the running prompt is not listed), and `mirror_url` (a legacy mirror topic link, `null` for silent workers); without an id, lists all workers owned by the calling orchestrator, most recently touched first. The worker's last answer is omitted by default, because the `[worker update]` already delivered it; `include_last_answer: true` adds it as `latest_final_text` (capped at 4000 characters; held in process memory, so `null` after a restart or for an unmanaged thread). `read_thread` covers older history. The worker snapshots returned by `create_thread`, `send_thread_message`, `remove_thread_prompt`, and `cancel_thread` never include it. Every status (managed or not) includes `run`: the shared live run status of a turn running on that thread in any process, or `null` when none runs. A managed worker also reports `current_tool` (the tool in flight, from the worker's streamed activity), `seconds_since_last_activity` (tool events only), and `turn_elapsed_seconds`. For a thread this process does not manage but whose file exists, the call succeeds with `managed: false`, `run`, and `seconds_since_last_write` (the thread file's mtime, which lags a running tool round).
- Managed status also includes `current_tool_input`: the shared primary-command/target extractor's preview, whitespace collapsed and capped at 200 Unicode characters including any truncation ellipsis. Name and preview are correlated by tool-use id and describe the most recently started unfinished call; finishing it falls back to any earlier unfinished call. The preview is null before input arrives, when no primary argument exists, or when idle, and is omitted for unmanaged threads. Input activity never attaches to a different id. Activity age is a hint, not proof of a hang.
- `get_thread_status` includes nullable `context` for managed and unmanaged threads: `input_tokens` is the latest recorded input-bearing request's noncached input + cache-read + cache-write tokens, never cumulative thread usage and never output tokens. `model`, `provider`, and `recorded_at` come from that record, not current thread overrides. A positive registry `context_limit` matched by provider and model yields `percent_used`; unknown attribution or limit leaves those two fields null while retaining the token count. No usable usage within the bounded final 256 KiB (or unreadable history) yields `context: null`, not zero. This is a last-request estimate (`basis: last_recorded_request_input`), not an exact forecast of resumed context; trailing output-only usage does not replace it.
- `update_thread(thread_id, title)` — title only; refused while that managed worker is mid-turn (the atomic rewrite must not race the worker's own appends).
- `remove_thread_prompt(thread_id, prompt_id)` — removes one waiting prompt; returns the removed `{prompt_id, prompt}` and the worker snapshot. Fails with `remove_thread_prompt_failed` when the worker is unmanaged or the id is not waiting (already running, finished, or never queued). Never touches the running turn.
- `cancel_thread(thread_id)` — cancels the current turn and clears the queue; the thread is preserved.

### Silent workers and Mini App access

- Workers are **silent**: creating or prompting a worker opens no Telegram topic and posts nothing to Telegram. The user talks to the orchestrator; only the orchestrator talks to workers. Worker turns run as `zdx exec` and never receive the Telegram reply layer, so their output has no Telegram length or format limits.
- The user reaches a worker through Mini App deep links (`{mini_app_url}?startapp=<worker_thread_id>`, opening the full transcript), using the orchestrator chat's `mini_app_url`. Each link is skipped when the Mini App is not configured for that chat. Links appear in three places:
  - the orchestrator's reply ends with one `🛠 <worker title>` link per worker the turn created or messaged (successful `Create_Thread`/`Send_Thread_Message` results, first-touch order, placed above `↗ Open thread`), so the user can open it without relying on the model to link it;
  - the orchestrator card's worker lines;
  - the completion notice (below).
- A worker that still has a legacy mirror topic links to that topic instead, in all three places.
- Worker controls stay with the orchestrator: `cancel_thread`, `remove_thread_prompt`, `send_thread_message`, and `get_thread_status` work for every worker. The Mini App has no worker controls; it shows workers on the orchestrator's Workers tab and in the thread list's `Workers` filter.

### Legacy mirror topics

Workers created before silent workers may have a **mirror topic**: a forum topic whose thread aliases the worker thread (`alias_to`) and is marked `worker_topic` in its meta. No new mirror topic is ever created, and existing ones are never deleted. For those workers only:

- **Restart recovery:** at startup the bridge scans persisted thread metadata once for `worker_topic` + `alias_to` pairs and rebuilds the worker→mirror map. Each worker's `mirror_url` is the topic link (`https://t.me/c/<internal_chat_id>/<topic_id>`; DM-hosted topics have no link form). Recovery is derived purely from the canonical JSONL meta lines; there is no separate store.
- Orchestrator-sent prompts are posted into the topic as `📤` messages rendered from Markdown.
- While a turn runs, one live status line names the tool currently running (`🔧 Running `bash`...`, `⏳ Waiting for model...`). It is edited at most every 3 seconds and deleted when the turn's result posts. It carries `⏹ Cancel worker` (callback `wk:c`; the worker is resolved from the topic's persisted alias, never from callback data; it cancels like `cancel_thread` and answers "nothing to cancel" when the worker is not managed) and `💬 Open Thread` (Mini App; omitted when the Mini App is not configured for the host chat).
- Each finished turn posts its final text (rendered from Markdown, bounded, followed by the `↗ Open thread` Mini App link) or its failure/cancellation into the topic, in addition to the orchestrator callback.
- Messages sent in a mirror topic never run an in-process turn (the worker's child process owns the aliased JSONL). **Every** mirror-topic message, including slash commands and staged flows, is checked before any local interpretation, queued verbatim into the worker's FIFO, and acknowledged. Retry callbacks are also refused in mirror topics. After a restart, a mirror-topic message re-attaches the worker owning itself, so results keep landing in the topic even though orchestrator callbacks are gone.
- Managed workers are excluded from the General launcher's `🔄 Continue` picker while managed, so a resume topic can never become a second writer on a running worker's thread.

### Completion callbacks and restart semantics

- Worker completion is the only mechanism that resumes an orchestrator; there is no tool that holds a turn open until a worker goes idle, and therefore no suppression or replay of callbacks claimed by a waiter. Every finished prompt dispatches its callback.
- After every finished worker prompt, the bot dispatches one best-effort synthetic `[worker update]` message (worker id, status, bounded final text, worker context) into the orchestrator topic **that queued that prompt** through that topic's normal queue, using the shared synthetic-message mechanism. Because the synthetic message itself is invisible in chat, a one-line notice is posted in the orchestrator topic first (`✅ Worker 🛠 <title> finished · reviewing…`, with the title linked to the worker as described above; `🚫 … was cancelled` / `❌ … failed`), so the user can tell which worker woke the orchestrator and open it.
- A turn's result goes to the orchestrator that queued its prompt, even if ownership moved while it ran. A legacy mirror-topic prompt uses the owner at queue time.
- **Batched updates:** worker updates that pile up while a turn is running are handled together. When the queue starts a worker update, it also takes every worker update already waiting behind it and runs them as one turn carrying all of their text, in arrival order. No update is dropped or replaced by the last one, the running turn is never interrupted, and nothing waits for an update that has not arrived yet. Batching stops at the first queued user message or command — those keep their own turn and run next, so ordering is preserved. One notice per completion is still posted, so five finished workers show five notices and one review turn. A queued update cancelled before it runs is skipped, as before.
- All manager state is in-memory by design: worker ownership, queues, routes, and pending callbacks are lost on bot restart. Orchestrator and worker JSONL transcripts survive; workers can be re-attached with `send_thread_message` (or a message in a legacy mirror topic), and legacy mirror associations are recovered at startup as described above. There is no database, scheduler, durable job store, or delivery guarantee.

---

## 19) Telegram incoming message content (`zdx-bot`)

Applies to every chat the bot reads (DM, group, forum topic).

- A **rich message** (Bot API 10.1: structured text with headings, lists, tables, quotes, code, media blocks) carries no `text` field — its content is in `rich_message.blocks`. The bot reads those blocks, so a rich message runs a turn exactly like a plain-text one instead of being ignored as empty.
- Blocks are flattened to Markdown-ish plain text: headings to `#`-prefixed lines, lists to `-`/label-prefixed items (task items to `- [x]`/`- [ ]`), preformatted blocks to fenced code with their language, quotations to `>` lines with their credit, tables to `|`-separated rows (with a separator line under a header row), dividers to `---`, details blocks to summary plus content, LaTeX to its expression.
- Inline styling (bold, italic, spoiler, …) is dropped and its text kept. A link keeps its text plus `(url)` when the two differ; a custom emoji becomes its alternative emoji.
- Media inside a rich message is **not** downloaded as an attachment; each media block is named (`[photo]`, `[voice note]`, …) followed by its caption, so nothing is silently lost.
- Unknown block and inline types are not an error: their nested text is kept, and an update is never rejected for containing a type this version does not model.
- Everything downstream of ingest is unchanged: a rich message is subject to the same allowlist, queueing, staging, and topic-creation rules, and a topic auto-created from `General` takes its provisional name from the flattened text.
- **Images:** every photo, and every file sent with an `image/*` MIME type, is saved whole to `$ZDX_HOME/telegram/<chat_id>/<message_id>_<name>`; for a photo, the largest size Telegram offers. The model gets a copy at most 1568px on the long edge and 1 MiB, re-encoded only when the original is larger, so uploads stay small; the prompt says when the copy is downscaled. An image the model cannot take inline (for example HEIC, or one that cannot be shrunk into the limits) is still saved, and the prompt gives the reason and says whether `ask_media` can read the file. All photos of an album reach one turn.
- **Attachments that cannot be loaded** are not dropped silently: a file over Telegram's 20 MiB bot download limit, or one whose download Telegram refuses, becomes a sentence in the prompt telling the agent to ask the user to resend it. Such a message runs a turn even when it has no other content. The sentence never includes the raw download error.
- **Download retries:** a download that fails on a network or Telegram server error (no readable answer, timeout, HTTP 5xx) is tried up to three times, waiting 2 s and then 4 s; Cancel on the status message stops the wait. When every try fails, the message does not run a turn, text included: the bot posts a notice with `🔄 Try again` and `✕ Dismiss`. Try again dispatches the same message again through the topic queue (Telegram keeps the file, so nothing has to be resent); Dismiss deletes the notice. Pending retries are in memory and end on bot restart.
- **Videos:** a Telegram video, video note, animation (GIF/clip), or a file sent with a `video/*` MIME type is downloaded to `$ZDX_HOME/telegram/<chat_id>/<message_id>_<name>` and named in the turn prompt with its path and an instruction to inspect it with `ask_media`. This applies with or without a caption; a video-only message runs a turn. An animation loads once, even though Telegram also sends it as a `document`. A video over the `ask_media` inline limit (15 MiB) is not downloaded; the prompt states its size and the limit instead, so the agent can ask for a smaller clip.
