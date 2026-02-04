//! LLM provider implementations.

mod debug_metrics;
mod debug_trace;
pub mod text_tool_parser;
pub mod thinking_parser;

pub mod anthropic;
pub mod gemini;
pub mod mimo;
pub mod mistral;
pub mod moonshot;
pub mod oauth;
pub mod openai;
pub mod openrouter;
pub mod shared;
pub mod stepfun;

pub use debug_trace::{DebugTrace, TraceStream, wrap_stream};
pub use shared::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, ReasoningBlock, ReplayToken, StreamEvent,
    Usage, resolve_api_key, resolve_base_url,
};

/// Provider selection based on model naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    ClaudeCli,
    OpenAICodex,
    OpenAI,
    OpenRouter,
    Mimo,
    Mistral,
    Moonshot,
    Stepfun,
    Gemini,
    GeminiCli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAuthMode {
    OAuth,
    ApiKey,
}

/// Provider selection result with normalized model ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSelection {
    pub kind: ProviderKind,
    pub model: String,
}

impl ProviderKind {
    /// Returns all provider kinds.
    pub fn all() -> &'static [ProviderKind] {
        &[
            ProviderKind::Anthropic,
            ProviderKind::ClaudeCli,
            ProviderKind::OpenAICodex,
            ProviderKind::OpenAI,
            ProviderKind::OpenRouter,
            ProviderKind::Mimo,
            ProviderKind::Mistral,
            ProviderKind::Moonshot,
            ProviderKind::Stepfun,
            ProviderKind::Gemini,
            ProviderKind::GeminiCli,
        ]
    }

    /// Returns the string identifier used in config files and model registry.
    pub fn id(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::ClaudeCli => "claude-cli",
            ProviderKind::OpenAICodex => "openai-codex",
            ProviderKind::OpenAI => "openai",
            ProviderKind::OpenRouter => "openrouter",
            ProviderKind::Mimo => "mimo",
            ProviderKind::Mistral => "mistral",
            ProviderKind::Moonshot => "moonshot",
            ProviderKind::Stepfun => "stepfun",
            ProviderKind::Gemini => "gemini",
            ProviderKind::GeminiCli => "gemini-cli",
        }
    }

    /// Returns the ProviderKind for a given id string.
    pub fn from_id(id: &str) -> Option<ProviderKind> {
        match id.to_lowercase().as_str() {
            "anthropic" => Some(ProviderKind::Anthropic),
            "claude-cli" => Some(ProviderKind::ClaudeCli),
            "openai-codex" | "codex" => Some(ProviderKind::OpenAICodex),
            "openai" => Some(ProviderKind::OpenAI),
            "openrouter" => Some(ProviderKind::OpenRouter),
            "mimo" => Some(ProviderKind::Mimo),
            "mistral" => Some(ProviderKind::Mistral),
            "moonshot" => Some(ProviderKind::Moonshot),
            "stepfun" => Some(ProviderKind::Stepfun),
            "gemini" => Some(ProviderKind::Gemini),
            "gemini-cli" => Some(ProviderKind::GeminiCli),
            _ => None,
        }
    }

    /// Returns the human-readable label for display.
    pub fn label(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::ClaudeCli => "Claude CLI",
            ProviderKind::OpenAICodex => "OpenAI Codex",
            ProviderKind::OpenAI => "OpenAI",
            ProviderKind::OpenRouter => "OpenRouter",
            ProviderKind::Mimo => "MiMo",
            ProviderKind::Mistral => "Mistral",
            ProviderKind::Moonshot => "Moonshot",
            ProviderKind::Stepfun => "StepFun",
            ProviderKind::Gemini => "Gemini",
            ProviderKind::GeminiCli => "Gemini CLI",
        }
    }

    pub fn supports_oauth(&self) -> bool {
        matches!(
            self,
            ProviderKind::ClaudeCli | ProviderKind::OpenAICodex | ProviderKind::GeminiCli
        )
    }

    /// Returns true if this provider is subscription-based (usage included in subscription).
    pub fn is_subscription(&self) -> bool {
        // OAuth providers are typically subscription-based (no per-token charges)
        self.supports_oauth()
    }

    pub fn api_key_env_var(&self) -> Option<&'static str> {
        match self {
            ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
            ProviderKind::ClaudeCli => None,
            ProviderKind::OpenAI => Some("OPENAI_API_KEY"),
            ProviderKind::OpenRouter => Some("OPENROUTER_API_KEY"),
            ProviderKind::Mimo => Some("MIMO_API_KEY"),
            ProviderKind::Mistral => Some("MISTRAL_API_KEY"),
            ProviderKind::Moonshot => Some("MOONSHOT_API_KEY"),
            ProviderKind::Stepfun => Some("STEPFUN_API_KEY"),
            ProviderKind::Gemini => Some("GEMINI_API_KEY"),
            ProviderKind::OpenAICodex => None,
            ProviderKind::GeminiCli => None,
        }
    }

    pub fn auth_mode(&self) -> ProviderAuthMode {
        match self {
            ProviderKind::Anthropic => ProviderAuthMode::ApiKey,
            ProviderKind::ClaudeCli => ProviderAuthMode::OAuth,
            ProviderKind::OpenAICodex => ProviderAuthMode::OAuth,
            ProviderKind::GeminiCli => ProviderAuthMode::OAuth,
            ProviderKind::OpenAI => ProviderAuthMode::ApiKey,
            ProviderKind::OpenRouter => ProviderAuthMode::ApiKey,
            ProviderKind::Mimo => ProviderAuthMode::ApiKey,
            ProviderKind::Mistral => ProviderAuthMode::ApiKey,
            ProviderKind::Moonshot => ProviderAuthMode::ApiKey,
            ProviderKind::Stepfun => ProviderAuthMode::ApiKey,
            ProviderKind::Gemini => ProviderAuthMode::ApiKey,
        }
    }
}

/// Resolves provider and model from a model identifier.
///
/// Supports explicit prefix format: `provider:model` or `provider/model`
/// Without prefix, defaults to Anthropic.
pub fn resolve_provider(model: &str) -> ProviderSelection {
    let trimmed = model.trim();

    // Check for explicit provider prefix (e.g., "mistral:devstral-2512")
    if let Some((kind, rest)) = parse_provider_prefix(trimmed)
        && !rest.is_empty()
    {
        return ProviderSelection {
            kind,
            model: rest.to_string(),
        };
    }

    // No prefix - default to Anthropic
    ProviderSelection {
        kind: ProviderKind::Anthropic,
        model: trimmed.to_string(),
    }
}

/// Infers the provider from a model identifier.
pub fn provider_for_model(model: &str) -> ProviderKind {
    resolve_provider(model).kind
}

/// Returns the ProviderKind for a provider id string (e.g., "anthropic", "openai").
pub fn provider_kind_from_id(id: &str) -> Option<ProviderKind> {
    ProviderKind::from_id(id)
}

fn parse_provider_prefix(model: &str) -> Option<(ProviderKind, &str)> {
    let separators = [':', '/'];
    for sep in separators {
        if let Some((prefix, rest)) = model.split_once(sep) {
            let prefix = prefix.trim().to_lowercase();
            let rest = rest.trim();
            let kind = match prefix.as_str() {
                "anthropic" | "claude" => ProviderKind::Anthropic,
                "claude-cli" => ProviderKind::ClaudeCli,
                "openai" | "openai-api" => ProviderKind::OpenAI,
                "openrouter" => ProviderKind::OpenRouter,
                "mimo" => ProviderKind::Mimo,
                "mistral" => ProviderKind::Mistral,
                "moonshot" | "kimi" => ProviderKind::Moonshot,
                "stepfun" => ProviderKind::Stepfun,
                "gemini" | "google" => ProviderKind::Gemini,
                "gemini-cli" | "google-gemini-cli" => ProviderKind::GeminiCli,
                "codex" | "openai-codex" => ProviderKind::OpenAICodex,
                _ => continue,
            };
            return Some((kind, rest));
        }
    }
    None
}
