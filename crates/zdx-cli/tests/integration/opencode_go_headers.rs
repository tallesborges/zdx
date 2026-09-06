//! `OpenCode` Go requires `x-opencode-session` on every request. Both routes
//! the meta-provider can pick for a shipped model (`openai-completions` and
//! `anthropic-messages`) must carry it, or the proxy rejects the request.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request};
use zdx_engine::providers::ChatMessage;
use zdx_engine::providers::opencode_go::{OpencodeGoClient, OpencodeGoConfig};

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
    session: Option<String>,
    thread_ids: Vec<String>,
}

/// Runs one `zdx exec` turn against a mock `OpenCode` Go endpoint and returns
/// the `x-opencode-session` header it received, plus any thread ids persisted
/// by that run.
async fn run_turn(model: &str, api_path: &str, body: String, persist_thread: bool) -> Captured {
    let zdx_home = TempDir::new().expect("create temp zdx home");
    let work_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);

    Mock::given(method("POST"))
        .and(path(api_path.to_string()))
        .respond_with(move |req: &Request| {
            let value = req
                .headers
                .get("x-opencode-session")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);
            seen_clone.lock().unwrap().push(value);
            sse_response(&body)
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut cmd = crate::fixtures::zdx_cmd();
    cmd.env("ZDX_HOME", zdx_home.path())
        .env("OPENCODE_API_KEY", "test-api-key")
        .env("OPENCODE_GO_BASE_URL", mock_server.uri())
        .args(["--root", work_dir.path().to_str().unwrap()]);
    if persist_thread {
        cmd.args(["--thread", "test-thread"]);
    } else {
        cmd.arg("--no-thread");
    }
    cmd.args(["exec", "-m", model, "-p", "ping"])
        .assert()
        .success();

    drop(mock_server);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "expected exactly one upstream request");

    let thread_ids = std::fs::read_dir(zdx_home.path().join("threads"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "jsonl")
                .then(|| path.file_stem()?.to_str().map(ToString::to_string))
                .flatten()
        })
        .collect();

    Captured {
        session: seen[0].clone(),
        thread_ids,
    }
}

#[tokio::test]
async fn opencode_go_completions_route_sends_encoded_thread_id_as_session() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn(
        "opencode-go:kimi-k2.6",
        "/v1/chat/completions",
        CHAT_COMPLETIONS_SSE.to_string(),
        true,
    )
    .await;

    assert_eq!(
        captured.thread_ids.len(),
        1,
        "expected the run to persist one thread"
    );
    assert_eq!(
        captured.session.as_deref(),
        Some("thread:dGVzdC10aHJlYWQ"),
        "x-opencode-session must encode test-thread on the openai-completions route"
    );
}

fn client(server: &MockServer, api_hint: &str, cache_key: Option<&str>) -> OpencodeGoClient {
    OpencodeGoClient::new(OpencodeGoConfig {
        api_key: "test-api-key".to_string(),
        base_url: server.uri(),
        model: "test-model".to_string(),
        max_tokens: Some(1024),
        fallback_max_tokens: 1024,
        thinking_enabled: false,
        thinking_budget_tokens: 0,
        thinking_effort: None,
        gemini_thinking: None,
        reasoning_effort: None,
        cache_key: cache_key.map(str::to_owned),
        api_hint: Some(api_hint.to_string()),
    })
}

#[tokio::test]
async fn opencode_go_sessions_are_stable_and_isolated_on_every_route() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response(""))
        .mount(&server)
        .await;
    let messages = [ChatMessage::user("ping")];
    let mut threadless_sessions = std::collections::HashSet::new();

    for (route, endpoint) in [
        ("openai-completions", "/v1/chat/completions"),
        ("anthropic-messages", "/v1/messages"),
        ("openai-responses", "/responses"),
        (
            "google-generative-ai",
            "/v1/models/test-model:streamGenerateContent",
        ),
    ] {
        let start = server.received_requests().await.unwrap().len();
        for key in [None, None, Some("foo!"), Some("foo?"), Some("foo!")] {
            let client = client(&server, route, key);
            for _ in 0..2 {
                drop(
                    client
                        .send_messages_stream(&messages, &[], None)
                        .await
                        .unwrap(),
                );
            }
        }
        let requests = server.received_requests().await.unwrap();
        let requests = &requests[start..];
        assert_eq!(requests.len(), 10);
        for pair in requests.chunks_exact(2) {
            assert_eq!(pair[0].url.path(), endpoint);
            let session = &pair[0].headers["x-opencode-session"];
            assert_eq!(session, &pair[1].headers["x-opencode-session"]);
            assert!(!session.is_empty());
        }
        for index in [0, 2] {
            assert!(
                threadless_sessions.insert(requests[index].headers["x-opencode-session"].clone())
            );
        }
        assert_ne!(
            requests[4].headers["x-opencode-session"],
            requests[6].headers["x-opencode-session"]
        );
        assert_eq!(
            requests[4].headers["x-opencode-session"],
            requests[8].headers["x-opencode-session"]
        );
    }
}

#[tokio::test]
async fn opencode_go_anthropic_route_sends_session_header_without_thread() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let captured = run_turn(
        "opencode-go:minimax-m3",
        "/v1/messages",
        text_sse("pong"),
        false,
    )
    .await;

    assert!(
        captured.session.is_some_and(|s| !s.is_empty()),
        "x-opencode-session must be present on the anthropic-messages route even with --no-thread"
    );
}
