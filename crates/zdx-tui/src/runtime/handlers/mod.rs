//! Effect handlers for the TUI runtime.
//!
//! This module contains the implementation of side effects triggered by the reducer.
//! These functions perform I/O and async tasks. They do NOT mutate state directly.
//!
//! ## Pure Async Pattern
//!
//! Handlers are pure async functions that return `UiEvent`. The runtime uses
//! `spawn_effect` to spawn them and send results to the inbox. This keeps
//! handlers focused on business logic while the runtime handles spawning.
//!
//! ```ignore
//! // Handler: pure async, returns UiEvent
//! pub async fn thread_list_load(cells: Vec<HistoryCell>) -> UiEvent { ... }
//!
//! // Runtime: spawns and sends to inbox
//! self.spawn_effect(Some(started_event), || handler(args));
//! ```

pub mod agent;
pub mod auth;
pub mod bash;
pub mod file_picker;
pub mod skills;
pub mod thread;

pub use agent::*;
pub use auth::*;
pub use bash::*;
pub use file_picker::*;
pub use skills::*;
pub use thread::*;

#[cfg(test)]
mod tests;
