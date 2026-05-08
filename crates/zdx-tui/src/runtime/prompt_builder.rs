//! Prompt-builder generation handlers.
//!
//! Spawns an isolated subagent that turns a short user intent into a polished
//! prompt for the `/prompt-builder` slash command. Mirrors `runtime/handoff.rs`
//! in shape; differs in that there is no thread context to summarize — the
//! intent itself is the only input.
//!
//! Uses `CancellationToken` for the unified cancellation model.

use std::path::PathBuf;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use zdx_engine::core::subagent::{ExecSubagentOptions, run_exec_subagent_with_cancel};
use zdx_engine::prompts::PROMPT_BUILDER_PROMPT_TEMPLATE;

use crate::events::UiEvent;

/// Timeout for prompt-builder generation subagent (2 minutes).
const PROMPT_BUILDER_TIMEOUT_SECS: u64 = 120;

/// Builds the prompt-builder generation prompt by substituting the intent.
fn build_prompt_builder_prompt(intent: &str) -> String {
    PROMPT_BUILDER_PROMPT_TEMPLATE.replace("{{INTENT}}", intent)
}

/// Runs the subagent process with timeout and cancellation support.
///
/// Pure async function — returns the generated prompt or error.
async fn run_subagent(
    cancel: CancellationToken,
    model: Option<String>,
    generation_prompt: String,
    root: PathBuf,
) -> Result<String, String> {
    let options = ExecSubagentOptions {
        model,
        system_prompt: None,
        thinking_level: Some(zdx_engine::config::ThinkingLevel::Minimal),
        no_tools: true,
        no_system_prompt: true,
        tools_override: None,
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: Some(Duration::from_secs(PROMPT_BUILDER_TIMEOUT_SECS)),
        ..Default::default()
    };

    run_exec_subagent_with_cancel(&root, &generation_prompt, &options, Some(cancel))
        .await
        .map_err(|err| format!("{err:#}"))
}

/// Runs prompt-builder generation with cancellation support.
///
/// Returns `UiEvent::PromptBuilderResult`; cancellation is cooperative via the
/// supplied token.
pub async fn prompt_builder_generation(
    intent: String,
    model: Option<String>,
    root: PathBuf,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let cancel = cancel.unwrap_or_default();

    let trimmed = intent.trim();
    if trimmed.is_empty() {
        return UiEvent::PromptBuilderResult {
            intent,
            result: Err("Prompt-builder intent cannot be empty.".to_string()),
        };
    }

    let generation_prompt = build_prompt_builder_prompt(trimmed);
    let result = run_subagent(cancel, model, generation_prompt, root).await;
    UiEvent::PromptBuilderResult { intent, result }
}

#[cfg(test)]
mod tests {
    use super::build_prompt_builder_prompt;

    #[test]
    fn substitutes_intent_placeholder_verbatim() {
        let intent = "make me a bug investigation loop with Oracle";
        let prompt = build_prompt_builder_prompt(intent);
        assert!(prompt.contains(intent));
        assert!(!prompt.contains("{{INTENT}}"));
    }

    #[test]
    fn template_keeps_role_framing() {
        let prompt = build_prompt_builder_prompt("anything");
        // Sanity-check that the asset was loaded and the framing survives
        // substitution. If the asset is missing or accidentally rewritten
        // these markers will trip first.
        assert!(prompt.contains("prompt construction tool"));
        assert!(prompt.contains("<intent>"));
    }

    #[test]
    fn template_teaches_zdx_house_style() {
        let prompt = build_prompt_builder_prompt("anything");
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
}
