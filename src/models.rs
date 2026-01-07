//! Model registry for the TUI model picker.
//!
//! This module re-exports the generated model data from `models_generated.rs`.
//! The generated file is produced by `cargo run --bin generate_models` and should
//! be committed to the repository.

#[path = "models_generated.rs"]
mod generated;

use std::sync::OnceLock;

// Re-export types for use throughout the application
pub use generated::{ModelOption, ModelPricing};

const CODEX_MODELS: &[ModelOption] = &[
    ModelOption {
        id: "gpt-5.1-codex",
        display_name: "GPT-5.1 Codex",
        pricing: ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 0.0,
        },
        context_limit: 400_000,
    },
    ModelOption {
        id: "gpt-5.1-codex-max",
        display_name: "GPT-5.1 Codex Max",
        pricing: ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 0.0,
        },
        context_limit: 400_000,
    },
    ModelOption {
        id: "gpt-5.1-codex-mini",
        display_name: "GPT-5.1 Codex Mini",
        pricing: ModelPricing {
            input: 0.25,
            output: 2.0,
            cache_read: 0.025,
            cache_write: 0.0,
        },
        context_limit: 400_000,
    },
    ModelOption {
        id: "gpt-5.2-codex",
        display_name: "GPT-5.2 Codex",
        pricing: ModelPricing {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_limit: 400_000,
    },
    ModelOption {
        id: "gpt-5.2",
        display_name: "GPT-5.2",
        pricing: ModelPricing {
            input: 1.75,
            output: 14.0,
            cache_read: 0.175,
            cache_write: 0.0,
        },
        context_limit: 400_000,
    },
    ModelOption {
        id: "gpt-5.1",
        display_name: "GPT-5.1",
        pricing: ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_read: 0.13,
            cache_write: 0.0,
        },
        context_limit: 400_000,
    },
];

static ALL_MODELS: OnceLock<Vec<ModelOption>> = OnceLock::new();

/// Returns all available models (generated + extra providers).
pub fn available_models() -> &'static [ModelOption] {
    ALL_MODELS
        .get_or_init(|| {
            let mut models = generated::AVAILABLE_MODELS.to_vec();
            models.extend_from_slice(CODEX_MODELS);
            models
        })
        .as_slice()
}

impl ModelOption {
    /// Finds a model by its ID.
    pub fn find_by_id(id: &str) -> Option<&'static ModelOption> {
        available_models().iter().find(|m| m.id == id)
    }
}
