//! Read thread tool.
//!
//! Answers a goal based on a saved thread transcript.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};
use crate::core::thread_persistence as tp;
use crate::prompts::READ_THREAD_PROMPT_TEMPLATE;

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
                Some(format!("Parse error: {e}")),
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

    let events = match tp::load_thread_events(&thread_id) {
        Ok(events) => events,
        Err(e) => {
            return ToolOutput::failure(
                "thread_not_found",
                format!("Thread '{thread_id}' not found"),
                Some(format!("Load error: {e}")),
            );
        }
    };

    if events.is_empty() {
        return ToolOutput::failure(
            "thread_not_found",
            format!("Thread '{thread_id}' not found"),
            None,
        );
    }

    let thread_content = tp::format_transcript(&events);
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
    let options = ExecSubagentOptions {
        model: Some(
            ctx.read_thread_model
                .clone()
                .unwrap_or_else(|| "gemini:gemini-2.5-flash-lite".to_string()),
        ),
        thinking_level: ctx.thinking_level,
        no_tools: true,
        timeout: ctx.timeout,
    };

    run_exec_subagent(&ctx.root, &prompt, &options)
        .await
        .map_err(|err| format!("Read thread failed: {err}"))
}
