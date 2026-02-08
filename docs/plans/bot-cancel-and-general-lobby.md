# Goals
- Users can cancel an in-progress or queued agent turn from Telegram (inline button)
- Cancelled queued messages are removed without being processed
- Bot shows "🧠 Thinking..." status while processing (editable message)

# Non-goals
- Showing which tool is being called
- "Show details" expandable tool output
- Streaming partial text via message edits (just thinking → final)
- Feedback buttons (👍/👎) after response
- Model picker inline keyboard

# Design principles
- User journey drives order
- Smallest useful change first: cancel button works even without the lobby redesign
- Reuse existing queue infrastructure; don't restructure what works

# User journey
1. User sends a message in General → bot creates a topic (already works)
2. Bot sends a "🧠 Thinking..." status message with a `[⏹ Cancel]` inline button
3. If user taps Cancel while processing → agent stops, message edited to "Cancelled ✓"
4. If user sends a second message while first is processing → bot sends "⏳ Queued" with `[✖ Cancel]`
5. If user cancels a queued message → message removed from queue, status edited to "Cancelled ✓", user's original message deleted (if bot has permission)
6. When agent finishes → status message edited to final response, Cancel button removed

# Foundations / Already shipped (✅)

## Forum topic auto-creation
- What exists: `dispatch_message` in `queue.rs` creates a forum topic when a message is sent to General, names it from first line of text
- ✅ Demo: send a message in General → topic created, reply appears in topic
- Gaps: no pinned lobby message, no inline button for new conversation

## Per-topic sequential queue
- What exists: `ChatQueueMap` with `(chat_id, topic_id)` keys, unbounded mpsc channels, sequential processing per topic
- ✅ Demo: send two messages in same topic → processed in order
- Gaps: no way to cancel queued items, no visibility into queue position

## Typing indicator
- What exists: `start_typing()` returns `TypingIndicator` with `CancellationToken`, auto-cancels on drop
- ✅ Demo: bot shows "typing..." while processing
- Gaps: no editable status message, no cancel button

## Telegram client
- What exists: `send_message`, `create_forum_topic`, `send_chat_action`, `get_file`, `download_file`
- ✅ Demo: bot sends messages, creates topics
- Gaps: missing `editMessageText`, `deleteMessage`, `answerCallbackQuery`, `pinChatMessage`, `hideGeneralForumTopic`; no inline keyboard support; no `callback_query` in `allowed_updates`

# MVP slices (ship-shaped, demoable)

## Slice 1: Telegram client — edit, delete, callback, inline keyboard

- **Goal**: Add the Telegram API methods and types needed by all subsequent slices
- **Scope checklist**:
  - [ ] Add `InlineKeyboardMarkup` and `InlineKeyboardButton` types (serializable)
  - [ ] Add `CallbackQuery` type (deserializable) with `id`, `from`, `message`, `data` fields
  - [ ] Extend `Update` type to include optional `callback_query` field
  - [ ] Add `allowed_updates: ["message", "callback_query"]` to `get_updates`
  - [ ] Add `send_message_with_reply_markup` method that returns the sent `Message` (need `message_id` back)
  - [ ] Add `edit_message_text(chat_id, message_id, text, reply_markup?)` method
  - [ ] Add `delete_message(chat_id, message_id)` method
  - [ ] Add `answer_callback_query(callback_query_id, text?)` method
  - [ ] Add `pin_chat_message(chat_id, message_id, disable_notification?)` method
  - [ ] Add `hide_general_forum_topic(chat_id)` method
- **✅ Demo**: cargo builds, new methods callable (unit-testable with mock or manual bot test)
- **Risks / failure modes**:
  - Inline keyboard JSON shape mismatch → test serialization against Telegram docs
  - `send_message` currently discards the returned `Message` → new variant must return it

## Slice 2: Status message with Cancel button on processing

- **Goal**: When the agent starts processing, send an editable "Thinking..." message with a Cancel inline button. On completion, edit it to the final response.
- **Scope checklist**:
  - [ ] In `handle_message`, after `record_user_message`, send a "🧠 Thinking..." message with `[⏹ Cancel]` button (callback_data: `cancel:{chat_id}:{topic_id}`)
  - [ ] Store the status message's `message_id` so it can be edited later
  - [ ] Add a `CancellationToken` per active agent turn, stored in a shared map keyed by `(chat_id, topic_id)`
  - [ ] On agent completion: `edit_message_text` the status message to the final response (remove inline keyboard)
  - [ ] On agent error: edit status message to error text
  - [ ] In the polling loop (`lib.rs`), handle `callback_query` updates: if data matches `cancel:*`, cancel the token and call `answer_callback_query`
  - [ ] When token is cancelled: agent turn aborts, status message edited to "Cancelled ✓"
  - [ ] Drop the `TypingIndicator` when status message is sent (they serve the same purpose; status message is better)
- **✅ Demo**: send a message → see "🧠 Thinking..." with Cancel button → tap Cancel → message changes to "Cancelled ✓". Or wait for completion → message changes to final response with no button.
- **Risks / failure modes**:
  - Agent turn (`run_agent_turn_with_persist`) doesn't support cancellation today → need to either pass a `CancellationToken` into the agent loop or use `tokio::select!` with the token in `handle_message`
  - Long messages may fail `editMessageText` (4096 char limit) → use same chunking as current `send_message`
  - Race: user cancels right as agent finishes → handle gracefully (edit might fail, that's ok)

## Slice 3: Cancel queued messages

- **Goal**: When a message is queued (second message while first is processing), show queue position with a Cancel button. Cancelling removes it from the queue.
- **Scope checklist**:
  - [ ] Change queue channel type from `mpsc::UnboundedSender<Message>` to send a wrapper that includes a per-item `CancellationToken` (or cancellation flag)
  - [ ] When a message is enqueued (not the first), send a "⏳ Queued" status message with `[✖ Cancel]` button
  - [ ] Store the queued status message_id with the queue item
  - [ ] In queue worker: before processing each item, check if cancelled → if yes, skip and edit status to "Cancelled ✓"
  - [ ] On cancel callback for a queued item: mark cancelled, edit status to "Cancelled ✓", attempt `delete_message` on user's original message (best-effort, ignore failure)
  - [ ] Callback data format: `cancel_q:{chat_id}:{topic_id}:{message_id}` to distinguish from active cancel
- **✅ Demo**: send message A (processing) → send message B (queued, shows "⏳ Queued" with Cancel) → tap Cancel on B → B's status changes to "Cancelled ✓", B never processed. A continues normally.
- **Risks / failure modes**:
  - Deleting user messages requires `can_delete_messages` admin right → document this requirement, fail silently if not granted
  - Queue ordering: cancelling middle items shouldn't break sequential processing
  - Race: item starts processing right as user cancels → treat as active cancel (Slice 2 handles it)

# Contracts (guardrails)
- Existing message flow must not break: messages in topics still processed sequentially
- Thread log persistence must continue working (user message → agent turn → assistant message logged)
- Allowlist enforcement unchanged (user and chat allowlists still checked)
- Commands (`/new`, `/worktree`) continue to work as before in topics
- Bot must handle Telegram API failures gracefully (edit fails → log and continue, don't crash)

# Key decisions (decide early)
- **Callback data format**: use `cancel:{chat_id}:{topic_id}` for active turns, `cancel_q:{chat_id}:{topic_id}:{msg_id}` for queued. Keep it short (callback_data has 64-byte limit).
- **Cancellation mechanism**: use `tokio_util::sync::CancellationToken` (already a dependency via `TypingIndicator`) stored in a shared `Arc<Mutex<HashMap<QueueKey, CancellationToken>>>` for active turns.
- **Status message ownership**: the status message replaces the typing indicator — send it once, edit it through the lifecycle (thinking → done / cancelled / error).
- **`send_message` return type**: new method `send_message_with_markup` returns `Message` (with `message_id`). Existing `send_message` stays unchanged (returns `()`).

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts:
  - Inline keyboard JSON serialization roundtrip
  - Callback data parsing (format validation)
  - `generate_topic_name` existing tests still pass

# Polish phases (after MVP)

## Phase 1: Queue position display
- Show "⏳ Queued (position #2)" with live updates as queue drains
- ✅ Check-in demo: send 3 messages rapidly, see positions count down

## Phase 2: Thinking status refinement
- Show elapsed time: "🧠 Thinking... (12s)"
- Edit periodically to show progress
- ✅ Check-in demo: watch timer tick up during a long response

# Later / Deferred
- General lobby with pinned "New Conversation" button (hide General, pin a single inline button to create topics) → revisit only if General gets noisy or multiple users need onboarding. Current auto-create-on-message flow is faster for power users.
- Streaming partial text via message edits → revisit when edit rate limits are understood (Telegram limits edits to ~1/sec per message)
- Tool call display ("🔧 Running web_search...") → revisit after MVP is dogfooded
- "Show details" expandable output → revisit after tool display
- Feedback buttons (👍/👎) → revisit after core cancel UX is solid
- Model picker inline keyboard → separate feature
