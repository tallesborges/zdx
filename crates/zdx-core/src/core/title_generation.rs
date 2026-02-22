//! Title generation from messages using LLM subagent.
//!
//! Provides shared title generation logic for thread/topic naming across zdx-tui and zdx-bot.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::config::ThinkingLevel;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};
use crate::prompts::THREAD_TITLE_PROMPT_TEMPLATE;

/// Generate a title from a message using the LLM subagent.
///
/// Returns `Ok(sanitized_title)` or an error describing the failure.
///
/// # Errors
/// Returns an error if the subagent fails, times out, or produces an empty/invalid title.
pub async fn generate_title(message: &str, title_model: &str, root: &Path) -> Result<String> {
    let prompt = THREAD_TITLE_PROMPT_TEMPLATE.replace("{{MESSAGE}}", message);

    let options = ExecSubagentOptions {
        model: Some(title_model.to_string()),
        thinking_level: Some(ThinkingLevel::Minimal),
        no_tools: true,
        timeout: Some(Duration::from_secs(60)),
    };

    let raw_output = run_exec_subagent(root, &prompt, &options).await?;

    sanitize_title(&raw_output)
}

fn sanitize_title(raw: &str) -> Result<String> {
    let mut line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| anyhow!("Empty title generated"))?
        .trim()
        .to_string();

    for prefix in ["title:", "Title:"] {
        if let Some(rest) = line.strip_prefix(prefix) {
            line = rest.trim().to_string();
            break;
        }
    }

    let trimmed = line
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string();

    if trimmed.is_empty() {
        Err(anyhow!("Title is empty after sanitization"))
    } else {
        Ok(trimmed)
    }
}
