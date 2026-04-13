//! Prompt file helpers (re-exports from `zdx-assets`).

/// Built-in instruction layer for headless automation behavior.
pub use zdx_assets::AUTOMATION_HARNESS_INSTRUCTION_LAYER;
/// Instruction layer for interactive TUI chat.
pub use zdx_assets::CHAT_INSTRUCTION_LAYER;
/// Instruction layer for non-interactive exec mode.
pub use zdx_assets::EXEC_INSTRUCTION_LAYER;
/// Prompt template for handoff generation (shared with TUI).
pub use zdx_assets::HANDOFF_PROMPT_TEMPLATE;
/// Shared identity prompt for ZDX-coded agent surfaces.
pub use zdx_assets::IDENTITY_PROMPT_TEMPLATE;
/// Prompt template for read thread tool (shared with tool execution).
pub use zdx_assets::READ_THREAD_PROMPT_TEMPLATE;
/// Prompt template for system prompt assembly (`MiniJinja`).
pub use zdx_assets::SYSTEM_PROMPT_TEMPLATE;
/// Instruction layer for Telegram bot.
pub use zdx_assets::TELEGRAM_INSTRUCTION_LAYER;
/// Prompt template for auto thread-title generation (shared with TUI).
pub use zdx_assets::THREAD_TITLE_PROMPT_TEMPLATE;

/// Returns the canonical identity prompt without leading/trailing whitespace.
#[must_use]
pub fn identity_prompt() -> &'static str {
    IDENTITY_PROMPT_TEMPLATE.trim()
}

/// Returns the built-in default system prompt template body.
#[must_use]
pub fn default_system_prompt_template() -> &'static str {
    SYSTEM_PROMPT_TEMPLATE.trim()
}
