//! Apply patch tool.
//!
//! Applies a file-oriented patch format used by Codex-style editors.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

pub mod parser;
pub mod types;

pub use types::{Hunk, ParseError, UpdateFileChunk};

/// Returns the tool definition for the apply_patch tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Apply_Patch".to_string(),
        description: "Apply a file-oriented patch. The patch must be wrapped in '*** Begin Patch' and '*** End Patch'. Each file section starts with one of: '*** Add File: <path>', '*** Delete File: <path>', or '*** Update File: <path>' (optionally followed by '*** Move to: <new path>'). Update sections contain one or more '@@' hunks with line prefixes: '+' to add, '-' to delete, ' ' (space) for context, and an empty line meaning context. Add File sections must use '+' lines for every line of content.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Patch text in the Codex apply_patch format"
                }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct ApplyPatchInput {
    patch: String,
}

/// Executes the apply_patch tool and returns a structured envelope.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: ApplyPatchInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for apply_patch tool",
                Some(format!("Parse error: {}", e)),
            );
        }
    };

    match apply_patch(&input.patch, &ctx.root) {
        Ok(result) => ToolOutput::success(result.to_json()),
        Err(err) => map_error(err),
    }
}

fn map_error(err: ApplyPatchError) -> ToolOutput {
    match err {
        ApplyPatchError::Parse(ParseError::InvalidPatch(message)) => {
            ToolOutput::failure("invalid_patch", "Invalid patch format", Some(message))
        }
        ApplyPatchError::Parse(ParseError::InvalidHunk {
            message,
            line_number,
        }) => ToolOutput::failure(
            "invalid_patch",
            format!("Invalid hunk at line {}", line_number),
            Some(message),
        ),
        ApplyPatchError::FileNotFound { path } => ToolOutput::failure(
            "file_not_found",
            format!("File not found '{}'", path.display()),
            None,
        ),
        ApplyPatchError::FileExists { path } => ToolOutput::failure(
            "file_exists",
            format!("File already exists '{}'", path.display()),
            None,
        ),
        ApplyPatchError::PatternNotFound { path, message } => ToolOutput::failure(
            "pattern_not_found",
            format!("Pattern not found in '{}'", path.display()),
            Some(message),
        ),
        ApplyPatchError::IoError { path, source } => {
            let details = match path {
                Some(path) => format!("Path: {} (OS error: {})", path.display(), source),
                None => format!("OS error: {}", source),
            };
            ToolOutput::failure("io_error", "I/O error while applying patch", Some(details))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedOp {
    Add {
        path: PathBuf,
        bytes: usize,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: usize,
        bytes: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyResult {
    pub applied: Vec<AppliedOp>,
}

impl ApplyResult {
    pub fn to_json(&self) -> Value {
        let applied = self
            .applied
            .iter()
            .map(|op| match op {
                AppliedOp::Add { path, bytes } => json!({
                    "op": "add",
                    "path": path.display().to_string(),
                    "bytes": bytes,
                }),
                AppliedOp::Delete { path } => json!({
                    "op": "delete",
                    "path": path.display().to_string(),
                }),
                AppliedOp::Update {
                    path,
                    move_path,
                    chunks,
                    bytes,
                } => json!({
                    "op": "update",
                    "path": path.display().to_string(),
                    "move_path": move_path.as_ref().map(|p| p.display().to_string()),
                    "chunks": chunks,
                    "bytes": bytes,
                }),
            })
            .collect::<Vec<_>>();

        json!({ "applied": applied })
    }
}

#[derive(Debug)]
pub enum ApplyPatchError {
    Parse(ParseError),
    FileNotFound {
        path: PathBuf,
    },
    FileExists {
        path: PathBuf,
    },
    PatternNotFound {
        path: PathBuf,
        message: String,
    },
    IoError {
        path: Option<PathBuf>,
        source: std::io::Error,
    },
}

impl From<ParseError> for ApplyPatchError {
    fn from(err: ParseError) -> Self {
        ApplyPatchError::Parse(err)
    }
}

pub fn apply_patch(patch: &str, root: &Path) -> Result<ApplyResult, ApplyPatchError> {
    let hunks = parser::parse_patch(patch)?;
    let mut result = ApplyResult::default();

    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                let target = resolve_path(&path, root);
                if target.exists() {
                    return Err(ApplyPatchError::FileExists { path: target });
                }
                if let Some(parent) = target.parent()
                    && !parent.as_os_str().is_empty()
                {
                    fs::create_dir_all(parent).map_err(|e| ApplyPatchError::IoError {
                        path: Some(parent.to_path_buf()),
                        source: e,
                    })?;
                }
                fs::write(&target, &contents).map_err(|e| ApplyPatchError::IoError {
                    path: Some(target.clone()),
                    source: e,
                })?;
                result.applied.push(AppliedOp::Add {
                    path: target,
                    bytes: contents.len(),
                });
            }
            Hunk::DeleteFile { path } => {
                let target = resolve_path(&path, root);
                if !target.exists() {
                    return Err(ApplyPatchError::FileNotFound { path: target });
                }
                fs::remove_file(&target).map_err(|e| ApplyPatchError::IoError {
                    path: Some(target.clone()),
                    source: e,
                })?;
                result.applied.push(AppliedOp::Delete { path: target });
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let target = resolve_path(&path, root);
                if !target.exists() {
                    return Err(ApplyPatchError::FileNotFound { path: target });
                }
                let updated_bytes = apply_update(&target, &chunks)?;
                let moved_to = if let Some(move_to) = move_path {
                    let target_path = resolve_path(&move_to, root);
                    if target_path != target && target_path.exists() {
                        return Err(ApplyPatchError::FileExists { path: target_path });
                    }
                    if target_path != target {
                        if let Some(parent) = target_path.parent()
                            && !parent.as_os_str().is_empty()
                        {
                            fs::create_dir_all(parent).map_err(|e| ApplyPatchError::IoError {
                                path: Some(parent.to_path_buf()),
                                source: e,
                            })?;
                        }
                        fs::rename(&target, &target_path).map_err(|e| {
                            ApplyPatchError::IoError {
                                path: Some(target.clone()),
                                source: e,
                            }
                        })?;
                        Some(target_path)
                    } else {
                        None
                    }
                } else {
                    None
                };

                result.applied.push(AppliedOp::Update {
                    path: target,
                    move_path: moved_to,
                    chunks: chunks.len(),
                    bytes: updated_bytes,
                });
            }
        }
    }

    Ok(result)
}

fn resolve_path(path: &Path, root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn apply_update(path: &Path, chunks: &[UpdateFileChunk]) -> Result<usize, ApplyPatchError> {
    let content = fs::read_to_string(path).map_err(|e| ApplyPatchError::IoError {
        path: Some(path.to_path_buf()),
        source: e,
    })?;

    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let ends_with_newline = content.ends_with('\n');
    let mut lines = split_lines(&content);

    let mut cursor = 0usize;

    for chunk in chunks {
        if chunk.old_lines.is_empty() {
            if let Some(ctx) = &chunk.change_context
                && !lines.iter().skip(cursor).any(|line| line == ctx)
            {
                return Err(ApplyPatchError::PatternNotFound {
                    path: path.to_path_buf(),
                    message: format!("Context '{}' not found", ctx),
                });
            }
            let insert_at = lines.len();
            lines.splice(insert_at..insert_at, chunk.new_lines.iter().cloned());
            cursor = insert_at + chunk.new_lines.len();
            continue;
        }

        let search_start = if let Some(ctx) = &chunk.change_context {
            match find_line(&lines, ctx, cursor) {
                Some(pos) => pos + 1,
                None => {
                    return Err(ApplyPatchError::PatternNotFound {
                        path: path.to_path_buf(),
                        message: format!("Context '{}' not found", ctx),
                    });
                }
            }
        } else {
            cursor
        };

        let match_start =
            find_sequence(&lines, &chunk.old_lines, search_start, chunk.is_end_of_file)
                .ok_or_else(|| ApplyPatchError::PatternNotFound {
                    path: path.to_path_buf(),
                    message: "Change pattern not found".to_string(),
                })?;

        let end = match_start + chunk.old_lines.len();
        lines.splice(match_start..end, chunk.new_lines.iter().cloned());
        cursor = match_start + chunk.new_lines.len();
    }

    let mut new_content = if lines.is_empty() {
        String::new()
    } else {
        lines.join(newline)
    };
    if ends_with_newline {
        new_content.push_str(newline);
    }

    fs::write(path, &new_content).map_err(|e| ApplyPatchError::IoError {
        path: Some(path.to_path_buf()),
        source: e,
    })?;

    Ok(new_content.len())
}

fn split_lines(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }

    content
        .split_terminator('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}

fn find_line(lines: &[String], needle: &str, start: usize) -> Option<usize> {
    lines.iter().enumerate().skip(start).find_map(
        |(idx, line)| {
            if line == needle { Some(idx) } else { None }
        },
    )
}

fn find_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    require_eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(lines.len());
    }
    if lines.len() < pattern.len() || start > lines.len() {
        return None;
    }

    let max_start = lines.len().saturating_sub(pattern.len());
    for idx in start..=max_start {
        if lines[idx..idx + pattern.len()] == *pattern {
            if require_eof && idx + pattern.len() != lines.len() {
                continue;
            }
            return Some(idx);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_apply_patch_add_file() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch = "*** Begin Patch\n*** Add File: hello.txt\n+Hello\n+World\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        let file_path = temp.path().join("hello.txt");
        assert!(file_path.exists());
        assert_eq!(fs::read_to_string(file_path).unwrap(), "Hello\nWorld");
    }

    #[test]
    fn test_apply_patch_update_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch =
            "*** Begin Patch\n*** Update File: test.txt\n@@\n-line2\n+line2b\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        let updated = fs::read_to_string(&file_path).unwrap();
        assert_eq!(updated, "line1\nline2b\nline3\n");
    }

    #[test]
    fn test_apply_patch_delete_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("delete.txt");
        fs::write(&file_path, "delete me").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch = "*** Begin Patch\n*** Delete File: delete.txt\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        assert!(!file_path.exists());
    }

    #[test]
    fn test_apply_patch_parse_error() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch = "*** Update File: missing_begin.txt\n@@\n-foo\n+bar\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_patch""#));
    }

    #[test]
    fn test_apply_patch_pattern_not_found() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("pattern.txt");
        fs::write(&file_path, "alpha\nbeta\n").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch =
            "*** Begin Patch\n*** Update File: pattern.txt\n@@\n-gamma\n+delta\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"pattern_not_found""#));
    }

    #[test]
    fn test_apply_patch_multi_hunk_update() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("multi.txt");
        fs::write(&file_path, "one\ntwo\nthree\nfour\n").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch = "*** Begin Patch\n*** Update File: multi.txt\n@@\n-two\n+TWO\n@@\n-four\n+FOUR\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let updated = fs::read_to_string(&file_path).unwrap();
        assert_eq!(updated, "one\nTWO\nthree\nFOUR\n");
    }

    #[test]
    fn test_apply_patch_move_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("original.txt");
        fs::write(&file_path, "content\n").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch = "*** Begin Patch\n*** Update File: original.txt\n*** Move to: renamed.txt\n@@\n-content\n+updated content\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        assert!(!file_path.exists());
        let new_path = temp.path().join("renamed.txt");
        assert!(new_path.exists());
        assert_eq!(fs::read_to_string(new_path).unwrap(), "updated content\n");
    }

    #[test]
    fn test_apply_patch_with_context() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("context.txt");
        fs::write(&file_path, "fn main() {\n    hello();\n    world();\n}\n").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let patch = "*** Begin Patch\n*** Update File: context.txt\n@@ fn main() {\n-    hello();\n+    greet();\n*** End Patch";
        let input = json!({"patch": patch});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let updated = fs::read_to_string(&file_path).unwrap();
        assert_eq!(updated, "fn main() {\n    greet();\n    world();\n}\n");
    }
}
