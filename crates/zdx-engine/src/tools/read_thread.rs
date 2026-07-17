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
use crate::zdx_context::build_zdx_context;

/// Returns the tool definition for the read thread tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Read_Thread".to_string(),
        description: "Answer a goal using a saved thread transcript. If the caller already provides a thread_id (for example the user pasted one), use it directly — do not call Thread_Search first. Only fall back to Thread_Search when the thread_id is unknown; never guess or invent one. Provide a specific `goal` describing what to extract; vague goals return vague answers. Best for historical thread context, prior decisions, or past outputs rather than current filesystem state. Returns response text only."
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
    let prompt = build_read_thread_prompt(&thread_content, &goal, &build_zdx_context(&ctx.root));
    let parent = tp::extract_handoff_from_from_events(&events);

    match run_subagent(prompt, ctx).await {
        Ok(response) => {
            let output = match &parent {
                Some(parent_id) => format!("{response}\n\nParent handoff thread: {parent_id}"),
                None => response,
            };
            ToolOutput::success(Value::String(output))
        }
        Err(err) => ToolOutput::failure("execution_failed", "Read thread failed", Some(err)),
    }
}

fn build_read_thread_prompt(thread_content: &str, goal: &str, zdx_context: &str) -> String {
    READ_THREAD_PROMPT_TEMPLATE
        .replace("{{ZDX_CONTEXT}}", zdx_context)
        .replace("{{THREAD_CONTENT}}", thread_content)
        .replace("{{GOAL}}", goal)
}

async fn run_subagent(prompt: String, ctx: &ToolContext) -> Result<String, String> {
    let model_spec = ctx
        .read_thread_model
        .clone()
        .unwrap_or_else(|| "gemini:gemini-3.1-flash-lite-preview".to_string());
    let (model, thinking) = crate::models::split_model_thinking(&model_spec);
    let options = ExecSubagentOptions {
        model: Some(model.to_string()),
        system_prompt: None,
        // An explicit `@thinking` suffix wins; otherwise inherit the caller's level.
        thinking_level: thinking.or(ctx.thinking_level),
        no_tools: true,
        no_system_prompt: true,
        tools_override: None,
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: ctx.timeout,
        thread_origin_kind: Some("helper:read_thread".to_string()),
        thread_parent_id: ctx.current_thread_id.clone(),
        ..Default::default()
    };

    run_exec_subagent(&ctx.root, &prompt, &options)
        .await
        .map_err(|err| format!("Read thread failed: {err}"))
}
