use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Categories of provider errors for consistent error handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// HTTP status error (4xx, 5xx)
    HttpStatus,
    /// Connection timeout or request timeout
    Timeout,
    /// Failed to parse response (JSON parse error, invalid SSE, etc.)
    Parse,
    /// API-level error returned by the provider (e.g., overloaded, rate_limit)
    ApiError,
}

impl fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderErrorKind::HttpStatus => write!(f, "http_status"),
            ProviderErrorKind::Timeout => write!(f, "timeout"),
            ProviderErrorKind::Parse => write!(f, "parse"),
            ProviderErrorKind::ApiError => write!(f, "api_error"),
        }
    }
}

/// Structured error from the provider with kind and details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderError {
    /// Error category
    pub kind: ProviderErrorKind,
    /// One-line summary suitable for display
    pub message: String,
    /// Optional additional details (e.g., raw error body)
    pub details: Option<String>,
}

impl ProviderError {
    /// Creates a new provider error.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    /// Creates a provider error with details.
    /// Creates an HTTP status error.
    pub fn http_status(status: u16, body: &str) -> Self {
        let message = format!("HTTP {}", status);
        let details = if body.is_empty() {
            None
        } else {
            // Try to extract a cleaner error message from JSON
            if let Ok(json) = serde_json::from_str::<Value>(body)
                && let Some(error_obj) = json.get("error")
                && let Some(msg) = error_obj.get("message").and_then(|v| v.as_str())
            {
                return Self {
                    kind: ProviderErrorKind::HttpStatus,
                    message: format!("HTTP {}: {}", status, msg),
                    details: Some(body.to_string()),
                };
            }
            Some(body.to_string())
        };
        Self {
            kind: ProviderErrorKind::HttpStatus,
            message,
            details,
        }
    }

    /// Creates a timeout error.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Timeout, message)
    }

    /// Creates an API error (from mid-stream error event).
    pub fn api_error(error_type: &str, message: &str) -> Self {
        Self {
            kind: ProviderErrorKind::ApiError,
            message: format!("{}: {}", error_type, message),
            details: None,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}
