//! CLI renderer for engine events.
//!
//! The renderer is responsible for all output formatting. It consumes
//! `EngineEvent`s and writes them to stdout/stderr following the contract:
//! - Assistant text (deltas/final) → stdout only
//! - Tool status, diagnostics, errors → stderr only

use std::io::{Stderr, Stdout, Write, stderr, stdout};

use tokio::task::JoinHandle;

use crate::engine::{EventRx, EventSink};
use crate::events::EngineEvent;

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
            EngineEvent::ToolRequested { .. } => {
                // Not rendered in CLI - the ToolStarted event shows activity
            }
            EngineEvent::ToolStarted { name, .. } => {
                let _ = write!(self.stderr, "⚙ Running {}...", name);
                let _ = self.stderr.flush();
            }
            EngineEvent::ToolFinished { .. } => {
                let _ = writeln!(self.stderr, " Done.");
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

    /// Prints a final newline to stdout if needed (after assistant output completes).
    pub fn finish(&mut self) {
        if self.needs_final_newline {
            let _ = writeln!(self.stdout);
            self.needs_final_newline = false;
        }
    }

    /// Creates an EventSink that delegates to this renderer.
    ///
    /// The sink takes ownership of a mutable reference, so the renderer
    /// must be created before the sink and finished after the engine returns.
    pub fn into_sink(self) -> (EventSink, RendererHandle) {
        use std::sync::{Arc, Mutex};

        let renderer = Arc::new(Mutex::new(self));
        let renderer_clone = renderer.clone();

        let sink: EventSink = Box::new(move |event| {
            let mut r = renderer_clone.lock().unwrap();
            r.handle_event(event);
        });

        (sink, RendererHandle { renderer })
    }
}

/// Handle to a renderer used by an EventSink.
///
/// Call `finish()` after the engine completes to print trailing newlines.
pub struct RendererHandle {
    renderer: std::sync::Arc<std::sync::Mutex<CliRenderer>>,
}

impl RendererHandle {
    /// Finishes rendering (prints final newline if needed).
    pub fn finish(self) {
        let mut r = self.renderer.lock().unwrap();
        r.finish();
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
