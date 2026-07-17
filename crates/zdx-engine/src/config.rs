//! Configuration management for ZDX.
//!
//! Loads configuration from ${`ZDX_HOME}/config.toml` with sensible defaults.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Skill source toggles grouped by source/type.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillSourceToggles {
    pub zdx_user: bool,
    pub zdx_project: bool,
    pub codex_user: bool,
    pub claude_user: bool,
    pub claude_project: bool,
    pub agents_user: bool,
    pub agents_project: bool,
}

/// Text verbosity for `OpenAI` Responses-compatible providers.
pub use zdx_types::TextVerbosity;
/// Thinking level for extended thinking feature.
///
/// Controls how much reasoning effort providers use before responding.
/// Higher levels use more tokens but provide deeper reasoning.
pub use zdx_types::ThinkingLevel;

fn default_skill_repositories() -> Vec<String> {
    vec![
        "openai/skills/skills/.curated".to_string(),
        "openai/skills/skills/.system".to_string(),
        "anthropics/skills/skills".to_string(),
    ]
}

fn default_subagents_enabled() -> bool {
    true
}

/// Prompt template rendering configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PromptTemplateConfig {
    /// Optional template file path.
    ///
    /// If relative, it is resolved from `ZDX_HOME`.
    pub file: Option<String>,
}

/// Skill discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub sources: SkillSourceToggles,
    #[serde(
        default = "default_skill_repositories",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub skill_repositories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_skills: Vec<String>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            sources: SkillSourceToggles {
                zdx_user: true,
                zdx_project: true,
                codex_user: true,
                claude_user: true,
                claude_project: true,
                agents_user: true,
                agents_project: true,
            },
            skill_repositories: default_skill_repositories(),
            ignored_skills: Vec::new(),
            include_skills: Vec::new(),
        }
    }
}

/// Subagent delegation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubagentsConfig {
    /// Enables/disables subagent delegation tool exposure.
    #[serde(default = "default_subagents_enabled")]
    pub enabled: bool,
    /// Available models for `invoke_subagent`.
    ///
    /// This list is derived at runtime from enabled providers and the model
    /// registry (same source used by the TUI model picker).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_models: Vec<String>,
}

impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            enabled: default_subagents_enabled(),
            available_models: Vec::new(),
        }
    }
}

/// Telegram bot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Model for the Telegram bot.
    pub model: String,
    /// Thinking level for the Telegram bot.
    pub thinking_level: ThinkingLevel,
    /// Per-chat project profiles keyed by profile name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, TelegramProfileConfig>,
}

/// Per-chat Telegram project profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramProfileConfig {
    /// Telegram chat ID routed to this profile.
    pub chat_id: i64,
    /// Working directory for agent turns in this chat.
    pub cwd: String,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            bot_token: None,
            allowlist_user_ids: Vec::new(),
            allowlist_chat_ids: Vec::new(),
            model: "claude-cli:claude-opus-4-6".to_string(),
            thinking_level: ThinkingLevel::Low,
            profiles: BTreeMap::new(),
        }
    }
}

impl TelegramProfileConfig {
    #[must_use]
    pub fn cwd_path(&self) -> PathBuf {
        expand_tilde(self.cwd.trim())
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTelegramRuntime {
    pub bot_token: String,
    pub allowlist_user_ids: Vec<i64>,
    pub allowlist_chat_ids: Vec<i64>,
}

fn normalize_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn deserialize_optional_non_empty_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        Some(value) => normalize_non_empty(&value)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("value must not be blank")),
        None => Ok(None),
    }
}

fn deserialize_memory_root<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = deserialize_optional_non_empty_string(deserializer)?;
    if let Some(value) = &value {
        let path = Path::new(value);
        let uses_tilde = value == "~" || value.starts_with("~/");
        if !uses_tilde && !path.is_absolute() {
            return Err(serde::de::Error::custom(
                "value must be an absolute path or use ~/",
            ));
        }
    }
    Ok(value)
}

fn default_qmd_command() -> String {
    "qmd".to_string()
}

fn deserialize_qmd_command<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    normalize_non_empty(&value).ok_or_else(|| serde::de::Error::custom("value must not be blank"))
}

/// Returns the default config template with comments.
///
/// This is embedded from `default_config.toml` at compile time.
/// To update, edit `default_config.toml` directly.
fn default_config_template() -> &'static str {
    zdx_assets::DEFAULT_CONFIG_TOML
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

    for (key, value) in source {
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

fn merge_generated_items(target: &mut toml_edit::Table, source: &toml_edit::Table) {
    use toml_edit::Item;

    for (key, value) in source {
        match value {
            Item::Value(v) => {
                target[key] = Item::Value(v.clone());
            }
            Item::Table(src_table) => {
                if let Some(Item::Table(target_table)) = target.get_mut(key) {
                    merge_generated_items(target_table, src_table);
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

pub mod paths {
    //! Path resolution for ZDX configuration and data directories.
    //!
    //! `ZDX_HOME` resolution order:
    //! 1. `ZDX_HOME` environment variable (if set)
    //! 2. ~/.zdx (default)

    use std::path::PathBuf;

    /// Returns the current user's home directory, if available.
    pub fn home_dir() -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("HOME") {
            let path = PathBuf::from(home);
            if !path.as_os_str().is_empty() {
                return Some(path);
            }
        }

        if let Some(user_profile) = std::env::var_os("USERPROFILE") {
            let path = PathBuf::from(user_profile);
            if !path.as_os_str().is_empty() {
                return Some(path);
            }
        }

        None
    }

    /// Returns the ZDX home directory.
    ///
    /// Checks `ZDX_HOME` env var first, falls back to ~/.zdx
    ///
    /// # Panics
    /// Panics if the home directory cannot be determined.
    pub fn zdx_home() -> PathBuf {
        if let Ok(home) = std::env::var("ZDX_HOME") {
            return PathBuf::from(home);
        }

        home_dir()
            .map(|h| h.join(".zdx"))
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

    /// Returns the path to the exports directory.
    pub fn exports_dir() -> PathBuf {
        zdx_home().join("exports")
    }

    /// Returns the path to exported thread transcripts.
    pub fn thread_exports_dir() -> PathBuf {
        exports_dir().join("threads")
    }

    /// Returns the artifact root directory (`$ZDX_HOME/artifacts`).
    pub fn artifact_root() -> PathBuf {
        zdx_home().join("artifacts")
    }
}

/// Default value for serde when `handoff_model` is missing.
fn default_handoff_model() -> String {
    Config::DEFAULT_HANDOFF_MODEL.to_string()
}

/// Default value for serde when `title_model` is missing.
fn default_title_model() -> String {
    Config::DEFAULT_TITLE_MODEL.to_string()
}

/// Default value for serde when `read_thread_model` is missing.
fn default_read_thread_model() -> String {
    Config::DEFAULT_READ_THREAD_MODEL.to_string()
}

/// Default value for serde when `tldr_model` is missing.
fn default_tldr_model() -> String {
    Config::DEFAULT_TLDR_MODEL.to_string()
}

/// Transcription configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TranscriptionConfig {
    /// Model to use for transcription: a `provider:model` id (e.g.
    /// `elevenlabs:scribe_v2`) or a bare provider name (e.g. `mistral`) to use
    /// that provider's default model.
    pub model: Option<String>,
    /// Language hint (ISO 639-1 code like "en", "pt", etc.)
    pub language: Option<String>,
}

/// Text-to-speech (speech synthesis) configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SpeechConfig {
    /// Model to use for speech synthesis: a `provider:model` id (e.g.
    /// `mistral:voxtral-mini-tts-latest`) or a bare provider name (e.g.
    /// `openai`) to use that provider's default model.
    pub model: Option<String>,
    /// Voice to use (provider-specific)
    pub voice: Option<String>,
    /// Output audio format (e.g. "mp3")
    pub format: Option<String>,
}

/// Turn-completion notification configuration for the interactive TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Emit OSC escape sequences for turn state: an OSC 9 desktop notification
    /// when a turn finishes, plus a window/tab title showing the thread title
    /// with an animated spinner while a turn runs. cmux, Ghostty, kitty, and
    /// `WezTerm` surface these; other terminals ignore them.
    pub osc: bool,
    /// Also drive cmux sidebar integration via the `cmux` CLI: a per-instance
    /// status pill showing an animated spinner with the thread title while a
    /// turn runs (settling to the bare title when complete, or `✗` on failure)
    /// plus a `todo_write` progress bar. No-op when `cmux` is not on `PATH`.
    pub cmux_status: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            osc: true,
            cmux_status: false,
        }
    }
}

/// qmd search backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QmdConfig {
    /// qmd command path or binary name.
    ///
    /// Defaults to `qmd` on `PATH`.
    #[serde(
        default = "default_qmd_command",
        deserialize_with = "deserialize_qmd_command"
    )]
    pub command: String,
}

impl Default for QmdConfig {
    fn default() -> Self {
        Self {
            command: default_qmd_command(),
        }
    }
}

/// Memory system configuration.
///
/// Configures the root directory for memory storage.
/// ZDX derives notes, calendar, and index paths under this root.
/// Defaults to `$ZDX_HOME/memory/` when unconfigured.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    /// Root directory for memory storage.
    /// Supports `~` for home directory expansion.
    /// Must not be blank when provided.
    /// Must be an absolute path or use `~/...`.
    /// Default: `$ZDX_HOME/memory`
    #[serde(default, deserialize_with = "deserialize_memory_root")]
    pub root: Option<String>,
}

impl MemoryConfig {
    /// Returns the effective memory root path using an explicit `ZDX_HOME`
    /// fallback instead of reading process-global environment.
    pub(crate) fn effective_root_path_with_zdx_home(&self, zdx_home: &Path) -> PathBuf {
        self.root
            .as_deref()
            .map_or_else(|| zdx_home.join("memory"), expand_tilde)
    }

    /// Returns the effective memory root path, expanding `~` and falling back to default.
    pub fn effective_root_path(&self) -> std::path::PathBuf {
        self.effective_root_path_with_zdx_home(&paths::zdx_home())
    }

    /// Returns the effective notes path derived from the memory root.
    pub fn effective_notes_path(&self) -> std::path::PathBuf {
        self.effective_root_path().join("Notes")
    }

    /// Returns the effective daily notes path derived from the memory root.
    pub fn effective_daily_path(&self) -> std::path::PathBuf {
        self.effective_root_path().join("Calendar")
    }

    /// Returns the effective memory index file path derived from the memory root.
    pub fn effective_index_file(&self) -> std::path::PathBuf {
        self.effective_notes_path().join("MEMORY.md")
    }
}

/// Expands `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = paths::home_dir() {
            return home.join(rest);
        }
    } else if path == "~"
        && let Some(home) = paths::home_dir()
    {
        return home;
    }
    std::path::PathBuf::from(path)
}

/// A favorite model preset cycled with Tab in the TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFavorite {
    pub alias: String,
    /// Model id, with or without a `provider:` prefix.
    pub model: String,
    #[serde(default)]
    pub thinking: ThinkingLevel,
}

impl ModelFavorite {
    /// True if this favorite matches the given model + thinking. Prefixed
    /// (`provider:id`) and bare model ids are treated as equivalent.
    #[must_use]
    pub fn matches(&self, model: &str, thinking: ThinkingLevel) -> bool {
        self.thinking == thinking && models_equivalent(&self.model, model)
    }

    /// True if this favorite's model resolves to one of the given available
    /// model ids. Bare and `provider:`-prefixed ids are treated as equivalent.
    #[must_use]
    pub fn model_available(&self, available: &[String]) -> bool {
        available.iter().any(|m| models_equivalent(&self.model, m))
    }
}

/// Bare and `provider:`-prefixed ids are equivalent when they resolve to the
/// same provider + model.
fn models_equivalent(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let ra = crate::providers::resolve_provider(a);
    let rb = crate::providers::resolve_provider(b);
    ra.kind == rb.kind && ra.model == rb.model
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

    /// Model to use for handoff generation subagent.
    #[serde(default = "default_handoff_model")]
    pub handoff_model: String,

    /// Model to use for auto-title generation subagent.
    #[serde(default = "default_title_model")]
    pub title_model: String,

    /// Model to use for `read_thread` subagent.
    #[serde(default = "default_read_thread_model")]
    pub read_thread_model: String,

    /// Model to use for thread TLDR generation subagent.
    #[serde(default = "default_tldr_model")]
    pub tldr_model: String,

    /// Thinking level for extended thinking feature
    #[serde(default)]
    pub thinking_level: ThinkingLevel,

    /// Favorite model presets cycled with Tab in the TUI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub favorites: Vec<ModelFavorite>,

    /// Skill discovery configuration
    #[serde(default)]
    pub skills: SkillsConfig,

    /// Subagent delegation configuration
    #[serde(default)]
    pub subagents: SubagentsConfig,

    /// System prompt template rendering configuration
    #[serde(default)]
    pub prompt_template: PromptTemplateConfig,

    /// Memory system configuration (root directory with derived notes/calendar/index paths)
    #[serde(default)]
    pub memory: MemoryConfig,

    /// Shared audio transcription configuration for TUI/CLI and as the default for integrations.
    #[serde(default)]
    pub transcription: TranscriptionConfig,

    /// Shared text-to-speech configuration for the `zdx speak` CLI and bot audio replies.
    #[serde(default)]
    pub speech: SpeechConfig,

    /// qmd search backend configuration.
    #[serde(default)]
    pub qmd: QmdConfig,

    /// Turn-completion notification configuration for the interactive TUI.
    #[serde(default)]
    pub notifications: NotificationsConfig,

    /// Telegram bot configuration
    #[serde(default)]
    pub telegram: TelegramConfig,
}

impl Config {
    const DEFAULT_MODEL: &str = "claude-haiku-4-5";
    const DEFAULT_MAX_TOKENS: u32 = 12288;
    /// Default is disabled
    const DEFAULT_TOOL_TIMEOUT_SECS: u32 = 0;
    const DEFAULT_HANDOFF_MODEL: &str = "gemini:gemini-3-flash-preview";
    const DEFAULT_TITLE_MODEL: &str = "gemini:gemini-3.1-flash-lite-preview";
    const DEFAULT_READ_THREAD_MODEL: &str = "gemini:gemini-3.1-flash-lite-preview";
    const DEFAULT_TLDR_MODEL: &str = "gemini:gemini-3.1-flash-lite-preview";

    /// Loads configuration from the default config path.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load() -> Result<Self> {
        Self::load_from(&paths::config_path())
    }

    /// Alias of the favorite matching the active model + thinking, if any.
    #[must_use]
    pub fn active_favorite_alias(&self) -> Option<&str> {
        self.favorites
            .iter()
            .find(|fav| fav.matches(&self.model, self.thinking_level))
            .map(|fav| fav.alias.as_str())
    }

    /// Resolves Telegram runtime credentials/settings from config + environment.
    ///
    /// # Errors
    /// Returns an error if the bot token or required allowlist is missing.
    pub fn resolve_telegram_runtime(&self) -> Result<ResolvedTelegramRuntime> {
        let bot_token = self
            .telegram
            .bot_token
            .as_deref()
            .and_then(normalize_non_empty)
            .or_else(|| {
                std::env::var("ZDX_TELEGRAM_BOT_TOKEN")
                    .ok()
                    .and_then(|token| normalize_non_empty(&token))
            })
            .or_else(|| {
                std::env::var("TELEGRAM_BOT_TOKEN")
                    .ok()
                    .and_then(|token| normalize_non_empty(&token))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("telegram.bot_token or ZDX_TELEGRAM_BOT_TOKEN is required")
            })?;

        if self.telegram.allowlist_user_ids.is_empty() {
            bail!("telegram.allowlist_user_ids must contain at least one user ID");
        }

        self.validate_telegram_profiles()?;

        let mut allowlist_chat_ids = self.telegram.allowlist_chat_ids.clone();
        for profile in self.telegram.profiles.values() {
            if !allowlist_chat_ids.contains(&profile.chat_id) {
                allowlist_chat_ids.push(profile.chat_id);
            }
        }

        Ok(ResolvedTelegramRuntime {
            bot_token,
            allowlist_user_ids: self.telegram.allowlist_user_ids.clone(),
            allowlist_chat_ids,
        })
    }

    /// Returns a Telegram profile by chat ID.
    #[must_use]
    pub fn telegram_profile_for_chat(
        &self,
        chat_id: i64,
    ) -> Option<(&str, &TelegramProfileConfig)> {
        self.telegram.profiles.iter().find_map(|(name, profile)| {
            (profile.chat_id == chat_id).then_some((name.as_str(), profile))
        })
    }

    /// Validates the Telegram profile map.
    ///
    /// # Errors
    /// Returns an error if profile names/cwds are blank or chat IDs are duplicated.
    pub fn validate_telegram_profiles(&self) -> Result<()> {
        let mut seen_chat_ids: BTreeMap<i64, &str> = BTreeMap::new();
        for (name, profile) in &self.telegram.profiles {
            if name.trim().is_empty() {
                bail!("telegram profile names must not be blank");
            }
            if profile.cwd.trim().is_empty() {
                bail!("telegram profile '{name}' cwd must not be blank");
            }
            if let Some(existing_name) = seen_chat_ids.insert(profile.chat_id, name) {
                bail!(
                    "telegram profiles '{existing_name}' and '{name}' use duplicate chat ID {}",
                    profile.chat_id
                );
            }
        }
        Ok(())
    }

    /// Saves one Telegram profile to a config file.
    ///
    /// # Errors
    /// Returns an error if the config cannot be read, parsed, validated, or written.
    pub fn save_telegram_profile(name: &str, profile: &TelegramProfileConfig) -> Result<()> {
        Self::save_telegram_profile_to(&paths::config_path(), name, profile)
    }

    /// Saves one Telegram profile to a specific config file path.
    ///
    /// # Errors
    /// Returns an error if the config cannot be read, parsed, validated, or written.
    pub fn save_telegram_profile_to(
        path: &Path,
        name: &str,
        profile: &TelegramProfileConfig,
    ) -> Result<()> {
        use toml_edit::{DocumentMut, Item, Table, value};

        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        // Ensure `[telegram]` is a regular table.
        if !doc.get("telegram").is_some_and(Item::is_table) {
            doc["telegram"] = Item::Table(Table::new());
        }
        let Some(telegram) = doc["telegram"].as_table_mut() else {
            bail!("failed to prepare telegram config table");
        };

        // Ensure `[telegram.profiles]` is a regular table. When created fresh,
        // mark it implicit so we don't emit a bare `[telegram.profiles]`
        // header alongside the per-profile sub-tables.
        if !telegram.get("profiles").is_some_and(Item::is_table) {
            let mut profiles = Table::new();
            profiles.set_implicit(true);
            telegram["profiles"] = Item::Table(profiles);
        }
        let Some(profiles) = telegram["profiles"].as_table_mut() else {
            bail!("failed to prepare telegram profiles config table");
        };

        // Insert this profile as its own sub-table so it serializes as
        // `[telegram.profiles.<name>]` instead of an inline-table sibling.
        let mut entry = Table::new();
        entry["chat_id"] = value(profile.chat_id);
        entry["cwd"] = value(profile.cwd.trim());
        profiles[name] = Item::Table(entry);

        Self::write_config(path, &doc.to_string())
    }

    /// Saves the global Telegram bot identity/settings to a config file.
    ///
    /// # Errors
    /// Returns an error if the config cannot be read, parsed, or written.
    pub fn save_telegram_bot_settings(
        bot_token: &str,
        allowlist_user_ids: &[i64],
        model: &str,
        thinking_level: ThinkingLevel,
    ) -> Result<()> {
        Self::save_telegram_bot_settings_to(
            &paths::config_path(),
            bot_token,
            allowlist_user_ids,
            model,
            thinking_level,
        )
    }

    /// Saves the global Telegram bot identity/settings to a specific config file path.
    ///
    /// # Errors
    /// Returns an error if the config cannot be read, parsed, or written.
    pub fn save_telegram_bot_settings_to(
        path: &Path,
        bot_token: &str,
        allowlist_user_ids: &[i64],
        model: &str,
        thinking_level: ThinkingLevel,
    ) -> Result<()> {
        use toml_edit::{Array, DocumentMut, value};

        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;
        let mut users = Array::new();
        for id in allowlist_user_ids {
            users.push(*id);
        }

        doc["telegram"]["bot_token"] = value(bot_token.trim());
        doc["telegram"]["allowlist_user_ids"] = value(users);
        doc["telegram"]["model"] = value(model.trim());
        doc["telegram"]["thinking_level"] = value(thinking_level.display_name());

        Self::write_config(path, &doc.to_string())
    }

    /// Loads configuration from a specific path.
    /// Returns defaults if file doesn't exist.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
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
    /// Preserves existing fields and comments using `toml_edit`.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_model(model: &str) -> Result<()> {
        Self::save_model_to(&paths::config_path(), model)
    }

    /// Saves only the model field to a specific config file path.
    ///
    /// Creates the file with default template if it doesn't exist.
    /// If file exists, merges user values into the latest template.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_model_to(path: &Path, model: &str) -> Result<()> {
        Self::save_model_field_to(path, "model", model)
    }

    /// Top-level model fields that can be edited from the monitor Config tab.
    pub const EDITABLE_MODEL_FIELDS: &'static [&'static str] = &[
        "model",
        "title_model",
        "tldr_model",
        "handoff_model",
        "read_thread_model",
        "transcription.model",
        "speech.model",
    ];

    /// Saves a single top-level model field (one of [`Self::EDITABLE_MODEL_FIELDS`]).
    ///
    /// # Errors
    /// Returns an error if the field is not editable or the write fails.
    pub fn save_model_field(field: &str, model: &str) -> Result<()> {
        Self::save_model_field_to(&paths::config_path(), field, model)
    }

    /// Saves a single top-level model field to a specific config path,
    /// preserving existing fields and comments via `toml_edit`.
    ///
    /// # Errors
    /// Returns an error if the field is not editable or the write fails.
    pub fn save_model_field_to(path: &Path, field: &str, model: &str) -> Result<()> {
        use toml_edit::{DocumentMut, Item, value};

        anyhow::ensure!(
            Self::EDITABLE_MODEL_FIELDS.contains(&field),
            "not an editable model field: {field}"
        );

        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        // Support one level of nesting (e.g. `transcription.model`).
        match field.split_once('.') {
            Some((table, key)) => {
                if doc.get(table).and_then(Item::as_table).is_none() {
                    doc[table] = Item::Table(toml_edit::Table::new());
                }
                doc[table][key] = value(model);
            }
            None => doc[field] = value(model),
        }

        Self::write_config(path, &doc.to_string())
    }

    /// Saves only the `telegram.model` field to the config file.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_telegram_model(model: &str) -> Result<()> {
        Self::save_telegram_model_to(&paths::config_path(), model)
    }

    /// Saves only the `telegram.model` field to a specific config file path.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_telegram_model_to(path: &Path, model: &str) -> Result<()> {
        use toml_edit::{DocumentMut, value};

        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        doc["telegram"]["model"] = value(model);

        Self::write_config(path, &doc.to_string())
    }

    /// Saves only the `telegram.thinking_level` field to the config file.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_telegram_thinking_level(level: ThinkingLevel) -> Result<()> {
        Self::save_telegram_thinking_level_to(&paths::config_path(), level)
    }

    /// Saves only the `telegram.thinking_level` field to a specific config file path.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_telegram_thinking_level_to(path: &Path, level: ThinkingLevel) -> Result<()> {
        use toml_edit::{DocumentMut, value};

        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        doc["telegram"]["thinking_level"] = value(level.display_name());

        Self::write_config(path, &doc.to_string())
    }

    /// Saves only the `thinking_level` field to the config file.
    ///
    /// Creates the file if it doesn't exist.
    /// Preserves existing fields and comments using `toml_edit`.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_thinking_level(level: ThinkingLevel) -> Result<()> {
        Self::save_thinking_level_to(&paths::config_path(), level)
    }

    /// Saves only the `thinking_level` field to a specific config file path.
    ///
    /// Creates the file with default template if it doesn't exist.
    /// If file exists, merges user values into the latest template.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
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

    /// Saves the provider-specific fast mode flag to the default config file.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_fast_mode_for_provider(
        provider: crate::providers::ProviderKind,
        enabled: bool,
    ) -> Result<()> {
        Self::save_provider_fast_mode_to(
            &paths::config_path(),
            provider_fast_mode_key(provider),
            enabled,
        )
    }

    /// Returns whether fast mode is enabled for a provider.
    #[must_use]
    pub fn fast_mode_for_provider(&self, provider: crate::providers::ProviderKind) -> bool {
        self.providers.get(provider).fast_mode
    }

    /// Updates the fast mode flag for a provider in memory.
    pub fn set_fast_mode_for_provider(
        &mut self,
        provider: crate::providers::ProviderKind,
        enabled: bool,
    ) {
        self.providers.get_mut(provider).fast_mode = enabled;
    }

    fn save_provider_fast_mode_to(path: &Path, provider_key: &str, enabled: bool) -> Result<()> {
        use toml_edit::{DocumentMut, value};

        let contents = if path.exists() {
            let user_config = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            merge_with_template(&user_config)?
        } else {
            default_config_template().to_string()
        };

        let mut doc: DocumentMut = contents
            .parse()
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        doc["providers"][provider_key]["fast_mode"] = value(enabled);

        Self::write_config(path, &doc.to_string())
    }

    /// Returns the effective system prompt, preferring the file if both are set.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn effective_system_prompt(&self) -> Result<Option<String>> {
        if let Some(path_str) = &self.system_prompt_file {
            let path = Path::new(path_str);
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read system prompt file: {path_str}"))?;
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
            Some(Duration::from_secs(u64::from(self.tool_timeout_secs)))
        }
    }

    /// Returns the path to the models file.
    /// Defaults to `<base>/models.toml`.
    pub fn models_path(&self) -> std::path::PathBuf {
        let base = paths::zdx_home();
        base.join("models.toml")
    }

    /// Returns all model ids available for subagent model overrides.
    ///
    /// Mirrors the TUI model picker source of truth:
    /// - model registry from `models.toml` / `default_models.toml`
    /// - filtered by enabled providers in config
    #[must_use]
    pub fn subagent_available_models(&self) -> Vec<String> {
        self.subagent_available_models_from(
            crate::models::available_models()
                .iter()
                .map(|model| (model.provider, model.id)),
        )
    }

    /// Filters candidate `(provider, model_id)` pairs down to the ones whose
    /// provider is enabled in this config, returning deduplicated
    /// `provider:id` strings.
    ///
    /// Pure over its inputs: used by [`Self::subagent_available_models`] with
    /// the global model registry, and directly by tests with a fixed list so
    /// the filter can be exercised without reading `$ZDX_HOME/models.toml`.
    fn subagent_available_models_from<'a>(
        &self,
        models: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<String> {
        use std::collections::HashSet;

        let enabled_providers: HashSet<&str> = crate::providers::ProviderKind::all()
            .iter()
            .filter(|kind| self.providers.is_enabled(kind.id()))
            .map(|kind| crate::providers::ProviderKind::id(*kind))
            .collect();

        let mut seen = HashSet::new();
        models
            .into_iter()
            .filter(|&(provider, _)| enabled_providers.contains(provider))
            .filter_map(|(provider, id)| {
                let full = format!("{provider}:{id}");
                seen.insert(full.to_ascii_lowercase()).then_some(full)
            })
            .collect()
    }

    /// Returns the effective `max_tokens` fallback for a model.
    ///
    /// Resolution order:
    /// 1) Explicit config `max_tokens` (if set)
    /// 2) Model output limit from the registry (exclusive, minus 1)
    /// 3) Fallback default
    ///
    /// Callers may still omit max tokens for providers that support provider-side defaults.
    pub fn effective_max_tokens_for(&self, model_id: &str) -> u32 {
        let configured = self.max_tokens;
        let model = crate::models::ModelOption::find_by_id(model_id);
        let output_limit = model
            .map(|model| model.capabilities.output_limit)
            .filter(|limit| *limit > 0)
            .and_then(|limit| u32::try_from(limit).ok());
        let context_limit = model
            .map(|model| model.context_limit)
            .filter(|limit| *limit > 0)
            .and_then(|limit| u32::try_from(limit).ok());
        let output_limit_exclusive = output_limit
            .and_then(|limit| limit.checked_sub(1))
            .filter(|limit| *limit > 0);
        let context_limit_exclusive = context_limit
            .and_then(|limit| limit.checked_sub(1))
            .filter(|limit| *limit > 0);

        let suspicious_output_limit = output_limit_exclusive
            .zip(context_limit_exclusive)
            .is_some_and(|(output, context)| output >= context);

        let max_tokens = configured
            .or({
                if suspicious_output_limit {
                    Some(Self::DEFAULT_MAX_TOKENS)
                } else {
                    output_limit_exclusive
                }
            })
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
    ///
    /// # Errors
    /// Returns an error if the operation fails.
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
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn generate() -> Result<String> {
        use toml_edit::DocumentMut;

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
        merge_generated_items(doc.as_table_mut(), generated_doc.as_table());

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
            handoff_model: Self::DEFAULT_HANDOFF_MODEL.to_string(),
            title_model: Self::DEFAULT_TITLE_MODEL.to_string(),
            read_thread_model: Self::DEFAULT_READ_THREAD_MODEL.to_string(),
            tldr_model: Self::DEFAULT_TLDR_MODEL.to_string(),
            thinking_level: ThinkingLevel::default(),
            favorites: Vec::new(),
            skills: SkillsConfig::default(),
            subagents: SubagentsConfig::default(),
            prompt_template: PromptTemplateConfig::default(),
            memory: MemoryConfig::default(),
            transcription: TranscriptionConfig::default(),
            speech: SpeechConfig::default(),
            qmd: QmdConfig::default(),
            notifications: NotificationsConfig::default(),
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
    #[serde(default = "default_deepseek_provider")]
    pub deepseek: ProviderConfig,
    #[serde(default = "default_moonshot_provider")]
    pub moonshot: ProviderConfig,
    #[serde(default = "default_stepfun_provider")]
    pub stepfun: ProviderConfig,
    #[serde(default = "default_lmstudio_provider")]
    pub lmstudio: ProviderConfig,
    #[serde(default = "default_xiaomi_provider")]
    pub xiaomi: ProviderConfig,
    #[serde(default = "default_xiaomi_plan_provider")]
    pub xiaomi_plan: ProviderConfig,
    #[serde(default = "default_gemini_provider")]
    pub gemini: ProviderConfig,
    #[serde(default = "default_google_antigravity_provider")]
    pub google_antigravity: ProviderConfig,
    #[serde(default = "default_mistral_provider")]
    pub mistral: ProviderConfig,
    #[serde(default = "default_opencode_go_provider")]
    pub opencode_go: ProviderConfig,
    #[serde(default = "default_minimax_provider")]
    pub minimax: ProviderConfig,
    #[serde(default = "default_zai_provider")]
    pub zai: ProviderConfig,
    #[serde(default = "default_xai_provider")]
    pub xai: ProviderConfig,
    #[serde(default = "default_grok_build_provider")]
    pub grok_build: ProviderConfig,
    #[serde(default = "default_meta_provider")]
    pub meta: ProviderConfig,
    #[serde(default = "default_elevenlabs_provider")]
    pub elevenlabs: ProviderConfig,
    /// User-defined OpenAI-compatible providers, keyed by name. Used by
    /// prefixing the model with the name (e.g. `<name>:model-id`).
    #[serde(default)]
    pub custom: std::collections::HashMap<String, CustomProviderConfig>,
}

impl ProvidersConfig {
    /// Returns whether a provider is enabled by its string identifier.
    ///
    /// Provider IDs match the model registry format (e.g., "anthropic", "openai", "gemini").
    /// Returns true if the provider is not found (unknown providers default to enabled).
    pub fn is_enabled(&self, provider_id: &str) -> bool {
        use crate::providers::ProviderKind;

        let config = match provider_id {
            id if id == ProviderKind::Anthropic.id() => &self.anthropic,
            id if id == ProviderKind::ClaudeCli.id() => &self.claude_cli,
            id if id == ProviderKind::OpenAI.id() => &self.openai,
            id if id == ProviderKind::OpenAICodex.id() => &self.openai_codex,
            id if id == ProviderKind::OpenRouter.id() => &self.openrouter,
            id if id == ProviderKind::DeepSeek.id() => &self.deepseek,
            id if id == ProviderKind::Moonshot.id() => &self.moonshot,
            id if id == ProviderKind::Stepfun.id() => &self.stepfun,
            id if id == ProviderKind::LMStudio.id() => &self.lmstudio,
            id if id == ProviderKind::Xiaomi.id() => &self.xiaomi,
            id if id == ProviderKind::XiaomiPlan.id() => &self.xiaomi_plan,
            id if id == ProviderKind::Gemini.id() => &self.gemini,
            id if id == ProviderKind::GoogleAntigravity.id() => &self.google_antigravity,
            id if id == ProviderKind::OpencodeGo.id() => &self.opencode_go,
            id if id == ProviderKind::Minimax.id() => &self.minimax,
            id if id == ProviderKind::Zai.id() => &self.zai,
            id if id == ProviderKind::Xai.id() => &self.xai,
            id if id == ProviderKind::GrokBuild.id() => &self.grok_build,
            id if id == ProviderKind::Meta.id() => &self.meta,
            id if id == ProviderKind::ElevenLabs.id() => &self.elevenlabs,
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
            ProviderKind::DeepSeek => &self.deepseek,
            ProviderKind::Mistral => &self.mistral,
            ProviderKind::Moonshot => &self.moonshot,
            ProviderKind::Stepfun => &self.stepfun,
            ProviderKind::LMStudio => &self.lmstudio,
            ProviderKind::Xiaomi => &self.xiaomi,
            ProviderKind::XiaomiPlan => &self.xiaomi_plan,
            ProviderKind::Gemini => &self.gemini,
            ProviderKind::GoogleAntigravity => &self.google_antigravity,
            ProviderKind::OpencodeGo => &self.opencode_go,
            ProviderKind::Minimax => &self.minimax,
            ProviderKind::Zai => &self.zai,
            ProviderKind::Xai => &self.xai,
            ProviderKind::GrokBuild => &self.grok_build,
            ProviderKind::Meta => &self.meta,
            ProviderKind::ElevenLabs => &self.elevenlabs,
        }
    }

    /// Returns the mutable provider config for a given provider kind.
    pub fn get_mut(&mut self, kind: crate::providers::ProviderKind) -> &mut ProviderConfig {
        use crate::providers::ProviderKind;

        match kind {
            ProviderKind::Anthropic => &mut self.anthropic,
            ProviderKind::ClaudeCli => &mut self.claude_cli,
            ProviderKind::OpenAI => &mut self.openai,
            ProviderKind::OpenAICodex => &mut self.openai_codex,
            ProviderKind::OpenRouter => &mut self.openrouter,
            ProviderKind::DeepSeek => &mut self.deepseek,
            ProviderKind::Mistral => &mut self.mistral,
            ProviderKind::Moonshot => &mut self.moonshot,
            ProviderKind::Stepfun => &mut self.stepfun,
            ProviderKind::LMStudio => &mut self.lmstudio,
            ProviderKind::Xiaomi => &mut self.xiaomi,
            ProviderKind::XiaomiPlan => &mut self.xiaomi_plan,
            ProviderKind::Gemini => &mut self.gemini,
            ProviderKind::GoogleAntigravity => &mut self.google_antigravity,
            ProviderKind::OpencodeGo => &mut self.opencode_go,
            ProviderKind::Minimax => &mut self.minimax,
            ProviderKind::Zai => &mut self.zai,
            ProviderKind::Xai => &mut self.xai,
            ProviderKind::GrokBuild => &mut self.grok_build,
            ProviderKind::Meta => &mut self.meta,
            ProviderKind::ElevenLabs => &mut self.elevenlabs,
        }
    }

    /// Resolves a `name:model` / `name/model` prefix to a configured custom
    /// provider (`[providers.custom.<name>]`), returning its config + bare model.
    pub fn custom_provider_for_model<'a>(
        &'a self,
        model: &str,
    ) -> Option<(&'a CustomProviderConfig, String)> {
        let trimmed = model.trim();
        for sep in [':', '/'] {
            if let Some((prefix, rest)) = trimmed.split_once(sep) {
                let prefix = prefix.trim();
                let rest = rest.trim();
                if !rest.is_empty()
                    && let Some(cfg) = self.custom.get(prefix)
                {
                    return Some((cfg, rest.to_string()));
                }
            }
        }
        None
    }
}

fn provider_fast_mode_key(provider: crate::providers::ProviderKind) -> &'static str {
    match provider {
        crate::providers::ProviderKind::OpenAI => "openai",
        crate::providers::ProviderKind::OpenAICodex => "openai_codex",
        _ => unreachable!("fast mode is only supported for OpenAI providers"),
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
            google_antigravity: default_google_antigravity_provider(),
            openrouter: default_openrouter_provider(),
            deepseek: default_deepseek_provider(),
            moonshot: default_moonshot_provider(),
            stepfun: default_stepfun_provider(),
            lmstudio: default_lmstudio_provider(),
            xiaomi: default_xiaomi_provider(),
            xiaomi_plan: default_xiaomi_plan_provider(),
            mistral: default_mistral_provider(),
            opencode_go: default_opencode_go_provider(),
            minimax: default_minimax_provider(),
            zai: default_zai_provider(),
            xai: default_xai_provider(),
            grok_build: default_grok_build_provider(),
            meta: default_meta_provider(),
            elevenlabs: default_elevenlabs_provider(),
            custom: std::collections::HashMap::new(),
        }
    }
}

fn default_anthropic_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "claude-fable-5".to_string(),
            "claude-opus-4-8".to_string(),
            "claude-sonnet-5".to_string(),
            "claude-haiku-4-5".to_string(),
        ],
        ..Default::default()
    }
}

fn default_claude_cli_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "claude-fable-5".to_string(),
            "claude-opus-4-8".to_string(),
            "claude-sonnet-5".to_string(),
            "claude-haiku-4-5".to_string(),
        ],
        ..Default::default()
    }
}

fn default_openai_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gpt-5.6-sol".to_string(),
            "gpt-5.6-terra".to_string(),
            "gpt-5.6-luna".to_string(),
            "gpt-5.5".to_string(),
            "gpt-5.4-nano".to_string(),
        ],
        ..Default::default()
    }
}

fn default_openai_codex_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gpt-5.6-sol".to_string(),
            "gpt-5.6-terra".to_string(),
            "gpt-5.6-luna".to_string(),
            "gpt-5.5".to_string(),
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

fn default_deepseek_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "deepseek-v4-pro".to_string(),
            "deepseek-v4-flash".to_string(),
        ],
        ..Default::default()
    }
}

fn default_moonshot_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["kimi-k2.6".to_string()],
        ..Default::default()
    }
}

fn default_meta_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["muse-spark-1.1".to_string()],
        ..Default::default()
    }
}

fn default_stepfun_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["step-3.7-flash".to_string(), "step-3.5-flash".to_string()],
        ..Default::default()
    }
}

fn default_lmstudio_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["google/gemma-4-e4b".to_string()],
        ..Default::default()
    }
}

fn default_xiaomi_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["mimo-v2.5-pro".to_string(), "mimo-v2.5".to_string()],
        ..Default::default()
    }
}

fn default_xiaomi_plan_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(false),
        models: vec!["mimo-v2.5-pro".to_string(), "mimo-v2.5".to_string()],
        ..Default::default()
    }
}

fn default_gemini_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gemini-3.5-flash".to_string(),
            "gemini-3.1-pro-preview".to_string(),
            "gemini-3.1-flash-lite-preview".to_string(),
        ],
        ..Default::default()
    }
}

fn default_google_antigravity_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "gemini-3.5-flash-low".to_string(),
            "gemini-3-flash-agent".to_string(),
            "gemini-3.1-pro-low".to_string(),
            "gemini-3.1-pro-high".to_string(),
            "claude-sonnet-4-6".to_string(),
            "claude-opus-4-6-thinking".to_string(),
            "gpt-oss-120b-medium".to_string(),
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

fn default_opencode_go_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec![
            "glm-5.2".to_string(),
            "kimi-k2.6".to_string(),
            "kimi-k2.7-code".to_string(),
            "mimo-v2.5-pro".to_string(),
            "mimo-v2.5".to_string(),
            "minimax-m3".to_string(),
            "qwen3.7-max".to_string(),
            "qwen3.7-plus".to_string(),
            "deepseek-v4-pro".to_string(),
            "deepseek-v4-flash".to_string(),
        ],
        ..Default::default()
    }
}

fn default_minimax_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["MiniMax-M3".to_string()],
        ..Default::default()
    }
}

fn default_zai_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["glm-5.2".to_string(), "glm-4.7-flash".to_string()],
        ..Default::default()
    }
}

fn default_xai_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["grok-4.5".to_string()],
        ..Default::default()
    }
}

fn default_grok_build_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
        models: vec!["grok-4.5".to_string()],
        ..Default::default()
    }
}

fn default_elevenlabs_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: Some(true),
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
    /// Optional text verbosity for `OpenAI` Responses-compatible providers.
    pub text_verbosity: Option<TextVerbosity>,
    /// Whether this provider is enabled for `zdx models update`.
    pub enabled: Option<bool>,
    /// Desired models for `zdx models update` (supports '*' wildcard).
    pub models: Vec<String>,
    /// Explicit list of enabled tools (if set, only these tools are used).
    /// If unset, all tools are available.
    pub tools: Option<Vec<String>>,
    /// Enable fast mode (`service_tier: "priority"`) for `OpenAI` Responses API (2× cost).
    #[serde(default)]
    pub fast_mode: bool,
    /// Use the persistent WebSocket transport for the `OpenAI` Responses API.
    #[serde(default)]
    pub websocket: bool,
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

    /// Returns the effective text verbosity if configured.
    #[must_use]
    pub fn effective_text_verbosity(&self) -> Option<TextVerbosity> {
        self.text_verbosity
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

/// A user-defined OpenAI-compatible provider (`[providers.custom.<name>]`),
/// e.g. a self-hosted `LiteLLM` proxy. The chat-completions path is appended
/// to `base_url`, so point it at the OpenAI-compatible root
/// (e.g. `https://llm.example.com/v1`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CustomProviderConfig {
    pub base_url: String,
    /// Inline API key; takes precedence over `api_key_env`.
    pub api_key: Option<String>,
    /// Env var to read the API key from when `api_key` is unset.
    pub api_key_env: Option<String>,
    /// Model allow-list shown in the picker (supports `*`).
    pub models: Vec<String>,
}

impl CustomProviderConfig {
    /// Trimmed base URL without a trailing slash.
    ///
    /// # Errors
    /// Returns an error if `base_url` is empty.
    pub fn effective_base_url(&self) -> anyhow::Result<String> {
        let url = self.base_url.trim().trim_end_matches('/');
        if url.is_empty() {
            anyhow::bail!("custom provider `base_url` must not be empty");
        }
        Ok(url.to_string())
    }

    /// Resolves the API key from `api_key`, then `api_key_env`.
    ///
    /// # Errors
    /// Returns an error if neither source yields a non-empty key.
    pub fn resolve_api_key(&self) -> anyhow::Result<String> {
        if let Some(key) = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(key.to_string());
        }
        if let Some(env) = self
            .api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let value = std::env::var(env).unwrap_or_default();
            let value = value.trim();
            if !value.is_empty() {
                return Ok(value.to_string());
            }
            anyhow::bail!("custom provider API key env var `{env}` is not set");
        }
        anyhow::bail!("custom provider requires `api_key` or `api_key_env`");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    /// Custom providers: a model prefixed with a configured custom-provider
    /// name resolves to that provider config + bare model id.
    #[test]
    fn test_custom_provider_resolution() {
        let mut providers = ProvidersConfig::default();
        providers.custom.insert(
            "myproxy".to_string(),
            CustomProviderConfig {
                base_url: "https://llm.example.com/v1".to_string(),
                api_key: Some("sk-test".to_string()),
                api_key_env: None,
                models: vec!["model-a".to_string()],
            },
        );

        // Both `:` and `/` separators resolve.
        for model in ["myproxy:model-a", "myproxy/model-a"] {
            let (cfg, bare) = providers
                .custom_provider_for_model(model)
                .expect("custom provider should resolve");
            assert_eq!(bare, "model-a");
            assert_eq!(cfg.base_url, "https://llm.example.com/v1");
        }

        // Unknown prefix and empty bare model do not resolve.
        assert!(
            providers
                .custom_provider_for_model("openai:gpt-5")
                .is_none()
        );
        assert!(providers.custom_provider_for_model("myproxy:").is_none());
        assert!(providers.custom_provider_for_model("model-a").is_none());
    }

    /// Custom providers: base URL trims trailing slash; empty errors.
    #[test]
    fn test_custom_provider_base_url_and_api_key() {
        let cfg = CustomProviderConfig {
            base_url: "https://llm.example.com/v1/".to_string(),
            api_key: Some("  sk-test  ".to_string()),
            api_key_env: None,
            models: vec![],
        };
        assert_eq!(
            cfg.effective_base_url().unwrap(),
            "https://llm.example.com/v1"
        );
        assert_eq!(cfg.resolve_api_key().unwrap(), "sk-test");

        let empty = CustomProviderConfig::default();
        assert!(empty.effective_base_url().is_err());
        assert!(empty.resolve_api_key().is_err());
    }

    /// Custom providers parse from a real config file.
    #[test]
    fn test_custom_provider_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"model = "myproxy:model-a"

[providers.custom.myproxy]
base_url = "https://llm.example.com/v1"
api_key = "sk-test"
models = ["model-a", "model-b"]
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.model, "myproxy:model-a");
        let (cfg, bare) = config
            .providers
            .custom_provider_for_model(&config.model)
            .expect("custom provider should resolve from loaded config");
        assert_eq!(bare, "model-a");
        assert_eq!(cfg.models, vec!["model-a", "model-b"]);
        // Built-in providers remain intact alongside custom ones.
        assert!(config.providers.is_enabled("anthropic"));
    }

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
        assert!(config.transcription.model.is_none());
        assert!(config.transcription.language.is_none());
        assert_eq!(config.qmd.command, "qmd");
    }

    #[test]
    fn test_top_level_transcription_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(
            &config_path,
            r#"[transcription]
model = "elevenlabs:scribe_v2"
language = "pt"
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.transcription.model.as_deref(),
            Some("elevenlabs:scribe_v2")
        );
        assert_eq!(config.transcription.language.as_deref(), Some("pt"));
    }

    #[test]
    fn test_qmd_command_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "[qmd]\ncommand = \"/opt/bin/qmd\"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.qmd.command, "/opt/bin/qmd");
    }

    #[test]
    fn test_blank_qmd_command_is_rejected() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "[qmd]\ncommand = \"   \"\n").unwrap();

        let error = Config::load_from(&config_path).unwrap_err();
        let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();

        assert!(
            chain
                .iter()
                .any(|message| message.contains("must not be blank")),
            "expected blank-value error in chain, got: {chain:?}"
        );
    }

    #[test]
    fn test_memory_root_loads_and_derives_paths() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let memory_root = dir.path().join("memory-root");

        fs::write(
            &config_path,
            format!(
                "[memory]\nroot = {:?}\n",
                memory_root.to_string_lossy().to_string()
            ),
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.memory.effective_root_path(), memory_root);
        assert_eq!(
            config.memory.effective_notes_path(),
            dir.path().join("memory-root").join("Notes")
        );
        assert_eq!(
            config.memory.effective_daily_path(),
            dir.path().join("memory-root").join("Calendar")
        );
        assert_eq!(
            config.memory.effective_index_file(),
            dir.path()
                .join("memory-root")
                .join("Notes")
                .join("MEMORY.md")
        );
    }

    #[test]
    fn test_memory_root_expands_tilde_and_trims_whitespace() {
        let Some(home) = paths::home_dir() else {
            return;
        };

        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "[memory]\nroot = \"  ~/SecondBrain  \"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.memory.effective_root_path(),
            home.join("SecondBrain")
        );
        assert_eq!(
            config.memory.effective_index_file(),
            home.join("SecondBrain").join("Notes").join("MEMORY.md")
        );
    }

    #[test]
    fn test_legacy_memory_fields_are_rejected() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(
            &config_path,
            "[memory]\nnotes_path = \"/tmp/notes\"\ndaily_path = \"/tmp/calendar\"\nindex_file = \"/tmp/MEMORY.md\"\n",
        )
        .unwrap();

        let error = Config::load_from(&config_path).unwrap_err();
        let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();

        assert!(
            chain
                .iter()
                .any(|message| message.contains("unknown field") && message.contains("notes_path")),
            "expected unknown-field error in chain, got: {chain:?}"
        );
    }

    #[test]
    fn test_blank_memory_root_is_rejected() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        for root in ["", "   "] {
            fs::write(&config_path, format!("[memory]\nroot = {root:?}\n")).unwrap();

            let error = Config::load_from(&config_path).unwrap_err();
            let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();

            assert!(
                chain
                    .iter()
                    .any(|message| message.contains("must not be blank")),
                "expected blank-value error in chain for root={root:?}, got: {chain:?}"
            );
        }
    }

    #[test]
    fn test_relative_memory_root_is_rejected() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        for root in ["SecondBrain", "./SecondBrain", "../SecondBrain"] {
            fs::write(&config_path, format!("[memory]\nroot = {root:?}\n")).unwrap();

            let error = Config::load_from(&config_path).unwrap_err();
            let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();

            assert!(
                chain
                    .iter()
                    .any(|message| message.contains("absolute path or use ~/")),
                "expected relative-path error in chain for root={root:?}, got: {chain:?}"
            );
        }
    }

    #[test]
    fn test_skill_source_toggles_accept_bool_values_only() {
        #[derive(Deserialize)]
        struct Wrapper {
            value: bool,
        }

        let parsed_true: Wrapper = toml::from_str("value = true").unwrap();
        let parsed_false: Wrapper = toml::from_str("value = false").unwrap();
        let parsed_on = toml::from_str::<Wrapper>("value = \"on\"");

        assert!(parsed_true.value);
        assert!(!parsed_false.value);
        assert!(parsed_on.is_err());
    }

    #[test]
    fn test_generate_uses_boolean_skill_source_flags() {
        let generated = Config::generate().unwrap();

        assert!(generated.contains("[skills.sources]"));
        assert!(generated.contains("zdx_user = true"));
        assert!(!generated.contains("zdx_user = \"on\""));
        assert!(generated.contains("[transcription]"));
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

    /// `save_model`: creates new config file with template if it doesn't exist.
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

    /// `save_model`: preserves other fields in existing config.
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

    /// `save_model`: uses template structure but preserves user values.
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

    /// `save_model`: creates parent directories if needed.
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

    /// `ThinkingLevel`: `effort_percent` returns correct values.
    #[test]
    fn test_thinking_level_effort_percent() {
        assert_eq!(ThinkingLevel::Off.effort_percent(), None);
        assert_eq!(ThinkingLevel::Low.effort_percent(), Some(20));
        assert_eq!(ThinkingLevel::Medium.effort_percent(), Some(50));
        assert_eq!(ThinkingLevel::High.effort_percent(), Some(80));
        assert_eq!(ThinkingLevel::XHigh.effort_percent(), Some(95));
        assert_eq!(ThinkingLevel::Max.effort_percent(), Some(95));
    }

    /// `ThinkingLevel`: `compute_reasoning_budget` returns correct values.
    #[test]
    fn test_thinking_level_compute_reasoning_budget() {
        // Off returns None
        assert_eq!(ThinkingLevel::Off.compute_reasoning_budget(10000), None);

        // Medium (50%) of 10000 = 5000
        assert_eq!(
            ThinkingLevel::Medium.compute_reasoning_budget(10000),
            Some(5000)
        );

        // High (80%) of 10000 = 8000
        assert_eq!(
            ThinkingLevel::High.compute_reasoning_budget(10000),
            Some(8000)
        );

        // XHigh (95%) of 10000 = 9500
        assert_eq!(
            ThinkingLevel::XHigh.compute_reasoning_budget(10000),
            Some(9500)
        );

        // Low (20%) of 5000 = 1000, but clamped to min 1024
        assert_eq!(
            ThinkingLevel::Low.compute_reasoning_budget(5000),
            Some(1024)
        );

        // No max clamp - XHigh (95%) of 200000 = 190000
        assert_eq!(
            ThinkingLevel::XHigh.compute_reasoning_budget(200_000),
            Some(190_000)
        );
        assert_eq!(
            ThinkingLevel::Max.compute_reasoning_budget(200_000),
            Some(190_000)
        );
    }

    /// `ThinkingLevel`: `display_name` returns short names.
    #[test]
    fn test_thinking_level_display_name() {
        assert_eq!(ThinkingLevel::Off.display_name(), "off");
        assert_eq!(ThinkingLevel::Medium.display_name(), "medium");
        assert_eq!(ThinkingLevel::High.display_name(), "high");
        assert_eq!(ThinkingLevel::Max.display_name(), "max");
    }

    /// `ThinkingLevel`: `all()` returns all levels.
    #[test]
    fn test_thinking_level_all() {
        let all = ThinkingLevel::all();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], ThinkingLevel::Off);
        assert_eq!(all[5], ThinkingLevel::Max);
    }

    /// Thinking: `effective_max_tokens` returns raw value when thinking disabled.
    #[test]
    fn test_effective_max_tokens_thinking_disabled() {
        let config = Config {
            max_tokens: Some(1024),
            thinking_level: ThinkingLevel::Off,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens_for("claude-haiku-4-5"), 1024);
    }

    /// Thinking: `effective_max_tokens` auto-adjusts when thinking enabled and `max_tokens` too low.
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

    /// Thinking: `effective_max_tokens` respects user value when sufficient.
    #[test]
    fn test_effective_max_tokens_respects_high_value() {
        let config = Config {
            max_tokens: Some(20000), // sufficient
            thinking_level: ThinkingLevel::Medium,
            ..Default::default()
        };
        assert_eq!(config.effective_max_tokens_for("claude-haiku-4-5"), 20000);
    }

    /// `effective_max_tokens` uses the model output limit (exclusive) when `max_tokens` is unset.
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

    /// Thinking: config loads from file with `thinking_level`.
    #[test]
    fn test_thinking_config_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, r#"thinking_level = "high""#).unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::High);
        assert!(config.thinking_level.is_enabled());
    }

    /// Thinking: old configs without `thinking_level` use defaults (serde default).
    #[test]
    fn test_thinking_config_missing_uses_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Old config without thinking_level field
        fs::write(&config_path, "model = \"claude-3-opus\"\n").unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::Off);
    }

    /// Favorites: load from `[[favorites]]` and survive an unrelated save.
    #[test]
    fn test_favorites_load_and_survive_save() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"model = "claude-sonnet-4"

[[favorites]]
alias = "sonnet-hi"
model = "anthropic:claude-sonnet-4-6"
thinking = "high"

[[favorites]]
alias = "opus"
model = "anthropic:claude-opus-4-6"
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.favorites.len(), 2);
        assert_eq!(config.favorites[0].alias, "sonnet-hi");
        assert_eq!(config.favorites[0].model, "anthropic:claude-sonnet-4-6");
        assert_eq!(config.favorites[0].thinking, ThinkingLevel::High);
        // thinking defaults to Off when omitted
        assert_eq!(config.favorites[1].thinking, ThinkingLevel::Off);

        // Saving an unrelated field (via template merge) must not drop favorites.
        Config::save_thinking_level_to(&config_path, ThinkingLevel::Medium).unwrap();
        let reloaded = Config::load_from(&config_path).unwrap();
        assert_eq!(reloaded.thinking_level, ThinkingLevel::Medium);
        assert_eq!(reloaded.favorites.len(), 2);
        assert_eq!(reloaded.favorites[1].alias, "opus");
        assert_eq!(reloaded.favorites[0].thinking, ThinkingLevel::High);
    }

    /// Favorites: `active_favorite_alias` matches the active model + thinking,
    /// treating bare and prefixed model ids as equivalent.
    #[test]
    fn test_active_favorite_alias() {
        let config = Config {
            model: "anthropic:claude-sonnet-4-6".to_string(),
            thinking_level: ThinkingLevel::High,
            favorites: vec![
                ModelFavorite {
                    alias: "sonnet-hi".to_string(),
                    // bare id should still match the prefixed active model
                    model: "claude-sonnet-4-6".to_string(),
                    thinking: ThinkingLevel::High,
                },
                ModelFavorite {
                    alias: "opus".to_string(),
                    model: "anthropic:claude-opus-4-6".to_string(),
                    thinking: ThinkingLevel::Off,
                },
            ],
            ..Default::default()
        };
        assert_eq!(config.active_favorite_alias(), Some("sonnet-hi"));

        // Same model but a different thinking level matches no favorite.
        let config = Config {
            thinking_level: ThinkingLevel::Off,
            ..config
        };
        assert_eq!(config.active_favorite_alias(), None);
    }

    #[test]
    fn test_openai_provider_text_verbosity_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(
            &config_path,
            r#"[providers.openai]
text_verbosity = "high"

[providers.openai_codex]
text_verbosity = "low"
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.providers.openai.effective_text_verbosity(),
            Some(TextVerbosity::High)
        );
        assert_eq!(
            config.providers.openai_codex.effective_text_verbosity(),
            Some(TextVerbosity::Low)
        );
    }

    /// `save_thinking_level`: creates new config file with template if it doesn't exist.
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

    /// `save_thinking_level`: preserves other fields in existing config.
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

    /// `save_thinking_level`: uses template structure but preserves user values.
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

    /// `save_thinking_level`: roundtrip - save and reload works correctly.
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
        Config::save_thinking_level_to(&config_path, ThinkingLevel::Max).unwrap();

        // Reload and verify again
        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.thinking_level, ThinkingLevel::Max);
    }

    /// `filter_tools`: returns all tools when no filtering configured.
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

    /// `filter_tools`: explicit tools list.
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

    /// `filter_tools`: case-insensitive matching.
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

    /// `filter_tools`: trims whitespace from tool names.
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

    /// `filter_tools`: ignores empty strings after trimming.
    #[test]
    fn test_filter_tools_ignores_empty_strings() {
        let config = ProviderConfig {
            tools: Some(vec![
                "bash".to_string(),
                "  ".to_string(),
                String::new(),
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

    /// `filter_tools`: `openai_codex` default has no tool filtering.
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

    /// `filter_tools`: anthropic default has no tool filtering.
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

    /// `ProvidersConfig::get` returns correct provider config.
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

    /// `TranscriptionConfig`: defaults are all None (auto-detect, no model override, no language).
    #[test]
    fn test_transcription_config_defaults() {
        let config = TranscriptionConfig::default();
        assert!(config.model.is_none());
        assert!(config.language.is_none());
    }

    #[test]
    fn test_telegram_config_defaults() {
        let config = TelegramConfig::default();
        assert_eq!(config.model, "claude-cli:claude-opus-4-6");
        assert_eq!(config.thinking_level, ThinkingLevel::Low);
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn test_telegram_profiles_load_from_config() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"[telegram]
allowlist_user_ids = [42]
bot_token = "token"

[telegram.profiles.zdx]
chat_id = -100123
cwd = "/tmp/zdx"

[telegram.profiles.work]
chat_id = -100456
cwd = "~/work"
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(config.telegram.profiles.len(), 2);
        let (name, profile) = config.telegram_profile_for_chat(-100_123).unwrap();
        assert_eq!(name, "zdx");
        assert_eq!(profile.cwd, "/tmp/zdx");
        assert!(config.telegram_profile_for_chat(-999).is_none());
    }

    #[test]
    fn test_telegram_profile_chat_ids_are_allowlisted_at_runtime() {
        let config = Config {
            telegram: TelegramConfig {
                bot_token: Some("token".to_string()),
                allowlist_user_ids: vec![42],
                allowlist_chat_ids: vec![-100_123],
                profiles: BTreeMap::from([(
                    "work".to_string(),
                    TelegramProfileConfig {
                        chat_id: -100_456,
                        cwd: "/tmp/work".to_string(),
                    },
                )]),
                ..Default::default()
            },
            ..Default::default()
        };

        let runtime = config.resolve_telegram_runtime().unwrap();
        assert_eq!(runtime.allowlist_chat_ids, vec![-100_123, -100_456]);
    }

    #[test]
    fn test_duplicate_telegram_profile_chat_ids_are_rejected() {
        let config = Config {
            telegram: TelegramConfig {
                bot_token: Some("token".to_string()),
                allowlist_user_ids: vec![42],
                profiles: BTreeMap::from([
                    (
                        "one".to_string(),
                        TelegramProfileConfig {
                            chat_id: -100_123,
                            cwd: "/tmp/one".to_string(),
                        },
                    ),
                    (
                        "two".to_string(),
                        TelegramProfileConfig {
                            chat_id: -100_123,
                            cwd: "/tmp/two".to_string(),
                        },
                    ),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };

        let error = config.resolve_telegram_runtime().unwrap_err().to_string();
        assert!(error.contains("duplicate chat ID"));
    }

    #[test]
    fn save_telegram_profile_emits_section_header_form() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"[telegram]
bot_token = "token"
allowlist_user_ids = [42]
"#,
        )
        .unwrap();

        Config::save_telegram_profile_to(
            &config_path,
            "zdx",
            &TelegramProfileConfig {
                chat_id: -100_123,
                cwd: "/tmp/zdx".to_string(),
            },
        )
        .unwrap();

        let after = fs::read_to_string(&config_path).unwrap();
        assert!(
            after.contains("[telegram.profiles.zdx]"),
            "missing section header form in:\n{after}"
        );
        assert!(after.contains("chat_id = -100123"));
        assert!(after.contains("cwd = \"/tmp/zdx\""));
        // Must not regress back to the inline-sibling style.
        assert!(
            !after.contains("zdx = { "),
            "saver regressed to inline form:\n{after}"
        );
        // Loading the saved config must roundtrip.
        let reloaded = Config::load_from(&config_path).unwrap();
        let (name, profile) = reloaded.telegram_profile_for_chat(-100_123).unwrap();
        assert_eq!(name, "zdx");
        assert_eq!(profile.cwd, "/tmp/zdx");
    }

    #[test]
    fn save_telegram_profile_appends_section_alongside_existing_inline_entries() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"[telegram]
bot_token = "token"
allowlist_user_ids = [42]

[telegram.profiles]
bravo = { chat_id = -100200, cwd = "/tmp/bravo" }
"#,
        )
        .unwrap();

        Config::save_telegram_profile_to(
            &config_path,
            "zdx",
            &TelegramProfileConfig {
                chat_id: -100_123,
                cwd: "/tmp/zdx".to_string(),
            },
        )
        .unwrap();

        let after = fs::read_to_string(&config_path).unwrap();
        assert!(after.contains("[telegram.profiles.zdx]"));
        // Existing inline entry stays as-is (we don't rewrite siblings).
        assert!(after.contains("bravo = { chat_id = -100200"));
        let reloaded = Config::load_from(&config_path).unwrap();
        assert_eq!(reloaded.telegram.profiles.len(), 2);
    }

    /// `SubagentsConfig`: defaults are enabled with dynamic model list resolution.
    #[test]
    fn test_subagents_config_defaults() {
        let config = SubagentsConfig::default();
        assert!(config.enabled);
        assert!(config.available_models.is_empty());
    }

    /// `PromptTemplateConfig`: defaults to built-in template and no custom file.
    #[test]
    fn test_prompt_template_config_defaults() {
        let config = PromptTemplateConfig::default();
        assert!(config.file.is_none());
    }

    /// Prompt template config loads from file.
    #[test]
    fn test_prompt_template_config_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(
            &config_path,
            r#"[prompt_template]
file = "prompts/template.md"
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.prompt_template.file,
            Some("prompts/template.md".to_string())
        );
    }

    /// Subagents config loads from file.
    #[test]
    fn test_subagents_config_loads_from_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(
            &config_path,
            r#"[subagents]
enabled = true
available_models = ["codex:gpt-5.3-codex"]
"#,
        )
        .unwrap();

        let config = Config::load_from(&config_path).unwrap();
        assert!(config.subagents.enabled);
        assert_eq!(
            config.subagents.available_models,
            vec!["codex:gpt-5.3-codex"]
        );
    }

    #[test]
    fn test_subagent_available_models_filters_disabled_providers() {
        let config = Config {
            providers: ProvidersConfig {
                anthropic: ProviderConfig {
                    enabled: Some(false),
                    ..default_anthropic_provider()
                },
                claude_cli: ProviderConfig {
                    enabled: Some(false),
                    ..default_claude_cli_provider()
                },
                openai_codex: ProviderConfig {
                    enabled: Some(false),
                    ..default_openai_codex_provider()
                },
                openrouter: ProviderConfig {
                    enabled: Some(false),
                    ..default_openrouter_provider()
                },
                deepseek: ProviderConfig {
                    enabled: Some(false),
                    ..default_deepseek_provider()
                },
                moonshot: ProviderConfig {
                    enabled: Some(false),
                    ..default_moonshot_provider()
                },
                stepfun: ProviderConfig {
                    enabled: Some(false),
                    ..default_stepfun_provider()
                },
                lmstudio: ProviderConfig {
                    enabled: Some(false),
                    ..default_lmstudio_provider()
                },
                xiaomi: ProviderConfig {
                    enabled: Some(false),
                    ..default_xiaomi_provider()
                },
                xiaomi_plan: ProviderConfig {
                    enabled: Some(false),
                    ..default_xiaomi_plan_provider()
                },
                gemini: ProviderConfig {
                    enabled: Some(false),
                    ..default_gemini_provider()
                },
                google_antigravity: ProviderConfig {
                    enabled: Some(false),
                    ..default_google_antigravity_provider()
                },
                mistral: ProviderConfig {
                    enabled: Some(false),
                    ..default_mistral_provider()
                },
                opencode_go: ProviderConfig {
                    enabled: Some(false),
                    ..default_opencode_go_provider()
                },
                minimax: ProviderConfig {
                    enabled: Some(false),
                    ..default_minimax_provider()
                },
                zai: ProviderConfig {
                    enabled: Some(false),
                    ..default_zai_provider()
                },
                xai: ProviderConfig {
                    enabled: Some(false),
                    ..default_xai_provider()
                },
                ..Default::default()
            },
            ..Default::default()
        };

        // Inject a fixed candidate list so the assertion is hermetic — it no
        // longer depends on the global registry ($ZDX_HOME/models.toml).
        let models = config.subagent_available_models_from([
            ("openai", "gpt-5.2"),
            ("openai", "gpt-5.2-mini"),
            ("anthropic", "claude-sonnet-4-5"),
            ("gemini", "gemini-3.1-flash"),
        ]);
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|id| id.starts_with("openai:")));
        assert!(
            !models
                .iter()
                .any(|id| id.starts_with("anthropic:") || id.starts_with("gemini:"))
        );
    }
}
