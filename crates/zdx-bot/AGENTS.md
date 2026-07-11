# zdx-bot development guide

Scope: Telegram bot runtime, ingest/handler flow, queueing, and Telegram API integration.

## Where things are

- `src/lib.rs`: bot crate entrypoint
- `src/followups.rs`: end-of-turn follow-up suggestion buttons (`<followups>` tag → tap dispatches new turn)
- `src/staging.rs`: staged (memory-only) slash-command flow — `/handoff` + `/prompt_builder` input capture, Accept/Discard/regenerate; handoff Accept seeds a new topic with `handoff_from`, prompt-builder Accept runs the prompt in place
- `src/command_picker.rs`: `/commands` picker — project/context `.md` commands only (picker-only; built-ins live in the native `/` menu)
- `src/commands.rs`: centralized slash-command parsing and matching
- `src/bot/mod.rs`: bot module exports
- `src/bot/context.rs`: shared bot context
- `src/bot/queue.rs`: per-chat queueing helpers
- `src/handlers/mod.rs`: handler module exports
- `src/handlers/message/mod.rs`: message intake orchestration + shared turn types (`ReplyContext`, `TurnStatus`, `TurnResult`, `SpawnRequest`, `StatusSnapshot`); re-exports the keyboard builders
- `src/handlers/message/commands.rs`: slash-command handlers (`/new`, `/model`, `/thinking`, `/status`, `/whereami`, thread/worktree, exit) + model/provider/thinking keyboards
- `src/handlers/message/turn.rs`: agent turn lifecycle (`run_agent_turn`, spawn/stream/finalize)
- `src/handlers/message/status.rs`: turn status setup/update/cleanup + status-message formatting (usage, pricing, context)
- `src/handlers/message/response.rs`: final response sending (text send/edit/fallback)
- `src/handlers/message/media.rs`: `<media>` routing parse + path classification (image→`sendPhoto`, `.ogg/.oga/.opus`→`sendVoice`, `.mp3/.m4a/.wav`→`sendAudio`, else `sendDocument`)
- `src/ingest/mod.rs`: Telegram message parsing + attachment loading
- `src/agent/mod.rs`: thread log + agent turn helpers
- `src/telegram/mod.rs`: Telegram API client + tool wiring
- `src/telegram/types.rs`: Telegram API DTOs
- `src/topic_title.rs`: async LLM-based topic title generation
- `src/transcribe.rs`: audio transcription helper
- `src/types.rs`: bot message/media types

## Conventions

- Keep Telegram API DTOs and request behavior inside `src/telegram/`.
- Keep orchestration in handlers; move reusable logic to smaller modules.

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-bot`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Add/move/delete prompt layer files in this crate: update this file.
- Bot behavior contract changes: update `docs/SPEC.md` as needed.