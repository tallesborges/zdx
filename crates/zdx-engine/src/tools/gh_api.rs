//! Read-only GitHub API tool.
//!
//! Wraps `gh api <path>` for GET requests only.
//!
//! It shells out to `gh` rather than talking to the REST API directly because
//! that is the only thing that actually authenticates on a normal machine:
//! `gh` stores its token in the OS keyring, so `~/.config/gh/hosts.yml` often
//! carries no `oauth_token` at all. Delegating also inherits `gh`'s own
//! precedence (`GH_TOKEN`/`GITHUB_TOKEN` over the keyring) and its enterprise
//! host handling for free.
//!
//! Read-only by construction: there is no method parameter and `--method` is
//! never passed, so `gh api` stays on its GET default. Creating issues,
//! comments, or reviews remains worker work.

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

/// Upper bound on returned text, so a large diff cannot flood the context.
const MAX_OUTPUT_CHARS: usize = 30_000;

/// Returns the tool definition for the GitHub API tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Gh_Api".to_string(),
        description: "Read the GitHub REST API through `gh`, for repositories that are not checked out locally. Use it whenever the user shares a GitHub URL (PR, issue, commit, file, release, compare, run) or asks about a repo you do not have on disk — inspect it here rather than guessing or fetching the HTML page. For a repository you do have locally, prefer the `git` tool.\n\n`path` is the API path with no leading slash and no host, for example `repos/example-org/verifiable/readme`, `repos/OWNER/REPO/pulls/123`, `repos/OWNER/REPO/issues/45/comments`, `repos/OWNER/REPO/commits/abc123`, or a query string like `repos/OWNER/REPO/pulls?state=open&per_page=5`. Map a browser URL onto the API: `github.com/O/R/pull/7` is `repos/O/R/pulls/7`.\n\nGET only — this tool cannot create, edit, merge, or comment on anything, so it is safe to call freely. Authentication is whatever `gh` is logged in as, so private repositories you can see in `gh` work here too.\n\nFile contents come back base64-encoded from the JSON API; this tool decodes them for you, so `repos/O/R/readme` and `repos/O/R/contents/path/to/file` return readable text. Use `media_type: \"diff\"` on a pull request or commit path to get an actual unified diff, `\"patch\"` for a mail-style patch, and `\"raw\"` to fetch a single file's bytes directly. Output is truncated at 30000 characters — prefer a narrower path or a specific file over a whole listing."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "GitHub REST API path without a leading slash or host, e.g. `repos/OWNER/REPO/pulls/123`. May include a query string."
                },
                "media_type": {
                    "type": "string",
                    "description": "Response format. `json` (default) returns the JSON body with any base64 file content decoded. `diff`/`patch` turn a pull request or commit path into a real diff. `raw` returns a single file's contents directly. `text`/`html`/`full` render issue and comment bodies.",
                    "enum": ["json", "raw", "diff", "patch", "text", "html", "full"]
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct GhApiInput {
    path: String,
    media_type: Option<String>,
}

/// Maps a media type name onto its GitHub `Accept` header value.
fn accept_header(media_type: &str) -> Result<Option<String>, ToolOutput> {
    let value = match media_type.trim().to_ascii_lowercase().as_str() {
        "" | "json" => return Ok(None),
        other @ ("raw" | "diff" | "patch" | "text" | "html" | "full") => other.to_string(),
        other => {
            return Err(ToolOutput::failure(
                "invalid_input",
                format!("Unknown media_type `{other}`"),
                Some("Expected json, raw, diff, patch, text, html, or full".to_string()),
            ));
        }
    };
    Ok(Some(format!("application/vnd.github.{value}")))
}

/// Rejects a path that `gh` could read as an option or that points off-host.
fn checked_path(raw: &str) -> Result<String, ToolOutput> {
    let trimmed = raw.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(ToolOutput::failure(
            "invalid_input",
            "`path` cannot be empty",
            None,
        ));
    }
    if trimmed.starts_with('-') {
        return Err(ToolOutput::failure(
            "invalid_input",
            "`path` must not start with '-'",
            Some("This tool builds its own flags; only an API path is accepted.".to_string()),
        ));
    }
    // A full URL would let the caller pick the host; keep it on the API.
    if trimmed.contains("://") {
        return Err(ToolOutput::failure(
            "invalid_input",
            "`path` must be an API path, not a URL",
            Some(
                "Use `repos/OWNER/REPO/pulls/123` rather than a github.com or api.github.com URL."
                    .to_string(),
            ),
        ));
    }
    Ok(trimmed.to_string())
}

/// Executes the GitHub API tool and returns a structured envelope.
pub async fn execute(input: &Value, _ctx: &ToolContext) -> ToolOutput {
    let input: GhApiInput = match serde_json::from_value(input.clone()) {
        Ok(value) => value,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for gh_api tool",
                Some(format!("Parse error: {err}")),
            );
        }
    };

    let path = match checked_path(&input.path) {
        Ok(path) => path,
        Err(output) => return output,
    };
    let media_type = input
        .media_type
        .as_deref()
        .map(str::trim)
        .filter(|media_type| !media_type.is_empty())
        .unwrap_or("json")
        .to_string();
    let accept = match accept_header(&media_type) {
        Ok(accept) => accept,
        Err(output) => return output,
    };

    // Fixed argument list. `gh api` defaults to GET and no `--method` is ever
    // passed, so there is no request here that can change state.
    let mut args: Vec<String> = vec!["api".to_string(), path.clone()];
    if let Some(accept) = accept {
        args.push("-H".to_string());
        args.push(format!("Accept: {accept}"));
    }

    let output = match Command::new("gh").args(&args).output().await {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return ToolOutput::failure(
                "execution_failed",
                "The `gh` CLI is not installed",
                Some("Install GitHub CLI and run `gh auth login`.".to_string()),
            );
        }
        Err(err) => {
            return ToolOutput::failure(
                "execution_failed",
                "Failed to run gh api",
                Some(err.to_string()),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if stderr.is_empty() { stdout } else { stderr };
        return ToolOutput::failure(
            "execution_failed",
            format!("gh api {path} failed"),
            Some(if details.is_empty() {
                "gh exited with a non-zero status".to_string()
            } else {
                truncate(&details, 2_000).0
            }),
        );
    }

    let body = String::from_utf8_lossy(&output.stdout).to_string();
    let body = decode_base64_content(&body).unwrap_or(body);
    let (body, truncated) = truncate(body.trim_end(), MAX_OUTPUT_CHARS);

    ToolOutput::success(json!({
        "path": path,
        "media_type": media_type,
        "body": body,
        "truncated": truncated,
    }))
}

/// Rewrites a GitHub contents response so `content` holds readable text.
///
/// The JSON API returns file contents as base64 with embedded newlines, which
/// is why the shell idiom `--jq '.content' | base64 -d` fails on some
/// decoders. Decoding here — and replacing the field rather than adding one —
/// keeps the surrounding metadata without carrying the blob twice.
fn decode_base64_content(body: &str) -> Option<String> {
    use base64::Engine as _;

    let mut value: Value = serde_json::from_str(body).ok()?;
    let object = value.as_object_mut()?;
    if object.get("encoding").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let encoded = object.get("content").and_then(Value::as_str)?;

    // GitHub wraps the payload, and standard base64 rejects the newlines.
    let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .ok()?;
    // Binary files stay encoded rather than becoming replacement characters.
    let text = String::from_utf8(bytes).ok()?;

    object.insert("content".to_string(), Value::String(text));
    object.insert("encoding".to_string(), Value::String("utf-8".to_string()));
    serde_json::to_string_pretty(&value).ok()
}

fn truncate(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir(), None)
    }

    #[test]
    fn definition_takes_a_path_and_has_no_method() {
        let definition = definition();
        assert_eq!(definition.name, "Gh_Api");

        let props = definition.input_schema["properties"]
            .as_object()
            .expect("properties");
        // A method parameter would make mutations reachable.
        assert!(!props.contains_key("method"));
        assert!(!props.contains_key("body"));
        assert_eq!(props.len(), 2);

        let required = definition.input_schema["required"]
            .as_array()
            .expect("required");
        assert!(required.contains(&json!("path")));
    }

    #[tokio::test]
    async fn rejects_flag_shaped_and_url_paths() {
        for bad in [
            json!({ "path": "--method=POST" }),
            json!({ "path": "-X" }),
            json!({ "path": "   " }),
            json!({ "path": "https://api.github.com/repos/x/y" }),
            json!({ "path": "http://evil.example/repos" }),
        ] {
            let output = execute(&bad, &ctx()).await;
            assert!(!output.is_ok(), "expected rejection for {bad}");
        }
    }

    #[tokio::test]
    async fn rejects_unknown_media_types() {
        let output = execute(
            &json!({ "path": "repos/o/r", "media_type": "sarif" }),
            &ctx(),
        )
        .await;
        assert!(!output.is_ok());
    }

    #[test]
    fn strips_leading_slash_and_keeps_query_strings() {
        assert_eq!(checked_path("/repos/o/r").unwrap(), "repos/o/r");
        assert_eq!(
            checked_path("repos/o/r/pulls?state=open").unwrap(),
            "repos/o/r/pulls?state=open"
        );
    }

    #[test]
    fn maps_media_types_to_accept_headers() {
        assert_eq!(accept_header("").unwrap(), None);
        assert_eq!(accept_header(" \t\n").unwrap(), None);
        assert_eq!(accept_header("json").unwrap(), None);
        assert_eq!(
            accept_header("diff").unwrap().as_deref(),
            Some("application/vnd.github.diff")
        );
        assert_eq!(
            accept_header("RAW").unwrap().as_deref(),
            Some("application/vnd.github.raw")
        );
        assert!(accept_header("nope").is_err());
    }

    #[test]
    fn decodes_wrapped_base64_content() {
        use base64::Engine as _;
        // GitHub wraps base64 across lines; the naive shell decode chokes here.
        let raw = "# Title\n\nBody line.\n".repeat(4);
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
        let wrapped = encoded
            .as_bytes()
            .chunks(60)
            .map(|c| String::from_utf8_lossy(c).to_string())
            .collect::<Vec<_>>()
            .join("\n");

        let body = serde_json::to_string(&json!({
            "name": "README.md",
            "sha": "abc",
            "encoding": "base64",
            "content": wrapped,
        }))
        .unwrap();

        let decoded = decode_base64_content(&body).expect("decoded");
        assert!(decoded.contains("# Title"));
        assert!(decoded.contains("\"encoding\": \"utf-8\""));
        // Metadata survives, and the blob is not carried twice.
        assert!(decoded.contains("\"sha\": \"abc\""));
        assert!(!decoded.contains(&wrapped));
    }

    #[test]
    fn leaves_non_content_and_binary_responses_alone() {
        // No encoding field (a normal API object).
        assert!(decode_base64_content(r#"{"full_name":"o/r"}"#).is_none());
        // An array response.
        assert!(decode_base64_content(r#"[{"number":1}]"#).is_none());
        // Base64 that is not valid UTF-8 stays encoded rather than becoming
        // replacement characters.
        let binary = json!({ "encoding": "base64", "content": "//7/AA==" }).to_string();
        assert!(decode_base64_content(&binary).is_none());
    }
}
