//! Glob tool for file discovery by name pattern.
//!
//! Uses `ignore::WalkBuilder` and `globset` for fast, `.gitignore`-respecting
//! file discovery that returns structured JSON results.

use std::path::{Path, PathBuf};

use globset::Glob;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

/// Maximum number of files to return (prevents context flooding).
const MAX_FILES: usize = 500;

/// Returns the tool definition for the glob tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Glob".to_string(),
        description:
            "Find files by name pattern (glob). Returns a sorted list of matching file paths. \
            Respects .gitignore by default. Retries without .gitignore if no results found. \
            Patterns without path separators are auto-prefixed with **/ for recursive matching."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files (e.g. \"*.rs\", \"**/AGENTS.md\", \"src/**/*.ts\")"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (defaults to project root; supports $VAR/${VAR} env vars)"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
}

/// Auto-prefix patterns that lack a path component with `**/` for recursive matching.
///
/// If the pattern already contains `/` or starts with `**/`, it is returned as-is.
fn make_recursive(pattern: &str) -> String {
    let trimmed = pattern.trim();
    if trimmed.contains('/') || trimmed.starts_with("**/") {
        trimmed.to_string()
    } else {
        format!("**/{trimmed}")
    }
}

/// Resolve the search path from user input + context root.
fn resolve_search_path(user_path: Option<&str>, root: &Path) -> Result<PathBuf, ToolOutput> {
    match user_path {
        Some(p) => {
            let p = p.trim();
            if p.is_empty() {
                return Ok(root.to_path_buf());
            }
            let full = super::resolve_input_path(p, root)?;
            if full.exists() {
                Ok(full)
            } else {
                Err(ToolOutput::failure(
                    "path_error",
                    format!("Path does not exist: '{}'", full.display()),
                    None,
                ))
            }
        }
        None => Ok(root.to_path_buf()),
    }
}

/// Walk a directory tree and collect files matching the glob pattern.
fn collect_files(
    search_path: &Path,
    root: &Path,
    glob_matcher: &globset::GlobMatcher,
    respect_gitignore: bool,
) -> Vec<String> {
    let mut files = Vec::new();

    let walker = WalkBuilder::new(search_path)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .build();

    for entry in walker {
        let Ok(entry) = entry else { continue };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());

        if glob_matcher.is_match(rel) {
            files.push(rel.to_string_lossy().to_string());
            // Collect one extra to detect truncation.
            if files.len() > MAX_FILES {
                break;
            }
        }
    }

    files
}

/// Executes the glob tool and returns structured results.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: GlobInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for glob tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    if input.pattern.trim().is_empty() {
        return ToolOutput::failure("invalid_input", "pattern cannot be empty", None);
    }

    let recursive_pattern = make_recursive(&input.pattern);

    let glob = match Glob::new(&recursive_pattern) {
        Ok(g) => g,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_pattern",
                format!("Invalid glob pattern: {e}"),
                None,
            );
        }
    };
    let glob_matcher = glob.compile_matcher();

    let search_path = match resolve_search_path(input.path.as_deref(), &ctx.root) {
        Ok(p) => p,
        Err(output) => return output,
    };

    // First attempt: respect .gitignore
    let mut files = collect_files(&search_path, &ctx.root, &glob_matcher, true);

    // Retry without gitignore if no results
    if files.is_empty() {
        files = collect_files(&search_path, &ctx.root, &glob_matcher, false);
    }

    // Sort alphabetically
    files.sort();

    // Cap at MAX_FILES
    let truncated = files.len() > MAX_FILES;
    files.truncate(MAX_FILES);

    let total = files.len();

    ToolOutput::success(json!({
        "files": files,
        "total": total,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn make_ctx(dir: &TempDir) -> ToolContext {
        ToolContext::new(dir.path().to_path_buf(), None)
    }

    #[test]
    fn test_basic_glob() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.txt"), "content").unwrap();
        fs::write(temp.path().join("world.txt"), "content").unwrap();
        fs::write(temp.path().join("readme.md"), "content").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "*.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total"], 2);
        assert_eq!(data["truncated"], false);
        let files = data["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "hello.txt"));
        assert!(files.iter().any(|f| f == "world.txt"));
    }

    #[test]
    fn test_auto_recursive() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("a/b")).unwrap();
        fs::write(temp.path().join("top.rs"), "").unwrap();
        fs::write(temp.path().join("a/mid.rs"), "").unwrap();
        fs::write(temp.path().join("a/b/deep.rs"), "").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "*.rs"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total"], 3);
    }

    #[test]
    fn test_explicit_path() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("sub")).unwrap();
        fs::write(temp.path().join("root.txt"), "").unwrap();
        fs::write(temp.path().join("sub/nested.txt"), "").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "*.txt", "path": "sub"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total"], 1);
        assert_eq!(data["files"][0], "sub/nested.txt");
    }

    #[test]
    fn test_empty_pattern_rejected() {
        let temp = TempDir::new().unwrap();
        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "  "});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
        assert!(json_str.contains("pattern cannot be empty"));
    }

    #[test]
    fn test_invalid_glob_pattern() {
        let temp = TempDir::new().unwrap();
        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "[unclosed"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_pattern""#));
    }

    #[test]
    fn test_nonexistent_path() {
        let temp = TempDir::new().unwrap();
        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "*.rs", "path": "nonexistent"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"path_error""#));
    }

    #[test]
    fn test_no_matches_returns_empty() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "*.xyz"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total"], 0);
        assert_eq!(data["files"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_sort_order() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("c.txt"), "").unwrap();
        fs::write(temp.path().join("a.txt"), "").unwrap();
        fs::write(temp.path().join("b.txt"), "").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "*.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let files: Vec<&str> = data["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(files, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn test_max_files_cap() {
        let temp = TempDir::new().unwrap();
        for i in 0..MAX_FILES + 50 {
            fs::write(temp.path().join(format!("file_{i:04}.txt")), "").unwrap();
        }

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "*.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total"], MAX_FILES);
        assert_eq!(data["truncated"], true);
    }

    #[test]
    fn test_relative_paths() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("a/b")).unwrap();
        fs::write(temp.path().join("a/b/deep.txt"), "").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "deep.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["files"][0], "a/b/deep.txt");
    }

    #[test]
    fn test_make_recursive_function() {
        // Simple filename → auto-prefix
        assert_eq!(make_recursive("*.rs"), "**/*.rs");
        assert_eq!(make_recursive("AGENTS.md"), "**/AGENTS.md");

        // Already has path component → unchanged
        assert_eq!(make_recursive("src/**/*.rs"), "src/**/*.rs");
        assert_eq!(make_recursive("**/test_*"), "**/test_*");
        assert_eq!(make_recursive("a/b/*.txt"), "a/b/*.txt");

        // Whitespace trimmed
        assert_eq!(make_recursive("  *.rs  "), "**/*.rs");
    }

    #[test]
    fn test_gitignore_retry() {
        let temp = TempDir::new().unwrap();

        // Init git repo so .gitignore is respected
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        // Create .gitignore that ignores "ignored/" dir
        fs::write(temp.path().join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir_all(temp.path().join("ignored")).unwrap();
        fs::write(temp.path().join("ignored/hidden.txt"), "content").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "hidden.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        // Should find the file via retry without gitignore
        assert_eq!(data["total"], 1);
        assert_eq!(data["files"][0], "ignored/hidden.txt");
    }
}
