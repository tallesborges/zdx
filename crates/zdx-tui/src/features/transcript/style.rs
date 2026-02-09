/// A styled span of text (UI-agnostic).
///
/// This is a minimal representation that can be converted to
/// ratatui Span/Line types at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: Style,
}

/// A line of styled spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl StyledLine {
    /// Creates an empty line.
    pub fn empty() -> Self {
        StyledLine { spans: vec![] }
    }
}

/// Semantic style identifiers (UI-agnostic).
///
/// These are translated to actual terminal styles by the renderer.
/// This keeps the transcript module free of terminal dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// No styling.
    Plain,
    /// User message prefix ("│ ").
    UserPrefix,
    /// User message content (italic).
    User,
    /// Assistant message content.
    Assistant,
    /// Streaming cursor indicator.
    StreamingCursor,
    /// System message prefix.
    SystemPrefix,
    /// System message content.
    System,
    /// Tool bracket/decoration.
    ToolBracket,
    /// Tool status text.
    ToolStatus,
    /// Tool error status.
    ToolError,
    /// Tool running spinner.
    ToolRunning,
    /// Tool success prefix (green $).
    ToolSuccess,
    /// Tool cancelled/interrupted command style.
    ToolCancelled,
    /// Tool output (stdout from bash, etc).
    ToolOutput,
    /// Tool truncation warning (yellow/dim).
    ToolTruncation,
    /// Interrupted suffix indicator (dim).
    Interrupted,
    /// Thinking block prefix ("Thinking: ").
    ThinkingPrefix,
    /// Thinking block content (dim/italic).
    Thinking,
    /// Timing/duration message (muted, shows tool execution time).
    Timing,

    // Markdown styles
    /// Inline code (`code`).
    CodeInline,
    /// Fenced code block content.
    CodeBlock,
    /// Code fence markers (` ``` ` - rendered subtly).
    CodeFence,
    /// Emphasized text (*italic*).
    Emphasis,
    /// Strong text (**bold**).
    Strong,
    /// Heading level 1 (# Heading).
    H1,
    /// Heading level 2 (## Heading).
    H2,
    /// Heading level 3+ (`### Heading`).
    H3,
    /// Link text.
    Link,
    /// Blockquote content.
    BlockQuote,
    /// List bullet marker.
    ListBullet,
    /// List number marker.
    ListNumber,
}
