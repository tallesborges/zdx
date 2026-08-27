> Stage: active. Keep this plan current while working: check completed scope items, and mark the phase done with its demo date. This file is the source of truth.

# Goals
- Let the user run `/goal`, then provide the objective as text or voice, and remove it with `/goal_clear` in both the TUI and Telegram.
- After each agent turn is persisted, ask a verifier agent whether the thread has completed the goal.
- Give the verifier only the thread ID, goal, and evaluation instructions; the verifier uses `Read_Thread` to inspect the latest work and evidence.
- When incomplete, start another agent turn from the verifier's `next_action`. When complete, stop and show the verifier's reason.
- Bound autonomous execution with a hard continuation limit.

# Non-goals
- No `Get_Goal`, `Create_Goal`, or `Update_Goal` model tools.
- No transcript or tool-result assembly in the scheduler; `Read_Thread` is the verifier's only history input.
- No token budget or per-goal usage accounting; the continuation limit is the MVP safety bound.
- No pause/resume controls, statusline UI, goal history, multiple goals, sub-goals, or independent deterministic checks.
- No durable goal state machine. Goal state is process-local; ending the process ends the run.
- No surface-specific goal dashboards or progress UI; the TUI and Telegram share the same minimal commands and completion messages.
- No verifier-model picker; use the configured `oracle` subagent so the verifier has `Read_Thread` access.

# User journey
1. The user runs `/goal` in a TUI thread or Telegram topic.
2. ZDX enters goal-input mode and asks for the objective; the user sends text or a voice note, like the existing `/btw` and `/handoff` flows.
3. ZDX stores that input as the active goal and starts the first normal agent turn from it.
4. After the turn is persisted, ZDX asks the verifier agent to inspect that thread.
5. If the verifier says the goal is incomplete, ZDX automatically starts the next turn with its recommended next action.
6. ZDX repeats until the verifier confirms completion or the continuation limit is reached.
7. ZDX displays the completion or limit reason on the originating surface and returns to normal turn-by-turn use.

# Phase 1 — A bounded verifier-driven goal loop
- [ ] Hold goal state in memory only — objective, continuation count, and latest verifier reason — keyed per thread in the bot's chat state and in the TUI runtime. The goal is not its own persisted event type.
- [ ] Treat process lifetime as the goal's lifetime: a bot restart, TUI exit, thread reopen, or fork ends the run, and no autonomous work resumes on its own. This is the safety property that replaces an explicit re-arm step; do not add a durable `active` flag that could restart work nobody asked for.
- [ ] Add `[goals] enabled = false` and `max_continuations = 10` to config. Goal mode remains opt-in.
- [ ] Add `/goal` and `/goal_clear` to the TUI command palette and Telegram command registry. `/goal` accepts no inline objective; `/goal_clear` stops further verification and continuation.
- [ ] Reuse the existing staged-input pattern from Telegram `/btw` and `/handoff`: `/goal` enters a per-topic pending session, prompts `Send your goal as text or a voice note, or /cancel to abort`, and consumes the next transcribed voice or text message as the objective. `/cancel` exits without changing goal state.
- [ ] Give goal staging its own acceptance cleanup: the accepted objective message becomes the first user turn, so it must survive. Existing staging records every staged input in `user_message_ids` and deletes them on cleanup (`staging.rs`); goal acceptance removes only the command and prompt artifacts. `/goal_clear` also cancels a pending capture session so the next message is not swallowed.
- [ ] Keep goal staging in the current topic's normal serial queue because accepting the objective writes to and starts work in that same thread; unlike `/btw`, it must not bypass the queue.
- [ ] In the TUI, `/goal` opens a pending goal composer using the same input area; submitting stores the objective and starts the first turn, while Escape cancels without changing goal state.
- [ ] Store the submitted objective as the in-memory active goal, reset its continuation count, and also use the submitted text as the first ordinary user turn so the agent begins work immediately.
- [ ] Verify only after a `Completed` turn. `Failed` ends the goal with a notice, and an interrupted turn ends it too, so provider errors and explicit cancellation can never produce more autonomous work. Commands that run no agent turn do not schedule verification.
- [ ] Schedule verification only once the turn is on disk. `TurnFinished` means the agent stopped emitting events, not that persistence finished; the bot's `await_persisted()` barrier is that signal, and the TUI scheduler must await its own persist handle before invoking the verifier.
- [ ] Carry `run_id` plus the thread revision the verifier inspected. Accept a verdict only when both still match, allow one verification in flight at a time, and invalidate outstanding verifiers whenever a real user turn, `/goal_clear`, cancellation, or a replacement objective lands. This is what makes clearing and cancellation correct rather than racy.
- [ ] Run the verifier through `run_exec_subagent` rather than the model-facing `Invoke_Subagent` tool: resolve the configured `oracle` model and thinking level, inherit that subagent's declared tools, pass a cancellation token and a finite timeout, and reject a verdict when `Read_Thread` was never called. Give it only the thread ID, the objective, and the evaluation contract. Inheriting the profile lets the verifier corroborate the thread against the workspace with read-only tools; it must never receive `bash`, `edit`, or `write`, because a verifier that can modify the work it grades can make a goal true instead of reporting it false.
- [ ] Parse the entire trimmed response as strict JSON — no fence stripping, no prose recovery — into `completed`, `reason`, and `next_action`, requiring a non-empty `next_action` exactly when incomplete. Cap objective, reason, and next-action lengths before rendering or persisting so a verdict cannot exceed Telegram's message limit. A malformed or failed verification stops the loop and reports the failure instead of guessing.
- [ ] If `completed` is true, drop the in-memory goal and show `Goal completed: <reason>`.
- [ ] If `completed` is false and the cap remains, increment the in-memory continuation count and start a `GoalContinuation` turn from `next_action`.
- [ ] Persist each `GoalContinuation` as an ordinary user `ThreadEvent::Message` carrying the objective, `round/max`, and `next_action`, so it replays to the provider like any other turn. It does not count as user input for queue priority or the continuation cap; that bookkeeping lives in the in-memory goal state.
- [ ] Add a `NoticeKind::Goal` variant and record every terminal outcome — completion, continuation limit, verifier failure, manual clear, cancellation — as a `ThreadEvent::Notice`, which already persists and renders on reload without replaying to the provider. Together with the goal-round turns, the thread stays readable as its own goal log after the in-memory state is gone.
- [ ] Before starting a continuation, give queued real user input priority. `/goal_clear` and cancellation prevent any pending verifier result from starting another turn.
- [ ] Enqueue Telegram continuations as a synthetic `telegram::Message`, reusing the pattern `seed_new_topic` already uses to dispatch fabricated messages (`staging.rs`). `QueueItem` carries a `Message` and nothing else, so this ships without reshaping the turn pipeline; the synthetic id must not collide with real message ids, and the status and cancel machinery keyed on it needs its own identity rather than a reply anchor to a message Telegram never saw.
- [ ] Run verification outside the serial worker as a cancellable task and re-enter the queue only to admit a continuation, so a slow verifier never blocks inbound messages. Between turns, queued inbound Telegram messages take priority over autonomous continuation.
- [ ] Reuse the same in-memory goal model, verifier contract, completion rules, and continuation cap across TUI and Telegram; keep only command parsing, queue adaptation, and result presentation surface-specific.
- [ ] On Telegram, post `Goal completed: <reason>`, verifier failure, or continuation-limit status to the originating topic and leave ordinary bot message handling unchanged when no goal is active.
- [ ] When `max_continuations` is reached, stop and show the verifier's latest reason and next action.
- [ ] Add focused regression tests for verifying only after `Completed` turns, persisted-turn scheduling, stale-verdict rejection via `run_id`/revision, strict verifier-output parsing, continuation replay as an ordinary user message, terminal-outcome notices, a reloaded thread starting no continuation, TUI and Telegram user-input priority, completion, clearing/cancellation, malformed verifier output, and the continuation cap.

✅ **Demo**: Enable goals and run `/goal` once in the TUI and once in a bound Telegram topic. Submit `Make the focused test pass` through the TUI composer and as a Telegram voice note. On both surfaces, ZDX stores the transcribed objective, starts the first turn, persists each result, invokes an Oracle verifier that calls `Read_Thread`, and automatically continues from `next_action` while incomplete. It stops with `Goal completed: <reason>` when the thread contains evidence that the test passes, or stops at 10 continuations. A new TUI or Telegram message is handled before another continuation, and each continuation is visible in the transcript as an ordinary user turn that replays on the next request. Killing the bot mid-goal ends the run: reopening the thread shows the goal-round turns written so far and starts no further continuation, and a terminal notice appears only for outcomes reached before the process died.

# Validated before building
- The verifier contract works against real threads. Five trials on a live thread and two synthetic adversarial threads returned correct verdicts, citing specific commits, exit codes, and test summaries.
- Oracle returns bare parseable JSON when the prompt demands it, despite its own mandated output format, so a system-prompt override is a safeguard rather than a prerequisite.
- Assistant claims are correctly ignored: a thread ending in `✅ all 52 tests are green` over a run that exited 1 was rejected, as was a success claim backed by no tool call at all.
- That rejection depends on tool output no longer being truncated to 500 bytes head-only; the decisive evidence sat at the tail of the output.
- Cost per goal at the current cap is roughly 33 model runs — 11 executor, 11 verifier, 11 `read_thread` helpers — before the executor's own tool loop.

# Later
- Replace the synthetic Telegram continuation message with an explicit turn origin (`HumanMessage` vs `GoalContinuation`) once goal mode earns the refactor. That removes the fabricated message id and makes human priority an explicit queue rule rather than a property of the pipeline believing a user spoke.
- Stop early when the verifier returns the same `next_action` twice in a row, if repeated non-progressing continuations turn out to waste the cap in real use.
- Add durable goal state plus an explicit re-arm step when goals need to survive a restart or span sessions. That is the point to introduce a persisted phase, kept separate from a process-local armed flag so reload and fork can never resume work on their own.
- Add `/goal pause|resume|status` and status UI when users need to manage goals that span interactive sessions.
- Add verifier/model selection when one fixed Oracle profile is measurably too slow or expensive.
- Add token budgets when continuation count alone does not provide enough cost control.
- Add deterministic checks or a second verifier when model-only completion produces false positives in real use.

# Open questions
- None blocking. The verifier contract, evidence quality, and false-positive resistance were measured before building; the Telegram continuation ships as a synthetic message with the turn-origin refactor deferred.
- Untested: ambiguous or partially-complete objectives, where the correct verdict is not binary. Watch for verifier drift there first.