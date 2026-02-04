//! Configuration management for ZDX.
//!
//! Loads configuration from ${ZDX_HOME}/config.toml with sensible defaults.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Thinking level for extended thinking feature.
///
/// Controls how much reasoning Claude shows before responding.
/// Higher levels use more tokens but provide deeper reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// No reasoning (default)
    #[default]
    Off,
    /// Very brief reasoning (~10% of max tokens)
    Minimal,
    /// Light reasoning (~20% of max tokens)
    Low,
    /// Moderate reasoning (~50% of max tokens)
    Medium,
    /// Deep reasoning (~80% of max tokens)
    High,
    /// Very deep reasoning (~95% of max tokens)
    XHigh,
}

/// Skill discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub enable_zdx_user: bool,
    pub enable_zdx_project: bool,
    pub enable_codex_user: bool,
    pub enable_claude_user: bool,
    pub enable_claude_project: bool,
    pub enable_agents_user: bool,
    pub enable_agents_project: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_skills: Vec<String>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enable_zdx_user: true,
            enable_zdx_project: true,
            enable_codex_user: true,
            enable_claude_user: true,
            enable_claude_project: true,
            enable_agents_user: true,
            enable_agents_project: true,
            ignored_skills: Vec::new(),
            include_skills: Vec::new(),
        }
    }
}

/// Telegram bot configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    /// Bot token for Telegram API.
    pub bot_token: Option<String>,
    /// Allowlist of numeric Telegram user IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowlist_user_ids: Vec<i64>,
    /// Allowlist of numeric Telegram chat IDs (for groups/supergroups).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowlist_chat_ids: Vec<i64>,
}

impl ThinkingLevel {
    /// Returns the effort percentage of max tokens for this thinking level.
    /// Returns None for Off (thinking disabled).
    pub fn effort_percent(&self) -> Option<u32> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Minimal => Some(5),
            ThinkingLevel::Low => Some(20),
            ThinkingLevel::Medium => Some(50),
            ThinkingLevel::High => Some(80),
            ThinkingLevel::XHigh => Some(95),
        }
    }

    /// Returns the normalized effort label for this level.
    pub fn effort_label(&self) -> Option<&'static str> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Minimal => Some("minimal"),
            ThinkingLevel::Low => Some("low"),
            ThinkingLevel::Medium => Some("medium"),
            ThinkingLevel::High => Some("high"),
            ThinkingLevel::XHigh => Some("xhigh"),
        }
    }

    /// Returns whether thinking is enabled for this level.
    pub fn is_enabled(&self) -> bool {
        !matches!(self, ThinkingLevel::Off)
    }

    /// Returns a human-readable description of this thinking level.
    pub fn description(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "No reasoning",
            ThinkingLevel::Minimal => "Very brief (~5%)",
            ThinkingLevel::Low => "Light (~20%)",
            ThinkingLevel::Medium => "Moderate (~50%)",
            ThinkingLevel::High => "Deep (~80%)",
            ThinkingLevel::XHigh => "Very deep (~95%)",
        }
    }

    /// Returns the short display name for this level.
    pub fn display_name(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::XHigh => "xhigh",
        }
    }

    /// Returns all thinking levels for iteration (e.g., in picker).
    pub fn all() -> &'static [ThinkingLevel] {
        &[
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::XHigh,
        ]
    }

    /// Computes the reasoning budget in tokens based on effort percent and max_tokens.
    ///
    /// Uses min 1024 tokens to ensure meaningful reasoning.
    /// Returns None if thinking is Off.
    pub fn compute_reasoning_budget(
        &self,
        max_tokens: u32,
        model_output_limit: Option<u32>,
    ) -> Option<u32> {
        let percent = self.effort_percent()?;

        // Base for calculation: min of max_tokens and model output limit
        let base = match model_output_limit {
            Some(limit) if limit > 0 => max_tokens.min(limit),
            _ => max_tokens,
        };

        // Calculate raw budget from percentage
        let raw_budget = (base as u64 * percent as u64 / 100) as u32;

        // Ensure minimum budget for meaningful reasoning
        const MIN_BUDGET: u32 = 1024;
        Some(raw_budget.max(MIN_BUDGET))
    }
}

/// Returns the default config template with comments.
///
/// This is embedded from default_config.toml at compile time.
/// To update, edit default_config.toml directly.
fn default_config_template() -> &'static str {
    include_str!("../default_config.toml")
}

/// Merges user config values into the default template.
///
/// This ensures new comments/sections from the template are always present,
/// while preserving user's customized values.
fn merge_with_template(user_config: &str) -> Result<String> {
    use toml_edit::DocumentMut;

    // Parse the template as the base
    let mut doc: DocumentMut = default_config_template()
        .parse()
        .context("Failed to parse default config template")?;

    // Parse user's existing config
    let user_doc: DocumentMut = user_config.parse().context("Failed to parse user config")?;

    // Overlay user values onto template
    merge_items(doc.as_table_mut(), user_doc.as_table());

    Ok(doc.to_string())
}

/// Recursively merges items from source table into target table.
fn merge_items(target: &mut toml_edit::Table, source: &toml_edit::Table) {
    use toml_edit::Item;

    for (key, value) in source.iter() {
        match value {
            Item::Value(v) => {
                // Scalar value: override in target
                target[key] = Item::Value(v.clone());
            }
            Item::Table(src_table) => {
                // Nested table: recursively merge
                if let Some(Item::Table(target_table)) = target.get_mut(key) {
                    merge_items(target_table, src_table);
                } else {
                    // Target doesn't have this table, copy it
                    target[key] = Item::Table(src_table.clone());
                }
            }
            Item::ArrayOfTables(src_arr) => {
                // Array of tables: replace entirely with user's version
                target[key] = Item::ArrayOfTables(src_arr.clone());
            }
            Item::None => {}
        }
    }
}

pub mod paths {
    //! Path resolution for ZDX configuration and data directories.
    //!
    //! ZDX_HOME resolution order:
    //! 1. ZDX_HOME environment variable (if set)
    //! 2. ~/.config/zdx (default)

    use std::path::PathBuf;

    /// Returns the ZDX home directory.
    ///
    /// Checks ZDX_HOME env var first, falls back to ~/.config/zdx
    pub fn zdx_home() -> PathBuf {
        if let Ok(home) = std::env::var("ZDX_HOME") {
            return PathBuf::from(home);
        }

        dirs::home_dir()
            .map(|h| h.join(".config").join("zdx"))
            .expect("Could not determine home directory")
    }

    /// Returns the path to the config.toml file.
    pub fn config_path() -> PathBuf {
        zdx_home().join("config.toml")
    }

    /// Returns the path to the threads directory.
    pub fn threads_dir() -> PathBuf {
        zdx_home().join("threads")
    }
}

/// Default value for serde when handoff_model is missing.
fn default_handoff_model() -> String {
    Config::DEFAULT_HANDOFF_MODEL.to_string()
}

/// Default value for serde when title_model is missing.
fn default_title_model() -> String {
    Config::DEFAULT_TITLE_MODEL.to_string()
}

/// Transcription configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TranscriptionConfig {
    /// Transcription provider: "openai" (default) or "mistral"
    pub provider: Option<String>,
    /// Model to use for transcription (provider-specific)
    pub model: Option<String>,
    /// Language hint (ISO 639-1 code like "en", "pt", etc.)
    pub language: Option<String>,
}

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The Claude model to use
    pub model: String,

    /// Maximum tokens for responses (optional)
    pub max_tokens: Option<u32>,

    /// Optional inline system prompt
    pub system_prompt: Option<String>,

    /// Optional path to a file containing the system prompt
    pub system_prompt_file: Option<String>,

    /// Timeout for tool execution in seconds (0 disables)
    pub tool_timeout_secs: u32,

    /// Provider configuration (base URLs, etc.).
    #[serde(default)]
    pub providers: ProvidersConfig,

    /// Transcription configuration.
    #[serde(default)]
    pub transcription: TranscriptionConfig,

    /// Model to use for handoff generation subagent.
    #[serde(default = "default_handoff_model")]
    pub handoff_model: String,

    /// Model to use for auto-title generation subagent.
    #[serde(default = "default_title_model")]
    pub title_model: String,

    /// Thinking level for extended thinking feature
    #[serde(default)]
    pub thinking_level: ThinkingLevel,

    /// Skill discovery configuration
    #[serde(default)]
    pub skills: SkillsConfig,

    /// Telegram bot configuration
    #[serde(default)]
    pub telegram: TelegramConfig,
}

impl Config {
    const DEFAULT_MODEL: &str = "claude-haiku-4-5";
    const DEFAULT_MAX_TOKENS: u32 = 12288;
    /// Default is disabled
    const DEFAULT_TOOL_TIMEOUT_SECS: u32 = 0;
    const DEFAULT_HANDOFF_MODEL: &str = "gemini-cli:gemini-3-flash-preview";
    const DEFAULT_TITLE_MODEL: &str = "gemini-cli:gemini-2.5-flash";

    /// Loads configuration from the default config path.
    pub fn load() -> Result<Self> {
        Self::load_from(&paths::config_path())
    }

    /// Loads configuration from a specific path.
    /// Returns defaults if file doesn't exist.
    pub fn load_from(path: &Path) -> Result<Self> {
        if path.exists() {
            let contents = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            toml::from_str(&contents)
                .with_context(|| format!("Failed to parse config from {}", path.display()))
        } else {
            Ok(Config::default())
        }
    }

    /// Saves only the model field to the config file.
    ///
    /// Creates the file if it doesn't exist.
    /// Preserves existing fields and comments using toml_edit.
    pub fn save_model(model: &str) -> Result<()> {
        Self::save_model_to(&paths::config_path(), model)
    }

    /// Saves only the model field to a specific config file path.
    ///
    /// Creates the file with default template if it doesn't exist.
    /// If file exists, merges user values into the latest template.
    pub fn save_model_to(path: &Path, model: &str) -> Result<()> {
        use toml_edit::{DocumentMut, value};

        // Start from template, merge user values if file exists
        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        // Parse as editable document
        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        // Update model field
        doc["model"] = value(model);

        Self::write_config(path, &doc.to_string())
    }

    /// Saves only the thinking_level field to the config file.
    ///
    /// Creates the file if it doesn't exist.
    /// Preserves existing fields and comments using toml_edit.
    pub fn save_thinking_level(level: ThinkingLevel) -> Result<()> {
        Self::save_thinking_level_to(&paths::config_path(), level)
    }

    /// Saves only the thinking_level field to a specific config file path.
    ///
    /// Creates the file with default template if it doesn't exist.
    /// If file exists, merges user values into the latest template.
    pub fn save_thinking_level_to(path: &Path, level: ThinkingLevel) -> Result<()> {
        use toml_edit::{DocumentMut, value};

        // Start from template, merge user values if file exists
        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        // Parse as editable document
        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        // Update thinking_level field
        doc["thinking_level"] = value(level.display_name());

        Self::write_config(path, &doc.to_string())
    }

    /// Returns the effective system prompt, preferring the file if both are set.
    pub fn effective_system_prompt(&self) -> Result<Option<String>> {
        if let Some(path_str) = &self.system_prompt_file {
            let path = Path::new(path_str);
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read system prompt file: {}", path_str))?;
            let trimmed = content.trim();
            return Ok((!trimmed.is_empty()).then(|| trimmed.to_string()));
        }

        let trimmed = self.system_prompt.as_deref().unwrap_or("").trim();
        Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
    }

    pub fn tool_timeout(&self) -> Option<Duration> {
        if self.tool_timeout_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(self.tool_timeout_secs as u64))
        }
    }

    /// Returns the path to the models file.
    /// Defaults to `<base>/models.toml`.
    pub fn models_path(&self) -> std::path::PathBuf {
        let base = paths::zdx_home();
        base.join("models.toml")
    }

    /// Returns the effective max_tokens to use in API requests for a model.
    ///
    /// Resolution order:
    /// 1) Explicit config max_tokens (if set)
    /// 2) Model output limit from the registry (exclusive, minus 1)
    /// 3) Fallback default
    pub fn effective_max_tokens_for(&self, model_id: &str) -> u32 {
        let configured = self.max_tokens;
        let output_limit = crate::models::ModelOption::find_by_id(model_id)
            .map(|model| model.capabilities.output_limit)
            .filter(|limit| *limit > 0)
            .and_then(|limit| u32::try_from(limit).ok());
        let output_limit_exclusive = output_limit
            .and_then(|limit| limit.checked_sub(1))
            .filter(|limit| *limit > 0);

        let max_tokens = configured
            .or(output_limit_exclusive)
            .unwrap_or(Self::DEFAULT_MAX_TOKENS);

        // Clamp to output limit if available
        if let Some(limit) = output_limit_exclusive {
            max_tokens.min(limit)
        } else {
            max_tokens
        }
    }

    /// Creates a default config file at the given path.
    /// Returns an error if the file already exists.
    pub fn init(path: &Path) -> Result<()> {
        if path.exists() {
            anyhow::bail!("Config file already exists at {}", path.display());
        }

        Self::write_config(path, default_config_template())
    }

    /// Generates a fresh config TOML from Rust defaults.
    ///
    /// This is used by `xtask update-default-config` to keep
    /// `default_config.toml` in sync with Rust default values.
    ///
    /// Uses the embedded template for structure/comments and merges
    /// generated values from `Config::default()` into it.
    pub fn generate() -> Result<String> {
        use toml_edit::{DocumentMut, Item};

        let config = Config::default();
        let generated_toml =
            toml::to_string(&config).context("Failed to serialize default config to TOML")?;

        // Parse template as base (preserves comments)
        let mut doc: DocumentMut = default_config_template()
            .parse()
            .context("Failed to parse default config template")?;

        // Parse generated values
        let generated_doc: DocumentMut = generated_toml
            .parse()
            .context("Failed to parse generated config")?;

        // Merge generated values into template (overwrites values, keeps comments)
        fn merge(target: &mut toml_edit::Table, source: &toml_edit::Table) {
            for (key, value) in source.iter() {
                match value {
                    Item::Value(v) => {
                        target[key] = Item::Value(v.clone());
                    }
                    Item::Table(src_table) => {
                        if let Some(Item::Table(target_table)) = target.get_mut(key) {
                            merge(target_table, src_table);
                        } else {
                            target[key] = Item::Table(src_table.clone());
                        }
                    }
                    Item::ArrayOfTables(arr) => {
                        target[key] = Item::ArrayOfTables(arr.clone());
                    }
                    Item::None => {}
                }
            }
        }

        merge(doc.as_table_mut(), generated_doc.as_table());

        Ok(doc.to_string())
    }

    /// Writes config content to a file, creating parent directories as needed.
    /// Uses atomic write (temp file + rename) to prevent corruption.
    fn write_config(path: &Path, content: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        let tmp_path = path.with_extension("toml.tmp");
        fs::write(&tmp_path, content)
            .with_context(|| format!("Failed to write config to {}", tmp_path.display()))?;
        fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Failed to rename {} to {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: Self::DEFAULT_MODEL.to_string(),
            max_tokens: None,
            system_prompt: None,
            system_prompt_file: None,
            tool_timeout_secs: Self::DEFAULT_TOOL_TIMEOUT_SECS,
            providers: ProvidersConfig::default(),
            transcription: TranscriptionConfig::default(),
            handoff_model: Self::DEFAULT_HANDOFF_MODEL.to_string(),
            title_model: Self::DEFAULT_TITLE_MODEL.to_string(),
            thinking_level: ThinkingLevel::default(),
            skills: SkillsConfig::default(),
            telegram: TelegramConfig::default(),
        }
    }
}

/// Provider-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    #[serde(default = "default_anthropic_provider")]
    pub anthropic: ProviderConfig,
    #[serde(default = "default_claude_cli_provider")]
    pub claude_cli: ProviderConfig,
    #[serde(default = "default_openai_provider")]
    pub openai: ProviderConfig,
    #[serde(default = "default_openai_codex_provider")]
    pub openai_codex: ProviderConfig,
    #[serde(default = "default_openrouter_provider")]
    pub openrouter: ProviderConfig,
    #[serde(default = "default_moonshot_provider")]
    pub moonshot: ProviderConfig,
    #[serde(default = "default_stepfun_provider")]
    pub stepfun: ProviderConfig,
    #[serde(default = "default_mimo_provider")]
    pub mimo: ProviderConfig,
    #[serde(default = "default_gemini_provider")]
    pub gemini: ProviderConfig,
    #[serde(default = "default_gemini_cli_provider")]
    pub gemini_cli: ProviderConfig,
    #[serde(default = "default_mistral_provider")]
    pub mistral: ProviderConfig,
}

impl ProvidersConfig {
    /// Returns whether a provider is enabled by its string identifier.
    ///
    /// Provider IDs match the model registry format (e.g., "anthropic", "openai", "gemini-cli").
    /// Returns true if the provider is not found (unknown providers default to enabled).
    pub fn is_enabled(&self, provider_id: &str) -> bool {
        use crate::providers::ProviderKind;

        let config = match provider_id {
            id if id == ProviderKind::Anthropic.id() => &self.anthropic,
            id if id == ProviderKind::ClaudeCli.id() => &self.claude_cli,
            id if id == ProviderKind::OpenAI.id() => &self.openai,
            id if id == ProviderKind::OpenAICodex.id() => &self.openai_codex,
            id if id == ProviderKind::OpenRouter.id() => &self.openrouter,
            id if id == ProviderKind::Moonshot.id() => &self.moonshot,
            id if id == ProviderKind::Stepfun.id() => &self.stepfun,
            id if id == ProviderKind::Mimo.id() => &self.mimo,
            id if id == ProviderKind::Gemini.id() => &self.gemini,
            id if id == ProviderKind::GeminiCli.id() => &self.gemini_cli,
            _ => return true, // Unknown providers default to enabled
        };
        config.enabled.unwrap_or(true)
    }

    /// Returns the provider config for a given provider kind.
    pub fn get(&self, kind: crate::providers::ProviderKind) -> &ProviderConfig {
        use crate::providers::ProviderKind;

        match kind {
            ProviderKind::Anthropic => &self.anthropic,
            ProviderKind::ClaudeCli => &self.claude_cli,
            ProviderKind::OpenAI => &self.openai,
            ProviderKind::OpenAICodex => &self.openai_codex,
            ProviderKind::OpenRouter => &self.openrouter,
            ProviderKind::Mistral => &self.mistral,
            ProviderKind::Moonshot => &self.moonshot,
            ProviderKind::Stepfun => &self.stepfun,
            ProviderKind::Mimo => &self.mimo,
            ProviderKind::Gemini => &self.gemini,
            ProviderKind::GeminiCli => &self.gemini_cli,
        }
    }
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            anthropic: default_anthropic_provider(),
            claude_cli: default_claude_cli_provider(),
            openai_codex: default_openai_codex_provider(),
            openai: default_openai_provider(),
            gemini: default_gemini_provider(),
            gemini_cli: default_gemini_cli_provider(),
            openrouter: default_openrouter_provider(),
            moonshot: default_moonshot_provider(),
            stepfun: default_stepfun_provider(),
            mimo: default_mimo_provider(),
            mistral: default_mistral_provider(),
        }
    }
}

fn default_anthropic_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "claude-opus-4-5".to_string(),
            "claude-sonnet-4-5".to_string(),
            "claude-haiku-4-5".to_string(),
        ],
        ..Default::default()
    }
}

fn default_claude_cli_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "claude-opus-4-5".to_string(),
            "claude-sonnet-4-5".to_string(),
            "claude-haiku-4-5".to_string(),
        ],
        ..Default::default()
    }
}

fn default_openai_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gpt-5.2".to_string(),
            "gpt-5.1".to_string(),
            "gpt-5-mini".to_string(),
            "gpt-5-nano".to_string(),
            "gpt-5.2-codex".to_string(),
            "gpt-5.1-codex-max".to_string(),
            "gpt-5.1-codex-mini".to_string(),
            "gpt-4.1".to_string(),
        ],
        ..Default::default()
    }
}

fn default_openai_codex_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gpt-5.2-codex".to_string(),
            "gpt-5.1-codex-max".to_string(),
            "gpt-5.1-codex-mini".to_string(),
            "gpt-5.2".to_string(),
        ],
        ..Default::default()
    }
}

fn default_openrouter_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["*:exacto".to_string()],
        ..Default::default()
    }
}

fn default_moonshot_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["kimi-k2.5".to_string()],
        ..Default::default()
    }
}

fn default_stepfun_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["step-3.5-flash".to_string()],
        ..Default::default()
    }
}

fn default_mimo_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["mimo-v2-flash".to_string()],
        ..Default::default()
    }
}

fn default_gemini_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gemini-3-flash-preview".to_string(),
            "gemini-3-pro-preview".to_string(),
            "gemini-2.5-flash".to_string(),
            "gemini-2.5-flash-lite".to_string(),
        ],
        ..Default::default()
    }
}

fn default_gemini_cli_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gemini-3-flash-preview".to_string(),
            "gemini-3-pro-preview".to_string(),
            "gemini-2.5-flash".to_string(),
            "gemini-2.5-flash-lite".to_string(),
        ],
        ..Default::default()
    }
}

fn default_mistral_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["voxtral-mini-latest".to_string()],
        ..Default::default()
    }
}

/// Provider configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProviderConfig {
    /// Optional API key (overrides environment variable).
    pub api_key: Option<String>,
    /// Optional API base URL (for proxies).
    pub base_url: Option<String>,
    /// Whether this provider is enabled for `zdx models update`.
    pub enabled: Option<bool>,
    /// Desired models for `zdx models update` (supports '*' wildcard).
    pub models: Vec<String>,
    /// Explicit list of enabled tools (if set, only these tools are used).
    /// If unset, all tools are available.
    pub tools: Option<Vec<String>>,
}

impl ProviderConfig {
    /// Returns the effective API key if set and non-empty.
    pub fn effective_api_key(&self) -> Option<&str> {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Returns the effective base URL if set and non-empty.
    pub fn effective_base_url(&self) -> Option<&str> {
        self.base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Filters a list of tool names based on this provider's tool configuration.
    ///
    /// If `tools` is set, returns only those tools (intersection with available).
    /// Otherwise returns all tools.
    ///
    /// Tool names are matched case-insensitively and trimmed of whitespace.
    pub fn filter_tools<'a>(&self, all_tools: &[&'a str]) -> Vec<&'a str> {
        if let Some(include) = &self.tools {
            let include_set: std::collections::HashSet<_> = include
                .iter()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            all_tools
                .iter()
                .filter(|t| include_set.contains(&t.to_lowercase()))
                .copied()
                .collect()
        } else {
            all_tools.to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    /// Config loading: missing file returns defaults (SPEC §9).
    #[test]
    fn test_load_missing_file_returns_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("nonexistent.toml");

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-haiku-4-5");
        assert_eq!(config.max_tokens, None);
    }

    /// Config loading: partial config merges with defaults.
    #[test]
    fn test_load_partial_config_merges_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "model = \"claude-3-opus\"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-3-opus");
        assert_eq!(config.max_tokens, None);
    }

    /// Config init: creates file with defaults, creates parent dirs (SPEC §9).
    #[test]
    fn test_init_creates_config_with_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("subdir").join("config.toml");

        Config::init(&config_path).unwrap();

        assert!(config_path.exists());
        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("claude-haiku-4-5"));
        assert!(contents.contains("# max_tokens ="));
    }

    /// Config init: fails if file exists (no silent overwrite).
    #[test]
    fn test_init_fails_if_exists() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "").unwrap();

        let result = Config::init(&config_path);
        assert!(result.is_err());
    }

    /// Prompt resolution: file wins over inline (SPEC §9).
    #[test]
    fn test_system_prompt_file_wins_over_inline() {
        let dir = tempdir().unwrap();
        let prompt_file = dir.path().join("prompt.txt");
        fs::write(&prompt_file, "file prompt").unwrap();

        let config = Config {
            system_prompt_file: Some(prompt_file.to_str().unwrap().to_string()),
            system_prompt: Some("inline prompt".to_string()),
            ..Default::default()
        };

        assert_eq!(
            config.effective_system_prompt().unwrap(),
            Some("file prompt".to_string())
        );
    }

    /// Timeout: zero disables timeout (SPEC §6).
    #[test]
    fn test_tool_timeout_zero_disables() {
        let config = Config {
            tool_timeout_secs: 0,
            ..Default::default()
        };
        assert_eq!(config.tool_timeout(), None);
    }

    /// Base URL: loaded from config file.
    #[test]
    fn test_anthropic_base_url_loaded_from_config() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(
            &config_path,
            "[providers.anthropic]\nbase_url = \"https://my-proxy.example.com\"\n",
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.providers.anthropic.effective_base_url(),
            Some("https://my-proxy.example.com")
        );
    }

    /// Base URL: empty/whitespace treated as unset.
    #[test]
    fn test_anthropic_base_url_empty_is_none() {
        let config = Config {
            providers: ProvidersConfig {
                anthropic: ProviderConfig {
                    base_url: Some("   ".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.providers.anthropic.effective_base_url(), None);
    }

    /// save_model: creates new config file with template if it doesn't exist.
    #[test]
    fn test_save_model_creates_file_with_template() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        Config::save_model_to(&config_path, "claude-sonnet-4-5-20250929").unwrap();

        assert!(config_path.exists());

        // Verify model was updated
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-sonnet-4-5-20250929");

        // Verify template comments are preserved
        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("# ZDX Configuration"));
        assert!(contents.contains("# Maximum tokens"));
        assert!(contents.contains("# max_tokens = 12288"));
    }

    /// save_model: preserves other fields in existing config.
    #[test]
    fn test_save_model_preserves_other_fields() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create config with other fields
        fs::write(
            &config_path,
            r#"model = "old-model"
max_tokens = 2048
tool_timeout_secs = 60
"#,
        )
        .unwrap();

        Config::save_model_to(&config_path, "new-model").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "new-model");
        assert_eq!(config.max_tokens, Some(2048)); // preserved
        assert_eq!(config.tool_timeout_secs, 60); // preserved
    }

    /// save_model: uses template structure but preserves user values.
    #[test]
    fn test_save_model_merges_with_template() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create config with user values (old format, no template comments)
        fs::write(
            &config_path,
            r#"model = "old-model"
max_tokens = 2048
"#,
        )
        .unwrap();

        Config::save_model_to(&config_path, "new-model").unwrap();

        let contents = fs::read_to_string(&config_path).unwrap();
        // Template comments should now be present
        assert!(contents.contains("# ZDX Configuration"));
        assert!(contents.contains("# Maximum tokens"));
        // User value should be preserved
        assert!(contents.contains("new-model"));
        // User's max_tokens value should be preserved
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.max_tokens, Some(2048));
    }

    /// save_model: creates parent directories if needed.
    #[test]
    fn test_save_model_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("nested").join("dir").join("config.toml");

        Config::save_model_to(&config_path, "claude-sonnet").unwrap();

        assert!(config_path.exists());
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-sonnet");
    }

    /// Thinking: defaults to Off.
    #[test]
    fn test_thinking_defaults() {
        let config = Config::default();
        assert_eq!(config.thinking_level, ThinkingLevel::Off);
        assert!(!config.thinking_level.is_enabled());
    }

    /// ThinkingLevel: effort_percent returns correct values.
    #[test]
    fn test_thinking_level_effort_percent() {
        assert_eq!(ThinkingLevel::Off.effort_percent(), None);
        assert_eq!(ThinkingLevel::Minimal.effort_percent(), Some(5));
        assert_eq!(ThinkingLevel::Low.effort_percent(), Some(20));
        assert_eq!(ThinkingLevel::Medium.effort_percent(), Some(50));
        assert_eq!(ThinkingLevel::High.effort_percent(), Some(80));
        assert_eq!(ThinkingLevel::XHigh.effort_percent(), Some(95));
    }

    /// ThinkingLevel: compute_reasoning_budget returns correct values.
    #[test]
    fn test_thinking_level_compute_reasoning_budget() {
        // Off returns None
        assert_eq!(
            ThinkingLevel::Off.compute_reasoning_budget(10000, None),
            None
        );

        // Medium (50%) of 10000 = 5000
        assert_eq!(
            ThinkingLevel::Medium.compute_reasoning_budget(10000, None),
            Some(5000)
        );

        // High (80%) of 10000 = 8000
        assert_eq!(
            ThinkingLevel::High.compute_reasoning_budget(10000, None),
            Some(8000)
        );

        // XHigh (95%) of 10000 = 9500
        assert_eq!(
            ThinkingLevel::XHigh.compute_reasoning_budget(10000, None),
            Some(9500)
        );

        // Minimal (5%) of 5000 = 250, but clamped to min 1024
        assert_eq!(
            ThinkingLevel::Minimal.compute_reasoning_budget(5000, None),
            Some(1024)
        );

        // Uses min of max_tokens and model_output_limit
        assert_eq!(
            ThinkingLevel::Medium.compute_reasoning_budget(20000, Some(10000)),
            Some(5000) // 50% of min(20000, 10000) = 5000
        );

        // No max clamp - XHigh (95%) of 200000 = 190000
        assert_eq!(
            ThinkingLevel::XHigh.compute_reasoning_budget(200000, None),
            Some(190_000)
        );
    }

    /// ThinkingLevel: display_name returns short names.
    #[test]
    fn test_thinking_level_display_name() {
        assert_eq!(ThinkingLevel::Off.display_name(), "off");
        assert_eq!(ThinkingLevel::Medium.display_name(), "medium");
        assert_eq!(ThinkingLevel::High.display_name(), "high");
    }

    /// ThinkingLevel: all() returns all levels.
    #[test]
    fn test_thinking_level_all() {
        let all = ThinkingLevel::all();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], ThinkingLevel::Off);
        assert_eq!(all[5], ThinkingLevel::XHigh);
    }

    /// Thinking: effective_max_tokens returns raw value when thinking disabled.
    #[test]
    fn test_effective_max_tokens_thinking_disabled() {
        let config = Config {
            max_tokens: Some(1024),
            thinking_level: ThinkingLevel::Off,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens_for("claude-haiku-4-5"), 1024);
    }

    /// Thinking: effective_max_tokens auto-adjusts when thinking enabled and max_tokens too low.
    #[test]
    fn test_effective_max_tokens_returns_configured_value() {
        let config = Config {
            max_tokens: Some(1024),
            thinking_level: ThinkingLevel::Medium, // doesn't affect max_tokens anymore
            ..Default::default()
        };
        // Now just returns configured value (no auto-adjustment)
        assert_eq!(config.effective_max_tokens_for("claude-haiku-4-5"), 1024);
    }

    /// Thinking: effective_max_tokens respects user value when sufficient.
    #[test]
    fn test_effective_max_tokens_respects_high_value() {
        let config = Config {
            max_tokens: Some(20000), // sufficient
            thinking_level: ThinkingLevel::Medium,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens_for("claude-haiku-4-5"), 20000);
    }

    /// effective_max_tokens uses the model output limit (exclusive) when max_tokens is unset.
    #[test]
    fn test_effective_max_tokens_uses_model_output_limit_when_unset() {
        let config = Config {
            max_tokens: None,
            thinking_level: ThinkingLevel::Off,
            ..Default::default()
        };
        let model_id = "claude-haiku-4-5";
        let expected = crate::models::ModelOption::find_by_id(model_id)
            .map(|model| model.capabilities.output_limit)
            .filter(|limit| *limit > 0)
            .and_then(|limit| u32::try_from(limit).ok())
            .and_then(|limit| limit.checked_sub(1))
            .filter(|limit| *limit > 0)
            .unwrap_or(Config::DEFAULT_MAX_TOKENS);

        assert_eq!(config.effective_max_tokens_for(model_id), expected);
    }

    /// Thinking: config loads from file with thinking_level.
    #[test]
    fn test_thinking_config_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, r#"thinking_level = "high""#).unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::High);
        assert!(config.thinking_level.is_enabled());
    }

    /// Thinking: old configs without thinking_level use defaults (serde default).
    #[test]
    fn test_thinking_config_missing_uses_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Old config without thinking_level field
        fs::write(&config_path, "model = \"claude-3-opus\"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::Off);
    }

    /// save_thinking_level: creates new config file with template if it doesn't exist.
    #[test]
    fn test_save_thinking_level_creates_file_with_template() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        Config::save_thinking_level_to(&config_path, ThinkingLevel::High).unwrap();

        assert!(config_path.exists());

        // Verify thinking_level was updated
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::High);

        // Verify template comments are preserved
        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("# ZDX Configuration"));
        assert!(contents.contains("thinking_level = \"high\""));
    }

    /// save_thinking_level: preserves other fields in existing config.
    #[test]
    fn test_save_thinking_level_preserves_other_fields() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create config with other fields
        fs::write(
            &config_path,
            r#"model = "claude-sonnet-4"
max_tokens = 4096
thinking_level = "off"
"#,
        )
        .unwrap();

        Config::save_thinking_level_to(&config_path, ThinkingLevel::Medium).unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::Medium);
        assert_eq!(config.model, "claude-sonnet-4"); // preserved
        assert_eq!(config.max_tokens, Some(4096)); // preserved
    }

    /// save_thinking_level: uses template structure but preserves user values.
    #[test]
    fn test_save_thinking_level_merges_with_template() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create config with user values (old format, no template comments)
        fs::write(
            &config_path,
            r#"model = "claude-sonnet-4"
thinking_level = "off"
max_tokens = 4096
"#,
        )
        .unwrap();

        Config::save_thinking_level_to(&config_path, ThinkingLevel::High).unwrap();

        let contents = fs::read_to_string(&config_path).unwrap();
        // Template comments should now be present
        assert!(contents.contains("# ZDX Configuration"));
        assert!(contents.contains("# Extended thinking level"));
        assert!(contents.contains("thinking_level = \"high\""));
        // User values should be preserved
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-sonnet-4");
        assert_eq!(config.max_tokens, Some(4096));
    }

    /// save_thinking_level: roundtrip - save and reload works correctly.
    #[test]
    fn test_save_thinking_level_roundtrip() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create initial config
        fs::write(&config_path, "model = \"test-model\"\n").unwrap();

        // Save thinking level
        Config::save_thinking_level_to(&config_path, ThinkingLevel::Low).unwrap();

        // Reload and verify
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::Low);
        assert_eq!(config.model, "test-model");

        // Change to different level
        Config::save_thinking_level_to(&config_path, ThinkingLevel::Minimal).unwrap();

        // Reload and verify again
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::Minimal);
    }

    /// filter_tools: returns all tools when no filtering configured.
    #[test]
    fn test_filter_tools_no_filtering() {
        let config = ProviderConfig::default();
        let all_tools = &[
            "bash",
            "apply_patch",
            "edit",
            "read",
            "read_thread",
            "write",
        ];

        let filtered = config.filter_tools(all_tools);
        assert_eq!(filtered, all_tools);
    }

    /// filter_tools: explicit tools list.
    #[test]
    fn test_filter_tools_explicit_include() {
        let config = ProviderConfig {
            tools: Some(vec!["bash".to_string(), "read".to_string()]),
            ..Default::default()
        };
        let all_tools = &[
            "bash",
            "apply_patch",
            "edit",
            "read",
            "read_thread",
            "write",
        ];

        let filtered = config.filter_tools(all_tools);
        assert_eq!(filtered, vec!["bash", "read"]);
    }

    /// filter_tools: case-insensitive matching.
    #[test]
    fn test_filter_tools_case_insensitive() {
        let config = ProviderConfig {
            tools: Some(vec!["BASH".to_string(), "READ".to_string()]),
            ..Default::default()
        };
        let all_tools = &[
            "bash",
            "apply_patch",
            "edit",
            "read",
            "read_thread",
            "write",
        ];

        let filtered = config.filter_tools(all_tools);
        assert_eq!(filtered, vec!["bash", "read"]);
    }

    /// filter_tools: trims whitespace from tool names.
    #[test]
    fn test_filter_tools_trims_whitespace() {
        let config = ProviderConfig {
            tools: Some(vec![" bash ".to_string(), "\tread\n".to_string()]),
            ..Default::default()
        };
        let all_tools = &[
            "bash",
            "apply_patch",
            "edit",
            "read",
            "read_thread",
            "write",
        ];

        let filtered = config.filter_tools(all_tools);
        assert_eq!(filtered, vec!["bash", "read"]);
    }

    /// filter_tools: ignores empty strings after trimming.
    #[test]
    fn test_filter_tools_ignores_empty_strings() {
        let config = ProviderConfig {
            tools: Some(vec![
                "bash".to_string(),
                "  ".to_string(),
                "".to_string(),
                "read".to_string(),
            ]),
            ..Default::default()
        };
        let all_tools = &[
            "bash",
            "apply_patch",
            "edit",
            "read",
            "read_thread",
            "write",
        ];

        let filtered = config.filter_tools(all_tools);
        assert_eq!(filtered, vec!["bash", "read"]);
    }

    /// filter_tools: openai_codex default has no tool filtering.
    #[test]
    fn test_openai_codex_default_tools() {
        let config = default_openai_codex_provider();
        assert!(config.tools.is_none());
        let all_tools = &[
            "bash",
            "apply_patch",
            "edit",
            "read",
            "read_thread",
            "write",
        ];

        let filtered = config.filter_tools(all_tools);
        assert_eq!(filtered, all_tools);
    }

    /// filter_tools: anthropic default has no tool filtering.
    #[test]
    fn test_anthropic_default_tools() {
        let config = default_anthropic_provider();
        assert!(config.tools.is_none());
        let all_tools = &[
            "bash",
            "apply_patch",
            "edit",
            "read",
            "read_thread",
            "write",
        ];

        let filtered = config.filter_tools(all_tools);
        assert_eq!(filtered, all_tools);
    }

    /// ProvidersConfig::get returns correct provider config.
    #[test]
    fn test_providers_config_get() {
        use crate::providers::ProviderKind;

        let providers = ProvidersConfig::default();

        let anthropic = providers.get(ProviderKind::Anthropic);
        assert!(anthropic.enabled.unwrap());
        assert!(anthropic.tools.is_none());

        let codex = providers.get(ProviderKind::OpenAICodex);
        assert!(codex.tools.is_none());
    }

    /// TranscriptionConfig: defaults are all None (auto-detect, no model override, no language).
    #[test]
    fn test_transcription_config_defaults() {
        let config = TranscriptionConfig::default();
        assert!(config.provider.is_none());
        assert!(config.model.is_none());
        assert!(config.language.is_none());
    }
}
