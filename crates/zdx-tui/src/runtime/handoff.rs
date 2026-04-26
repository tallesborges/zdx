//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from thread context.
//!
//! Uses `CancellationToken` for unified cancellation model.

use std::path::PathBuf;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use zdx_engine::core::subagent::{ExecSubagentOptions, run_exec_subagent_with_cancel};
use zdx_engine::core::thread_persistence;
use zdx_engine::prompts::HANDOFF_PROMPT_TEMPLATE;

use crate::events::UiEvent;

/// Timeout for handoff generation subagent (2 minutes).
const HANDOFF_TIMEOUT_SECS: u64 = 120;

/// Prefix shown at the beginning of generated handoff output.
///
/// Surfaces the user's literal goal verbatim so the new chat sees it
/// directly rather than relying on the LLM to preserve it through summarization.
fn build_handoff_prefix(thread_id: &str, goal: &str) -> String {
    let trimmed = goal.trim();
    if trimmed.is_empty() {
        format!(
            "Continuing work from thread {thread_id}. If you need specific information, use read_thread to get it.\n\nContext from the previous thread:"
        )
    } else {
        format!(
            "Continuing work from thread {thread_id}.\n\nMy goal: {trimmed}\n\nContext from the previous thread (use read_thread if you need more):"
        )
    }
}

/// Builds the prompt for handoff generation.
fn build_handoff_prompt(thread_content: &str, goal: &str) -> String {
    HANDOFF_PROMPT_TEMPLATE
        .replace("{{THREAD_CONTENT}}", thread_content)
        .replace("{{GOAL}}", goal)
}

/// Loads and validates thread content for handoff.
fn load_thread_content(thread_id: &str) -> Result<String, String> {
    let events = thread_persistence::load_thread_events(thread_id)
        .map_err(|e| format!("Could not load thread: {e}"))?;

    if events.is_empty() {
        return Err(format!("Thread '{thread_id}' is empty"));
    }

    Ok(thread_persistence::format_transcript(&events))
}

/// Runs the subagent process with timeout and cancellation support.
///
/// Pure async function - returns the generated prompt or error.
/// Uses `CancellationToken` for unified cancellation.
async fn run_subagent(
    cancel: CancellationToken,
    handoff_model: String,
    generation_prompt: String,
    root: PathBuf,
) -> Result<String, String> {
    let options = ExecSubagentOptions {
        model: Some(handoff_model),
        system_prompt: None,
        thinking_level: Some(zdx_engine::config::ThinkingLevel::Minimal),
        no_tools: true,
        no_system_prompt: true,
        tools_override: None,
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: Some(Duration::from_secs(HANDOFF_TIMEOUT_SECS)),
    };

    run_exec_subagent_with_cancel(&root, &generation_prompt, &options, Some(cancel))
        .await
        .map_err(|err| format!("{err:#}"))
}

/// Runs handoff generation with cancellation support.
///
/// Returns `HandoffResult`; cancellation is cooperative via token.
pub async fn handoff_generation(
    thread_id: String,
    goal: String,
    handoff_model: String,
    root: PathBuf,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let cancel = cancel.unwrap_or_default();

    // Load thread content synchronously (it's quick I/O)
    let thread_content = load_thread_content(&thread_id);

    let content = match thread_content {
        Ok(content) => content,
        Err(e) => {
            return UiEvent::HandoffResult {
                goal,
                result: Err(e),
            };
        }
    };

    let generation_prompt = build_handoff_prompt(&content, &goal);
    let handoff_prefix = build_handoff_prefix(&thread_id, &goal);
    let result = run_subagent(cancel, handoff_model, generation_prompt, root)
        .await
        .map(|generated_prompt| format!("{handoff_prefix}\n\n{generated_prompt}"));
    UiEvent::HandoffResult { goal, result }
}

#[cfg(test)]
mod tests {
    use super::build_handoff_prefix;

    #[test]
    fn handoff_prefix_mentions_thread_and_read_thread_tool() {
        let prefix = build_handoff_prefix("thread-123", "ship the new feature");
        assert!(prefix.contains("thread-123"));
        assert!(prefix.contains("read_thread"));
    }

    #[test]
    fn handoff_prefix_includes_goal_verbatim() {
        let goal = "refactor the handoff prompt to drop length guidance";
        let prefix = build_handoff_prefix("thread-xyz", goal);
        assert!(prefix.contains(goal));
        assert!(prefix.contains("My goal:"));
    }

    #[test]
    fn handoff_prefix_handles_empty_goal() {
        let prefix = build_handoff_prefix("thread-abc", "   ");
        assert!(prefix.contains("thread-abc"));
        assert!(prefix.contains("read_thread"));
        assert!(!prefix.contains("My goal:"));
    }
}
