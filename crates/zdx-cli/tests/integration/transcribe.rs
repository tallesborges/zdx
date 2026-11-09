//! Integration tests for `zdx transcribe`.

use std::io::Write;

use predicates::prelude::*;

#[test]
fn test_transcribe_help_shows_flags() {
    crate::fixtures::zdx_cmd()
        .args(["transcribe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--language"))
        .stdout(predicate::str::contains("--diarize"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--list-models"));
}

#[test]
fn test_transcribe_list_models_lists_providers() {
    crate::fixtures::zdx_cmd()
        .args(["transcribe", "--list-models"])
        .assert()
        .success()
        .stdout(predicate::str::contains("elevenlabs:scribe_v2"))
        .stdout(predicate::str::contains("mistral:voxtral-mini-latest"));
}

#[test]
fn test_transcribe_requires_file_argument() {
    crate::fixtures::zdx_cmd()
        .args(["transcribe"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<FILE>"));
}

#[test]
fn test_transcribe_missing_file_errors() {
    crate::fixtures::zdx_cmd()
        .args(["transcribe", "/no/such/path/audio.ogg"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read audio file"));
}

#[test]
fn test_transcribe_reports_when_no_provider_configured() {
    let home = tempfile::tempdir().expect("temp home");
    let mut audio = tempfile::Builder::new()
        .suffix(".ogg")
        .tempfile()
        .expect("temp audio");
    audio.write_all(b"not-real-audio").expect("write audio");

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home.path())
        .env_remove("OPENAI_API_KEY")
        .env_remove("MISTRAL_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("ELEVENLABS_API_KEY")
        .env_remove("ZDX_TRANSCRIPTION_MODEL")
        .args(["transcribe", audio.path().to_str().expect("audio path")])
        .assert()
        .success()
        .stderr(predicate::str::contains("No transcription provider"));
}

/// The global vocabulary and `--vocabulary` files reach Mistral as one
/// `context_bias` field per entry: extra files first, case duplicates
/// collapsed to the first spelling. Voxtral Transcribe 2 rejects the whole
/// request when one entry has a space or a comma (the mock answers like the
/// real API), so such entries are skipped and the transcription still runs.
#[tokio::test]
async fn test_transcribe_sends_vocabulary_to_mistral() {
    use std::sync::{Arc, Mutex};

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    let home = tempfile::tempdir().expect("temp home");
    let work = tempfile::tempdir().expect("temp work");
    std::fs::write(
        home.path().join("vocabulary.txt"),
        "# global\nTermA\nsharedterm\nTwo Words\nterm,list\n",
    )
    .unwrap();
    let extra = work.path().join("meeting.txt");
    std::fs::write(&extra, "SharedTerm\n\nTermB\n").unwrap();
    let audio = work.path().join("clip.ogg");
    std::fs::write(&audio, b"not-real-audio").unwrap();

    let server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen_clone = Arc::clone(&seen);
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(move |req: &Request| {
            let body = String::from_utf8_lossy(&req.body).into_owned();
            let terms = context_bias_fields(&body);
            *seen_clone.lock().unwrap() = terms.clone();
            if let Some(bad) = terms
                .iter()
                .find(|term| term.contains(|c: char| c.is_whitespace() || c == ','))
            {
                return ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "object": "error",
                    "message": format!(
                        "Context bias item '{bad}' must not contain commas or whitespace (context_bias_input_method=comma_separated)"
                    ),
                    "type": "invalid_request_invalid_args",
                    "code": "3051",
                }));
            }
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": "ok" }))
        })
        .expect(1)
        .mount(&server)
        .await;

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home.path())
        .env("MISTRAL_API_KEY", "test-api-key")
        .env("MISTRAL_BASE_URL", format!("{}/v1", server.uri()))
        .env_remove("ZDX_TRANSCRIPTION_MODEL")
        .args([
            "transcribe",
            "--model",
            "mistral:voxtral-mini-latest",
            "--vocabulary",
            extra.to_str().unwrap(),
            audio.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));

    assert_eq!(*seen.lock().unwrap(), ["SharedTerm", "TermB", "TermA"]);
}

/// The value of every `context_bias` field in a multipart body, in order.
fn context_bias_fields(body: &str) -> Vec<String> {
    body.split("name=\"context_bias\"")
        .skip(1)
        .map(|part| {
            let value = part.trim_start_matches(['\r', '\n']);
            value.split("\r\n--").next().unwrap_or_default().to_string()
        })
        .collect()
}
