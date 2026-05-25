//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff context from thread history.
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
/// The user's literal next-chat message leads (so the new assistant sees the
/// user's own words first, exactly as typed), followed by a short parenthetical
/// pointing at the source thread. The LLM-generated context block is appended
/// after this prefix.
fn build_handoff_prefix(thread_id: &str, next_message: &str) -> String {
    let trimmed = next_message.trim();
    if trimmed.is_empty() {
        format!("(Continuing from thread {thread_id} — call read_thread for full context.)")
    } else {
        format!(
            "{trimmed}\n\n(Continuing from thread {thread_id} — call read_thread for anything below that's missing.)"
        )
    }
}

/// Builds the prompt for handoff generation.
fn build_handoff_prompt(thread_content: &str, next_message: &str) -> String {
    HANDOFF_PROMPT_TEMPLATE
        .replace("{{THREAD_CONTENT}}", thread_content)
        .replace("{{NEXT_MESSAGE}}", next_message)
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
        ..Default::default()
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
    next_message: String,
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
                next_message,
                result: Err(e),
            };
        }
    };

    let generation_prompt = build_handoff_prompt(&content, &next_message);
    let handoff_prefix = build_handoff_prefix(&thread_id, &next_message);
    let result = run_subagent(cancel, handoff_model, generation_prompt, root)
        .await
        .map(|generated_prompt| format!("{handoff_prefix}\n\n{generated_prompt}"));
    UiEvent::HandoffResult {
        next_message,
        result,
    }
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
    fn handoff_prefix_leads_with_next_message_verbatim() {
        let msg = "now lets streamline the comments";
        let prefix = build_handoff_prefix("thread-xyz", msg);
        assert!(prefix.starts_with(msg), "user message must lead the prefix");
        assert!(
            !prefix.contains("My goal:"),
            "prefix must not relabel the user's message as a goal"
        );
    }

    #[test]
    fn handoff_prefix_handles_empty_next_message() {
        let prefix = build_handoff_prefix("thread-abc", "   ");
        assert!(prefix.contains("thread-abc"));
        assert!(prefix.contains("read_thread"));
        // Empty case has no leading user-text section, just the parenthetical.
        assert!(prefix.starts_with('('));
    }
}
