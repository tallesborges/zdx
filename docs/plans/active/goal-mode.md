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
- [ ] Add minimal per-thread goal persistence to the JSONL event log: objective, `active|completed`, continuation count, and verifier reason. Goal events must not render, replay as chat messages, or contribute searchable text.
- [ ] Add `[goals] enabled = false` and `max_continuations = 10` to config. Goal mode remains opt-in.
- [ ] Add `/goal` and `/goal_clear` to the TUI command palette and Telegram command registry. `/goal` accepts no inline objective; `/goal_clear` stops further verification and continuation.
- [ ] Reuse the existing staged-input pattern from Telegram `/btw` and `/handoff`: `/goal` enters a per-topic pending session, prompts `Send your goal as text or a voice note, or /cancel to abort`, and consumes the next transcribed voice or text message as the objective. `/cancel` exits without changing goal state.
- [ ] Keep goal staging in the current topic's normal serial queue because accepting the objective writes to and starts work in that same thread; unlike `/btw`, it must not bypass the queue.
- [ ] In the TUI, `/goal` opens a pending goal composer using the same input area; submitting stores the objective and starts the first turn, while Escape cancels without changing goal state.
- [ ] Persist the submitted objective as the active goal, reset its continuation count, and also use the submitted text as the first ordinary user turn so the agent begins work immediately.
- [ ] After `TurnFinished`, wait until the current turn checkpoint is flushed before scheduling verification so `Read_Thread` sees the latest assistant response and tool evidence.
- [ ] Invoke the configured `oracle` subagent with only the thread ID, active goal, and this contract: call `Read_Thread`, judge completion from evidence in the thread, and return strict structured output with `completed`, `reason`, and `next_action` when incomplete.
- [ ] Parse and validate the verifier response. A malformed or failed verification stops the loop and reports the failure instead of guessing.
- [ ] If `completed` is true, persist the completed state and show `Goal completed: <reason>`.
- [ ] If `completed` is false and the cap remains, persist the incremented continuation count and start a typed `GoalContinuation` turn using `next_action` as an ephemeral instruction.
- [ ] Keep `GoalContinuation` separate from the user prompt queue: it must not persist, render, export, index, or replay as a user-authored message, and it must not trigger title generation.
- [ ] Before starting a continuation, give queued real user input priority. `/goal clear` and cancellation prevent any pending verifier result from starting another turn.
- [ ] Route each Telegram verification/continuation step through the existing per-topic serial queue as separate work rather than looping inside one message handler. Between turns, queued inbound Telegram messages take priority over autonomous continuation.
- [ ] Reuse the same persisted goal projection, verifier contract, completion rules, and continuation cap across TUI and Telegram; keep only command parsing, queue adaptation, and result presentation surface-specific.
- [ ] On Telegram, post `Goal completed: <reason>`, verifier failure, or continuation-limit status to the originating topic and leave ordinary bot message handling unchanged when no goal is active.
- [ ] When `max_continuations` is reached, stop and show the verifier's latest reason and next action.
- [ ] Add focused regression tests for goal-event restoration, post-flush verifier scheduling, strict verifier-output parsing, TUI and Telegram user-input priority, completion, clearing/cancellation, malformed verifier output, and the continuation cap.

✅ **Demo**: Enable goals and run `/goal` once in the TUI and once in a bound Telegram topic. Submit `Make the focused test pass` through the TUI composer and as a Telegram voice note. On both surfaces, ZDX stores the transcribed objective, starts the first turn, persists each result, invokes an Oracle verifier that calls `Read_Thread`, and automatically continues from `next_action` while incomplete. It stops with `Goal completed: <reason>` when the thread contains evidence that the test passes, or stops at 10 continuations. A new TUI or Telegram message is handled before another continuation, and no synthetic continuation appears as a user message in the transcript, export, or thread search.

# Later
- Add `/goal pause|resume|status` and status UI when users need to manage goals that span interactive sessions.
- Add verifier/model selection when one fixed Oracle profile is measurably too slow or expensive.
- Add token budgets when continuation count alone does not provide enough cost control.
- Add deterministic checks or a second verifier when model-only completion produces false positives in real use.

# Open questions
- None for the MVP. The verifier is an Oracle subagent with `Read_Thread`; autonomous work is bounded by `max_continuations`.