//! Write file tool.
//!
//! Allows the agent to write content to files on the filesystem.

use std::fs;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, insert_path_fields, resolve_input_path};
use crate::core::events::ToolOutput;

/// Returns the tool definition for the write tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Write".to_string(),
        description:
            "Write content to a file. Creates the file if it doesn't exist, overwrites if it does."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write. Relative paths resolve from the current working directory; if the path came from a sourced instruction file, resolve it from that file's directory first, then pass the converted path. Supports $VAR/${VAR} env vars."
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct WriteInput {
    path: String,
    content: String,
}

/// Executes the write tool and returns a structured envelope.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: WriteInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for write tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let display_path = input.path.trim();
    let file_path = match resolve_input_path(display_path, &ctx.root) {
        Ok(path) => path,
        Err(output) => return output,
    };

    // Create parent directories if needed (mkdir -p behavior)
    if let Some(parent) = file_path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return ToolOutput::failure(
            "mkdir_error",
            format!("Failed to create directory '{}'", parent.display()),
            Some(format!("OS error: {e}")),
        );
    }

    // Check if file already exists (to determine `created` field)
    let created = !file_path.exists();

    // Write the content
    let bytes = input.content.len();
    match fs::write(&file_path, &input.content) {
        Ok(()) => {
            let resolved_path = file_path.canonicalize().ok();
            let mut data = serde_json::Map::new();
            insert_path_fields(&mut data, display_path, resolved_path.as_deref());
            data.insert("bytes".to_string(), Value::from(bytes));
            data.insert("created".to_string(), Value::from(created));
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
    fn test_write_new_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "new.txt", "content": "hello world"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""bytes":11"#));
        assert!(json_str.contains(r#""created":true"#));

        // Verify the file was actually written
        let file_path = temp.path().join("new.txt");
        assert!(file_path.exists());
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "hello world");
    }

    #[test]
    fn test_write_overwrites_existing() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("existing.txt");
        fs::write(&file_path, "old content").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "existing.txt", "content": "new content"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""created":false"#));

        // Verify the file was overwritten
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "new content");
    }

    #[test]
    fn test_write_invalid_input() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt"}); // missing content

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    #[test]
    fn test_write_rejects_empty_path() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "   ", "content": "hello"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let payload = serde_json::to_value(result).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "path cannot be empty");
    }

    #[test]
    fn test_write_creates_parent_dirs() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "new_dir/nested/file.txt", "content": "nested content"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""created":true"#));

        // Verify directories were created and file was written
        let file_path = temp.path().join("new_dir/nested/file.txt");
        assert!(file_path.exists());
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "nested content");
    }

    #[test]
    fn test_write_nested_path() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("subdir")).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "subdir/nested.txt", "content": "nested content"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""created":true"#));

        // Verify the file was written
        let file_path = temp.path().join("subdir/nested.txt");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "nested content");
    }

    #[test]
    fn test_write_absolute_path() {
        let temp = TempDir::new().unwrap();
        let abs_path = temp.path().join("absolute.txt");

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": abs_path.to_str().unwrap(), "content": "absolute content"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&abs_path).unwrap(), "absolute content");
    }

    #[test]
    fn test_write_preserves_requested_path_and_reports_resolved_path() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("subdir")).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "subdir/../written.txt", "content": "hello"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        let data = result.data().expect("should have data");
        let file_path = temp.path().join("written.txt");
        assert_eq!(data["path"], "subdir/../written.txt");
        assert_eq!(
            data["resolved_path"],
            file_path.canonicalize().unwrap().display().to_string()
        );
    }
}
