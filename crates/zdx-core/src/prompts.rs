//! Prompt file helpers.

/// Shared identity prompt for ZDX-coded agent surfaces.
pub const IDENTITY_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/identity_prompt.md"
));

/// Prompt template for handoff generation (shared with TUI).
pub const HANDOFF_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/handoff_prompt.md"
));

/// Prompt template for auto thread-title generation (shared with TUI).
pub const THREAD_TITLE_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/thread_title_prompt.md"
));

/// Prompt template for system prompt assembly (`MiniJinja`).
pub const SYSTEM_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/system_prompt_template.md"
));

/// Prompt template for read thread tool (shared with tool execution).
pub const READ_THREAD_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/read_thread_prompt.md"
));

/// Returns the canonical identity prompt without leading/trailing whitespace.
#[must_use]
pub fn identity_prompt() -> &'static str {
    IDENTITY_PROMPT_TEMPLATE.trim()
}
