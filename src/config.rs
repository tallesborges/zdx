//! Configuration management for ZDX.
//!
//! Loads configuration from ${ZDX_HOME}/config.toml with sensible defaults.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

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
}

impl Config {
    const DEFAULT_MODEL: &str = "claude-haiku-4-5";
    const DEFAULT_MAX_TOKENS: u32 = 1024;
    const DEFAULT_TOOL_TIMEOUT_SECS: u32 = 30;

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

    /// Creates a default config file at the given path.
    /// Returns an error if the file already exists.
    pub fn init(path: &Path) -> Result<()> {
        if path.exists() {
            anyhow::bail!("Config file already exists at {}", path.display());
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        let toml = format!(
            "# ZDX Configuration\n\nmodel = \"{}\"\nmax_tokens = {}\ntool_timeout_secs = {}\n\n# system_prompt = \"You are a helpful assistant.\"\n# system_prompt_file = \"/path/to/system_prompt.md\"\n\n# anthropic_base_url = \"https://api.anthropic.com\"\n",
            Self::DEFAULT_MODEL,
            Self::DEFAULT_MAX_TOKENS,
            Self::DEFAULT_TOOL_TIMEOUT_SECS
        );

        fs::write(path, toml)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;

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
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.model, "claude-haiku-4-5");
        assert_eq!(config.max_tokens, 1024);
        assert_eq!(config.tool_timeout_secs, 30);
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("nonexistent.toml");

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-haiku-4-5");
    }

    #[test]
    fn test_load_partial_config() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "model = \"claude-3-opus\"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-3-opus");
        assert_eq!(config.max_tokens, 1024);
    }

    #[test]
    fn test_init_creates_config() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("subdir").join("config.toml");

        Config::init(&config_path).unwrap();

        assert!(config_path.exists());
        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("claude-haiku-4-5"));
    }

    #[test]
    fn test_init_fails_if_exists() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "").unwrap();

        let result = Config::init(&config_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_effective_system_prompt_inline() {
        let mut config = Config::default();
        config.system_prompt = Some("inline prompt".to_string());
        assert_eq!(
            config.effective_system_prompt().unwrap(),
            Some("inline prompt".to_string())
        );
    }

    #[test]
    fn test_effective_system_prompt_inline_empty_is_none() {
        let mut config = Config::default();
        config.system_prompt = Some("   ".to_string());
        assert_eq!(config.effective_system_prompt().unwrap(), None);
    }

    #[test]
    fn test_effective_system_prompt_file() {
        let dir = tempdir().unwrap();
        let prompt_file = dir.path().join("prompt.txt");
        fs::write(&prompt_file, "file prompt").unwrap();

        let mut config = Config::default();
        config.system_prompt_file = Some(prompt_file.to_str().unwrap().to_string());
        config.system_prompt = Some("inline prompt".to_string());

        assert_eq!(
            config.effective_system_prompt().unwrap(),
            Some("file prompt".to_string())
        );
    }

    #[test]
    fn test_effective_system_prompt_file_empty_is_none() {
        let dir = tempdir().unwrap();
        let prompt_file = dir.path().join("prompt.txt");
        fs::write(&prompt_file, " \n\t ").unwrap();

        let mut config = Config::default();
        config.system_prompt_file = Some(prompt_file.to_str().unwrap().to_string());
        config.system_prompt = Some("inline prompt".to_string());

        assert_eq!(config.effective_system_prompt().unwrap(), None);
    }

    #[test]
    fn test_effective_system_prompt_none() {
        let config = Config::default();
        assert_eq!(config.effective_system_prompt().unwrap(), None);
    }

    #[test]
    fn test_tool_timeout_zero_disables() {
        let mut config = Config::default();
        config.tool_timeout_secs = 0;
        assert_eq!(config.tool_timeout(), None);
    }

    #[test]
    fn test_anthropic_base_url_from_config() {
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

    #[test]
    fn test_anthropic_base_url_empty_string_is_none() {
        let mut config = Config::default();
        config.anthropic_base_url = Some("".to_string());
        assert_eq!(config.effective_anthropic_base_url(), None);
    }

    #[test]
    fn test_anthropic_base_url_whitespace_is_none() {
        let mut config = Config::default();
        config.anthropic_base_url = Some("   ".to_string());
        assert_eq!(config.effective_anthropic_base_url(), None);
    }

    #[test]
    fn test_anthropic_base_url_default_is_none() {
        let config = Config::default();
        assert_eq!(config.anthropic_base_url, None);
        assert_eq!(config.effective_anthropic_base_url(), None);
    }

    #[test]
    fn test_init_includes_commented_anthropic_base_url() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        Config::init(&config_path).unwrap();

        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("# anthropic_base_url"));
    }
}
