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
    /// Number of tools used during the current turn.
    turn_tool_count: usize,
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
            turn_tool_count: 0,
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
        self.turn_tool_count = 0;
    }

    /// Increment tool count for this turn.
    pub fn mark_tool_used(&mut self) {
        self.turn_tool_count += 1;
    }

    /// Clear turn timing and return elapsed duration with tool count.
    ///
    /// Returns (duration, tool_count) if tools were used, None otherwise.
    pub fn end_turn(&mut self) -> Option<(Duration, usize)> {
        let tool_count = self.turn_tool_count;
        self.turn_tool_count = 0;
        self.turn_started_at
            .take()
            .filter(|_| tool_count > 0)
            .map(|start| (start.elapsed(), tool_count))
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

        // End turn without tools - should return None
        let result = acc.end_turn();
        assert!(result.is_none());
        let snapshot = acc.snapshot();
        assert!(snapshot.turn_elapsed.is_none());

        // Start another turn with tools
        acc.start_turn();
        acc.mark_tool_used();
        acc.mark_tool_used();
        acc.mark_tool_used();
        let result = acc.end_turn();
        assert!(result.is_some());
        let (duration, tool_count) = result.unwrap();
        assert!(duration.as_nanos() > 0);
        assert_eq!(tool_count, 3);

        // End again - should return None
        let result = acc.end_turn();
        assert!(result.is_none());
    }
}
