# zdx-bot development guide

Scope: Telegram bot runtime, ingest/handler flow, queueing, and Telegram API integration.

## Where things are

- `src/lib.rs`: bot crate entrypoint
- `src/followups.rs`: end-of-turn follow-up suggestion buttons (`<followups>` tag → tap dispatches new turn)
- `src/retry.rs`: post-failure "Try again" button — on a turn that fails without a reply, offers a button that re-runs the same turn from persisted thread state (no new user message); `retry:go`/`retry:x` callbacks
- `src/server.rs`: embedded HTTP web server for the unified Mini App. Serves the built Svelte app from `apps/web/dist` via `rust-embed` — `/app` is the SPA entry point (`no-cache`), `/app/{*path}` serves content-hashed assets (`immutable`, one year), and `/threads` / `/monitor` are compatibility aliases for the same shell. Responses are gzip-compressed via `tower_http::CompressionLayer`. Every `/api/*` endpoint requires fresh bot-token-signed Telegram `initData` from an allowlisted user. The read-only monitor snapshot is built off the async hot path and cached for 30 seconds; live quotas come from the same shared snapshot API as `zdx quota`. Git inspection resolves the selected thread root before the bot root and bounds lazy per-file diffs. Diffs are scope-aware: `/api/git` returns the uncommitted status, and `/api/git/scope?scope=all|<hash>` lists the files a history scope touches (`all` = merge base with the repository's **main branch** against the working tree, so it shows what this branch adds the way a pull request would, plus local edits; on main itself that equals uncommitted. The main branch is read from `origin/HEAD`, falling back to `main` then `master`, preferring a local branch over its remote; with none found the base is `HEAD`). Revisions reaching a `git` argument list are validated as 7–40 hex characters by `valid_commit_hash`, so nothing readable as an option or a range is ever passed. `/api/threads` lists the 50 most recent top-level threads, limited in SQL, each carrying a `t.me/c/<internal_id>/<topic_id>` link when the thread id encodes a supergroup topic.
- `src/staging.rs`: staged (memory-only) slash-command flow — `/handoff`, `/btw` + `/prompt_builder` input capture. `/handoff` and `/btw` complete on the staged input with no Accept tap and seed a new topic with `handoff_from` via the shared `seed_new_topic`; only `/prompt_builder` stages an Accept/Discard preview with regenerate, because accepting it runs the prompt in the current thread. `/handoff` generates its context block first and passes it to `seed_new_topic` as a `record` posted into the new topic (the seed is dispatched synthetically and never appears in chat); on failure the session stays open to retry. `/btw` makes no LLM call at all: its seed is the question plus a parent-thread pointer the new topic's agent resolves with `Read_Thread`. `/btw` also bypasses the per-topic queue (both the command and the staged question, via `staging::awaiting_btw_input`), so a side question never waits behind the running turn. Bypassed inputs are marked `Message::routed_as_btw_input`, so if the session is gone by the time they land they are answered with a hint instead of falling through to a normal (concurrent) turn; messages older than the command's `message_id` are never taken as staged input.
- `src/command_picker.rs`: `/commands` picker — project/context `.md` commands only (picker-only; built-ins live in the native `/` menu)
- `src/commands.rs`: centralized slash-command parsing and matching, including strict `/restart [f|force|--force|q|queue]` parsing (`RestartMode::{Gated,Force,Queued}`); `/restart` bypasses the per-topic queue
- `src/bot/mod.rs`: bot module exports
- `src/bot/context.rs`: shared bot context; also owns per-profile layered configs (`config_for_chat`) and the process-lifetime orchestrator route map (owner thread → chat/topic/user for worker completion callbacks)
- `src/bot/queue.rs`: per-chat queueing helpers; topics created from ordinary General messages — and brand-new Threaded Mode DM threads on first sighting — are marked as the persistent `orchestrator` profile before the first turn
- `src/bot/synthetic.rs`: shared synthetic-message helper (`i64::MAX`-countdown id counter + dispatch through the topic queue), used by `/goal` continuations and the orchestrator completion bridge
- `src/orchestrator.rs`: orchestrator worker bridge — consumes engine `WorkerManager` `WorkerEvent`s: `Created` opens a mirror topic (aliased + `worker_topic`-marked) in the project's/owner's forum chat, posts the standard pinned status card for the worker thread plus the first prompt, and registers its `t.me/c/...` link as the worker's `mirror_url` (resolved to `None` when no mirror opened, so `create_thread` never waits out its bounded link wait); `Activity` maintains one debounced live status line per running turn (current tool only, same shape as a normal turn's status, deleted when the result posts); `Completed` posts the result and wakes the owning orchestrator topic with a bounded synthetic `[worker update]` turn. Mirror header/status/result messages carry `⏹ Cancel worker` (`wk:c`, worker resolved from the topic's alias) and Mini App `💬 Open Thread` buttons. At startup the bridge rebuilds the worker→topic map from persisted `worker_topic` + `alias_to` meta (`list_worker_topics`) so feeds resume after restarts; routes stay process-lifetime
- `src/handlers/mod.rs`: handler module exports
- `src/handlers/message/mod.rs`: message intake orchestration + shared turn types (`ReplyContext`, `TurnStatus`, `TurnResult`, `SpawnRequest`, `StatusSnapshot`); re-exports the keyboard builders
- `src/handlers/message/commands.rs`: slash-command handlers (`/new`, `/model`, `/thinking`, `/status`, `/whereami`, `/launcher`, thread/worktree, exit, `/restart` incl. the `q` mode's background wait-for-idle task that polls `agent_activity::list_active` and fires the gated restart, single-slot via `BotContext::try_claim_queued_restart`) + model/provider/thinking keyboards + `ModelPickerScope` (General/Topic/NewThread)
- `src/handlers/message/launcher.rs`: General-topic thread launcher — bot-visible `[[favorites]]` filter, `create_topic_with_model`, `create_topic_resuming`, `/launcher` keyboard (`nt:p:{alias}`/`nt:custom`/`nt:resume`) + callback routing; Custom opens the model picker in `NewThread` scope; `🔄 Continue` picker resumes a source thread via `alias_to`; `LauncherMap` + `schedule_repost` keep the launcher as the last message in General (debounced per-chat repost)
- `src/handlers/message/mod.rs`: message intake orchestration + shared turn types; `thread_id_for_chat` + `parse_topic_thread_id` (inverse) + `resolve_effective_thread_id` (follows one `alias_to` hop so resumed topics load/persist to the source thread); `mini_app_base_url`; re-exports the keyboard builders
- `src/handlers/message/turn.rs`: agent turn lifecycle (`run_agent_turn`, spawn/stream/finalize)
- `src/handlers/message/status.rs`: turn status setup/update/cleanup, the status keyboard (Cancel + Mini App `Open Thread`), and status-message formatting (usage, pricing, context)
- `src/handlers/message/thread_header.rs`: first-message topic status card, Mini App/refresh keyboard, and silent pinning
- `src/handlers/message/response.rs`: final response sending (text send/edit/fallback). Every answer gets a trailing `↗ Open thread` Mini App deep link (`{mini_app_url}?startapp={thread_id}`) via `append_thread_link`, skipped when the Mini App is not configured for the chat. Answers are sent through the `*_without_preview` client wrappers so that link does not render a preview card; other message paths keep previews.
- `src/handlers/message/media.rs`: `<media>` routing parse + path classification (image→`sendPhoto`, `.ogg/.oga/.opus`→`sendVoice`, `.mp3/.m4a/.wav`→`sendAudio`, else `sendDocument`)
- `src/ingest/mod.rs`: Telegram message parsing + attachment loading
- `src/agent/mod.rs`: thread log + agent turn helpers
- `src/telegram/mod.rs`: Telegram API client + tool wiring (HTML send/edit fallback ladder: HTML → `html::sanitize` retry → plain text); `topic_link(chat_id, topic_id)` builds the `t.me/c/<internal_id>/<topic_id>` form for supergroup topics (shared by the Mini App thread list and worker mirrors)
- `src/telegram/html.rs`: Telegram HTML repair pass — escapes stray `<`/`&`, drops unsupported tags/entities, balances unclosed or crossed tags
- `src/telegram/markdown.rs`: model-text Markdown → Telegram HTML edge converter + entity-safe visible-character truncation
- `src/telegram/types.rs`: Telegram API DTOs
- `src/topic_title.rs`: async LLM-based topic title generation
- `src/transcribe.rs`: audio transcription helper
- `src/types.rs`: bot message/media types

## Conventions

- Keep Telegram API DTOs and request behavior inside `src/telegram/`.
- Keep orchestration in handlers; move reusable logic to smaller modules.

## Checks

- The Mini App must be built before any cargo command touches this crate: `just web-build` (rust-embed reads `apps/web/dist` at compile time). `just build-release` and CI do this automatically.
- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-bot`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Add/move/delete prompt layer files in this crate: update this file.
- Bot behavior contract changes: update `docs/SPEC.md` as needed.