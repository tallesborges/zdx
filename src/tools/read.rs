//! Read file tool.
//!
//! Allows the agent to read file contents from the filesystem.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};

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

/// Executes the read tool.
pub fn execute(input: &Value, ctx: &ToolContext) -> Result<String> {
    let input: ReadInput =
        serde_json::from_value(input.clone()).context("Invalid input for read tool")?;

    let file_path = resolve_path(&input.path, &ctx.root)?;

    fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))
}

/// Resolves a path relative to the root directory.
fn resolve_path(path: &str, root: &Path) -> Result<std::path::PathBuf> {
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
        .with_context(|| format!("Path does not exist: {}", full_path.display()))?;

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

        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({"path": "test.txt"});

        let result = execute(&input, &ctx).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_read_nested_file() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("subdir")).unwrap();
        let file_path = temp.path().join("subdir/nested.txt");
        fs::write(&file_path, "nested content").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({"path": "subdir/nested.txt"});

        let result = execute(&input, &ctx).unwrap();
        assert_eq!(result, "nested content");
    }

    #[test]
    fn test_read_file_not_found() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({"path": "nonexistent.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_outside_root_allowed() {
        let root = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("external.txt");
        fs::write(&outside_file, "external content").unwrap();

        let ctx = ToolContext::new(root.path().to_path_buf());
        let input = json!({ "path": outside_file.to_str().unwrap() });

        let result = execute(&input, &ctx).unwrap();
        assert_eq!(result, "external content");
    }
}
