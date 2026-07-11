# Goals
- Bring the TUI's input-taking slash commands to the Telegram bot, starting with `/handoff`.
- Add a memory-only **staging flow** for commands that need free-form input: send the command, then send the input (text **or** audio); the bot generates a suggestion the user can Accept, Discard, or regenerate. Nothing touches the real thread until Accept. This replaces the TUI's "type command → keep typing in the composer" flow, which doesn't map to Telegram.
- Add a `/commands` picker that mirrors the TUI's context-dependent command palette: custom `.md` commands (bundled + `$ZDX_HOME/commands` + project `.zdx/commands`) plus agent built-ins (handoff, prompt-builder, tldr) — discoverable and launchable from Telegram.
- Reuse the existing engine handoff/prompt-builder/tldr generation instead of reimplementing it in the bot.

# Non-goals
- Reimplementing the TUI's built-in UI commands (tabs, overlays, pickers) on Telegram. In scope: custom `.md` commands and the agent built-ins handoff (firm), prompt-builder (likely), tldr, and btw (maybe).
- Persisting the transient staging state across bot restarts (MVP keeps it in memory).
- Changing the TUI command behavior.
- Inline-button-driven multi-step wizards beyond what already exists (model/thinking keyboards stay as-is).

# Design principles
- User journey drives order: handoff ships first because it's the firm want.
- Reuse before rebuild: move the small handoff/prompt-builder wrapper logic from `crates/zdx-tui/src/runtime/` into `zdx-engine` so both TUI and bot call one implementation. The heavy lifting (`run_exec_subagent_with_cancel`, prompt templates in `zdx_engine::prompts`, `zdx_engine::zdx_context::build_zdx_context`) already lives in the engine.
- Don't pollute the real thread: command input and suggestions are staged in bot memory; generation runs as an isolated helper subagent. The thread only changes on Accept.
- Telegram flow ≠ TUI flow: input arrives as separate messages, so commands enter a staging state with Accept/Discard buttons instead of reading a live composer.

# User journey (staging model)
The command interaction is **staged in the bot's memory** and never touches the real thread until the user accepts. Handoff and prompt-builder share the exact same flow.
1. User taps `/` in Telegram (or sends `/help`) and sees the available commands, including handoff / prompt-builder / btw.
2. User sends `/handoff` (or `/prompt-builder`) → bot enters a memory-only staging mode and asks for input.
3. User sends a normal message (text or voice note). The bot consumes it as the command input (not a normal agent turn), generates a suggestion, and posts it with **Accept / Discard** buttons. Nothing is written to the real thread.
4. **Send another message instead of tapping** = implicit "not this, try again": the bot deletes its previous suggestion and regenerates from the new input. Staging mode stays active.
5. **Discard** → the bot deletes the staging messages (clean UI); the real thread is untouched and staging mode ends.
6. **Accept** →
   - **Handoff**: create a new topic seeded with the generated context (`handoff_from` linked), then clean up the staging messages in the source thread.
   - **Prompt-builder**: use the generated prompt as the user's real message → run a normal agent turn (first thing that touches the thread).

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Bot slash-command framework
- What exists: `crates/zdx-bot/src/commands.rs` centralizes parsing (`parse_command`, `COMMAND_DEFS`, `telegram_command_specs`) and `crates/zdx-bot/src/handlers/message/commands.rs` holds handlers. Routing order is in `crates/zdx-bot/src/handlers/message/mod.rs`: `handle_pre_agent_commands` → `handle_thread_setup_commands` → `run_agent_turn`.
- ✅ Demo: send `/status` or `/model` in a bound chat; command intercepts before an agent turn.
- Gaps: no handoff/btw/prompt-builder; no staging ("awaiting input" / pending-command) state.

## Inline buttons, message edit/delete, and callback handling
- What exists: `crates/zdx-bot/src/followups.rs` already implements the exact staging pattern needed — post a message with an inline keyboard, handle the tap via `answer_callback_query`, then `delete_message` the message. Client methods `edit_message_text` / `edit_message_text_raw` / `delete_message` / `answer_callback_query` live in `crates/zdx-bot/src/telegram/mod.rs`; callbacks are dispatched in `crates/zdx-bot/src/lib.rs`.
- ✅ Demo: end-of-turn follow-up buttons appear, tap dispatches a new turn and removes the suggestion.
- Gaps: no Accept/Discard/regenerate flow yet; `delete_message` on the **user's** messages requires the bot to have the `can_delete_messages` admin right (impossible in DMs — bots can only delete their own messages there).

## Telegram native command menu
- What exists: `telegram_command_specs()` feeds `set_my_commands` at `crates/zdx-bot/src/lib.rs:93` (client method `set_my_commands` in `crates/zdx-bot/src/telegram/mod.rs`).
- ✅ Demo: the `/` menu in Telegram lists the registered commands.
- Gaps: new commands must be added to the spec list to appear.

## Engine handoff + prompt-builder generation
- What exists: `zdx_engine::core::subagent::run_exec_subagent_with_cancel` with `ExecSubagentOptions`; templates `HANDOFF_PROMPT_TEMPLATE` / `PROMPT_BUILDER_PROMPT_TEMPLATE` via `zdx_engine::prompts`; `zdx_engine::zdx_context::build_zdx_context`. TUI wraps them in `crates/zdx-tui/src/runtime/handoff.rs` and `runtime/prompt_builder.rs` (thread transcript via `thread_persistence::format_transcript`, lineage via `handoff_from`).
- ✅ Demo: `/handoff` and `/prompt-builder` already work in the TUI.
- Gaps: the wrapper fns (build prompt, build lineage/prefix, run subagent) are TUI-crate-local; the bot can't call them without duplication.

## Audio transcription intake
- What exists: `crates/zdx-bot/src/transcribe.rs` + ingest already transcribe voice notes into `incoming.text` before routing (`handlers/message/mod.rs` audio preprocessing).
- ✅ Demo: send a voice note in a bound chat; it's transcribed and drives a turn.
- Gaps: none — the two-step flow can rely on the transcript already being present on the follow-up message.

# MVP phases (ship-shaped, demoable)

## Phase 1: Shared generation in the engine — DONE (2026-07-09)
- **Goal**: Move the handoff (and prompt-builder) wrapper logic into `zdx-engine` so both TUI and bot call one path. No behavior change for the TUI.
- **Scope checklist**:
  - [x] Add an engine module — shipped as `zdx_engine::core::handoff_generation` (matching the existing `title_generation`/`tldr_generation` convention) exposing `generate_handoff(thread_id, next_message, handoff_model, root, cancel) -> Result<String>` that builds the prompt, resolves lineage/prefix, and runs the subagent.
  - [x] Add `zdx_engine::core::prompt_builder_generation::generate_prompt_builder(intent, model, root, cancel) -> Result<String>`.
  - [x] Rewire `crates/zdx-tui/src/runtime/handoff.rs` and `runtime/prompt_builder.rs` to call the engine functions (kept the `UiEvent` wrapping in the TUI; both are now thin adapters).
  - [x] Update `crates/zdx-tui/AGENTS.md` and `crates/zdx-engine/AGENTS.md` for the new module paths.
- **✅ Demo**: `just ci-fast` clean; `cargo nextest run -p zdx-engine` 428 passed and `-p zdx-tui` 317 passed; the 10 prefix/template tests moved with the code and pass under `core::handoff_generation` / `core::prompt_builder_generation`.
- **Risks / failure modes**:
  - Lineage/prefix logic is TUI-flavored (`UiEvent`); keep the engine fn returning plain strings and leave surface-specific formatting in each caller.

## Phase 2: Staging flow + `/handoff` in the bot — IMPLEMENTED (2026-07-09), manual demo pending
- **Goal**: `/handoff` works end-to-end on Telegram via the memory-only staging flow, with Accept / Discard / regenerate. The source thread is never polluted.
- **Scope checklist**:
  - [x] Per-thread staging store: `crates/zdx-bot/src/staging.rs` (`StagingMap` in `BotContext`, keyed by `thread_id`; tracks suggestion text/message id, bot + user staging message ids, 15-min TTL).
  - [x] `Handoff` registered in `COMMAND_DEFS` + `telegram_command_specs()` (native `/` menu).
  - [x] On `/handoff`: enters staging, asks for the starter message (Discard button + `/cancel`); outside a forum topic it explains it needs one; in General it replies in place (no topic auto-create).
  - [x] Staging intercept in `handle_message` (after setup commands, before `run_agent_turn`): input (text or voice transcript) is consumed, never persisted to the thread; generation runs via `zdx_engine::core::handoff_generation::generate_handoff`.
  - [x] Suggestion posted as an editable preview with ✅ Accept / 🗑 Discard inline buttons (`stg:` callbacks, `followups.rs` pattern).
  - [x] Regenerate on new message: previous suggestion deleted, new preview generated; staging stays active.
  - [x] Discard / `/cancel`: deletes bot staging messages (always) + user messages (best-effort); source thread untouched.
  - [x] Accept: creates topic (`Handoff <ts>`), pre-creates the thread with `handoff_from` (new engine setter `Thread::set_handoff_from`) + pending auto-title, cleans up staging messages, and dispatches the handoff prompt as a synthetic first message into the new topic (runs the first turn there).
- **✅ Demo (pending manual run)**: In a bound group, `/handoff` → send a voice note → tap Accept → a new topic appears seeded with the handoff context, `handoff_from` linked, and the source thread has no leftover turn. Sending a second message before accepting regenerates the suggestion. Discard leaves a clean source thread.
- **Verified so far**: `just ci-fast` clean; `cargo nextest run -p zdx-bot -p zdx-engine` 461/461 (new tests: `/handoff` parsing/blocking/queue, staging expiry, preview escaping/truncation). `docs/SPEC.md` §16 documents the staged handoff contract.
- **Risks / failure modes**:
  - Deleting the user's messages needs `can_delete_messages`; degrades gracefully (delete only the bot's messages) when unavailable.
  - Interaction with the per-chat queue (`crates/zdx-bot/src/bot/queue.rs`) and topic auto-create (`is_topic_blocking_command`) — staging interception must slot in before both.
  - Concurrency: staging is scoped by full `thread_id` (chat+topic).

## Phase 3: `/commands` picker — project commands + native built-ins — IMPLEMENTED (2026-07-09), manual demo pending
- **Goal**: two clean surfaces instead of one mixed picker (design settled 2026-07-09): the **native Telegram `/` menu** carries the stable agent built-ins (`/handoff`, `/tldr`, later `/prompt-builder`, `/btw`), and **`/commands`** is purely the dynamic project/context picker.
- **Command surfaces**:
  - **Native `/` menu (typed commands)**: `/tldr` posts a recap of the current topic's thread via the shared `zdx_engine::core::tldr_generation::generate_tldr` (read-only, `Config::tldr_model`, bypasses the queue like `/status`). `/handoff` was already native (Phase 2).
  - **`/commands` picker (custom `.md` commands only)**: discovered via `zdx_engine::custom_commands::load_custom_commands(cwd, builtin_names)` with `cwd` = the thread root override or the chat's profile root. Includes bundled (`/plan`, `/execute-plan`, `/investigate`, `/review-loop`), user (`$ZDX_HOME/commands/*.md`), and project (`.zdx/commands/*.md`) commands. Tapping one dispatches its `content` as a normal agent turn (synthetic dispatch, `followups.rs` pattern).
- **Scope checklist**:
  - [x] `/commands` posts the picker (`crates/zdx-bot/src/command_picker.rs`): custom commands sorted project → user → bundled; body lists name/description/source; inline buttons namespaced `cmd:{idx}` indexing into a per-message `CommandPickerMap` registry (64-byte callback-data limit respected); one-shot with Dismiss.
  - [x] Tap on a custom command → dispatches its `content` as a new turn in the current topic (picker edited to `▶️ /name`).
  - [x] `/tldr` typed handler (`handle_tldr_command` in `handlers/message/commands.rs`): placeholder message edited into the recap or error; registered in the native menu; bypasses the queue.
  - [x] Custom `.md` commands are **picker-only**: not typed commands, not in the native `/` menu — `/commands` is their single entry point on the bot.
  - [x] `/commands` + `/tldr` registered in `telegram_command_specs()`; `native_command_names()` feeds the shadowing rule (built-ins always win).
- **✅ Demo (pending manual run)**: in a bound project group, `/commands` lists only `.md` commands (`/plan`, project commands); tapping `/plan` runs the plan prompt as a turn; typing `/tldr` posts a topic recap; the native `/` menu shows `handoff`, `tldr`, `commands`.
- **Verified so far**: `cargo clippy -p zdx-bot` clean; `cargo nextest run -p zdx-bot` 36/36 (picker body has no built-ins, keyboard index order + dismiss, truncation, `/tldr` parsing + queue bypass). `docs/SPEC.md` §16 documents the picker + `/tldr` contracts. Note: full workspace `just ci-fast` is currently blocked by unrelated uncommitted `zdx-types`/`zdx-engine` changes from a concurrent session (not this plan's work).
- **Risks / failure modes**:
  - Callback-data 64-byte limit forces an id→command lookup registry per picker message (stale-entry handling like `followup_map`).
  - Custom commands that expect arguments/input run as-is for MVP; input-taking custom commands could reuse the staging flow later (deferred).
  - Command name collisions with registered bot commands (`/new`, `/status`, …): built-ins always win (same rule as the TUI's `builtin_names` shadowing).
  - Very long custom-command lists may need pagination or source filters in the picker — deferred until it hurts.

## Phase 4: `/prompt-builder` on the bot — IMPLEMENTED (2026-07-09), manual demo pending
- **Goal**: Generate a prompt from a short intent on Telegram, reusing the same staging flow.
- **Scope checklist**:
  - [x] `/prompt_builder` registered as a typed native command (patterns `/prompt-builder`, `/prompt_builder`, `/promptbuilder`; Telegram menu names can't contain hyphens).
  - [x] Wired through the staging flow (`StagingCommand::PromptBuilder` in `crates/zdx-bot/src/staging.rs`): intent = the input message (text or voice transcript); works in topics and DMs (no forum-topic requirement, unlike handoff); blocked in `General`.
  - [x] Calls `generate_prompt_builder` from Phase 1 with the thread's model override (or the chat default model), mirroring the TUI.
  - [x] Regenerate on new message; Discard / `/cancel` cleanup — shared staging behavior, unchanged.
  - [x] **Accept**: the generated prompt is dispatched as the user's real message in the current topic (normal agent turn); the preview message is kept and edited to `▶️ Prompt accepted — running…` so the turn's reply anchor stays valid; other staging messages are cleaned up.
- **✅ Demo (pending manual run)**: `/prompt_builder` → "a bug-investigation loop with Oracle" → preview with Accept/Discard → Accept runs it as a real turn in the same topic; a second message before accepting regenerates.
- **Verified so far**: `cargo clippy -p zdx-bot` clean; `cargo nextest run -p zdx-bot` 37/37 (new tests: `/prompt-builder`/`/prompt_builder` parsing, prompt preview wording). `docs/SPEC.md` §16 documents the contract.
- **Risks / failure modes**:
  - Accept auto-runs the prompt (no composer to park an editable draft in) — confirmed decision; to tweak, send a new message to regenerate instead.

## Phase 5: `/btw` on the bot (maybe)
- **Goal**: Ask a side question against the current thread context without committing it to the main thread.
- **Scope checklist**:
  - [ ] Reuse the btw / thread-question logic (`docs/plans/done/btw-transcript-reuse.md`, `thread-question-tool.md`) to answer using the thread transcript as context.
  - [ ] Two-step flow: `/btw`, then the question (text/audio); reply without appending to main thread history.
- **✅ Demo**: mid-conversation, `/btw` + "what files did we touch?"; bot answers without polluting the thread.
- **Risks / failure modes**:
  - Deciding what "doesn't pollute the thread" means for bot persistence (separate ephemeral thread vs read-only pass).

# Contracts (guardrails)
- Existing bot commands (`/new`, `/status`, `/model`, `/thinking`, `/whereami`, `/worktree`, `/exit`) keep working unchanged.
- TUI `/handoff` and `/prompt-builder` behavior is unchanged after the Phase 1 refactor.
- A message consumed as staging input MUST NOT trigger a normal agent turn and MUST NOT be persisted to the real thread.
- Staging never writes to the real thread until Accept. Discard leaves the source thread exactly as it was.
- Staging state is per-`thread_id`; it never leaks across chats/topics.
- Message deletion degrades gracefully: always delete the bot's own staging messages; delete the user's messages only when permitted, never erroring the flow when it isn't.

# Key decisions (decide early)
- **Prompt-builder Accept semantics (CONFIRMED):** Accept runs the generated prompt as the user's real message (a normal agent turn). Telegram has no composer to park an editable draft in; to tweak, the user sends a new message (regenerate) instead.
- **Accept / Discard / regenerate:** two buttons (Accept, Discard); sending a new message while a suggestion is shown = regenerate (replace previous suggestion). No `/skip` — an empty handoff isn't part of this model.
- **Staging storage:** in-memory in `BotContext` for MVP (short-lived; lost on restart is acceptable). Add a timeout + `/cancel`.
- **Clean-UI deletion scope:** delete the bot's suggestion messages always; delete the user's input messages best-effort (requires `can_delete_messages`; not possible in DMs).

# Testing
- Manual smoke demos per phase (in a bound test group).
- Regression tests only for contracts: command parsing (`commands.rs` test module already exists), and "message-as-input is not double-processed as a turn."

# Polish rounds (after MVP)

## Polish round 1: UX affordances
- Timeout + auto-clear for a stale staging session; clear `/cancel` messaging.
- ✅ Check-in demo: start `/handoff`, wait past timeout, send a normal message — it runs as a normal turn, not as staging input.

# Later / Deferred
- Persisting staging state across restarts — revisit if restarts during a staging session become common.
- Bringing additional TUI commands (e.g. variants, save-as-command/skill) to Telegram — revisit if requested.
- Oracle review of the Phase 1 engine refactor before promoting this plan to `active/`.
