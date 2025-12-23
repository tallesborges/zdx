//! Model registry for the TUI model picker.
//!
//! This module re-exports the generated model data from `models_generated.rs`.
//! The generated file is produced by `cargo run --bin generate_models` and should
//! be committed to the repository.

#[path = "models_generated.rs"]
mod generated;

// ModelOption is re-exported for future use (e.g., filtering, grouping)
#[allow(unused_imports)]
pub use generated::{AVAILABLE_MODELS, ModelOption};
