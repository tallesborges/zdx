//! `zdx imagine` with `OpenAI` Images API models (`gpt-image-2.6-*`).
//!
//! The provider request body is covered by unit tests in `zdx-providers`;
//! these tests cover the CLI contract: which endpoint is called, which file is
//! written, and which flag combinations fail before any request is sent.

use predicates::prelude::*;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL: &str = "openai:gpt-image-2.6-flare";
const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
/// `b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"`
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUg==";
const SVG_B64: &str = "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4=";

fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

fn imagine_cmd(home: &TempDir, base_url: &str) -> assert_cmd::Command {
    let mut cmd = crate::fixtures::zdx_cmd();
    cmd.env("ZDX_HOME", home.path())
        .env("OPENAI_API_KEY", "test-api-key")
        .env("OPENAI_BASE_URL", base_url)
        .arg("imagine");
    cmd
}

fn write_file(dir: &TempDir, name: &str, bytes: &[u8]) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write fixture file");
    path.to_str().expect("utf-8 path").to_string()
}

#[tokio::test]
async fn svg_generation_saves_svg_file() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let home = TempDir::new().unwrap();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/images/generations"))
        .and(body_partial_json(json!({
            "model": "gpt-image-2.6-flare",
            "prompt": "A bakery logo",
            "output_format": "svg",
            "complexity": "low",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "created": 1,
            "data": [{"b64_json": SVG_B64}],
            "output_format": "svg",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = imagine_cmd(&home, &format!("{}/v1", server.uri()))
        .args([
            "-p",
            "A bakery logo",
            "--model",
            MODEL,
            "--format",
            "svg",
            "--complexity",
            "low",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let saved = String::from_utf8(output).unwrap();
    let saved = saved.trim();
    assert_eq!(
        std::path::Path::new(saved).extension(),
        Some(std::ffi::OsStr::new("svg")),
        "saved path: {saved}"
    );
    assert_eq!(std::fs::read(saved).unwrap(), SVG.to_vec());
}

#[tokio::test]
async fn markup_edit_posts_to_edits_endpoint() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    let home = TempDir::new().unwrap();
    let files = TempDir::new().unwrap();
    let source = write_file(&files, "kitchen.png", &[1, 2, 3]);
    let markup = write_file(&files, "circle.png", &[9]);
    let out = files.path().join("edited.png");
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/images/edits"))
        .and(body_partial_json(json!({
            "model": "gpt-image-2.6-flare",
            "images": [{"image_url": "data:image/png;base64,AQID"}],
            "markup": {"image_url": "data:image/png;base64,CQ=="},
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "created": 1,
            "data": [{"b64_json": "BwcH"}],
            "output_format": "png",
        })))
        .expect(1)
        .mount(&server)
        .await;

    imagine_cmd(&home, &format!("{}/v1", server.uri()))
        .args([
            "-p",
            "Make the circled vase blue",
            "--model",
            MODEL,
            "-s",
            &source,
            "--markup",
            &markup,
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(std::fs::read(&out).unwrap(), vec![7, 7, 7]);
}

#[test]
fn images_api_flags_are_rejected_for_other_providers() {
    let home = TempDir::new().unwrap();
    imagine_cmd(&home, "http://127.0.0.1:9/v1")
        .args([
            "-p",
            "x",
            "--model",
            "gemini:gemini-3.1-flash-image-preview",
        ])
        .args(["--format", "gif"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "only work with OpenAI Images API models",
        ))
        .stderr(predicate::str::contains("--format"));
}

#[test]
fn frames_without_gif_fail_before_any_request() {
    let home = TempDir::new().unwrap();
    imagine_cmd(&home, "http://127.0.0.1:9/v1")
        .args(["-p", "x", "--model", MODEL, "--frames", "4"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("require output format gif"));
}

#[test]
fn svg_with_source_fails_before_any_request() {
    let home = TempDir::new().unwrap();
    let files = TempDir::new().unwrap();
    let source = write_file(&files, "logo.png", &[1]);
    imagine_cmd(&home, "http://127.0.0.1:9/v1")
        .args([
            "-p", "x", "--model", MODEL, "--format", "svg", "-s", &source,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("generation only"));
}

#[test]
fn markup_requires_source() {
    let home = TempDir::new().unwrap();
    imagine_cmd(&home, "http://127.0.0.1:9/v1")
        .args(["-p", "x", "--model", MODEL, "--markup", "circle.png"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--source"));
}

#[test]
fn thinking_modifier_is_rejected_for_images_api() {
    let home = TempDir::new().unwrap();
    imagine_cmd(&home, "http://127.0.0.1:9/v1")
        .args(["-p", "x", "--model", "openai:gpt-image-2.6-flare@high"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("use --complexity"));
}

#[test]
fn codex_rejects_explicit_image_model() {
    let home = TempDir::new().unwrap();
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home.path())
        .args([
            "imagine",
            "-p",
            "x",
            "--model",
            "openai-codex:gpt-image-2.6-flare",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "accepts only 'openai-codex:gpt-image-2'",
        ));
}

#[test]
fn codex_rejects_size_and_images_api_flags() {
    let home = TempDir::new().unwrap();
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home.path())
        .args(["imagine", "-p", "x", "--model", "openai-codex:gpt-image-2"])
        .args(["--size", "1K"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("picks its own size"));

    for flags in [
        ["--format", "gif"],
        ["--format", "svg"],
        ["--complexity", "low"],
        ["--quality", "high"],
    ] {
        crate::fixtures::zdx_cmd()
            .env("ZDX_HOME", home.path())
            .args(["imagine", "-p", "x", "--model", "openai-codex:gpt-image-2"])
            .args(flags)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "only work with OpenAI Images API models",
            ));
    }
}

/// A valid Codex request reaches subscription auth (and stops there without
/// credentials), so no request leaves the machine.
#[test]
fn codex_baseline_request_needs_subscription_credentials() {
    let home = TempDir::new().unwrap();
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", home.path())
        .args(["imagine", "-p", "x", "--model", "openai-codex:gpt-image-2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("server-managed"))
        .stderr(predicate::str::contains(
            "No OpenAI Codex OAuth credentials found",
        ));
}

/// A backend can ignore the requested GIF or SVG output. Requested and reported
/// formats must not override the bytes: PNG bytes are saved as `.png`.
#[tokio::test]
async fn ignored_format_request_saves_actual_png_bytes() {
    if !can_bind_localhost() {
        eprintln!("Skipping: cannot bind localhost TCP port in this environment.");
        return;
    }
    for format in ["gif", "svg"] {
        let home = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .and(body_partial_json(json!({"output_format": format})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "created": 1,
                "data": [{"b64_json": PNG_B64}],
                "output_format": format,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let assert = imagine_cmd(&home, &format!("{}/v1", server.uri()))
            .args(["-p", "A waving owl", "--model", MODEL, "--format", format])
            .assert()
            .success()
            .stderr(predicate::str::contains(format!(
                "requested --format {format} but the API returned image/png"
            )));

        let saved = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        let saved = std::path::Path::new(saved.trim());
        assert_eq!(saved.extension(), Some(std::ffi::OsStr::new("png")));
        assert!(std::fs::read(saved).unwrap().starts_with(b"\x89PNG"));
    }
}
