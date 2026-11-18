//! GitHub Copilot serves each model on one API. A `zdx exec` turn must reach
//! the route for the model with the stored Copilot token as a bearer and the
//! VS Code identity headers Copilot expects.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use wiremock::{Mock, MockServer, Request};

use crate::fixtures::{sse_response, text_sse};

const CHAT_COMPLETIONS_SSE: &str = concat!(
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

const RESPONSES_SSE: &str = concat!(
    "event: response.output_text.delta\n",
    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",",
    "\"output_index\":0,\"content_index\":0,\"delta\":\"pong\"}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",",
    "\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,",
    "\"total_tokens\":2}}}\n\n",
);

const COPILOT_TOKEN: &str = "tid=test;exp=9999999999;proxy-ep=proxy.example.invalid";

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

struct Seen {
    path: String,
    headers: Vec<(String, String)>,
}

impl Seen {
    fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn header(&self, name: &str) -> &str {
        self.all(name).first().copied().unwrap_or_default()
    }
}

async fn run_turn(model: &str, body: String) -> Seen {
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    std::fs::write(
        zdx_home.path().join("oauth.json"),
        serde_json::json!({
            "github-copilot": {
                "type": "oauth",
                "refresh": "gho_test",
                "access": COPILOT_TOKEN,
                "expires": 9_999_999_999_999_u64,
            }
        })
        .to_string(),
    )
    .unwrap();

    let mock_server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |req: &Request| {
            seen_clone.lock().unwrap().push(Seen {
                path: req.url.path().to_string(),
                headers: req
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_string(),
                            value.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect(),
            });
            sse_response(&body)
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env("GITHUB_COPILOT_BASE_URL", mock_server.uri())
        .args([
            "--root",
            work_dir.path().to_str().unwrap(),
            "--no-thread",
            "exec",
            "--no-tools",
            "-m",
            model,
            "-p",
            "ping",
        ])
        .assert()
        .success();

    drop(mock_server);
    let mut seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "expected exactly one upstream request");
    seen.remove(0)
}

fn assert_copilot_headers(seen: &Seen) {
    assert_eq!(
        seen.header("authorization"),
        format!("Bearer {COPILOT_TOKEN}")
    );
    assert!(seen.all("x-api-key").is_empty());
    assert_eq!(seen.all("user-agent"), ["GitHubCopilotChat/0.35.0"]);
    assert_eq!(seen.header("editor-version"), "vscode/1.107.0");
    assert_eq!(seen.header("copilot-integration-id"), "vscode-chat");
    assert_eq!(seen.header("openai-intent"), "conversation-edits");
    assert_eq!(seen.header("x-initiator"), "user");
}

#[tokio::test]
async fn copilot_routes_each_model_family_with_copilot_auth() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }

    for (model, body, path) in [
        (
            "github-copilot:claude-sonnet-5.5",
            text_sse("pong"),
            "/v1/messages",
        ),
        (
            "copilot:gpt-6-luna",
            RESPONSES_SSE.to_string(),
            "/responses",
        ),
        (
            "github-copilot:gemini-4-hydrogen",
            CHAT_COMPLETIONS_SSE.to_string(),
            "/chat/completions",
        ),
    ] {
        let seen = run_turn(model, body).await;
        assert_eq!(seen.path, path, "{model}");
        assert_copilot_headers(&seen);
    }
}
