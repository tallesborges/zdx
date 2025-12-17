//! Agent module for handling prompt execution with tool support.
//!
//! This module provides backward-compatible wrappers around the engine.
//! New code should use the engine module directly.

use std::io::{Write, stderr, stdout};
use std::path::PathBuf;

use anyhow::Result;

use crate::config::Config;
use crate::engine::{self, EngineOptions, EventSink};
use crate::events::EngineEvent;
use crate::providers::anthropic::ChatMessage;
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

    // Create a sink that renders to stdout/stderr (backward-compatible behavior)
    let sink = create_cli_sink();

    let (final_text, _messages) = engine::run_turn(
        messages,
        config,
        &engine_opts,
        system_prompt.as_deref(),
        sink,
    )
    .await?;

    // Final newline after streaming completes
    if !final_text.is_empty() {
        println!();
    }

    // Log assistant response to session
    if let Some(s) = session {
        s.append(&SessionEvent::assistant_message(&final_text))?;
    }

    Ok(final_text)
}

/// Creates a CLI sink that renders events to stdout/stderr.
/// This maintains backward-compatible behavior during the migration.
fn create_cli_sink() -> EventSink {
    let mut stdout = stdout();
    let mut stderr = stderr();

    Box::new(move |event| {
        match event {
            EngineEvent::AssistantDelta { text } => {
                let _ = write!(stdout, "{}", text);
                let _ = stdout.flush();
            }
            EngineEvent::AssistantFinal { .. } => {
                // Final text is handled outside the sink for newline control
            }
            EngineEvent::ToolRequested { .. } => {
                // Not rendered in current CLI (will be in Commit 2)
            }
            EngineEvent::ToolStarted { name, .. } => {
                let _ = write!(stderr, "⚙ Running {}...", name);
                let _ = stderr.flush();
            }
            EngineEvent::ToolFinished { .. } => {
                let _ = writeln!(stderr, " Done.");
            }
            EngineEvent::Error { message } => {
                let _ = writeln!(stderr, "Error: {}", message);
            }
            EngineEvent::Interrupted => {
                // Handled by the caller
            }
        }
    })
}
