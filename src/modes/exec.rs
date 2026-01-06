//! Streamed stdout/stderr rendering and exec wrapper.
//!
//! This module provides:
//! - `ExecRenderer` + `spawn_exec_renderer_task` for agent events
//! - `run_exec` for single-shot exec mode

use std::collections::HashMap;
use std::io::{Stderr, Stdout, Write, stderr, stdout};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::core::agent::AgentOptions;
use crate::core::events::{AgentEvent, ToolOutput};
use crate::core::session::{self, Session, SessionEvent};
use crate::providers::anthropic::ChatMessage;

/// Options for exec execution.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
}

impl From<&ExecOptions> for AgentOptions {
    fn from(opts: &ExecOptions) -> Self {
        AgentOptions {
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
/// This is a backward-compatible wrapper that uses the agent internally.
pub async fn run_exec(
    prompt: &str,
    config: &Config,
    mut session: Option<Session>,
    options: &ExecOptions,
) -> Result<String> {
    let effective =
        crate::core::context::build_effective_system_prompt_with_paths(config, &options.root)?;

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
    let agent_opts = AgentOptions::from(options);

    // Create channels for broadcast
    let (agent_tx, agent_rx) = crate::core::agent::create_event_channel();
    let (render_tx, render_rx) = crate::core::agent::create_event_channel();

    // Spawn renderer task
    let renderer_handle = spawn_exec_renderer_task(render_rx);

    // Spawn persist task if session exists
    let persist_handle = if let Some(sess) = session.clone() {
        let (persist_tx, persist_rx) = crate::core::agent::create_event_channel();
        let broadcaster =
            crate::core::agent::spawn_broadcaster(agent_rx, vec![render_tx, persist_tx]);
        let persist = session::spawn_persist_task(sess, persist_rx);
        Some((broadcaster, persist))
    } else {
        // No session - just broadcast to renderer
        let broadcaster = crate::core::agent::spawn_broadcaster(agent_rx, vec![render_tx]);
        Some((broadcaster, tokio::spawn(async {}))) // Dummy persist task
    };

    // Run the agent turn
    let result = crate::core::agent::run_turn(
        messages,
        config,
        &agent_opts,
        effective.prompt.as_deref(),
        agent_tx,
    )
    .await;

    // Wait for all tasks to complete (even on error, to flush error events)
    if let Some((broadcaster, persist)) = persist_handle {
        let _ = broadcaster.await;
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

/// CLI renderer that writes agent events to stdout/stderr.
///
/// # Output contract
/// - `AssistantDelta` and `AssistantComplete` → stdout
/// - `ToolStarted`, `ToolFinished`, `Error`, etc. → stderr
pub struct ExecRenderer {
    stdout: Stdout,
    stderr: Stderr,
    /// Whether the final newline has been printed after assistant output.
    needs_final_newline: bool,
    /// Tracks tool_use id -> name for ToolFinished rendering.
    tool_names: HashMap<String, String>,
    /// Tracks tool start times for duration calculation.
    tool_start_times: HashMap<String, Instant>,
}

impl Default for ExecRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecRenderer {
    /// Creates a new CLI renderer.
    pub fn new() -> Self {
        Self {
            stdout: stdout(),
            stderr: stderr(),
            needs_final_newline: false,
            tool_names: HashMap::new(),
            tool_start_times: HashMap::new(),
        }
    }

    /// Handles a single agent event by writing to the appropriate stream.
    pub fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AssistantDelta { text } => {
                if !text.is_empty() {
                    let _ = write!(self.stdout, "{}", text);
                    let _ = self.stdout.flush();
                    self.needs_final_newline = true;
                }
            }
            AgentEvent::AssistantComplete { text } => {
                // Final text is already streamed via deltas; track newline state
                if !text.is_empty() {
                    self.needs_final_newline = true;
                }
            }
            AgentEvent::ToolRequested { id, name, .. } => {
                // Ensure newline after assistant text before tool status
                if self.needs_final_newline {
                    let _ = writeln!(self.stdout);
                    let _ = self.stdout.flush();
                    self.needs_final_newline = false;
                }

                // Track tool name for ToolFinished rendering
                self.tool_names.insert(id.clone(), name.clone());
            }
            AgentEvent::ToolInputReady { id, name, input } => {
                // Emit debug line for bash tool (per SPEC §10)
                // This is emitted here (not ToolRequested) because we now have the full input
                if name == "bash"
                    && let Some(command) = input.get("command").and_then(|v| v.as_str())
                {
                    let _ = writeln!(self.stderr, "Tool requested: bash command=\"{}\"", command);
                }

                // Track tool name for ToolFinished rendering (if not already tracked)
                self.tool_names.entry(id.clone()).or_insert(name.clone());
            }
            AgentEvent::ToolStarted { id, name } => {
                self.tool_start_times.insert(id, Instant::now());
                let _ = write!(self.stderr, "⚙ Running {}...", name);
                let _ = self.stderr.flush();
            }
            AgentEvent::ToolFinished { id, result } => {
                // Calculate duration if we have a start time
                let duration_str = self
                    .tool_start_times
                    .remove(&id)
                    .map(|start| format!(" ({:.2}s)", start.elapsed().as_secs_f64()))
                    .unwrap_or_default();

                let _ = writeln!(self.stderr, " Done.{}", duration_str);

                // Emit debug line for bash tool (per SPEC §10)
                if let Some(name) = self.tool_names.get(&id)
                    && name == "bash"
                {
                    self.emit_bash_finish_details(&result);
                }
            }
            AgentEvent::Error {
                kind,
                message,
                details,
            } => {
                // Print one-liner to stderr
                let _ = writeln!(self.stderr, "Error [{}]: {}", kind, message);
                // Print details if present (indented)
                if let Some(ref detail_text) = details {
                    let _ = writeln!(self.stderr, "  Details: {}", detail_text);
                }
            }
            AgentEvent::Interrupted => {
                // Print interruption message to stderr (per SPEC §10)
                let _ = writeln!(self.stderr, "\n^C Interrupted.");
            }
            AgentEvent::TurnComplete { .. } => {
                // Turn complete - no action needed in exec mode.
                // The caller handles the final result from run_turn.
            }
            AgentEvent::ThinkingDelta { text } => {
                // In exec mode, stream thinking text to stderr (no styling)
                if !text.is_empty() {
                    let _ = write!(self.stderr, "{}", text);
                    let _ = self.stderr.flush();
                }
            }
            AgentEvent::ThinkingComplete { .. } => {
                // Thinking complete - ensure newline after thinking output
                let _ = writeln!(self.stderr);
                let _ = self.stderr.flush();
            }
            AgentEvent::UsageUpdate { .. } => {
                // Usage tracking not displayed in exec mode
            }
            AgentEvent::TurnStarted => {
                // Turn start not displayed in exec mode
            }
            AgentEvent::ToolOutputDelta { .. } => {
                // TODO: Stream tool output in real-time
                // For now, we only show final output in ToolFinished
            }
        }
    }

    /// Emits bash tool finish details to stderr.
    fn emit_bash_finish_details(&mut self, result: &ToolOutput) {
        match result {
            ToolOutput::Success { data, .. } => {
                // Check for timed_out first
                if let Some(true) = data.get("timed_out").and_then(|v| v.as_bool()) {
                    let _ = writeln!(self.stderr, "Tool finished: bash timed_out=true");
                } else if let Some(exit_code) = data.get("exit_code").and_then(|v| v.as_i64()) {
                    let _ = writeln!(self.stderr, "Tool finished: bash exit={}", exit_code);
                }
            }
            ToolOutput::Failure { error, .. } => {
                let _ = writeln!(
                    self.stderr,
                    "Tool finished: bash error=\"{}\"",
                    error.message
                );
            }
            ToolOutput::Canceled { message } => {
                let _ = writeln!(self.stderr, "Tool finished: bash canceled ({})", message);
            }
        }
    }

    /// Prints a final newline to stdout if needed (after assistant output completes).
    pub fn finish(&mut self) {
        if self.needs_final_newline {
            let _ = writeln!(self.stdout);
            self.needs_final_newline = false;
        }
    }
}

/// Spawns a renderer task that consumes events from a channel.
///
/// The task owns the `ExecRenderer` and processes events until the channel closes.
/// Returns a `JoinHandle` that resolves when all events have been rendered.
pub fn spawn_exec_renderer_task(mut rx: crate::core::agent::AgentEventRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut renderer = ExecRenderer::new();

        while let Some(event) = rx.recv().await {
            renderer.handle_event((*event).clone());
        }

        renderer.finish();
    })
}
