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
- `src/telegram/mod.rs`: Telegram API client + tool wiring
- `src/telegram/types.rs`: Telegram API DTOs
- `src/topic_title.rs`: async LLM-based topic title generation
- `src/transcribe.rs`: audio transcription helper
- `src/types.rs`: bot message/media types

## Conventions

- Keep Telegram API DTOs and request behavior inside `src/telegram/`.
- Keep orchestration in handlers; move reusable logic to smaller modules.

## Checks

- Targeted: `cargo test -p zdx-bot`
- Workspace lint/test: use `just lint` / `just test` from repo root

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Bot behavior contract changes: update `docs/SPEC.md` as needed.