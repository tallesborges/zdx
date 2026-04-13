//! Edit file tool.
//!
//! Allows the agent to perform exact string replacements in files.

use std::fs;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    ToolContext, ToolDefinition, ToolOutput, insert_file_path_fields, resolve_existing_path,
};

/// Returns the tool definition for the edit tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Edit".to_string(),
        description: "Edit an existing file by performing an exact string replacement. The 'old_string' text must match exactly (including whitespace and newlines).".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit. Relative paths resolve from the current working directory; if the path came from a sourced instruction file, resolve it from that file's directory first, then pass the converted path. Supports $VAR/${VAR} env vars."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace (must match exactly)"
                },
                "new_string": {
                    "type": "string",
                    "description": "New text to replace the old text with"
                },
                "expected_replacements": {
                    "type": "integer",
                    "description": "Expected number of replacements (default: 1)",
                    "default": 1
                }
            },
            "required": ["file_path", "old_string", "new_string"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    file_path: String,
    old_string: String,
    new_string: String,
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
                Some(format!("Parse error: {e}")),
            );
        }
    };

    if input.file_path.trim().is_empty() {
        return ToolOutput::failure("invalid_input", "file_path cannot be empty", None);
    }

    // Validate input
    if input.old_string.is_empty() {
        return ToolOutput::failure("invalid_input", "'old_string' text cannot be empty", None);
    }

    if input.expected_replacements < 1 {
        return ToolOutput::failure(
            "invalid_input",
            "'expected_replacements' must be at least 1",
            None,
        );
    }

    let Ok(expected) = usize::try_from(input.expected_replacements) else {
        return ToolOutput::failure(
            "invalid_input",
            "'expected_replacements' is out of supported range",
            None,
        );
    };

    // Resolve path
    let resolved = match resolve_existing_path(&input.file_path, &ctx.root) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let file_path = &resolved.resolved_path;

    // Read file content
    let content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read file '{}'", file_path.display()),
                Some(format!("OS error: {e}")),
            );
        }
    };

    // Count non-overlapping occurrences
    let count = content.matches(&input.old_string).count();

    if count == 0 {
        return ToolOutput::failure(
            "old_not_found",
            format!(
                "No occurrences of the specified text found in '{}'",
                file_path.display()
            ),
            Some(format!("Searched for: {}", input.old_string)),
        );
    }

    if count != expected {
        return ToolOutput::failure(
            "replacement_count_mismatch",
            format!("Expected {expected} replacement(s), but found {count} occurrence(s)"),
            Some(format!("File: {}", file_path.display())),
        );
    }

    // Perform the replacement
    let new_content = content.replace(&input.old_string, &input.new_string);

    // Write back
    match fs::write(file_path, &new_content) {
        Ok(()) => {
            let mut data = serde_json::Map::new();
            insert_file_path_fields(&mut data, &resolved.path, Some(file_path));
            data.insert("replacements".to_string(), Value::from(count));
            ToolOutput::success(Value::Object(data))
        }
        Err(e) => ToolOutput::failure(
            "write_error",
            format!("Failed to write file '{}'", file_path.display()),
            Some(format!("OS error: {e}")),
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

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"file_path": "test.txt", "old_string": "world", "new_string": "Rust"});

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

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"file_path": "test.txt", "old_string": "nonexistent", "new_string": "replacement"});

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

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Default expected_replacements is 1, but there are 3 occurrences
        let input = json!({"file_path": "test.txt", "old_string": "foo", "new_string": "qux"});

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

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"file_path": "test.txt", "old_string": "foo", "new_string": "qux", "expected_replacements": 3});

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

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"file_path": "test.txt", "old_string": "", "new_string": "replacement"});

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

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"file_path": "test.txt", "old_string": "world", "new_string": "Rust", "expected_replacements": 0});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    #[test]
    fn test_edit_file_not_found() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input =
            json!({"file_path": "nonexistent.txt", "old_string": "foo", "new_string": "bar"});

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

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"file_path": "test.txt", "old_string": "line2\r\n", "new_string": "replaced\r\n"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        // Verify CRLF is preserved
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "line1\r\nreplaced\r\nline3");
    }

    #[test]
    fn test_edit_preserves_requested_path_and_reports_resolved_file_path() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("subdir")).unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "Hello world!").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input =
            json!({"file_path": "subdir/../test.txt", "old_string": "world", "new_string": "Rust"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        let data = result.data().expect("should have data");
        assert_eq!(data["file_path"], "subdir/../test.txt");
        assert_eq!(
            data["resolved_file_path"],
            file_path.canonicalize().unwrap().display().to_string()
        );
    }

    #[test]
    fn test_edit_rejects_empty_file_path() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"file_path": "   ", "old_string": "a", "new_string": "b"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let payload = serde_json::to_value(result).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "file_path cannot be empty");
    }

    #[test]
    fn test_edit_rejects_legacy_parameter_names() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "Hello world!").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Old param names (path/old/new) must be rejected now that we use file_path/old_string/new_string
        let input = json!({"path": "test.txt", "old": "world", "new": "Rust"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));

        // File must be untouched
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello world!");
    }
}
