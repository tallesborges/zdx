# zdx-providers

LLM provider implementations extracted from `zdx-core`.

## Layout

- `src/lib.rs` — crate root: module declarations, `ProviderKind`, `ProviderSelection`, `resolve_provider()`, `ProviderBuildContext`
- `src/shared.rs` — provider-agnostic helpers (`resolve_api_key`, `resolve_base_url`, `merge_system_prompt`, `USER_AGENT`); re-exports value types from `zdx-types`
- `src/oauth.rs` — OAuth token storage/retrieval (Claude CLI, OpenAI Codex, Google Antigravity, Grok Build)
- `src/subscription_quota.rs` — read-only live quota fetchers plus the shared concurrent snapshot consumed by CLI and UI surfaces
- `src/anthropic/` — Anthropic Messages API + Claude CLI OAuth provider
- `src/openai/` — OpenAI Responses/Chat Completions/image generation API + Codex OAuth provider
- `src/gemini/` — Google Gemini API + Antigravity OAuth providers
- `src/openrouter.rs`, `src/deepseek.rs`, `src/mistral.rs`, `src/moonshot.rs`, `src/stepfun.rs`, `src/xiaomi.rs`, `src/minimax.rs`, `src/zai.rs`, `src/xai.rs` — thin OpenAI-compatible providers
- `src/grok_build.rs` — Grok Build provider: xAI Grok subscription OAuth over the xAI Responses API (bearer from `oauth::grok_build`, refreshed on demand)
- `src/openai_compatible.rs` — generic OpenAI-compatible chat-completions client for user-defined "custom" providers (`[providers.custom.<name>]`); carries no `ProviderKind`, built directly by the engine from a resolved base URL + API key
- `src/embeddings.rs` — hosted text-embeddings client (OpenAI-compatible `/embeddings`); explicit opt-in corpus/query embedding for native memory — batching, budgets, and persistence live in `zdx-engine`
- `src/opencode_go.rs` — meta-provider that routes to inner clients based on model registry hints; every route sends `x-opencode-session` using collision-safe encoding of the thread/conversation id, or a client-scoped UUID for one-shot threadless runs. Threadless TUI chats supply a stable conversation id across turns.
- `src/debug_metrics.rs`, `src/debug_trace.rs` — debug/tracing wrappers for provider streams
- `src/thinking_parser.rs` — SSE stream content parser

## Conventions

- Pure value types (DTOs, enums) belong in `zdx-types`, not here.
- This crate must NOT depend on `zdx-engine` (no circular deps).
- `zdx-engine` re-exports everything via a thin `providers.rs` facade.
- Provider routing hints (e.g. for the opencode-go meta-provider) are passed as `api_hint: Option<String>` parameters — model registry lookups happen in the caller (`zdx-engine`).

### OAuth credentials

- `oauth.json` is rewritten whole on every change, and providers rotate the refresh token on each refresh. Any read-modify-write MUST hold the cross-process lock (`<zdx home>/oauth.lock`): use `OAuthCache::update` for writes and `oauth::refresh_locked` for refreshes. `OAuthCache::save` is atomic (temp file + rename, 0600) but does not lock on its own.
- `refresh_locked` re-reads the cache after taking the lock and returns the stored credentials when another process already refreshed, so a rotated refresh token is never replayed. Provider `resolve_credentials` must not refresh or persist tokens itself.

### Request logging

- Every streaming request site MUST go through `shared::log_request(client, url)` before sending and `shared::check_response_status(client, response)` for the status check. `check_response_status` owns the non-success path (log + `ProviderError::http_status`); do not hand-roll it.
- `client` is the request implementation label so callers sharing a helper stay distinguishable. Streaming: `anthropic`, `claude-cli`, `openai`, `codex`, `xai`, `grok-build`, `chat-completions`, `gemini`, `google-antigravity`. Non-streaming: `gemini-image`, `gemini-media`, `openai-image`, `codex-image`, `alibaba-image`.
- MUST NOT log headers, request bodies, or full URLs. `endpoint_label` intentionally keeps only host + path: a configured base URL may embed `user:pass@` and some APIs accept `?key=`.
- Error bodies are truncated **and** whitespace-collapsed (`truncate_body`) so one event is always one log line — pretty-printed JSON would otherwise break line-oriented log viewers such as the monitor's Logs tab.
- Do not log the model here; the engine's `turn` span already carries it. Providers must not reach for engine state — rely on parent-span context.
