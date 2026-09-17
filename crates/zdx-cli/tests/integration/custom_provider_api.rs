//! `[providers.custom.<name>]` speaks Chat Completions by default and
//! Anthropic Messages when `api = "anthropic"`. Both share the same
//! `base_url` convention (`.../v1`), so the Anthropic route must land on
//! `/v1/messages`, not `/v1/v1/messages`. An explicit per-model `api` in
//! `model_overrides.toml` outranks the provider's `api`, and `@off` must send
//! a real disable on both routes because proxied backends think by default.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::{sse_response, text_sse};

const CHAT_COMPLETIONS_SSE: &str = concat!(
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

struct Captured {
    path: String,
    x_api_key: Option<String>,
    authorization: Option<String>,
    body: Value,
}

struct Turn<'a> {
    /// Extra lines appended to `[providers.custom.proxy]`.
    provider_lines: &'a str,
    /// Contents of `$ZDX_HOME/model_overrides.toml`, if any.
    overrides: Option<&'a str>,
    /// Model spec passed to `-m`.
    model: &'a str,
    /// SSE body the mock answers with.
    response: String,
}

/// Runs one `zdx exec` turn against a custom provider whose `base_url` is
/// `<mock>/v1` and returns the request the mock received.
async fn run_turn(turn: Turn<'_>) -> Captured {
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);

    std::fs::write(
        zdx_home.path().join("config.toml"),
        format!(
            "[providers.custom.proxy]\nbase_url = \"{}/v1\"\napi_key = \"test-api-key\"\nmodels = [\"some-model\"]\n{}\n",
            mock_server.uri(),
            turn.provider_lines
        ),
    )
    .unwrap();
    if let Some(overrides) = turn.overrides {
        std::fs::write(zdx_home.path().join("model_overrides.toml"), overrides).unwrap();
    }

    let response = turn.response;
    Mock::given(method("POST"))
        .respond_with(move |req: &Request| {
            let header = |name: &str| {
                req.headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(ToString::to_string)
            };
            seen_clone.lock().unwrap().push(Captured {
                path: req.url.path().to_string(),
                x_api_key: header("x-api-key"),
                authorization: header("authorization"),
                body: serde_json::from_slice(&req.body).unwrap_or(Value::Null),
            });
            sse_response(&response)
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args([
            "--root",
            work_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "-m",
            turn.model,
            "-p",
            "ping",
        ])
        .assert()
        .success();

    drop(mock_server);

    let mut seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "expected exactly one upstream request");
    seen.pop().unwrap()
}

fn chat(
    provider_lines: &'static str,
    overrides: Option<&'static str>,
    model: &'static str,
) -> Turn<'static> {
    Turn {
        provider_lines,
        overrides,
        model,
        response: CHAT_COMPLETIONS_SSE.to_string(),
    }
}

fn messages(
    provider_lines: &'static str,
    overrides: Option<&'static str>,
    model: &'static str,
) -> Turn<'static> {
    Turn {
        provider_lines,
        overrides,
        model,
        response: text_sse("pong"),
    }
}

const OVERRIDE_ANTHROPIC: &str =
    "[[override]]\nid = \"proxy:some-model\"\nreasoning = true\napi = \"anthropic\"\n";
const OVERRIDE_PRICING_ONLY: &str = "[[override]]\nid = \"proxy:some-model\"\ninput = 1.0\n";

#[tokio::test]
async fn custom_provider_defaults_to_chat_completions() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn(chat("", None, "proxy:some-model@high")).await;

    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer test-api-key")
    );
    assert_eq!(captured.body["reasoning_effort"], "high");
}

#[tokio::test]
async fn custom_provider_anthropic_api_hits_v1_messages_once() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn(messages(
        "api = \"anthropic\"",
        None,
        "proxy:some-model@low",
    ))
    .await;

    assert_eq!(captured.path, "/v1/messages");
    assert_eq!(captured.x_api_key.as_deref(), Some("test-api-key"));
    assert_eq!(captured.body["thinking"]["type"], "adaptive");
    assert_eq!(captured.body["output_config"]["effort"], "low");
}

/// An explicit per-model `api` override wins over the provider default.
#[tokio::test]
async fn custom_provider_model_override_api_outranks_provider_api() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn(messages("", Some(OVERRIDE_ANTHROPIC), "proxy:some-model")).await;

    assert_eq!(captured.path, "/v1/messages");
    assert_eq!(captured.x_api_key.as_deref(), Some("test-api-key"));
}

/// An override that does not set `api` leaves the provider's choice alone:
/// the synthesized model's implicit `openai-completions` must not leak in.
#[tokio::test]
async fn custom_provider_override_without_api_keeps_provider_api() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn(messages(
        "api = \"anthropic\"",
        Some(OVERRIDE_PRICING_ONLY),
        "proxy:some-model",
    ))
    .await;

    assert_eq!(captured.path, "/v1/messages");
}

/// `@off` must disable thinking explicitly on both routes.
#[tokio::test]
async fn custom_provider_off_sends_explicit_disable() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let chat_off = run_turn(chat("", None, "proxy:some-model@off")).await;
    assert_eq!(chat_off.path, "/v1/chat/completions");
    assert_eq!(chat_off.body["reasoning_effort"], "none");

    let messages_off = run_turn(messages(
        "api = \"anthropic\"",
        None,
        "proxy:some-model@off",
    ))
    .await;
    assert_eq!(messages_off.path, "/v1/messages");
    assert_eq!(
        messages_off.body["thinking"],
        serde_json::json!({"type": "disabled"})
    );
    assert!(messages_off.body.get("output_config").is_none());
}
