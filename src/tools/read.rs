//! Read file tool.
//!
//! Allows the agent to read file contents from the filesystem.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

/// Maximum file size before truncation (50KB).
const MAX_BYTES: usize = 50 * 1024;

/// Returns the tool definition for the read tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "read".to_string(),
        description: "Read the contents of a file. Returns the file content as text.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to root directory)"
                }
            },
            "required": ["path"]
        }),
    }
}

#[derive(Debug, Deserialize)]
struct ReadInput {
    path: String,
}

/// Executes the read tool and returns a structured envelope.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: ReadInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Invalid input for read tool: {}", e),
            );
        }
    };

    let file_path = match resolve_path(&input.path, &ctx.root) {
        Ok(p) => p,
        Err(e) => return ToolOutput::failure("path_error", e),
    };

    let content = match fs::read_to_string(&file_path) {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read file '{}': {}", file_path.display(), e),
            );
        }
    };

    let bytes = content.len();
    let (content, truncated) = if bytes > MAX_BYTES {
        (content[..MAX_BYTES].to_string(), true)
    } else {
        (content, false)
    };

    ToolOutput::success(json!({
        "path": file_path.display().to_string(),
        "content": content,
        "truncated": truncated,
        "bytes": bytes
    }))
}

/// Resolves a path relative to the root directory.
fn resolve_path(path: &str, root: &Path) -> Result<std::path::PathBuf, String> {
    let requested = Path::new(path);

    // Join with root (handles both absolute and relative paths)
    let full_path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };

    // Canonicalize to resolve any .. or symlinks
    let canonical = full_path
        .canonicalize()
        .map_err(|e| format!("Path does not exist '{}': {}", full_path.display(), e))?;

    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_read_file_success() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""content":"hello world""#));
        assert!(json_str.contains(r#""truncated":false"#));
        assert!(json_str.contains(r#""bytes":11"#));
    }

    #[test]
    fn test_read_nested_file() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("subdir")).unwrap();
        let file_path = temp.path().join("subdir/nested.txt");
        fs::write(&file_path, "nested content").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "subdir/nested.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""content":"nested content""#));
    }

    #[test]
    fn test_read_file_not_found() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "nonexistent.txt"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":false"#));
        assert!(json_str.contains(r#""code":"path_error""#));
    }

    #[test]
    fn test_read_outside_root_allowed() {
        let root = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("external.txt");
        fs::write(&outside_file, "external content").unwrap();

        let ctx = ToolContext::with_timeout(root.path().to_path_buf(), None);
        let input = json!({ "path": outside_file.to_str().unwrap() });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""content":"external content""#));
    }

    #[test]
    fn test_read_large_file_truncated() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("large.txt");
        // Create a file larger than MAX_BYTES (50KB)
        let content = "x".repeat(60 * 1024);
        fs::write(&file_path, &content).unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "large.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""truncated":true"#));
        // bytes should reflect original size
        assert!(json_str.contains(r#""bytes":61440"#));
    }

    #[test]
    fn test_read_invalid_input() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"wrong_field": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }
}
