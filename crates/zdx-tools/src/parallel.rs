//! Shared HTTP client, retry policy, and error reporting for the Parallel.ai
//! web tools (`web_search` and `fetch_webpage`).

use std::error::Error as _;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde::Serialize;

pub(crate) const PARALLEL_BETA_HEADER: &str = "search-extract-2025-10-10";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_ATTEMPTS: u32 = 3;
const BASE_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Shared client: reusing it keeps the connection pool warm so parallel tool
/// calls don't each pay for a fresh DNS lookup and TLS handshake.
fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

/// Renders a `reqwest::Error` with its classification and full source chain.
///
/// `Display` alone collapses to `error sending request for url (...)`, which
/// hides whether the failure was DNS, connect, TLS, or a timeout.
pub(crate) fn describe_request_error(err: &reqwest::Error) -> String {
    let mut kinds = Vec::new();
    if err.is_timeout() {
        kinds.push("timeout");
    }
    if err.is_connect() {
        kinds.push("connect");
    }
    if err.is_request() {
        kinds.push("request");
    }
    if err.is_body() {
        kinds.push("body");
    }
    if err.is_decode() {
        kinds.push("decode");
    }
    if err.is_redirect() {
        kinds.push("redirect");
    }
    if err.is_builder() {
        kinds.push("builder");
    }
    if kinds.is_empty() {
        kinds.push("unknown");
    }

    let mut out = format!("kind=[{}] error={err}", kinds.join(","));
    if let Some(url) = err.url() {
        let _ = write!(out, " url={url}");
    }
    if let Some(status) = err.status() {
        let _ = write!(out, " status={status}");
    }

    let mut source = err.source();
    let mut depth = 0;
    while let Some(cause) = source {
        depth += 1;
        let _ = write!(out, " | cause[{depth}]: {cause}");
        source = cause.source();
    }

    out
}

fn is_retryable_transport_error(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout() || err.is_request()
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn backoff_for(attempt: u32) -> Duration {
    BASE_BACKOFF
        .saturating_mul(1 << attempt.min(4))
        .min(MAX_BACKOFF)
}

fn retry_after(response: &Response) -> Option<Duration> {
    let raw = response.headers().get(reqwest::header::RETRY_AFTER)?;
    let secs = raw.to_str().ok()?.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(secs).min(MAX_BACKOFF))
}

/// POSTs a JSON body to a Parallel endpoint, retrying transient failures with
/// bounded exponential backoff.
///
/// Retries connect/timeout errors and 429/502/503/504 responses, honoring
/// `Retry-After` when present. The final attempt's outcome is returned as-is,
/// so non-2xx responses still reach the caller for status-specific handling.
pub(crate) async fn post_json<T: Serialize>(
    url: &str,
    api_key: &str,
    body: &T,
) -> Result<Response, reqwest::Error> {
    let mut attempt = 0;
    loop {
        let result = client()
            .post(url)
            .header("Content-Type", "application/json")
            .header("x-api-key", api_key)
            .header("parallel-beta", PARALLEL_BETA_HEADER)
            .json(body)
            .send()
            .await;

        let last_attempt = attempt + 1 >= MAX_ATTEMPTS;
        let delay = match &result {
            Err(err) if !last_attempt && is_retryable_transport_error(err) => {
                tracing::warn!(
                    url,
                    attempt = attempt + 1,
                    error = %describe_request_error(err),
                    "parallel request failed, retrying"
                );
                Some(backoff_for(attempt))
            }
            Ok(response) if !last_attempt && is_retryable_status(response.status()) => {
                let status = response.status();
                let delay = retry_after(response).unwrap_or_else(|| backoff_for(attempt));
                tracing::warn!(
                    url,
                    attempt = attempt + 1,
                    %status,
                    "parallel request returned retryable status, retrying"
                );
                Some(delay)
            }
            _ => None,
        };

        let Some(delay) = delay else {
            return result;
        };

        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff_for(0), Duration::from_millis(500));
        assert_eq!(backoff_for(1), Duration::from_secs(1));
        assert_eq!(backoff_for(2), Duration::from_secs(2));
        assert!(backoff_for(10) <= MAX_BACKOFF);
    }

    #[test]
    fn retryable_statuses_are_transient_only() {
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[tokio::test]
    async fn describe_request_error_includes_kind_and_url() {
        let err = client()
            .get("http://127.0.0.1:1/never")
            .send()
            .await
            .expect_err("connection to a closed port should fail");
        let described = describe_request_error(&err);
        assert!(described.starts_with("kind=["), "{described}");
        assert!(
            described.contains("url=http://127.0.0.1:1/never"),
            "{described}"
        );
        assert!(described.contains("cause[1]"), "{described}");
    }
}
