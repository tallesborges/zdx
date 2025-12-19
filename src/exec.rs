//! Exec module for handling single-shot prompt execution with tool support.
//!
//! This module provides backward-compatible wrappers around the engine.
//! New code should use the engine module directly.

use anyhow::Result;
use std::path::PathBuf;

use crate::config::Config;
use crate::engine::{self, EngineOptions};
use crate::providers::anthropic::ChatMessage;
use crate::renderer;
use crate::session::{self, Session, SessionEvent};

/// Options for exec execution.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
        }
    }
}

impl From<&ExecOptions> for EngineOptions {
    fn from(opts: &ExecOptions) -> Self {
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
    mut session: Option<Session>,
    options: &ExecOptions,
) -> Result<String> {
    let effective =
        crate::context::build_effective_system_prompt_with_paths(config, &options.root)?;

    // Emit warnings from context loading to stderr
    for warning in &effective.warnings {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "Warning: {}", warning.message);
    }

    // Emit loaded AGENTS.md paths info (per SPEC §10)
    if !effective.loaded_agents_paths.is_empty() {
        let paths_str: Vec<String> = effective
            .loaded_agents_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "Loaded AGENTS.md from: {}",
            paths_str.join(", ")
        );
    }

    // Log user message to session (ensures meta is written for new sessions)
    if let Some(ref mut s) = session {
        s.append(&SessionEvent::user_message(prompt))?;
    }

    let messages = vec![ChatMessage::user(prompt)];
    let engine_opts = EngineOptions::from(options);

    // Create channels for fan-out
    let (engine_tx, engine_rx) = engine::create_event_channel();
    let (render_tx, render_rx) = engine::create_event_channel();

    // Spawn renderer task
    let renderer_handle = renderer::spawn_renderer_task(render_rx);

    // Spawn persist task if session exists
    let persist_handle = if let Some(sess) = session.clone() {
        let (persist_tx, persist_rx) = engine::create_event_channel();
        let fanout = engine::spawn_fanout_task(engine_rx, vec![render_tx, persist_tx]);
        let persist = session::spawn_persist_task(sess, persist_rx);
        Some((fanout, persist))
    } else {
        // No session - just fan out to renderer
        let fanout = engine::spawn_fanout_task(engine_rx, vec![render_tx]);
        Some((fanout, tokio::spawn(async {}))) // Dummy persist task
    };

    // Run the engine turn
    let result = engine::run_turn(
        messages,
        config,
        &engine_opts,
        effective.prompt.as_deref(),
        engine_tx,
    )
    .await;

    // Wait for all tasks to complete (even on error, to flush error events)
    if let Some((fanout, persist)) = persist_handle {
        let _ = fanout.await;
        let _ = persist.await;
    }
    let _ = renderer_handle.await;

    // Propagate error after tasks complete
    let (final_text, _messages) = result?;

    // Log assistant response to session
    if let Some(ref mut s) = session {
        s.append(&SessionEvent::assistant_message(&final_text))?;
    }

    Ok(final_text)
}
