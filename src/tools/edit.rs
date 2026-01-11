//! Edit file tool.
//!
//! Allows the agent to perform exact string replacements in files.

use std::fs;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, resolve_existing_path};
use crate::core::events::ToolOutput;

/// Returns the tool definition for the edit tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Edit".to_string(),
        description: "Edit an existing file by performing an exact string replacement. The 'old' text must match exactly (including whitespace and newlines).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative to root directory)"
                },
                "old": {
                    "type": "string",
                    "description": "Exact text to find and replace (must match exactly)"
                },
                "new": {
                    "type": "string",
                    "description": "New text to replace the old text with"
                },
                "expected_replacements": {
                    "type": "integer",
                    "description": "Expected number of replacements (default: 1)",
                    "default": 1
                }
            },
            "required": ["path", "old", "new"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct EditInput {
    path: String,
    old: String,
    new: String,
    #[serde(default = "default_expected_replacements")]
    expected_replacements: i64,
}

fn default_expected_replacements() -> i64 {
    1
}

/// Executes the edit tool and returns a structured envelope.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: EditInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for edit tool",
                Some(format!("Parse error: {}", e)),
            );
        }
    };

    // Validate input
    if input.old.is_empty() {
        return ToolOutput::failure("invalid_input", "'old' text cannot be empty", None);
    }

    if input.expected_replacements < 1 {
        return ToolOutput::failure(
            "invalid_input",
            "'expected_replacements' must be at least 1",
            None,
        );
    }

    let expected = input.expected_replacements as usize;

    // Resolve path
    let file_path = match resolve_existing_path(&input.path, &ctx.root) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Read file content
    let content = match fs::read_to_string(&file_path) {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read file '{}'", file_path.display()),
                Some(format!("OS error: {}", e)),
            );
        }
    };

    // Count non-overlapping occurrences
    let count = content.matches(&input.old).count();

    if count == 0 {
        return ToolOutput::failure(
            "old_not_found",
            format!(
                "No occurrences of the specified text found in '{}'",
                file_path.display()
            ),
            Some(format!("Searched for: {}", input.old)),
        );
    }

    if count != expected {
        return ToolOutput::failure(
            "replacement_count_mismatch",
            format!(
                "Expected {} replacement(s), but found {} occurrence(s)",
                expected, count
            ),
            Some(format!("File: {}", file_path.display())),
        );
    }

    // Perform the replacement
    let new_content = content.replace(&input.old, &input.new);

    // Write back
    match fs::write(&file_path, &new_content) {
        Ok(()) => ToolOutput::success(json!({
            "path": file_path.display().to_string(),
            "replacements": count
        })),
        Err(e) => ToolOutput::failure(
            "write_error",
            format!("Failed to write file '{}'", file_path.display()),
            Some(format!("OS error: {}", e)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_edit_success_single_match() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "Hello world!").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt", "old": "world", "new": "Rust"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""replacements":1"#));

        // Verify file was updated
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello Rust!");
    }

    #[test]
    fn test_edit_failure_old_not_found() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "Hello world!").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt", "old": "nonexistent", "new": "replacement"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":false"#));
        assert!(json_str.contains(r#""code":"old_not_found""#));

        // Verify file was not modified
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello world!");
    }

    #[test]
    fn test_edit_failure_replacement_count_mismatch() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        // Default expected_replacements is 1, but there are 3 occurrences
        let input = json!({"path": "test.txt", "old": "foo", "new": "qux"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":false"#));
        assert!(json_str.contains(r#""code":"replacement_count_mismatch""#));

        // Verify file was not modified
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "foo bar foo baz foo");
    }

    #[test]
    fn test_edit_success_multiple_replacements() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input =
            json!({"path": "test.txt", "old": "foo", "new": "qux", "expected_replacements": 3});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""replacements":3"#));

        // Verify file was updated
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "qux bar qux baz qux");
    }

    #[test]
    fn test_edit_invalid_input_empty_old() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "Hello world!").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt", "old": "", "new": "replacement"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    #[test]
    fn test_edit_invalid_input_zero_expected() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "Hello world!").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input =
            json!({"path": "test.txt", "old": "world", "new": "Rust", "expected_replacements": 0});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    #[test]
    fn test_edit_file_not_found() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "nonexistent.txt", "old": "foo", "new": "bar"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"path_error""#));
    }

    #[test]
    fn test_edit_preserves_crlf() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "line1\r\nline2\r\nline3").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt", "old": "line2\r\n", "new": "replaced\r\n"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        // Verify CRLF is preserved
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "line1\r\nreplaced\r\nline3");
    }
}
