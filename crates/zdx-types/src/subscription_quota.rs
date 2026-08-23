//! Pure value types for live subscription-quota snapshots.

use chrono::{DateTime, Utc};

/// A single rate-limit window, such as a session or weekly window.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
    pub scope: Option<String>,
}

/// A provider's subscription quota snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionQuota {
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
}

/// Bounded failure categories that never carry raw provider response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaError {
    NotAuthenticated,
    Expired,
    Unauthorized,
    RateLimited { retry_after_secs: Option<u64> },
    Timeout,
    Http(u16),
    Incompatible,
    Transport,
}

impl QuotaError {
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

/// One provider account's result in a live subscription-quota snapshot.
#[derive(Debug, Clone)]
pub struct SubscriptionQuotaResult {
    pub provider: &'static str,
    pub account: Option<String>,
    pub quota: Result<SubscriptionQuota, QuotaError>,
}

/// A live read-only quota snapshot across every supported stored account.
#[derive(Debug, Clone)]
pub struct SubscriptionQuotaSnapshot {
    pub providers: Vec<SubscriptionQuotaResult>,
}
