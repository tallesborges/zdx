//! Read-only git tool.
//!
//! Answers "what is the state of this repo" — `status`, `log`, `diff`, `show`
//! — and nothing else.
//!
//! Read-only by construction, not by convention: the subcommand is chosen from
//! a fixed set, every flag is built in code, and no caller-supplied string is
//! ever used as a flag. Refs and paths are rejected if they could be read as
//! options, and pathspecs are passed after `--`, so there is no argument that
//! turns one of these into a mutating command.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use super::{ToolContext, ToolDefinition, resolve_existing_path};
use crate::core::events::ToolOutput;

/// Upper bound on returned text, so a huge diff cannot flood the context.
const MAX_OUTPUT_CHARS: usize = 20_000;
/// Default and maximum commit counts for `log`.
const DEFAULT_LOG_LIMIT: i32 = 20;
const MAX_LOG_LIMIT: i32 = 200;

/// Returns the tool definition for the git tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Git".to_string(),
        description: "Inspect a git repository's state, read-only: `status` (working tree changes and branch), `log` (recent commits), `diff` (uncommitted changes, or changes against a revision), `show` (a single commit's message and patch). Use it to answer what changed, what is uncommitted, what landed recently, or what a commit did, without starting a worker.\n\nThis tool cannot modify anything — there is no commit, stage, checkout, push, or fetch, and no way to pass arbitrary flags — so it is safe to call freely. It reads the local repository only and never contacts a remote.\n\n`repo` defaults to the current working directory; pass it to inspect another checkout. `path` scopes the result to a file or directory. Prefer `stat: true` on `diff`/`show` first when a change might be large, then re-run without it on the specific `path` you care about. Output is truncated at 20000 characters."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Which read-only inspection to run.",
                    "enum": ["status", "log", "diff", "show"]
                },
                "repo": {
                    "type": "string",
                    "description": "Repository path. Defaults to the current working directory. Relative paths resolve from it; $VAR and ~ are expanded."
                },
                "revision": {
                    "type": "string",
                    "description": "Commit-ish. Required for `show` (the commit to display). For `diff`, the base to compare the working tree against (e.g. `HEAD~3`, a branch, or `main...HEAD`). For `log`, the starting point. Must not begin with `-`."
                },
                "path": {
                    "type": "string",
                    "description": "Optional file or directory to scope the result to. Must not begin with `-`."
                },
                "staged": {
                    "type": "boolean",
                    "description": "For `diff`: show staged changes instead of unstaged ones. Ignored by other actions."
                },
                "stat": {
                    "type": "boolean",
                    "description": "For `diff`/`show`: return a per-file summary instead of the full patch. Use this first when the change may be large."
                },
                "limit": {
                    "type": "integer",
                    "description": "For `log`: how many commits to return (default 20, max 200)."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct GitInput {
    action: String,
    repo: Option<String>,
    revision: Option<String>,
    path: Option<String>,
    #[serde(default, deserialize_with = "zdx_tools::bool_or_string::deserialize")]
    staged: bool,
    #[serde(default, deserialize_with = "zdx_tools::bool_or_string::deserialize")]
    stat: bool,
    // Models routinely send numeric args as strings; accept both.
    #[serde(
        default,
        deserialize_with = "zdx_tools::i64_or_string::deserialize_optional"
    )]
    limit: Option<i64>,
}

/// Rejects a caller-supplied value that git could read as an option.
///
/// Combined with the `--` separators below, this is what keeps the fixed
/// argument lists fixed.
fn checked_operand(value: &str, field: &str) -> Result<String, ToolOutput> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolOutput::failure(
            "invalid_input",
            format!("`{field}` cannot be empty"),
            None,
        ));
    }
    if trimmed.starts_with('-') {
        return Err(ToolOutput::failure(
            "invalid_input",
            format!("`{field}` must not start with '-'"),
            Some("This tool builds its own flags; only refs and paths are accepted.".to_string()),
        ));
    }
    Ok(trimmed.to_string())
}

/// Executes the git tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: GitInput = match serde_json::from_value(input.clone()) {
        Ok(value) => value,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for git tool",
                Some(format!("Parse error: {err}")),
            );
        }
    };

    let repo = match input
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(repo) => match resolve_existing_path(repo, &ctx.root) {
            Ok(resolved) => resolved.resolved_path,
            Err(output) => return output,
        },
        None => ctx.root.clone(),
    };

    let revision = match input.revision.as_deref() {
        Some(value) => match checked_operand(value, "revision") {
            Ok(value) => Some(value),
            Err(output) => return output,
        },
        None => None,
    };
    let path = match input.path.as_deref() {
        Some(value) => match checked_operand(value, "path") {
            Ok(value) => Some(value),
            Err(output) => return output,
        },
        None => None,
    };

    let action = input.action.trim();
    let mut args: Vec<String> = vec!["--no-pager".to_string()];

    match action {
        "status" => {
            args.extend(["status".into(), "--short".into(), "--branch".into()]);
        }
        "log" => {
            let limit = input
                .limit
                .unwrap_or(i64::from(DEFAULT_LOG_LIMIT))
                .clamp(1, i64::from(MAX_LOG_LIMIT));
            args.extend([
                "log".into(),
                "--no-ext-diff".into(),
                format!("--max-count={limit}"),
                "--date=short".into(),
                "--format=%h %ad %an %s".into(),
            ]);
            if let Some(revision) = &revision {
                args.push(revision.clone());
            }
        }
        "diff" => {
            args.extend(["diff".into(), "--no-ext-diff".into()]);
            if input.staged {
                args.push("--cached".into());
            }
            if input.stat {
                args.push("--stat".into());
            }
            if let Some(revision) = &revision {
                args.push(revision.clone());
            }
        }
        "show" => {
            let Some(revision) = &revision else {
                return ToolOutput::failure(
                    "invalid_input",
                    "`revision` is required for action `show`",
                    None,
                );
            };
            args.extend(["show".into(), "--no-ext-diff".into()]);
            if input.stat {
                args.push("--stat".into());
            }
            args.push(revision.clone());
        }
        other => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Unknown action `{other}`"),
                Some("Expected status, log, diff, or show".to_string()),
            );
        }
    }

    // Everything after `--` is a pathspec, never an option.
    if let Some(path) = &path {
        args.push("--".into());
        args.push(path.clone());
    }

    run_git(&repo, &args, action).await
}

async fn run_git(repo: &Path, args: &[String], action: &str) -> ToolOutput {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .await;

    let output = match output {
        Ok(output) => output,
        Err(err) => {
            return ToolOutput::failure(
                "execution_failed",
                format!("Failed to run git {action}"),
                Some(err.to_string()),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return ToolOutput::failure(
            "execution_failed",
            format!("git {action} failed"),
            Some(if stderr.is_empty() {
                "git exited with a non-zero status".to_string()
            } else {
                stderr
            }),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = stdout.trim_end();
    let truncated = text.chars().count() > MAX_OUTPUT_CHARS;
    let body: String = if truncated {
        text.chars().take(MAX_OUTPUT_CHARS).collect()
    } else {
        text.to_string()
    };

    ToolOutput::success(json!({
        "action": action,
        "repo": repo.display().to_string(),
        "output": body,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "--quiet", "-m", "first commit"]);
        dir
    }

    fn ctx_for(dir: &tempfile::TempDir) -> ToolContext {
        ToolContext::new(dir.path().to_path_buf(), None)
    }

    fn output_text(out: &ToolOutput) -> String {
        out.data().unwrap()["output"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn status_log_diff_show_read_repo_state() {
        let dir = init_repo();
        let ctx = ctx_for(&dir);

        let log = execute(&json!({ "action": "log" }), &ctx).await;
        assert!(log.is_ok());
        assert!(output_text(&log).contains("first commit"));

        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();

        let status = execute(&json!({ "action": "status" }), &ctx).await;
        assert!(status.is_ok());
        assert!(output_text(&status).contains("a.txt"));

        let diff = execute(&json!({ "action": "diff" }), &ctx).await;
        assert!(diff.is_ok());
        assert!(output_text(&diff).contains("-one"));

        let show = execute(&json!({ "action": "show", "revision": "HEAD" }), &ctx).await;
        assert!(show.is_ok());
        assert!(output_text(&show).contains("first commit"));
    }

    #[tokio::test]
    async fn rejects_flag_shaped_operands() {
        let dir = init_repo();
        let ctx = ctx_for(&dir);

        // A ref or path that git could read as an option must be refused
        // outright rather than reaching the command line.
        for input in [
            json!({ "action": "show", "revision": "--help" }),
            json!({ "action": "log", "revision": "--output=/tmp/pwned" }),
            json!({ "action": "diff", "path": "--output=/tmp/pwned" }),
            json!({ "action": "status", "path": "-x" }),
        ] {
            let output = execute(&input, &ctx).await;
            assert!(!output.is_ok(), "expected rejection for {input}");
        }
    }

    #[tokio::test]
    async fn rejects_unknown_and_mutating_actions() {
        let dir = init_repo();
        let ctx = ctx_for(&dir);

        for action in ["commit", "push", "checkout", "reset", "clean", "fetch"] {
            let output = execute(&json!({ "action": action }), &ctx).await;
            assert!(!output.is_ok(), "{action} must not be reachable");
        }
    }

    #[tokio::test]
    async fn show_requires_a_revision_and_log_limit_is_capped() {
        let dir = init_repo();
        let ctx = ctx_for(&dir);

        assert!(!execute(&json!({ "action": "show" }), &ctx).await.is_ok());

        // Out-of-range limits clamp instead of failing or being passed through.
        let log = execute(&json!({ "action": "log", "limit": 100_000 }), &ctx).await;
        assert!(log.is_ok());
    }

    #[tokio::test]
    async fn accepts_booleans_as_strings() {
        // Models routinely send booleans as JSON strings; a live probe showed
        // three consecutive calls lost to `stat: "true"`.
        let dir = init_repo();
        let ctx = ctx_for(&dir);

        let output = execute(
            &json!({ "action": "show", "revision": "HEAD", "stat": "true" }),
            &ctx,
        )
        .await;
        assert!(output.is_ok(), "string boolean must parse");
        assert!(output_text(&output).contains("a.txt"));

        let real_bool = execute(
            &json!({ "action": "show", "revision": "HEAD", "stat": true }),
            &ctx,
        )
        .await;
        assert!(real_bool.is_ok());
    }

    #[tokio::test]
    async fn reports_git_failure_for_a_bad_revision() {
        let dir = init_repo();
        let ctx = ctx_for(&dir);
        let output = execute(
            &json!({ "action": "show", "revision": "no-such-ref" }),
            &ctx,
        )
        .await;
        assert!(!output.is_ok());
    }
}
