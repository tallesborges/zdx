//! LLM provider implementations.

mod debug_metrics;
mod debug_trace;
pub mod text_tool_parser;
pub mod thinking_parser;

pub mod anthropic;
pub mod deepseek;
pub mod gemini;
pub mod lmstudio;
pub mod minimax;
pub mod mistral;
pub mod moonshot;
pub mod oauth;
pub mod openai;
pub mod opencode_go;
pub mod openrouter;
pub mod shared;
pub mod stepfun;
pub mod xai;
pub mod xiaomi;
pub mod xiaomi_plan;
pub mod zai;

use std::future::Future;
use std::pin::Pin;

use crate::anthropic::types::EffortLevel as AnthropicEffortLevel;
use crate::gemini::shared::GeminiThinkingConfig;
pub use debug_trace::{DebugTrace, TraceStream, wrap_stream};
pub use shared::{
    ChatContentBlock, ChatMessage, ContentBlockType, IdOrigin, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, ReasoningBlock, ReplayToken,
    SignatureProvider, StreamEvent, Usage, UsageDelta, error_message_from_payload,
    map_event_stream_error, resolve_api_key, resolve_base_url,
};
use zdx_types::ToolDefinition;
use zdx_types::config::{TextVerbosity, ThinkingLevel};

/// Object-safe trait for streaming LLM providers.
///
/// All provider clients implement this so the engine can hold a
/// `Box<dyn StreamingProvider>` instead of an enum with a match arm per
/// provider. The method mirrors the existing `send_messages_stream` on each
/// concrete client; the trait just adds a boxed-future shim so the call works
/// through dynamic dispatch.
pub trait StreamingProvider: Send + Sync {
    /// Streams a completion request, returning a boxed stream of events.
    fn stream_messages<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
        system: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<ProviderStream>> + Send + 'a>>;
}

macro_rules! impl_streaming_provider {
    ($($t:ty),* $(,)?) => {
        $(
            impl StreamingProvider for $t {
                fn stream_messages<'a>(
                    &'a self,
                    messages: &'a [ChatMessage],
                    tools: &'a [ToolDefinition],
                    system: Option<&'a str>,
                ) -> Pin<Box<dyn Future<Output = anyhow::Result<ProviderStream>> + Send + 'a>> {
                    Box::pin(self.send_messages_stream(messages, tools, system))
                }
            }
        )*
    };
}

impl_streaming_provider!(
    anthropic::api::AnthropicClient,
    anthropic::cli::ClaudeCliClient,
    openai::api::OpenAIClient,
    openai::codex::OpenAICodexClient,
    openai::chat_completions::OpenAIChatCompletionsClient,
    openai::responses_ws::OpenAIResponsesWsClient,
    openrouter::OpenRouterClient,
    deepseek::DeepSeekClient,
    gemini::api::GeminiClient,
    gemini::cli::GeminiCliClient,
    gemini::antigravity::AntigravityClient,
    xiaomi::XiaomiClient,
    xiaomi_plan::XiaomiPlanClient,
    mistral::MistralClient,
    moonshot::MoonshotClient,
    stepfun::StepfunClient,
    lmstudio::LMStudioClient,
    minimax::MinimaxClient,
    zai::ZaiClient,
    xai::XaiClient,
    opencode_go::OpencodeGoClient,
);

/// Blanket impl so `Box<dyn StreamingProvider>` is itself a `StreamingProvider`.
/// This lets callers pass `&Box<dyn StreamingProvider>` where `&dyn StreamingProvider`
/// is expected without explicit dereferencing.
impl<T: StreamingProvider + ?Sized> StreamingProvider for Box<T> {
    fn stream_messages<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolDefinition],
        system: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<ProviderStream>> + Send + 'a>> {
        (**self).stream_messages(messages, tools, system)
    }
}

/// Consolidated context for provider client construction.
///
/// Carries both raw inputs (model, provider kind) and derived values
/// (thinking, reasoning, cache key) plus resolved per-provider config
/// values (`base_url`, `api_key`, `websocket`, `text_verbosity`).
///
/// The engine constructs this by resolving its `Config` and then calls
/// `ProviderKind::build_client(&ctx)`. This crate never needs to reference
/// the engine's `Config` type.
pub struct ProviderBuildContext<'a> {
    pub model: &'a str,
    pub provider: ProviderKind,
    /// Effective max tokens (`u32`) — used by `Anthropic`, `ClaudeCli`, `OpenAICodex`, `Xiaomi`.
    pub max_tokens: u32,
    /// Global `config.max_tokens` (`Option<u32>`) — used by `OpenAI`, `OpenRouter`, `Gemini`, etc.
    pub config_max_tokens: Option<u32>,
    pub thinking_level: ThinkingLevel,
    pub thinking_enabled: bool,
    pub reasoning_effort: Option<String>,
    pub anthropic_effort: Option<AnthropicEffortLevel>,
    pub thinking_budget_tokens: u32,
    pub gemini_thinking: Option<GeminiThinkingConfig>,
    pub cache_key: Option<String>,
    pub text_verbosity: Option<TextVerbosity>,
    pub service_tier: Option<String>,
    /// Resolved per-provider base URL override.
    pub base_url: Option<&'a str>,
    /// Resolved per-provider API key.
    pub api_key: Option<&'a str>,
    /// Per-provider text verbosity default.
    pub provider_text_verbosity: Option<TextVerbosity>,
    /// Per-provider websocket flag (`OpenAI`/`OpenAICodex`).
    pub websocket: bool,
    /// API routing hint for the `opencode-go` meta-provider.
    pub api_hint: Option<String>,
}

impl<'a> ProviderBuildContext<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: &'a str,
        provider: ProviderKind,
        max_tokens: u32,
        config_max_tokens: Option<u32>,
        thinking_level: ThinkingLevel,
        text_verbosity: Option<TextVerbosity>,
        model_output_limit: Option<u32>,
        thread_id: Option<&'a str>,
        service_tier: Option<&'a str>,
        base_url: Option<&'a str>,
        api_key: Option<&'a str>,
        provider_text_verbosity: Option<TextVerbosity>,
        websocket: bool,
        api_hint: Option<String>,
    ) -> Self {
        let thinking_enabled = thinking_level.is_enabled();
        let reasoning_effort = map_thinking_to_reasoning(thinking_level);
        let anthropic_effort = map_thinking_to_anthropic_effort(thinking_level, model);
        let thinking_budget_tokens = thinking_level
            .compute_reasoning_budget(max_tokens, model_output_limit)
            .unwrap_or(0);
        // Always emit a Gemini thinking config — even when ThinkingLevel::Off — so that
        // `Off` sends an explicit minimum-thinking config rather than omitting
        // `thinkingConfig` (which lets Gemini fall back to its default high reasoning).
        let gemini_thinking = Some(GeminiThinkingConfig::from_thinking_level(
            thinking_level,
            model,
        ));

        Self {
            model,
            provider,
            max_tokens,
            config_max_tokens,
            thinking_level,
            thinking_enabled,
            reasoning_effort,
            anthropic_effort,
            thinking_budget_tokens,
            gemini_thinking,
            cache_key: thread_id.map(str::to_owned),
            text_verbosity,
            service_tier: service_tier.map(str::to_owned),
            base_url,
            api_key,
            provider_text_verbosity,
            websocket,
            api_hint,
        }
    }
}

fn map_thinking_to_reasoning(level: ThinkingLevel) -> Option<String> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal | ThinkingLevel::Low => Some("low".to_string()),
        ThinkingLevel::Medium => Some("medium".to_string()),
        ThinkingLevel::High => Some("high".to_string()),
        ThinkingLevel::XHigh => Some("xhigh".to_string()),
    }
}

pub fn map_thinking_to_anthropic_effort(
    level: ThinkingLevel,
    model: &str,
) -> Option<AnthropicEffortLevel> {
    if matches!(level, ThinkingLevel::Off) {
        return None;
    }

    let normalized = model.rsplit(':').next().unwrap_or(model);

    if normalized.starts_with("claude-opus-4-6")
        || normalized.starts_with("claude-sonnet-4-6")
        || normalized.starts_with("claude-opus-4-5")
    {
        return Some(match level {
            ThinkingLevel::Off => unreachable!(),
            ThinkingLevel::Minimal | ThinkingLevel::Low => AnthropicEffortLevel::Low,
            ThinkingLevel::Medium => AnthropicEffortLevel::Medium,
            ThinkingLevel::High => AnthropicEffortLevel::High,
            ThinkingLevel::XHigh => AnthropicEffortLevel::Max,
        });
    }

    Some(match level {
        ThinkingLevel::Off => unreachable!(),
        ThinkingLevel::Minimal => AnthropicEffortLevel::Low,
        ThinkingLevel::Low => AnthropicEffortLevel::Medium,
        ThinkingLevel::Medium => AnthropicEffortLevel::High,
        ThinkingLevel::High => AnthropicEffortLevel::XHigh,
        ThinkingLevel::XHigh => AnthropicEffortLevel::Max,
    })
}

pub fn resolve_text_verbosity(
    runtime_override: Option<TextVerbosity>,
    provider_default: Option<TextVerbosity>,
) -> Option<TextVerbosity> {
    runtime_override.or(provider_default)
}

/// Provider selection based on model naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    ClaudeCli,
    OpenAICodex,
    OpenAI,
    OpenRouter,
    DeepSeek,
    Xiaomi,
    XiaomiPlan,
    Mistral,
    Moonshot,
    Stepfun,
    LMStudio,
    Gemini,
    GeminiCli,
    GoogleAntigravity,
    OpencodeGo,
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

/// Static metadata for a single provider kind.
struct ProviderMeta {
    id: &'static str,
    aliases: &'static [&'static str],
    label: &'static str,
    api_key_env: Option<&'static str>,
    base_url: &'static str,
    base_url_env: Option<&'static str>,
    supports_oauth: bool,
    is_subscription: bool,
}

impl ProviderKind {
    /// Returns the static metadata for this provider.
    #[allow(clippy::too_many_lines)]
    const fn meta(self) -> ProviderMeta {
        match self {
            Self::Anthropic => ProviderMeta {
                id: "anthropic",
                aliases: &["claude"],
                label: "Anthropic",
                api_key_env: Some("ANTHROPIC_API_KEY"),
                base_url: "https://api.anthropic.com",
                base_url_env: Some("ANTHROPIC_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::ClaudeCli => ProviderMeta {
                id: "claude-cli",
                aliases: &[],
                label: "Claude CLI",
                api_key_env: None,
                base_url: "https://api.anthropic.com",
                base_url_env: Some("ANTHROPIC_BASE_URL"),
                supports_oauth: true,
                is_subscription: true,
            },
            Self::OpenAICodex => ProviderMeta {
                id: "openai-codex",
                aliases: &["codex"],
                label: "OpenAI Codex",
                api_key_env: None,
                base_url: "https://chatgpt.com/backend-api",
                base_url_env: None,
                supports_oauth: true,
                is_subscription: true,
            },
            Self::OpenAI => ProviderMeta {
                id: "openai",
                aliases: &["openai-api"],
                label: "OpenAI",
                api_key_env: Some("OPENAI_API_KEY"),
                base_url: "https://api.openai.com/v1",
                base_url_env: Some("OPENAI_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::OpenRouter => ProviderMeta {
                id: "openrouter",
                aliases: &[],
                label: "OpenRouter",
                api_key_env: Some("OPENROUTER_API_KEY"),
                base_url: "https://openrouter.ai/api/v1",
                base_url_env: Some("OPENROUTER_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::DeepSeek => ProviderMeta {
                id: "deepseek",
                aliases: &[],
                label: "DeepSeek",
                api_key_env: Some("DEEPSEEK_API_KEY"),
                base_url: "https://api.deepseek.com",
                base_url_env: Some("DEEPSEEK_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::Xiaomi => ProviderMeta {
                id: "xiaomi",
                aliases: &[],
                label: "Xiaomi MiMo",
                api_key_env: Some("XIAOMI_API_KEY"),
                base_url: "https://api.xiaomimimo.com/v1",
                base_url_env: Some("XIAOMI_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::XiaomiPlan => ProviderMeta {
                id: "xiaomi-plan",
                aliases: &["mimo-plan"],
                label: "Xiaomi MiMo Plan",
                api_key_env: Some("XIAOMI_PLAN_API_KEY"),
                base_url: "https://token-plan-sgp.xiaomimimo.com/v1",
                base_url_env: Some("XIAOMI_PLAN_BASE_URL"),
                supports_oauth: false,
                is_subscription: true,
            },
            Self::Mistral => ProviderMeta {
                id: "mistral",
                aliases: &[],
                label: "Mistral",
                api_key_env: Some("MISTRAL_API_KEY"),
                base_url: "https://api.mistral.ai/v1",
                base_url_env: Some("MISTRAL_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::Moonshot => ProviderMeta {
                id: "moonshot",
                aliases: &["kimi"],
                label: "Moonshot",
                api_key_env: Some("MOONSHOT_API_KEY"),
                base_url: "https://api.moonshot.ai/v1",
                base_url_env: Some("MOONSHOT_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::Stepfun => ProviderMeta {
                id: "stepfun",
                aliases: &[],
                label: "StepFun",
                api_key_env: Some("STEPFUN_API_KEY"),
                base_url: "https://api.stepfun.ai/v1",
                base_url_env: Some("STEPFUN_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::LMStudio => ProviderMeta {
                id: "lmstudio",
                aliases: &[],
                label: "LMStudio",
                api_key_env: None,
                base_url: "http://127.0.0.1:1234/v1",
                base_url_env: Some("LMSTUDIO_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::Gemini => ProviderMeta {
                id: "gemini",
                aliases: &["google"],
                label: "Gemini",
                api_key_env: Some("GEMINI_API_KEY"),
                base_url: "https://generativelanguage.googleapis.com/v1beta",
                base_url_env: Some("GEMINI_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::GeminiCli => ProviderMeta {
                id: "gemini-cli",
                aliases: &["google-gemini-cli"],
                label: "Gemini CLI",
                api_key_env: None,
                base_url: "https://generativelanguage.googleapis.com/v1beta",
                base_url_env: Some("GEMINI_BASE_URL"),
                supports_oauth: true,
                is_subscription: true,
            },
            Self::GoogleAntigravity => ProviderMeta {
                id: "google-antigravity",
                aliases: &["antigravity"],
                label: "Google Antigravity",
                api_key_env: None,
                base_url: "https://daily-cloudcode-pa.googleapis.com",
                base_url_env: None,
                supports_oauth: true,
                is_subscription: true,
            },
            Self::OpencodeGo => ProviderMeta {
                id: "opencode-go",
                aliases: &["opencode", "go"],
                label: "OpenCode Go",
                api_key_env: Some("OPENCODE_API_KEY"),
                base_url: "https://opencode.ai/zen/go",
                base_url_env: Some("OPENCODE_GO_BASE_URL"),
                supports_oauth: false,
                is_subscription: true,
            },
            Self::Minimax => ProviderMeta {
                id: "minimax",
                aliases: &[],
                label: "MiniMax",
                api_key_env: Some("MINIMAX_API_KEY"),
                base_url: "https://api.minimax.io/v1",
                base_url_env: Some("MINIMAX_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::Zai => ProviderMeta {
                id: "zai",
                aliases: &["zhipu", "glm"],
                label: "Z.AI",
                api_key_env: Some("ZAI_API_KEY"),
                base_url: "https://api.z.ai/api/paas/v4",
                base_url_env: Some("ZAI_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
            Self::Xai => ProviderMeta {
                id: "xai",
                aliases: &["grok", "x"],
                label: "xAI",
                api_key_env: Some("XAI_API_KEY"),
                base_url: "https://api.x.ai/v1",
                base_url_env: Some("XAI_BASE_URL"),
                supports_oauth: false,
                is_subscription: false,
            },
        }
    }

    /// Returns all provider kinds.
    pub fn all() -> &'static [ProviderKind] {
        &[
            ProviderKind::Anthropic,
            ProviderKind::ClaudeCli,
            ProviderKind::OpenAICodex,
            ProviderKind::OpenAI,
            ProviderKind::OpenRouter,
            ProviderKind::DeepSeek,
            ProviderKind::Xiaomi,
            ProviderKind::XiaomiPlan,
            ProviderKind::Mistral,
            ProviderKind::Moonshot,
            ProviderKind::Stepfun,
            ProviderKind::LMStudio,
            ProviderKind::Gemini,
            ProviderKind::GeminiCli,
            ProviderKind::GoogleAntigravity,
            ProviderKind::OpencodeGo,
            ProviderKind::Minimax,
            ProviderKind::Zai,
            ProviderKind::Xai,
        ]
    }

    /// Returns the string identifier used in config files and model registry.
    pub fn id(self) -> &'static str {
        self.meta().id
    }

    /// Returns the `ProviderKind` for a given id string.
    pub fn from_id(id: &str) -> Option<ProviderKind> {
        let lower = id.to_lowercase();
        for kind in Self::all() {
            let meta = kind.meta();
            if meta.id == lower || meta.aliases.contains(&lower.as_str()) {
                return Some(*kind);
            }
        }
        None
    }

    /// Returns the human-readable label for display.
    pub fn label(self) -> &'static str {
        self.meta().label
    }

    pub fn supports_oauth(self) -> bool {
        self.meta().supports_oauth
    }

    /// Returns true if this provider is subscription-based (usage included in subscription).
    pub fn is_subscription(self) -> bool {
        self.meta().is_subscription
    }

    pub fn api_key_env_var(self) -> Option<&'static str> {
        self.meta().api_key_env
    }

    /// Returns the default base URL for this provider's API.
    pub fn default_base_url(self) -> &'static str {
        self.meta().base_url
    }

    /// Returns the environment variable name for the base URL override.
    pub fn base_url_env_var(self) -> Option<&'static str> {
        self.meta().base_url_env
    }

    /// Resolves the base URL: env var > config > default.
    ///
    /// # Errors
    /// Returns an error if the resolved URL is invalid.
    pub fn resolve_base_url(self, config_base_url: Option<&str>) -> anyhow::Result<String> {
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
    pub fn resolve_api_key(self, config_api_key: Option<&str>) -> anyhow::Result<String> {
        shared::resolve_api_key(
            config_api_key,
            self.api_key_env_var().unwrap_or_default(),
            self.id(),
        )
    }

    pub fn auth_mode(self) -> ProviderAuthMode {
        if self.supports_oauth() {
            ProviderAuthMode::OAuth
        } else {
            ProviderAuthMode::ApiKey
        }
    }

    /// Builds a provider client from the given context.
    ///
    /// Thin dispatcher that delegates to each provider module's `build()` function.
    /// Adding a new provider: implement the client + `StreamingProvider` trait,
    /// add a `build()` function in the module, add a `ProviderKind` variant +
    /// `ProviderMeta` entry, then add one match arm here.
    ///
    /// # Errors
    /// Returns an error if the selected provider's configuration cannot be resolved.
    pub fn build_client(
        &self,
        ctx: &ProviderBuildContext<'_>,
    ) -> anyhow::Result<Box<dyn StreamingProvider>> {
        match self {
            Self::Anthropic => anthropic::api::build(ctx),
            Self::ClaudeCli => anthropic::cli::build(ctx),
            Self::OpenAICodex => openai::codex::build(ctx),
            Self::OpenAI => openai::api::build(ctx),
            Self::OpenRouter => openrouter::build(ctx),
            Self::DeepSeek => deepseek::build(ctx),
            Self::Xiaomi => xiaomi::build(ctx),
            Self::XiaomiPlan => xiaomi_plan::build(ctx),
            Self::Mistral => mistral::build(ctx),
            Self::Moonshot => moonshot::build(ctx),
            Self::Stepfun => stepfun::build(ctx),
            Self::LMStudio => lmstudio::build(ctx),
            Self::Gemini => gemini::api::build(ctx),
            Self::GeminiCli => gemini::cli::build(ctx),
            Self::GoogleAntigravity => gemini::antigravity::build(ctx),
            Self::OpencodeGo => opencode_go::build(ctx),
            Self::Minimax => minimax::build(ctx),
            Self::Zai => zai::build(ctx),
            Self::Xai => xai::build(ctx),
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
    for sep in [':', '/'] {
        if let Some((prefix, rest)) = model.split_once(sep) {
            let prefix = prefix.trim();
            let rest = rest.trim();
            if let Some(kind) = ProviderKind::from_id(prefix) {
                return Some((kind, rest));
            }
        }
    }
    None
}
