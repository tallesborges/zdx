//! LLM provider implementations.

mod debug_metrics;

pub mod anthropic;
pub mod claude_cli;
pub mod gemini;
pub mod gemini_cli;
pub mod gemini_shared;
pub mod moonshot;
pub mod oauth;
pub mod openai;
pub mod openrouter;
pub mod shared;

pub use shared::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, ReasoningBlock, ReplayToken, StreamEvent,
    Usage,
};

/// Provider selection based on model naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    ClaudeCli,
    OpenAICodex,
    OpenAI,
    OpenRouter,
    Moonshot,
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
            ProviderKind::Moonshot,
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
            ProviderKind::Moonshot => "moonshot",
            ProviderKind::Gemini => "gemini",
            ProviderKind::GeminiCli => "gemini-cli",
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
            ProviderKind::Moonshot => "Moonshot",
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
            ProviderKind::Moonshot => Some("MOONSHOT_API_KEY"),
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
            ProviderKind::Moonshot => ProviderAuthMode::ApiKey,
            ProviderKind::Gemini => ProviderAuthMode::ApiKey,
        }
    }
}

/// Infers the provider and normalized model from a model identifier.
pub fn resolve_provider(model: &str) -> ProviderSelection {
    let trimmed = model.trim();
    let lower = trimmed.to_lowercase();

    if let Some((kind, rest)) = parse_provider_prefix(trimmed)
        && !rest.is_empty()
    {
        return ProviderSelection {
            kind,
            model: rest.to_string(),
        };
    }

    let kind = if lower.contains("codex") {
        ProviderKind::OpenAICodex
    } else if lower.starts_with("gpt-")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
    {
        ProviderKind::OpenAI
    } else if lower.starts_with("kimi-") || lower.starts_with("moonshot-") {
        ProviderKind::Moonshot
    } else if lower.starts_with("gemini") {
        ProviderKind::Gemini
    } else if lower.starts_with("claude-cli") {
        ProviderKind::ClaudeCli
    } else {
        ProviderKind::Anthropic
    };

    ProviderSelection {
        kind,
        model: trimmed.to_string(),
    }
}

/// Infers the provider from a model identifier.
pub fn provider_for_model(model: &str) -> ProviderKind {
    resolve_provider(model).kind
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
                "moonshot" | "kimi" => ProviderKind::Moonshot,
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
