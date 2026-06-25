//! Provider client construction.
//!
//! Centralizes all provider client building logic so the engine
//! only needs to resolve config values and call `build_provider_client`.

use anyhow::Result;
use zdx_types::config::{TextVerbosity, ThinkingLevel};

use crate::anthropic::api::{AnthropicClient, AnthropicConfig};
use crate::anthropic::cli::{ClaudeCliClient, ClaudeCliConfig};
use crate::anthropic::types::EffortLevel as AnthropicEffortLevel;
use crate::deepseek::{DeepSeekClient, DeepSeekConfig};
use crate::gemini::antigravity::{AntigravityClient, AntigravityConfig};
use crate::gemini::api::{GeminiClient, GeminiConfig};
use crate::gemini::cli::{GeminiCliClient, GeminiCliConfig};
use crate::gemini::shared::GeminiThinkingConfig;
use crate::lmstudio::{LMStudioClient, LMStudioConfig};
use crate::minimax::{MinimaxClient, MinimaxConfig};
use crate::mistral::{MistralClient, MistralConfig};
use crate::moonshot::{MoonshotClient, MoonshotConfig};
use crate::openai::api::{OpenAIClient, OpenAIConfig};
use crate::openai::codex::{OpenAICodexClient, OpenAICodexConfig};
use crate::opencode_go::{OpencodeGoClient, OpencodeGoConfig};
use crate::openrouter::{OpenRouterClient, OpenRouterConfig};
use crate::stepfun::{StepfunClient, StepfunConfig};
use crate::xai::{XaiClient, XaiConfig};
use crate::xiaomi::{XiaomiClient, XiaomiConfig};
use crate::xiaomi_plan::{XiaomiPlanClient, XiaomiPlanConfig};
use crate::zai::{ZaiClient, ZaiConfig};
use crate::{ProviderKind, StreamingProvider};

/// Consolidated context for provider client construction.
///
/// Carries both raw inputs (model, provider kind) and derived values
/// (thinking, reasoning, cache key) plus resolved per-provider config
/// values (`base_url`, `api_key`, `websocket`, `text_verbosity`).
///
/// The engine constructs this by resolving its `Config` and then calls
/// `build_provider_client(&ctx)`. This crate never needs to reference
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

/// Builds a provider client from the given context.
///
/// # Errors
/// Returns an error if provider configuration (API key, base URL) cannot be resolved.
pub fn build_provider_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    match ctx.provider {
        ProviderKind::Anthropic => build_anthropic_client(ctx),
        ProviderKind::ClaudeCli => Ok(Box::new(ClaudeCliClient::new(ClaudeCliConfig::new(
            ctx.model.to_string(),
            ctx.max_tokens,
            ctx.base_url,
            ctx.thinking_enabled,
            ctx.thinking_budget_tokens,
            ctx.anthropic_effort,
        )))),
        ProviderKind::OpenAICodex => Ok(Box::new(OpenAICodexClient::new(OpenAICodexConfig::new(
            ctx.model.to_string(),
            ctx.max_tokens,
            ctx.reasoning_effort.clone(),
            resolve_text_verbosity(ctx.text_verbosity, ctx.provider_text_verbosity),
            ctx.cache_key.clone(),
            ctx.service_tier.clone(),
            ctx.websocket,
        )))),
        ProviderKind::OpenAI => build_openai_client(ctx),
        ProviderKind::OpenRouter => build_openrouter_client(ctx),
        ProviderKind::DeepSeek => build_deepseek_client(ctx),
        ProviderKind::Xiaomi => build_xiaomi_client(ctx),
        ProviderKind::XiaomiPlan => build_xiaomi_plan_client(ctx),
        ProviderKind::Mistral => build_mistral_client(ctx),
        ProviderKind::Moonshot => build_moonshot_client(ctx),
        ProviderKind::Stepfun => build_stepfun_client(ctx),
        ProviderKind::LMStudio => build_lmstudio_client(ctx),
        ProviderKind::Minimax => build_minimax_client(ctx),
        ProviderKind::Zai => build_zai_client(ctx),
        ProviderKind::Xai => build_xai_client(ctx),
        ProviderKind::Gemini => build_gemini_client(ctx),
        ProviderKind::GeminiCli => Ok(Box::new(GeminiCliClient::new(GeminiCliConfig::new(
            ctx.model.to_string(),
            ctx.config_max_tokens,
            GeminiThinkingConfig::from_thinking_level(ctx.thinking_level, ctx.model),
        )))),
        ProviderKind::GoogleAntigravity => {
            Ok(Box::new(AntigravityClient::new(AntigravityConfig::new(
                ctx.model.to_string(),
                ctx.config_max_tokens,
                Some(antigravity_thinking_config(
                    ctx.thinking_level,
                    ctx.model,
                    ctx.config_max_tokens,
                )),
            ))))
        }
        ProviderKind::OpencodeGo => build_opencode_go_client(ctx),
    }
}

fn antigravity_thinking_config(
    level: ThinkingLevel,
    model: &str,
    max_tokens: Option<u32>,
) -> GeminiThinkingConfig {
    let budget = if model.starts_with("claude-") {
        match level {
            ThinkingLevel::Off => 0,
            _ => 1024,
        }
    } else if model.starts_with("gpt-oss-") {
        match level {
            ThinkingLevel::Off => 0,
            _ => 4096,
        }
    } else {
        return GeminiThinkingConfig::from_thinking_level(level, model);
    };

    let capped_budget = max_tokens
        .and_then(|tokens| i32::try_from(tokens.saturating_sub(1)).ok())
        .map_or(budget, |limit| budget.min(limit));
    GeminiThinkingConfig::Budget(capped_budget.max(0))
}

fn build_anthropic_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(AnthropicClient::new(AnthropicConfig::from_env(
        ctx.model.to_string(),
        ctx.max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.thinking_enabled,
        ctx.thinking_budget_tokens,
        ctx.anthropic_effort,
    )?)))
}

fn build_openai_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(OpenAIClient::new(OpenAIConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.reasoning_effort.clone(),
        resolve_text_verbosity(ctx.text_verbosity, ctx.provider_text_verbosity),
        ctx.cache_key.clone(),
        ctx.service_tier.clone(),
        ctx.websocket,
    )?)))
}

pub fn resolve_text_verbosity(
    runtime_override: Option<TextVerbosity>,
    provider_default: Option<TextVerbosity>,
) -> Option<TextVerbosity> {
    runtime_override.or(provider_default)
}

fn build_openrouter_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(OpenRouterClient::new(OpenRouterConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.reasoning_effort.clone(),
        ctx.cache_key.clone(),
    )?)))
}

fn build_deepseek_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(DeepSeekClient::new(DeepSeekConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
        ctx.reasoning_effort.clone(),
    )?)))
}

fn build_xiaomi_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(XiaomiClient::new(XiaomiConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        None,
        ctx.thinking_enabled,
    )?)))
}

fn build_xiaomi_plan_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(XiaomiPlanClient::new(XiaomiPlanConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        None,
        ctx.thinking_enabled,
    )?)))
}

fn build_mistral_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(MistralClient::new(MistralConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
    )?)))
}

fn build_moonshot_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(MoonshotClient::new(MoonshotConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
    )?)))
}

fn build_stepfun_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(StepfunClient::new(StepfunConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
    )?)))
}

fn build_lmstudio_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(LMStudioClient::new(LMStudioConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
    )?)))
}

fn build_minimax_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(MinimaxClient::new(MinimaxConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
    )?)))
}

fn build_zai_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(ZaiClient::new(ZaiConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
    )?)))
}

fn build_xai_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(XaiClient::new(XaiConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_enabled,
    )?)))
}

fn build_gemini_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(GeminiClient::new(GeminiConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.gemini_thinking.clone(),
    )?)))
}

fn build_opencode_go_client(ctx: &ProviderBuildContext<'_>) -> Result<Box<dyn StreamingProvider>> {
    Ok(Box::new(OpencodeGoClient::new(OpencodeGoConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.thinking_enabled,
        ctx.thinking_budget_tokens,
        ctx.anthropic_effort,
        ctx.gemini_thinking.clone(),
        ctx.reasoning_effort.clone(),
        ctx.cache_key.clone(),
        ctx.api_hint.clone(),
    )?)))
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
