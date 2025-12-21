//! Write file tool.
//!
//! Allows the agent to write content to files on the filesystem.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

/// Returns the tool definition for the write tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "write".to_string(),
        description:
            "Write content to a file. Creates the file if it doesn't exist, overwrites if it does."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to root directory)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
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
                format!("Invalid input for write tool: {}", e),
            );
        }
    };

    let file_path = resolve_path(&input.path, &ctx.root);

    // Create parent directories if needed (mkdir -p behavior)
    if let Some(parent) = file_path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return ToolOutput::failure(
            "mkdir_error",
            format!("Failed to create directory '{}': {}", parent.display(), e),
        );
    }

    // Check if file already exists (to determine `created` field)
    let created = !file_path.exists();

    // Write the content
    let bytes = input.content.len();
    match fs::write(&file_path, &input.content) {
        Ok(()) => ToolOutput::success(json!({
            "path": file_path.display().to_string(),
            "bytes": bytes,
            "created": created
        })),
        Err(e) => ToolOutput::failure(
            "write_error",
            format!("Failed to write file '{}': {}", file_path.display(), e),
        ),
    }
}

/// Resolves a path relative to the root directory.
fn resolve_path(path: &str, root: &Path) -> std::path::PathBuf {
    let requested = Path::new(path);

    // Join with root (handles both absolute and relative paths)
    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_write_new_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
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

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
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
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt"}); // missing content

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    #[test]
    fn test_write_creates_parent_dirs() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
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

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
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

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": abs_path.to_str().unwrap(), "content": "absolute content"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&abs_path).unwrap(), "absolute content");
    }
}
