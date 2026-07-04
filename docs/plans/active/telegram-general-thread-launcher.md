# Goals
- Give the Telegram bot's **General** topic a fast, always-reachable launcher to spin up a **new thread pre-set to a chosen model**, without typing `/model` and setting overrides by hand.
- Support **favorite presets** by **reusing the existing top-level `[[favorites]]` config** (`ModelFavorite`: `alias`, `model`, `thinking`) already used by the TUI Tab-cycle — each tapping straight into a ready-to-type new thread.
- Support a **🎛 Custom** button that opens the existing provider → model picker but targets a *new* thread.
- Support a **🔄 Continue** button that resumes an existing thread (e.g. one started on the desktop) inside a fresh General topic, so cross-device work continues from Telegram.
- Keep the launcher as the **last message in General** so it's always available at the bottom.

# Non-goals
- No launcher in DMs or inside topics — General only.
- No `ReplyKeyboardMarkup` / persistent bottom keyboard (unreliable in supergroups; chat-wide, leaks into topics).
- No thinking-level picker inside the Custom flow for v1 (favorites may still carry a thinking level from config).
- No change to how normal General messages auto-create topics.
- No new "plain new thread" button (sending any message already does this).

# Design principles
- User journey drives order: get a tappable launcher that creates a pre-set thread working first; make it "always at the bottom" second.
- **Stateless callbacks**: encode a stable preset id (e.g. `nt:p:fast`), never a raw model id or list index. Resolve against current config at click time so stale/pre-restart buttons stay correct or fail gracefully.
- Reuse existing machinery (inline keyboards, model picker UI, per-thread overrides, auto-title) instead of new subsystems.
- "Ugly but functional" launcher first; polish (always-bottom repost) as its own slice.

# User journey
1. In General, the user sees a launcher message with buttons: `⚡ Fast · 🧠 Smart · 🎯 Balanced · 🎛 Custom`.
2. User taps a favorite → bot creates a new topic already set to that model (+ optional thinking) → posts "🆕 ready, type here" → user types in the new topic.
3. Or user taps `🎛 Custom` → provider list → model list → bot creates a new topic set to that model → "ready, type here".
4. After each new message the user sends in General, the launcher is refreshed to the bottom so it's always the latest message.

# Foundations / Already shipped (✅)

## Forum General → auto-topic routing
- What exists: any normal message in General auto-creates a forum topic and answers there. `src/bot/queue.rs::dispatch_message` (async `createForumTopic`, sets `synthetic_topic_routed_from_general`); bot-sent messages are ignored so the bot never loops on its own launcher.
- ✅ Demo: send a message in General → a new topic appears and the reply lands there.

## Provider → model picker (inline)
- What exists: `/model` shows `build_provider_keyboard` → `build_models_keyboard`; callbacks `model_provider` / `model_pick:{provider}:{index}:{scope}` / `model_back` / `model_cancel` handled in `src/lib.rs::handle_model_callback`. `scope` is `"general"` or `"topic"`.
- ✅ Demo: `/model` in General → tap provider → tap model → default model updates.
- Gaps: only two scopes; handler computes `is_general = scope == "general"`, so **any non-`general` scope is silently treated as `topic`** — must be fixed before adding a new scope.

## Per-thread overrides + auto-title
- What exists: `thread_persistence::{set_model_override, set_thinking_override, set_pending_topic_title}` + `read_thread_model_override`. `/new` in General creates an empty topic and marks `pending_topic_title(true)`; auto-title fires on the first real user message (`handlers/message.rs` ~1016).
- ✅ Demo: `/new` in General → empty topic created → first message renames it.

## Follow-up buttons (repost + callback pattern)
- What exists: `src/followups.rs` posts an inline-keyboard message, tracks it in an in-memory map on `BotContext`, tapping dispatches a synthetic message; a Dismiss button deletes it.
- ✅ Demo: end a turn with a `<followups>` block → buttons appear → tap dispatches.

# MVP slices (ship-shaped, demoable)

## Slice 1: Wire favorites into the bot (reuse existing config)
- **Goal**: Expose the existing `[[favorites]]` list to the bot, filtered/validated for launcher rendering.
- **Scope checklist**:
  - [ ] No new config — read `context.config().favorites` (`Vec<ModelFavorite>` in `zdx-engine/src/config.rs`).
  - [ ] Render-time filter helper: skip favorites whose `model` ∉ `subagent_available_models()` (provider disabled), log skipped; empty list ⇒ launcher shows only `🎛 Custom`.
  - [ ] Unit test: filter drops unavailable-model favorites, keeps valid ones.
- **✅ Demo**: with the existing `[[favorites]]` in `config.toml`, a test/log shows the bot-visible favorites list; a favorite pointing at a disabled provider is skipped, others remain.
- **Risks / failure modes**: none new — favorites already parse and are covered by `test_favorites_load_and_survive_save`.

## Slice 2: Core helper — create topic with model override
- **Goal**: One reusable action: new topic → apply model (+optional thinking) → mark pending title → post "ready".
- **Scope checklist**:
  - [ ] `create_topic_with_model(context, chat_id, model, thinking: Option<ThinkingLevel>) -> Result<i64>`: `create_forum_topic`, derive thread id (`thread_id_for_chat`), `set_model_override`, optional `set_thinking_override`, `set_pending_topic_title(true)`, send "🆕 New thread — model `X`. Send your message here." into the new topic.
  - [ ] Reuse in the existing General `/new` path where sensible (do not regress current behavior).
- **✅ Demo**: call the helper (temporary `/newmodel <id>` or test) → new topic with the model set; first message auto-titles it.
- **Risks / failure modes**: topic creation failure → user-facing "couldn't create topic" + logged, matching current `/new` handling.

## Slice 3: Static General launcher (favorites + Custom)
- **Goal**: A tappable launcher in General that creates pre-set threads. Posted on demand (command) / once — not yet auto-repositioned.
- **Scope checklist**:
  - [ ] Build launcher inline keyboard: one button per preset (`callback nt:p:{id}`) + `🎛 Custom` (`nt:custom`).
  - [ ] `/launcher` (or `/menu`) command in General posts it; ignore in topics/DMs.
  - [ ] Route `nt:p:{id}` in `handle_callback_query`: resolve preset from current config → `create_topic_with_model`; if missing answer "Preset no longer configured".
  - [ ] Route `nt:custom` → open provider keyboard in the new `newthread` scope (Slice 4).
  - [ ] Answer callbacks; graceful message when no presets configured (show only Custom).
- **✅ Demo**: `/launcher` in General → tap a favorite alias → new topic pre-set to that model + thinking, "ready" message → typing works and auto-titles.
- **Risks / failure modes**: stale launcher after restart → callbacks still resolve from config by `alias`, so they keep working (stateless). Long aliases → keep `nt:p:{alias}` ≤ 64 bytes (aliases are short).

## Slice 4: Custom flow → new-thread scope (picker refactor)
- **Goal**: `🎛 Custom` reuses the provider→model picker but creates a *new* thread instead of setting default/topic override.
- **Scope checklist**:
  - [ ] Replace `is_general: bool` plumbing with `enum ModelPickerScope { General, Topic, NewThread }` across `build_provider_keyboard` / `build_models_keyboard` / `handle_model_callback` (preserve existing general/topic behavior exactly).
  - [ ] On `model_pick` with `NewThread`: call `create_topic_with_model` (thinking = default for v1).
  - [ ] Assert callback data ≤ 64 bytes (test); keep provider+index encoding.
- **✅ Demo**: `/launcher` → `🎛 Custom` → pick provider → pick model → new topic pre-set to that model; `/model` in General still edits the default; `/model` in a topic still sets the topic override.
- **Risks / failure modes**: scope regression → covered by keeping General/Topic paths byte-for-byte and adding tests.

## Slice 5: Always-bottom launcher (repost layer)
- **Goal**: Keep the launcher as the last message in General.
- **Scope checklist**:
  - [ ] In-memory `(chat_id -> launcher message_id)` map on `BotContext`.
  - [ ] After a General message routes to a new topic, delete the previous launcher and repost a fresh one at the bottom.
  - [ ] Per-chat serialization (mutex/generation) + light debounce to coalesce rapid messages; treat delete failures as non-fatal (log, continue).
  - [ ] Callbacks remain stateless (do not depend on the stored id being current).
- **✅ Demo**: send several messages in General → launcher always ends up as the last message; rapid bursts don't spawn duplicate launchers or error-spam.
- **Risks / failure modes**: repost racing with async topic creation, Telegram rate limits → mitigated by per-chat serialization + debounce.

## Slice 6: Thread-alias resolution + `create_topic_resuming` helper
- **Goal**: A new General topic can *adopt* an existing thread (e.g. a desktop UUID thread) so messages sent in that topic append to the original thread's history — no data copy, future history stays unified.
- **Mechanism (alias/redirect)**: Store an `alias_to = <source_thread_id>` field in the topic thread's meta (same `rewrite_meta_with_*` / `read_meta_*` pattern as `root_path`/`model_override`). When the bot resolves the effective thread for a chat/topic, if `alias_to` is set, use the source id for both loading history **and** persisting new events; the topic thread stays a thin, stable pointer.
- **Scope checklist**:
  - [ ] Add `alias_to: Option<String>` to the meta event; add `rewrite_meta_with_alias` + `read_meta_alias` and public `read_thread_alias(id) -> Result<Option<String>>` in `thread_persistence.rs` (mirror `read_thread_root_path`).
  - [ ] `Thread::set_alias(Option<String>)` writing meta before first non-meta event.
  - [ ] Bot thread resolution: after `thread_id_for_chat(...)`, wrap with `resolve_effective_thread_id(id)` that follows a single `alias_to` hop (guard against self/loops); use it in `agent::load_thread_state` **and** the persist path (`handlers/message.rs` ~990-1016) so history load + writes target the source thread. Root/model overrides continue to resolve from the source thread.
  - [ ] `create_topic_resuming(context, chat_id, source_thread_id) -> Result<i64>`: `create_forum_topic` named from the source thread's existing title (fallback to a trimmed preview), derive topic thread id (`thread_id_for_chat`), `set_alias(Some(source_thread_id))`, set the topic name to the source title, send "🔄 Resumed **<title>** — continue here." into the new topic. Does **not** set `pending_topic_title(true)` (the source already has a title).
  - [ ] Unit tests: alias round-trips through meta; `read_thread_alias` returns it; `resolve_effective_thread_id` follows one hop and is a no-op when unset.
- **✅ Demo**: temporary `/resume <thread_id>` in General → new topic named after the source thread → first message you send appends to the original thread (verify via `zdx threads show <source_id>` seeing the new turn); the topic's own JSONL stays a bare meta pointer.
- **Risks / failure modes**: bad/deleted source id → helper validates the source thread file exists first, else user-facing "that thread no longer exists" + log; alias loops → single-hop resolution only, reject aliasing to a `telegram-*` or already-aliased thread.

## Slice 7: `🔄 Continue` launcher button + thread picker
- **Goal**: Add a `🔄 Continue` button to the General launcher that lists recent threads and resumes the chosen one via Slice 6.
- **Scope checklist**:
  - [ ] Add `🔄 Continue` (`callback nt:resume`) to the launcher keyboard next to the presets + `🎛 Custom`.
  - [ ] Route `nt:resume`: build a thread-picker inline keyboard from `list_threads()` (top-level only) **excluding** ids starting with `telegram-`, filtered to `root_path == context.root_for_chat(chat_id).root`, newest first, capped (e.g. 8) with a `More`/`Cancel` control. Each button label = display title + relative time; `callback nt:r:{thread_id}`.
  - [ ] Route `nt:r:{thread_id}` → validate + `create_topic_resuming`; if the source vanished, answer "that thread no longer exists".
  - [ ] Empty list (no matching threads) → answer "No recent threads for this project yet".
  - [ ] Assert callback data ≤ 64 bytes (`nt:r:` + UUID ≈ 41 bytes; test guards it).
- **✅ Demo**: start a thread on the desktop in project X → in the bot's General for project X tap `🔄 Continue` → pick it from the list → new topic named after it → typing continues the same conversation with full prior context.
- **Risks / failure modes**: many threads → cap + newest-first (optional `nt:resume:q` free-text search variant deferred); stale picker after restart → callbacks carry the stable `thread_id`, resolved at click time, so they keep working.

# Contracts (guardrails)
- `/model` in General (edits default) and `/model` in a topic (sets topic override) must not change behavior.
- Resuming must **append to the source thread**, never fork/copy it; the topic thread stays an alias pointer.
- Resume must reuse the **source thread's existing title** for the topic and must **not** auto-title from the "resumed" message.
- The thread picker must only list top-level, non-`telegram-*` threads whose `root_path` matches the chat's resolved project.
- Alias resolution follows **at most one hop** and must reject loops / aliasing onto `telegram-*` threads.
- Normal General messages must still auto-create + auto-title topics.
- The bot must never process its own launcher message as user input.
- Launcher/preset callbacks must not create a topic unless the user tapped a create action.
- Callback data must stay ≤ 64 bytes.

# Key decisions (decide early)
- Callbacks carry a **favorite `alias`**, resolved from config at click time (not model id / index).
- Favorites **reuse the existing top-level `[[favorites]]` list** (shared with the TUI) — no Telegram-specific copy.
- Picker scope becomes an **enum** before adding `NewThread` (avoids the silent "non-general == topic" bug).
- New topics from callbacks set **`pending_topic_title(true)`**; the bot's "ready" message must not be used as the title source.

# Testing
- Manual smoke demo per slice (above).
- Regression tests: favorites render-filter (drops unavailable models); callback-data length; picker scope routing (General/Topic/NewThread) selects the right action.

# Polish phases (after MVP)

## Phase 1: Custom-flow thinking + launcher UX
- Optional thinking picker step in the Custom flow; nicer launcher header/labels; group presets across rows.
- ✅ Check-in demo: Custom flow lets you pick model then thinking before the thread is created.

# Later / Deferred
- Persistent bottom keyboard (`ReplyKeyboardMarkup`) — revisit only if Telegram fixes supergroup reliability and per-topic scoping.
- Pinned launcher variant — revisit if repost proves too noisy in an active General.
- Editable presets via bot commands (instead of `config.toml`) — revisit if hand-editing config becomes a pain.

# Oracle review (addendum)
- Ship the **static launcher first** (Slices 1–4); make **always-bottom repost its own slice** (Slice 5) — reflected above.
- **Stateless preset-id callbacks** are essential (stale buttons after restart).
- **Refactor picker scope to an enum before adding `NewThread`** — today `is_general = scope == "general"` means any other scope acts as `topic` and would set the wrong override.
- Favorites in `TelegramConfig`; validate against `subagent_available_models()`; empty/invalid presets must not break startup.
- New topic from callback must set `pending_topic_title(true)`; don't title from the "ready" message.
- Repost layer needs per-chat serialization + debounce and must tolerate delete failures.
r needs per-chat serialization + debounce and must tolerate delete failures.
