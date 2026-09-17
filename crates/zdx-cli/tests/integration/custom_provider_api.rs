//! `[providers.custom.<name>]` speaks Chat Completions by default and
//! Anthropic Messages when `api = "anthropic"`. Both share the same
//! `base_url` convention (`.../v1`), so the Anthropic route must land on
//! `/v1/messages`, not `/v1/v1/messages`.

use std::sync::{Arc, Mutex};

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
}

/// Runs one `zdx exec` turn against a custom provider whose `base_url` is
/// `<mock>/v1` and returns the path + auth headers the mock received.
async fn run_turn(api_line: &str, body: String) -> Captured {
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);

    std::fs::write(
        zdx_home.path().join("config.toml"),
        format!(
            "[providers.custom.proxy]\nbase_url = \"{}/v1\"\napi_key = \"test-api-key\"\nmodels = [\"some-model\"]\n{api_line}\n",
            mock_server.uri()
        ),
    )
    .unwrap();

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
            });
            sse_response(&body)
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
            "proxy:some-model",
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

#[tokio::test]
async fn custom_provider_defaults_to_chat_completions() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn("", CHAT_COMPLETIONS_SSE.to_string()).await;

    assert_eq!(captured.path, "/v1/chat/completions");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer test-api-key")
    );
}

#[tokio::test]
async fn custom_provider_anthropic_api_hits_v1_messages_once() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn("api = \"anthropic\"", text_sse("pong")).await;

    assert_eq!(captured.path, "/v1/messages");
    assert_eq!(captured.x_api_key.as_deref(), Some("test-api-key"));
}
