//! Prompt file helpers.

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

/// Prompt template for Codex (openai_codex provider).
pub const CODEX_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/openai_codex.md"
));

/// Prompt template for read thread tool (shared with tool execution).
pub const READ_THREAD_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/read_thread_prompt.md"
));

/// Prompt template for Gemini agentic coding.
pub const GEMINI_AGENTIC_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/gemini_agentic.md"
));
