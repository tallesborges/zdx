//! Live subscription-quota readers for flat-rate providers.
//!
//! Reads the (mostly undocumented) usage/quota endpoints that Claude Code,
//! Codex CLI, Google Antigravity, Grok Build, and `OpenCode` Go expose, using
//! zdx's configured credentials **read-only** (OAuth tokens are never refreshed
//! or written from here — see the subscription-quota-monitor plan).
//!
//! These endpoints are undocumented and may change; parsing is permissive and
//! failures degrade to a bounded [`QuotaError`] rather than propagating raw
//! provider response bodies (which are never logged or surfaced).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use futures_util::future::join_all;
use serde::Deserialize;
pub use zdx_types::{
    QuotaError, QuotaWindow, SubscriptionQuota, SubscriptionQuotaResult, SubscriptionQuotaSnapshot,
};

use crate::ProviderKind;
pub use crate::oauth::account_cache_key;
use crate::oauth::{
    OAuthCredentials, claude_cli, google_antigravity, grok_build, muse_code, openai_codex,
};

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Claude's OAuth profile. The usage endpoint carries no plan metadata, so the
/// plan label is read from here instead (never inferred from utilization).
const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const ANTIGRAVITY_QUOTA_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
const GROK_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// Provider id for the Claude (claude-cli) subscription.
pub const PROVIDER_CLAUDE: &str = claude_cli::PROVIDER_KEY;
/// Provider id for the Codex (openai-codex) subscription.
pub const PROVIDER_CODEX: &str = openai_codex::PROVIDER_KEY;
/// Provider id for the Google Antigravity subscription.
pub const PROVIDER_ANTIGRAVITY: &str = google_antigravity::PROVIDER_KEY;
/// Provider id for the Grok Build (xAI) subscription.
pub const PROVIDER_GROK: &str = grok_build::PROVIDER_KEY;
/// Provider id for the `OpenCode` Go subscription.
pub const PROVIDER_OPENCODE_GO: &str = "opencode-go";
/// Provider id for the Muse Code (Meta) subscription.
pub const PROVIDER_MUSE_CODE: &str = muse_code::PROVIDER_KEY;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A boxed quota-fetch future.
pub type QuotaFuture = Pin<Box<dyn Future<Output = Result<SubscriptionQuota, QuotaError>> + Send>>;
/// A read-only quota fetcher for one provider account (`None` = default account).
pub type QuotaFetcher = fn(Option<String>) -> QuotaFuture;

/// Human-friendly display name for a subscription provider id.
#[must_use]
pub fn provider_display(provider: &str) -> &str {
    match provider {
        PROVIDER_CLAUDE => "Claude",
        PROVIDER_CODEX => "Codex",
        PROVIDER_ANTIGRAVITY => "Antigravity",
        PROVIDER_GROK => "Grok",
        PROVIDER_OPENCODE_GO => "OpenCode Go",
        PROVIDER_MUSE_CODE => "Muse Code",
        other => other,
    }
}

/// Registry of supported subscription-quota fetchers, keyed by provider id.
/// The monitor iterates this (intersected with credential presence) — adding a
/// provider is one new `fetch_*` + one entry here, no new render code.
pub const FETCHERS: &[(&str, QuotaFetcher)] = &[
    (PROVIDER_CLAUDE, |account| {
        Box::pin(fetch_claude_quota(account))
    }),
    (PROVIDER_CODEX, |account| {
        Box::pin(fetch_codex_quota(account))
    }),
    (PROVIDER_ANTIGRAVITY, |account| {
        Box::pin(fetch_antigravity_quota(account))
    }),
    (PROVIDER_GROK, |account| Box::pin(fetch_grok_quota(account))),
    (PROVIDER_OPENCODE_GO, |account| {
        Box::pin(fetch_opencode_go_quota(account))
    }),
    (PROVIDER_MUSE_CODE, |account| {
        Box::pin(fetch_muse_code_quota(account))
    }),
];

/// Display label for a provider account (`Claude` or `Claude @parity`).
#[must_use]
pub fn account_display(provider: &str, account: Option<&str>) -> String {
    match account {
        Some(name) => format!("{} @{name}", provider_display(provider)),
        None => provider_display(provider).to_string(),
    }
}

/// Lists every stored `(provider, account, fetcher)` triple.
///
/// Providers with no stored credentials still yield their default account so
/// callers can render a "not logged in" row.
///
/// # Errors
/// Returns an error if the OAuth cache cannot be read.
pub fn stored_accounts() -> anyhow::Result<Vec<(&'static str, Option<String>, QuotaFetcher)>> {
    let cache = crate::oauth::OAuthCache::load()?;
    Ok(FETCHERS
        .iter()
        .flat_map(|(provider, fetch)| {
            let accounts = cache.accounts(provider);
            if accounts.is_empty() {
                vec![(*provider, None, *fetch)]
            } else {
                accounts
                    .into_iter()
                    .map(|account| (*provider, account, *fetch))
                    .collect()
            }
        })
        .collect())
}

/// Fetches every supported stored account concurrently into one shared snapshot.
///
/// Provider failures stay attached to their account instead of failing the
/// snapshot. Credentials are read-only: fetchers never refresh or write tokens.
///
/// # Errors
/// Returns an error only when the OAuth account store cannot be read.
pub async fn fetch_snapshot() -> anyhow::Result<SubscriptionQuotaSnapshot> {
    let pending = stored_accounts()?
        .into_iter()
        .map(|(provider, account, fetch)| async move {
            SubscriptionQuotaResult {
                quota: fetch(account.clone()).await,
                provider,
                account,
            }
        });
    Ok(SubscriptionQuotaSnapshot {
        providers: join_all(pending).await,
    })
}

/// Loads stored OAuth credentials **read-only** (no refresh, no write); maps a
/// missing store or missing provider entry to [`QuotaError::NotAuthenticated`].
fn require_creds(
    loaded: Result<Option<OAuthCredentials>, anyhow::Error>,
) -> Result<OAuthCredentials, QuotaError> {
    loaded
        .map_err(|err| {
            tracing::debug!(error = %format!("{err:#}"), "Quota: failed to load OAuth cache");
            QuotaError::NotAuthenticated
        })?
        .ok_or(QuotaError::NotAuthenticated)
}

fn quota_client() -> Result<reqwest::Client, QuotaError> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| {
            tracing::debug!(
                error = &err as &dyn std::error::Error,
                "Quota: failed to build HTTP client"
            );
            QuotaError::Transport
        })
}

fn classify_send(err: &reqwest::Error) -> QuotaError {
    if err.is_timeout() {
        QuotaError::Timeout
    } else {
        QuotaError::Transport
    }
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

fn error_for_status(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> QuotaError {
    match status.as_u16() {
        401 | 403 => QuotaError::Unauthorized,
        429 => QuotaError::RateLimited {
            retry_after_secs: parse_retry_after(headers),
        },
        code => QuotaError::Http(code),
    }
}

/// Fetches the Claude (claude-cli) subscription quota.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing/expired creds or endpoint failure.
pub async fn fetch_claude_quota(account: Option<String>) -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(claude_cli::load_credentials(account.as_deref()))?;
    if creds.is_expired() {
        return Err(QuotaError::Expired);
    }

    let client = quota_client()?;
    // Concurrent so the extra metadata call costs no extra latency. The plan
    // is best-effort: a failed profile read degrades to an unlabelled quota
    // rather than failing the whole fetch.
    let (usage, plan) = futures_util::future::join(
        claude_request(&client, &creds.access, CLAUDE_USAGE_URL).send(),
        fetch_claude_plan(&client, &creds.access),
    )
    .await;

    let resp = usage.map_err(|e| classify_send(&e))?;

    if !resp.status().is_success() {
        return Err(error_for_status(resp.status(), resp.headers()));
    }

    let wire: ClaudeUsageWire = resp.json().await.map_err(|err| {
        tracing::debug!(
            provider = "claude",
            error = &err as &dyn std::error::Error,
            "Quota: response decode failed"
        );
        QuotaError::Incompatible
    })?;
    parse_claude(&wire, plan).ok_or(QuotaError::Incompatible)
}

/// Fetches the Codex (openai-codex) subscription quota.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing/expired creds or endpoint failure.
pub async fn fetch_codex_quota(account: Option<String>) -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(openai_codex::load_credentials(account.as_deref()))?;
    if creds.is_expired() {
        return Err(QuotaError::Expired);
    }
    let account_id = creds
        .account_id
        .clone()
        .ok_or(QuotaError::NotAuthenticated)?;

    let resp = quota_client()?
        .get(CODEX_USAGE_URL)
        .header("Authorization", format!("Bearer {}", creds.access))
        .header("chatgpt-account-id", account_id)
        .header("originator", "zdx")
        .header("user-agent", concat!("zdx/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|e| classify_send(&e))?;

    if !resp.status().is_success() {
        return Err(error_for_status(resp.status(), resp.headers()));
    }

    let wire: CodexUsageWire = resp.json().await.map_err(|err| {
        tracing::debug!(
            provider = "codex",
            error = &err as &dyn std::error::Error,
            "Quota: response decode failed"
        );
        QuotaError::Incompatible
    })?;
    parse_codex(&wire).ok_or(QuotaError::Incompatible)
}

/// Fetches the Google Antigravity subscription quota.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing/expired creds or endpoint failure.
pub async fn fetch_antigravity_quota(
    account: Option<String>,
) -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(google_antigravity::load_credentials(account.as_deref()))?;
    if creds.is_expired() {
        return Err(QuotaError::Expired);
    }
    let body = serde_json::json!({ "project": creds.account_id.clone().unwrap_or_default() });

    let resp = quota_client()?
        .post(ANTIGRAVITY_QUOTA_URL)
        .header("Authorization", format!("Bearer {}", creds.access))
        .header("Accept", "application/json")
        // The quota-summary endpoint requires an Antigravity-style UA (a plain UA 401s).
        .header("user-agent", "antigravity/cli/1.0.0")
        .json(&body)
        .send()
        .await
        .map_err(|e| classify_send(&e))?;

    if !resp.status().is_success() {
        return Err(error_for_status(resp.status(), resp.headers()));
    }

    let wire: AntigravityUsageWire = resp.json().await.map_err(|err| {
        tracing::debug!(
            provider = "google-antigravity",
            error = &err as &dyn std::error::Error,
            "Quota: response decode failed"
        );
        QuotaError::Incompatible
    })?;
    parse_antigravity(&wire).ok_or(QuotaError::Incompatible)
}

/// Fetches the Grok Build (xAI) subscription quota.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing/expired creds or endpoint failure.
pub async fn fetch_grok_quota(account: Option<String>) -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(grok_build::load_credentials(account.as_deref()))?;
    if creds.is_expired() {
        return Err(QuotaError::Expired);
    }

    let resp = quota_client()?
        .get(GROK_BILLING_URL)
        .header("Authorization", format!("Bearer {}", creds.access))
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("x-grok-client-version", "1.0.0")
        .header("x-grok-client-mode", "interactive")
        .send()
        .await
        .map_err(|e| classify_send(&e))?;

    if !resp.status().is_success() {
        return Err(error_for_status(resp.status(), resp.headers()));
    }

    let wire: GrokBillingWire = resp.json().await.map_err(|err| {
        tracing::debug!(
            provider = "grok",
            error = &err as &dyn std::error::Error,
            "Quota: response decode failed"
        );
        QuotaError::Incompatible
    })?;
    parse_grok(&wire).ok_or(QuotaError::Incompatible)
}

/// Fetches the `OpenCode` Go subscription quota using its configured API key.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on a missing API key or endpoint failure.
pub async fn fetch_opencode_go_quota(
    _account: Option<String>,
) -> Result<SubscriptionQuota, QuotaError> {
    let api_key = ProviderKind::OpencodeGo
        .resolve_api_key(None)
        .map_err(|err| {
            tracing::debug!(provider = "opencode-go", error = %format!("{err:#}"), "Quota: failed to resolve API key");
            QuotaError::NotAuthenticated
        })?;

    let resp = quota_client()?
        .get(OPENCODE_GO_USAGE_URL)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| classify_send(&e))?;

    if !resp.status().is_success() {
        return Err(error_for_status(resp.status(), resp.headers()));
    }

    let wire: OpenCodeGoUsageWire = resp.json().await.map_err(|err| {
        tracing::debug!(
            provider = "opencode-go",
            error = &err as &dyn std::error::Error,
            "Quota: response decode failed"
        );
        QuotaError::Incompatible
    })?;
    parse_opencode_go(&wire).ok_or(QuotaError::Incompatible)
}

/// Fetches the Muse Code (Meta) subscription allowance.
///
/// Meta publishes no usage endpoint, so allowance is read from the same
/// `POST /muse-code/key` call used at login, sent with the stored **identity
/// token** and `onboard: false`. The minted key in the response is discarded:
/// this path stays read-only and never writes to the OAuth cache.
///
/// Unlike the other fetchers this does not short-circuit on
/// [`OAuthCredentials::is_expired`], because that timestamp tracks the cached
/// minted key, not the identity token this call actually presents.
///
/// The `subs_usage` shape is undocumented by Meta and may change without
/// notice; an unexpected payload degrades to [`QuotaError::Incompatible`].
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing creds or endpoint failure.
pub async fn fetch_muse_code_quota(
    account: Option<String>,
) -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(muse_code::load_credentials(account.as_deref()))?;

    let resp = quota_client()?
        .post(muse_code::KEY_MINT_URL)
        .header("Authorization", format!("Bearer {}", creds.refresh))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("x-api-version", "1.0.0")
        // Meta's auth host turns away clients with no user agent; the mint
        // host currently does not, but send one so this path does not start
        // failing if that gating is extended.
        .header("user-agent", crate::shared::USER_AGENT)
        .body("{}")
        .send()
        .await
        .map_err(|e| classify_send(&e))?;

    if !resp.status().is_success() {
        return Err(error_for_status(resp.status(), resp.headers()));
    }

    let wire: muse_code::MuseCodeKeyResponse = resp.json().await.map_err(|err| {
        tracing::debug!(
            provider = "muse-code",
            error = &err as &dyn std::error::Error,
            "Quota: response decode failed"
        );
        QuotaError::Incompatible
    })?;

    if wire.is_subs_active == Some(false) {
        return Err(QuotaError::Unauthorized);
    }
    parse_muse_code(&wire).ok_or(QuotaError::Incompatible)
}

/// Parses Meta's `resets_at`, which is either epoch seconds or an RFC 3339
/// string depending on the window.
fn parse_muse_code_reset(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    match value? {
        serde_json::Value::String(s) => parse_rfc3339(Some(s)),
        serde_json::Value::Number(n) => {
            let secs = n.as_f64()?;
            if secs <= 0.0 {
                return None;
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "epoch seconds are well within i64"
            )]
            Utc.timestamp_opt(secs as i64, 0).single()
        }
        _ => None,
    }
}

/// Formats a rolling-window label from its duration (`300` mins -> `5h`).
fn muse_code_window_label(duration_mins: Option<f64>) -> String {
    match duration_mins {
        Some(mins) if mins > 0.0 => {
            let mins = mins.round();
            if (mins % 60.0).abs() < f64::EPSILON {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "window durations are small"
                )]
                let hours = (mins / 60.0) as i64;
                format!("{hours}h")
            } else {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "window durations are small"
                )]
                let mins = mins as i64;
                format!("{mins}m")
            }
        }
        _ => "rolling".to_string(),
    }
}

fn muse_code_window(
    window: Option<&muse_code::SubscriptionWindow>,
    label: Option<&str>,
) -> Option<QuotaWindow> {
    let window = window?;
    let used_percent = window.used_percent?;
    if !used_percent.is_finite() || used_percent < 0.0 {
        return None;
    }
    Some(QuotaWindow {
        label: label.map_or_else(
            || muse_code_window_label(window.window_duration_mins),
            ToString::to_string,
        ),
        used_percent,
        resets_at: parse_muse_code_reset(window.resets_at.as_ref()),
        scope: None,
    })
}

/// Parses the Muse Code key payload into a neutral snapshot.
fn parse_muse_code(wire: &muse_code::MuseCodeKeyResponse) -> Option<SubscriptionQuota> {
    let usage = wire.subs_usage.as_ref()?;
    let windows: Vec<QuotaWindow> = [
        muse_code_window(usage.window.as_ref(), None),
        muse_code_window(usage.weekly.as_ref(), Some("weekly")),
    ]
    .into_iter()
    .flatten()
    .collect();

    if windows.is_empty() {
        return None;
    }
    Some(SubscriptionQuota {
        plan: wire
            .subs_tier_name
            .as_deref()
            .or(wire.subs_tier_id.as_deref())
            .map(str::trim)
            .filter(|tier| !tier.is_empty())
            .map(ToString::to_string),
        windows,
    })
}

/// Builds a Claude OAuth request carrying the headers the endpoints expect.
fn claude_request(client: &reqwest::Client, access: &str, url: &str) -> reqwest::RequestBuilder {
    client
        .get(url)
        .header("Authorization", format!("Bearer {access}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .header("user-agent", "claude-cli/2.1.2 (external, cli)")
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("x-app", "cli")
}

/// Reads the declared plan from Claude's OAuth profile.
///
/// Best-effort: any failure yields `None` so the quota still renders.
async fn fetch_claude_plan(client: &reqwest::Client, access: &str) -> Option<String> {
    let resp = claude_request(client, access, CLAUDE_PROFILE_URL)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        tracing::debug!(
            provider = "claude",
            status = resp.status().as_u16(),
            "Quota: profile lookup failed; continuing without a plan label"
        );
        return None;
    }
    let wire: ClaudeProfileWire = resp.json().await.ok()?;
    wire.plan_label()
}

// --- Claude wire shape ---

#[derive(Debug, Deserialize)]
struct ClaudeProfileWire {
    organization: Option<ClaudeOrganization>,
}

#[derive(Debug, Deserialize)]
struct ClaudeOrganization {
    /// e.g. `default_claude_max_20x` — the only field distinguishing Max 5x
    /// from Max 20x, so it is preferred over the coarser `organization_type`.
    rate_limit_tier: Option<String>,
    /// e.g. `claude_max`, `claude_pro`.
    organization_type: Option<String>,
}

impl ClaudeProfileWire {
    /// The declared plan, preferring the precise tier.
    ///
    /// Only the uninformative `default_` prefix is stripped; the rest is kept
    /// verbatim so a new tier shows up as-is instead of being mapped to a
    /// stale guess.
    fn plan_label(&self) -> Option<String> {
        let org = self.organization.as_ref()?;
        org.rate_limit_tier
            .as_deref()
            .or(org.organization_type.as_deref())
            .map(str::trim)
            .filter(|tier| !tier.is_empty())
            .map(|tier| tier.strip_prefix("default_").unwrap_or(tier).to_string())
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeUsageWire {
    five_hour: Option<ClaudeFlatWindow>,
    seven_day: Option<ClaudeFlatWindow>,
    #[serde(default)]
    limits: Vec<ClaudeLimit>,
}

#[derive(Debug, Deserialize)]
struct ClaudeFlatWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

impl ClaudeFlatWindow {
    fn to_window(&self, label: &str) -> QuotaWindow {
        QuotaWindow {
            label: label.to_string(),
            used_percent: self.utilization.unwrap_or(0.0),
            resets_at: parse_rfc3339(self.resets_at.as_deref()),
            scope: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeLimit {
    group: Option<String>,
    percent: Option<f64>,
    resets_at: Option<String>,
    scope: Option<ClaudeScope>,
}

#[derive(Debug, Deserialize)]
struct ClaudeScope {
    model: Option<ClaudeScopeModel>,
}

#[derive(Debug, Deserialize)]
struct ClaudeScopeModel {
    display_name: Option<String>,
}

fn parse_rfc3339(s: Option<&str>) -> Option<DateTime<Utc>> {
    let s = s?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Parses the Claude usage payload into a neutral snapshot.
///
/// Prefers the self-describing `limits[]` array (unscoped session + weekly),
/// falling back to the legacy `five_hour`/`seven_day` fields.
fn parse_claude(wire: &ClaudeUsageWire, plan: Option<String>) -> Option<SubscriptionQuota> {
    let mut windows = Vec::new();

    for limit in &wire.limits {
        let scoped_model = limit
            .scope
            .as_ref()
            .and_then(|s| s.model.as_ref())
            .and_then(|m| m.display_name.as_deref());
        // Session + weekly (both the account-wide window and any per-model
        // weekly limit, e.g. "Fable"). Other scoped limits are skipped.
        let (label, scope) = match (limit.group.as_deref(), scoped_model) {
            (Some("session"), None) => ("5h", None),
            (Some("weekly"), None) => ("weekly", None),
            (Some("weekly"), Some(model)) => ("weekly", Some(model.to_string())),
            _ => continue,
        };
        windows.push(QuotaWindow {
            label: label.to_string(),
            used_percent: limit.percent.unwrap_or(0.0),
            resets_at: parse_rfc3339(limit.resets_at.as_deref()),
            scope,
        });
    }

    if windows.is_empty() {
        if let Some(w) = &wire.five_hour {
            windows.push(w.to_window("5h"));
        }
        if let Some(w) = &wire.seven_day {
            windows.push(w.to_window("weekly"));
        }
    }

    if windows.is_empty() {
        return None;
    }
    Some(SubscriptionQuota { plan, windows })
}

// --- Codex wire shape ---

#[derive(Debug, Deserialize)]
struct CodexUsageWire {
    plan_type: Option<String>,
    rate_limit: Option<CodexRateLimit>,
    spend_control: Option<CodexSpendControl>,
}

#[derive(Debug, Deserialize)]
struct CodexRateLimit {
    primary_window: Option<CodexWindow>,
    secondary_window: Option<CodexWindow>,
}

#[derive(Debug, Deserialize)]
struct CodexSpendControl {
    individual_limit: Option<CodexSpendLimit>,
}

/// Dollar amounts arrive as strings; `used_percent` arrives as a number.
#[derive(Debug, Deserialize)]
struct CodexSpendLimit {
    limit: Option<String>,
    used: Option<String>,
    used_percent: Option<f64>,
    reset_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CodexWindow {
    used_percent: Option<f64>,
    limit_window_seconds: Option<i64>,
    reset_at: Option<i64>,
}

fn codex_window_label(seconds: Option<i64>) -> &'static str {
    match seconds {
        Some(18_000) => "5h",
        Some(604_800) => "weekly",
        _ => "window",
    }
}

fn codex_window(w: &CodexWindow) -> QuotaWindow {
    QuotaWindow {
        label: codex_window_label(w.limit_window_seconds).to_string(),
        used_percent: w.used_percent.unwrap_or(0.0),
        resets_at: w.reset_at.and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
        scope: None,
    }
}

/// Builds the spend-budget window used by business/workspace plans.
///
/// This is a spend budget in dollars, not a rolling rate-limit window, so it is
/// labelled `spend` and carries the raw dollar figures as the window scope.
/// `used_percent` from the payload is integer-rounded, so the percentage is
/// derived from `used`/`limit` whenever both parse.
fn codex_spend_window(spend: &CodexSpendLimit) -> Option<QuotaWindow> {
    let limit = spend.limit.as_deref().and_then(|v| v.parse::<f64>().ok());
    let used = spend.used.as_deref().and_then(|v| v.parse::<f64>().ok());
    let (used_percent, scope) = match (used, limit) {
        (Some(used), Some(limit)) if limit > 0.0 => (
            used / limit * 100.0,
            Some(format!("${used:.2} of ${limit:.2}")),
        ),
        _ => (spend.used_percent?, None),
    };
    Some(QuotaWindow {
        label: "spend".to_string(),
        used_percent,
        resets_at: spend
            .reset_at
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
        scope,
    })
}

/// Parses the Codex usage payload into a neutral snapshot.
///
/// Consumer plans report rolling `rate_limit` windows. Business/workspace plans
/// report `rate_limit: null` and expose a dollar budget under
/// `spend_control.individual_limit` instead, which is used as the fallback.
fn parse_codex(wire: &CodexUsageWire) -> Option<SubscriptionQuota> {
    let mut windows = Vec::new();
    if let Some(rate_limit) = &wire.rate_limit {
        if let Some(w) = &rate_limit.primary_window {
            windows.push(codex_window(w));
        }
        if let Some(w) = &rate_limit.secondary_window {
            windows.push(codex_window(w));
        }
    }
    if windows.is_empty() {
        let spend = wire.spend_control.as_ref()?.individual_limit.as_ref()?;
        windows.push(codex_spend_window(spend)?);
    }
    Some(SubscriptionQuota {
        plan: wire.plan_type.clone(),
        windows,
    })
}

// --- Antigravity wire shape ---

#[derive(Debug, Deserialize)]
struct AntigravityUsageWire {
    #[serde(default)]
    groups: Vec<AntigravityGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityGroup {
    display_name: Option<String>,
    #[serde(default)]
    buckets: Vec<AntigravityBucket>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityBucket {
    window: Option<String>,
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

fn antigravity_window_label(window: Option<&str>) -> &'static str {
    match window {
        Some("5h") => "5h",
        _ => "weekly",
    }
}

/// Parses the Antigravity quota summary into weekly/5h windows for the Gemini
/// model group only (other groups like Claude/GPT are not used). The group name
/// rides on `QuotaWindow.scope`.
fn parse_antigravity(wire: &AntigravityUsageWire) -> Option<SubscriptionQuota> {
    let mut windows = Vec::new();
    for group in &wire.groups {
        let is_gemini = group
            .display_name
            .as_deref()
            .is_some_and(|name| name.to_ascii_lowercase().contains("gemini"));
        if !is_gemini {
            continue;
        }
        for bucket in &group.buckets {
            let used = (1.0 - bucket.remaining_fraction.unwrap_or(1.0)) * 100.0;
            windows.push(QuotaWindow {
                label: antigravity_window_label(bucket.window.as_deref()).to_string(),
                used_percent: used,
                resets_at: parse_rfc3339(bucket.reset_time.as_deref()),
                scope: group.display_name.clone(),
            });
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(SubscriptionQuota {
        plan: None,
        windows,
    })
}

// --- Grok wire shape ---

#[derive(Debug, Deserialize)]
struct GrokBillingWire {
    config: Option<GrokConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokConfig {
    credit_usage_percent: Option<f64>,
    current_period: Option<GrokPeriod>,
    subscription_tier: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokPeriod {
    #[serde(rename = "type")]
    period_type: Option<String>,
    end: Option<String>,
}

fn grok_period_label(period_type: Option<&str>) -> &'static str {
    match period_type {
        Some("USAGE_PERIOD_TYPE_MONTHLY") => "monthly",
        _ => "weekly",
    }
}

/// Parses the Grok Build credits config into a single credit-usage window.
fn parse_grok(wire: &GrokBillingWire) -> Option<SubscriptionQuota> {
    let config = wire.config.as_ref()?;
    // proto3 JSON omits zero-valued scalars, so absent usage means 0%.
    let used_percent = config.credit_usage_percent.unwrap_or(0.0);
    let (label, reset) = match &config.current_period {
        Some(p) => (
            grok_period_label(p.period_type.as_deref()),
            p.end.as_deref(),
        ),
        None => ("weekly", None),
    };
    Some(SubscriptionQuota {
        plan: config.subscription_tier.clone(),
        windows: vec![QuotaWindow {
            label: label.to_string(),
            used_percent,
            resets_at: parse_rfc3339(reset),
            scope: None,
        }],
    })
}

// --- OpenCode Go wire shape ---

#[derive(Debug, Deserialize)]
struct OpenCodeGoUsageWire {
    usage: Option<OpenCodeGoWindows>,
}

#[derive(Debug, Deserialize)]
struct OpenCodeGoWindows {
    rolling: Option<OpenCodeGoWindow>,
    weekly: Option<OpenCodeGoWindow>,
    monthly: Option<OpenCodeGoWindow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeGoWindow {
    status: Option<String>,
    percent: Option<f64>,
    resets_at: Option<String>,
}

fn opencode_go_window(window: Option<&OpenCodeGoWindow>, label: &str) -> Option<QuotaWindow> {
    let window = window?;
    if window.status.as_deref() != Some("ok") {
        return None;
    }
    let used_percent = window.percent?;
    if !used_percent.is_finite() || !(0.0..=100.0).contains(&used_percent) {
        return None;
    }
    Some(QuotaWindow {
        label: label.to_string(),
        used_percent,
        resets_at: parse_rfc3339(window.resets_at.as_deref()),
        scope: None,
    })
}

fn parse_opencode_go(wire: &OpenCodeGoUsageWire) -> Option<SubscriptionQuota> {
    let usage = wire.usage.as_ref()?;
    let windows = [
        opencode_go_window(usage.rolling.as_ref(), "5h"),
        opencode_go_window(usage.weekly.as_ref(), "weekly"),
        opencode_go_window(usage.monthly.as_ref(), "monthly"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if windows.is_empty() {
        return None;
    }
    Some(SubscriptionQuota {
        plan: None,
        windows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/quota")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    }

    #[test]
    fn parses_claude_limits_array() {
        let wire: ClaudeUsageWire =
            serde_json::from_str(&load_fixture("claude_usage.json")).unwrap();
        let quota = parse_claude(&wire, None).expect("claude quota");
        // Session + account-wide weekly + the model-scoped weekly (e.g. "Fable").
        assert_eq!(quota.windows.len(), 3);
        let session = &quota.windows[0];
        assert_eq!(session.label, "5h");
        assert!((session.used_percent - 0.0).abs() < f64::EPSILON);
        assert!(session.scope.is_none());
        let weekly = &quota.windows[1];
        assert_eq!(weekly.label, "weekly");
        assert!((weekly.used_percent - 45.0).abs() < f64::EPSILON);
        assert!(weekly.resets_at.is_some());
        assert!(weekly.scope.is_none());
        // The scoped weekly window carries the model display name.
        let scoped = &quota.windows[2];
        assert_eq!(scoped.label, "weekly");
        assert_eq!(scoped.scope.as_deref(), Some("Opus"));
    }

    #[test]
    fn claude_falls_back_to_flat_fields_when_no_limits() {
        let wire = ClaudeUsageWire {
            five_hour: Some(ClaudeFlatWindow {
                utilization: Some(12.5),
                resets_at: Some("2026-07-14T01:09:59.535017+00:00".to_string()),
            }),
            seven_day: Some(ClaudeFlatWindow {
                utilization: Some(80.0),
                resets_at: None,
            }),
            limits: Vec::new(),
        };
        let quota = parse_claude(&wire, None).expect("fallback quota");
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].label, "5h");
        assert!((quota.windows[0].used_percent - 12.5).abs() < f64::EPSILON);
        assert!(quota.windows[0].resets_at.is_some());
        assert_eq!(quota.windows[1].label, "weekly");
        assert!(quota.windows[1].resets_at.is_none());
    }

    #[test]
    fn parses_codex_windows_with_unix_reset() {
        let wire: CodexUsageWire = serde_json::from_str(&load_fixture("codex_usage.json")).unwrap();
        let quota = parse_codex(&wire).expect("codex quota");
        assert_eq!(quota.plan.as_deref(), Some("prolite"));
        assert_eq!(quota.windows.len(), 1);
        let w = &quota.windows[0];
        assert_eq!(w.label, "weekly");
        assert!((w.used_percent - 6.0).abs() < f64::EPSILON);
        // 1784502675 → a valid UTC instant.
        assert_eq!(w.resets_at, Utc.timestamp_opt(1_784_502_675, 0).single());
    }

    #[test]
    fn parses_antigravity_grouped_weekly_and_5h() {
        let wire: AntigravityUsageWire =
            serde_json::from_str(&load_fixture("antigravity_usage.json")).unwrap();
        let quota = parse_antigravity(&wire).expect("antigravity quota");
        // Only the Gemini group is kept: weekly + 5h = 2 windows (Claude/GPT dropped).
        assert_eq!(quota.windows.len(), 2);
        assert!(
            quota
                .windows
                .iter()
                .all(|w| w.scope.as_deref() == Some("Gemini Models"))
        );
        let gemini_weekly = &quota.windows[0];
        assert_eq!(gemini_weekly.label, "weekly");
        // remainingFraction 0.9971372 → ~0.29% used.
        assert!(gemini_weekly.used_percent < 1.0);
        assert!(gemini_weekly.resets_at.is_some());
        let gemini_5h = &quota.windows[1];
        assert_eq!(gemini_5h.label, "5h");
        // remainingFraction 0.4 → 60% used.
        assert!((gemini_5h.used_percent - 60.0).abs() < 1e-6);
    }

    #[test]
    fn antigravity_non_gemini_groups_are_dropped() {
        let wire = AntigravityUsageWire {
            groups: vec![AntigravityGroup {
                display_name: Some("Claude and GPT models".to_string()),
                buckets: vec![AntigravityBucket {
                    window: Some("weekly".to_string()),
                    remaining_fraction: Some(1.0),
                    reset_time: None,
                }],
            }],
        };
        // No Gemini group → nothing to show.
        assert!(parse_antigravity(&wire).is_none());
    }

    #[test]
    fn parses_grok_credit_usage() {
        let wire: GrokBillingWire = serde_json::from_str(&load_fixture("grok_usage.json")).unwrap();
        let quota = parse_grok(&wire).expect("grok quota");
        assert_eq!(quota.plan.as_deref(), Some("SuperGrok Heavy"));
        assert_eq!(quota.windows.len(), 1);
        let w = &quota.windows[0];
        assert_eq!(w.label, "weekly");
        assert!((w.used_percent - 12.0).abs() < f64::EPSILON);
        assert!(w.resets_at.is_some());
    }

    #[test]
    fn grok_missing_usage_percent_is_zero_not_error() {
        let wire = GrokBillingWire {
            config: Some(GrokConfig {
                credit_usage_percent: None,
                current_period: None,
                subscription_tier: None,
            }),
        };
        let quota = parse_grok(&wire).expect("grok quota");
        assert!((quota.windows[0].used_percent - 0.0).abs() < f64::EPSILON);
        assert!(parse_grok(&GrokBillingWire { config: None }).is_none());
    }

    #[test]
    fn parses_opencode_go_windows() {
        let wire: OpenCodeGoUsageWire = serde_json::from_str(
            r#"{
                "usage": {
                    "rolling": { "status": "ok", "percent": 12.5, "resetsAt": "2026-08-14T15:10:54.202Z" },
                    "weekly": { "status": "ok", "percent": 34, "resetsAt": "2026-08-17T00:00:00.202Z" },
                    "monthly": { "status": "ok", "percent": 56, "resetsAt": "2026-08-24T12:32:08.202Z" }
                }
            }"#,
        )
        .unwrap();
        let quota = parse_opencode_go(&wire).expect("OpenCode Go quota");
        assert_eq!(quota.windows.len(), 3);
        assert_eq!(quota.windows[0].label, "5h");
        assert!((quota.windows[0].used_percent - 12.5).abs() < f64::EPSILON);
        assert_eq!(quota.windows[1].label, "weekly");
        assert_eq!(quota.windows[2].label, "monthly");
        assert!(
            quota
                .windows
                .iter()
                .all(|window| window.resets_at.is_some())
        );
    }

    #[test]
    fn parses_codex_business_spend_control_when_rate_limit_is_null() {
        let wire: CodexUsageWire =
            serde_json::from_str(&load_fixture("codex_usage_business.json")).unwrap();
        let quota = parse_codex(&wire).expect("codex business quota");
        assert_eq!(quota.plan.as_deref(), Some("business"));
        assert_eq!(quota.windows.len(), 1);
        let w = &quota.windows[0];
        assert_eq!(w.label, "spend");
        // Derived from used/limit, not the integer-rounded used_percent (0).
        assert!((w.used_percent - 0.035_721_998).abs() < 1e-6);
        assert_eq!(w.scope.as_deref(), Some("$0.89 of $2500.00"));
        assert_eq!(w.resets_at, Utc.timestamp_opt(1_790_812_801, 0).single());
    }

    #[test]
    fn codex_prefers_rate_limit_windows_over_spend_control() {
        // The consumer fixture carries `spend_control.individual_limit: null`,
        // so a plan with real windows never reaches the spend fallback.
        let wire: CodexUsageWire = serde_json::from_str(&load_fixture("codex_usage.json")).unwrap();
        let quota = parse_codex(&wire).expect("codex quota");
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "weekly");
    }

    #[test]
    fn codex_spend_window_falls_back_to_used_percent_without_dollar_amounts() {
        let spend = CodexSpendLimit {
            limit: None,
            used: None,
            used_percent: Some(42.0),
            reset_at: None,
        };
        let w = codex_spend_window(&spend).expect("spend window");
        assert_eq!(w.label, "spend");
        assert!((w.used_percent - 42.0).abs() < f64::EPSILON);
        assert!(w.scope.is_none());
        assert!(w.resets_at.is_none());

        // A zero limit is not divisible and has no percentage to fall back to.
        assert!(
            codex_spend_window(&CodexSpendLimit {
                limit: Some("0".to_string()),
                used: Some("0".to_string()),
                used_percent: None,
                reset_at: None,
            })
            .is_none()
        );
    }

    #[test]
    fn codex_window_labels_from_seconds() {
        assert_eq!(codex_window_label(Some(18_000)), "5h");
        assert_eq!(codex_window_label(Some(604_800)), "weekly");
        assert_eq!(codex_window_label(Some(7_200)), "window");
        assert_eq!(codex_window_label(None), "window");
    }

    #[test]
    fn empty_payloads_return_none() {
        let claude = ClaudeUsageWire {
            five_hour: None,
            seven_day: None,
            limits: Vec::new(),
        };
        assert!(parse_claude(&claude, None).is_none());
        let codex = CodexUsageWire {
            plan_type: None,
            rate_limit: None,
            spend_control: None,
        };
        assert!(parse_codex(&codex).is_none());
    }

    #[test]
    fn retry_after_parsing() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
        headers.insert(reqwest::header::RETRY_AFTER, "42".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(42));
        // HTTP-date form is not parsed to seconds → None (falls back to default backoff).
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn error_reasons_are_bounded_and_nonempty() {
        for e in [
            QuotaError::NotAuthenticated,
            QuotaError::Expired,
            QuotaError::Unauthorized,
            QuotaError::RateLimited {
                retry_after_secs: Some(30),
            },
            QuotaError::Timeout,
            QuotaError::Http(500),
            QuotaError::Incompatible,
            QuotaError::Transport,
        ] {
            assert!(!e.reason().is_empty());
        }
    }
    #[test]
    fn parses_muse_code_rolling_and_weekly_windows() {
        // Shape follows oh-my-pi's schema/fixtures: the 5-hour window reports
        // its duration in minutes and an epoch-seconds reset; the weekly
        // window reports an RFC 3339 reset.
        let wire: muse_code::MuseCodeKeyResponse = serde_json::from_str(
            r#"{
                "api_key": "LLM|secret",
                "is_subs_active": true,
                "subs_tier_name": "Power Usage",
                "subs_usage": {
                    "window": {
                        "used_percent": 42,
                        "resets_at": 1800000000,
                        "window_duration_mins": 300
                    },
                    "weekly": {
                        "used_percent": 75,
                        "resets_at": "2030-01-08T00:00:00.000Z"
                    }
                }
            }"#,
        )
        .expect("payload parses");

        let quota = parse_muse_code(&wire).expect("muse code quota");
        assert_eq!(quota.plan.as_deref(), Some("Power Usage"));
        assert_eq!(quota.windows.len(), 2);

        let rolling = &quota.windows[0];
        assert_eq!(rolling.label, "5h");
        assert!((rolling.used_percent - 42.0).abs() < f64::EPSILON);
        assert_eq!(
            rolling.resets_at,
            Utc.timestamp_opt(1_800_000_000, 0).single()
        );

        let weekly = &quota.windows[1];
        assert_eq!(weekly.label, "weekly");
        assert!((weekly.used_percent - 75.0).abs() < f64::EPSILON);
        assert!(weekly.resets_at.is_some());
    }

    #[test]
    fn muse_code_quota_without_usage_is_incompatible() {
        // An active subscription that reports no allowance block must not be
        // rendered as 0% used.
        let wire: muse_code::MuseCodeKeyResponse =
            serde_json::from_str(r#"{"api_key":"LLM|k","is_subs_active":true}"#)
                .expect("payload parses");
        assert!(parse_muse_code(&wire).is_none());
    }

    #[test]
    fn muse_code_window_label_falls_back_when_duration_is_absent() {
        assert_eq!(muse_code_window_label(Some(300.0)), "5h");
        assert_eq!(muse_code_window_label(Some(90.0)), "90m");
        assert_eq!(muse_code_window_label(None), "rolling");
    }
    #[test]
    fn claude_plan_prefers_the_precise_rate_limit_tier() {
        // Max 5x and Max 20x share organization_type, so the tier must win.
        let wire: ClaudeProfileWire = serde_json::from_str(
            r#"{"organization":{"organization_type":"claude_max",
                 "rate_limit_tier":"default_claude_max_20x"}}"#,
        )
        .unwrap();
        assert_eq!(wire.plan_label().as_deref(), Some("claude_max_20x"));
    }

    #[test]
    fn claude_plan_falls_back_to_organization_type() {
        let wire: ClaudeProfileWire =
            serde_json::from_str(r#"{"organization":{"organization_type":"claude_pro"}}"#).unwrap();
        assert_eq!(wire.plan_label().as_deref(), Some("claude_pro"));
    }

    #[test]
    fn claude_plan_is_absent_when_the_profile_says_nothing() {
        for body in [
            r#"{}"#,
            r#"{"organization":{}}"#,
            r#"{"organization":{"rate_limit_tier":""}}"#,
        ] {
            let wire: ClaudeProfileWire = serde_json::from_str(body).unwrap();
            assert_eq!(wire.plan_label(), None, "{body}");
        }
    }

    #[test]
    fn claude_quota_carries_the_plan_through() {
        let wire: ClaudeUsageWire =
            serde_json::from_str(&load_fixture("claude_usage.json")).unwrap();
        let quota = parse_claude(&wire, Some("claude_max_20x".to_string())).expect("quota");
        assert_eq!(quota.plan.as_deref(), Some("claude_max_20x"));
        // Plan metadata must not disturb the parsed windows.
        assert_eq!(quota.windows.len(), 3);
    }
}
