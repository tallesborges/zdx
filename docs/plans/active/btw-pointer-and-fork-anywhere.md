> Stage: active. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- `/btw` opens a side thread that copies **no** transcript: just the user's question plus a pointer line naming the parent thread ID. The agent pulls context by calling `Read_Thread`.
- `/btw` and fork both work while a run is in flight, without stopping it.
- Fork has exactly one behavior on every surface: it opens a new tab (TUI) or a new conversation (Telegram) and never replaces the current one.
- Fork keeps copying a precise event prefix up to the chosen branch point.
- `/btw` exists on the Telegram bot as a new topic.

# Non-goals
- Fork-by-reply on the Telegram bot (moved to Polish round 2 — nice-to-have, needs new persistence).
- Changing what fork copies (still a `cells_to_events` prefix; still no `Usage`/`Meta` events).
- Adding a turn-range parameter to `Read_Thread`.
- Reimplementing `/handoff`, `/tldr`, or `/prompt-builder`.
- Prompt-caching or cost-accounting rework.

# Design principles
- User journey drives order.
- Reuse before rebuild: `Read_Thread` and handoff's lineage-pointer wording already exist and ship today.
- **btw = whole thread** (a pointer is sufficient). **fork = a cut point** (a copy is required, because `Read_Thread` can only read a whole thread). Keep the two distinct.
- Prefer simple and explicit: accept on-disk staleness rather than adding fallback layers (workspace alpha stance, `AGENTS.md`).

# User journey
1. Mid-run, the user asks a side question with `/btw`.
2. A new tab (TUI) or new topic (bot) opens empty — no transcript copied.
3. The agent calls `Read_Thread` on the parent thread when the question needs prior context, then answers.
4. The user asks follow-ups in that same side thread.
5. Separately, mid-run, the user opens the timeline and forks at a chosen turn into a new tab.

# Foundations / Already shipped (✅)

## `Read_Thread` tool
- What exists: `crates/zdx-engine/src/tools/read_thread.rs` takes `thread_id` + `goal`, loads the thread via `tp::load_thread_events`, renders it with `tp::format_transcript`, and runs a `no_tools` / `no_system_prompt` subagent on `read_thread_model` (default `gemini:gemini-3.1-flash-lite-preview`, `crates/zdx-engine/src/config.rs`). Returns response text only. Registered in the default tool registry at `crates/zdx-engine/src/tools/mod.rs:336`.
- ✅ Demo: ask the agent to `Read_Thread` a known thread ID.
- Gaps: reads the **whole** thread (no turn range), and reads **from disk only**.

## Handoff lineage pointer
- What exists: `crates/zdx-engine/src/core/handoff_generation.rs:64-103` — `build_lineage_note` builds `(Continuing from thread {id} — call read_thread for full context.)` plus a multi-ancestor `Lineage: ...` variant, and `build_handoff_prefix` (`:96`) prefixes it to the user's literal message.
- ✅ Demo: run `/handoff` in the TUI and read the first message of the new thread.
- Gaps: currently coupled to handoff generation; needs extracting so btw can reuse it.

## TUI btw tab machinery
- What exists: `/btw` command (`crates/zdx-tui/src/common/commands.rs:46`) → `OverlayRequest::Btw` → `build_btw_base_messages` + `create_btw_tab` (`crates/zdx-tui/src/update.rs:1268-1275`, `:1375-1386`, `:1414+`); `TabKind::Btw { base_messages }` (`crates/zdx-tui/src/state.rs:86-93`); first-send thread creation in `prepare_btw_tab_thread` (`crates/zdx-tui/src/runtime/handlers/agent.rs:130-196`).
- ✅ Demo: `/btw` mid-conversation opens a tab pre-loaded with the parent transcript.
- Gaps: copies the full parent history into the model's context **and** onto disk via `messages_to_events`.

## Timeline fork
- What exists: `crates/zdx-tui/src/overlays/timeline.rs` — `f` forks in place, `t` forks as a tab; both build a prefix with `cells_to_events`. `fork_thread_sync` (`crates/zdx-tui/src/runtime/handlers/thread.rs:401-448`) creates a new thread, appends the events, and rebuilds usage via `extract_usage_from_thread_events`.
- ✅ Demo: open the timeline, press `t`.
- Gaps: `f` is blocked mid-run by `tui.agent_state.is_running()` → `"Stop the current task first."` (`timeline.rs:156-163`). `t` already works mid-run.

## Bot handoff + topic creation
- What exists: staged `/handoff` creates a topic, pre-creates its thread with `handoff_from`, inherits the source model/thinking overrides, sets `pending_topic_title`, then sends a synthetic first message through the normal turn flow (`crates/zdx-bot/src/staging.rs:486-550`). Thread ID mapping is `telegram-{chat_id}-topic-{topic_id}` (`crates/zdx-bot/src/handlers/message/mod.rs:294-299`).
- ✅ Demo: `/handoff` inside a topic.
- Gaps: no `/btw` and no `/fork` in `BotCommand` / `COMMAND_DEFS` (`crates/zdx-bot/src/commands.rs:27-129`).

## Telegram reply plumbing
Relevant to Polish round 2 only; nothing in the MVP depends on it.
- What exists: raw `reply_to_message` is deserialized at `crates/zdx-bot/src/telegram/types.rs:69-74`, but is used only to recover a topic ID (`:84-92`).
- Gaps: `IncomingMessage` (`crates/zdx-bot/src/types.rs:3-14`) drops the reply target; `crates/zdx-bot/src/agent/mod.rs:39-67` persists only `ThreadEvent::user_message(text)`; `ThreadEvent::Message` (`crates/zdx-engine/src/core/thread_persistence/event.rs:128-145`) carries no Telegram IDs. **Reply → turn resolution must be built.**

# MVP phases (ship-shaped, demoable)

## Phase 1: TUI btw becomes a pointer — IMPLEMENTED (2026-07-30), manual demo pending
- **Goal**: `/btw` opens an empty tab whose first message is the user's question plus the parent-thread pointer line; the agent fetches context with `Read_Thread`.
- **Scope checklist**:
  - [x] Extract the lineage-note builder out of `handoff_generation.rs` into a shared helper that both handoff and btw call. Added `pub fn build_side_thread_seed(source_thread_id, message)` + private `join_message_and_note`; renamed `build_lineage_note`'s flag to `full_context` (btw passes `true` so the note asks for the full context instead of gaps "below"). Handoff behavior is byte-identical.
  - [x] Replace `TabKind::Btw { base_messages }` with `TabKind::Btw { parent_thread_id: Option<String> }`.
  - [x] Delete `build_btw_base_messages` and the transcript pre-population inside `create_btw_tab`.
  - [x] In `prepare_btw_tab_thread`, stop writing `messages_to_events(base_messages)`; instead prepend the shared lineage note to the user's first message (plain user message — see Key decisions).
  - [x] When the parent tab has no persisted thread, open a plain new tab with no pointer.
  - [x] Keep model/thinking override inheritance on the new thread.
- **✅ Demo**: mid-conversation, `/btw` + "what files did we touch?" → a new empty tab, the agent calls `Read_Thread`, answers correctly; the parent tab is untouched; the new thread's JSONL contains only btw turns.
- **Risks / failure modes**:
  - The agent answers a referential question without calling `Read_Thread`.
  - `Read_Thread` reads disk, so an in-flight parent turn may not be flushed yet and is missing from the transcript (accepted — see Key decisions).

## Phase 2: Fork always opens a new tab, and works mid-run — IMPLEMENTED (2026-07-30), manual demo pending
- **Goal**: collapse fork to a single behavior — fork always creates a new tab (TUI) / new conversation (bot) and never replaces the current one. In-place fork is deleted, which removes the mid-run guard's reason to exist.
- **Scope checklist**:
  - [x] Collapse the two timeline key handlers into one. `fork_effect` and `fork_as_tab_effect` were identical except for the effect variant — kept `fork_effect`. Kept the `f` binding, dropped the `t` binding; hint now reads `f  fork to new tab`.
  - [x] Remove the `tui.agent_state.is_running()` guard. With no in-place fork there is nothing to splice a streaming turn into. (Verified: this was fork's only mid-run guard — the `runtime/mod.rs` one is the thread-picker switch guard, unrelated.)
  - [x] Delete the duplicate effect: kept `UiEffect::ForkThread` with as-tab semantics, deleted `ForkThreadAsTab` and its runtime handler.
  - [x] Have `fork_thread_sync` emit `ThreadUiEvent::OpenAsTab` directly; deleted `ThreadUiEvent::ForkedLoaded` and `map_forked_to_tab`.
  - [x] Delete the now-dead in-place fork handler: `handle_thread_forked` and `struct ThreadForked` plus its match arm.
  - [x] Preserve two behaviors from `handle_thread_forked`: the `"Forked from turn {n}."` marker is now appended as a `HistoryCell::system` cell inside `fork_thread_sync` (after `history` is collected, so it never pollutes input history), and the `user_input` prefill rides on `OpenAsTab`.
  - [x] Keep the `TaskKind::ThreadFork` re-entrancy guard.
- **✅ Demo**: start a long run, open the timeline, fork at a chosen turn — a new tab opens with history up to that turn, the input is prefilled if you forked at a user turn, and the original run keeps streaming to completion in its own tab.
- **Risks / failure modes**:
  - Losing the `"Forked from turn {n}."` marker or the input prefill during the collapse.
  - Muscle memory: `t` no longer forks.

## Phase 3: `/btw` on the bot — IMPLEMENTED (2026-07-30), manual demo pending
- **Goal**: `/btw` inside a topic creates a new topic that answers from the parent thread via `Read_Thread`.
- **Scope checklist**:
  - [x] Add `Btw` to `BotCommand`, `COMMAND_DEFS`, and the native command menu (`crates/zdx-bot/src/commands.rs`). `blocks_topic_autocreate: true`, so from General it reports "must be used inside a topic" instead of auto-creating one.
  - [x] Two-step staging like handoff: `/btw`, then the question as text or voice (`StagingCommand::Btw`).
  - [x] On input, open the topic immediately — no preview, no Accept tap. `start_btw_topic` builds the seed, drops the staging session, and calls the shared `seed_new_topic`, which creates the topic, pre-creates the thread with `handoff_from = source_thread_id`, inherits model/thinking overrides, sets `pending_topic_title`, and dispatches the seed through the normal queue. `/handoff` Accept now reuses `seed_new_topic` too.
  - [x] Send the question + pointer line as the synthetic first message. No `handoff_model` generation call: the seed is `build_side_thread_seed(thread_id, question)`, built locally.
  - [x] Thread the dispatch capability down to the staging flow: `handle_message` and `handle_staging_flow` now take `&Arc<BotContext>` + `&ChatQueueMap` so a new topic's first turn goes through the real per-topic queue instead of a detached task.
- **✅ Demo**: mid-run in a topic, `/btw` + "what did we decide about X?" → a new topic answers; the original topic is untouched and its run is uninterrupted.
- **Risks / failure modes**:
  - Staging session collides with `/handoff` or `/prompt-builder` — handled: starting any staged command replaces the existing session for that topic.
  - Topic creation failure leaves the staging session in place so the next message retries, with the error shown behind a Discard button.

# Contracts (guardrails)
- Fork copies a precise event prefix. `Read_Thread` must never be used to reconstruct a fork.
- Fork never replaces or mutates the tab/conversation it was invoked from — it only ever creates a new one.
- Fork/btw context reconstruction emits no `Usage` or `Meta` events — keep the `cells_to_events_never_emits_usage_or_meta` guard test (`crates/zdx-tui/src/overlays/timeline.rs`).
- A btw or forked thread never writes into its parent's JSONL.
- Forked and btw threads start untitled and generate their own title (`SetTitle(None)`, `crates/zdx-tui/src/features/thread/update.rs:376`).
- New side threads inherit the parent's model and thinking overrides.
- `/btw` and fork never interrupt, cancel, or mutate the parent's running turn.

# Key decisions (decide early)
- **The pointer rides in the first user message as plain text.** Two alternatives were researched against live provider docs (2026-07-30) and both rejected:

  **Alternative A — append to the top-level system prompt.** Rejected:
  1. **Breaks prompt caching.** `build_system_blocks` (`crates/zdx-providers/src/anthropic/shared.rs:151-170`) sets `cache_control` on every system block, and BP1 (last system block) caches "system prompt + AGENTS.md context. Reused across threads with the same config" (`crates/zdx-providers/src/anthropic/mod.rs:9-16`). Anthropic hashes the prefix in order `tools` → `system` → `messages`, so any change to the top-level `system` field misses the cache for the system prompt and every cached message after it. A per-thread system suffix makes the system prompt unique per btw thread — cache miss on the largest cached block, partly cancelling this plan's savings.
  2. **Does not survive resume.** The system prompt is rebuilt from config/AGENTS.md on load and is never persisted in the thread JSONL, so a system-injected pointer disappears when the btw thread is resumed. A user-message prefix persists forever.

  **Alternative B — a `role: "system"` message inside the messages array.** This is genuinely supported now, but not for this use case:
  - **Anthropic native**: supported on Claude Fable 5, Claude Mythos 5, Claude Opus 4.8, and Claude Opus 5 (no beta header), but **not** on Claude Sonnet 5. Critically, a `system` message **cannot be the first entry in `messages`** — it must immediately follow a `user` turn and precede an `assistant` turn or end the array; wrong placement returns 400. The btw pointer is inherently the *first* thing in a brand-new thread, which is exactly the banned placement.
  - **Gemini**: `contents` accepts only `user` and `model` roles; `systemInstruction` is top-level and its `role` field is ignored. No mid-conversation system role at all.
  - **OpenAI-compatible**: `system`/`developer` messages are accepted anywhere in the array.
  - So supporting it would mean adding a `system` role to `ChatMessage` plus a per-provider capability matrix and a downgrade path for Gemini/Sonnet — real complexity for a placement Anthropic forbids anyway.

  **Resolution**: plain user message, no new format. Reuse the lineage note handoff already ships — `(Continuing from thread {id} — call read_thread for full context.)` (`crates/zdx-engine/src/core/handoff_generation.rs:64-88`) — extracted into a shared helper. It works identically on every provider, is persisted in the thread JSONL, and survives resume. Wording refinement (including whether a delimited `<side_thread>` tag reads better than the parenthetical) is deferred to Polish round 1; Phase 1 ships the existing line as-is.
- **Fork always opens a new tab / new conversation. In-place fork is deleted.** This is a simplification, not a workaround: one fork behavior across both surfaces (TUI tab, Telegram topic), and it removes the entire class of "streaming turn spliced into a replaced transcript" bugs that the mid-run guard existed to prevent. It also deletes a duplicated effect, event, mapper, handler, and keybinding (see Phase 2).
- **btw staleness is accepted.** `Read_Thread` reads from disk, so a not-yet-flushed in-flight turn will be missing. No fallback layer, no partial in-memory splice.
- **btw with no parent thread** opens a plain new tab with no pointer line.
- **Bot message-ID mapping storage shape** (new `ThreadEvent` field vs. sidecar index) is deferred with Polish round 2. Nothing in the MVP depends on it, so it no longer gates Phase 1-3.

# Testing
- Manual smoke demos per phase.
- Keep the existing `cells_to_events_never_emits_usage_or_meta` guard test.
- [x] Regression test asserting a btw tab copies no parent transcript/messages and records the parent thread id (`btw_tab_copies_no_parent_transcript_or_messages`, `btw_tab_without_parent_thread_carries_no_pointer` in `crates/zdx-tui/src/update.rs`).
- [x] Regression test asserting the btw seed asks for the *full* context rather than gaps below (`side_thread_note_asks_for_full_context`, `side_thread_seed_leads_with_the_user_message` in `crates/zdx-engine/src/core/handoff_generation.rs`).
- [x] Bot regression tests: `parse_btw_command` (parsing, topic-blocking, no queue bypass) in `crates/zdx-bot/src/commands.rs`; `btw_preview_says_it_answers_in_a_new_topic` and `only_topic_seeding_commands_require_a_forum_topic` in `crates/zdx-bot/src/staging.rs`.

# Polish rounds (after MVP)

## Polish round 1: pointer wording
Phase 1 ships handoff's existing parenthetical verbatim. Only revisit the wording if the demo shows the agent making wrong tool-call decisions.
- Tune the pointer line so the agent reliably calls `Read_Thread` for referential questions and skips it for self-contained ones. Candidates: keep the parenthetical, or switch to a delimited `<side_thread parent_thread_id="...">` block so it reads less like the user's own words.
- ✅ Check-in demo: five mixed questions (referential vs. self-contained), correct tool-call decision each time.

## Polish round 2: Fork-by-reply on the bot
Nice-to-have, explicitly deferred out of the MVP. The TUI already covers fork-at-a-point via the timeline; Telegram does not, and closing that gap needs new persistence rather than reuse.
- **Goal**: reply to an earlier message in a topic and fork the conversation from that point into a new topic.
- **Why it is not MVP**: the bot deserializes `reply_to_message` (`crates/zdx-bot/src/telegram/types.rs:69-74`) but drops it — `IncomingMessage` (`crates/zdx-bot/src/types.rs:3-14`) has no reply field, `crates/zdx-bot/src/agent/mod.rs:39-67` persists only `ThreadEvent::user_message(text)`, and `ThreadEvent::Message` (`crates/zdx-engine/src/core/thread_persistence/event.rs:128-145`) carries no Telegram IDs. There is no message-ID ↔ thread-event mapping to reuse, so this is a persistence-format change, not a feature wiring change.
- **Scope checklist**:
  - [ ] Carry `reply_to_message.message_id` through `parse_incoming_message` into `IncomingMessage` (`crates/zdx-bot/src/ingest/mod.rs:48-85`).
  - [ ] Settle the storage shape (new `ThreadEvent` field vs. sidecar index) — this is the decision that gates the rest.
  - [ ] Persist a Telegram-message-ID ↔ thread-event association for both incoming user messages and outgoing bot answers.
  - [ ] Decide how a multi-message or media answer maps to one logical turn.
  - [ ] Add `/fork`: resolve replied message → event index → copy the JSONL prefix into a new thread, reusing the `fork_thread_sync` shape.
  - [ ] Create the topic and bind the forked thread, reusing the Phase 3 topic-creation path.
- **✅ Check-in demo**: scroll up in a topic, reply to an old message, `/fork` → a new topic whose history ends exactly at that message.
- **Risks / failure modes**:
  - Threads created before this ships have no recorded message IDs, so fork-by-reply only works for messages sent afterwards.
- **Revisit trigger**: when branching a Telegram conversation mid-history becomes a recurring need, or when Phase 3's `/btw` proves the topic-creation path is solid enough to build on.

# Later / Deferred
- **Mid-conversation system messages for queued user input during a run.** Separate feature, surfaced while researching this plan. Anthropic documents this exact pattern: when the user sends a message while the agent is mid-task, relay it after the tool result as `{"role": "system", "content": "The user sent the following message while you were working: ..."}`, phrased as context rather than a command. This placement (after a `user`/tool-result turn, mid-array) is the *supported* one, unlike btw's first-message case, and it is explicitly cache-safe: appending after the cached prefix does not change the prefix hash. Would require adding a `system` role to `ChatMessage` plus a per-provider capability matrix (Anthropic: Fable 5 / Mythos 5 / Opus 4.8 / Opus 5 only, not Sonnet 5; OpenAI-compatible: fine; Gemini: unsupported, needs a `user`-turn downgrade). Revisit if queued-input handling during long runs becomes a real pain point.
- A turn-range parameter for `Read_Thread`, which would let fork use a pointer too. Revisit if whole-thread btw reads become expensive on very long threads.
- Backfilling Telegram message IDs for threads created before Polish round 2.
