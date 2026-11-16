//! A failed turn leaves a `turn_failed` notice in the thread: the error's
//! message with credentials redacted, once per failed turn, and never sent
//! back to the model on the next turn.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::fixtures::sse_response;

const CHAT_COMPLETIONS_SSE: &str = concat!(
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":",
    "[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

fn notices(zdx_home: &Path, thread: &str) -> Vec<Value> {
    std::fs::read_to_string(zdx_home.join("threads").join(format!("{thread}.jsonl")))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["type"] == "notice")
        .collect()
}

#[tokio::test]
async fn test_a_failed_turn_is_saved_as_a_notice() {
    if std::net::TcpListener::bind("127.0.0.1:0").is_err() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let zdx_home = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let mock_server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);
    let failing = Arc::new(Mutex::new(true));
    let failing_clone = Arc::clone(&failing);
    Mock::given(method("POST"))
        .respond_with(move |req: &Request| {
            seen_clone
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&req.body).unwrap_or(Value::Null));
            if *failing_clone.lock().unwrap() {
                ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": {
                        "message": "Incorrect API key provided: sk-proj-abcdefghijklmnop1234",
                        "type": "invalid_request_error",
                    }
                }))
            } else {
                sse_response(CHAT_COMPLETIONS_SSE)
            }
        })
        .mount(&mock_server)
        .await;
    std::fs::write(
        zdx_home.path().join("config.toml"),
        format!(
            "model = \"proxy:default-model\"\n\
             [providers.custom.proxy]\nbase_url = \"{}/v1\"\napi_key = \"test-api-key\"\n\
             models = [\"default-model\"]\n",
            mock_server.uri()
        ),
    )
    .unwrap();
    let run = |prompt: &str| {
        crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", zdx_home.path())
            .args(["--root", work_dir.path().to_str().unwrap()])
            .args(["--thread", "failing", "exec", "-p", prompt])
            .output()
            .unwrap()
    };

    assert!(!run("first").status.success());
    let saved = notices(zdx_home.path(), "failing");
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!(saved[0]["kind"], "turn_failed");
    let message = saved[0]["message"].as_str().unwrap();
    assert!(message.contains("HTTP 400"), "{message}");
    assert!(message.contains("Incorrect API key provided"), "{message}");
    assert!(message.contains("[redacted]"), "{message}");
    assert!(
        !message.contains("sk-proj-abcdefghijklmnop1234"),
        "{message}"
    );

    *failing.lock().unwrap() = false;
    assert!(run("second").status.success());
    assert_eq!(notices(zdx_home.path(), "failing").len(), 1);
    let last = seen.lock().unwrap().last().unwrap().to_string();
    assert!(
        !last.contains("Incorrect API key"),
        "the notice went to the model: {last}"
    );
    assert!(last.contains("first") && last.contains("second"), "{last}");
}
