//! Bash tool for executing shell commands.
//!
//! Allows the agent to run shell commands with safety guards.
//! Requires `--allow-bash` flag or the tool returns "denied".

use std::fs::File;
use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

/// Maximum bytes per output stream (stdout/stderr) before truncation.
const MAX_OUTPUT_BYTES: usize = 40 * 1024; // 40KB

/// Writes full output to a temp file and returns the file path.
///
/// Used when output is truncated so the AI can use the Read tool to access
/// the complete data with offset/limit parameters.
fn write_temp_file(bytes: &[u8], stream_name: &str) -> Option<String> {
    let temp_dir = std::env::temp_dir();
    let filename = format!("zdx-bash-{}-{}.txt", Uuid::new_v4(), stream_name);
    let path = temp_dir.join(filename);

    let mut file = File::create(&path).ok()?;
    file.write_all(bytes).ok()?;

    Some(path.to_string_lossy().into_owned())
}

/// Truncates a byte slice at a valid UTF-8 character boundary.
///
/// Returns the truncated string and whether truncation occurred.
fn truncate_at_utf8_boundary(bytes: &[u8], max_bytes: usize) -> (String, bool, usize) {
    let total_bytes = bytes.len();

    if total_bytes <= max_bytes {
        // No truncation needed
        return (
            String::from_utf8_lossy(bytes).into_owned(),
            false,
            total_bytes,
        );
    }

    // Find the last valid UTF-8 boundary at or before max_bytes
    let truncated_bytes = &bytes[..max_bytes];

    // Walk backwards to find a valid UTF-8 boundary
    // UTF-8 continuation bytes start with 10xxxxxx (0x80-0xBF)
    let mut end = max_bytes;
    while end > 0 && (truncated_bytes[end - 1] & 0xC0) == 0x80 {
        end -= 1;
    }

    // If we hit a multi-byte sequence start, back up one more
    // to avoid cutting in the middle of a character
    if end > 0 && truncated_bytes[end - 1] >= 0x80 {
        // Check if this is a valid start of a multi-byte sequence
        let byte = truncated_bytes[end - 1];
        let char_len = if byte >= 0xF0 {
            4
        } else if byte >= 0xE0 {
            3
        } else if byte >= 0xC0 {
            2
        } else {
            1
        };

        // If the sequence would extend beyond our truncation point, remove it
        if end - 1 + char_len > max_bytes {
            end -= 1;
        }
    }

    let truncated = String::from_utf8_lossy(&bytes[..end]).into_owned();
    (truncated, true, total_bytes)
}

/// Returns the tool definition for the bash tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Bash".to_string(),
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
            "required": ["command"],
            "additionalProperties": false
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
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_total_bytes: usize,
    pub stderr_total_bytes: usize,
    /// Path to temp file containing full stdout (when truncated).
    pub stdout_file: Option<String>,
    /// Path to temp file containing full stderr (when truncated).
    pub stderr_file: Option<String>,
}

impl BashOutput {
    /// Converts to structured envelope format.
    pub fn into_tool_output(self) -> ToolOutput {
        let mut data = json!({
            "stdout": self.stdout,
            "stderr": self.stderr,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "stdout_truncated": self.stdout_truncated,
            "stderr_truncated": self.stderr_truncated,
            "stdout_total_bytes": self.stdout_total_bytes,
            "stderr_total_bytes": self.stderr_total_bytes
        });

        // Add file paths when truncated (for AI to use Read tool)
        if let Some(path) = self.stdout_file {
            data["stdout_file"] = json!(path);
        }
        if let Some(path) = self.stderr_file {
            data["stderr_file"] = json!(path);
        }

        ToolOutput::success(data)
    }
}

/// Executes the bash tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext, timeout: Option<Duration>) -> ToolOutput {
    let input: BashInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Invalid input for bash tool: {e}"),
                None,
            );
        }
    };

    if input.command.trim().is_empty() {
        return ToolOutput::failure("invalid_input", "command cannot be empty", None);
    }

    match run_command(&input.command, ctx, timeout).await {
        Ok(output) => output.into_tool_output(),
        Err(e) => e,
    }
}

/// Executes a bash command directly (convenience wrapper).
///
/// This is a simpler API that takes the command string directly,
/// useful for direct user invocation (e.g., `$` shortcut).
pub async fn run(command: &str, ctx: &ToolContext, timeout: Option<Duration>) -> ToolOutput {
    if command.trim().is_empty() {
        return ToolOutput::failure("invalid_input", "command cannot be empty", None);
    }

    match run_command(command, ctx, timeout).await {
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
        // Signal to programs that we are a non-interactive, dumb terminal.
        // This suppresses ANSI escape sequences, color output, and progress bars
        // in most well-behaved CLI tools (e.g. gcloud, npm, pip).
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            ToolOutput::failure(
                "spawn_error",
                format!("Failed to execute command '{command}'"),
                Some(format!("Error: {e}")),
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
                    stdout_truncated: false,
                    stderr_truncated: false,
                    stdout_total_bytes: 0,
                    stderr_total_bytes: 0,
                    stdout_file: None,
                    stderr_file: None,
                });
            }
        },
        None => output_fut.await,
    }
    .map_err(|e| {
        ToolOutput::failure(
            "exec_error",
            format!("Failed to execute command '{command}'"),
            Some(format!("Error: {e}")),
        )
    })?;

    let (stdout, stdout_truncated, stdout_total_bytes) =
        truncate_at_utf8_boundary(&output.stdout, MAX_OUTPUT_BYTES);
    let (stderr, stderr_truncated, stderr_total_bytes) =
        truncate_at_utf8_boundary(&output.stderr, MAX_OUTPUT_BYTES);

    // Write full output to temp files when truncated
    let stdout_file = if stdout_truncated {
        write_temp_file(&output.stdout, "stdout")
    } else {
        None
    };
    let stderr_file = if stderr_truncated {
        write_temp_file(&output.stderr, "stderr")
    } else {
        None
    };

    Ok(BashOutput {
        stdout,
        stderr,
        exit_code: output.status.code().unwrap_or(-1),
        timed_out: false,
        stdout_truncated,
        stderr_truncated,
        stdout_total_bytes,
        stderr_total_bytes,
        stdout_file,
        stderr_file,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn test_bash_executes_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "echo hello"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert!(data["stdout"].as_str().unwrap().contains("hello"));
        assert_eq!(data["exit_code"], 0);
        assert_eq!(data["timed_out"], false);
        assert_eq!(data["stdout_truncated"], false);
        assert_eq!(data["stderr_truncated"], false);
    }

    #[tokio::test]
    async fn test_bash_captures_stderr() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "echo error >&2"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert!(data["stderr"].as_str().unwrap().contains("error"));
    }

    #[tokio::test]
    async fn test_bash_captures_exit_code() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "exit 42"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["exit_code"], 42);
    }

    #[tokio::test]
    async fn test_bash_runs_in_root_directory() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("test.txt"), "content").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "ls"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert!(data["stdout"].as_str().unwrap().contains("test.txt"));
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "sleep 5"});

        let result = execute(&input, &ctx, Some(Duration::from_millis(100))).await;
        assert!(result.is_ok()); // timed_out is success with timed_out=true
        let data = result.data().expect("should have data");
        assert_eq!(data["timed_out"], true);
        assert_eq!(data["stdout_truncated"], false);
        assert_eq!(data["stderr_truncated"], false);
    }

    #[tokio::test]
    async fn test_bash_invalid_input() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"wrong_field": "ls"});

        let result = execute(&input, &ctx, None).await;
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    #[tokio::test]
    async fn test_bash_rejects_empty_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"command": "   "});

        let result = execute(&input, &ctx, None).await;
        assert!(!result.is_ok());
        let payload = serde_json::to_value(result).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "command cannot be empty");
    }

    #[tokio::test]
    async fn test_bash_run_rejects_empty_command() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);

        let result = run("", &ctx, None).await;
        assert!(!result.is_ok());
        let payload = serde_json::to_value(result).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "command cannot be empty");
    }

    #[test]
    fn test_truncate_at_utf8_boundary_no_truncation() {
        let input = "Hello, world!".as_bytes();
        let (result, truncated, total) = truncate_at_utf8_boundary(input, 100);
        assert_eq!(result, "Hello, world!");
        assert!(!truncated);
        assert_eq!(total, 13);
    }

    #[test]
    fn test_truncate_at_utf8_boundary_multibyte() {
        // "こんにちは" - each character is 3 bytes in UTF-8
        let input = "こんにちは".as_bytes();
        assert_eq!(input.len(), 15); // 5 chars * 3 bytes

        // Truncate at 10 bytes - should keep 3 full characters (9 bytes)
        let (result, truncated, total) = truncate_at_utf8_boundary(input, 10);
        assert_eq!(result, "こんに");
        assert!(truncated);
        assert_eq!(total, 15);
    }

    #[test]
    fn test_truncate_at_utf8_boundary_emoji() {
        // Emoji "😀" is 4 bytes in UTF-8
        let input = "Hi😀there".as_bytes();
        // "Hi" = 2 bytes, "😀" = 4 bytes, "there" = 5 bytes = 11 total

        // Truncate at 5 bytes - should keep "Hi" (2 bytes), skip partial emoji
        let (result, truncated, total) = truncate_at_utf8_boundary(input, 5);
        assert_eq!(result, "Hi");
        assert!(truncated);
        assert_eq!(total, 11);
    }

    #[tokio::test]
    async fn test_bash_stdout_truncated_writes_temp_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Generate more than 40KB of output (50KB of 'x' characters)
        let input = json!({"command": "head -c 51200 /dev/zero | tr '\\0' 'x'"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");

        // Should have a stdout_file path
        let stdout_file = data["stdout_file"]
            .as_str()
            .expect("should have stdout_file");
        assert!(stdout_file.contains("zdx-bash-"));
        assert!(stdout_file.contains("-stdout.txt"));

        // File should exist and contain full output
        let file_contents = std::fs::read_to_string(stdout_file).expect("should read temp file");
        assert_eq!(file_contents.len(), 51200);
        assert!(file_contents.chars().all(|c| c == 'x'));

        // Clean up
        let _ = std::fs::remove_file(stdout_file);
    }

    #[tokio::test]
    async fn test_bash_stderr_truncated_writes_temp_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Generate more than 40KB of stderr output (50KB)
        let input = json!({"command": "head -c 51200 /dev/zero | tr '\\0' 'y' >&2"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");

        // Should have a stderr_file path
        let stderr_file = data["stderr_file"]
            .as_str()
            .expect("should have stderr_file");
        assert!(stderr_file.contains("zdx-bash-"));
        assert!(stderr_file.contains("-stderr.txt"));

        // File should exist and contain full output
        let file_contents = std::fs::read_to_string(stderr_file).expect("should read temp file");
        assert_eq!(file_contents.len(), 51200);
        assert!(file_contents.chars().all(|c| c == 'y'));

        // Clean up
        let _ = std::fs::remove_file(stderr_file);
    }

    #[tokio::test]
    async fn test_bash_no_truncation_no_temp_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Generate less than 40KB of output (1KB)
        let input = json!({"command": "head -c 1024 /dev/zero | tr '\\0' 'z'"});

        let result = execute(&input, &ctx, None).await;
        assert!(result.is_ok());
        let data = result.data().expect("should have data");

        // Should NOT have stdout_file or stderr_file
        assert!(data.get("stdout_file").is_none());
        assert!(data.get("stderr_file").is_none());
    }

    #[test]
    fn test_write_temp_file() {
        let content = b"Hello, temp file!";
        let path = write_temp_file(content, "test").expect("should write temp file");

        assert!(path.contains("zdx-bash-"));
        assert!(path.contains("-test.txt"));

        // Verify file contents
        let read_content = std::fs::read_to_string(&path).expect("should read temp file");
        assert_eq!(read_content.as_bytes(), content);

        // Clean up
        let _ = std::fs::remove_file(&path);
    }
}
