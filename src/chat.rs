//! Interactive chat module for ZDX.
//!
//! Provides a REPL-style chat interface that maintains conversation history.
//! Uses the engine module for streaming and tool execution.
//!
//! ## Output Contract
//! - Assistant text (streamed) → stdout only
//! - REPL UI (welcome, prompts, goodbye, warnings) → stderr only
//! - Tool status indicators → stderr only (via renderer)

use std::io::{BufRead, Write, stderr, stdin};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::engine::{self, EngineOptions};
use crate::providers::anthropic::ChatMessage;
use crate::renderer;
use crate::session::{self, Session, SessionEvent};

const QUIT_COMMAND: &str = ":q";
const PROMPT_PREFIX: &str = "you> ";

/// Runs the interactive chat loop with stdin/stdout.
pub async fn run_interactive_chat(
    config: &Config,
    session: Option<Session>,
    root: PathBuf,
) -> Result<()> {
    run_interactive_chat_with_history(config, session, Vec::new(), root).await
}

/// Runs the interactive chat loop with pre-loaded history.
pub async fn run_interactive_chat_with_history(
    config: &Config,
    session: Option<Session>,
    history: Vec<ChatMessage>,
    root: PathBuf,
) -> Result<()> {
    let effective = crate::context::build_effective_system_prompt_with_paths(config, &root)?;

    let engine_opts = EngineOptions { root };

    // Print welcome banner to stderr
    let mut err = stderr();
    writeln!(err, "ZDX Chat (type :q to quit)")?;
    writeln!(err, "Model: {}", config.model)?;
    if let Some(ref s) = session {
        writeln!(err, "Session: {}", s.id)?;
    }
    if !history.is_empty() {
        writeln!(err, "Loaded {} previous messages", history.len())?;
    }

    // Emit warnings from context loading (per SPEC §10)
    for warning in &effective.warnings {
        writeln!(err, "Warning: {}", warning.message)?;
    }

    // Show loaded AGENTS.md files
    if !effective.loaded_agents_paths.is_empty() {
        writeln!(err, "Loaded AGENTS.md from:")?;
        for path in &effective.loaded_agents_paths {
            writeln!(err, "  - {}", path.display())?;
        }
    }
    write!(err, "{}", PROMPT_PREFIX)?;
    err.flush()?;

    run_chat_loop(
        stdin().lock(),
        config,
        &engine_opts,
        session,
        history,
        effective.prompt.as_deref(),
    )
    .await
}

/// Internal chat loop that reads from a BufRead source.
///
/// This is separated for testability.
async fn run_chat_loop<R: BufRead>(
    input: R,
    config: &Config,
    engine_opts: &EngineOptions,
    mut session: Option<Session>,
    initial_history: Vec<ChatMessage>,
    system_prompt: Option<&str>,
) -> Result<()> {
    let mut history = initial_history;
    let mut err = stderr();

    for line in input.lines() {
        let line = line.context("Failed to read input line")?;
        let trimmed = line.trim();

        // Handle quit command
        if trimmed == QUIT_COMMAND {
            writeln!(err, "Goodbye!")?;
            break;
        }

        // Skip empty lines - re-render prompt
        if trimmed.is_empty() {
            write!(err, "{}", PROMPT_PREFIX)?;
            err.flush()?;
            continue;
        }

        // Add user message to history
        history.push(ChatMessage::user(trimmed));

        // Log user message to session (this ensures meta is written for new sessions)
        if let Some(ref mut s) = session {
            if let Err(e) = s.append(&SessionEvent::user_message(trimmed)) {
                writeln!(err, "Warning: Failed to save session: {}", e)?;
            }
        }

        // Clone session for the persist task (tool events will be logged there)
        // User/assistant messages are logged here in the chat loop
        let session_for_turn = session.clone();

        // Run the turn through the engine with channel-based rendering
        let result = run_chat_turn_async(
            history.clone(),
            config,
            engine_opts,
            system_prompt.as_deref(),
            session_for_turn,
        )
        .await;

        match result {
            Ok((final_text, new_history)) => {
                // Renderer task handles finish() automatically

                if !final_text.is_empty() {
                    // Log assistant response to session
                    if let Some(ref mut s) = session {
                        if let Err(e) = s.append(&SessionEvent::assistant_message(&final_text)) {
                            writeln!(err, "Warning: Failed to save session: {}", e)?;
                        }
                    }
                }

                // Update history for next turn
                history = new_history;
            }
            Err(e) => {
                if e.downcast_ref::<crate::interrupt::InterruptedError>()
                    .is_some()
                {
                    // Interrupted event is already persisted via the persist task
                    crate::interrupt::reset();
                } else {
                    writeln!(err, "Error: {}", e)?;
                }
                // Remove the failed user message from history
                history.pop();
            }
        }

        // Re-render prompt
        write!(err, "{}", PROMPT_PREFIX)?;
        err.flush()?;
    }

    Ok(())
}

/// Runs a single chat turn through the engine with channel-based rendering.
///
/// This is the new channel-based implementation that spawns separate tasks
/// for rendering and session persistence, avoiding Arc<Mutex> across awaits.
async fn run_chat_turn_async(
    messages: Vec<ChatMessage>,
    config: &Config,
    engine_opts: &EngineOptions,
    system_prompt: Option<&str>,
    session: Option<Session>,
) -> Result<(String, Vec<ChatMessage>)> {
    // Create channels for fan-out
    let (engine_tx, engine_rx) = engine::create_event_channel();
    let (render_tx, render_rx) = engine::create_event_channel();

    // Spawn renderer task
    let renderer_handle = renderer::spawn_renderer_task(render_rx);

    // Spawn persist task if session exists, otherwise create a dummy receiver
    let persist_handle = if let Some(sess) = session {
        let (persist_tx, persist_rx) = engine::create_event_channel();
        let fanout = engine::spawn_fanout_task(engine_rx, vec![render_tx, persist_tx]);
        let persist = session::spawn_persist_task(sess, persist_rx);
        Some((fanout, persist))
    } else {
        // No session - just fan out to renderer
        let fanout = engine::spawn_fanout_task(engine_rx, vec![render_tx]);
        Some((fanout, tokio::spawn(async {}))) // Dummy persist task
    };

    // Run the engine turn (don't use ? yet - need to await tasks first)
    let result = engine::run_turn_async(
        messages,
        config,
        engine_opts,
        system_prompt,
        engine_tx,
    )
    .await;

    // Wait for all tasks to complete (even on error, to flush error events)
    if let Some((fanout, persist)) = persist_handle {
        let _ = fanout.await;
        let _ = persist.await;
    }
    let _ = renderer_handle.await;

    // Now propagate the result (success or error)
    result
}
