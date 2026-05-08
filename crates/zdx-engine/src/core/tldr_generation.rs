//! TLDR/recap generation for an existing thread using an LLM subagent.
//!
//! Loads the thread's persisted events, formats them as a transcript, and
//! asks a cheap subagent (configured via `Config::tldr_model`) to summarize
//! the user's most recent activity.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};

use crate::config::ThinkingLevel;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};
use crate::core::thread_persistence as tp;
use crate::prompts::THREAD_TLDR_PROMPT_TEMPLATE;

/// Generate a TLDR/recap of the given thread using the configured TLDR model.
///
/// Returns the model's plain markdown summary on success.
///
/// # Errors
/// Returns an error when the thread cannot be loaded, has no events, or the
/// subagent fails / times out / returns an empty response.
pub async fn generate_tldr(thread_id: &str, tldr_model: &str, root: &Path) -> Result<String> {
    let events = tp::load_thread_events(thread_id)
        .with_context(|| format!("load thread '{thread_id}' for TLDR"))?;
    ensure!(!events.is_empty(), "Thread has no events to summarize");

    let transcript = tp::format_transcript(&events);
    let trimmed = transcript.trim();
    ensure!(!trimmed.is_empty(), "Thread transcript is empty");

    let prompt = THREAD_TLDR_PROMPT_TEMPLATE.replace("{{TRANSCRIPT}}", trimmed);

    let options = ExecSubagentOptions {
        model: Some(tldr_model.to_string()),
        system_prompt: None,
        thinking_level: Some(ThinkingLevel::Minimal),
        no_tools: true,
        no_system_prompt: true,
        tools_override: None,
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: Some(Duration::from_mins(1)),
    };

    let raw = run_exec_subagent(root, &prompt, &options).await?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Empty TLDR generated"));
    }
    Ok(trimmed.to_string())
}
