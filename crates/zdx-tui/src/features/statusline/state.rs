//! Status line state types.

use std::time::{Duration, Instant};

/// Public, immutable snapshot read by the renderer each frame.
#[derive(Clone, Debug, Default)]
pub struct StatusLine {
    pub fps: f32,
    /// Elapsed time since turn started (None if not running).
    pub turn_elapsed: Option<Duration>,
}

/// Mutable accumulator that tracks FPS and turn timing.
#[derive(Debug)]
pub struct StatusLineAccumulator {
    fps_ema: f32,
    /// When the current turn started (None if idle).
    turn_started_at: Option<Instant>,
}

impl Default for StatusLineAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusLineAccumulator {
    pub fn new() -> Self {
        Self {
            fps_ema: 60.0,
            turn_started_at: None,
        }
    }

    /// Update with frame time (ms).
    pub fn on_frame(&mut self, frame_ms: u16) {
        let fps = if frame_ms > 0 {
            1000.0 / frame_ms as f32
        } else {
            self.fps_ema
        };
        self.fps_ema += 0.1 * (fps - self.fps_ema);
    }

    /// Mark the start of a new turn.
    pub fn start_turn(&mut self) {
        self.turn_started_at = Some(Instant::now());
    }

    /// Clear turn timing (turn completed or interrupted).
    pub fn end_turn(&mut self) {
        self.turn_started_at = None;
    }

    /// Get snapshot for rendering.
    pub fn snapshot(&self) -> StatusLine {
        StatusLine {
            fps: (self.fps_ema * 10.0).round() / 10.0,
            turn_elapsed: self.turn_started_at.map(|start| start.elapsed()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fps_tracking() {
        let mut acc = StatusLineAccumulator::new();
        acc.on_frame(16); // ~60fps
        acc.on_frame(16);
        acc.on_frame(16);
        let snapshot = acc.snapshot();
        assert!(snapshot.fps > 50.0);
    }

    #[test]
    fn test_turn_timing() {
        let mut acc = StatusLineAccumulator::new();

        // Initially no turn is running
        let snapshot = acc.snapshot();
        assert!(snapshot.turn_elapsed.is_none());

        // Start a turn
        acc.start_turn();
        let snapshot = acc.snapshot();
        assert!(snapshot.turn_elapsed.is_some());

        // End the turn
        acc.end_turn();
        let snapshot = acc.snapshot();
        assert!(snapshot.turn_elapsed.is_none());
    }
}
