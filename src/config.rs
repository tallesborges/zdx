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
    /// Very brief reasoning (~1k tokens)
    Minimal,
    /// Light reasoning (~2k tokens)
    Low,
    /// Moderate reasoning (~8k tokens)
    Medium,
    /// Deep reasoning (~16k tokens)
    High,
}

impl ThinkingLevel {
    /// Returns the token budget for this thinking level.
    /// Returns None for Off (thinking disabled).
    pub fn budget_tokens(&self) -> Option<u32> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Minimal => Some(1024),
            ThinkingLevel::Low => Some(2048),
            ThinkingLevel::Medium => Some(8192),
            ThinkingLevel::High => Some(16384),
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
            ThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
            ThinkingLevel::Low => "Light reasoning (~2k tokens)",
            ThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
            ThinkingLevel::High => "Deep reasoning (~16k tokens)",
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
        ]
    }
}

/// Default config template with comments, embedded at compile time.
const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../default_config.toml");

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

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The Claude model to use
    pub model: String,

    /// Maximum tokens for responses
    pub max_tokens: u32,

    /// Optional inline system prompt
    pub system_prompt: Option<String>,

    /// Optional path to a file containing the system prompt
    pub system_prompt_file: Option<String>,

    /// Timeout for tool execution in seconds (0 disables)
    pub tool_timeout_secs: u32,

    /// Provider configuration (base URLs, etc.).
    #[serde(default)]
    pub providers: ProvidersConfig,

    /// Thinking level for extended thinking feature
    #[serde(default)]
    pub thinking_level: ThinkingLevel,
}

impl Config {
    const DEFAULT_MODEL: &str = "claude-haiku-4-5";
    const DEFAULT_MAX_TOKENS: u32 = 1024;
    const DEFAULT_TOOL_TIMEOUT_SECS: u32 = 30;
    /// Minimum buffer above thinking budget for response tokens.
    const THINKING_RESPONSE_BUFFER: u32 = 4096;

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
    /// Preserves existing fields and comments using toml_edit.
    pub fn save_model_to(path: &Path, model: &str) -> Result<()> {
        use toml_edit::{DocumentMut, value};

        // Read existing file or use default template
        let contents = if path.exists() {
            fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?
        } else {
            DEFAULT_CONFIG_TEMPLATE.to_string()
        };

        // Parse as editable document (preserves comments and formatting)
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
    /// Preserves existing fields and comments using toml_edit.
    pub fn save_thinking_level_to(path: &Path, level: ThinkingLevel) -> Result<()> {
        use toml_edit::{DocumentMut, value};

        // Read existing file or use default template
        let contents = if path.exists() {
            fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?
        } else {
            DEFAULT_CONFIG_TEMPLATE.to_string()
        };

        // Parse as editable document (preserves comments and formatting)
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

    /// Returns the effective max_tokens to use in API requests.
    ///
    /// When thinking is enabled, ensures max_tokens >= thinking_budget + buffer
    /// to leave room for the response.
    pub fn effective_max_tokens(&self) -> u32 {
        let Some(thinking_budget) = self.thinking_level.budget_tokens() else {
            // Thinking disabled
            return self.max_tokens;
        };

        let min_required = thinking_budget + Self::THINKING_RESPONSE_BUFFER;
        if self.max_tokens >= min_required {
            self.max_tokens
        } else {
            // Auto-adjust to ensure room for both thinking and response
            min_required
        }
    }

    /// Creates a default config file at the given path.
    /// Returns an error if the file already exists.
    pub fn init(path: &Path) -> Result<()> {
        if path.exists() {
            anyhow::bail!("Config file already exists at {}", path.display());
        }

        Self::write_config(path, DEFAULT_CONFIG_TEMPLATE)
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
            max_tokens: Self::DEFAULT_MAX_TOKENS,
            system_prompt: None,
            system_prompt_file: None,
            tool_timeout_secs: Self::DEFAULT_TOOL_TIMEOUT_SECS,
            providers: ProvidersConfig::default(),
            thinking_level: ThinkingLevel::default(),
        }
    }
}

/// Provider-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    #[serde(default = "default_anthropic_provider")]
    pub anthropic: ProviderConfig,
    #[serde(default = "default_openai_provider")]
    pub openai: ProviderConfig,
    #[serde(default = "default_openai_codex_provider")]
    pub openai_codex: ProviderConfig,
    #[serde(default = "default_openrouter_provider")]
    pub openrouter: ProviderConfig,
    #[serde(default = "default_gemini_provider")]
    pub gemini: ProviderConfig,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            anthropic: default_anthropic_provider(),
            openai: default_openai_provider(),
            openai_codex: default_openai_codex_provider(),
            openrouter: default_openrouter_provider(),
            gemini: default_gemini_provider(),
        }
    }
}

fn default_anthropic_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "claude-haiku-4-5".to_string(),
            "claude-opus-4-5".to_string(),
            "claude-sonnet-4-5".to_string(),
        ],
        ..Default::default()
    }
}

fn default_openai_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gpt-5.2".to_string(),
            "gpt-5-mini".to_string(),
            "gpt-5-nano".to_string(),
            "gpt-5.1-codex".to_string(),
            "gpt-5.1-codex-max".to_string(),
            "gpt-5.1-codex-mini".to_string(),
        ],
        ..Default::default()
    }
}

fn default_openai_codex_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gpt-5.1-codex-max".to_string(),
            "gpt-5.1-codex-mini".to_string(),
            "gpt-5.2-codex".to_string(),
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

/// Provider configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProviderConfig {
    /// Optional API base URL (for proxies).
    pub base_url: Option<String>,
    /// Whether this provider is enabled for `zdx models update`.
    pub enabled: Option<bool>,
    /// Desired models for `zdx models update` (supports '*' wildcard).
    pub models: Vec<String>,
}

impl ProviderConfig {
    /// Returns the effective base URL if set and non-empty.
    pub fn effective_base_url(&self) -> Option<&str> {
        self.base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
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
        assert_eq!(config.max_tokens, 1024);
    }

    /// Config loading: partial config merges with defaults.
    #[test]
    fn test_load_partial_config_merges_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "model = \"claude-3-opus\"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-3-opus");
        assert_eq!(config.max_tokens, 1024); // default preserved
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
        assert!(contents.contains("max_tokens"));
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
        assert!(contents.contains("max_tokens = 1024"));
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
        assert_eq!(config.max_tokens, 2048); // preserved
        assert_eq!(config.tool_timeout_secs, 60); // preserved
    }

    /// save_model: preserves comments in config file.
    #[test]
    fn test_save_model_preserves_comments() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create config with comments
        fs::write(
            &config_path,
            r#"# My config file
model = "old-model"
# This is important
max_tokens = 2048
"#,
        )
        .unwrap();

        Config::save_model_to(&config_path, "new-model").unwrap();

        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("# My config file"));
        assert!(contents.contains("# This is important"));
        assert!(contents.contains("new-model"));
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

    /// ThinkingLevel: budget_tokens returns correct values.
    #[test]
    fn test_thinking_level_budget_tokens() {
        assert_eq!(ThinkingLevel::Off.budget_tokens(), None);
        assert_eq!(ThinkingLevel::Minimal.budget_tokens(), Some(1024));
        assert_eq!(ThinkingLevel::Low.budget_tokens(), Some(2048));
        assert_eq!(ThinkingLevel::Medium.budget_tokens(), Some(8192));
        assert_eq!(ThinkingLevel::High.budget_tokens(), Some(16384));
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
        assert_eq!(all.len(), 5);
        assert_eq!(all[0], ThinkingLevel::Off);
        assert_eq!(all[4], ThinkingLevel::High);
    }

    /// Thinking: effective_max_tokens returns raw value when thinking disabled.
    #[test]
    fn test_effective_max_tokens_thinking_disabled() {
        let config = Config {
            max_tokens: 1024,
            thinking_level: ThinkingLevel::Off,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens(), 1024);
    }

    /// Thinking: effective_max_tokens auto-adjusts when thinking enabled and max_tokens too low.
    #[test]
    fn test_effective_max_tokens_auto_adjusts_when_thinking_enabled() {
        let config = Config {
            max_tokens: 1024,                      // too low for thinking
            thinking_level: ThinkingLevel::Medium, // 8192 budget
            ..Default::default()
        };
        // Should be thinking_budget (8192) + buffer (4096) = 12288
        assert_eq!(config.effective_max_tokens(), 8192 + 4096);
    }

    /// Thinking: effective_max_tokens respects user value when sufficient.
    #[test]
    fn test_effective_max_tokens_respects_high_value() {
        let config = Config {
            max_tokens: 20000, // sufficient
            thinking_level: ThinkingLevel::Medium,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens(), 20000);
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
        assert_eq!(config.max_tokens, 4096); // preserved
    }

    /// save_thinking_level: preserves comments in config file.
    #[test]
    fn test_save_thinking_level_preserves_comments() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create config with comments
        fs::write(
            &config_path,
            r#"# My custom config
model = "claude-sonnet-4"
# Thinking configuration
thinking_level = "off"
"#,
        )
        .unwrap();

        Config::save_thinking_level_to(&config_path, ThinkingLevel::High).unwrap();

        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("# My custom config"));
        assert!(contents.contains("# Thinking configuration"));
        assert!(contents.contains("thinking_level = \"high\""));
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
}
