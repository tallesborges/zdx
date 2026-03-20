# zdx-bot development guide

Scope: Telegram bot runtime, ingest/handler flow, queueing, and Telegram API integration.

## Where things are

- `src/lib.rs`: bot crate entrypoint
- `src/commands.rs`: centralized slash-command parsing and matching
- `src/bot/mod.rs`: bot module exports
- `src/bot/context.rs`: shared bot context
- `src/bot/queue.rs`: per-chat queueing helpers
- `src/handlers/mod.rs`: handler module exports
- `src/handlers/message.rs`: message flow orchestration
- `src/ingest/mod.rs`: Telegram message parsing + attachment loading
- `src/agent/mod.rs`: thread log + agent turn helpers
- `prompts/telegram_instruction_layer.md`: Telegram-specific output instruction layer
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
- Intermediate iteration for this crate: `cargo test -p zdx-bot`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Add/move/delete prompt layer files in this crate: update this file.
- Bot behavior contract changes: update `docs/SPEC.md` as needed.