//! Agent module for handling prompt execution with tool support.
//!
//! This module provides backward-compatible wrappers around the engine.
//! New code should use the engine module directly.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::config::Config;
use crate::engine::{self, EngineOptions, EventSink};
use crate::events::EngineEvent;
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
/// If a session is provided, logs the user prompt and final assistant response,
/// plus tool_use and tool_result events for full history.
/// Implements tool loop - if the model requests tools, executes them and continues.
/// Returns the complete response text.
///
/// This is a backward-compatible wrapper that uses the engine internally.
pub async fn execute_prompt_streaming(
    prompt: &str,
    config: &Config,
    session: Option<Session>,
    options: &AgentOptions,
) -> Result<String> {
    let system_prompt = crate::context::build_effective_system_prompt(config, &options.root)?;

    // Wrap session in Arc<Mutex> for shared access in event sink
    let session = session.map(|s| Arc::new(Mutex::new(s)));

    // Log user message to session
    if let Some(ref s) = session {
        s.lock()
            .unwrap()
            .append(&SessionEvent::user_message(prompt))?;
    }

    let messages = vec![ChatMessage::user(prompt)];
    let engine_opts = EngineOptions::from(options);

    // Create a combined sink that handles both rendering and session persistence
    let renderer = Arc::new(Mutex::new(CliRenderer::new()));
    let sink = create_persisting_sink(session.clone(), renderer.clone());

    let (final_text, _messages) = engine::run_turn(
        messages,
        config,
        &engine_opts,
        system_prompt.as_deref(),
        sink,
    )
    .await?;

    // Finish rendering (prints final newline if needed)
    renderer.lock().unwrap().finish();

    // Log assistant response to session
    if let Some(ref s) = session {
        s.lock()
            .unwrap()
            .append(&SessionEvent::assistant_message(&final_text))?;
    }

    Ok(final_text)
}

/// Creates an EventSink that renders to CLI and persists tool events to session.
fn create_persisting_sink(
    session: Option<Arc<Mutex<Session>>>,
    renderer: Arc<Mutex<CliRenderer>>,
) -> EventSink {
    Box::new(move |event: EngineEvent| {
        // Persist tool and interrupt events to session
        if let Some(ref s) = session {
            match &event {
                EngineEvent::ToolRequested { id, name, input } => {
                    let _ = s.lock().unwrap().append(&SessionEvent::tool_use(
                        id.clone(),
                        name.clone(),
                        input.clone(),
                    ));
                }
                EngineEvent::ToolFinished { id, result } => {
                    let output = serde_json::to_value(result).unwrap_or_default();
                    let _ = s.lock().unwrap().append(&SessionEvent::tool_result(
                        id.clone(),
                        output,
                        result.is_ok(),
                    ));
                }
                EngineEvent::Interrupted => {
                    // Persist interrupted event (best-effort, per SPEC §10)
                    let _ = s.lock().unwrap().append(&SessionEvent::interrupted());
                }
                _ => {}
            }
        }

        // Render to CLI
        renderer.lock().unwrap().handle_event(event);
    })
}
