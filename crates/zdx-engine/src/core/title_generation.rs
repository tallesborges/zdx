//! Title generation from messages using LLM subagent.
//!
//! Provides shared title generation logic for thread/topic naming across zdx-tui and zdx-bot.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::config::ThinkingLevel;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};
use crate::prompts::THREAD_TITLE_PROMPT_TEMPLATE;

/// Model used when the configured title model fails.
const FALLBACK_TITLE_MODEL: &str = "openai-codex:gpt-5.6-luna";

/// Generate a title from a message using the LLM subagent.
///
/// Falls back to [`FALLBACK_TITLE_MODEL`] if the configured model fails.
///
/// Returns `Ok(sanitized_title)` or an error describing the failure.
///
/// # Errors
/// Returns an error if both the configured and fallback models fail, time out,
/// or produce an empty/invalid title.
pub async fn generate_title(
    message: &str,
    title_model: &str,
    root: &Path,
    parent_thread_id: Option<&str>,
) -> Result<String> {
    let primary = generate_with_model(message, title_model, root, parent_thread_id).await;

    let Err(err) = primary else {
        return primary;
    };

    if title_model == FALLBACK_TITLE_MODEL {
        return Err(err);
    }

    tracing::warn!(
        model = title_model,
        fallback = FALLBACK_TITLE_MODEL,
        error = %err,
        "title generation failed; retrying with fallback model"
    );

    generate_with_model(message, FALLBACK_TITLE_MODEL, root, parent_thread_id).await
}

async fn generate_with_model(
    message: &str,
    title_model: &str,
    root: &Path,
    parent_thread_id: Option<&str>,
) -> Result<String> {
    let prompt = THREAD_TITLE_PROMPT_TEMPLATE.replace("{{MESSAGE}}", message);

    let spec = crate::models::ModelSpec::parse(title_model);
    let options = ExecSubagentOptions {
        model: Some(spec.without_thinking()),
        system_prompt: None,
        thinking_level: Some(spec.thinking.unwrap_or(ThinkingLevel::Low)),
        no_tools: true,
        no_system_prompt: true,
        tools_override: None,
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: Some(Duration::from_mins(1)),
        activity_kind: Some("helper:title".to_string()),
        activity_parent_thread_id: parent_thread_id.map(str::to_string),
        thread_origin_kind: Some("helper:title".to_string()),
        ..Default::default()
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
