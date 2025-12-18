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
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::engine::{self, EngineOptions, EventSink};
use crate::events::EngineEvent;
use crate::providers::anthropic::ChatMessage;
use crate::renderer::CliRenderer;
use crate::session::{Session, SessionEvent};

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
    let system_prompt = crate::context::build_effective_system_prompt(config, &root)?;

    // Wrap session in Arc<Mutex> for shared access in event sink
    let session = session.map(|s| Arc::new(Mutex::new(s)));

    let engine_opts = EngineOptions { root };

    // Print welcome banner to stderr
    let mut err = stderr();
    writeln!(err, "ZDX Chat (type :q to quit)")?;
    if let Some(ref s) = session {
        writeln!(err, "Session: {}", s.lock().unwrap().id)?;
    }
    if !history.is_empty() {
        writeln!(err, "Loaded {} previous messages", history.len())?;
    }
    write!(err, "{}", PROMPT_PREFIX)?;
    err.flush()?;

    run_chat_loop(
        stdin().lock(),
        config,
        &engine_opts,
        session,
        history,
        system_prompt.as_deref(),
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
    session: Option<Arc<Mutex<Session>>>,
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

        // Log user message to session
        if let Some(ref s) = session
            && let Err(e) = s
                .lock()
                .unwrap()
                .append(&SessionEvent::user_message(trimmed))
        {
            writeln!(err, "Warning: Failed to save session: {}", e)?;
        }

        // Run the turn through the engine
        let renderer = Arc::new(Mutex::new(CliRenderer::new()));
        let result = run_chat_turn(
            history.clone(),
            config,
            &engine_opts,
            system_prompt.as_deref(),
            session.clone(),
            renderer.clone(),
        )
        .await;

        match result {
            Ok((final_text, new_history)) => {
                // Finish rendering (prints final newline if needed)
                renderer.lock().unwrap().finish();

                if !final_text.is_empty() {
                    // Log assistant response to session
                    if let Some(ref s) = session
                        && let Err(e) = s
                            .lock()
                            .unwrap()
                            .append(&SessionEvent::assistant_message(&final_text))
                    {
                        writeln!(err, "Warning: Failed to save session: {}", e)?;
                    }
                }

                // Update history for next turn
                history = new_history;
            }
            Err(e) => {
                if e.downcast_ref::<crate::interrupt::InterruptedError>()
                    .is_some()
                {
                    // Interrupted event is already persisted via the event sink
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

/// Runs a single chat turn through the engine with rendering.
async fn run_chat_turn(
    messages: Vec<ChatMessage>,
    config: &Config,
    engine_opts: &EngineOptions,
    system_prompt: Option<&str>,
    session: Option<Arc<Mutex<Session>>>,
    renderer: Arc<Mutex<CliRenderer>>,
) -> Result<(String, Vec<ChatMessage>)> {
    let sink = create_persisting_sink(session, renderer);

    engine::run_turn(messages, config, engine_opts, system_prompt, sink).await
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

/// Runs the chat loop with an output writer (for testing).
///
/// Note: In the refactored version, we maintain backward compatibility by
/// writing REPL UI to `output` but assistant text goes to stdout.
#[allow(dead_code)] // Useful for testing
pub async fn run_chat<R, W>(
    input: R,
    mut output: W,
    config: &Config,
    session: Option<Session>,
    root: PathBuf,
) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let system_prompt = crate::context::build_effective_system_prompt(config, &root)?;
    let session = session.map(|s| Arc::new(Mutex::new(s)));
    let engine_opts = EngineOptions { root };

    // Write welcome to the provided output (for test capture)
    writeln!(output, "ZDX Chat (type :q to quit)")?;
    if let Some(ref s) = session {
        writeln!(output, "Session: {}", s.lock().unwrap().id)?;
    }
    write!(output, "{}", PROMPT_PREFIX)?;
    output.flush()?;

    let mut history: Vec<ChatMessage> = Vec::new();

    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();

        if trimmed == QUIT_COMMAND {
            writeln!(output, "Goodbye!")?;
            break;
        }

        if trimmed.is_empty() {
            write!(output, "{}", PROMPT_PREFIX)?;
            output.flush()?;
            continue;
        }

        history.push(ChatMessage::user(trimmed));

        if let Some(ref s) = session
            && let Err(e) = s
                .lock()
                .unwrap()
                .append(&SessionEvent::user_message(trimmed))
        {
            writeln!(output, "Warning: Failed to save session: {}", e)?;
        }

        let renderer = Arc::new(Mutex::new(CliRenderer::new()));
        let result = run_chat_turn(
            history.clone(),
            config,
            &engine_opts,
            system_prompt.as_deref(),
            session.clone(),
            renderer.clone(),
        )
        .await;

        match result {
            Ok((final_text, new_history)) => {
                // Finish rendering (prints final newline if needed)
                renderer.lock().unwrap().finish();

                if !final_text.is_empty() {
                    if let Some(ref s) = session
                        && let Err(e) = s
                            .lock()
                            .unwrap()
                            .append(&SessionEvent::assistant_message(&final_text))
                    {
                        writeln!(output, "Warning: Failed to save session: {}", e)?;
                    }
                }
                history = new_history;
            }
            Err(e) => {
                if e.downcast_ref::<crate::interrupt::InterruptedError>()
                    .is_some()
                {
                    // Interrupted event is already persisted via the event sink
                    crate::interrupt::reset();
                } else {
                    writeln!(output, "Error: {}", e)?;
                }
                history.pop();
            }
        }

        write!(output, "{}", PROMPT_PREFIX)?;
        output.flush()?;
    }

    Ok(())
}
