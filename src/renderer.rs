//! CLI renderer for engine events.
//!
//! The renderer is responsible for all output formatting. It consumes
//! `EngineEvent`s and writes them to stdout/stderr following the contract:
//! - Assistant text (deltas/final) → stdout only
//! - Tool status, diagnostics, errors → stderr only

use std::io::{Stderr, Stdout, Write, stderr, stdout};

use crate::engine::EventSink;
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
            EngineEvent::Error { message } => {
                let _ = writeln!(self.stderr, "Error: {}", message);
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::events::ToolOutput;

    #[test]
    fn test_renderer_tracks_newline_state() {
        let mut renderer = CliRenderer::new();
        assert!(!renderer.needs_final_newline);

        renderer.handle_event(EngineEvent::AssistantDelta {
            text: "Hello".to_string(),
        });
        assert!(renderer.needs_final_newline);
    }

    #[test]
    fn test_renderer_empty_delta_no_newline() {
        let mut renderer = CliRenderer::new();
        renderer.handle_event(EngineEvent::AssistantDelta {
            text: String::new(),
        });
        assert!(!renderer.needs_final_newline);
    }

    #[test]
    fn test_renderer_into_sink() {
        let renderer = CliRenderer::new();
        let (mut sink, handle) = renderer.into_sink();

        // Sink should be callable
        sink(EngineEvent::AssistantDelta {
            text: "test".to_string(),
        });

        // Handle should be finishable
        handle.finish();
    }

    #[test]
    fn test_renderer_handles_all_event_types() {
        let mut renderer = CliRenderer::new();

        // These should not panic
        renderer.handle_event(EngineEvent::AssistantDelta {
            text: "Hello".to_string(),
        });
        renderer.handle_event(EngineEvent::AssistantFinal {
            text: "Hello".to_string(),
        });
        renderer.handle_event(EngineEvent::ToolRequested {
            id: "1".to_string(),
            name: "read".to_string(),
            input: json!({"path": "test.txt"}),
        });
        renderer.handle_event(EngineEvent::ToolStarted {
            id: "1".to_string(),
            name: "read".to_string(),
        });
        renderer.handle_event(EngineEvent::ToolFinished {
            id: "1".to_string(),
            result: ToolOutput::success(json!({"content": "hello"})),
        });
        renderer.handle_event(EngineEvent::Error {
            message: "test error".to_string(),
        });
        renderer.handle_event(EngineEvent::Interrupted);
    }
}
