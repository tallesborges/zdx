//! Live subscription-quota readers for flat-rate OAuth providers.
//!
//! Reads the (mostly undocumented) usage/quota endpoints that Claude Code,
//! Codex CLI, Google Antigravity, and Grok Build expose, using zdx's own
//! stored OAuth tokens **read-only** (never refreshed or written from here —
//! see the subscription-quota-monitor plan).
//!
//! These endpoints are undocumented and may change; parsing is permissive and
//! failures degrade to a bounded [`QuotaError`] rather than propagating raw
//! provider response bodies (which are never logged or surfaced).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use crate::oauth::{OAuthCredentials, claude_cli, google_antigravity, grok_build, openai_codex};

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const ANTIGRAVITY_QUOTA_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
const GROK_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";

/// Provider id for the Claude (claude-cli) subscription.
pub const PROVIDER_CLAUDE: &str = claude_cli::PROVIDER_KEY;
/// Provider id for the Codex (openai-codex) subscription.
pub const PROVIDER_CODEX: &str = openai_codex::PROVIDER_KEY;
/// Provider id for the Google Antigravity subscription.
pub const PROVIDER_ANTIGRAVITY: &str = google_antigravity::PROVIDER_KEY;
/// Provider id for the Grok Build (xAI) subscription.
pub const PROVIDER_GROK: &str = grok_build::PROVIDER_KEY;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A boxed quota-fetch future.
pub type QuotaFuture = Pin<Box<dyn Future<Output = Result<SubscriptionQuota, QuotaError>> + Send>>;
/// A read-only quota fetcher for one provider.
pub type QuotaFetcher = fn() -> QuotaFuture;

/// Human-friendly display name for a subscription provider id.
#[must_use]
pub fn provider_display(provider: &str) -> &str {
    match provider {
        PROVIDER_CLAUDE => "Claude",
        PROVIDER_CODEX => "Codex",
        PROVIDER_ANTIGRAVITY => "Antigravity",
        PROVIDER_GROK => "Grok",
        other => other,
    }
}

/// Registry of supported subscription-quota fetchers, keyed by provider id.
/// The monitor iterates this (intersected with credential presence) — adding a
/// provider is one new `fetch_*` + one entry here, no new render code.
pub const FETCHERS: &[(&str, QuotaFetcher)] = &[
    (PROVIDER_CLAUDE, || Box::pin(fetch_claude_quota())),
    (PROVIDER_CODEX, || Box::pin(fetch_codex_quota())),
    (PROVIDER_ANTIGRAVITY, || Box::pin(fetch_antigravity_quota())),
    (PROVIDER_GROK, || Box::pin(fetch_grok_quota())),
];

/// A single rate-limit window (e.g. the ~5h session window or the weekly window).
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    /// Human label derived from provider data (e.g. `"5h"`, `"weekly"`).
    pub label: String,
    /// Percent of the window consumed, 0..=100.
    pub used_percent: f64,
    /// When the window resets, if the provider reported it.
    pub resets_at: Option<DateTime<Utc>>,
    /// Model this window is scoped to, when the limit is model-specific
    /// (e.g. Claude's per-model weekly limit like `"Fable"`).
    pub scope: Option<String>,
}

/// A provider's subscription quota snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionQuota {
    /// Plan label when the provider reports one (e.g. Codex `plan_type`).
    pub plan: Option<String>,
    /// Windows in display order.
    pub windows: Vec<QuotaWindow>,
}

/// Bounded failure categories. Raw response bodies are intentionally not carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaError {
    /// No stored OAuth credentials for the provider.
    NotAuthenticated,
    /// Stored access token is expired (a normal zdx run refreshes it).
    Expired,
    /// Endpoint rejected the token (401/403).
    Unauthorized,
    /// Endpoint rate-limited the request (429). Carries `Retry-After` seconds
    /// when the endpoint provided a numeric value.
    RateLimited { retry_after_secs: Option<u64> },
    /// Request timed out.
    Timeout,
    /// Other non-success HTTP status.
    Http(u16),
    /// Response did not match a known shape.
    Incompatible,
    /// Network/transport error.
    Transport,
}

impl QuotaError {
    /// A short, user-facing reason with no sensitive content.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NotAuthenticated => "not logged in".to_string(),
            Self::Expired => "expired · re-login in zdx".to_string(),
            Self::Unauthorized => "unauthorized".to_string(),
            Self::RateLimited { .. } => "rate limited".to_string(),
            Self::Timeout => "timed out".to_string(),
            Self::Http(code) => format!("HTTP {code}"),
            Self::Incompatible => "unexpected response".to_string(),
            Self::Transport => "network error".to_string(),
        }
    }
}

/// Loads stored OAuth credentials **read-only** (no refresh, no write); maps a
/// missing store or missing provider entry to [`QuotaError::NotAuthenticated`].
fn require_creds(
    loaded: Result<Option<OAuthCredentials>, anyhow::Error>,
) -> Result<OAuthCredentials, QuotaError> {
    loaded
        .map_err(|err| {
            tracing::debug!(%err, "quota: failed to load OAuth cache");
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
            tracing::debug!(%err, "quota: failed to build client");
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
pub async fn fetch_claude_quota() -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(claude_cli::load_credentials())?;
    if creds.is_expired() {
        return Err(QuotaError::Expired);
    }

    let resp = quota_client()?
        .get(CLAUDE_USAGE_URL)
        .header("Authorization", format!("Bearer {}", creds.access))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .header("user-agent", "claude-cli/2.1.2 (external, cli)")
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("x-app", "cli")
        .send()
        .await
        .map_err(|e| classify_send(&e))?;

    if !resp.status().is_success() {
        return Err(error_for_status(resp.status(), resp.headers()));
    }

    let wire: ClaudeUsageWire = resp.json().await.map_err(|err| {
        tracing::debug!(%err, "quota: claude response decode failed");
        QuotaError::Incompatible
    })?;
    parse_claude(&wire).ok_or(QuotaError::Incompatible)
}

/// Fetches the Codex (openai-codex) subscription quota.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing/expired creds or endpoint failure.
pub async fn fetch_codex_quota() -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(openai_codex::load_credentials())?;
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
        tracing::debug!(%err, "quota: codex response decode failed");
        QuotaError::Incompatible
    })?;
    parse_codex(&wire).ok_or(QuotaError::Incompatible)
}

/// Fetches the Google Antigravity subscription quota.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing/expired creds or endpoint failure.
pub async fn fetch_antigravity_quota() -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(google_antigravity::load_credentials())?;
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
        tracing::debug!(%err, "quota: antigravity response decode failed");
        QuotaError::Incompatible
    })?;
    parse_antigravity(&wire).ok_or(QuotaError::Incompatible)
}

/// Fetches the Grok Build (xAI) subscription quota.
///
/// # Errors
/// Returns a bounded [`QuotaError`] on missing/expired creds or endpoint failure.
pub async fn fetch_grok_quota() -> Result<SubscriptionQuota, QuotaError> {
    let creds = require_creds(grok_build::load_credentials())?;
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
        tracing::debug!(%err, "quota: grok response decode failed");
        QuotaError::Incompatible
    })?;
    parse_grok(&wire).ok_or(QuotaError::Incompatible)
}

// --- Claude wire shape ---

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
fn parse_claude(wire: &ClaudeUsageWire) -> Option<SubscriptionQuota> {
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
    Some(SubscriptionQuota {
        plan: None,
        windows,
    })
}

// --- Codex wire shape ---

#[derive(Debug, Deserialize)]
struct CodexUsageWire {
    plan_type: Option<String>,
    rate_limit: Option<CodexRateLimit>,
}

#[derive(Debug, Deserialize)]
struct CodexRateLimit {
    primary_window: Option<CodexWindow>,
    secondary_window: Option<CodexWindow>,
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

/// Parses the Codex usage payload into a neutral snapshot.
fn parse_codex(wire: &CodexUsageWire) -> Option<SubscriptionQuota> {
    let rate_limit = wire.rate_limit.as_ref()?;
    let mut windows = Vec::new();
    if let Some(w) = &rate_limit.primary_window {
        windows.push(codex_window(w));
    }
    if let Some(w) = &rate_limit.secondary_window {
        windows.push(codex_window(w));
    }
    if windows.is_empty() {
        return None;
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
        let quota = parse_claude(&wire).expect("claude quota");
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
        let quota = parse_claude(&wire).expect("fallback quota");
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
        assert!(parse_claude(&claude).is_none());
        let codex = CodexUsageWire {
            plan_type: None,
            rate_limit: None,
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
}
