# zdx-providers

LLM provider implementations extracted from `zdx-core`.

## Layout

- `src/lib.rs` — crate root: module declarations, `ProviderKind`, `ProviderSelection`, `resolve_provider()`, `ProviderBuildContext`
- `src/shared.rs` — provider-agnostic helpers (`resolve_api_key`, `resolve_base_url`, `merge_system_prompt`, `USER_AGENT`); re-exports value types from `zdx-types`
- `src/oauth.rs` — OAuth token storage/retrieval (Claude CLI, OpenAI Codex, Google Antigravity, Grok Build, Muse Code). All but Muse Code use PKCE authorization-code with a localhost callback; `oauth::muse_code` is the only RFC 8628 device-authorization flow (Meta exposes no redirect client), and it stores two credentials: `refresh` = Meta identity token, `access` = minted Model API key (`NO_EXPIRY`; replaced only via `remint_rejected_key`, a compare-and-swap under the cache lock so a concurrent process does not re-hit the rate-limited mint endpoint). Every call in this module uses a bounded HTTP client (connect + request ceilings) that **must** send a user agent and **must not** follow redirects: `auth.meta.com` answers a user-agent-less request with a 302 to `facebook.com/unsupportedbrowser`, and reqwest sends none by default, so following that redirect turns a rejected request into a confusing JSON parse failure on an HTML body (both device-code endpoints are affected; the `api.meta.ai` mint host currently is not). The device-code poll additionally bounds *both* its waits and each request/body-read by the grant's remaining lifetime, so it cannot outlive the advertised deadline nor accept a token that lands after the code expired — the client's fixed timeout is only a per-request upper bound.
- `src/subscription_quota.rs` — read-only live quota fetchers plus the shared concurrent snapshot consumed by CLI and UI surfaces. Plan labels always come from provider-declared metadata, never inferred from utilization: Codex `plan_type`, Grok `subscription_tier`, Muse Code `subs_tier_name`. Claude's usage endpoint carries no plan, so it is read from `/api/oauth/profile` (`rate_limit_tier`, e.g. `default_claude_max_20x` → `claude_max_20x`; only the `default_` prefix is stripped) concurrently with the usage call, and degrades to an unlabelled quota on failure.
- `src/anthropic/` — Anthropic Messages API + Claude CLI OAuth provider
- `src/openai/` — OpenAI Responses/Chat Completions/image generation API + Codex OAuth provider
- `src/gemini/` — Google Gemini API + Antigravity OAuth providers
- `src/openrouter.rs`, `src/deepseek.rs`, `src/mistral.rs`, `src/moonshot.rs`, `src/stepfun.rs`, `src/xiaomi.rs`, `src/minimax.rs`, `src/zai.rs`, `src/xai.rs` — thin OpenAI-compatible providers
- `src/zai.rs` — Z.AI GLM. The GLM-5.3 family always reasons: it 400s on `thinking: {"type": "disabled"}` (code 1210) and accepts only `reasoning_effort` `low|high|max`, so that family gets no thinking toggle and carries the level as a top-level effort instead; other GLM models keep the documented enabled/disabled toggle (verified against `api.z.ai` 2026-09-15).
- `src/meta.rs` — Meta Muse Spark over the Responses API. Reasoning cannot be disabled: `effort: "none"` is HTTP 400 and omitting `reasoning` hands depth to the model (~118 reasoning tokens for a one-line answer vs 5 at `minimal`), so `off` sends `minimal` with no summary. `max` is Standard-tier `muse-spark-1.3` only (Contributor 400s) and is clamped to `xhigh` elsewhere (verified live 2026-09-16; docs: dev.meta.ai/docs/reasoning).
- `src/grok_build.rs` — Grok Build provider: xAI Grok subscription OAuth over the xAI Responses API (bearer from `oauth::grok_build`, refreshed on demand)
- `src/muse_code.rs` — Muse Code provider: Meta subscription over the same Responses shape as `meta.rs` (shares `meta::responses_config`), authenticated with a Model API key minted by `oauth::muse_code`. The key has no published lifetime, so it is stored with `NO_EXPIRY` and never proactively re-minted; a 401/403 from the Responses call triggers exactly one `remint_rejected_key` + retry. ⚠️ Meta documents the subscription as CLI-only; whether a key minted here bills to the subscription or pay-as-you-go is **unverified** (no billed request was made). Because zdx zeroes pricing for every subscription provider (`models.rs` `push_candidate`), muse-code reports $0 cost — if billing is actually pay-as-you-go, that under-reports. Protocol follows the MIT-licensed `oh-my-pi`; that license covers code reuse only, not Meta service authorization.
- `src/openai_compatible.rs` — generic OpenAI-compatible chat-completions client for user-defined "custom" providers (`[providers.custom.<name>]`, the default `api = "chat-completions"`); carries no `ProviderKind`, built directly by the engine from a resolved base URL + API key. Forwards the model spec's thinking level as a top-level `reasoning_effort` string; `off` sends `"none"` because proxied reasoning backends (DeepSeek) think by default, and a LiteLLM-style proxy honors `none` while dropping DeepSeek's `thinking.type = "disabled"` (verified live 2026-09-17). Custom providers with `api = "anthropic"` reuse `AnthropicClient` via `anthropic::api::build_custom` (same budget/effort mapping as the first-party provider, `x-api-key` auth, and `explicit_thinking_off` so `off` sends `thinking: {"type": "disabled"}` instead of omitting it); the engine strips one trailing `/v1` from `base_url` first because the client appends `/v1/messages`. The engine picks the protocol from an explicit per-model `api` override before the provider `api`; keep `explicit_thinking_off` false on every other `AnthropicConfig` site until a backend is verified to accept the value.
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
