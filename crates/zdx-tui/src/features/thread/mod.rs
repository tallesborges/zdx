//! Thread feature slice.
//!
//! Owns thread state, thread events, and thread-related update logic.
//!
//! ## Module Structure
//!
//! - `state.rs`: ThreadState, ThreadUsage - in-memory thread state
//! - `update.rs`: Thread event handlers (loading, switching, creating, renaming)
//! - `render.rs`: Thread picker overlay rendering
//! - `tree.rs`: Thread tree derivation for hierarchical display
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod render;
mod state;
mod tree;
mod update;

// Re-export state types
// Re-export view functions
pub use render::{MAX_VISIBLE_THREADS, render_thread_picker};
pub use state::{ThreadState, ThreadUsage};
// Re-export tree types for picker display
pub use tree::{ThreadDisplayItem, flatten_as_tree, flatten_refs_as_tree};
// Re-export reducer functions
pub use update::{ThreadOverlayAction, handle_thread_event};
