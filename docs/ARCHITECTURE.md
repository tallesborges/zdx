# zdx Architecture

zdx is a terminal-based AI coding assistant built in Rust, featuring a non-interactive execution mode and a full-screen interactive TUI.

## Workspace Layout

- **zdx (binary):** CLI + exec mode, routes to the TUI when the `tui` feature is enabled.
- **zdx-types:** Pure shared value types (DTOs/enums) used across providers, tools, and events. No runtime deps.
- **zdx-assets:** Embedded asset content (prompts, instruction layers, default TOMLs, bundled skills, built-in subagents). No runtime deps.
- **zdx-providers:** LLM provider implementations (Anthropic, OpenAI, Gemini, etc.). Depends on zdx-types and zdx-assets.
- **zdx-tools:** Leaf tool implementations (bash, edit, read, write, glob, grep, etc.) with a minimal ToolContext. Depends on zdx-types.
- **zdx-engine:** Runtime engine: config, agent orchestration, thread persistence, skills, subagents, MCP, engine-backed tools, and all remaining runtime modules. Depends on zdx-types, zdx-assets, zdx-providers, zdx-tools.
- **zdx-tui:** Full-screen TUI (Elm/MVU), depends on zdx-engine.
- **zdx-bot:** Telegram bot surface.
- **zdx-monitor:** Service dashboard TUI.

Config provenance is an opt-in companion to the engine's layered loader: `Config::load_layered_with_sources` uses the same merge/parser as ordinary loading, retaining explicit leaf origins and array-owner origins in `ConfigSources` without serializing them into `Config`. Monitor loads its supplied root through this API and stores row labels separately from values; rendering adds compact workspace names without changing picker inputs or save targets. Ordinary config loads do not collect provenance.

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
- **Prompt layers:** additive prompt fragments appended after the base prompt. These are used for surface/runtime constraints (for example Telegram or exec output guidance) and behavior harnesses (for example automation/headless execution).
- **Named subagents:** optional standalone prompt profiles for delegated child runs. A subagent provides its own prompt body and can override model/tool/thinking configuration without inheriting the shared base prompt.

This keeps one source of truth for the default assistant while allowing surfaces and automation behavior to compose cleanly, and still supports specialist standalone subagents when needed.

## Orchestrator + Worker Threads (Telegram bot)

The reserved built-in `orchestrator` profile (SPEC §18) reuses existing primitives instead of adding a database or workflow engine:

- **Persistent profile resolution:** a top-level thread with `origin_kind = None` and `subagent_name = "orchestrator"` is a persistent-profile thread (`thread_persistence::{set_persistent_profile, read_persistent_profile}`). The bot resolves the reserved embedded definition (`subagents::load_builtin_orchestrator`) on every turn — user/project subagent files cannot override it — and pins the turn's tool selection to the profile's declared tools (`ToolSelection::Explicit`).
- **WorkerManager (`zdx-engine/src/core/workers.rs`):** the only new runtime structure. An in-memory map of worker thread id → owner thread, canonical root, prompt FIFO, status, cancellation token, and latest result, plus one unbounded completion channel. One tokio task drains each worker FIFO serially; different workers overlap naturally. All state is process-lifetime.
- **Live tool previews:** `read_stdout_events` correlates child input/start events by tool-use id, emitting a synthetic start before input when needed and deduplicating the later start. Input summaries use `zdx_types::tool_command_text` and are bounded to one line/200 characters before relaying. `forward_activity` records each unfinished call by id before forwarding to the surface; status uses the newest unfinished call's name and preview together. Finishing one concurrent call does not clear another, and turn boundaries clear all retained calls.
- **Status context usage:** `Get_Thread_Status` reads at most the final 256 KiB of each requested thread through `thread_persistence::read_latest_context_usage`, off the async executor. It selects the newest input-bearing usage record, includes cached input, and resolves the recorded provider/model's context limit from the model registry (including configured custom entries). Unknown usage/limits stay nullable. This path neither scans the thread corpus nor expands/rebuilds the SQLite index; other worker-control responses do not perform the context read.
- **Child-runner reuse:** worker prompts run through `run_exec_subagent_with_cancel` with `ExecSubagentOptions.thread_id`, i.e. `zdx --thread <id> exec` in the worker's project root, so the worker resumes its own JSONL history. The child is spawned in its own process group; cancellation/timeout TERMs the group, waits briefly, KILLs, and reaps the child before the FIFO can start a successor.
- **Thread-control tools (`zdx-engine/src/tools/orchestrator.rs`):** six tools registered as unbound stubs in the default registry (schemas/validation everywhere) and rebound to the live `WorkerManager` in the bot's registry via `register_boxed`.
- **Completion bridge (`zdx-bot/src/orchestrator.rs`):** one task consumes the manager's `WorkerEvent` stream (per worker: `Created` → `Prompted`/`Activity`* → `Completed`). `Created` opens the Telegram mirror topic and registers its link back on the manager (`set_mirror_url`, resolved even when no mirror opened), which is how the asynchronous topic reaches `create_thread`'s bounded wait and every later snapshot/callback. `Activity` (the child runner's `SubagentStreamSink` relayed as `WorkerActivity`) drives one debounced live message per turn. `CompletionEvent`s are turned into synthetic `[worker update]` messages dispatched through the owner topic's normal per-topic queue, using the same shared synthetic-message helper (`zdx-bot/src/bot/synthetic.rs`) as `/goal` continuations. Routes are recorded per orchestrator turn and are process-lifetime; the worker→mirror map is rebuilt at startup from persisted `worker_topic` + `alias_to` meta lines (`list_worker_topics`), so no extra store is needed; missed callbacks are dropped by design.

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
Token usage is event-sourced. The agent buffers usage deltas per attempt and emits one combined `AgentEvent::UsageUpdate` (carrying the active `model` + `provider`) at commit boundaries; transparently retried attempts drop their buffered usage to avoid double counting. A request's terminal usage event (the `consume_stream` EOF-success flush) additionally carries per-request latency (`duration_ms` + `ttft_ms`); interim/failed flushes do not, so latency rides exactly one usage event per request. `UsagePersistor` (in `thread_persistence`) turns those events into `usage` `ThreadEvent`s, attaching the model/provider (and latency on the terminal event) so any saved thread can be attributed per provider. `core/usage_stats.rs` aggregates usage/cost across all saved threads (per provider/model) for `zdx stats` and the monitor, reusing `ModelPricing` for cost. To stay fast at thousands of threads it maintains a **derived, disposable SQLite cache** (`$ZDX_HOME/cache/usage.sqlite`) of each thread's partial aggregate keyed by `(thread_id, mtime, size)`, re-scanning only changed threads (JSONL stays canonical; the cache is rebuilt on schema/`default_model` change or corruption, and bypassed via a full lean scan if unavailable). The monitor runs the aggregation on a worker thread so the dashboard never blocks.

Live subscription limits are separate from event-sourced token usage. `zdx-providers::subscription_quota::fetch_snapshot()` discovers stored provider accounts and fetches their quota windows concurrently without refreshing or writing credentials. `zdx-engine` re-exports that API; both `zdx quota` and the Telegram Monitor consume the same snapshot model. Long-lived UI surfaces may cache the snapshot to avoid polling provider quota endpoints on every render, while each CLI invocation remains live.

### Native Memory Index
Memory search is native-owned in `zdx-engine/src/core/native_memory.rs`. `zdx memory index` refreshes exported top-level thread Markdown, syncs the derived thread index, and builds `memory.sqlite` with document/chunk tables plus FTS5 over exported thread Markdown, Notes, and Calendar. The thread index (`zdx-engine/src/core/thread_index.rs`) owns `threads.sqlite`: thread metadata, export dirty state, a contentless FTS5 index over titles + user/assistant message text (hits resolve via `thread_meta.doc_id = thread_fts.rowid`, so no transcript copy is stored or read), and tool-call rows, synced incrementally by `(mtime,size)`; `list_threads()`, `search_threads()`, and `search_thread_tools()` are served from it with a raw file-scan fallback, and `browse_threads()`/`browse_projects()` back the monitor's Threads tab (child runs included, filtered and capped in SQL). Both databases live under `$ZDX_HOME/cache/` and are disposable; thread JSONL, exported Markdown, Notes, and Calendar remain canonical.

Search results carry the canonical source path (plus `thread_id` for thread hits) so callers read current files rather than the indexed snapshot; `zdxmem:v1:<source>:<hex16>` docids remain internal index identity. Lexical search is the default and explicit `keyword` path. Vector/hybrid requests are rejected unless a complete opt-in embedding profile exists, and agent searches never trigger corpus embedding or hosted-token spend.

---
*For file locations, see `AGENTS.md`.*
