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
/// collapsed to the first spelling.
#[tokio::test]
async fn test_transcribe_sends_vocabulary_to_mistral() {
    use std::sync::{Arc, Mutex};

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    let home = tempfile::tempdir().expect("temp home");
    let work = tempfile::tempdir().expect("temp work");
    std::fs::write(
        home.path().join("vocabulary.txt"),
        "# global\nTermA\nshared term\n",
    )
    .unwrap();
    let extra = work.path().join("meeting.txt");
    std::fs::write(&extra, "Shared Term\n\nTermB\n").unwrap();
    let audio = work.path().join("clip.ogg");
    std::fs::write(&audio, b"not-real-audio").unwrap();

    let server = MockServer::start().await;
    let seen: Arc<Mutex<Vec<u8>>> = Arc::default();
    let seen_clone = Arc::clone(&seen);
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(move |req: &Request| {
            *seen_clone.lock().unwrap() = req.body.clone();
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
            "mistral",
            "--vocabulary",
            extra.to_str().unwrap(),
            audio.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));

    let body = String::from_utf8_lossy(&seen.lock().unwrap()).into_owned();
    let terms: Vec<&str> = body
        .split("name=\"context_bias\"")
        .skip(1)
        .map(|part| part.trim_start().lines().next().unwrap_or_default().trim())
        .collect();
    assert_eq!(terms, ["Shared Term", "TermB", "TermA"]);
}
