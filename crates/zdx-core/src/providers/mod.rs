//! LLM provider implementations.

mod debug_metrics;
mod debug_trace;
pub mod text_tool_parser;
pub mod thinking_parser;

pub mod anthropic;
pub mod apiyi;
pub mod gemini;
pub mod minimax;
pub mod mistral;
pub mod moonshot;
pub mod oauth;
pub mod openai;
pub mod openrouter;
pub mod shared;
pub mod stepfun;
pub mod xai;
pub mod xiaomi;
pub mod zai;
pub mod zen;

pub use debug_trace::{DebugTrace, TraceStream, wrap_stream};
pub use shared::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, ReasoningBlock, ReplayToken,
    SignatureProvider, StreamEvent, Usage, resolve_api_key, resolve_base_url,
};

/// Provider selection based on model naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    ClaudeCli,
    OpenAICodex,
    OpenAI,
    OpenRouter,
    Xiomi,
    Mistral,
    Moonshot,
    Stepfun,
    Gemini,
    GeminiCli,
    Zen,
    Apiyi,
    Minimax,
    Zai,
    Xai,
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
            ProviderKind::Xiomi,
            ProviderKind::Mistral,
            ProviderKind::Moonshot,
            ProviderKind::Stepfun,
            ProviderKind::Gemini,
            ProviderKind::GeminiCli,
            ProviderKind::Zen,
            ProviderKind::Apiyi,
            ProviderKind::Minimax,
            ProviderKind::Zai,
            ProviderKind::Xai,
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
            ProviderKind::Xiomi => "xiaomi",
            ProviderKind::Mistral => "mistral",
            ProviderKind::Moonshot => "moonshot",
            ProviderKind::Stepfun => "stepfun",
            ProviderKind::Gemini => "gemini",
            ProviderKind::GeminiCli => "gemini-cli",
            ProviderKind::Zen => "zen",
            ProviderKind::Apiyi => "apiyi",
            ProviderKind::Minimax => "minimax",
            ProviderKind::Zai => "zai",
            ProviderKind::Xai => "xai",
        }
    }

    /// Returns the `ProviderKind` for a given id string.
    pub fn from_id(id: &str) -> Option<ProviderKind> {
        match id.to_lowercase().as_str() {
            "anthropic" => Some(ProviderKind::Anthropic),
            "claude-cli" => Some(ProviderKind::ClaudeCli),
            "openai-codex" | "codex" => Some(ProviderKind::OpenAICodex),
            "openai" => Some(ProviderKind::OpenAI),
            "openrouter" => Some(ProviderKind::OpenRouter),
            "xiaomi" => Some(ProviderKind::Xiomi),
            "mistral" => Some(ProviderKind::Mistral),
            "moonshot" => Some(ProviderKind::Moonshot),
            "stepfun" => Some(ProviderKind::Stepfun),
            "gemini" => Some(ProviderKind::Gemini),
            "gemini-cli" => Some(ProviderKind::GeminiCli),
            "zen" => Some(ProviderKind::Zen),
            "apiyi" => Some(ProviderKind::Apiyi),
            "minimax" => Some(ProviderKind::Minimax),
            "zai" | "zhipu" | "glm" => Some(ProviderKind::Zai),
            "xai" | "grok" | "x" => Some(ProviderKind::Xai),
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
            ProviderKind::Xiomi => "Xiomi",
            ProviderKind::Mistral => "Mistral",
            ProviderKind::Moonshot => "Moonshot",
            ProviderKind::Stepfun => "StepFun",
            ProviderKind::Gemini => "Gemini",
            ProviderKind::GeminiCli => "Gemini CLI",
            ProviderKind::Zen => "Zen",
            ProviderKind::Apiyi => "APIYI",
            ProviderKind::Minimax => "MiniMax",
            ProviderKind::Zai => "Z.AI",
            ProviderKind::Xai => "xAI",
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
            ProviderKind::OpenAI => Some("OPENAI_API_KEY"),
            ProviderKind::OpenRouter => Some("OPENROUTER_API_KEY"),
            ProviderKind::Xiomi => Some("XIAOMI_API_KEY"),
            ProviderKind::Mistral => Some("MISTRAL_API_KEY"),
            ProviderKind::Moonshot => Some("MOONSHOT_API_KEY"),
            ProviderKind::Stepfun => Some("STEPFUN_API_KEY"),
            ProviderKind::Gemini => Some("GEMINI_API_KEY"),
            ProviderKind::Zen => Some("ZEN_API_KEY"),
            ProviderKind::Apiyi => Some("APIYI_API_KEY"),
            ProviderKind::Minimax => Some("MINIMAX_API_KEY"),
            ProviderKind::Zai => Some("ZAI_API_KEY"),
            ProviderKind::Xai => Some("XAI_API_KEY"),
            ProviderKind::ClaudeCli | ProviderKind::OpenAICodex | ProviderKind::GeminiCli => None,
        }
    }

    /// Returns the default base URL for this provider's API.
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Self::Anthropic | Self::ClaudeCli => "https://api.anthropic.com",
            Self::OpenAI => "https://api.openai.com/v1",
            Self::OpenAICodex => "https://chatgpt.com/backend-api",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Mistral => "https://api.mistral.ai/v1",
            Self::Moonshot => "https://api.moonshot.ai/v1",
            Self::Stepfun => "https://api.stepfun.ai/v1",
            Self::Gemini | Self::GeminiCli => "https://generativelanguage.googleapis.com/v1beta",
            Self::Xiomi => "https://api.xiaomimimo.com/v1",
            Self::Zen => "https://opencode.ai/zen",
            Self::Apiyi => "https://api.apiyi.com",
            Self::Minimax => "https://api.minimax.io/v1",
            Self::Zai => "https://api.z.ai/api/paas/v4",
            Self::Xai => "https://api.x.ai/v1",
        }
    }

    /// Returns the environment variable name for the base URL override.
    pub fn base_url_env_var(&self) -> Option<&'static str> {
        match self {
            Self::Anthropic | Self::ClaudeCli => Some("ANTHROPIC_BASE_URL"),
            Self::OpenAI => Some("OPENAI_BASE_URL"),
            Self::OpenAICodex => None,
            Self::OpenRouter => Some("OPENROUTER_BASE_URL"),
            Self::Mistral => Some("MISTRAL_BASE_URL"),
            Self::Moonshot => Some("MOONSHOT_BASE_URL"),
            Self::Stepfun => Some("STEPFUN_BASE_URL"),
            Self::Gemini | Self::GeminiCli => Some("GEMINI_BASE_URL"),
            Self::Xiomi => Some("XIAOMI_BASE_URL"),
            Self::Zen => Some("ZEN_BASE_URL"),
            Self::Apiyi => Some("APIYI_BASE_URL"),
            Self::Minimax => Some("MINIMAX_BASE_URL"),
            Self::Zai => Some("ZAI_BASE_URL"),
            Self::Xai => Some("XAI_BASE_URL"),
        }
    }

    /// Resolves the base URL: env var > config > default.
    ///
    /// # Errors
    /// Returns an error if the resolved URL is invalid.
    pub fn resolve_base_url(&self, config_base_url: Option<&str>) -> anyhow::Result<String> {
        shared::resolve_base_url(
            config_base_url,
            self.base_url_env_var().unwrap_or_default(),
            self.default_base_url(),
            self.label(),
        )
    }

    /// Resolves the API key: config > env var.
    ///
    /// # Errors
    /// Returns an error if no API key is found.
    pub fn resolve_api_key(&self, config_api_key: Option<&str>) -> anyhow::Result<String> {
        shared::resolve_api_key(
            config_api_key,
            self.api_key_env_var().unwrap_or_default(),
            self.id(),
        )
    }

    pub fn auth_mode(&self) -> ProviderAuthMode {
        match self {
            ProviderKind::ClaudeCli | ProviderKind::OpenAICodex | ProviderKind::GeminiCli => {
                ProviderAuthMode::OAuth
            }
            ProviderKind::Anthropic
            | ProviderKind::OpenAI
            | ProviderKind::OpenRouter
            | ProviderKind::Xiomi
            | ProviderKind::Mistral
            | ProviderKind::Moonshot
            | ProviderKind::Stepfun
            | ProviderKind::Gemini
            | ProviderKind::Zen
            | ProviderKind::Apiyi
            | ProviderKind::Minimax
            | ProviderKind::Zai
            | ProviderKind::Xai => ProviderAuthMode::ApiKey,
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

/// Returns the `ProviderKind` for a provider id string (e.g., "anthropic", "openai").
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
                "xiaomi" => ProviderKind::Xiomi,
                "mistral" => ProviderKind::Mistral,
                "moonshot" | "kimi" => ProviderKind::Moonshot,
                "stepfun" => ProviderKind::Stepfun,
                "gemini" | "google" => ProviderKind::Gemini,
                "gemini-cli" | "google-gemini-cli" => ProviderKind::GeminiCli,
                "codex" | "openai-codex" => ProviderKind::OpenAICodex,
                "zen" | "opencode" => ProviderKind::Zen,
                "apiyi" => ProviderKind::Apiyi,
                "minimax" => ProviderKind::Minimax,
                "zai" | "zhipu" | "glm" => ProviderKind::Zai,
                "xai" | "grok" | "x" => ProviderKind::Xai,
                _ => continue,
            };
            return Some((kind, rest));
        }
    }
    None
}
