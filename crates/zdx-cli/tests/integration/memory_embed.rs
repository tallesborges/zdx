//! Integration tests for hosted memory embeddings (`zdx memory index --embed`
//! and vector/hybrid `zdx memory search`) against a mock OpenAI-compatible
//! embeddings endpoint.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn write_notes(temp_dir: &TempDir) {
    let notes_dir = temp_dir.path().join("memory").join("Notes");
    fs::create_dir_all(&notes_dir).unwrap();
    fs::write(
        notes_dir.join("Solar.md"),
        "# Solar\n\nsolar panel sizing for the chacara roof\n",
    )
    .unwrap();
    fs::write(
        notes_dir.join("Cooking.md"),
        "# Cooking\n\npasta recipe with garlic and butter\n",
    )
    .unwrap();
}

fn write_embeddings_config(temp_dir: &TempDir, base_url: &str, max_run_tokens: u64) {
    fs::write(
        temp_dir.path().join("config.toml"),
        format!(
            r#"[memory.embeddings]
provider = "openai"
model = "test-embed"
base_url = "{base_url}"
sources = ["note"]
usd_per_million_tokens = 0.02
max_run_tokens = {max_run_tokens}
"#
        ),
    )
    .unwrap();
}

/// Mock embeddings endpoint: returns an axis vector per input based on
/// content, so "solar" queries rank the solar note first. Counts calls.
async fn mount_embeddings_mock(server: &MockServer) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(move |request: &Request| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let inputs = body["input"].as_array().unwrap();
            let data: Vec<serde_json::Value> = inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    let text = input.as_str().unwrap_or_default().to_lowercase();
                    let embedding = if text.contains("solar") {
                        [1.0, 0.0, 0.0]
                    } else {
                        [0.0, 1.0, 0.0]
                    };
                    serde_json::json!({ "index": index, "embedding": embedding })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": data,
                "usage": { "prompt_tokens": 42 }
            }))
        })
        .mount(server)
        .await;
    calls
}

#[tokio::test]
async fn test_memory_embed_flow_incremental_and_semantic_search() {
    let temp_dir = TempDir::new().unwrap();
    write_notes(&temp_dir);
    let server = MockServer::start().await;
    let calls = mount_embeddings_mock(&server).await;
    write_embeddings_config(&temp_dir, &server.uri(), 1_000_000);

    // Dry run performs no provider calls and no database writes.
    let dry = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env("OPENAI_API_KEY", "test-key")
        .args(["memory", "index", "--embed", "--dry-run", "--json"])
        .assert()
        .success();
    let dry: serde_json::Value = serde_json::from_slice(&dry.get_output().stdout).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(dry["embeddings"]["provider"], "openai");
    assert!(!temp_dir.path().join("cache").join("memory.sqlite").exists());

    // Real run embeds the allowlisted note chunks.
    let first = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env("OPENAI_API_KEY", "test-key")
        .args(["memory", "index", "--embed", "--json"])
        .assert()
        .success();
    let first: serde_json::Value = serde_json::from_slice(&first.get_output().stdout).unwrap();
    let first_calls = calls.load(Ordering::SeqCst);
    assert!(first_calls >= 1);
    assert_eq!(first["embeddings"]["state"], "ready");
    assert!(first["embeddings"]["pending_inputs"].as_u64().unwrap() >= 2);
    assert!(first["embeddings"]["actual_tokens"].as_u64().is_some());

    // Second run embeds zero unchanged inputs and spends zero hosted tokens.
    let second = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env("OPENAI_API_KEY", "test-key")
        .args(["memory", "index", "--embed", "--json"])
        .assert()
        .success();
    let second: serde_json::Value = serde_json::from_slice(&second.get_output().stdout).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), first_calls);
    assert_eq!(second["embeddings"]["pending_inputs"], 0);
    assert!(second["embeddings"]["cached_inputs"].as_u64().unwrap() >= 2);
    assert_eq!(second["embeddings"]["actual_tokens"], 0);
    assert_eq!(second["embeddings"]["state"], "ready");

    // Vector search embeds only the query and ranks the semantic match first.
    let vector = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env("OPENAI_API_KEY", "test-key")
        .args([
            "memory",
            "search",
            "solar sizing",
            "--strategy",
            "vector",
            "--json",
        ])
        .assert()
        .success();
    let vector: serde_json::Value = serde_json::from_slice(&vector.get_output().stdout).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), first_calls + 1);
    assert_eq!(vector["results"][0]["file"], "note://Solar.md");
    assert!(
        vector["warnings"][0]
            .as_str()
            .unwrap()
            .contains("query text was sent to openai")
    );

    // Hybrid search fuses lexical and vector rankings.
    let hybrid = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env("OPENAI_API_KEY", "test-key")
        .args([
            "memory",
            "search",
            "solar sizing",
            "--strategy",
            "hybrid",
            "--json",
        ])
        .assert()
        .success();
    let hybrid: serde_json::Value = serde_json::from_slice(&hybrid.get_output().stdout).unwrap();
    assert_eq!(hybrid["results"][0]["file"], "note://Solar.md");
}

#[tokio::test]
async fn test_memory_embed_refuses_over_budget_run_without_calls() {
    let temp_dir = TempDir::new().unwrap();
    write_notes(&temp_dir);
    let server = MockServer::start().await;
    let calls = mount_embeddings_mock(&server).await;
    write_embeddings_config(&temp_dir, &server.uri(), 1);

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env("OPENAI_API_KEY", "test-key")
        .args(["memory", "index"])
        .assert()
        .success();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .env("OPENAI_API_KEY", "test-key")
        .args(["memory", "index", "--embed"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("exceeds configured budget"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn test_memory_embed_and_vector_search_require_configuration() {
    let temp_dir = TempDir::new().unwrap();
    write_notes(&temp_dir);

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index", "--embed"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[memory.embeddings]"));

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "index"])
        .assert()
        .success();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", temp_dir.path())
        .args(["memory", "search", "solar", "--strategy", "vector"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[memory.embeddings]"));
}
