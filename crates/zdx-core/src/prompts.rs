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

/// Built-in `general_assistant` subagent template.
pub const GENERAL_ASSISTANT_SUBAGENT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/subagents/general_assistant.md"
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

/// Returns the built-in default system prompt template body.
#[must_use]
pub fn default_system_prompt_template() -> &'static str {
    strip_yaml_frontmatter(GENERAL_ASSISTANT_SUBAGENT_TEMPLATE).trim()
}

fn strip_yaml_frontmatter(content: &'static str) -> &'static str {
    let Some(rest) = content.strip_prefix("---\n") else {
        return content;
    };

    if let Some(idx) = rest.find("\n---\n") {
        return &rest[idx + 5..];
    }

    if let Some(idx) = rest.find("\n...\n") {
        return &rest[idx + 5..];
    }

    content
}
