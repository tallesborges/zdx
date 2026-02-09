//! Parser for text-based tool call formats.
//!
//! Some models output tool calls as XML-like text in the content field
//! instead of using native function calling. This parser handles those formats.
//! The format can vary, so we handle multiple variations:
//!
//! Full format:
//! ```text
//! <tool_call>
//! <function=calculator>
//! <parameter=expr>2+2</parameter>
//! </function>
//! </tool_call>
//! ```
//!
//! Minimal format (sometimes model omits some tags):
//! ```text
//! <function=read>
//! <parameter=path>README.md</parameter>
//! </tool_call>
//! ```

use serde_json::{Value, json};

/// A parsed tool call from `StepFun`'s text format.
#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub name: String,
    pub arguments: Value,
}

/// Check if content contains a tool call.
pub fn contains_tool_call(content: &str) -> bool {
    content.contains("<tool_call>") || content.contains("<function=")
}

/// Check if content contains a complete tool call (has closing tag).
pub fn has_complete_tool_call(content: &str) -> bool {
    // Check for explicit closing tag or if we have function with parameters that seem complete
    if content.contains("</tool_call>") {
        return true;
    }
    // Also consider complete if we have </function> (some models omit </tool_call>)
    if content.contains("</function>") {
        return true;
    }
    // Check if we have a function with at least one complete parameter
    if content.contains("<function=") && content.contains("</parameter>") {
        return true;
    }
    false
}

/// Parse tool calls from `StepFun`'s text format.
///
/// Returns a vector of parsed tool calls and the remaining text (if any).
pub fn parse_tool_calls(content: &str) -> (Vec<ParsedToolCall>, String) {
    let mut tool_calls = Vec::new();
    let mut remaining = content.to_string();

    // Try to find tool calls in various formats
    loop {
        // Find start of a tool call (either <tool_call> or <function=)
        let tool_start = remaining.find("<tool_call>");
        let func_start = remaining.find("<function=");

        let (start_pos, skip_len) = match (tool_start, func_start) {
            (Some(t), Some(f)) => {
                if t < f {
                    (t, "<tool_call>".len())
                } else {
                    (f, 0) // Don't skip <function=, we need it
                }
            }
            (Some(t), None) => (t, "<tool_call>".len()),
            (None, Some(f)) => (f, 0),
            (None, None) => break,
        };

        // Find the end - could be </tool_call>, </function>, or end of last </parameter>
        let search_from = start_pos + skip_len;
        let end_pos = find_tool_call_end(&remaining[search_from..]);

        let Some(end_offset) = end_pos else {
            break;
        };

        let end_abs = search_from + end_offset;
        let tool_call_content = &remaining[start_pos + skip_len..end_abs];

        if let Some(parsed) = parse_single_tool_call(tool_call_content) {
            tool_calls.push(parsed);
        }

        // Remove the parsed tool call from remaining
        remaining = format!(
            "{}{}",
            remaining[..start_pos].trim(),
            remaining[end_abs..].trim_start()
        );
    }

    // Clean up remaining text
    let remaining = remaining.trim().to_string();

    (tool_calls, remaining)
}

/// Find the end of a tool call, returning the position after the closing tag.
fn find_tool_call_end(content: &str) -> Option<usize> {
    // Priority order: </tool_call>, </function>, last </parameter>
    if let Some(pos) = content.find("</tool_call>") {
        return Some(pos + "</tool_call>".len());
    }
    if let Some(pos) = content.find("</function>") {
        return Some(pos + "</function>".len());
    }
    // Find the last </parameter> as fallback
    if let Some(pos) = content.rfind("</parameter>") {
        return Some(pos + "</parameter>".len());
    }
    None
}

/// Parse a single tool call content.
fn parse_single_tool_call(content: &str) -> Option<ParsedToolCall> {
    // Find function name: <function=NAME> or <function=NAME ...
    let func_start = content.find("<function=")?;
    let func_name_start = func_start + "<function=".len();

    // Function name ends at > or whitespace
    let remaining = &content[func_name_start..];
    let func_name_end = remaining
        .find('>')
        .or_else(|| remaining.find(char::is_whitespace))?;
    let function_name = remaining[..func_name_end].trim();

    // Parse parameters - look for all <parameter=NAME>VALUE</parameter> patterns
    let mut arguments = serde_json::Map::new();
    let mut search_start = 0;

    while let Some(param_start) = content[search_start..].find("<parameter=") {
        let abs_param_start = search_start + param_start;
        let param_name_start = abs_param_start + "<parameter=".len();

        // Find end of parameter name (> or whitespace)
        let param_remaining = &content[param_name_start..];
        let Some(param_name_end_rel) = param_remaining
            .find('>')
            .or_else(|| param_remaining.find(char::is_whitespace))
        else {
            break;
        };
        let param_name_end = param_name_start + param_name_end_rel;
        let param_name = content[param_name_start..param_name_end].trim();

        // Find the value - between > and </parameter> (or next tag)
        let value_start = param_name_end + 1;

        // Skip the > if we stopped at whitespace
        let value_start = if content[param_name_end..].starts_with('>') {
            param_name_end + 1
        } else {
            // Find the actual >
            content[param_name_end..]
                .find('>')
                .map_or(value_start, |p| param_name_end + p + 1)
        };

        let value_end = content[value_start..]
            .find("</parameter>")
            .or_else(|| content[value_start..].find('<'))
            .map_or(content.len(), |p| value_start + p);

        let param_value = content[value_start..value_end].trim();

        // Try to parse as JSON, fall back to string
        let value: Value = serde_json::from_str(param_value).unwrap_or_else(|_| json!(param_value));

        arguments.insert(param_name.to_string(), value);

        // Move past this parameter
        search_start = content[value_end..]
            .find("</parameter>")
            .map_or(value_end + 1, |p| value_end + p + "</parameter>".len());
    }

    if function_name.is_empty() {
        return None;
    }

    Some(ParsedToolCall {
        name: function_name.to_string(),
        arguments: Value::Object(arguments),
    })
}

/// Extract text before any tool call markers.
#[allow(dead_code)]
pub fn extract_text_before_tool_call(content: &str) -> Option<&str> {
    let tool_call_pos = content.find("<tool_call>");
    let func_pos = content.find("<function=");

    let start_pos = match (tool_call_pos, func_pos) {
        (Some(t), Some(f)) => Some(t.min(f)),
        (Some(t), None) => Some(t),
        (None, Some(f)) => Some(f),
        (None, None) => None,
    };

    if let Some(idx) = start_pos {
        let before = content[..idx].trim();
        if !before.is_empty() {
            return Some(before);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_tool_call() {
        let content = r"<tool_call>
<function=calculator>
<parameter=expr>
2+2
</parameter>
</function>
</tool_call>";

        let (calls, remaining) = parse_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "calculator");
        assert_eq!(calls[0].arguments["expr"], "2+2");
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_tool_call_with_multiple_params() {
        let content = r"<tool_call>
<function=bash>
<parameter=command>
ls -la
</parameter>
<parameter=timeout>
30
</parameter>
</function>
</tool_call>";

        let (calls, remaining) = parse_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments["command"], "ls -la");
        assert_eq!(calls[0].arguments["timeout"], 30);
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_with_text_before() {
        let content = r"Let me calculate that for you.

<tool_call>
<function=calculator>
<parameter=expr>
2+2
</parameter>
</function>
</tool_call>";

        let before = extract_text_before_tool_call(content);
        assert_eq!(before, Some("Let me calculate that for you."));

        let (calls, _) = parse_tool_calls(content);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn test_parse_minimal_format() {
        // Format without <tool_call> wrapper and missing </function>
        let content = r"<function=read>
<parameter=path>
README.md
</parameter>
</tool_call>";

        let (calls, remaining) = parse_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments["path"], "README.md");
        assert!(remaining.is_empty(), "remaining: {remaining}");
    }

    #[test]
    fn test_parse_minimal_format_no_tool_call_tag() {
        // Format with only function and parameter tags
        let content = r"<function=read>
<parameter=path>README.md</parameter>
</function>";

        let (calls, remaining) = parse_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments["path"], "README.md");
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_inline_format() {
        // Compact inline format
        let content = r"<function=read> <parameter=path> README.md </parameter> </tool_call>";

        let (calls, remaining) = parse_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments["path"], "README.md");
        assert!(remaining.is_empty(), "remaining: '{remaining}'");
    }

    #[test]
    fn test_contains_tool_call() {
        assert!(contains_tool_call("<tool_call>"));
        assert!(contains_tool_call("<function=test>"));
        assert!(contains_tool_call("some text <tool_call> more"));
        assert!(!contains_tool_call("no tool here"));
    }

    #[test]
    fn test_has_complete_tool_call() {
        assert!(has_complete_tool_call("<tool_call></tool_call>"));
        assert!(has_complete_tool_call("<function=x></function>"));
        assert!(has_complete_tool_call(
            "<function=x><parameter=y>z</parameter>"
        ));
        assert!(!has_complete_tool_call("<tool_call>"));
        assert!(!has_complete_tool_call("<function=x>"));
    }
}
