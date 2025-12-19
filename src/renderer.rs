//! CLI renderer for engine events.
//!
//! The renderer is responsible for all output formatting. It consumes
//! `EngineEvent`s and writes them to stdout/stderr following the contract:
//! - Assistant text (deltas/final) → stdout only
//! - Tool status, diagnostics, errors → stderr only

use std::collections::HashMap;
use std::io::{Stderr, Stdout, Write, stderr, stdout};
use std::time::Instant;

use tokio::task::JoinHandle;

use crate::engine::EventRx;
use crate::events::{EngineEvent, ToolOutput};

/// CLI renderer that writes engine events to stdout/stderr.
///
/// # Output contract
/// - `AssistantDelta` and `AssistantFinal` → stdout
/// - `ToolStarted`, `ToolFinished`, `Error`, etc. → stderr
pub struct CliRenderer {
    stdout: Stdout,
    stderr: Stderr,
    /// Whether the final newline has been printed after assistant output.
    needs_final_newline: bool,
    /// Tracks tool_use id -> name for ToolFinished rendering.
    tool_names: HashMap<String, String>,
    /// Tracks tool start times for duration calculation.
    tool_start_times: HashMap<String, Instant>,
}

impl Default for CliRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CliRenderer {
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

    /// Handles a single engine event by writing to the appropriate stream.
    pub fn handle_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::AssistantDelta { text } => {
                if !text.is_empty() {
                    let _ = write!(self.stdout, "{}", text);
                    let _ = self.stdout.flush();
                    self.needs_final_newline = true;
                }
            }
            EngineEvent::AssistantFinal { text } => {
                // Final text is already streamed via deltas; track newline state
                if !text.is_empty() {
                    self.needs_final_newline = true;
                }
            }
            EngineEvent::ToolRequested { id, name, input } => {
                // Track tool name for ToolFinished rendering
                self.tool_names.insert(id.clone(), name.clone());

                // Emit debug line for bash tool (per SPEC §10)
                if name == "bash"
                    && let Some(command) = input.get("command").and_then(|v| v.as_str())
                {
                    let _ =
                        writeln!(self.stderr, "Tool requested: bash command=\"{}\"", command);
                }
            }
            EngineEvent::ToolStarted { id, name } => {
                self.tool_start_times.insert(id, Instant::now());
                let _ = write!(self.stderr, "⚙ Running {}...", name);
                let _ = self.stderr.flush();
            }
            EngineEvent::ToolFinished { id, result } => {
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
            EngineEvent::Warning { message } => {
                // Print warning to stderr (per SPEC §10)
                let _ = writeln!(self.stderr, "Warning: {}", message);
            }
            EngineEvent::Error {
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
            EngineEvent::Interrupted => {
                // Print interruption message to stderr (per SPEC §10)
                let _ = writeln!(self.stderr, "\n^C Interrupted.");
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
/// The task owns the `CliRenderer` and processes events until the channel closes.
/// Returns a `JoinHandle` that resolves when all events have been rendered.
///
/// # Example
///
/// ```ignore
/// let (tx, rx) = engine::create_event_channel();
/// let renderer_handle = spawn_renderer_task(rx);
///
/// // ... send events to tx ...
/// drop(tx); // Close channel
///
/// renderer_handle.await.unwrap(); // Wait for renderer to finish
/// ```
pub fn spawn_renderer_task(mut rx: EventRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut renderer = CliRenderer::new();

        while let Some(event) = rx.recv().await {
            renderer.handle_event((*event).clone());
        }

        renderer.finish();
    })
}
