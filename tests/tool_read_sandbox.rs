//! Tests for the read tool sandbox behavior.
//!
//! Verifies that files can be read within the root directory
//! and that absolute paths outside root are allowed.

use std::fs;
use tempfile::TempDir;

// Import the tool module
use zdx_cli::tools::{self, ToolContext};

#[test]
fn test_read_file_inside_root() {
    let temp = TempDir::new().unwrap();
    let file_path = temp.path().join("test.txt");
    fs::write(&file_path, "hello from test").unwrap();

    let ctx = ToolContext::new(temp.path().to_path_buf());
    let input = serde_json::json!({"path": "test.txt"});

    let result = tools::execute_tool("read", "test-id", &input, &ctx).unwrap();

    assert!(!result.is_error);
    assert_eq!(result.content, "hello from test");
    assert_eq!(result.tool_use_id, "test-id");
}

#[test]
fn test_read_nested_file_inside_root() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("subdir")).unwrap();
    let file_path = temp.path().join("subdir/nested.txt");
    fs::write(&file_path, "nested content").unwrap();

    let ctx = ToolContext::new(temp.path().to_path_buf());
    let input = serde_json::json!({"path": "subdir/nested.txt"});

    let result = tools::execute_tool("read", "test-id", &input, &ctx).unwrap();

    assert!(!result.is_error);
    assert_eq!(result.content, "nested content");
}

#[test]
fn test_read_path_traversal_allowed() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let outside_file = outside.path().join("outside.txt");
    fs::write(&outside_file, "outside content").unwrap();

    let ctx = ToolContext::new(root.path().to_path_buf());

    // Use an absolute path outside the root
    let input = serde_json::json!({"path": outside_file.to_str().unwrap()});
    let result = tools::execute_tool("read", "test-id", &input, &ctx).unwrap();

    assert!(!result.is_error);
    assert_eq!(result.content, "outside content");
}

#[test]
fn test_read_nonexistent_file_fails() {
    let temp = TempDir::new().unwrap();
    let ctx = ToolContext::new(temp.path().to_path_buf());

    let input = serde_json::json!({"path": "nonexistent.txt"});
    let result = tools::execute_tool("read", "test-id", &input, &ctx).unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("Error"));
}

#[test]
fn test_read_unknown_tool_fails() {
    let temp = TempDir::new().unwrap();
    let ctx = ToolContext::new(temp.path().to_path_buf());

    let input = serde_json::json!({});
    let result = tools::execute_tool("unknown_tool", "test-id", &input, &ctx).unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("Unknown tool"));
}
