//! Agent module for handling prompt execution with tool support.
//!
//! This module provides backward-compatible wrappers around the engine.
//! New code should use the engine module directly.

use std::path::PathBuf;

use anyhow::Result;

use crate::config::Config;
use crate::engine::{self, EngineOptions};
use crate::providers::anthropic::ChatMessage;
use crate::renderer::CliRenderer;
use crate::session::{Session, SessionEvent};

/// Options for agent execution.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
        }
    }
}

impl From<&AgentOptions> for EngineOptions {
    fn from(opts: &AgentOptions) -> Self {
        EngineOptions {
            root: opts.root.clone(),
        }
    }
}

/// Sends a prompt to the LLM and streams text response to stdout.
///
/// If a session is provided, logs the user prompt and final assistant response.
/// Implements tool loop - if the model requests tools, executes them and continues.
/// Returns the complete response text.
///
/// This is a backward-compatible wrapper that uses the engine internally.
pub async fn execute_prompt_streaming(
    prompt: &str,
    config: &Config,
    session: Option<&Session>,
    options: &AgentOptions,
) -> Result<String> {
    let system_prompt = crate::context::build_effective_system_prompt(config, &options.root)?;

    // Log user message to session
    if let Some(s) = session {
        s.append(&SessionEvent::user_message(prompt))?;
    }

    let messages = vec![ChatMessage::user(prompt)];
    let engine_opts = EngineOptions::from(options);

    // Create a CLI renderer and its event sink
    let renderer = CliRenderer::new();
    let (sink, handle) = renderer.into_sink();

    let (final_text, _messages) = engine::run_turn(
        messages,
        config,
        &engine_opts,
        system_prompt.as_deref(),
        sink,
    )
    .await?;

    // Print final newline after streaming completes
    handle.finish();

    // Log assistant response to session
    if let Some(s) = session {
        s.append(&SessionEvent::assistant_message(&final_text))?;
    }

    Ok(final_text)
}
