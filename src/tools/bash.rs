//! Bash tool for executing shell commands.
//!
//! Allows the agent to run shell commands with safety guards.
//! Requires `--allow-bash` flag or the tool returns "denied".

use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

/// Returns the tool definition for the bash tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "bash".to_string(),
        description: "Execute a shell command. Returns stdout, stderr, and exit code. \
            Useful for running tools like rg (ripgrep), cargo test, etc."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        }),
    }
}

#[derive(Debug, Deserialize)]
struct BashInput {
    command: String,
}

/// Output from a bash command execution.
#[derive(Debug)]
pub struct BashOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

impl BashOutput {
    /// Converts to structured envelope format.
    pub fn into_tool_output(self) -> ToolOutput {
        ToolOutput::success(json!({
            "stdout": self.stdout,
            "stderr": self.stderr,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out
        }))
    }
}

/// Executes the bash tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext, timeout: Option<Duration>) -> ToolOutput {
    let input: BashInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Invalid input for bash tool: {}", e),
            );
        }
    };

    match run_command(&input.command, ctx, timeout).await {
        Ok(output) => output.into_tool_output(),
        Err(e) => e,
    }
}

/// Runs a shell command in the context's root directory.
async fn run_command(
    command: &str,
    ctx: &ToolContext,
    timeout: Option<Duration>,
) -> Result<BashOutput, ToolOutput> {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&ctx.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            ToolOutput::failure(
                "spawn_error",
                format!("Failed to execute command '{}': {}", command, e),
            )
        })?;

    let output_fut = child.wait_with_output();
    let output = match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, output_fut).await {
            Ok(result) => result,
            Err(_) => {
                return Ok(BashOutput {
                    stdout: String::new(),
                    stderr: format!("Command timed out after {} seconds", timeout.as_secs()),
                    exit_code: -1,
                    timed_out: true,
                });
            }
        },
        None => output_fut.await,
    }
    .map_err(|e| {
        ToolOutput::failure(
            "exec_error",
            format!("Failed to execute command '{}': {}", command, e),
        )
    })?;

    Ok(BashOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
        timed_out: false,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn test_bash_executes_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"command": "echo hello"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""stdout":"hello"#));
        assert!(json_str.contains(r#""exit_code":0"#));
        assert!(json_str.contains(r#""timed_out":false"#));
    }

    #[tokio::test]
    async fn test_bash_captures_stderr() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"command": "echo error >&2"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""stderr":"error"#));
    }

    #[tokio::test]
    async fn test_bash_captures_exit_code() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"command": "exit 42"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""exit_code":42"#));
    }

    #[tokio::test]
    async fn test_bash_runs_in_root_directory() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("test.txt"), "content").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"command": "ls"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains("test.txt"));
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"command": "sleep 5"});

        let result = execute(&input, &ctx, Some(Duration::from_millis(100))).await;
        assert!(result.is_ok()); // timed_out is success with timed_out=true
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""timed_out":true"#));
    }

    #[tokio::test]
    async fn test_bash_invalid_input() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"wrong_field": "ls"});

        let result = execute(&input, &ctx, None).await;
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }
}
