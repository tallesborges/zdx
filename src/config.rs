//! Configuration management for ZDX.
//!
//! Loads configuration from ${ZDX_HOME}/config.toml with sensible defaults.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::paths;

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The Claude model to use
    pub model: String,

    /// Maximum tokens for responses
    pub max_tokens: u32,
}

impl Config {
    const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";
    const DEFAULT_MAX_TOKENS: u32 = 1024;

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
            "# ZDX Configuration\n\nmodel = \"{}\"\nmax_tokens = {}\n",
            Self::DEFAULT_MODEL,
            Self::DEFAULT_MAX_TOKENS
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.max_tokens, 1024);
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("nonexistent.toml");

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "claude-sonnet-4-20250514");
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
        assert!(contents.contains("claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_init_fails_if_exists() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "").unwrap();

        let result = Config::init(&config_path);
        assert!(result.is_err());
    }
}
