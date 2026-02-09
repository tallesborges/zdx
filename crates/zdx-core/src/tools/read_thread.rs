//! Read thread tool.
//!
//! Answers a goal based on a saved thread transcript.

use std::process::Stdio;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::thread_persistence as thread_log;
use crate::prompts::READ_THREAD_PROMPT_TEMPLATE;

const READ_THREAD_MODEL: &str = "gemini-cli:gemini-2.5-flash-lite";

/// Returns the tool definition for the read thread tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Read_Thread".to_string(),
        description: "Answer a goal using a saved thread transcript. Returns response text only."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "ID of the thread to query"
                },
                "goal": {
                    "type": "string",
                    "description": "A clear description of what information you need from the thread. Be specific about what to extract."
                }
            },
            "required": ["thread_id", "goal"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct ReadThreadInput {
    thread_id: String,
    goal: String,
}

/// Executes the read thread tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: ReadThreadInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for read_thread tool",
                Some(format!("Parse error: {}", e)),
            );
        }
    };

    let thread_id = input.thread_id.trim().to_string();
    if thread_id.is_empty() {
        return ToolOutput::failure("invalid_input", "thread_id cannot be empty", None);
    }

    let goal = input.goal.trim().to_string();
    if goal.is_empty() {
        return ToolOutput::failure("invalid_input", "goal cannot be empty", None);
    }

    let events = match thread_log::load_thread_events(&thread_id) {
        Ok(events) => events,
        Err(e) => {
            return ToolOutput::failure(
                "thread_not_found",
                format!("Thread '{}' not found", thread_id),
                Some(format!("Load error: {}", e)),
            );
        }
    };

    if events.is_empty() {
        return ToolOutput::failure(
            "thread_not_found",
            format!("Thread '{}' not found", thread_id),
            None,
        );
    }

    let thread_content = thread_log::format_transcript(&events);
    let prompt = build_read_thread_prompt(&thread_content, &goal);

    match run_subagent(prompt, ctx).await {
        Ok(response) => ToolOutput::success(Value::String(response)),
        Err(err) => ToolOutput::failure("execution_failed", "Read thread failed", Some(err)),
    }
}

fn build_read_thread_prompt(thread_content: &str, goal: &str) -> String {
    READ_THREAD_PROMPT_TEMPLATE
        .replace("{{THREAD_CONTENT}}", thread_content)
        .replace("{{GOAL}}", goal)
}

async fn run_subagent(prompt: String, ctx: &ToolContext) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Failed to get executable: {}", e))?;

    let mut command = Command::new(exe);
    command
        .arg("--root")
        .arg(&ctx.root)
        .args(["--no-thread", "exec", "-p", &prompt, "--no-tools"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    command.args(["-m", READ_THREAD_MODEL]);

    if let Some(level) = ctx.thinking_level {
        command.args(["-t", level.display_name()]);
    }

    let child = command
        .spawn()
        .map_err(|e| format!("Failed to spawn subagent: {}", e))?;

    let output = if let Some(timeout) = ctx.timeout {
        tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| format!("Read thread timed out after {} seconds", timeout.as_secs()))
            .and_then(|result| {
                result.map_err(|e| format!("Failed to get subagent output: {}", e))
            })?
    } else {
        child
            .wait_with_output()
            .await
            .map_err(|e| format!("Failed to get subagent output: {}", e))?
    };

    process_subagent_output(output)
}

fn process_subagent_output(output: std::process::Output) -> Result<String, String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Read thread failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("Read thread returned empty output".to_string());
    }

    Ok(stdout)
}
