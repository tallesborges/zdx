> Stage: drafts (implemented 2026-11-12; the phase demos below have not been run live). Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Mid-run steering

## Context

Today a message sent to a busy agent waits in a queue and runs as a new turn after the current turn ends. This is true for the TUI, the Telegram bot, orchestrator threads, and orchestrator workers. Steering adds a second path: the message goes into the **running** turn at the next safe point. The agent gets more detail without a cancel.

Research (2026-10-30) on Claude Code, Codex, pi, and OpenCode found that steering is a harness feature. Each tool holds the message and adds it before the next model request in the same turn, usually after the current tool calls finish. I found no documented Anthropic or OpenAI text-generation API that lets a client append user input to a response that is already generating. Steering never stops a tool that is already running.

Code facts this plan builds on (verified at `8ece7f66`, 2026-11-12):

- **One loop for every surface.** `run_turn_inner` (`crates/zdx-engine/src/core/agent.rs:1226`) serves every surface. Each iteration is one model request plus its tool round. The only outside control today is a `CancellationToken`.
- **Tool round.** `process_tool_turn` (`agent.rs:2790`) appends the assistant blocks, runs all tools concurrently, appends all results as one message (`agent.rs:2831`), checks for interruption, then emits `TurnCheckpoint` (`agent.rs:2846`).
- **Turn end.** A text-only answer ends the turn through `finalize_non_tool_turn` (`agent.rs:2857`). The tool-repetition guard can end it through `finish_turn_early` (`agent.rs:1040`).
- **Where runs live:**
  - **TUI:** in-process (`crates/zdx-tui/src/runtime/handlers/agent.rs:48`).
  - **Telegram agent topics:** in the bot process (`crates/zdx-bot/src/agent/mod.rs:169`), behind a per-topic queue (`crates/zdx-bot/src/bot/queue.rs:57`).
  - **Orchestrator threads:** in the `orchestrator` service process, which also owns `WorkerManager`, the per-thread input queues, and worker-result delivery (`crates/zdx-engine/src/orchestration/runtime.rs`). The bot and `zdx orchestrator send` are clients over a Unix socket. Turns run in-process in the service (`run_profile_turn`, `runtime.rs:1051`).
  - **Workers:** by default a child `zdx --thread <id> exec` per turn (`crates/zdx-engine/src/core/workers.rs:896`). With `[workers] runner = "external"`, a launcher runs the turn in a container or VM and returns the transcript at the end (`crates/zdx-engine/src/core/external_worker.rs`). Neither has an input channel during a turn: the child's stdin is null or inherited (`crates/zdx-tools/src/process_supervisor.rs:411-415`).
- **Child event stream.** The worker child already streams tool lifecycle chunks (`{"t":"start"|"input"|"done"|"error"}`) on stdout, which the manager parses (`crates/zdx-engine/src/core/subagent.rs:560-581`, `workers.rs:88-107`).
- **Worker updates today.** A finished worker prompt becomes a `[worker update] Worker thread `<id>` …` input queued on the thread that asked for it (`queue_worker_update`, `runtime.rs:924`). Updates waiting together at the head of the queue are batched into one turn, and batching stops at the first user message (`drain`, `runtime.rs:567`). The bridge posts a one-line notice before the turn (`crates/zdx-bot/src/orchestrator.rs`).
- **Reasoning replay scope** starts at the last user message that carries no tool result: `current_turn_start` (`crates/zdx-types/src/messages.rs:391`), used by every serializer. Chat Completions also has `tool_call_turn_replay` (`crates/zdx-providers/src/openai/chat_completions.rs:333`).
- **Other code treats every user message as a new turn:**
  - thread trajectories (`crates/zdx-engine/src/core/thread_trajectory.rs:278-330`);
  - replay, which attaches pending delivery and post notes to the next user message (`crates/zdx-engine/src/core/thread_persistence/replay.rs:372-386`).
- **Voice waits for the queue.** Telegram downloads and transcribes voice inside `handle_message` (`crates/zdx-bot/src/handlers/message/mod.rs:47`), after the topic queue starts the item.
- **Staged input belongs to the session** (for example `/handoff`; `crates/zdx-bot/src/staging.rs`).
- **No server tools.** ZDX does not request Anthropic server tools (`crates/zdx-providers/src/anthropic/sse.rs:579`), so the rule against text after unresolved server-tool results does not apply today.

## Evidence from recorded timing (2026-11-12): estimates, not a steering test

**Method.** A read-only replay of real logs and thread files, 2026-11-04 to 2026-11-12:
- The `orchestrator` service logs (worker-update queue times, turn starts and ends).
- Bot queue logs.
- Per-request `usage` start and end times in the thread files.

It applies this plan's rule to each message that arrived while a turn was running. The message would be injected before the next model request of that turn that is not its first request. If no such request follows, it falls back to the queue. The script and its output are kept as a run artifact (`steer_timing/analyze.py`, `result.json`), not in the repo.

**Limits**
- This does **not** run steering and says nothing about how models react to a steer. Steering needs the implementation below before an agent test is possible.
- Turn ends are taken from log lines. When a log line is missing (an interrupted turn), the end is inferred from the last request.
- "Wait today" is a proxy:
  - worker updates: the time until the update was recorded in the orchestrator transcript;
  - Telegram messages: the time until the next turn start in that thread;
  - worker sends: the time left in the worker's running turn.
- Steer waits cover only the messages that would land in the turn.

| Messages that arrived while the target turn ran | Count | Would land in the turn (estimate) | Steer wait, p50 / p90 (estimate) | Wait today, p50 / p90 (proxy) |
|---|---|---|---|---|
| Worker updates → orchestrator | 75 of 1132 | 41 (55%) | 8 s / 22 s | 21 s / 40 s |
| Telegram user messages → their topic | 109 of 1721 | 53 (49%) | 9 s / 28 s | 25 s / 70 s |
| Orchestrator sends → a running worker | 327 of 967 | 319 (98%) | 15 s / 170 s | 345 s / 1503 s |

**Findings** (estimates)
- **Fast when the turn has another request.** Orchestrator and Telegram turns are short and spend most of their time in model requests, so a steer that lands waits about 8 to 9 s at the median.
- **About half the busy-time messages miss the turn.** 45% of busy worker updates and 51% of busy Telegram messages arrived when no later request followed in that turn, mostly while the final answer was being written. They fall back to the queue and wait about as long as today. "Continue after the final answer" (Later) would cover them.
- **Workers gain the most, with a long tail.** Today a message to a running worker waits for the rest of the worker turn (p50 about 6 min). A steer waits for the current tool round, but a long build or subagent call still makes the p90 170 s.
- **Unseen-finish sends are rare.** 22 `send_thread_message` calls were made after the target worker had finished but before the orchestrator had seen its update. The replay cannot tell whether those sends were wrong.
  - In 17, the update arrived while the deciding request was already in flight, so steering could not have shown it first.
  - In 3, steering would have shown it first.
  - In 2, the update was waiting in the queue behind a user turn.

  For the in-flight case the send result helps: a steer to a finished worker returns `queued` with reason `not_running`. With the trial on, the update is injected at the next safe point only if the turn has one; otherwise it runs as the next turn.

## Settled decisions

1. **Orchestrator → worker: the orchestrator model chooses Steer, Queue, or Cancel.**
   - **Steer:** `Send_Thread_Message` with `delivery: "steer"` (new parameter). The target is the worker, not one prompt: the message joins the worker's current work when it can; otherwise it is queued like today, and the result says which. No `expected_prompt_id`.
   - **Queue:** `Send_Thread_Message` with `delivery: "queue"`, or with `delivery` omitted (unchanged).
   - **Cancel:** `Cancel_Thread` (unchanged, separate tool).
2. **Worker → orchestrator: trial steering, with a switch (changed 2026-11-12).**
   - A `[worker update]` that arrives while the owner's orchestrator turn runs is steered into that turn at the same safe point as every other steer.
   - When the owner has no running turn, or its turn is closing, the update takes today's queued, batched delivery.
   - `[workers] update_delivery = "queue"` turns the trial off and restores today's behavior exactly.
   - Provenance stays: the same `[worker update] Worker thread `<id>` …` text and the same Telegram notice. The persisted message carries `phase: "steer"`; its source is in its text (worker update header, orchestrator sender line, the user's words) and in the live `user_message_injected` event. Decided 2026-11-12 during implementation: no extra persisted id/source fields, since no behavior reads them.
   - No special batching subsystem, no sleep, no debounce.
3. **Full surface scope stays:** TUI, Telegram text and voice in agent topics and orchestrator topics (user → orchestrator), and workers. Simplify the plumbing, not the features.
4. **Safe points only.** A steer never stops a running tool, never changes a request already in flight, and never enters the provider retry loop. It cannot prevent every stale tool call: a call decided before the steer arrived still runs.
5. **The ordinary queue stays.** Steering is an extra path.
   - An accepted steer is never dropped silently. It is injected, runs as a later turn or prompt, or is discarded or given back by an explicit cancel, which says so.
   - The one exception is a worker steer whose state is unknown because the child's report stream ended without its end-of-turn event (a crash, a kill, or a failed stdout write). Missing confirmation does not prove the worker did not read it, so it is **not** re-sent: it is reported as `uncertain` (see Workers). Steers that are clearly unread still take the normal queue.
   - No new restart durability: service and bot state stays in memory, as today.

# Goals

- The orchestrator can add details to a worker's current work without a cancel, and learns whether the message went into the turn or was queued.
- An orchestrator turn sees a worker's result at its next safe point, instead of after its final answer, when the trial is on.
- A TUI user and a Telegram user (text and voice, agent and orchestrator topics) can steer their own running turn.
- Every accepted steer ends in one reported outcome: injected, queued for later, discarded or given back by a cancel, or (workers only, after a crash) `uncertain` and not re-sent.
- Agents and scripts have programmatic paths: `Send_Thread_Message` for workers, and `zdx orchestrator send --delivery steer` for orchestrator threads.
- Steered threads replay correctly on every provider ZDX supports (thinking blocks, thought signatures, reasoning items).

# Non-goals

- Cutting a text stream that is already running, or moving running tools to the background (Codex `instant_interrupt`, Claude Code "send now").
- Continuing the same turn after the model has given its final answer. A steer that arrives then runs as the next turn.
- Live steering of external-runner workers. Their steers queue (reason `unsupported_runner`).
- Images in steers for the first version. A message with an image takes the normal queue.
- A generic `zdx steer` CLI for TUI or bot-agent runs in another process.
- Restart durability for steers, and automatic re-runs after a crash.
- Remote workers (`docs/plans/drafts/remote-workers.md` stays as written until that plan is revisited).

# Design

## Engine contract (shared)

**Steer source**
- `AgentOptions` (`agent.rs:37`) gets `steer: Option<SteerSource>`. `SteerSource` has two backends (implemented as `Inbox` and `File`):
  - `Memory(SteerInbox)` for in-process runs: TUI, bot agent topics, and orchestrator turns in the service.
  - `ChildFile(path)` for local worker children (see Workers).
- No backend sleeps, polls a timer, or debounces. The loop reads only at safe points.

**`SteerInbox`** (new `crates/zdx-engine/src/core/steer.rs`)
- It is a `Mutex` over `{ open, items }`. Each item has `id`, `text`, `source`, and `accepted_at`.
- `try_push(item) -> Accepted | Closed`. It succeeds only while the inbox is open.
- `take() -> Vec<Item>` is called by the loop at a safe point.
- `close()` closes the inbox. It is idempotent and does **not** return or remove items, so the engine can close it and the owner can still collect the unread items.
- `drain_unread() -> Vec<Item>` is called once by the owner at settlement, after the run has returned, on every outcome: completed, failed, or cancelled.
- The one mutex decides each item once: either it is pushed before the close, and the loop takes it or the owner drains it; or it is refused with `Closed`, and the caller queues it. There is no file and no I/O.

**Safe point**

It sits in `process_tool_turn`, after all tool results are appended and the interruption check passes, before `TurnCheckpoint`:
1. Take the pending items.
2. Append each one as a user `ChatMessage` with `phase: Some("steer")` (`ChatMessage::steer`); the phase persists on `ThreadEvent::Message`.
3. Emit `AgentEvent::UserMessageInjected { id, source }`.

The existing checkpoint, which carries the steer, is emitted before the next request. Persistence stays asynchronous and best-effort, as today. Parallel tools must all finish first, so a long tool delays delivery.

**New input counts as progress**
- `ToolTurnStats` reports the injected count.
- A round that injected steers resets the tool-loop tracker and the malformed-turn counter, and skips both loop aborts for that round. Otherwise a steer could be persisted and never seen.

**Closing**
- A scoped close guard covers every exit of `run_turn_inner`, including setup failure, interruption, provider errors, and loop aborts.
- `close()` also runs explicitly **before** the terminal event in `finish_turn_early` and `finalize_non_tool_turn`, so an owner that reacts to `TurnFinished` already sees a closed inbox.
- At the final answer, unread items stay in the inbox. The answer is delivered first and unchanged, and the owner drains and handles the unread items.

**Framing**
- Each source frames its own text, so no new wire helper is needed:
  - Worker updates keep `worker_update_prompt`.
  - Orchestrator sends keep `frame_worker_prompt` (`crates/zdx-engine/src/tools/orchestrator.rs:488`).
  - User messages keep the bot's `build_user_text` framing.
- The persisted `phase: "steer"` lets every surface show that a message joined a running turn.

## Provider wire shape, reasoning replay, and turn boundaries

**History shape**
- Each steer is its own user message after the tool-results message, so live state equals replayed JSONL.
- Replay already rebuilds such a message and keeps its `phase` (`replay.rs`).

**Reasoning scope**
- `current_turn_start` and `tool_call_turn_replay` skip `phase == "steer"` messages. A steer then does not end the current reasoning scope, and earlier thinking in the same run keeps replaying.
- This matters for Anthropic, where a `tool_use` turn sent without its thinking can be rejected.

**Other boundaries**
- Trajectories treat a steer as input of the current turn.
- Replay leaves pending delivery and post notes for the next non-steer user message.
- The transcript keeps showing steers as user cells.

**Fallbacks are ordinary**
- A steer that falls back runs as an ordinary user message or prompt, not `phase: "steer"`. It starts a new turn.

**Per serializer**

The steer always comes after every tool result.
- **Anthropic:** text after all `tool_result` blocks. Either a consecutive user message or a trailing block; the live check picks.
- **Responses (SSE and WS):** a user item after all `function_call_output` items. WS `previous_response_id` chaining must still see a prefix extension.
- **Chat Completions:** a user message after the `tool` messages.
- **Gemini:** a user turn after the `functionResponse` turn, with thought signatures kept on earlier model parts.

## Orchestrator service

The service is the one owner of orchestrator threads, worker results, and workers. Both orchestrator-thread steers live here, in memory.

**Running turn inbox**
- `Running` (`runtime.rs:104`) gets the turn's `SteerInbox`, and `run_profile_turn` passes it in `AgentOptions`.

**Admission (one rule for worker updates and user steers)**
- Under the threads lock: allocate the input id, `try_push` into the running turn's inbox, and on success add the id to `Running.inputs`.
- Allocating the id inside the lock keeps id order equal to admission order. Today `queue_worker_update` allocates before it takes the lock, so this moves.
- Adding the id to `Running.inputs` lets `CancelTarget::Input(id)` find a steer that the running turn holds.
- On `Closed` or no running turn: queue it exactly as today.

**Worker updates (trial)**
- `queue_worker_update` uses the admission rule when `update_delivery = "steer"`, with `source: "worker_update"`. Otherwise, and on fallback, batching is unchanged.
- The bridge notice is emitted first, as today.

**User input**
- `Submit` (`crates/zdx-engine/src/orchestration/protocol.rs:49`) gets `delivery: "queue" | "steer"`, default `"queue"`.
- With `"steer"`, the admission rule decides, and the reply says `in_turn` or `queued`. A `queued` input is already in the service queue.
- `follow` with `delivery: "steer"` (decided during implementation): admitted into the running turn, the reply says `in_turn: true` and the connection closes, so a follower never waits for a `TurnStarted` that will not name the input; queued, it is followed as usual. The bot's busy-topic path submits unfollowed; its free-slot path (`run_service_turn`) submits followed, so it renders the turn when the message queues.
- `zdx orchestrator send` gets `--delivery steer` (no `--wait`). This is the programmatic path for orchestrator threads.
- No new events: the injection reaches clients as the `user_message_injected` agent event (id = input id), and steers given back or dropped are reported in `InputsDropped`.

**Turn end** (in `drain`, under the threads lock, in the same step that clears `entry.running`, for completed, failed, and cancelled turns)
1. `close()` (idempotent) and `drain_unread()`.
2. Apply the turn's cancel disposition (below), if any.
3. Put the remaining unread items back into the queue **by input id**. Ids are allocated under the same lock, so this keeps arrival order against anything queued while the inbox was closed.
4. Unread worker updates become normal `WorkerUpdate` inputs, so today's batching applies. Unread user steers become normal user inputs.
5. Settlement checks (`is_settled`) see them as queued work, so a thread never looks idle with an unread update.

**Cancel** (under the threads lock: close the inbox and record the disposition on `Running`; settlement applies it)
- **Running turn cancelled, queue kept** (`CancelTarget::Turn`, `Input` naming the running turn or one of its steers, or `Thread { clear_queue: false }`):
  - unread worker updates are requeued, because results are never lost;
  - unread user steers are reported in `InputsDropped`, and the bot answers "Not added: the turn was cancelled".
- **`Thread { clear_queue: true }`:** unread items are dropped and reported in `InputsDropped`. Settlement never reinserts them.
- **A cancel of an unrelated queued input** does not touch the running inbox.

**Setting**
- `WorkersConfig` (`crates/zdx-engine/src/config.rs:681`) gets `update_delivery: "steer" | "queue"`, read when the service starts like the rest of `[workers]`.
- Changing it needs a service restart.

## Workers (`WorkerManager`)

**Pending list, owned by the manager**
- Each running local run has `steers: Vec<{ seq, text, requester, state }>`, a private append-only file (`<run dir>/steer.jsonl`, one `{"seq":N,"text":…}` line per steer), and `last_injected_seq`.
- `steer()` holds the manager state lock for the whole decision. Cancel, settlement, and the FIFO's pop of the next prompt take the same lock.

**`steer(worker, text, requester)` returns `in_turn` or `queued`, with a reason, under the manager lock**
1. **Local, `Running`, run not cancelled, steering still open:** append the line with one `write_all`, record it in the pending list, return `in_turn`.
   - On an append error: return the error, queue nothing, and close steering for this run, so later steers queue. A failed `write_all` leaves an incomplete line, which the child never parses.
2. **Running but cancelled** → queue, reason `cancelling`.
3. **Not running** → queue, reason `not_running`.
4. **External runner** → queue, reason `unsupported_runner`.
5. **Unmanaged** → today's re-attach and queue path, reason `not_managed`.

**Fallback queue entries**
- Every fallback is a normal `QueuedPrompt` at the back of the FIFO, for its own requester, marked `origin: "steer"`.
- It runs with the worker's full context, routes its completion to its requester, can be removed with `Remove_Thread_Prompt`, and is cleared by `Cancel_Thread`.

**Child side**
- The child reads only complete, valid lines past its last seq, at safe points.
- When it injects a steer, the engine emits `UserMessageInjected { id: seq }`, and the child writes it on its existing stdout stream of serialized `AgentEvent`s.
- The parent's stream parser (`crates/zdx-engine/src/core/subagent.rs`) translates it, and the manager records `last_injected_seq` directly in worker state, the same way it records tool activity before forwarding (`workers.rs`).
- At the final answer the child stops reading.
- Reports arrive while the run is still going, so error and cancel exits need no final report. The runner awaits its stdout reader before it returns, so every report that was written is recorded before settlement.

**Settlement** (after the runner returns, in the FIFO, under the manager lock, before `Completed` is emitted)
- **Cancelled run:** discard every pending item, mark it `discarded`, even when the runner returned `Ok`.
- **The parent saw the child's end-of-turn event:** the report stream is complete (reports come before `TurnFinished` on one ordered stream), so every item with `seq > last_injected_seq` is clearly unread. Queue each one (reason `turn_ended`) at the back of the FIFO, in order, with its own requester.
- **The stream ended without that event** (crash, kill, failed stdout write): items with `seq > last_injected_seq` are marked `uncertain` and reported. They are not re-sent.
- Then set the final status and emit `Completed` as today. Idle is never published before the requeue.
- Remove the run dir after the lock is released. A cleanup failure is logged and loses nothing.

**Uncertain delivery (no automatic re-run)**
- The manager learns of an injection only from the child's stdout report. A missing report after a crash or an output failure does not prove the worker did not read the steer.
- Such steers are marked `uncertain` in `Get_Thread_Status` and in the `[worker update]` for that run (one short line). They are never queued again automatically; the orchestrator decides whether to resend.
- There is no recovery machinery: no persisted-id reconciliation and no retry.

**Idle and settlement**
- `idle_for_requester` also counts unresolved steers from that requester: pending items of a running local run. A steer can still become queued work for its requester even when no queued prompt names it yet.

**Reporting**
- The tool result is `{ "delivery": "steer", "outcome": "in_turn" | "queued", "steer_id", "prompt_id"?, "reason"?, "worker": <snapshot> }`.
- `Get_Thread_Status` adds `steers` for the running and the last finished run: `steer_id` → `pending` | `injected` | `queued` (with `prompt_id`) | `discarded` | `uncertain`.
- Reporting does not depend on the fallback still waiting in `queue`.

## `Send_Thread_Message` gets `delivery`

- `delivery: "queue" | "steer"`, optional. Omitted means `"queue"`, exactly today's behavior. Any other value is `invalid_input`.
- `"steer"` calls `WorkerManager::steer`, keeps the `frame_worker_prompt` sender line, and emits `WorkerEvent::Prompted` marked "steer" or "steer, queued".
- Description text:

  > Send a message to an existing worker thread. `delivery: "queue"` (default) runs it as a new prompt after the worker's current and queued prompts, in order. `delivery: "steer"` adds it to the worker's current work: the worker reads it after its current tool calls finish. Steering does not stop running tools. When the worker is not running, or its turn ends before it reads the message, the message is queued instead, the same as `delivery: "queue"`. `outcome` says `in_turn` or `queued`; `Get_Thread_Status` reports what finally happened to each steer. Use `Cancel_Thread` to stop a worker; it also discards steers that were not read yet.

- The tool count stays six, and `crates/zdx-assets/subagents/orchestrator.md` is unchanged.
- Orchestrator prompt guidance:
  - steer to add detail to or correct a worker's current task;
  - queue for a new task;
  - `Cancel_Thread` to stop;
  - when trial worker updates appear mid-turn, re-check any action on that worker.

## Telegram bot

**Agent topics (in the bot process)**
- `BotContext` keeps a per-topic map of the running turn's `SteerInbox`, set when `spawn_agent_turn` starts and cleared after settlement.
- `dispatch_message` tries `try_push` for plain user messages before enqueueing.
- These keep today's path: commands, `/btw`, staged-session input, mirror topics, and synthetic messages.
- Text goes through the normal parse and `build_user_text` path, so replies and grouped content keep their framing.
- **Turn end, inside the same queue slot:**
  1. Finish the engine run.
  2. Await persistence.
  3. Deliver the answer.
  4. `close()`, `drain_unread()`, and clear the map. This step runs even if delivery failed or the turn failed.
  5. **Completed turn:** run the unread items as the next turn, with their original reply metadata. **User cancel:** answer "Not added (turn cancelled)" to each unread message.

**Orchestrator topics (in the service)**
- When the topic's thread is busy in the service, the bot submits the parsed message with `delivery: "steer"`, unfollowed and without a holder, through a new submit path. It does not reuse `run_service_turn`, whose follower waits for a turn that names the input. It does not take the topic's queue slot.
- The running turn keeps its own renderer and holder.
- The service answers `in_turn` or `queued`. A `queued` input is already in the service queue: the bot does not queue it again. When it runs, it is a service-started, unfollowed turn, and the existing bridge renders any routed, unfollowed turn (`crates/zdx-bot/src/orchestrator.rs:196`).
- The bridge handles `user_message_injected` (edit to "Added"), `TurnStarted` naming a steered input (edit to "runs as the next turn"), and `InputsDropped` (edit to "Not added") for these steers, even for turns it is not rendering. Acknowledgement state keeps an outcome that arrives before the acknowledgement is posted.
- Agent topics: unread steers of a finished turn run next in the same queue slot from one FIFO (a steer that misses its own fallback turn goes behind the ones already waiting).

**`/queue <text>`**
- New. It forces the normal queue in both kinds of topic.

**Acknowledgement**
- "↪️ Will be added to the running turn", edited to "↪️ Added to the running turn" on `UserMessageInjected`.
- The edit is keyed by steer id, so an injection that arrives before the acknowledgement is sent still ends in "Added".
- A fallback says nothing extra; it runs like a queued message.

**Voice**
- Transcribe outside the occupied queue slot, then steer the result. The target is the **thread**, as for workers: the message joins whatever turn of that thread is running when the transcript is ready, or it queues. A steer never needs a turn guard, because every turn of the thread keeps its context.
- **Agent topics:** look up the topic's current inbox after transcription. On `Closed` or no running turn, the parsed message goes to the bot queue through a new queue-item variant, so the audio is not downloaded or transcribed twice.
- **Orchestrator topics:** submit with `delivery: "steer"`. A `queued` reply means the service has already queued it; the bot does nothing more.

## TUI

- `spawn_agent_turn` opens a `SteerInbox` and keeps it beside the cancel token.
- The busy-submit path (`crates/zdx-tui/src/features/input/update.rs:1020`) gets a steer action.
  - A pending steer shows as a gray cell; `UserMessageInjected` turns it into a user cell.
  - On `Closed`, the message goes to the normal queue.
- On `TurnFinished`, `drain_unread()`, then clear or convert pending cells first, because pending user cells block the dequeue.
  - **Completed turn:** unread items go to the front of `input.queued`.
  - **Cancel:** they go back into the composer.

## Trial, measurement, and switch-off

**Logs**
- One `info` line per steer outcome: `Steer injected` or `Steer queued`, with thread, source, `wait_ms`, and reason.
- The replay script used for the evidence above can then compare the trial with the baseline.

**Switches**
- Worker updates: `[workers] update_delivery = "queue"`, then a service restart.
- Per message: `delivery: "queue"` (tool and service), `/queue` (Telegram), the queue key (TUI).

**Live check**
- Use a dev build and a temporary orchestrator thread.
- Do not replace the installed service, change the global config, or restart services without the user's approval.

# Phase 1: Engine safe point, in-memory inbox, and worker updates steered in the orchestrator service

- [x] `core/steer.rs`: `SteerInbox` (`try_push`, `take`, idempotent `close`, `drain_unread`) and `SteerSource`.
- [x] Agent loop:
  - take steers at the safe point;
  - append `phase: "steer"` messages with id and source;
  - emit `UserMessageInjected`;
  - injected count that resets and skips the loop aborts for that round.
- [x] Closing: a scoped close guard, plus an explicit `close()` before the terminal event in `finish_turn_early` and `finalize_non_tool_turn`.
- [x] Reasoning replay and boundaries:
  - `current_turn_start` and `tool_call_turn_replay` skip steers;
  - trajectory and replay-note guards;
  - serializer tests for Anthropic (thinking + `tool_use` + `tool_result` + steer), Responses, Chat Completions, and Gemini;
  - round-trip tests for consecutive steers, a later genuine user turn, and a pending `Posted` note.
- [x] Service:
  - an inbox on `Running`;
  - the admission rule (id allocated under the threads lock, steer ids added to `Running.inputs`);
  - worker-update steering behind `[workers] update_delivery` (`"steer"` | `"queue"`);
  - unread items drained and put back by input id at turn end, for completed, failed, and cancelled turns;
  - cancel disposition recorded on `Running` and applied once at settlement;
  - outcome logs.
- [x] Tests:
  - **Engine integration** (wiremock, temporary `ZDX_HOME` on the child process): a steer pushed during a slow tool is in the next request body after the tool result; a steer pushed during the final answer is still returned by `drain_unread()` after the engine closed the inbox.
  - **Runtime tests with the existing fake runner and workers:**
    - an update during a running turn goes into the inbox;
    - an update during a closing or idle turn is queued and batched as today;
    - unread updates are requeued in arrival order, also after a failed turn;
    - an update interleaved with a user submission keeps admission order;
    - push, close, and cancel races: `clear_queue: true` never reinserts dropped items; a cancel of an unrelated queued input leaves the inbox alone; `CancelTarget::Input` finds a steer the turn holds;
    - `update_delivery = "queue"` gives today's event sequence exactly;
    - settlement never reports idle with an unread update.
- [x] Docs: SPEC.md steering contract and the worker-update setting; `docs/ARCHITECTURE.md`; `crates/zdx-engine/AGENTS.md`.

**Gate (live check, needs approval to run against real providers):** a dev-build orchestrator turn with thinking on `claude-cli`, `openai-codex` (SSE and WS), and Gemini. A worker update arrives mid-turn, is injected, and the next request is accepted.

Gate result (2026-11-12, dev build, `zdx exec --steer-file`, steer pending at the first safe point, thinking high): `claude-cli:claude-opus-5-5`, `openai-codex:gpt-6-astra` (SSE), and `google-antigravity:gemini-3.8-flash-high` each accepted the request after the steer and answered it. Codex over WebSocket ran with a project overlay but the transport was not confirmed from logs. Not run live: the orchestrator service path (worker update steered into a real turn), Telegram, and the TUI; those are covered by tests only.

✅ **Demo**: in a temporary orchestrator thread on a dev build, a worker finishes while the orchestrator runs a tool. The orchestrator's next request carries the `[worker update]` as a steer, and the transcript shows it inside the same turn. With `update_delivery = "queue"`, the same scenario runs exactly as today.

# Phase 2: Orchestrator steers a worker's current work

- [x] `WorkerManager`:
  - pending list and run file;
  - `steer()` under the manager lock, with reasons `not_running`, `cancelling`, `unsupported_runner`, and `not_managed`;
  - `last_injected_seq` recorded directly in worker state from the child's `UserMessageInjected` events;
  - settlement before `Completed` (requeue or discard);
  - unresolved steers counted in `idle_for_requester`;
  - `steers` in status.
- [x] Child: hidden `zdx exec --steer-file <path>`, the `ChildFile` backend, and parent-side decoding of `UserMessageInjected` from the child's stdout events.
- [x] `Send_Thread_Message`: `delivery`, the steer branch, the description, and the prompt guidance.
- [x] Tests:
  - schema;
  - omitted and `"queue"` behave as today;
  - steer to an idle, unmanaged, or external worker queues with the right reason;
  - append error queues nothing and closes steering for the run;
  - partial line ignored;
  - a late steer is queued exactly once;
  - cancel before settlement discards even when the runner returns `Ok`;
  - a steer after cancel queues;
  - `all_idle` is never true before the requeue.

✅ **Demo**: a worker runs a long command. `Send_Thread_Message` with `delivery: "steer"` returns `in_turn`, and the worker's answer follows the detail; status shows `injected`. A steer sent during the worker's final answer runs once as its next prompt. A steer to an idle worker returns `queued` / `not_running`.

# Phase 3: Telegram text steers agent and orchestrator topics

- [x] Agent topics: the inbox map, `try_push` in `dispatch_message`, the exclusions, and the turn-end order in the same queue slot.
- [x] Orchestrator topics: `Submit.delivery` (an in-turn steer closes a followed connection), an unfollowed bot submit path for busy topics, the bridge acknowledging from `user_message_injected` / `TurnStarted` / `InputsDropped`, and `zdx orchestrator send --delivery steer`.
- [x] `/queue <text>`, the acknowledgement keyed by steer id, and "Not added" on cancel.
- [x] Tests pin that staged input, commands, `/btw`, mirror topics, and synthetic messages keep today's path.

✅ **Demo**: in an agent topic and in an orchestrator topic with a running turn, a text gets "Will be added", then "Added" after the tool round, and the final answer reflects it. `/queue` waits for the next turn.

# Phase 4: Telegram voice steers without waiting for the queue slot

- [x] Transcribe outside the slot, then steer to the thread (agent topics: the current inbox; orchestrator topics: `delivery: "steer"`), and the parsed-message queue variant for local `Closed` fallbacks.

✅ **Demo**: during a long tool call, a voice note is transcribed while the turn runs and added at the next tool-round boundary. If the turn ends first, the transcript runs next without a second download.

# Phase 5: TUI steers the running turn

- [x] Inbox per run, steer key action, pending and injected cells, turn-end ordering, and unread items to the queue front or back to the composer on cancel.

✅ **Demo**: during a long tool call, a steered "skip the tests" shows as a gray cell, becomes a user cell after the tool round, and the next response follows it.

# Decisions with defaults (not blocking)

| Choice | Recommended default | Why |
|---|---|---|
| `update_delivery` default in the trial build | `"steer"` | The user wants to try it and switch it off if it confuses the model. `"queue"` restores today exactly. |
| TUI key | Enter steers while busy, Tab queues | Same split as Codex. Implemented: while a turn runs, Tab with text queues (it used to insert spaces); Tab on an empty composer still cycles model modes, and when idle Tab still inserts spaces. |
| Telegram plain text to a busy topic | Steer, `/queue <text>` to force the queue | Matches Claude Code, Codex, and OpenCode. |
| Steer that arrives during the final answer | Next turn, after the answer | The bot delivers only `final_text`. The replay estimates 45 to 51% of busy-time messages hit this case, so revisit after the trial. |
| Unread user steers on a user cancel | Give them back ("Not added", composer) | A cancel should not start new work by itself. |
| Unread worker updates on a turn cancel | Requeue unless the queue is cleared | Results must not be lost. |
| Unread worker steers on `Cancel_Thread` | Discard and report | Cancel already clears queued prompts. |
| Images in steers | Not in the first version | Image bytes are not persisted. |

# Risks

- **Model confusion.** No data yet on how models react to a mid-turn update or detail. Mitigations: provenance text, the persisted steer marker, outcome logs, and per-source switches. The trial decides.
- **Stale actions remain.** Steering cannot change a request already in flight; the data shows most stale sends were decided then. The send result (`not_running`) and the next-safe-point update are the mitigation, not a guarantee.
- **Reasoning replay.** A wrong boundary can make Anthropic reject a request or break Gemini signatures. Mitigations: the marker, serializer tests, and the live gate.
- **Latency.** Long tool rounds delay steers (worker p90 170 s in the replay). Acknowledgements say "will be added" until injection.
- **Uncertain worker delivery.** A worker steer whose report stream ended early (crash, kill, failed stdout write) is reported as `uncertain` and not re-sent, so it is never processed twice; the orchestrator may need to resend it.
- **Ordering.** Unread items are put back by input id (service) or at the FIFO back (workers); tests pin both.

# Later

- **Continue after the final answer** (Codex style). Trigger: the trial confirms many steers miss the turn at the final answer.
- **Push worker updates already waiting in the queue into a running turn.** Trigger: stale actions from queued updates show up in the trial logs (2 of 22 in the replay).
- **Cut a text-only stream when a steer arrives.** Trigger: users wait on long final answers.
- **Live steering for external-runner workers.** Trigger: a launcher protocol with an input channel.
- **A `zdx steer` CLI for TUI or bot-agent runs.** Trigger: a real script needs it.
- **Image steers.** Trigger: a real need to show a screenshot mid-run.

# Open questions

- Whether `claude-cli` accepts a consecutive user message after a tool-results message, or needs the steer as a trailing block. The Phase 1 live gate answers this.
