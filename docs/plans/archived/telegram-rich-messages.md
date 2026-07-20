> Stage: archived (abandoned 2026-07-20). Implemented and dogfooded, then reverted.

# Outcome: reverted — I did not like it (2026-07-20)
Rich messages were fully built (default send path) and the Telegram output contract was updated to emit native headings and tables. After dogfooding, I did not like the result: the native rendering felt worse than the compact classic style for everyday chat (notably the larger heading fonts), and the existing HTML-attachment path already handles the "I need the full detail" case better than an inline table.

Decision: revert to the classic HTML send path. The only thing kept was the unrelated cleanup — restructuring the Telegram instruction layer from dated XML wrappers into lean Markdown (same classic rules, just tidier). This plan is archived for reference only.

# Goals
- Send final assistant answers as Telegram **rich messages** (`sendRichMessage` with an `html` field) so replies can render native headings, tables, lists, collapsible `<details>`, and longer content (up to 32768 chars).
- Make it **toggleable** via a config flag so it can be dogfooded and turned on/off without a rebuild decision baked in.
- Keep the existing `<media>` HTML-attachment / visual-render path **unchanged** for big/detailed deliverables.
- Preserve safe delivery: **rich → HTML → plain** fallback so nothing breaks on unsupported clients or API errors.

# Non-goals
- Replacing or modifying the `<media>` attachment path (`send_document_from_path` / `send_photo_from_path`).
- Rich formatting for status/progress edits, staging previews, command pickers, or the followups keyboard message — those stay on the classic path.
- Token-by-token streaming of the answer (not used today; out of scope).
- Per-profile toggle in phase 1 (global `[telegram]` flag first; per-profile is Later).

# Design principles
- User journey drives order: get a rich final answer visible in a real chat as early as possible, behind a flag.
- Reuse before rebuild: rich send rides the existing `post()` plumbing and mirrors `send_message_raw`; do not fork a parallel client.
- Scope to final answers only: the single touchpoint is the final-answer send in `handlers/message/response.rs`.
- Fail safe: layer rich on top of the current HTML→plain fallback rather than replacing it.

# User journey
1. User sends a message to the zdx Telegram bot.
2. Bot works the turn (status edits as today).
3. Bot delivers the **final answer** — when `rich_messages` is on and content warrants it, as a native rich message; otherwise the current HTML message.
4. Followups keyboard and any `<media>` attachments follow, exactly as today.
5. User (Talles) dogfoods, compares rich vs classic, and decides the default.

# Foundations / Already shipped (✅)
## Low-level send plumbing
- What exists: `crates/zdx-bot/src/telegram/mod.rs` — `send_message_raw` → `SendMessageRequest` → `post("sendMessage")`; `TELEGRAM_PARSE_MODE = "HTML"`; HTML→plain fallback in `send_message_inner*` (matches on `"can't parse entities"` / `"Can't find end of"`).
- ✅ Demo: current bot replies render `<b>`/`<blockquote>` correctly.
- Gaps: no `sendRichMessage` request struct or sender; no rich content flavor.

## Final-answer send touchpoint
- What exists: `handlers/message/turn.rs:183-239` captures `final_text` on `AgentEvent::TurnFinished` → `send_final_response`; `handlers/message/response.rs:8-142` parses content and sends via `edit_message_text` (replace status) or `send_message` / `send_message_with_reply_params` (fallback / cross-topic).
- ✅ Demo: final answers today replace the status message in one edit.
- Gaps: single send helper hardcoded to the HTML/text path.

## Media attachment path (keep as-is)
- What exists: `handlers/message/media.rs:17-148` parses `<medias>/<media>` and uploads via `send_photo_from_path` / `send_document_from_path` (`telegram/mod.rs:305-375, 775-888`, posting `sendPhoto` / `sendDocument`).
- ✅ Demo: an HTML report attached via `<media>` still uploads as a document.
- Gaps: none — explicitly untouched.

## Followups keyboard (separate message, keep as-is)
- What exists: `followups.rs:30-77` sends a separate "💡 Suggested replies" message with inline buttons **after** the answer; callbacks handled at `followups.rs:80-163`.
- ✅ Demo: followup buttons still appear and dispatch synthetic turns.
- Gaps: none for phase 1 (rich answer does not need reply_markup).

## Config
- What exists: `crates/zdx-engine/src/config.rs:119-156` `TelegramConfig` (token/allowlists/model/thinking/profiles); loaded at `zdx-bot/src/lib.rs:52-54`; settings resolved at `telegram/mod.rs:33-44`.
- ✅ Demo: `[telegram]` settings already read from config today.
- Gaps: no `rich_messages` field yet.

# MVP phases (ship-shaped, demoable)

## Phase 1: rich send path + global toggle (dogfoodable)
- **Goal**: final answers can be sent as a rich message when `[telegram] rich_messages = true`, with fallback to today's behavior.
- **Scope checklist**:
  - [ ] Add `SendRichMessageRequest` (`chat_id`, `html`, `message_thread_id`, `reply_parameters`) + `send_rich_message_raw` mirroring `send_message_raw`, posting `sendRichMessage` (`telegram/mod.rs`).
  - [ ] Add `rich_messages: bool` (default `false`) to `TelegramConfig` (`config.rs:119-156`) and thread it to the final-response path (via `context.config().telegram` / `TelegramSettings`).
  - [ ] In `response.rs` final-answer send, when flag is on, try `send_rich_message_raw(html = answer)`; on error (unsupported method / parse), fall back to the existing `edit_message_text` / `send_message` HTML path (which already falls back to plain).
  - [ ] Confirm cross-topic replies still work (rich equivalent of `reply_parameters`, else fall back to classic for cross-topic).
- **✅ Demo**: with `rich_messages = true`, ask the bot a question whose answer has a heading + a small table; it renders natively in Telegram. Set `rich_messages = false`; same answer renders in the current HTML style. `<media>` attachment + followups still work in both.
- **Risks / failure modes**:
  - `sendRichMessage` may not accept `reply_markup` or a status-edit equivalent — mitigated by keeping followups/status on the classic path.
  - Status-message replace: rich may need a fresh `send` instead of `edit_message_text` (no `editRichMessage`) — fall back to delete-status-then-send if edit isn't supported.
  - Client rollout: older clients may render poorly — the flag + fallback contain the blast radius.

## Phase 2: content flavor + smart routing
- **Goal**: generate answer content that actually uses rich features, and only route to rich when it pays off.
- **Scope checklist**:
  - [ ] Decide the rich content source: emit rich HTML for the answer and derive a classic-HTML downgrade for fallback (strip headings/tables → blockquotes/plain), OR generate both flavors.
  - [ ] Add a routing heuristic: use rich only when the answer contains structure (headings/tables/long content); short chat replies stay classic to avoid visual noise.
  - [ ] Verify long answers (>4096 chars) send in one rich message instead of being truncated (closes the current no-splitter gap noted in `staging.rs:32-35`).
- **✅ Demo**: a long structured answer that previously would exceed 4096 chars now sends as one rich message; a short "yes/here's the command" reply stays in the compact classic style.
- **Risks / failure modes**:
  - Downgrade logic could drop content — fallback must be lossless enough to stay readable.
  - Heuristic misfires (rich for trivial replies) — keep it conservative.

# Contracts (guardrails)
- The `<media>` HTML-attachment / visual-render path must not change behavior.
- Followups keyboard must still appear and dispatch after the answer.
- Status/progress edits, staging previews, and command pickers stay on the classic path.
- With `rich_messages = false`, behavior is byte-for-byte today's behavior.
- Any rich send failure must fall back to a delivered classic message (never a dropped answer).

# Key decisions (decide early)
- **Toggle scope**: global `[telegram] rich_messages` first (per `explorer` this is the low-friction fit); per-profile deferred.
- **Default value**: ship `false`, dogfood, then flip to `true` if it holds up (this is the "keep it as default?" question — resolved by testing, not upfront).
- **Edit vs send for final answer**: confirm whether rich answers can replace the status message via edit; if not, standardize on delete-status-then-rich-send.
- **Content strategy**: rich-primary with classic downgrade vs dual-generation (affects Phase 2 shape).

# Testing
- Manual smoke demos per phase (above ✅ Demos) in a real Telegram chat.
- Minimal regression: assert `rich_messages = false` still routes through `send_message`/`edit_message_text` unchanged; a unit/integration check that the media + followups path is untouched.
- Verify the fallback chain (force a rich send error → lands on HTML → plain).

# Polish rounds (after MVP)
## Polish round 1: routing + ergonomics
- Tune the rich-vs-classic heuristic based on dogfooding.
- Optional per-profile override of `rich_messages`.
- ✅ Check-in demo: across a day of real use, structured answers go rich and quick replies stay classic, with no fallback surprises.

# Later / Deferred
- Per-profile `rich_messages` toggle (extend `TelegramProfileConfig`, `config.rs:139-157`) — revisit if different chats want different defaults.
- `editRichMessage`-based progressive/status rendering — revisit if Telegram exposes it and streaming is wanted.
- Rich formatting for staging/command/followup messages — revisit only if there's a clear UX win.
