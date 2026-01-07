//! LLM provider implementations.

pub mod anthropic;
pub mod oauth;
pub mod openai_codex;

/// Provider selection based on model naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAICodex,
}

/// Infers the provider from a model identifier.
pub fn provider_for_model(model: &str) -> ProviderKind {
    let normalized = model.trim().to_lowercase();
    if normalized.starts_with("gpt-") || normalized.contains("codex") {
        ProviderKind::OpenAICodex
    } else {
        ProviderKind::Anthropic
    }
}
