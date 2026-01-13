//! Status line feature slice.
//!
//! Provides a debug/performance status bar showing frame times, FPS, queue depth,
//! and other diagnostics. Inspired by Helix, k9s, and htop status lines.
//!
//! ## Module Structure
//!
//! - `state.rs`: StatusLineAccumulator (mutable counters) and StatusLine (immutable snapshot)
//! - `render.rs`: Status line rendering
//!
//! ## Update Cadence
//!
//! - **Per-frame**: frame_ms, fps_ema, sparkline (ultra-cheap)
//! - **~1s heartbeat**: inbox_depth, pending_tasks, spawn_rate, cpu/mem (sampled)
//! - **~5s**: p95/p99 frame times (computed off-frame)
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod render;
mod state;

pub use render::render_debug_status_line;
pub use state::{StatusLine, StatusLineAccumulator};
