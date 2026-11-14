//! Flood control and delivery-outcome classification for
//! [`super::TelegramOutbound`].
//!
//! What Telegram documents (Bots FAQ, Bot API `ResponseParameters`): avoid
//! more than about one message per second in a chat, at most 20 messages per
//! minute in a group, and on HTTP 429 wait `retry_after` seconds before the
//! request is repeated.
//!
//! What it does not document, and this module therefore does not assume:
//! whether a 429 is scoped to one chat, one method, or the whole bot; whether
//! forum topics have their own budget; and how edits, typing, deletes, or
//! albums count. This module does no pacing: per-chat pacing is the Go
//! gateway's (`apps/gateway/internal/telegram/pacer.go`). A refusal can
//! arrive at any time, so every caller must handle 429.

use std::fmt;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

/// Wait used when a 429 carries no usable `retry_after`.
const FALLBACK_RETRY_AFTER: Duration = Duration::from_secs(5);

/// Non-secret identity of a bot token: the numeric bot id before `:`. The
/// secret half is never logged.
#[must_use]
pub fn bot_identity(token: &str) -> &str {
    token
        .split_once(':')
        .map(|(id, _)| id)
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or("unknown")
}

/// Telegram answered with `ok: false`. The request reached Telegram and was
/// refused, so nothing was delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramApiError {
    pub method: String,
    pub code: Option<u16>,
    pub description: String,
    /// `parameters.retry_after` (seconds) on a flood-control refusal.
    pub retry_after: Option<u64>,
}

impl TelegramApiError {
    /// Flood-control wait when this is a 429, else `None`.
    #[must_use]
    pub fn flood_wait(&self) -> Option<Duration> {
        if self.code != Some(429) {
            return None;
        }
        let secs = self
            .retry_after
            .or_else(|| retry_after_from_description(&self.description));
        Some(secs.map_or(FALLBACK_RETRY_AFTER, |secs| {
            Duration::from_secs(secs.max(1))
        }))
    }
}

impl fmt::Display for TelegramApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self
            .code
            .map_or_else(|| "unknown".to_string(), |code| code.to_string());
        write!(
            f,
            "Telegram API error for {} ({code}): {}",
            self.method, self.description
        )
    }
}

impl std::error::Error for TelegramApiError {}

fn retry_after_from_description(description: &str) -> Option<u64> {
    let lower = description.to_ascii_lowercase();
    let rest = &lower[lower.find("retry after ")? + "retry after ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether a transport failure could have reached Telegram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportStage {
    /// The request was never sent (connection or request build failed).
    NotSent,
    /// The request may have been processed: it was written but no readable
    /// answer came back (timeout, dropped connection, unparseable body).
    Ambiguous,
}

/// The HTTP exchange with Telegram failed. Carries no URL, because Bot API
/// URLs embed the token.
#[derive(Debug)]
pub struct TelegramTransportError {
    pub method: String,
    pub stage: TransportStage,
    detail: String,
}

impl TelegramTransportError {
    /// Classifies a `reqwest` failure. The URL is stripped before the error
    /// text is kept.
    #[must_use]
    pub fn from_reqwest(method: &str, err: reqwest::Error) -> Self {
        let stage = if err.is_connect() || err.is_builder() {
            TransportStage::NotSent
        } else {
            TransportStage::Ambiguous
        };
        let err = err.without_url();
        let mut detail = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            detail.push_str(": ");
            detail.push_str(&cause.to_string());
            source = cause.source();
        }
        Self {
            method: method.to_string(),
            stage,
            detail,
        }
    }

    /// The body is not kept: it may echo request data, and logs and delivery
    /// records must not carry message bodies or the token.
    fn unreadable_body(method: &str, body: &[u8]) -> Self {
        Self {
            method: method.to_string(),
            stage: TransportStage::Ambiguous,
            detail: format!("unreadable response ({} bytes)", body.len()),
        }
    }
}

impl fmt::Display for TelegramTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.stage {
            TransportStage::NotSent => write!(
                f,
                "Telegram request for {} was not sent: {}",
                self.method, self.detail
            ),
            TransportStage::Ambiguous => write!(
                f,
                "Telegram request for {} got no readable answer and may have been delivered: {}",
                self.method, self.detail
            ),
        }
    }
}

impl std::error::Error for TelegramTransportError {}

/// Parses a Bot API response body into its `result`.
///
/// # Errors
/// [`TelegramApiError`] for `ok: false`; an ambiguous
/// [`TelegramTransportError`] when the body is not a Bot API envelope.
pub fn parse_response(method: &str, bytes: &[u8]) -> Result<Value> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Err(TelegramTransportError::unreadable_body(method, bytes).into());
    };
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(value.get("result").cloned().unwrap_or(Value::Bool(true))),
        Some(false) => Err(TelegramApiError {
            method: method.to_string(),
            code: value
                .get("error_code")
                .and_then(Value::as_u64)
                .and_then(|code| u16::try_from(code).ok()),
            description: value
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Telegram API error")
                .to_string(),
            retry_after: value
                .get("parameters")
                .and_then(|params| params.get("retry_after"))
                .and_then(Value::as_u64),
        }
        .into()),
        None => Err(TelegramTransportError::unreadable_body(method, bytes).into()),
    }
}

/// What a failed call means for delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Flood control refused it. Not delivered; may be repeated after the
    /// wait.
    FloodWait(Duration),
    /// Telegram refused it for another reason. Not delivered.
    Rejected,
    /// Never sent (local error, connection failure). Not delivered.
    NotSent,
    /// May or may not have been delivered. Never resent automatically: a
    /// resend could duplicate the message.
    Unknown,
}

impl FailureClass {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::FloodWait(_) => "flood_wait",
            Self::Rejected => "rejected",
            Self::NotSent => "not_sent",
            Self::Unknown => "unknown",
        }
    }
}

/// Classifies a client error.
///
/// Errors carrying none of this module's types come from local work before
/// any request (reading a file, size checks), so they count as not sent. Every
/// network path in a ZDX Telegram client must therefore surface its failure
/// as one of these types.
#[must_use]
pub fn classify(err: &anyhow::Error) -> FailureClass {
    for cause in err.chain() {
        if let Some(api) = cause.downcast_ref::<TelegramApiError>() {
            return api
                .flood_wait()
                .map_or(FailureClass::Rejected, FailureClass::FloodWait);
        }
        if let Some(transport) = cause.downcast_ref::<TelegramTransportError>() {
            return match transport.stage {
                TransportStage::NotSent => FailureClass::NotSent,
                TransportStage::Ambiguous => FailureClass::Unknown,
            };
        }
    }
    FailureClass::NotSent
}

/// Bounded retry for important sends after an explicit flood refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, including the first.
    pub max_attempts: u32,
    /// Longest time from the first attempt to the start of the last one.
    pub deadline: Duration,
}

impl RetryPolicy {
    /// Policy for reply text and files: three attempts within two minutes.
    pub const IMPORTANT: Self = Self {
        max_attempts: 3,
        deadline: Duration::from_mins(2),
    };

    /// Delay before the next attempt, or `None` to give up. The wait is never
    /// shortened to fit the deadline: if Telegram's wait does not fit, the
    /// send fails instead.
    #[must_use]
    pub fn delay_before_retry(
        self,
        attempts_made: u32,
        elapsed: Duration,
        retry_after: Duration,
    ) -> Option<Duration> {
        if attempts_made >= self.max_attempts {
            return None;
        }
        (elapsed + retry_after <= self.deadline).then_some(retry_after)
    }
}

/// One Telegram call attempt, for the per-call log line.
#[derive(Debug, Clone, Copy)]
pub struct CallRecord<'a> {
    /// `"outbound"` (`zdx telegram`, the tool). The Go gateway logs its own
    /// calls as `"gateway"`.
    pub client: &'static str,
    pub bot: &'a str,
    pub method: &'a str,
    pub chat: Option<i64>,
    pub topic: Option<i64>,
    /// Whether the call creates a new chat message.
    pub new_message: bool,
    pub attempt: u32,
    pub waited: Duration,
    pub duration: Duration,
}

/// Logs one call attempt. New messages log at `info` so message rates can be
/// measured from the default log; other calls log at `debug`. Message bodies
/// and tokens are never logged. `client` plus `pid` identify the sending
/// tool or CLI process; a reply's delivery record logs
/// the message ids it produced, which ties these lines to that record.
pub fn log_call(record: &CallRecord<'_>, outcome: Result<Option<i64>, &anyhow::Error>) {
    let waited_ms = u64::try_from(record.waited.as_millis()).unwrap_or(u64::MAX);
    let duration_ms = u64::try_from(record.duration.as_millis()).unwrap_or(u64::MAX);
    let pid = std::process::id();
    match outcome {
        Ok(message_id) => {
            if record.new_message {
                tracing::info!(
                    client = record.client,
                    pid,
                    bot = record.bot,
                    method = record.method,
                    chat = record.chat,
                    topic = record.topic,
                    attempt = record.attempt,
                    message_id,
                    waited_ms,
                    duration_ms,
                    outcome = "ok",
                    "Telegram call"
                );
            } else {
                tracing::debug!(
                    client = record.client,
                    pid,
                    bot = record.bot,
                    method = record.method,
                    chat = record.chat,
                    topic = record.topic,
                    attempt = record.attempt,
                    waited_ms,
                    duration_ms,
                    outcome = "ok",
                    "Telegram call"
                );
            }
        }
        Err(err) => {
            let class = classify(err);
            let retry_after_s = match class {
                FailureClass::FloodWait(wait) => Some(wait.as_secs()),
                _ => None,
            };
            let error = format!("{err:#}");
            if record.new_message {
                tracing::warn!(
                    client = record.client,
                    pid,
                    bot = record.bot,
                    method = record.method,
                    chat = record.chat,
                    topic = record.topic,
                    attempt = record.attempt,
                    retry_after_s,
                    waited_ms,
                    duration_ms,
                    outcome = class.label(),
                    error,
                    "Telegram call failed"
                );
            } else {
                tracing::debug!(
                    client = record.client,
                    pid,
                    bot = record.bot,
                    method = record.method,
                    chat = record.chat,
                    topic = record.topic,
                    attempt = record.attempt,
                    retry_after_s,
                    waited_ms,
                    duration_ms,
                    outcome = class.label(),
                    error,
                    "Telegram call failed"
                );
            }
        }
    }
}

/// Extracts `message_id` from a send result, for logs and delivery records.
#[must_use]
pub fn result_message_id(result: &Value) -> Option<i64> {
    result.get("message_id").and_then(Value::as_i64)
}

/// Decodes an `ok: true` result and returns it with its `message_id`. A
/// decode failure means Telegram accepted the call but the answer was not
/// understood, so it classifies as ambiguous rather than as "not sent". The
/// serde error is not kept: it can quote values from the response.
///
/// # Errors
/// An ambiguous [`TelegramTransportError`] when `value` does not decode.
pub fn decode_result<T: serde::de::DeserializeOwned>(
    method: &str,
    value: Value,
) -> Result<(Option<i64>, T)> {
    let message_id = result_message_id(&value);
    serde_json::from_value(value)
        .map(|decoded| (message_id, decoded))
        .map_err(|_quotes_response_values| {
            TelegramTransportError {
                method: method.to_string(),
                stage: TransportStage::Ambiguous,
                detail: "accepted, but the result did not decode".to_string(),
            }
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_error(code: u16, description: &str, retry_after: Option<u64>) -> anyhow::Error {
        TelegramApiError {
            method: "sendDocument".to_string(),
            code: Some(code),
            description: description.to_string(),
            retry_after,
        }
        .into()
    }

    #[test]
    fn bot_identity_keeps_only_the_numeric_id() {
        assert_eq!(bot_identity("123456:secret-part"), "123456");
        assert_eq!(bot_identity("no-colon"), "unknown");
        assert_eq!(bot_identity("abc:secret"), "unknown");
    }

    #[test]
    fn parses_flood_refusal_with_retry_after() {
        let body = br#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 11","parameters":{"retry_after":11}}"#;
        let err = parse_response("sendDocument", body).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Telegram API error for sendDocument (429): Too Many Requests: retry after 11"
        );
        assert_eq!(
            classify(&err),
            FailureClass::FloodWait(Duration::from_secs(11))
        );
    }

    #[test]
    fn flood_wait_falls_back_to_the_description() {
        let err = api_error(429, "Too Many Requests: retry after 7", None);
        assert_eq!(
            classify(&err),
            FailureClass::FloodWait(Duration::from_secs(7))
        );
        let err = api_error(429, "Too Many Requests", None);
        assert_eq!(
            classify(&err),
            FailureClass::FloodWait(FALLBACK_RETRY_AFTER)
        );
    }

    #[test]
    fn other_refusals_are_rejected_and_context_is_kept() {
        let err = api_error(400, "Bad Request: chat not found", None).context("send reply");
        assert_eq!(classify(&err), FailureClass::Rejected);
    }

    #[test]
    fn unreadable_bodies_are_ambiguous_and_not_echoed() {
        let body = b"<html>502 echo of bot123:secret-marker</html>";
        let err = parse_response("sendMessage", body).unwrap_err();
        assert_eq!(classify(&err), FailureClass::Unknown);
        let text = format!("{err:#}");
        assert!(text.contains("may have been delivered"));
        assert!(!text.contains("secret-marker"));
    }

    #[test]
    fn undecodable_results_are_ambiguous_and_not_echoed() {
        #[derive(Debug, serde::Deserialize)]
        struct Sent {
            #[allow(dead_code)]
            message_id: i64,
        }
        let value = parse_response(
            "sendMessage",
            br#"{"ok":true,"result":{"message_id":"secret-marker"}}"#,
        )
        .expect("ok envelope");
        let err = decode_result::<Sent>("sendMessage", value).unwrap_err();
        assert_eq!(classify(&err), FailureClass::Unknown);
        assert!(!format!("{err:#}").contains("secret-marker"));
    }

    #[test]
    fn local_errors_count_as_not_sent() {
        let err = anyhow::anyhow!("read document file /tmp/missing");
        assert_eq!(classify(&err), FailureClass::NotSent);
    }

    #[test]
    fn ok_results_are_returned() {
        let value =
            parse_response("sendMessage", br#"{"ok":true,"result":{"message_id":5}}"#).expect("ok");
        assert_eq!(result_message_id(&value), Some(5));
    }

    #[test]
    fn retry_policy_honours_attempts_and_deadline() {
        let policy = RetryPolicy::IMPORTANT;
        let wait = Duration::from_secs(11);
        assert_eq!(
            policy.delay_before_retry(1, Duration::ZERO, wait),
            Some(wait)
        );
        assert_eq!(policy.delay_before_retry(3, Duration::ZERO, wait), None);
        // Never shortened to fit: a wait that overruns the deadline gives up.
        assert_eq!(
            policy.delay_before_retry(1, Duration::from_secs(115), wait),
            None
        );
    }
}
