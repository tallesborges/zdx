//! Embedded assets for ZDX: prompts, default configs, bundled skills, and subagents.
//!
//! This crate owns raw asset content and exposes them as `&'static str` / `&'static [u8]`
//! constants so the runtime crates can reference them without pulling in their own
//! include paths.

// ---------------------------------------------------------------------------
// Prompt templates
// ---------------------------------------------------------------------------

/// Shared identity prompt for ZDX-coded agent surfaces.
pub const IDENTITY_PROMPT_TEMPLATE: &str = include_str!("../prompts/identity_prompt.md");

/// Prompt template for handoff generation (shared with TUI).
pub const HANDOFF_PROMPT_TEMPLATE: &str = include_str!("../prompts/handoff_prompt.md");

/// Prompt template for auto thread-title generation (shared with TUI).
pub const THREAD_TITLE_PROMPT_TEMPLATE: &str = include_str!("../prompts/thread_title_prompt.md");

/// Prompt template for system prompt assembly (`MiniJinja`).
pub const SYSTEM_PROMPT_TEMPLATE: &str = include_str!("../prompts/system_prompt_template.md");

/// Built-in instruction layer for headless automation behavior.
pub const AUTOMATION_HARNESS_INSTRUCTION_LAYER: &str =
    include_str!("../instruction_layers/automation_harness.md");

/// Instruction layer for non-interactive exec mode.
pub const EXEC_INSTRUCTION_LAYER: &str =
    include_str!("../instruction_layers/exec_instruction_layer.md");

/// Instruction layer for interactive TUI chat.
pub const CHAT_INSTRUCTION_LAYER: &str =
    include_str!("../instruction_layers/chat_instruction_layer.md");

/// Instruction layer for Telegram bot.
pub const TELEGRAM_INSTRUCTION_LAYER: &str =
    include_str!("../instruction_layers/telegram_instruction_layer.md");

/// Prompt template for read thread tool (shared with tool execution).
pub const READ_THREAD_PROMPT_TEMPLATE: &str = include_str!("../prompts/read_thread_prompt.md");

// ---------------------------------------------------------------------------
// Default TOML configs
// ---------------------------------------------------------------------------

/// Embedded `default_config.toml`.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../default_config.toml");

/// Embedded `default_models.toml`.
pub const DEFAULT_MODELS_TOML: &str = include_str!("../default_models.toml");

// ---------------------------------------------------------------------------
// Built-in subagent definitions
// ---------------------------------------------------------------------------

/// Built-in `explorer` subagent definition.
pub const EXPLORER_SUBAGENT: &str = include_str!("../subagents/explorer.md");

/// Built-in `thread-searcher` subagent definition.
pub const THREAD_SEARCHER_SUBAGENT: &str =
    include_str!("../subagents/thread-searcher.md");

/// Built-in `oracle` subagent definition.
pub const ORACLE_SUBAGENT: &str = include_str!("../subagents/oracle.md");

// ---------------------------------------------------------------------------
// Bundled skills (materialized into `$ZDX_HOME/bundled-skills` at runtime)
// ---------------------------------------------------------------------------

/// A single embedded bundled-skill asset.
pub struct BundledSkillAsset {
    pub relative_path: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/bundled_skills_manifest.rs"));

/// Returns the full list of embedded bundled-skill assets.
#[must_use]
pub fn bundled_skill_assets() -> &'static [BundledSkillAsset] {
    BUNDLED_SKILL_ASSETS
}
