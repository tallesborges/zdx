//! Prompt-builder generation: turns a short user intent into a polished,
//! ready-to-use prompt via an isolated LLM subagent.
//!
//! Mirrors `handoff_generation` in shape; differs in that there is no thread
//! context to summarize — the intent itself is the only input. Shared by the
//! TUI `/prompt-builder` command and the Telegram bot.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, ensure};
use tokio_util::sync::CancellationToken;

use crate::config::ThinkingLevel;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent_with_cancel};
use crate::prompts::PROMPT_BUILDER_PROMPT_TEMPLATE;
use crate::zdx_context::build_zdx_context;

/// Timeout for prompt-builder generation subagent (2 minutes).
const PROMPT_BUILDER_TIMEOUT_SECS: u64 = 120;

/// Builds the prompt-builder generation prompt by substituting the intent.
fn build_prompt_builder_prompt(intent: &str, zdx_context: &str) -> String {
    PROMPT_BUILDER_PROMPT_TEMPLATE
        .replace("{{ZDX_CONTEXT}}", zdx_context)
        .replace("{{INTENT}}", intent)
}

/// Generates a polished, ready-to-use prompt from a short user `intent`.
///
/// # Errors
/// Returns an error when the intent is empty or the subagent fails / times
/// out / is cancelled.
pub async fn generate_prompt_builder(
    intent: &str,
    model: Option<String>,
    root: &Path,
    cancel: Option<CancellationToken>,
) -> Result<String> {
    let trimmed = intent.trim();
    ensure!(!trimmed.is_empty(), "Prompt-builder intent cannot be empty");

    let generation_prompt = build_prompt_builder_prompt(trimmed, &build_zdx_context(root));

    let options = ExecSubagentOptions {
        model,
        system_prompt: None,
        thinking_level: Some(ThinkingLevel::Low),
        no_tools: true,
        no_system_prompt: true,
        tools_override: None,
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: Some(Duration::from_secs(PROMPT_BUILDER_TIMEOUT_SECS)),
        thread_origin_kind: Some("helper:prompt_builder".to_string()),
        ..Default::default()
    };

    run_exec_subagent_with_cancel(root, &generation_prompt, &options, cancel, None).await
}

#[cfg(test)]
mod tests {
    use super::build_prompt_builder_prompt;

    #[test]
    fn substitutes_intent_placeholder_verbatim() {
        let intent = "make me a bug investigation loop with Oracle";
        let prompt = build_prompt_builder_prompt(intent, "");
        assert!(prompt.contains(intent));
        assert!(!prompt.contains("{{INTENT}}"));
        assert!(!prompt.contains("{{ZDX_CONTEXT}}"));
    }

    #[test]
    fn template_keeps_role_framing() {
        let prompt = build_prompt_builder_prompt("anything", "");
        // Sanity-check that the asset was loaded and the framing survives
        // substitution. If the asset is missing or accidentally rewritten
        // these markers will trip first.
        assert!(prompt.contains("prompt construction tool"));
        assert!(prompt.contains("<intent>"));
    }

    #[test]
    fn template_teaches_zdx_house_style() {
        let prompt = build_prompt_builder_prompt("anything", "");
        // The template must teach the iterative ZDX-style blocks (Rules,
        // loop arrow, termination contract, deliverables) and the subagent
        // vocabulary. If any of these get accidentally stripped the prompt
        // builder will collapse back to a generic prompt and lose its house
        // style — which is exactly what this test guards against.
        assert!(prompt.contains("Rules:"));
        assert!(prompt.contains("Repeat until:"));
        assert!(prompt.contains("Loop arrow"));
        assert!(prompt.contains("→"));
        assert!(prompt.contains("At the end, give me:"));
        assert!(prompt.contains("Oracle"));
        assert!(prompt.contains("Explorer"));
    }

    #[test]
    fn template_teaches_todo_write_progress_tracking() {
        let prompt = build_prompt_builder_prompt("anything", "");
        // The template must teach the future assistant to plan and track
        // progress with `Todo_Write` for multi-step or multi-phase prompts.
        // If these markers disappear the generated prompts will stop
        // directing the receiving agent to track work, which defeats the
        // ZDX house style for iterative workflows.
        assert!(prompt.contains("Todo_Write"));
        assert!(prompt.contains("in_progress"));
    }

    #[test]
    fn substitutes_zdx_context_placeholder() {
        let prompt = build_prompt_builder_prompt("anything", "MY_CTX_MARKER");
        assert!(prompt.contains("MY_CTX_MARKER"));
        assert!(!prompt.contains("{{ZDX_CONTEXT}}"));
    }
}
