# Goals
- The agent can ask the user a mid-run question in the interactive TUI (`zdx`), with a selectable option picker, and continue the same run with the answer — feature parity with the Telegram bot's `ask_user_question`.
- Assistant replies in the TUI can end with follow-up suggestions the user can select to send as their next message — parity with the bot's `<followups>` buttons.

# Non-goals
- No engine *behavior* changes: `ask_user_question` stays a surface-registered tool (never advertised to exec/subagents). Pure shared data/helpers in `zdx-engine` are in scope (see Key decisions).
- No Telegram bot behavior changes — the bot only switches imports to the shared helpers; its tests keep passing unchanged.
- No persistence of pending questions or follow-up suggestions across TUI restarts.
- No multi-question batches, no multi-select.
- `zdx exec` / headless / subagents do not get the tool.

# Design principles
- User journey drives order: the blocking question flow ships first (it unblocks the model), follow-ups after (typing is cheap in a terminal, so suggestion buttons are polish there).
- Mirror the bot's proven design (pending map + oneshot + `question_registered` marker event) instead of inventing a new mechanism.
- Respect the TUI's Elm/MVU architecture (docs/ARCHITECTURE.md): mutations in reducers, side effects as `UiEffect`, overlays as modal state in `AppState.overlay`.

# User journey
1. User asks for something ambiguous in the TUI; the model calls `ask_user_question`.
2. A question picker appears with the options; the user picks one with arrows + Enter (or dismisses it and types a free-form answer).
3. The run continues with the answer; the Q&A is visible in the transcript tool cell.
4. At the end of a reply, the model may offer follow-up suggestions; the user selects one and it is sent as their next message.

# Foundations / Already shipped (✅)
## Bot-side feature (reference implementation)
- What exists: `crates/zdx-bot/src/ask_user.rs` (tool definition, pending map keyed by surface, handler that awaits a oneshot, `REGISTERED_MARKER` via `ToolOutputDelta`, drop-guard cleanup) and `crates/zdx-bot/src/followups.rs` (`extract_followups` tag parsing).
- ✅ Demo: ask the Telegram bot to use the tool.
- Gaps: keying is Telegram-specific (`parse_telegram_thread_id`); the TUI needs its own keying (TUI thread ids are UUIDs — `thread_persistence.rs:963`).

## TUI seams the plan builds on
- What exists:
  - Tool config: `TuiState::with_history` builds `ToolConfig::default()` (`crates/zdx-tui/src/state.rs:362-371`).
  - Event loop: `update.rs::handle_agent_event` → `features/transcript/update.rs` handles `ToolInputCompleted`/`ToolOutputDelta`/`ToolCompleted`.
  - Overlays: modal, key-first routing, work mid-turn (`overlays/mod.rs`, `update.rs:1592-1626`); `thinking_picker.rs` is the closest template.
  - Submission: `input::build_send_effects` (idle) and `InputState::enqueue_prompt` (mid-turn) (`features/input/update.rs`).
  - Instruction layer: `CHAT_INSTRUCTION_LAYER` injected in `crates/zdx-tui/src/lib.rs:29-37`.
- ✅ Demo: existing pickers (`/model`, thinking) and tool cells in any TUI session.

# MVP slices (ship-shaped, demoable)

## Slice 1: TUI-local `ask_user_question` tool + typed answers (ugly but functional)
- **Goal**: the model can ask and the user can answer by typing — no picker UI yet.
- **Scope checklist**:
  - [x] Extract the pure surface-neutral pieces into `zdx-engine` (e.g. `zdx_engine::tools::ask_user_question`): `TOOL_NAME`, `REGISTERED_MARKER`, `QuestionInput`/`OptionInput`, `definition()`. Switch `crates/zdx-bot/src/ask_user.rs` to import them (no behavior change; bot tests stay green).
  - [x] New `crates/zdx-tui/src/ask_user.rs`: pending map keyed by `ctx.current_thread_id` string (UUID; works per tab/thread), mirroring the bot's mechanics: `PendingQuestion { tool_use_id, sender, option_labels }`, drop guard, marker emission after registration. Pending-map mechanics stay per-surface for now (generic abstraction deferred).
  - [x] `TuiState::with_history`: build `ToolRegistry::builtins().with_tool(...)` + `ToolSelection::Auto { base: ToolSet::Default, include: vec![TOOL_NAME] }` instead of `ToolConfig::default()` (mirror `zdx-bot/src/lib.rs:101-112`). Interactive chat TUI only.
  - [x] Input interception: in the mid-turn Enter path (`features/input/update.rs:1001-1050`), if a pending question exists for the active tab's thread, resolve it with the typed text instead of `enqueue_prompt`.
  - [x] Cleanup: clear pending entries for a thread on `TurnFinished`/interrupt/tab close (guard already handles engine-side abort).
- **✅ Demo**: in `just run`, send "use ask_user_question to ask me which fruit I prefer, 3 options" → tool cell shows the running call (options visible in tool input), type "banana" + Enter → tool result contains the answer, run continues. Esc interrupt mid-question leaves no stuck state.
- **Risks / failure modes**:
  - Mid-turn Enter currently always queues; the interception must only trigger when a question is pending for the *active tab*.
  - The `question_registered` marker chunk will appear as raw tool output noise (fixed in Slice 2).

## Slice 2: Question picker overlay
- **Goal**: options are selectable with arrows + Enter, like the bot's buttons.
- **Scope checklist**:
  - [x] New `crates/zdx-tui/src/overlays/question_picker.rs` modeled on `thinking_picker.rs`: shows question, option labels + descriptions; Enter resolves the pending entry with the selected label; Esc closes the overlay only (question stays pending, typed answers still work — the "Other" path).
  - [x] `update.rs::handle_agent_event`: stash `ToolInputCompleted` input for `ask_user_question`; on the matching `ToolOutputDelta == REGISTERED_MARKER`, open the overlay (active tab only) and suppress the marker from reaching `transcript.append_tool_output_delta_for`.
  - [x] Close the overlay automatically when the question resolves (typed answer or `ToolCompleted`) or the turn ends.
- **✅ Demo**: same prompt as Slice 1 → picker opens automatically → arrow + Enter → answer lands in the tool result and the run continues. Esc → type a custom answer → same outcome. Marker no longer visible in tool output.
- **Risks / failure modes**:
  - Background-tab events must not steal the overlay; key by tab/thread and only open for the active tab.
  - Overlay state must stay a pure state machine: Enter returns an overlay update/mutation handled by the reducer path — never resolve the oneshot from inside render/overlay internals.

## Slice 3: Follow-up suggestions — render + strip
- **Goal**: TUI replies can end with follow-up suggestions, displayed instead of leaking raw tags.
- **Scope checklist**:
  - [x] Add followups guidance to `chat_instruction_layer.md` (TUI-appropriate wording: suggestions are optional next messages; no no-op suggestions).
  - [x] Move `extract_followups` (pure string parsing) from `crates/zdx-bot/src/followups.rs` into `zdx-engine` (e.g. `zdx_engine::followups`); bot and TUI both call it.
  - [x] Strip the block from the visible assistant cell at `AssistantCompleted`/`TurnFinished` (`features/transcript/update.rs:39-57,112-151`) and render suggestions as a compact transcript cell (e.g. `💡 1. … 2. …`).
- **✅ Demo**: ask "give me 2 follow-up suggestions after your answer" → reply renders clean, suggestions cell lists them, no raw `<followups>` tags in the transcript.
- **Risks / failure modes**:
  - Tags arrive via streaming deltas, not just final text — strip at finalize, accept transient raw text during streaming (ugly-but-functional), or hold back trailing partial tags.
  - Decide whether thread-persisted messages keep the tags (bot persists them today — keep consistent).

## Slice 4 (optional polish): Follow-up selection
- **Goal**: pick a suggestion and send it as the next user message. Typing is cheap in a terminal — implement only if selecting proves genuinely useful after dogfooding Slice 3.
- **Scope checklist**:
  - [ ] Keybinding on the suggestions cell or a small picker overlay (reuse `question_picker` rendering) listing the last reply's suggestions.
  - [ ] Selection invokes the normal submission path (`input::build_send_effects`) when idle; replace/clear stale suggestions when a new turn starts.
- **✅ Demo**: after a reply with suggestions, trigger the picker, select one → it appears as the user message and a new turn starts.
- **Risks / failure modes**:
  - Don't fork a second "send" code path — selection must produce the same effects as typing + Enter.

# Contracts (guardrails)
- Telegram bot behavior is unchanged (its tests keep passing: `cargo nextest run -p zdx-bot`).
- Engine/tool registry contract unchanged: the tool is advertised only on surfaces that register it; `zdx exec`/subagents never see it.
- Cancellation never leaves a stuck pending question (engine `abort_all` + drop guard, plus TUI cleanup on TurnFinished/tab close).
- MVU discipline: reducers mutate, runtime executes effects, overlays stay modal state machines.

# Key decisions (decide early)
- **Keying**: pending questions keyed by raw `ctx.current_thread_id` string (UUID). No parsing — simpler than the bot, and naturally multi-tab safe.
- **Marker reuse (Oracle-confirmed)**: keep the bot's `ToolOutputDelta`/`question_registered` mechanism. The race is real even in-process: `ToolInputCompleted` fires during streaming, the handler registers only at execution; an early overlay answer would fall into the mid-turn prompt queue and never drain (the turn is blocked waiting for the answer). TUI must intercept the marker before transcript display.
- **Share pure data, duplicate mechanics**: `TOOL_NAME`, `REGISTERED_MARKER`, input structs, `definition()`, and `extract_followups` move to `zdx-engine` (pure data/string logic both surfaces already depend on — copying guarantees drift). Pending-map mechanics, keying, rendering, callbacks, and overlay/input integration stay per-surface (sharing those would be premature abstraction).

# Testing
- Manual smoke demo per slice (commands above).
- Unit tests mirroring the bot's: pending-map resolve/stale-id/guard, `extract_followups` parsing, marker emission ordering (the bot's `execute_emits_registered_marker_then_returns_answer` pattern).
- No new integration-test harness; transcript/overlay logic verified via existing reducer-level test patterns in `zdx-tui`.

# Polish phases (after MVP)
## Phase 1: UX refinement
- Status-line hint while a question is pending on an inactive tab.
- Recommended-option preselected in the picker.
- ✅ Check-in demo: question on background tab shows a hint; picker opens with the recommended option highlighted.

# Later / Deferred
- Generic shared `PendingQuestionMap`/guard abstraction — trigger: the per-surface mechanics start drifting or a third surface appears.
- Multi-select / multi-question batches — trigger: real usage demand.
- Followup persistence in thread history rendering on reload — trigger: complaints about stale suggestion cells after restart.
