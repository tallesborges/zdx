> Stage: drafts. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- When an idle persisted TUI thread is at or above 85% of its model context window, submitting the next plain-text message automatically generates a handoff, creates a linked thread in a new tab, and starts that message there without requiring `/handoff` or a second Enter.
- Keep the source thread's persisted transcript unchanged and retain its full lineage so the new assistant can recover omitted context with `read_thread`.
- Preserve the existing normal-send and manual `/handoff` journeys when automatic handoff is not eligible.
- Prove the behavior with reducer-level tests and an end-to-end TUI smoke demo.

# Non-goals
- In-thread summarization or compaction events.
- Provider-native compaction APIs.
- Automatic handoff in Telegram, bot, CLI exec, automations, or subagent runs.
- Automatic handoff for image-bearing submissions or queued prompts in the MVP.
- User-configurable thresholds in the MVP.

# Design principles
- User journey drives order.
- Reuse the existing handoff generator, linked-thread metadata, tab flow, and normal send effects.
- Keep the policy in `zdx-tui`; keep `zdx-engine` UI-agnostic.
- Preserve data before optimizing convenience: generation cancellation and thread-creation failure must leave recoverable input.
- Use one explicit automatic/manual mode through the async flow instead of inferring intent from incidental state.

# User journey
1. The user works in a persisted TUI thread until the latest request reaches 85% of the active model's context limit.
2. The user types and submits the next plain-text message normally.
3. ZDX shows the existing handoff-generation state while keeping the source thread intact.
4. ZDX opens a new tab, creates a thread linked through `handoff_from`, and sends the generated handoff prompt automatically.
5. The new assistant continues the task and can call `read_thread` on the source or its ancestors when the generated context omitted something.

# Foundations / Already shipped (✅)

## Context usage and model limits
- What exists: `ThreadUsage` tracks latest-request context separately from cumulative cost and exposes `context_tokens()` / `context_percentage()` in `crates/zdx-tui/src/features/thread/state.rs:110-229`. Model context limits come from `ModelOption.context_limit` in `crates/zdx-engine/src/models.rs:88-89`.
- ✅ Demo: the TUI input bar already renders context-window percentage from these values in `crates/zdx-tui/src/features/input/render.rs:599-628`.
- Gaps: the input reducer does not receive current usage, and no automatic-handoff eligibility policy exists.

## Manual handoff generation
- What exists: `UiEffect::StartHandoff` reaches the cancellable handoff task through `crates/zdx-tui/src/runtime/mod.rs:1057-1079`; `generate_handoff()` loads the persisted transcript, includes the literal next message and lineage, and runs the helper model in `crates/zdx-engine/src/core/handoff_generation.rs:123-160`.
- ✅ Demo: `/handoff` generates an editable continuation prompt using the source transcript.
- Gaps: `HandoffState::Generating` does not retain the original message or distinguish manual and automatic flows (`crates/zdx-tui/src/features/input/state.rs:49-69`).

## Linked threads and source-preserving tabs
- What exists: `HandoffSubmit` opens a fresh tab in `crates/zdx-tui/src/update.rs:1799-1814`; runtime creates the thread through `thread_create()` in `crates/zdx-tui/src/runtime/handlers/thread.rs:313-352`; `Thread::new_with_root_and_source()` persists `handoff_from`.
- ✅ Demo: the existing regression test `handoff_submit_opens_new_tab_and_preserves_source_tab` in `crates/zdx-tui/src/update.rs:2213-2307` verifies the source tab remains intact.
- Gaps: tab preparation currently happens only in the keyboard-input path, so a `HandoffSubmit` emitted by an async result would bypass it.

## New-thread initialization and normal send
- What exists: `ThreadUiEvent::Created` initializes a new thread in `crates/zdx-tui/src/features/thread/update.rs:110-126,291-326`; `build_send_effects()` persists the user event, appends the user cell/message, starts the agent, and optionally suggests a title in `crates/zdx-tui/src/features/input/update.rs:1248-1339`.
- ✅ Demo: creating a thread and pressing Enter runs a normal persisted first turn.
- Gaps: `initial_input` is only placed in the composer. There is no explicit create-and-send mode.

# MVP phases (ship-shaped, demoable)

## Phase 1: Automatic context handoff for direct text submissions
- **Goal**: Complete the full daily-usable journey: one normal Enter near the context limit generates a handoff, creates a linked thread in a new tab, and starts the continuation automatically without mutating the source transcript.
- **Scope checklist**:
  - [ ] Add a small pure eligibility helper near the input submission logic in `crates/zdx-tui/src/features/input/update.rs:20-69,876-959`. Use a code constant of 85%, latest-request `ThreadUsage::context_tokens()`, and the active model's `ModelOption.context_limit`.
  - [ ] Pass the active thread usage into `InputContext` from `crates/zdx-tui/src/update.rs:1779-1797`; return false for no persisted thread, unknown/zero context limit, empty text, pending images, modal activity, slash commands, bash commands, or busy task/agent state.
  - [ ] Intercept eligible text after existing busy/modal guards but before normal-send mutation in `submit_input()` (`crates/zdx-tui/src/features/input/update.rs:910-1017`). Do not append or persist the new user message in the source thread.
  - [ ] Introduce one explicit handoff mode (`Manual | Automatic`) and carry it through `UiEffect::StartHandoff`, `TaskMeta::Handoff`, `UiEvent::HandoffResult`, and the handoff result reducer (`crates/zdx-tui/src/effects.rs:171-200`, `crates/zdx-tui/src/common/task.rs:38-52`, `crates/zdx-tui/src/events.rs:277-284`, `crates/zdx-tui/src/update.rs:177-185`).
  - [ ] Expand the generating handoff state to retain the original next message and mode before clearing the composer (`crates/zdx-tui/src/features/input/state.rs:49-69`). Esc during automatic generation restores that message and returns to idle; manual cancellation/review behavior remains unchanged (`crates/zdx-tui/src/features/input/update.rs:500-521,1343-1377`).
  - [ ] Keep manual generation success unchanged: put the generated prompt in the composer and wait for review. On automatic success, emit `HandoffSubmit` immediately without writing the generated prompt into the source composer.
  - [ ] Centralize the small “prepare a fresh tab for `HandoffSubmit`” reducer step so both keyboard-produced and async-result-produced submit effects preserve the source tab (`crates/zdx-tui/src/update.rs:177-185,1799-1814`). Do not create a generic effect middleware abstraction.
  - [ ] Carry an explicit `auto_send` flag through `HandoffSubmit`, `thread_create()`, and `ThreadUiEvent::Created` (`crates/zdx-tui/src/runtime/mod.rs:1081-1095`, `crates/zdx-tui/src/runtime/handlers/thread.rs:313-352`, `crates/zdx-tui/src/events.rs:84-91`).
  - [ ] For `auto_send`, initialize the new linked thread and then reuse `build_send_effects()` to persist the generated user message, append it once, start the agent once, and trigger normal first-turn title suggestion. Do not also prefill the composer (`crates/zdx-tui/src/features/thread/update.rs:110-126,291-326`).
  - [ ] Keep `initial_input` draft behavior unchanged for manual handoff. Fresh-thread usage is zero, but the auto-send path must bypass eligibility checks explicitly rather than relying on that as a re-entry guard.
  - [ ] Preserve recoverable input on every failure: generation failure/cancel restores the original message on the source tab; thread-creation failure carries the generated prompt back in `ThreadUiEvent::CreateFailed` and places it in the active composer for retry instead of losing it (`crates/zdx-tui/src/features/thread/update.rs:137-146`).
  - [ ] Add a short TUI system message while automatic handoff generation is active so the tab change is explainable; reuse the existing handoff visual state in `crates/zdx-tui/src/features/input/render.rs:466-487` rather than adding an overlay.
- **✅ Demo**: set up reducer state with a persisted thread, a model context limit of 100 tokens, and latest-request usage of 85 tokens; submit `continue the work` once. Observe `StartHandoff(Automatic)` with no source `SaveThread`/user-cell mutation, feed a successful `HandoffResult`, then a successful `ThreadUiEvent::Created`; a new tab contains a linked thread whose first message is persisted and whose agent turn starts automatically. Repeat at 84 tokens and observe the exact existing normal-send effects.
- **Risks / failure modes**:
  - Async `HandoffResult` bypasses today's keyboard-local tab setup. Centralizing the existing small tab-preparation step prevents the source tab from being overwritten.
  - The helper succeeds but thread creation fails. Carrying recovery input in `CreateFailed` prevents the generated prompt and original intent from disappearing.
  - The generated prompt is appended or sent twice. The created-thread reducer must choose exactly one path: draft for manual, `build_send_effects()` for automatic.
  - A custom model has no context limit. Eligibility fails closed, preserving normal send behavior.
  - Automatic generation is cancelled. State-owned original input restores the draft without changing the source JSONL.

# Contracts (guardrails)
- Thread JSONL remains append-only; no existing source events or metadata are rewritten (`docs/SPEC.md:128-178`).
- The submitted next message must not be persisted in both source and destination threads.
- The automatic destination thread must persist `handoff_from` and keep the full lineage/read-thread behavior from `crates/zdx-engine/src/core/handoff_generation.rs:26-104`.
- Manual `/handoff` remains reviewable and requires explicit Enter after generation.
- Below 85%, unknown context limit, unpersisted threads, image-bearing input, slash commands, bash commands, and queued prompts retain current behavior.
- Generation and create failures must leave either the original message or generated prompt recoverable in a composer.
- Effects remain explicit and I/O-free reducer boundaries remain intact (`docs/ARCHITECTURE.md:51-112`).

# Key decisions (decide early)
- **Threshold**: a fixed 85% code constant for MVP. Add configuration only if dogfooding shows one threshold does not fit the supported models.
- **Usage basis**: latest committed request context tokens, not cumulative thread usage and not a new provider token-count request.
- **Trigger point**: direct idle text submission before source persistence. Do not hand off reactively after a provider overflow.
- **Mode routing**: explicit `Manual | Automatic` data carried through effects/events. Do not infer mode from whether the composer is empty or from task timing.
- **Automatic first turn**: reuse `build_send_effects()` after linked thread creation. Do not route the generated prompt back through the regular eligibility gate.
- **Recovery after create failure**: keep the generated prompt in the failed destination tab's composer. It already contains the user's literal next message and source lineage.

# Testing
- Add pure policy tests at 84%, 85%, and above 100%, plus no-thread, zero-limit, image, slash, and bash cases near `crates/zdx-tui/src/features/input/update.rs`.
- Add reducer tests proving automatic interception emits no source `SaveThread` or source user-cell/message mutation, while below-threshold submission remains unchanged.
- Extend `crates/zdx-tui/src/update.rs:2213-2307` coverage for async auto-success tab creation and source-tab preservation.
- Add created-thread tests proving automatic input is persisted/appended/sent exactly once and manual initial input remains a draft.
- Add failure/cancellation tests proving original or generated input is recoverable and the source JSONL receives no new user message.
- Run `cargo nextest run -p zdx-tui`, then `just ci-fast`; run `just test` because the change affects user-visible thread behavior.
- Manually smoke-test `just run` with a test model/context limit that crosses the threshold, then verify both thread files and `handoff_from` with `zdx threads show <id>`.

# Polish rounds (after MVP)

## Polish round 1: Queue-aware automatic handoff
- Route a queued text prompt through the same eligibility policy when `TurnFinished` updates usage, preserving active/background `TabContext` behavior in `crates/zdx-tui/src/update.rs:390-429,616-656`.
- If eligible, start handoff instead of draining through `build_send_effects_for_tab`; preserve queue order and attached data without moving work to the wrong tab.
- ✅ Check-in demo: queue a text prompt while a near-limit turn is running in a background tab; when the turn finishes, ZDX creates and continues in a linked tab without sending the queued prompt to the old thread.

# Later / Deferred
- Image-bearing automatic handoff. Revisit when the handoff generation and linked-thread first-turn payload can carry attachments without dropping or duplicating them.
- User-configurable threshold or enable/disable setting. Revisit after dogfooding the 85% default across small and million-token models.
- Telegram/bot automatic handoff. Revisit after the TUI policy and recovery behavior prove reliable.
- In-thread compaction or provider-native compaction. Revisit only if linked handoffs create unacceptable workflow fragmentation.