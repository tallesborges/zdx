//! Configuration management for ZDX.
//!
//! Loads configuration from ${ZDX_HOME}/config.toml with sensible defaults.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default config template with comments, embedded at compile time.
const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("default_config.toml");

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

    /// Returns the path to the sessions directory.
    pub fn sessions_dir() -> PathBuf {
        zdx_home().join("sessions")
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

    /// Optional Anthropic API base URL (for test rigs or proxies)
    pub anthropic_base_url: Option<String>,

    /// Enable extended thinking (default: false)
    pub thinking_enabled: bool,

    /// Token budget for thinking when enabled (default: 8000)
    pub thinking_budget_tokens: u32,
}

impl Config {
    const DEFAULT_MODEL: &str = "claude-haiku-4-5";
    const DEFAULT_MAX_TOKENS: u32 = 1024;
    const DEFAULT_TOOL_TIMEOUT_SECS: u32 = 30;
    const DEFAULT_THINKING_ENABLED: bool = false;
    const DEFAULT_THINKING_BUDGET_TOKENS: u32 = 8000;
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

    /// Returns the effective Anthropic base URL from config, if set.
    /// Empty strings are treated as unset.
    pub fn effective_anthropic_base_url(&self) -> Option<&str> {
        self.anthropic_base_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
    }

    /// Returns the effective max_tokens to use in API requests.
    ///
    /// When thinking is enabled, ensures max_tokens >= thinking_budget_tokens + buffer
    /// to leave room for the response. Logs when auto-adjusting.
    pub fn effective_max_tokens(&self) -> u32 {
        self.effective_max_tokens_inner(true)
    }

    /// Inner implementation that optionally logs adjustments.
    /// Used by tests to avoid noisy output.
    fn effective_max_tokens_inner(&self, _log_adjustment: bool) -> u32 {
        if !self.thinking_enabled {
            return self.max_tokens;
        }

        let min_required = self.thinking_budget_tokens + Self::THINKING_RESPONSE_BUFFER;
        if self.max_tokens >= min_required {
            self.max_tokens
        } else {
            // Note: We don't log here because eprintln! corrupts the TUI display.
            // The adjustment is visible in debug logs if RUST_LOG is set.
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
            anthropic_base_url: None,
            thinking_enabled: Self::DEFAULT_THINKING_ENABLED,
            thinking_budget_tokens: Self::DEFAULT_THINKING_BUDGET_TOKENS,
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
            "anthropic_base_url = \"https://my-proxy.example.com\"\n",
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.effective_anthropic_base_url(),
            Some("https://my-proxy.example.com")
        );
    }

    /// Base URL: empty/whitespace treated as unset.
    #[test]
    fn test_anthropic_base_url_empty_is_none() {
        let config = Config {
            anthropic_base_url: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(config.effective_anthropic_base_url(), None);
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

    /// Thinking: defaults to disabled with 8000 budget.
    #[test]
    fn test_thinking_defaults() {
        let config = Config::default();
        assert!(!config.thinking_enabled);
        assert_eq!(config.thinking_budget_tokens, 8000);
    }

    /// Thinking: effective_max_tokens returns raw value when thinking disabled.
    #[test]
    fn test_effective_max_tokens_thinking_disabled() {
        let config = Config {
            max_tokens: 1024,
            thinking_enabled: false,
            thinking_budget_tokens: 8000,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens_inner(false), 1024);
    }

    /// Thinking: effective_max_tokens auto-adjusts when thinking enabled and max_tokens too low.
    #[test]
    fn test_effective_max_tokens_auto_adjusts_when_thinking_enabled() {
        let config = Config {
            max_tokens: 1024, // too low for thinking
            thinking_enabled: true,
            thinking_budget_tokens: 8000,
            ..Default::default()
        };
        // Should be thinking_budget_tokens + buffer (4096)
        assert_eq!(config.effective_max_tokens_inner(false), 8000 + 4096);
    }

    /// Thinking: effective_max_tokens respects user value when sufficient.
    #[test]
    fn test_effective_max_tokens_respects_high_value() {
        let config = Config {
            max_tokens: 20000, // sufficient
            thinking_enabled: true,
            thinking_budget_tokens: 8000,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens_inner(false), 20000);
    }

    /// Thinking: config loads from file with thinking fields.
    #[test]
    fn test_thinking_config_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(
            &config_path,
            r#"thinking_enabled = true
thinking_budget_tokens = 10000
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert!(config.thinking_enabled);
        assert_eq!(config.thinking_budget_tokens, 10000);
    }

    /// Thinking: old configs without thinking fields use defaults (serde default).
    #[test]
    fn test_thinking_config_missing_uses_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Old config without thinking fields
        fs::write(&config_path, "model = \"claude-3-opus\"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert!(!config.thinking_enabled);
        assert_eq!(config.thinking_budget_tokens, 8000);
    }
}
