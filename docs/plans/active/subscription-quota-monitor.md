> Stage: **active**. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Subscription Quota Monitor

> Show remaining subscription quota (5-hour + weekly limits) for Claude Code and OpenAI Codex inside `zdx monitor`.
> Source feature note: "ZDX Feature — ZDX Monitor" → Usage/Spend panel → "Limits / Subscriptions — remaining quota, spend-cap headroom when the provider exposes it".

# Goals
- Surface each configured subscription provider's live quota in the monitor `Usage` tab: percent used / remaining and time-until-reset, for both the ~5-hour rolling window and the weekly window.
- Cover the two subscriptions the user has today: **Anthropic Claude (claude-cli OAuth)** and **OpenAI Codex (ChatGPT OAuth)**.
- Reuse zdx's own stored OAuth tokens (`$ZDX_HOME/oauth.json`) — no scraping of external `~/.claude` / `~/.codex` files.
- Keep it honest: label the data `Provider (live)` vs stale/unavailable, and mark these integrations as undocumented/best-effort.

# Non-goals
- Billed-USD accuracy for subscriptions — they stay flat-rate `subscription` in the existing token/cost aggregator (`usage-stats-monitor.md` contract). This feature is about **quota headroom**, not spend.
- Reading quota for API-key providers (they have no shared 5h/weekly subscription window; API rate-limit headers are per-org RPM/TPM, a different thing).
- A CLI surface (`zdx stats`) in the MVP — monitor tab first; CLI is a polish item.
- A web dashboard or historical quota charts. Terminal-native, current-snapshot only.
- Predicting/estimating quota locally from token counts. Use the provider's own numbers.

# Design principles
- User journey drives order: get a real number on screen for one provider first, then add the second and polish.
- Verify before building: undocumented endpoints/wire-shapes are confirmed with real zdx tokens (Phase 0) and captured as fixtures before any parser is written.
- Reuse the **pattern**, not the literal worker: copy the monitor's non-blocking worker + `poll_*` cache approach into an **independent** quota job/cache so a slow network fetch can never delay the already-shipped local cost tables.
- Reuse the existing per-provider OAuth `resolve_credentials()` (which already refreshes tokens), but only after credential persistence is made concurrency-safe (see Prerequisite).
- Fail soft: a provider that isn't configured, has no creds, or whose endpoint errors keeps its last-good value labeled `stale` (or shows a bounded `unavailable` reason); never blocks the tab or the other provider.
- Undocumented endpoints are isolated behind one small provider-side client module and clearly marked, so a breakage is contained and easy to disable.

# User journey
1. User has Claude and/or Codex subscriptions configured in zdx (already OAuth-logged-in).
2. User opens `zdx monitor` and switches to the `Usage` tab.
3. Near the top (or in a dedicated "Subscriptions" block), user sees, per provider: 5h window used/remaining + reset-in, and weekly used/remaining + reset-in.
4. User can force a refresh (`R`, already the Usage refresh key) and see updated numbers without aggressive polling.

# Foundations / Already shipped (✅)
Capabilities that already exist and must be reused, not rebuilt.

## Monitor `Usage` tab + non-blocking cache pattern
- What exists: `Section::Usage` (`crates/zdx-monitor/src/app.rs`), `render_usage`/`build_usage_lines`/`usage_table` (`crates/zdx-monitor/src/ui.rs`), lazy compute on first entry, 30s auto-refresh (`USAGE_STALE_AFTER`), background worker (`std::thread` + `mpsc`, `poll_usage_result`), `R` = force refresh, scrollable Paragraph.
- ✅ Demo: `just monitor` → Tab to `Usage` → token/cost tables render without freezing.
- Gaps: no subscription-quota block; the tab only aggregates local thread usage today.

## zdx-owned OAuth tokens + refresh
- What exists: `crates/zdx-providers/src/oauth.rs` stores per-provider creds in `$ZDX_HOME/oauth.json` (`OAuthCredentials { access, refresh, expires, account_id }`, 0600). Per-provider modules `oauth::claude_cli` and `oauth::openai_codex`; `resolve_credentials()` in `anthropic/cli.rs:56` and `openai/codex.rs:77` load + refresh-if-expired and return a fresh access token (Codex also carries `account_id`).
- ✅ Demo: an authenticated `zdx` run on `claude-cli` / `openai-codex` refreshes and uses these tokens.
- Gaps: no code path uses them to hit a usage/quota endpoint (only completions).

## Subscription provider classification
- What exists: `ProviderKind::is_subscription()`/`id()`/`from_id()` (`crates/zdx-providers/src/lib.rs:190`) already distinguishes `claude-cli` / `openai-codex` as subscription providers.
- ✅ Demo: existing stats bucket them as `subscription`.
- Gaps: none — reuse as the gate for "which providers can show quota".

# Research findings (data sources — undocumented, verify live before shipping)
- **Anthropic (claude-cli):** `GET https://api.anthropic.com/api/oauth/usage` with `Authorization: Bearer <access>` + `anthropic-beta: oauth-2025-04-20`. Response observed to carry `five_hour.{utilization,resets_at}` and `seven_day.{utilization,resets_at}` (+ optional `seven_day_sonnet`). Reference impls: `ohugonnot/claude-code-statusline`, `fredrikaverpil/claudeline`.
- **OpenAI Codex:** `GET https://chatgpt.com/backend-api/wham/usage` with the same ChatGPT OAuth bearer zdx already uses + optional `ChatGPT-Account-Id: <account_id>`. Current Codex source: `codex-rs/backend-client/src/client/rate_limit_resets.rs`, parser `codex-rs/codex-api/src/rate_limits.rs` (primary/secondary windows: `used_percent`, `window_minutes`, `resets_at` unix-seconds). Older tools cite `/backend-api/codex/usage` (may be stale). Reference impls: `wakamex/codex-cli-usage`, `lawrencecchen/codex-accounts`.
- **Labeling:** derive window labels from the provider's own `window_minutes`/field names rather than hardcoding "5h/7d", since exact windows can vary by plan. Anthropic evidence shows both legacy flat fields (`five_hour`/`seven_day`) **and** a newer self-describing `limits` array; parse permissively and accept both.

# Prerequisite: monitor reads OAuth tokens read-only (decided)
The monitor must **never refresh or write** OAuth tokens. Today `OAuthCache::save()` truncates + rewrites `oauth.json` with no lock/atomic rename (`crates/zdx-providers/src/oauth.rs:87-125`) and each provider does load→modify→save (`oauth.rs:403-406`, `671-674`), so a monitor refresh racing a live zdx session could clobber a rotated refresh token (**credential loss**). We sidestep that entirely instead of adding cross-process locking.
- **Scope checklist**:
  - [ ] Quota fetchers **load** creds only (read `oauth.json` for the provider's `access` token + `account_id`); they do **not** call the refreshing `resolve_credentials()` and never call `save`.
  - [ ] If the loaded access token is expired (or missing), render `expired · re-login in zdx` (bounded `unavailable` reason) — do not attempt a refresh.
  - [ ] A normal zdx run (TUI/exec/bot) still refreshes tokens as it does today; the monitor picks up the fresh token on its next read. Live quota returns automatically after the next real zdx call.
- **Non-goal (explicitly dropped)**: cross-process lock + atomic-rename persistence. Not needed while the monitor is read-only; revisit only if the monitor ever needs to refresh tokens itself.
- ✅ Demo: with an expired token, the Subscriptions block shows `expired · re-login in zdx` and never mutates `oauth.json` (file mtime unchanged); after any normal zdx turn refreshes the token, the next monitor read shows a live quota.

# MVP phases (ship-shaped, demoable)

## Phase 0: Live protocol probe + fixtures (no UI) — ✅ DONE (2026-07-13)
- **Goal**: Prove the two undocumented endpoints work with **zdx's own** Claude/Codex OAuth tokens, and lock the real response shapes/headers/scopes into fixtures before writing parsers.
- **Scope checklist**:
  - [x] Live `GET` to each endpoint with current zdx creds — both **HTTP 200**.
  - [x] Confirmed zdx-issued OAuth tokens (from `$ZDX_HOME/oauth.json`) grant the usage endpoints — no extra scope/flow needed.
  - [x] Verified Claude identity headers (see results).
  - [x] Verified Codex: `chatgpt-account-id` + `originator: zdx` + `user-agent: zdx/<ver>` on `/backend-api/wham/usage` (200; the zdx originator is accepted).
  - [x] Saved sanitized fixtures: `crates/zdx-providers/tests/fixtures/quota/claude_usage.json` (incl. legacy `five_hour`/`seven_day` **and** the `limits[]` array) + `codex_usage.json` (PII redacted).
  - [x] Provider order decided: **either works; Claude first** — its shape is the simpler direct read (`five_hour`/`seven_day`).

### Phase 0 results (verified live)
- **Claude** — `GET https://api.anthropic.com/api/oauth/usage`, HTTP 200. Headers sent: `Authorization: Bearer`, `anthropic-beta: oauth-2025-04-20`, `anthropic-version: 2023-06-01`, `user-agent: claude-cli/2.1.2 (external, cli)`, `anthropic-dangerous-direct-browser-access: true`, `x-app: cli`. Body has **both** legacy flat fields — `five_hour.{utilization(0..100 float),resets_at(RFC3339)}`, `seven_day.{…}` — and a self-describing `limits[]` (`kind`/`group`/`percent`/`severity`/`resets_at`/`scope`/`is_active`). **Parse `limits[]` as primary** (it carries `is_active` + `group=session|weekly` and scoped weekly rows), fall back to `five_hour`/`seven_day`. `utilization` is a **percent** (0–100), not a 0–1 fraction.
- **Codex** — `GET https://chatgpt.com/backend-api/wham/usage`, HTTP 200. Headers: `Authorization: Bearer`, `chatgpt-account-id: <account_id>`, `originator: zdx`, `user-agent: zdx/<ver>`. Real shape differs from earlier research: `rate_limit.{primary_window,secondary_window}` where each window = `{used_percent(int), limit_window_seconds, reset_after_seconds, reset_at(unix seconds)}` — **not** `window_minutes`. Also `plan_type`, `additional_rate_limits[]` (named/metered limits), `credits`, `spend_control`, `rate_limit_reset_credits.available_count`. Derive the window label from `limit_window_seconds` (e.g. 604800 → "weekly"); use `reset_at` (unix seconds) for reset-in.
- **Note**: zdx's own OAuth tokens are sufficient for both — the read-only Prerequisite holds; no refresh path is exercised here.
- **✅ Demo**: two committed fixture files + these recorded results. Done.

## Phase 1: Live quota for the first provider (from Phase 0) — ✅ DONE (2026-07-13)
- **Goal**: One real subscription (order decided in Phase 0) shows its session (~5h) + weekly window used/remaining + reset-in inside the `Usage` tab.
- **Scope checklist**:
  - [x] New provider-side module `crates/zdx-providers/src/subscription_quota.rs`: neutral type `SubscriptionQuota { provider, plan, windows: Vec<QuotaWindow { label, used_percent: f64, resets_at: Option<DateTime<Utc>> }> }` + wire parsers (fixture-driven from Phase 0). Type + client live here; monitor consumes via the engine re-export (`zdx_engine::providers::subscription_quota`).
  - [x] `fetch_claude_quota()` / `fetch_codex_quota()`: **load** creds read-only (`OAuthCache::load().get(...)`, no refresh/write), Phase 0 identity headers, connect 3s / total 10s timeouts, async reqwest. Expired/missing token → bounded `Expired`/`NotAuthenticated`, no refresh attempt. (Codex folded in here too — the parser was cheap and fixture-verified.)
  - [x] Provider-specific timestamp parsing: Claude `resets_at` = RFC3339; Codex = Unix seconds — parsed explicitly. Past reset → "reset due", never negative.
  - [x] Bounded `QuotaError` (NotAuthenticated / Expired / Unauthorized / RateLimited / Timeout / Http / Incompatible / Transport); raw bodies kept out of TUI (debug-logged only via `tracing`).
  - [x] Monitor: **independent** quota worker + cache (`quota_rx`, `CachedQuotas`/`QuotaEntry`) separate from `usage_rx`; worker owns a **current-thread Tokio runtime** (`Builder::new_current_thread().enable_all().block_on`). `R` starts both jobs; a failed refresh keeps the last-good value (labeled stale).
  - [x] "Subscriptions" block at the top of `build_usage_lines` (`crates/zdx-monitor/src/ui.rs`) with `loading…`/live/`stale (reason)`/`unavailable` labels, reset-in formatting, and Codex `plan` badge.
  - [x] Fetch cadence 5 min (`QUOTA_STALE_AFTER`); on-tick refresh only when Usage tab active + stale; `R` forces a fetch.
- **✅ Demo (verified 2026-07-13)**: live Rust fetch through the real endpoints returned `Claude 5h 6% / weekly 46%` and `Codex [prolite] weekly 6%` with correct reset instants; 6 provider unit tests + full `zdx-providers`/`zdx-monitor` suites (231) green; `cargo clippy` clean.
- **Deferred out of Phase 1**: ~~`Retry-After`/429 backoff min-interval on `R`~~ (✅ done 2026-07-13 — per-provider `quota_backoff` cooldown honors `Retry-After`, default 60s; `R` and on-tick refresh skip a cooling-down provider) and last_success_at/last_attempt_at split — see Polish. A registry-gated provider set is effectively satisfied (only Claude + Codex fetchers exist; not-authenticated rows are hidden).

## Phase 2: Add the second provider — ✅ FOLDED INTO PHASE 1
- Codex (`fetch_codex_quota`) shipped alongside Claude in Phase 1: same neutral `SubscriptionQuota`, `chatgpt-account-id` always sent, windows labeled from `limit_window_seconds`, `NotAuthenticated` rows hidden. No separate work remaining; the `is_subscription()`-enumeration pitfall is avoided (only the two explicit fetchers exist).

# Contracts (guardrails)
- The subscription-quota fetch runs on an **independent** monitor worker with its own cache; it never shares the usage-aggregation job, so a slow/hanging network call cannot delay the local token/cost tables. The UI thread never blocks on network.
- Reuse zdx's `oauth.json` **read-only**; the monitor never refreshes or writes tokens (no external `~/.claude`/`~/.codex` reads either).
- The monitor never mutates `oauth.json`; an expired token renders `expired · re-login in zdx`, not a refresh.
- Tokens and raw provider response bodies are never logged or rendered; only derived percentages/reset times and bounded error categories are shown.
- A failing/absent provider keeps its last-good value labeled `stale` (or a bounded `unavailable` reason) and never breaks the existing token/cost tables or the other provider.
- Existing `Usage` tab behavior (token/cost aggregation, scroll, `R`, 30s auto-refresh) must not regress.
- Subscription providers remain `subscription` (flat-rate) in the spend aggregator — this feature adds quota only, changes no cost math.

# Key decisions (decide early)
- **Live probe before parsers**: Phase 0 captures real fixtures + required headers + token scope; parsers are written against those, not assumptions. Provider order is chosen from Phase 0 results (Codex may be lower-risk than Claude-first).
- **Reuse zdx OAuth, read-only**: the monitor loads tokens from `$ZDX_HOME/oauth.json` but never refreshes or writes them — sidesteps the concurrent-write/credential-loss risk entirely (no lock/atomic-rename work needed). Expired token → `expired · re-login in zdx`; live quota returns after the next normal zdx run refreshes the token.
- **Value shape + location**: one neutral `SubscriptionQuota { windows: [{ label, used_percent, resets_at: Option }] }`, defined in `zdx-providers` (not `zdx-types`/engine), consumed by the monitor via the engine re-export.
- **Async seam**: the quota worker owns a current-thread Tokio runtime and uses async reqwest; the monitor crate takes an explicit `zdx-providers`/`zdx-engine` dependency for this. No second blocking HTTP stack.
- **Independent cache**: separate `quota_rx` + `CachedSubscriptionQuotas` with `last_success_at`/`last_attempt_at`/`error`; not merged into the usage cache.
- **Cadence**: ≥5-minute network refresh; `R` respects a ≥60s hard min-interval; respect `Retry-After`/429 backoff.
- **Codex account id**: `resolve_credentials()` returns a non-optional `account_id` (`openai/codex.rs:89-107`), so `ChatGPT-Account-Id` is **always** sent — current-zdx-account only; multi-account is out of scope (the cache holds one record per provider, `oauth.rs:49-57`).
- **Provider gating**: explicit supported-quota-fetcher registry ∩ credential presence — not the full `is_subscription()` set, not config-enablement alone.
- **Surface**: monitor `Usage` tab in MVP (a `zdx quota`/`--json` surface is deferred to polish).

# Testing
- Phase 0: committed sanitized fixtures for both providers (incl. Anthropic legacy vs `limits`-array variants where seen).
- Manual smoke demos per phase (the ✅ Demo lines).
- Unit tests: wire-shape → `SubscriptionQuota` mapping for both providers from fixtures (no network); Claude RFC3339 vs Codex Unix-seconds parsing explicitly; reset-in formatting incl. past-reset → "reset due"; expired/missing token → bounded `expired`/`not-authenticated`, not panic, and **no write** to `oauth.json`.
- HTTP-boundary tests via a mock server: asserts URL, required headers, timeout behavior, 401/429 handling, and that a failed fetch preserves the previous cache.
- Contract test: monitor still renders token/cost tables when both quota fetches fail/hang; quota fetch never writes `oauth.json`.
- Verification: `just ci-fast` during iteration; `cargo nextest run -p zdx-monitor` and `-p zdx-providers`; `just test` before wrapping up.

# Polish rounds (after MVP)
## Polish round 1: CLI + machine-readable
- Add quota to a CLI surface (extend `zdx stats` or a small `zdx quota`) with `--json`.
- ✅ Check-in demo: `zdx quota --json` returns per-provider windows with `used_percent` + `resets_at`.

## Polish round 2: Warnings + at-a-glance
- [x] Color/threshold cues — window used% is green (<75%), yellow (75–89%), red (≥90%) via `quota_percent_color` in `crates/zdx-monitor/src/ui.rs`; locked by unit tests (`percent_color_thresholds`, `near_limit_window_renders_red_span`). ✅ DONE (2026-07-13)
- [ ] Compact one-line summary string usable elsewhere (e.g. bot `/status`).
- ✅ Check-in demo: near-limit windows render highlighted; a one-line summary string is available.

# Later / Deferred
- Additional subscription providers (e.g. `grok-build`, Gemini OAuth) — add a `fetch_*` + registry entry when the user has them and an endpoint is known.
- Extra/scoped limits + credits (Anthropic scoped weekly limits like `seven_day_sonnet`, Codex named/metered limits, reset-credit availability, spend-cap headroom) — parse unknown fields permissively but only render the primary session + aggregate weekly windows in MVP; surface the rest later if useful.
- Multi-account support — the OAuth cache holds one record per provider; revisit only if multiple accounts per provider are needed.
- Passive header/SSE-based quota capture (e.g. Codex `x-codex-*` response headers, `codex.rate_limits` events; Anthropic `anthropic-ratelimit-*`) piggybacked on real completions to avoid extra calls — revisit if the dedicated endpoints prove unreliable or rate-limited.
- Historical quota trends / charts — only if the user wants tracking over time.
