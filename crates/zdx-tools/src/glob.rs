//! Glob tool for file discovery by name pattern.
//!
//! Uses `ignore::WalkBuilder` and `globset` for fast, `.gitignore`-respecting
//! file discovery that returns structured JSON results.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use globset::Glob;
use ignore::WalkState;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, ToolOutput};
use crate::walk::{self, WALK_BUDGET, WalkPolicy};

/// Maximum number of files to return (prevents context flooding).
const MAX_FILES: usize = 500;

/// Returns the tool definition for the glob tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Glob".to_string(),
        description:
            "Find files by name pattern. Use it for path lookups and to find files before Read; use Grep to search file contents. NEVER use find or `rg --files` through Bash for file discovery. Patterns match at any depth, and `*` crosses `/`, so `*.rs` and `src/*.rs` both match nested files; results come back sorted and relative to the workspace root. Respects .gitignore, includes hidden files, and skips `.git` unless the pattern names it. A `truncated` result is incomplete rather than proof that nothing else matches — re-run against a narrower `path`."
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
                    "description": "Directory to search in. Relative paths resolve from the current working directory. Defaults to the current working directory. Supports $VAR/${VAR} env vars."
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

/// Walk a directory tree and collect files matching the glob pattern.
///
/// Traversal is parallel and stops early when `policy`'s shared budget expires,
/// so a miss on a huge tree costs bounded time instead of minutes.
fn collect_files(
    search_path: &Path,
    root: &Path,
    glob_matcher: &globset::GlobMatcher,
    policy: &WalkPolicy,
) -> Vec<String> {
    let files = Mutex::new(Vec::new());

    walk::walk(search_path, policy, |entry| {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            return WalkState::Continue;
        }

        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if !glob_matcher.is_match(rel) {
            return WalkState::Continue;
        }

        let mut files = files.lock().unwrap_or_else(PoisonError::into_inner);
        files.push(rel.to_string_lossy().to_string());
        // Collect one extra to detect truncation.
        if files.len() > MAX_FILES {
            return WalkState::Quit;
        }
        WalkState::Continue
    });

    files.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Executes the glob tool and returns structured results.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: GlobInput = match super::parse_tool_input(input, "glob") {
        Ok(i) => i,
        Err(out) => return out,
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

    let search_path = match super::resolve_search_path(input.path.as_deref(), &ctx.root) {
        Ok(p) => p,
        Err(output) => return output,
    };

    // First attempt: respect .gitignore
    let policy = WalkPolicy::for_pattern(Some(&recursive_pattern));
    let mut files = collect_files(&search_path, &ctx.root, &glob_matcher, &policy);

    // A miss can mean the file is gitignored, but retrying doubles the traversal
    // and un-prunes `target/`, `node_modules/`, and friends. Only retry while the
    // shared budget still has time left, so both passes cost one deadline total.
    if files.is_empty() && !policy.budget.is_expired() {
        files = collect_files(
            &search_path,
            &ctx.root,
            &glob_matcher,
            &policy.without_gitignore(),
        );
    }

    // Sort alphabetically (parallel traversal yields no stable order)
    files.sort();

    // Cap at MAX_FILES
    let capped = files.len() > MAX_FILES;
    files.truncate(MAX_FILES);

    let timed_out = policy.budget.is_partial();
    let mut data = serde_json::Map::new();
    data.insert(
        "files".to_string(),
        serde_json::to_value(files).unwrap_or(Value::Null),
    );
    data.insert("truncated".to_string(), Value::from(capped || timed_out));
    if timed_out {
        let secs = WALK_BUDGET.as_secs();
        data.insert(
            "warning".to_string(),
            Value::String(format!(
                "Traversal stopped after about {secs}s, so these results are partial and a missing file is not proof of absence. Walk cost tracks the size of the directory tree, not how narrow the pattern is: re-run against a deeper `path`."
            )),
        );
    }

    ToolOutput::success(Value::Object(data))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::walk::WalkBudget;

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
        assert_eq!(data["files"].as_array().unwrap().len(), 2);
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
        assert_eq!(data["files"].as_array().unwrap().len(), 3);
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
        assert_eq!(data["files"].as_array().unwrap().len(), 1);
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
        assert_eq!(data["files"].as_array().unwrap().len(), MAX_FILES);
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
        assert_eq!(data["files"].as_array().unwrap().len(), 1);
        assert_eq!(data["files"][0], "ignored/hidden.txt");
    }

    #[test]
    fn test_finds_hidden_files() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join(".zshrc"), "").unwrap();
        fs::create_dir_all(temp.path().join(".cargo")).unwrap();
        fs::write(temp.path().join(".cargo/config.toml"), "").unwrap();

        let ctx = make_ctx(&temp);

        let result = execute(&json!({"pattern": ".zshrc"}), &ctx);
        let data = result.data().unwrap();
        assert_eq!(data["files"], json!([".zshrc"]));

        let result = execute(&json!({"pattern": ".cargo/config.toml"}), &ctx);
        let data = result.data().unwrap();
        assert_eq!(data["files"], json!([".cargo/config.toml"]));
    }

    #[test]
    fn test_ordinary_patterns_reach_dot_directories() {
        // `.github/workflows/*.yml` is ordinary content, not something the
        // caller should have to ask for with a dotted pattern.
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".github/workflows")).unwrap();
        fs::write(temp.path().join(".github/workflows/ci.yml"), "").unwrap();
        fs::write(temp.path().join("visible.yml"), "").unwrap();

        let ctx = make_ctx(&temp);
        let result = execute(&json!({"pattern": "*.yml"}), &ctx);
        let data = result.data().unwrap();

        assert_eq!(
            data["files"],
            json!([".github/workflows/ci.yml", "visible.yml"])
        );
    }

    #[test]
    fn test_git_directory_is_pruned_unless_named() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/config"), "").unwrap();
        fs::write(temp.path().join("config"), "").unwrap();

        let ctx = make_ctx(&temp);

        let result = execute(&json!({"pattern": "config"}), &ctx);
        let data = result.data().unwrap();
        assert_eq!(data["files"], json!(["config"]), "no .git noise by default");

        let result = execute(&json!({"pattern": ".git/config"}), &ctx);
        let data = result.data().unwrap();
        assert_eq!(
            data["files"],
            json!([".git/config"]),
            "naming .git reaches it"
        );
    }

    #[test]
    fn test_expired_budget_stops_traversal_and_reports_partial() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("a.txt"), "").unwrap();
        fs::write(temp.path().join("b.txt"), "").unwrap();

        let matcher = Glob::new("**/*.txt").unwrap().compile_matcher();
        let policy = WalkPolicy {
            budget: WalkBudget::new(Duration::ZERO),
            ..WalkPolicy::for_pattern(Some("**/*.txt"))
        };

        let files = collect_files(temp.path(), temp.path(), &matcher, &policy);

        assert!(files.is_empty(), "no entries visited past the deadline");
        assert!(
            policy.budget.is_partial(),
            "cutoff is reported, so the caller can mark the result partial"
        );
    }
}
