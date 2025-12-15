//! Bash tool for executing shell commands.
//!
//! Allows the agent to run shell commands with safety guards.
//! Requires `--allow-bash` flag or the tool returns "denied".

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Command;

use super::{ToolContext, ToolDefinition};

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
}

impl std::fmt::Display for BashOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit_code: {}\n\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.exit_code, self.stdout, self.stderr
        )
    }
}

/// Executes the bash tool.
pub fn execute(input: &Value, ctx: &ToolContext) -> Result<String> {
    let input: BashInput =
        serde_json::from_value(input.clone()).context("Invalid input for bash tool")?;

    let output = run_command(&input.command, ctx)?;
    Ok(output.to_string())
}

/// Runs a shell command in the context's root directory.
fn run_command(command: &str, ctx: &ToolContext) -> Result<BashOutput> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&ctx.root)
        .output()
        .with_context(|| format!("Failed to execute command: {}", command))?;

    Ok(BashOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_bash_executes_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({"command": "echo hello"});

        let result = execute(&input, &ctx).unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("exit_code: 0"));
    }

    #[test]
    fn test_bash_captures_stderr() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({"command": "echo error >&2"});

        let result = execute(&input, &ctx).unwrap();
        assert!(result.contains("error"));
        assert!(result.contains("stderr"));
    }

    #[test]
    fn test_bash_captures_exit_code() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({"command": "exit 42"});

        let result = execute(&input, &ctx).unwrap();
        assert!(result.contains("exit_code: 42"));
    }

    #[test]
    fn test_bash_runs_in_root_directory() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("test.txt"), "content").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({"command": "ls"});

        let result = execute(&input, &ctx).unwrap();
        assert!(result.contains("test.txt"));
    }
}
