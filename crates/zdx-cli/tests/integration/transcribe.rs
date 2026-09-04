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
