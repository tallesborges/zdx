//! Status line state types.

/// Public, immutable snapshot read by the renderer each frame.
#[derive(Clone, Debug, Default)]
pub struct StatusLine {
    pub fps: f32,
}

/// Mutable accumulator that tracks FPS.
#[derive(Debug)]
pub struct StatusLineAccumulator {
    fps_ema: f32,
}

impl Default for StatusLineAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusLineAccumulator {
    pub fn new() -> Self {
        Self { fps_ema: 60.0 }
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

    /// Get snapshot for rendering.
    pub fn snapshot(&self) -> StatusLine {
        StatusLine {
            fps: (self.fps_ema * 10.0).round() / 10.0,
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
}
