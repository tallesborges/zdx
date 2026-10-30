# zdx Architecture

zdx is a terminal-based AI coding assistant built in Rust, featuring a non-interactive execution mode and a full-screen interactive TUI.

## Workspace Layout

- **zdx (binary):** CLI + exec mode, routes to the TUI when the `tui` feature is enabled.
- **zdx-types:** Pure shared value types (DTOs/enums) used across providers, tools, and events. No runtime deps.
- **zdx-assets:** Embedded asset content (prompts, instruction layers, default TOMLs, bundled skills, built-in subagents). No runtime deps.
- **zdx-providers:** LLM provider implementations (Anthropic, OpenAI, Gemini, etc.). Depends on zdx-types and zdx-assets.
- **zdx-tools:** Leaf tool implementations (bash, edit, read, write, glob, grep, etc.) with a minimal ToolContext, plus Unix supervision for invocation-owned process groups. Depends on zdx-types.
- **zdx-engine:** Runtime engine: config, agent orchestration, thread persistence, skills, subagents, MCP, engine-backed tools, and all remaining runtime modules. Depends on zdx-types, zdx-assets, zdx-providers, zdx-tools.
- **zdx-tui:** Full-screen TUI (Elm/MVU), depends on zdx-engine.
- **zdx-bot:** Telegram bot surface.
- **zdx-monitor:** Service dashboard TUI.

Config provenance is an opt-in companion to the engine's layered loader: `Config::load_layered_with_sources` uses the same merge/parser as ordinary loading, retaining explicit leaf origins and array-owner origins in `ConfigSources` without serializing them into `Config`. Monitor loads the viewed workspace through this API and stores row labels separately from values; rendering adds compact workspace names without changing picker inputs. Ordinary config loads do not collect provenance.

`workspace_config` is the shared workspace layer on top of it: it lists workspaces (Telegram profile `cwd`s + the caller's root), resolves each model setting to its value, origin layer (default/global/own overlay/ancestor overlay) and inherited fallback, and performs targeted writes and resets against one named file. A `Scope` (global alone, or one workspace through its layers) ties what is shown to where edits go, so an edit never starts from another chain's values; a workspace whose overlay is the global file (`overlay_is_global`) writes global. `reach` reports which listed workspaces read a file's value for each key. Monitor Config (read/write), `zdx config workspaces` and the Mini App `GET /api/workspaces` (read-only) are thin adapters over it. Config writers go through `Config::layer_document`, which refreshes only the global file against the commented template and keeps workspace overlays minimal.

## MCP Internal Engine

MCP support lives in `zdx-engine/src/mcp.rs` as an internal engine. The primary product surface is the helper CLI (`zdx mcp ...`), not automatic model-visible tool exposure.

- **Config source:** project-local `.mcp.json` using the standard `mcpServers` JSON shape.
- **Workspace/runtime:** `load_workspace(root)` initializes configured servers, resolves cached HTTP MCP OAuth credentials, captures per-server status/diagnostics, lists tools, exposes schemas, and supports direct MCP `tools/call` execution.
- **Helper CLI:** `zdx mcp servers|auth|logout|tools|schema|call` uses that workspace and emits structured JSON for inspection plus interactive auth/logout flows for OAuth-protected HTTP MCP servers.
- **HTTP OAuth cache:** remote MCP OAuth credentials are stored separately from model-provider OAuth tokens in `<base>/mcp_oauth.json`.
- **Naming:** discovered tools still get stable internal names like `mcp__xcode__build_app`, which the helper CLI can surface in structured output.
- **Default agent surfaces:** `zdx exec`, the TUI, and the Telegram bot keep the built-in model-visible tool list by default; MCP catalogs are not dumped into the provider tool list automatically.
- **Failure isolation:** each server is initialized independently; failed servers contribute diagnostics but do not prevent healthy MCP servers from loading.
- **Auth discovery:** OAuth-protected HTTP MCP servers are classified as `auth_required` when ZDX can discover protected-resource/auth-server metadata, instead of surfacing only generic load failures.
- **Lifecycle:** the helper CLI loads MCP state for the current invocation. Long-lived warm-session reuse for interactive surfaces is deferred until a dedicated session model is added.

This keeps provider integration unchanged for normal agent turns: providers still see the built-in `ToolDefinition` list unless an explicit MCP augmentation path is used.

## Prompt Architecture

Prompt assembly is layered in `zdx-engine` (assets come from `zdx-assets`):

- **Base system prompt:** `prompts/system_prompt_template.md` is the canonical default prompt.
- **Prompt layers:** additive prompt fragments appended after the base prompt. These are used for surface/runtime constraints (for example Telegram or exec output guidance) and behavior harnesses (for example job/headless execution). Jobs stack the exec layer and the job layer; a job that names a subagent gets the project-instructions, Safety/Git, and runtime-layer partials appended to the subagent body.
- **Named subagents:** optional standalone prompt profiles for delegated child runs. A subagent provides its own prompt body and can override model/tool/thinking configuration without inheriting the shared base prompt.

This keeps one source of truth for the default assistant while allowing surfaces and job behavior to compose cleanly, and still supports specialist standalone subagents when needed.

## Telegram Delivery and Flood Control

Outbound Telegram traffic and the truth about what reached a chat are owned once and consumed by surfaces (SPEC §8 `delivery` events, §15, §17):

- **Shared rules (`zdx-engine/src/telegram/flood.rs`):** typed errors (`TelegramApiError` with `retry_after`, `TelegramTransportError` that never keeps the token-bearing URL, local `FloodWaitActive`), `classify` into flood wait / rejected / not sent / unknown, `RetryPolicy::IMPORTANT` (3 attempts within 2 minutes, never shortening `retry_after`), the per-chat `ChatPacer` (policy: ≥1 s between new messages, ≤20 per rolling minute in groups, flood-wait cooldown), and the per-call log line. Both clients use it: the bot runtime client and `TelegramOutbound` (the `telegram` tool and `zdx telegram`).
- **Bot client gate (`zdx-bot/src/telegram/mod.rs`):** one `ChatGate` per client (one token), keyed by chat, shared by all topics. Calls declare a kind (new message / update / typing / unscoped) and the handle declares a mode: `Immediate` (default; skips a chat in flood wait, never waits, so the polling loop and the worker bridge never stall), `paced()` (waits out the flood wait and the pacing slot), `reliable()` (paced plus flood-refusal retries). Waiting modes check and record admission atomically at send time (`ChatPacer::try_admit_new_message`) and never wait past the retry deadline; a call that would is skipped with `FloodWaitActive`. Ambiguous failures are never retried. The gate is process-local; `TelegramOutbound` runs elsewhere and bypasses it.
- **Delivery records (`zdx-engine/src/core/delivery.rs`):** `DeliveryRecord` items (text/file → pending/sent/failed/unknown) persisted as `ThreadEvent::Delivery` before and after the sends (`zdx-bot/src/handlers/message/response.rs`, `deliver_reply`). Replay folds records by id and prepends `model_note` to the next user message (`ThreadReplay::trailing_note` covers a record after the last user message, so the live message equals the replayed one); transcripts, `format_transcript`, and the Mini App thread API render `notice_text`; the bot posts `user_summary` once. `zdx-bot/src/delivery_tracker.rs` is the process-local set of running deliveries (an RAII guard held from before the pending record is written until the outcome is persisted), shared with the Mini App server: `ThreadResponse.delivering` is sampled around the event read (`ActiveDeliveries::observe`; the running set and a finished-deliveries count share one mutex, so a delivery that ends during the read is always seen), so it is never false while the returned events can still show the pending record. `get_thread` resolves an alias id to its source thread before the read.
- **Posted messages (`zdx-engine/src/core/posted.rs`):** a message a job or another agent sends with `TelegramOutbound` (`zdx telegram`, the `telegram` tool) is recorded after delivery by `telegram::record_post` as `ThreadEvent::Posted` in the destination conversation's thread (`telegram::conversation_thread_id`, the same id the bot uses; one `alias_to` hop), with the sender's thread id and optional `context`. A send into the sender's own thread is not recorded. Replay prepends `posted::model_note` to the next user message, after any delivery note, through the same `trailing_note`, so the bot's first turn in a topic it never ran in still knows what the user is answering.
- **File cards (`zdx-bot/src/file_card.rs`):** an HTML file with an `Open artifact` link goes out as a text message (a `link_only` delivery item) with `Open artifact` and `Send file` (`sf:<key>`) instead of an upload. Cards are write-once: each delivery posts new ones, and nothing edits, deletes, or refreshes earlier messages. A Send file tap carries only the key; the handler derives the thread from the card's chat and topic and resolves the key through `core::artifacts` (the same list and path guard the Mini App download route uses), then uploads the current bytes as a reply to the card. Telegram's refusal of a late `answerCallbackQuery` is the only stale-tap signal (callback updates carry no time).

## Artifact Feedback (Mini App → agent)

The one Mini App write path (SPEC, Mini App server). Data flow:

- **Preview (`apps/web`, `ArtifactAnnotator.svelte`, `lib/annotate.ts`, `lib/draftStore.ts`):** the HTML artifact stays in an opaque-origin `sandbox="allow-scripts"` iframe. A bridge script injected after the doctype answers parent requests over `postMessage`; its reports are untrusted page data (a per-load nonce only filters stray messages). A newer version of the open file (`/artifacts/version`, polled only while the preview is visible) is checked in a hidden second frame with its own bridge (`lib/versionMatch.ts`); notes move only all together after mechanical checks and the user's Update, and keep `carried_from`. Text selection keeps that shape: in Browse the parent asks for the page's selection on a timer, and only an explicit Add note makes it a draft (a text quote with context and its holding element). The parent owns all input while annotating (an overlay for taps and drags, a canvas for strokes), so the page only ever answers. Drafts carry the SHA-256 of the annotated bytes and live in IndexedDB until sent; they are locked while a send is in flight. Send posts one JSON submission with base64 media and a client `submission_id`. Image artifacts (PNG/JPEG/WebP) use the same component with `ImageStage.svelte` instead of the iframe: no bridge. The app shows the engine's canonical copy (`/artifacts/image`) as is, and pins and strokes are stored as fractions of that copy while zoom and pan live in a separate image-relative view (`lib/imageMarks.ts`), so marks survive zoom, resize, and rotation and name the pixels the agent gets.
- **Shared layer (`zdx-engine/src/core/artifact_feedback.rs`, `artifact_feedback/image_marks.rs`):** for image artifacts `artifact_feedback/canonical.rs` owns the canonical copy (one bounded, upright JPEG/PNG per original hash and normalization version, cached under `$ZDX_HOME/cache/artifact-images/`) that the preview, the coordinate frame, and the model input all use; the save checks the submission names exactly that copy (the cached copy of the annotated version when sending a changed file anyway) and `image_marks.rs` draws the marks on it at its own size, plus per-mark close-ups; payload types and limits, artifact resolution through `core::artifacts`, version check (`allow_stale`) and snapshot from the same read, retry identity (fingerprint of sender/destination binding plus content), bundle under `$ZDX_HOME/artifact-feedback/<thread>/<submission_id>/` (committed by rename, marked `queued` by the surface), and the prompt with per-note media labels. Any other surface submits the same `FeedbackSubmission`.
- **Bot adapter (`zdx-bot/src/server.rs` + `src/artifact_feedback.rs`):** auth (user id from the middleware), destination from the requested thread id (a Telegram chat or topic only), the queue's allowlist gate checked up front, per-submission in-flight key, then the bundle is marked queued and a synthetic `ArtifactFeedback` message with labelled `local_attachments` goes on the per-topic queue. Ingest loads them with the same image and transcription code as Telegram attachments; in a legacy mirror topic the existing worker-topic routing forwards the result to the worker.

## Orchestrator + Worker Threads (Telegram bot)

The reserved built-in `orchestrator` profile (SPEC §18) reuses existing primitives instead of adding a database or workflow engine:

- **Persistent profile resolution:** a top-level thread with `origin_kind = None` and `subagent_name = "orchestrator"` is a persistent-profile thread (`thread_persistence::{set_persistent_profile, read_persistent_profile}`). The bot resolves the reserved embedded definition (`subagents::load_builtin_orchestrator`) on every turn — user/project subagent files cannot override it — and pins the turn's tool selection to the profile's declared tools (`ToolSelection::Explicit`).
- **WorkerManager (`zdx-engine/src/core/workers.rs`):** the only new runtime structure. An in-memory map of worker thread id → owner thread, canonical root, prompt FIFO, status, cancellation token, and latest result, plus one unbounded completion channel. One tokio task drains each worker FIFO serially; different workers overlap naturally. All state is process-lifetime.
- **Event-driven coordination only:** the orchestrator delegates, ends its conversational turn, and is resumed by the worker's completion callback. The manager exposes no call that blocks on worker progress, so there is no wait registry, no claiming of completions by a waiter, and no suppression or replay of callbacks — every finished prompt emits exactly one `WorkerEvent::Completed` and the bridge dispatches it. `Get_Thread_Status` is on-demand inspection of current state, not a polling primitive.
- **Live tool previews:** `read_stdout_events` correlates child input/start events by tool-use id, emitting a synthetic start before input when needed and deduplicating the later start. Input summaries use `zdx_types::tool_command_text` and are bounded to one line/200 characters before relaying. `forward_activity` records each unfinished call by id before forwarding to the surface; status uses the newest unfinished call's name and preview together. Finishing one concurrent call does not clear another, and turn boundaries clear all retained calls.
- **Status context usage:** `Get_Thread_Status` reads at most the final 256 KiB of each requested thread through `thread_persistence::read_latest_context_usage`, off the async executor. It selects the newest input-bearing usage record, includes cached input, and resolves the recorded provider/model's context limit from the model registry (including configured custom entries). Unknown usage/limits stay nullable. This path neither scans the thread corpus nor expands/rebuilds the SQLite index; other worker-control responses do not perform the context read.
- **Child-runner reuse:** worker prompts run through `run_exec_subagent_with_cancel` with `ExecSubagentOptions.thread_id`, i.e. `zdx --thread <id> exec` in the worker's project root, so the worker resumes its own JSONL history. The manager-owned runner holds the child supervisor lease, so workers survive the originating orchestrator turn but cancellation or manager-process death closes the lease and completes TERM→KILL cleanup before the FIFO can start a successor.
- **Thread-control tools (`zdx-engine/src/tools/orchestrator.rs`):** six tools registered as unbound stubs in the default registry (schemas/validation everywhere) and rebound to the live `WorkerManager` in the bot's registry via `register_boxed`.
- **Completion bridge (`zdx-bot/src/orchestrator.rs`):** one task consumes the manager's `WorkerEvent` stream (per worker: `Created` → `Prompted`/`Activity`* → `Completed`). Workers are silent: `Created` only refreshes the owner's pinned card, and no Telegram topic is opened. The user reaches a worker through Mini App deep links built by the shared `worker_link` helper (orchestrator reply, card, completion notice). Every `Completed` carries just its `CompletionEvent`: the bridge posts a one-line notice in the owner topic and unconditionally dispatches one synthetic `[worker update]` message through the owner topic's normal per-topic queue, using the shared synthetic-message helper (`zdx-bot/src/bot/synthetic.rs`). Routes are recorded per orchestrator turn and are process-lifetime; missed callbacks are dropped by design. Legacy mirror topics (from before silent workers) are recovered at startup from persisted `worker_topic` + `alias_to` meta lines (`list_worker_topics`) and registered on the manager (`set_mirror_url`); only for those workers does `Prompted`/`Activity`/`Completed` still feed the topic.
- **Worker identity:** there is no stored worker type. `thread_persistence::is_worker_lineage` (top-level + `parent_thread_id`) is the one rule, with its SQL twin `thread_index::WORKER_SQL` in the same crate; `list_recent_workers`, `ThreadKindFilter::Worker`, `worker_counts_for_parents`, the Mini App `?kind=workers` list, `zdx threads list --workers`, and the monitor badge all go through it.

## Jobs Scheduler

Jobs (`$ZDX_HOME/jobs/*.md`) are owned by `zdx-engine::jobs`; surfaces only call it.

- **Engine (`zdx-engine/src/jobs/`):** `mod.rs` parses definitions into a `JobCatalog` (invalid files are listed, never fatal), resolves root/model, and derives each job's shared `JobState` (status, next run, last outcome). `schedule.rs` owns schedules and occurrence keys, `store.rs` the durable state (claims, `flock` run lock, `last.json`, `jobs_runs.jsonl`, scheduler state, archive), `plan.rs` the pure per-tick planner (`plan_tick` → `Start` / `MissOnce` / `MissRecurring` / `Archive` / `Recover`), and `migrate.rs` the temporary move from `automations/`.
- **Runner (`zdx jobs run`, `zdx-cli/src/cli/commands/jobs.rs`):** takes the job's lock, claims the occurrence (scheduled runs, and once jobs without `--keep`), runs the attempts in-process through `exec::run`, then writes the outcome to the claim and `last.json` and archives a consumed once job.
- **Scheduler (`zdx jobs daemon`, `zdx-cli/src/cli/commands/daemon.rs`):** one tokio loop. Each tick reads the catalog, calls `plan_tick`, and applies the actions. A `Start` spawns a child `zdx jobs run --occurrence …` process (max 3 live, one per job) rather than running in-process, because runtime env vars such as `ZDX_THREAD_ID` and `ZDX_ARTIFACT_DIR` are process-global and a crash in one run must not stop the scheduler. The daemon writes outcomes only for occurrences that never ran (missed, failed to start) or whose run died (interrupted), always under the job's lock. It sleeps until the next due instant or the poll interval, and wakes early when a child exits.
- **Repair:** final claims are authoritative. The daemon's start-up and 10-minute rescans of all claims mark orphaned ones interrupted and rewrite `last.json` where a crash left it stale; the planner's `Archive` action finishes an archive a crash interrupted. Missed recurring summaries are deduplicated by a per-job checkpoint, not by claims, because a fast recurring schedule would otherwise create one file per missed minute.
- **Guarantees:** the exclusive claim makes each occurrence start at most once; the lock makes a job have at most one live run; the OS releasing the lock on process death is how an orphaned claim is detected. Side effects of a run are not deduplicated.

## Owned Process Lifetimes

On Unix, foreground Bash commands and child `zdx exec` runs use `zdx-tools::process_supervisor`. The owner retains the sole lease endpoint while an external supervisor, isolated from both the owner's and target's process groups, directly owns the target. The supervisor reports the target's `pid`/`pgid` back over the startup handshake, so the owner can register the process without inspecting it. Lease closure from cancellation, future drop, or owner-process death triggers TERM, a bounded teardown grace, KILL if needed, same-group cleanup, and target reaping. Healthy commands have no supervision deadline: the foreground bound (`bash_foreground_bound_secs`) ends the *wait*, not the command. On expiry the live handle — lease, completion, pipes, process group — moves out of the tool call into the process-global adopted-jobs registry (`zdx-tools::adopted`), the stream sinks flip from the in-memory tail to the background log files, and the job continues under the same supervisor; this makes adopted jobs session-scoped and killable through the same lease path. Because the adopting process holds that lease, the handoff is enabled only where the process can keep the job alive. One per-run policy, `tools::BackgroundExecution`, decides it: a run with background execution off (every subagent unless its definition sets `background_commands: true`, carried to the child as `zdx exec --no-background-commands`) never hands off and has no implicit limit either: a command stays in the foreground until it finishes, and only the caller's `timeout_secs`, the run's deadline, or cancellation kill it; with it on, `tools::surface_keeps_background_jobs` decides by surface: the TUI (`chat`) and the bot daemon (`telegram`) outlive the run, while `zdx exec` — and therefore orchestrator workers and opted-in `invoke_subagent` children — *drains* instead, awaiting `zdx_tools::adopted::drain` at the end of the exec command before returning from `dispatch`. The same policy shapes the tool list (`background_output`/`background_kill` present exactly when background execution is on and the run has `bash` or `invoke_subagent`) and the `Bash` and `invoke_subagent` schemas, in `core::agent::resolve_tools`. A managed subagent (`tools::subagent_job`) reuses the same machinery, for both `invoke_subagent` with `background: true` and a normal call in a run with a handoff bound, which waits up to `bash_foreground_bound_secs` and then hands the same child off, like a `Bash` handoff. The child is adopted at spawn and registered with the turn at once; its registry marker is written only at the handoff (a child that finishes first returns its answer and leaves nothing behind), under a lock shared with its exit callback so exactly one of completion or handoff wins. Beyond that: its child `zdx exec` is spawned like a blocking one, registered as an adopted job of kind `subagent`, and its stdout events are condensed into the job log that `background_output` reads; the end of the child's turn is written the moment the child reports it, so an answer is readable while the process is still exiting and after it is stopped. A run's tool deadline stops it as it stops a blocking call. Its owner is the agent turn, not the process: `run_turn_inner` holds a `TurnTasksGuard` whose drop closes the lease of every background subagent the turn started, so the child stops when the turn ends however it ends, before any drain, and the drain then only waits for that teardown. Draining must complete inside `rt.block_on`: the runtime is dropped when `cli::run` returns, which would drop the task holding the lease and kill the job. `adopt` awaits the job's reader tasks and records its exit before deregistering it, so a drained process never truncates a job's log or leaves its bookkeeping half-written. An interrupt during the drain routes to `terminate_all`, closing every lease so nothing is orphaned. `WorkerManager` owns worker-prompt leases independently of originating chat turns, while Bash `background: true` continues through the durable detached-process registry and intentionally bypasses invocation supervision. Descendants that deliberately leave the owned process group are outside this containment model.

## TUI Architecture (Elm/MVU)

The interactive mode (`crates/zdx-tui/src/`) strictly follows The Elm Architecture (Model-View-Update).

**Core Principle:** All state lives in one place (`AppState`). All mutations happen via the reducer (`update`). All side effects are explicit descriptions (`UiEffect`) executed by the runtime.

## Design Principles (Guidance)

- **Decision simplicity (prefer):** Favor designs where the answer to a UI question is obvious and derived from one clear place, reducing ambiguity and making decisions faster.
- **Low‑drift structures (prefer):** Avoid parallel state that can fall out of sync; prefer structures that minimize maintenance and drift over time.

### Data Flow

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│  Term Event  │    │ Agent Event  │    │ Async Event  │
└───────┬──────┘    └──────┬───────┘    └──────┬───────┘
        │                  │                   │
        ▼                  ▼                   ▼
    ┌─────────────────────────────────────────┐
    │           UiEvent (Unified)             │
    └────────────────────┬────────────────────┘
                         │
               ┌─────────▼─────────┐
               │ update(state, msg)│
               │ -> (state, effs)  │
               └────┬───────────┬──┘
                    │           │
          ┌─────────▼─┐       ┌─▼─────────────┐
          │ new state │       │ Vec<UiEffect> │
          └────┬──────┘       └─┬─────────────┘
               │                │
      ┌────────▼───────┐    ┌───▼─────────────┐
      │ render(state)  │    │ runtime executes│
      └────────────────┘    └─────────────────┘
```

### 1. Model (`AppState`)
State is a plain struct containing:
- `tui`: Core application state (input, transcript, thread, config).
- `overlay`: `Option<Overlay>` for modal UIs (command palette, file picker, etc.).

State is organized into **feature slices** (auth, input, thread, transcript), each exposing `state`, `update`, and `render` modules.

### 2. Update (The Reducer)
The `update` function is the single source of truth for state transitions. It handles `UiEvent`s and returns `Vec<UiEffect>`. It never performs I/O directly.

**StateMutations:** Feature slices return `StateMutation` enums to request changes on other slices (e.g., Input slice requesting a Transcript scroll). The reducer routes each mutation to the owning slice’s `apply()` method.

### 3. View
Pure functions render `&AppState` to a Ratatui frame.
*   **Interior Mutability:** `RefCell` is used *only* for render-time caches (markdown wrapping, selection mapping) to avoid expensive re-computations without mutating logical state.

### 4. Effects & Runtime
`UiEffect` describes I/O and task spawning only (e.g., `Quit`, `OpenBrowser`, `SaveThread`).
The `TuiRuntime`:
1.  Collects events (User input, Agent messages, Async channels).
2.  Feeds them to `update`.
3.  Executes resulting `UiEffect`s.
4.  Renders the view.

## Key Patterns

### Overlays (Modals)
Overlays (e.g., Command Palette, File Picker) are self-contained state machines in `AppState.overlay`.
- **Mutual Exclusion:** Only one overlay is active at a time.
- **Input Priority:** Active overlay intercepts keys before the main app.
- **Lifecycle:**
    - **Open:** Set directly by the reducer (often from input or overlay actions). File picker opening returns `DiscoverFiles` for I/O.
    - **Update:** Internal mutations + `StateMutation`s for global changes.
    - **Close:** Returns effects to run after dismissal (e.g., `LoadSession`).

### Async & Concurrency
- **Receivers in State:** Receivers for async workflows live in `AppState`. The runtime polls them and emits `UiEvent`s.
- **Task state only:** All async operations are modeled as tasks; UI derives loading state from `TaskState` only (no separate loading flags).
- **Task lifecycle:** The runtime emits `UiEvent::TaskStarted` when a task is actually spawned, and `UiEvent::TaskCompleted` with the wrapped result when it finishes. The reducer is the only place that mutates `TaskState`, and uses `TaskId` for latest-only gating.
- **Cancellation pattern:** Cancelable tasks use `CancellationToken` carried in `TaskStarted`. The reducer initiates cancellation via `UiEffect::CancelTask` (with the token); the runtime only calls `token.cancel()`.
- **Lifecycle flow:**
  - User action → reducer emits effect (with/without `TaskId`)
  - Runtime `spawn_task` emits `TaskStarted` → reducer marks running
  - Runtime emits `TaskCompleted` with result → reducer clears task + applies result

### Performance
- **Delta Coalescing:** High-frequency events (streaming text, scrolling) are buffered and applied once per frame (`UiEvent::Frame`).
- **Lazy Rendering:** Only visible transcript cells are rendered.
- **Wrap Cache:** Markdown layout is cached per cell ID.

### Agent State Machine
The agent progresses through `Idle` -> `Waiting` (for first byte) -> `Streaming` (accumulating deltas) -> `Idle`.

### Provider Error Classification
Provider adapters normalize failures into `ProviderError` with a typed kind plus optional HTTP `status` and provider-native `code`. Transport/request construction is classified at the `reqwest` or WebSocket boundary; HTTP adapters retain response status and extract common OpenAI, Anthropic, and Gemini error code/type fields; stream error events retain their provider code/type through the engine. Retry classification is structured-first: request/parse and known account-limit errors are terminal, transport/timeout is transient, HTTP `408`/`429`/`500..=599` and known overload/rate-limit codes are transient, and message matching is only the final fallback for unknown or unstructured upstreams. The provider-agnostic retry loop still owns the three-attempt budget, exponential backoff, retry events, and pre-visible-content safety gate.

### Usage Accounting
Token usage is event-sourced. The agent buffers usage deltas per attempt and emits one combined `AgentEvent::UsageUpdate` (carrying the active `model` + `provider`) at commit boundaries; transparently retried attempts drop their buffered usage to avoid double counting. A successful request's terminal usage event (the `consume_stream` EOF-success flush) additionally carries per-request latency (`duration_ms` + `ttft_ms`) and explicit wall-clock boundaries (`started_at` + `completed_at`); interim/failed flushes do not, so timing rides exactly one usage event per request. `UsagePersistor` (in `thread_persistence`) turns those events into `usage` `ThreadEvent`s, attaching the model/provider and terminal timing so any saved thread can be attributed and placed on a trajectory. Tool completions carry the same optional boundaries; the persistor queues timing metadata by tool id until checkpoint-batched results are written in canonical request order, preserving concurrent overlap without changing replay order. `core/usage_stats.rs` aggregates usage/cost across all saved threads (per provider/model) for `zdx stats` and the monitor, reusing `ModelPricing` for cost. To stay fast at thousands of threads it maintains a **derived, disposable SQLite cache** (`$ZDX_HOME/cache/usage-v3.sqlite`) of each thread's partial aggregate keyed by `(thread_id, mtime, size)`, re-scanning only changed threads (JSONL stays canonical; the cache is rebuilt on a schema change or corruption, and bypassed via a full lean scan if unavailable). Changed transcripts are parsed with no lock held and written in short chunked write transactions, so concurrent callers no longer time out on one another's sync; each caller skips chunks that another has already written (best effort, so some parsing can still be duplicated). The totals and daily series are read from one snapshot. Usage that falls back to the caller's `default_model` is cached unattributed and credited when read, so callers with different configs (bot, Monitor, `zdx stats`) share one cache instead of rebuilding it for each other. The monitor runs the aggregation on a worker thread so the dashboard never blocks.

### Live Run Status

Every agent turn writes one marker file in `$ZDX_HOME/run/agents/` (`zdx-engine/src/agent_activity.rs`), so any process can see what a run does without IPC. The turn rewrites it on phase transitions, on tool start and finish, on streamed tool output (at most once per second), and on a 5 s heartbeat that also carries the latest activity time. Finished calls stay in the marker until the next tool round, because the thread JSONL gets a round only at its `TurnCheckpoint`; readers drop calls whose id is already persisted. `RunRecord::status` is the one derivation (`RunStatus`: state, label, turn age, activity and heartbeat ages, tool counts). The Mini App thread, list, workers, and monitor routes, the Monitor TUI overlay, and `Get_Thread_Status` all read it. It reports ages only and never calls a run stuck. `list_active` decides liveness from process identity only (PID alive and its OS start time equal to the marker's `birth_id`), which is what restart gating and subagent continuation read.

### Shared Thread Trajectory

`zdx-engine/src/core/thread_trajectory.rs` is the canonical, UI-agnostic projection from persisted `ThreadEvent`s plus optional live tool snapshots into per-turn spans, exact interval tracks, observed wall/span/concurrency aggregates, and bottleneck rankings. It preserves recorded request/tool duration separately from explicit boundary width, pairs repeated tool IDs FIFO, and keeps legacy or incomplete spans unanchored. `zdx threads inspect --json` exposes that serde model for agents/scripts; the text CLI and Monitor timing overlay render its shared formatter. The authenticated Mini App endpoint `GET /api/threads/{id}/trajectory` adds bounded live-tool snapshots and returns the same projection. The Svelte pane owns only the mobile timeline, bottleneck, and detail presentation, joining persisted payload/result details from the existing activity stream by source-event sequence.

Live subscription limits are separate from event-sourced token usage. `zdx-providers::subscription_quota::fetch_snapshot()` discovers stored provider accounts and fetches their quota windows concurrently without refreshing or writing credentials. `zdx-engine` re-exports that API; both `zdx quota` and the Telegram Monitor consume the same snapshot model. Long-lived UI surfaces may cache the snapshot to avoid polling provider quota endpoints on every render, while each CLI invocation remains live.

### Native Memory Index
Memory search is native-owned in `zdx-engine/src/core/native_memory.rs`. `zdx memory index` refreshes exported top-level thread Markdown, syncs the derived thread index, and builds `memory.sqlite` with document/chunk tables plus FTS5 over exported thread Markdown, Notes, and Calendar. The thread index (`zdx-engine/src/core/thread_index.rs`) owns `threads.sqlite`: thread metadata, export dirty state, a contentless FTS5 index over titles + user/assistant message text (hits resolve via `thread_meta.doc_id = thread_fts.rowid`, so no transcript copy is stored or read), and tool-call rows, synced incrementally by `(mtime,size)`; `list_threads()`, `search_threads()`, and `search_thread_tools()` are served from it with a raw file-scan fallback, and `browse_threads()`/`browse_projects()` back the monitor's Threads tab (child runs included, filtered and capped in SQL). Both databases live under `$ZDX_HOME/cache/` and are disposable; thread JSONL, exported Markdown, Notes, and Calendar remain canonical.

Search results carry the canonical source path (plus `thread_id` for thread hits) so callers read current files rather than the indexed snapshot; `zdxmem:v1:<source>:<hex16>` docids remain internal index identity. Lexical search is the default and explicit `keyword` path. Vector/hybrid requests are rejected unless a complete opt-in embedding profile exists, and agent searches never trigger corpus embedding or hosted-token spend.

---
*For file locations, see `AGENTS.md`.*
