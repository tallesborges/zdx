//! Model registry for the TUI model picker.
//!
//! This module re-exports the generated model data from `models_generated.rs`.
//! The generated file is produced by `cargo run --bin generate_models` and should
//! be committed to the repository.

#[path = "models_generated.rs"]
mod generated;

// Re-export types for use throughout the application
pub use generated::{AVAILABLE_MODELS, ModelOption, ModelPricing};

impl ModelOption {
    /// Finds a model by its ID.
    pub fn find_by_id(id: &str) -> Option<&'static ModelOption> {
        AVAILABLE_MODELS.iter().find(|m| m.id == id)
    }
}
