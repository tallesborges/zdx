//! Parser for thinking/reasoning block transitions.
//!
//! Some models (like StepFun) output reasoning content with `<think>` tags
//! and may bleed the final response content into the reasoning field
//! immediately after the `</think>` tag. This parser handles splitting
//! reasoning from content at the `</think>` boundary.
//!
//! Example input in reasoning_content field:
//! ```text
//! Let me analyze this...
//! </think>
//! Here is my response.
//! ```
//!
//! This should be split into:
//! - Reasoning: "Let me analyze this..."
//! - Content: "Here is my response."

/// Result of parsing thinking content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkingParseResult {
    /// The reasoning/thinking portion (before `</think>`)
    pub reasoning: String,
    /// Content that bled into reasoning (after `</think>`), if any
    pub content: Option<String>,
    /// Whether the `</think>` tag was found (reasoning is complete)
    pub thinking_complete: bool,
}

/// Parses reasoning content that may contain a `</think>` tag.
///
/// If the content contains `</think>`:
/// - Everything before it is returned as `reasoning`
/// - Everything after it (trimmed) is returned as `content`
/// - `thinking_complete` is set to `true`
///
/// If no `</think>` tag is found:
/// - The entire content is returned as `reasoning`
/// - `content` is `None`
/// - `thinking_complete` is `false`
pub fn parse_thinking(content: &str) -> ThinkingParseResult {
    if let Some(think_end) = content.find("</think>") {
        let reasoning_text = &content[..think_end];
        let after_think = &content[think_end + "</think>".len()..];

        // Trim leading whitespace from content that comes after </think>
        let content_text = after_think.trim_start();

        ThinkingParseResult {
            reasoning: reasoning_text.to_string(),
            content: if content_text.is_empty() {
                None
            } else {
                Some(content_text.to_string())
            },
            thinking_complete: true,
        }
    } else {
        ThinkingParseResult {
            reasoning: content.to_string(),
            content: None,
            thinking_complete: false,
        }
    }
}

/// Check if content contains a `</think>` tag.
pub fn contains_think_end(content: &str) -> bool {
    content.contains("</think>")
}

/// Check if content contains a `<think>` opening tag.
pub fn contains_think_start(content: &str) -> bool {
    content.contains("<think>")
}

/// Strip the `<think>` opening tag from content if present at the start.
/// Only strips if `<think>` appears at the beginning (after optional whitespace).
/// Returns the content after the tag, or the original content if not found at start.
pub fn strip_think_start(content: &str) -> &str {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<think>") {
        rest.trim_start_matches('\n')
    } else {
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_thinking_with_content_bleed() {
        let input = "Let me analyze this...\n</think>\nHere is my response.";
        let result = parse_thinking(input);

        assert!(result.thinking_complete);
        assert_eq!(result.reasoning, "Let me analyze this...\n");
        assert_eq!(result.content, Some("Here is my response.".to_string()));
    }

    #[test]
    fn test_parse_thinking_no_bleed() {
        let input = "Let me analyze this...\n</think>";
        let result = parse_thinking(input);

        assert!(result.thinking_complete);
        assert_eq!(result.reasoning, "Let me analyze this...\n");
        assert_eq!(result.content, None);
    }

    #[test]
    fn test_parse_thinking_incomplete() {
        let input = "Let me analyze this...";
        let result = parse_thinking(input);

        assert!(!result.thinking_complete);
        assert_eq!(result.reasoning, "Let me analyze this...");
        assert_eq!(result.content, None);
    }

    #[test]
    fn test_parse_thinking_empty_after_tag() {
        let input = "Thinking...\n</think>\n\n";
        let result = parse_thinking(input);

        assert!(result.thinking_complete);
        assert_eq!(result.reasoning, "Thinking...\n");
        assert_eq!(result.content, None);
    }

    #[test]
    fn test_parse_thinking_multiline_content() {
        let input = "Step 1: analyze\nStep 2: conclude\n</think>\nFirst point.\nSecond point.";
        let result = parse_thinking(input);

        assert!(result.thinking_complete);
        assert_eq!(result.reasoning, "Step 1: analyze\nStep 2: conclude\n");
        assert_eq!(
            result.content,
            Some("First point.\nSecond point.".to_string())
        );
    }

    #[test]
    fn test_contains_think_end() {
        assert!(contains_think_end("some text </think> more"));
        assert!(contains_think_end("</think>"));
        assert!(!contains_think_end("some text"));
        assert!(!contains_think_end("<think>"));
    }

    #[test]
    fn test_contains_think_start() {
        assert!(contains_think_start("some text <think> more"));
        assert!(contains_think_start("<think>"));
        assert!(!contains_think_start("some text"));
        assert!(!contains_think_start("</think>"));
    }

    #[test]
    fn test_strip_think_start() {
        // Leading <think> should be stripped
        assert_eq!(strip_think_start("<think>\nHello"), "Hello");
        assert_eq!(strip_think_start("<think>Hello"), "Hello");
        // Leading whitespace before <think> is allowed
        assert_eq!(strip_think_start("  <think>\nHello"), "Hello");
        assert_eq!(strip_think_start("\n<think>Hello"), "Hello");
        // No <think> tag - return as-is
        assert_eq!(strip_think_start("Hello"), "Hello");
        // <think> not at start - preserve content (don't strip mid-content tags)
        assert_eq!(
            strip_think_start("prefix<think>\nHello"),
            "prefix<think>\nHello"
        );
    }
}
