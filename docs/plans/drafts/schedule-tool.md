# Schedule Tool — Design Proposal (v5, thread-scoped self-wake)

> Stage: **draft**. Research + design, not implementation.
> v5 adds user-facing visibility/control. v4 corrected the mechanism after review. v3 corrected the model (user). v2 was the wrong model entirely.

**TL;DR:** A schedule is a **thread-scoped self-wake**: the agent asks to be re-prompted in its *own* conversation after a delay, optionally repeating until a bound is hit. In-memory, keyed by thread id, process-lifetime — same shape as goal mode. No daemon, no durable store, no cron. Schedules are **user-visible and user-cancellable**: a `/schedules` overlay in the TUI, a Schedules tab in the monitor, and read-only listing in the Mini App, backed by an ephemeral marker-file projection modelled on `agent_activity`.

---

## 1. The model

| | Automations (today; user plans to rename to **cron**) | **Schedule** (this proposal) |
|---|---|---|
| Defined by | Human, as a static `.md` file | The model, mid-conversation, via a tool call |
| Lifetime | Forever, until the file is deleted | Bounded: fire cap, expiry, or cancel |
| Runs in | A fresh thread per fire | **The thread that created it** |
| Context | Clean slate + project context | The full existing conversation |
| Survives restart | Yes (daemon + files) | **No, by design** |
| Owner | `zdx automations daemon` | The live TUI/bot process holding the thread |
| Purpose | Recurring unattended jobs | "Check every 10s whether X finished" |

The driving use case is **bounded polling inside a conversation**: the model waits for something without writing a `sleep` loop and without blocking its turn.

"Lives in memory" was literal. A schedule belongs to a live thread; if the process dies the conversation is not live, so the schedule is meaningless.

---

## 2. Verified current state

**Goal mode is the direct precedent** — in-memory, thread-keyed, bounded self-continuation:

- `GoalMap = Arc<Mutex<HashMap<String, Goal>>>` keyed by thread id (`zdx-bot/src/goal.rs:30`)
- *"Goal state lives in memory only. Process lifetime bounds a goal run, so a restart, thread reopen, or fork ends it and nothing resumes autonomous work on its own."* (`core/goal.rs:11`)
- Bounded by `has_capacity()` (`core/goal.rs:144`); `run_id: Uuid` fences async work (`core/goal.rs:174`)
- Terminal outcomes post to chat **and** persist a `ThreadEvent::Notice` (`goal.rs:208`)
- A cancelled/failed turn ends the run rather than feeding an error into another round (`goal.rs:115`)

**Wake paths are asymmetric:**

- **Bot:** `dispatch_synthetic_prompt(context, queues, chat, topic, user, prompt)` onto the topic's normal queue (`bot/synthetic.rs:51`); synthetic ids count down from `i64::MAX` (`synthetic.rs:19`).
- **TUI:** `enqueue_prompt` exists (`features/input/state.rs:915`), **but the queue only drains on `TurnFinished`** (`update.rs:385-399`). Enqueueing into an **idle** tab strands the prompt forever.

**TUI is multi-tab and swap-based:** `app.tui` is always the visible tab; inactive tabs live in `background_tabs: Vec<TuiState>` (`state.rs:23-30`). It also collapses interruption: `Completed | Interrupted => TurnOutcome::Succeeded` (`update.rs:406-409`).

**Cross-process live state already has an idiom.** Two registries publish one process's runtime state as JSON marker files that other processes read:
- `agent_activity` — ephemeral markers under `~/.zdx/run/agents/`, removed on `Drop`, stale (dead-PID) markers filtered when listing (`agent_activity.rs:1-6`).
- `background_activity` — markers under `~/.zdx/run/background/<bg_id>.json`, identity-guarded by pid + birth-time + pgid against PID reuse (`background_activity.rs:1-15`).

**Both surfaces already ship the exact UI being requested, for background processes:**
- **TUI `Overlay::Background`** — *"lists the background processes started by the current thread and lets the user stop them. Read-only view over the `zdx_engine::background_activity` registry; kill is issued as a `UiEffect`"* (`overlays/background.rs:3`). Keys: `↑↓` navigate, `x` kill, `r` refresh, `Esc` close (`background.rs:234`), with optimistic removal reconciled by the refresh tick (`background.rs:102`). Opened thread-scoped from `app.tui.thread.thread_handle` (`update.rs:1164-1169`), registered as command `background` with aliases `bg`/`jobs` (`common/commands.rs:235`) and routed at `command_palette.rs:399`.
- **Monitor Background tab** — lists processes from `background_activity`, `x` kills the selected one, `Enter` opens a detail overlay (`zdx-monitor/AGENTS.md`).

**Mini App is read-only by contract.** All API routes are `get(...)`: `/threads`, `/threads/{id}`, `/monitor`, `/git`, `/git/scope`, `/git/diff` (`zdx-bot/src/server.rs:438-443`). `apps/web/AGENTS.md`: *"The API is read-only and every route is GET. Do not add write calls until the Rust side grows them."*

**Bot thread ids can be aliased** (`alias_to` thin pointers), so an effective thread id does not reverse-map to a Telegram topic.

**Gap:** goal mode is bot-only — no goal/`after_turn` wiring in `crates/zdx-tui/src`.

---

## 3. Open-source grounding

**Claude Code session-scoped tasks — still applicable.** Default `CronCreate` is `durable: false` (session-only). Tasks "live in the current conversation", "only fire while Claude Code is running and idle", with "no catch-up for missed fires". `/loop` lets the model pick the interval and end the loop itself via `ScheduleWakeup(stop: true)`.

**No longer applicable:** Hermes Agent `cronjob` (durable `jobs.json`, `workdir`, delivery routing, misfire catch-up), AMD GAIA `schedule_task` (SQLite, pause/resume), Mastra `@mastra/scheduler` (storage-backed cron, concurrency policy, retries). All are durable-cron systems; in zdx that role is automations.

None of them model the user-facing control surface being added here — that comes from zdx's own `background_activity` idiom.

---

## 4. Core design

### 4.1 State: in-memory, keyed by canonical thread id

```rust
pub type ScheduleMap = Arc<Mutex<HashMap<String, Vec<Schedule>>>>;

pub struct Schedule {
    id: String,            // "sch_3f2a"
    label: Option<String>,
    prompt: String,
    kind: Kind,            // Once { after } | Repeat { every }
    state: State,          // §4.4
    fires: u32,
    max_fires: u32,
    expires_at: Instant,
    site: OwnerSite,       // §4.6
}
```

Keyed by the **effective/canonical** thread id (post-`alias_to`), so an aliased topic and its source share one entry.

### 4.2 No daemon

The owning process holds the timer: on create, the surface spawns a tokio task that sleeps and wakes the thread. `zdx automations daemon` is untouched.

### 4.3 Firing: an ordinary turn in the owning thread

Wrapped so the model knows it woke itself, mirroring goal mode's `<goal_round>` (`core/goal.rs:191`):

```
<schedule_fire id="sch_3f2a" fire="3/10">
You asked to be woken to check on this. If what you were waiting for is done,
act on it and cancel this schedule. If not, let it re-fire or cancel it.

Check: {prompt}
</schedule_fire>
```

Full conversation context comes free, and the answer surfaces wherever the thread already lives.

**Bot dispatch:** post a one-line visible notice (`⏰ woke to check: <label>`), then `dispatch_synthetic_prompt` onto the topic queue — synthetic prompts are invisible in chat (`orchestrator.rs:588`).

**TUI dispatch (new):** `enqueue_prompt` alone is insufficient (§2), so the timer sends a targeted event:

```rust
UiEvent::ScheduleFired { tab_id, thread_id, schedule_id, arm_id, prompt }
```

- Target tab missing, or its thread ≠ `thread_id` → terminate the schedule
- Target tab **idle** → start the turn immediately (`StartAgentTurn` / `StartAgentTurnInBackgroundTab`)
- Target tab **busy** → enqueue a provenance-bearing prompt (§4.5)

### 4.4 Lifecycle state machine

A bare `run_id` fences one async result; a schedule has more races. Explicit states plus a rotating `arm_id`:

```
Sleeping { arm_id } → Queued { arm_id, fire_no } → Running { arm_id, fire_no }
                    → Sleeping { new_arm_id } | removed
```

- `arm_id` rotates on every re-arm.
- Timer wake atomically validates `Sleeping + arm_id`, checks bounds, increments `fires`, claims `Queued`.
- At turn start, validate `Queued + arm_id`; drop the prompt if cancelled or superseded.
- Completion only mutates a matching `Running + arm_id`; never reinsert a cloned schedule.
- Never hold the map lock across a channel send or a Telegram await.

**Cancel** removes the live entry atomically. A running fire may finish but cannot re-arm; a queued fire is validated and dropped. This covers cancel-while-sleeping, cancel-mid-dispatch, and cancel-after-dispatch-before-run — and is the same path used by every user-facing cancel in §5.

### 4.5 Re-arm after completion, with fire provenance

`every: "10s"` means **10s after the previous fired turn completes**, not a fixed cadence — polling is the use case, and re-arming after completion makes overlap structurally impossible. The cost is drift, irrelevant for "is it done yet".

This needs to know *which* turn completed. Neither surface carries that today (bot `after_turn` gets only `TurnSite`/`TurnOutcome` at `goal.rs:103`; TUI `QueuedPrompt` holds only text and images at `features/input/state.rs:42`):

- **TUI:** add `PromptOrigin::Schedule { id, arm_id, fire_no }` to `QueuedPrompt`; track the running turn's origin. Read `TurnStatus` directly — `last_turn_outcome` collapses `Interrupted` into `Succeeded` (`update.rs:406`), but interruption must stop the schedule.
- **Bot:** reserve the synthetic message id before enqueueing, store it on the claimed fire, correlate on that id — never by parsing prompt text.

### 4.6 Internal owner site

`dispatch_synthetic_prompt` needs `chat`/`topic`/`user`, but `ToolContext` exposes only the effective thread id, which under `alias_to` cannot be reversed into a topic. So each schedule stores its origin:

```rust
enum OwnerSite {
    Telegram { chat: i64, topic: Option<i64>, user: i64 },
    Tui { tab_id: TabId },
}
```

Runtime routing metadata captured at creation, not durable user config. Bind a route-aware tool per bot turn and per TUI tab. Do **not** keep a single "latest route per thread" map — aliases would overwrite each other. Forks and `/handoff` produce new thread ids and do not inherit schedules.

### 4.7 Termination

1. **Model self-cancel** — primary; same shape as `ScheduleWakeup(stop: true)`.
2. **User cancel** — from any surface where the schedule is visible (§5). A schedule must never be something the user cannot get rid of.
3. **`max_fires`** — default `10`. On exhaustion, terminal notice (`Schedule "x" stopped at the fire limit.`), mirroring `GoalOutcome::LimitReached`.
4. **Expiry** — every schedule expires 1h after creation. A one-shot `after` beyond the lifetime is rejected at create, so an advertised delay is always honorable. No separate TTL parameter.
5. **Process/tab death** — restart, tab close, or the tab's thread being replaced.

Mirroring `goal.rs:115`: a fired turn **cancelled/interrupted by the user** cancels the schedule; a fired turn that **fails** cancels and reports.

A plain user message does **not** cancel anything (§8.1). Every terminal stop posts a notice and persists a `ThreadEvent::Notice` (new `NoticeKind::Schedule`).

**Anti-recursion:** a fired turn may call `list`/`cancel` but **not** `create`, or the model could reset its own bounds forever. Cap active schedules per thread at 3.

### 4.8 Tool surface

```json
{
  "name": "schedule",
  "description": "Wake yourself later in this same conversation. Use this instead of sleeping in bash or writing a polling loop when you need to wait for something to finish. The prompt arrives as a new turn in this thread with full context, and queues if a turn is already running. Repeats measure the interval from the end of the previous check, so it is not an exact cadence. Every fire costs a full turn, so prefer longer intervals and cancel as soon as the thing you were waiting for has happened. The user can see and cancel your schedules. Schedules expire after 1 hour and are lost if the process restarts. For durable recurring jobs that must survive restarts, an automation file is the right tool, not this.",
  "input_schema": {
    "type": "object",
    "properties": {
      "action": { "type": "string", "enum": ["create", "cancel", "list"] },
      "after":  { "type": "string", "description": "One-shot delay: '30s', '10m'. Must be under 1 hour. Use exactly one of after or every." },
      "every":  { "type": "string", "description": "Repeat interval measured from the end of each check: '10s', '5m'. Use exactly one of after or every." },
      "prompt": { "type": "string", "description": "Required for create. What to check or do when woken." },
      "label":  { "type": "string", "description": "Optional short label shown to the user in the schedules list." },
      "max_fires": { "type": "integer", "description": "For every. Stop after this many fires. Default 10." },
      "id": { "type": "string", "description": "Required for cancel. From create or list." }
    },
    "required": ["action"]
  }
}
```

`label` is no longer cosmetic: it is the primary identifier in every user-facing list, so the description should encourage setting it.

### 4.9 Binding and surface support

`schedule` is an **unbound stub** in the default registry, rebound per surface via `register_boxed`. It is **not** added to `ToolSet::Default`/`OpenAICodex` (that would advertise it where it cannot work); supporting surfaces opt in explicitly, exactly as orchestrator tools do.

| Surface | Supported | Why |
|---|---|---|
| TUI | Yes (new wiring) | Long-lived process owns the tab/thread |
| Telegram bot | Yes | Long-lived process; synthetic dispatch exists |
| `zdx exec` | No — `schedule_unavailable` | Process exits when the turn ends |
| Automation/cron runs | No — same | Use the automation's own schedule |

---

## 5. User-facing visibility and control (new in v5)

Requirement: schedules are user state, not just model state. The user must be able to see what is pending and kill it from wherever it is shown.

### 5.1 The cross-process problem

Schedules live in the owning process's memory. The TUI can render its own. **The monitor is a separate process and cannot read TUI or bot memory**, and the Mini App is served by the bot so it can only see bot schedules.

zdx already solves exactly this for background processes: publish runtime state as marker files that any process can read (`agent_activity`, `background_activity`).

### 5.2 Marker projection

Each owning process publishes one JSON marker per live schedule:

```
~/.zdx/run/schedules/<pid>-<schedule_id>.json
```

Contents: `schedule_id`, `label`, `prompt`, `kind` (`after`/`every` + interval), `fires`/`max_fires`, `next_fire_at`, `expires_at`, `thread_id`, `surface` (`tui`/`telegram`), `owner_pid`, plus the Telegram `chat`/`topic` when applicable so a reader can deep-link.

Rules, following `agent_activity`:

- The owning process is the **single writer** for its own markers — no locking protocol is needed for correctness.
- Written on create, rewritten on fire/re-arm, removed on cancel/expiry/`Drop`.
- Readers filter markers whose `owner_pid` is dead, and reap them, so a crashed process leaves nothing behind.

**This is a derived projection, not a return of v2's durable store.** In-memory remains the source of truth; the marker is ephemeral, PID-guarded, and auto-reaped; restart still drops every schedule, which is the intended semantics.

### 5.3 Cancel from a non-owning process

Visibility is a read; cancel is a write into another process's memory. The Background tab's `x` works because killing a PID is an OS primitive — there is no equivalent for an in-memory timer.

**Mechanism: cancel-request files.** A reader writes `~/.zdx/run/schedules/requests/<schedule_id>.cancel`. Each owning process runs **one** lightweight sweep task (~2s) that applies pending requests to its in-memory map through the ordinary cancel path (§4.4) and deletes the request file.

- One task per process, not per schedule.
- Worst-case latency ~2s, invisible at these intervals.
- Works uniformly for TUI-owned and bot-owned schedules.
- A request for an unknown id is deleted as stale.

Rejected alternative: a bot HTTP write route — it covers only bot schedules and breaks the Mini App's read-only contract (§5.6).

### 5.4 TUI: `/schedules` overlay

A direct copy of the Background overlay, which already implements this interaction for background processes.

| Wiring point | Change |
|---|---|
| `common/commands.rs:235` (next to `background`) | `Command { name: "schedules", aliases: &["sched"], description: "List and cancel schedules for this thread", category: "thread", shortcut: None }` |
| `overlays/command_palette.rs:399` | `"schedules" => (Some(OverlayRequest::Schedules), vec![], vec![])` |
| `overlays/mod.rs:172` | `Schedules(SchedulesState)` variant + render/key arms |
| `update.rs:1164` | Open thread-scoped from `app.tui.thread.thread_handle`, same as `Background` |
| `overlays/schedules.rs` | **New**, modelled on `background.rs` |

Behavior, matching `background.rs:234`'s hint row:

- `↑↓` navigate, `gg`/`G` ends, `r` refresh, `Esc` close
- **`x` cancel** — optimistic removal reconciled on the refresh tick (`background.rs:102`)
- **`Enter` detail** — full prompt text, interval, next fire, fires used, expiry

List row: `label · every 10s · next in 4s · fire 3/10 · expires in 52m`.

The TUI reads its **own** in-memory map directly — no marker round-trip for its own schedules.

### 5.5 Monitor: Schedules tab

A new tab modelled on the Background tab, which already proves the pattern (load from a registry, `x` to act, `Enter` for detail). It reads the §5.2 markers, so it shows schedules from **every** live process — TUI tabs and bot topics alike — grouped by surface and thread, the way the Background tab groups under per-thread headers.

`x` writes a cancel request (§5.3); the row clears on a later tick once the owner applies it.

Worth considering instead: once automations are renamed to **cron**, fold both into one "Scheduled work" tab (durable cron files + ephemeral schedules). That is a cheaper end state than two adjacent tabs, but it depends on a rename that has not happened, so the separate tab is the proposal.

### 5.6 Mini App: read-only listing

Add one GET route (`/api/schedules`) returning the marker projection, plus a Schedules section in `MonitorView.svelte` and/or a thread-scoped list in `ThreadView`. Types mirrored in `src/lib/types.ts` per `apps/web/AGENTS.md`.

**Recommendation: do not add a cancel write route.** The Mini App API is read-only by documented contract and every existing route is a GET (`server.rs:438-443`). More importantly, cancel is already available on that surface *conversationally* — the user can tell the bot "cancel the build watcher" and the model calls the tool. That satisfies "cancellable everywhere it is visible" without making this feature the one that breaks the read-only boundary.

If a button is wanted anyway, it should be a deliberate decision to open the API for writes, not a side effect of this feature.

### 5.7 "Main app" is ambiguous — needs your call

"Also on the main app" has two plausible readings in this repo, and they are different work:

1. **The Mini App** (`apps/web`, Svelte, served by `zdx-bot` at `/app`). Supports the reading that you listed three surfaces — monitor, main app, then separately "if I'm on the TUI". Note it has its own `MonitorView`, so "monitor" and "app" are already distinct there. Cost: §5.6.
2. **The main TUI** (`zdx` interactive) as opposed to the monitor TUI — i.e. "the main app" is just where you chat, and the following sentence elaborates it. Under this reading §5.4 already satisfies it and no Mini App work is in scope.

I have designed §5.6 as optional rather than guessing. Reading (2) makes the Mini App work disappear entirely.

Independently: if you want a schedule to be visible in the TUI *without* opening an overlay, a one-line status indicator (`⏰ 2`) next to the existing status line is the cheap version. Not proposed as part of this scope.

---

## 6. Implementation scope

| Component | Change |
|---|---|
| `zdx-types/src/events.rs` | Add `NoticeKind::Schedule` |
| `zdx-engine/src/core/schedules.rs` | **New**: `Schedule`, `ScheduleMap`, duration parsing, state machine + `arm_id`, bounds |
| `zdx-engine/src/schedule_activity.rs` | **New**: marker write/list/reap + cancel-request read/write, modelled on `agent_activity` |
| `zdx-engine/src/tools/schedule.rs` | **New**: unbound stub + bound impl, anti-recursion guard |
| `zdx-engine/src/tools/mod.rs` | Register stub only (not in base tool sets) |
| `zdx-bot/src/schedules.rs` | **New**: timers, synthetic-id reservation, notices, completion correlation, cancel sweep |
| `zdx-bot` turn setup | Bind route-aware tool per turn (`OwnerSite::Telegram`) |
| `zdx-bot/src/server.rs` | *(optional, §5.6)* `GET /api/schedules` |
| `zdx-tui` runtime/update | `UiEvent::ScheduleFired`, idle-vs-busy dispatch, `PromptOrigin::Schedule`, running-turn origin, per-tab binding, cancel on tab close, cancel sweep |
| `zdx-tui/src/overlays/schedules.rs` + 4 wiring points | **New** `/schedules` overlay (§5.4) |
| `zdx-monitor/src/tabs/schedules.rs` + `Section` enum | **New** Schedules tab (§5.5) |
| `apps/web` | *(optional, §5.6)* types + Schedules section |

No new dependencies.

---

## 7. Risks

- **TUI wiring is the real work** — goal mode never shipped there, so idle-dispatch, per-tab targeting, and provenance are all new. The `/schedules` overlay is the cheap part (a Background-overlay copy).
- **Marker/memory divergence.** A marker can briefly lag its in-memory schedule (fire in flight, cancel just applied). Readers must treat markers as a view, and the dead-PID reap is what keeps it self-healing. Cancel is idempotent, so a stale row cancelling an already-gone schedule is a no-op.
- **Cancel latency** ~2s cross-process. The TUI's own overlay is instant since it mutates memory directly.
- **Token cost** — every fire is a full turn with full context. Bounded by `max_fires: 10` and the 1h expiry.
- **Two adjacent monitor tabs** (Background, Schedules) if the cron rename never happens.

---

## 8. Decisions

1. **Plain user messages do not cancel schedules.** Confirmed. Goal mode calls `invalidate()` on a user turn (`core/goal.rs:174`), but a timer is independent of chatter: a user talking while waiting for a build should not silently stop the watch. Explicit cancel exists on every surface instead (§5), so a schedule is never unkillable.
2. **Defaults:** `max_fires: 10`, 1h expiry, 3 active per thread. Kept as proposed; revisit after real usage.
3. **Open — "main app" (§5.7).** Mini App vs main TUI. Changes whether §5.6 is in scope.
4. **Open — Mini App cancel button (§5.6).** Recommendation: no; use conversational cancel and keep the API read-only.

---

## 9. Why the earlier versions were wrong

**v2** modeled a schedule as a lightweight automation (durable store, daemon, fresh thread, delivery routing) — all consequences of assuming it outlives the conversation.

**v3** had the right model but assumed both surfaces could wake a thread symmetrically. The TUI cannot: its queue drains only on `TurnFinished`, it is multi-tab, and neither surface tracked which schedule a finished turn belonged to.

**v4** was correct but treated schedules as model-only state. Making them user-visible reintroduces a file — but as an ephemeral, PID-guarded *projection* of in-memory truth, which is a different thing from v2's durable store, and it is the idiom `agent_activity`/`background_activity` already established.

---

## 10. Acceptance criteria

**Core**
- [ ] `schedule(action:"create", every:"10s", prompt:"check if the build finished", max_fires:10)` returns `{ id, next_fire_in, max_fires, expires_in }`
- [ ] After ~10s the thread receives a turn containing the `<schedule_fire>` block with full prior context
- [ ] **Firing into an idle TUI tab starts a turn** (regression guard for the v3 defect)
- [ ] Firing into a **background** TUI tab targets that tab, not the visible one
- [ ] Firing while a turn runs queues the prompt; no overlap
- [ ] A repeat re-arms only after its **own** fired turn completes
- [ ] Self-cancel inside a fired turn stops it; completion does not resurrect it
- [ ] Cancel while sleeping, while queued, and mid-dispatch all stop it with no stray fire
- [ ] `max_fires` / 1h expiry post a terminal notice and persist `NoticeKind::Schedule`
- [ ] A user-interrupted fired turn cancels the schedule (read from `TurnStatus`)
- [ ] `create` rejected inside a schedule-fired turn; `list`/`cancel` allowed
- [ ] `after` longer than 1h rejected at create
- [ ] Aliased Telegram topic: keyed by canonical thread id, fires into the originating topic
- [ ] Restart drops all schedules; nothing fires afterwards; markers are reaped
- [ ] `zdx exec` returns `schedule_unavailable`

**User-facing**
- [ ] `/schedules` (alias `/sched`) lists the current thread's schedules with label, interval, next fire, fires used, expiry
- [ ] `Enter` shows the full prompt; `x` cancels; the row disappears and no further fire occurs
- [ ] A plain user message does **not** cancel an active schedule
- [ ] Monitor's Schedules tab shows schedules from a **different** process (bot schedules visible while running the monitor)
- [ ] Monitor `x` cancels a bot-owned schedule within ~2s
- [ ] Killing an owning process leaves no visible rows (dead-PID reap)
- [ ] Cancelling an already-gone schedule is a no-op, not an error
