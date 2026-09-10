//! Glob tool for file discovery by name pattern.
//!
//! Uses `ignore::WalkBuilder` and `globset` for fast, `.gitignore`-respecting
//! file discovery that returns structured JSON results.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkState;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, ToolOutput};
use crate::walk::{self, WALK_BUDGET, WalkPolicy};

/// Maximum number of matching files and directories to return in aggregate.
const MAX_RESULTS: usize = 500;

/// Returns the tool definition for the glob tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Glob".to_string(),
        description:
            "Discover files and directories by glob pattern within a scoped path. Patterns without a path separator match at any depth; `max_depth` bounds the walk, so one level can be listed on its own before descending. Hidden entries are included; ignore rules apply by default, and `.git` is searched only when explicitly targeted. Results are sorted into `files` and `directories` and capped at 500 combined. `truncated: true` means the result is partial; narrow or split the search path."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}, "minItems": 1}
                    ],
                    "description": "One glob pattern or an array of alternative patterns (e.g. \"*.rs\" or [\"*.rs\", \"*.md\"])."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Relative paths resolve from the current working directory. Defaults to the current working directory. Supports $VAR/${VAR} env vars."
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Match patterns without case sensitivity (default: false)."
                },
                "match_target": {
                    "type": "string",
                    "enum": ["path", "name"],
                    "description": "Match each pattern against the full relative path (default) or only the entry's file/directory name."
                },
                "entry_type": {
                    "type": "string",
                    "enum": ["file", "directory", "any"],
                    "description": "Entry kind to return (default: any). Use `file` or `directory` to drop the other kind."
                },
                "max_depth": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Deepest level to walk, relative to `path` (1 = its immediate children). Omit to walk the whole tree."
                },
                "include_ignored": {
                    "type": "boolean",
                    "description": "Include entries excluded by .gitignore, .ignore, and global git excludes from the first pass (default: false). The traversal deadline still applies."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct GlobInput {
    pattern: PatternInput,
    path: Option<String>,
    #[serde(default, deserialize_with = "crate::bool_or_string::deserialize")]
    case_insensitive: bool,
    #[serde(default)]
    match_target: MatchTarget,
    #[serde(default)]
    entry_type: EntryType,
    #[serde(
        default,
        deserialize_with = "crate::u64_or_string::deserialize_optional"
    )]
    max_depth: Option<u64>,
    #[serde(default, deserialize_with = "crate::bool_or_string::deserialize")]
    include_ignored: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PatternInput {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MatchTarget {
    #[default]
    Path,
    Name,
}

impl PatternInput {
    fn into_patterns(self) -> Result<Vec<String>, ToolOutput> {
        let patterns = match self {
            Self::One(pattern) => vec![pattern],
            Self::Many(patterns) => patterns,
        };
        if patterns.is_empty() || patterns.iter().any(|pattern| pattern.trim().is_empty()) {
            return Err(ToolOutput::failure(
                "invalid_input",
                "pattern must contain one or more non-empty glob patterns",
                None,
            ));
        }
        Ok(patterns)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EntryType {
    File,
    Directory,
    #[default]
    Any,
}

impl EntryType {
    fn includes(self, kind: EntryKind) -> bool {
        matches!(
            (self, kind),
            (Self::File, EntryKind::File)
                | (Self::Directory, EntryKind::Directory)
                | (Self::Any, _)
        )
    }
}

#[derive(Debug, Clone, Copy)]
enum EntryKind {
    File,
    Directory,
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

/// Walk a directory tree and collect entries matching any glob pattern.
///
/// Traversal is parallel and stops early when `policy`'s shared budget expires,
/// so a miss on a huge tree costs bounded time instead of minutes.
fn collect_entries(
    search_path: &Path,
    root: &Path,
    glob_matcher: &GlobSet,
    match_target: MatchTarget,
    entry_type: EntryType,
    policy: &WalkPolicy,
) -> Vec<(EntryKind, String)> {
    let entries = Mutex::new(Vec::new());

    walk::walk(search_path, policy, |entry| {
        let Some(file_type) = entry.file_type() else {
            return WalkState::Continue;
        };
        let kind = if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_dir() {
            if entry.depth() == 0 {
                return WalkState::Continue;
            }
            EntryKind::Directory
        } else {
            return WalkState::Continue;
        };
        if !entry_type.includes(kind) {
            return WalkState::Continue;
        }

        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        let matched = match match_target {
            MatchTarget::Path => glob_matcher.is_match(rel),
            MatchTarget::Name => glob_matcher.is_match(Path::new(entry.file_name())),
        };
        if !matched {
            return WalkState::Continue;
        }

        let mut entries = entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.push((kind, rel.to_string_lossy().to_string()));
        // Collect one extra to detect truncation.
        if entries.len() > MAX_RESULTS {
            return WalkState::Quit;
        }
        WalkState::Continue
    });

    entries.into_inner().unwrap_or_else(PoisonError::into_inner)
}

fn build_glob_set(
    patterns: &[String],
    case_insensitive: bool,
    match_target: MatchTarget,
) -> Result<(GlobSet, Vec<String>), ToolOutput> {
    let mut builder = GlobSetBuilder::new();
    let mut recursive_patterns = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let recursive = match match_target {
            MatchTarget::Path => make_recursive(pattern),
            MatchTarget::Name => pattern.trim().to_string(),
        };
        let glob = GlobBuilder::new(&recursive)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|error| {
                ToolOutput::failure(
                    "invalid_pattern",
                    format!("Invalid glob pattern '{pattern}': {error}"),
                    None,
                )
            })?;
        builder.add(glob);
        recursive_patterns.push(recursive);
    }
    let matcher = builder.build().map_err(|error| {
        ToolOutput::failure(
            "invalid_pattern",
            format!("Failed to build glob patterns: {error}"),
            None,
        )
    })?;
    Ok((matcher, recursive_patterns))
}

/// Executes the glob tool and returns structured results.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: GlobInput = match super::parse_tool_input(input, "glob") {
        Ok(i) => i,
        Err(out) => return out,
    };

    let patterns = match input.pattern.into_patterns() {
        Ok(patterns) => patterns,
        Err(output) => return output,
    };
    let (glob_matcher, recursive_patterns) =
        match build_glob_set(&patterns, input.case_insensitive, input.match_target) {
            Ok(result) => result,
            Err(output) => return output,
        };

    let search_path = match super::resolve_search_path(input.path.as_deref(), &ctx.root) {
        Ok(p) => p,
        Err(output) => return output,
    };

    let policy_patterns = if input.case_insensitive {
        recursive_patterns.join("\n").to_ascii_lowercase()
    } else {
        recursive_patterns.join("\n")
    };
    let policy = WalkPolicy::for_pattern(Some(&policy_patterns))
        .with_include_ignored(input.include_ignored)
        .with_max_depth(
            input
                .max_depth
                .map(|depth| usize::try_from(depth.max(1)).unwrap_or(usize::MAX)),
        );
    let mut entries = collect_entries(
        &search_path,
        &ctx.root,
        &glob_matcher,
        input.match_target,
        input.entry_type,
        &policy,
    );

    // A miss can mean the file is gitignored, but retrying doubles the traversal
    // and un-prunes `target/`, `node_modules/`, and friends. Only retry while the
    // shared budget still has time left, so both passes cost one deadline total.
    if entries.is_empty() && !input.include_ignored && !policy.budget.is_expired() {
        entries = collect_entries(
            &search_path,
            &ctx.root,
            &glob_matcher,
            input.match_target,
            input.entry_type,
            &policy.without_gitignore(),
        );
    }

    entries.sort_by(|left, right| left.1.cmp(&right.1));
    let capped = entries.len() > MAX_RESULTS;
    entries.truncate(MAX_RESULTS);
    let mut files = Vec::new();
    let mut directories = Vec::new();
    for (kind, path) in entries {
        match kind {
            EntryKind::File => files.push(path),
            EntryKind::Directory => directories.push(path),
        }
    }

    let timed_out = policy.budget.is_partial();
    let mut data = serde_json::Map::new();
    data.insert(
        "files".to_string(),
        serde_json::to_value(files).unwrap_or(Value::Null),
    );
    data.insert(
        "directories".to_string(),
        serde_json::to_value(directories).unwrap_or(Value::Null),
    );
    data.insert("truncated".to_string(), Value::from(capped || timed_out));
    if timed_out || capped {
        let warning = match (timed_out, capped) {
            (true, true) => format!(
                "Traversal stopped after about {}s and results were capped at {MAX_RESULTS}; results are partial and absence is not proven. Narrow or split `path` to continue.",
                WALK_BUDGET.as_secs()
            ),
            (true, false) => format!(
                "Traversal stopped after about {}s, so results are partial and absence is not proven. Narrow or split `path` to continue.",
                WALK_BUDGET.as_secs()
            ),
            (false, true) => format!(
                "Results were capped at {MAX_RESULTS}; results are partial. Narrow or split `path` to continue."
            ),
            (false, false) => unreachable!(),
        };
        data.insert("warning".to_string(), Value::String(warning));
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
        assert_eq!(data["directories"], json!([]));
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
    fn directories_are_returned_by_default() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("crates/inner")).unwrap();
        fs::write(temp.path().join("Cargo.toml"), "").unwrap();

        let ctx = make_ctx(&temp);
        let result = execute(&json!({"pattern": "*"}), &ctx);

        assert!(result.is_ok());
        let data = result.data().unwrap();
        let directories = data["directories"].as_array().unwrap();
        assert!(directories.iter().any(|d| d == "crates"));
        assert!(directories.iter().any(|d| d == "crates/inner"));
        assert_eq!(data["files"], json!(["Cargo.toml"]));
    }

    #[test]
    fn max_depth_lists_one_level_like_ls() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("app/src/deep")).unwrap();
        fs::write(temp.path().join("README.md"), "").unwrap();
        fs::write(temp.path().join("app/main.rs"), "").unwrap();
        fs::write(temp.path().join("app/src/deep/util.rs"), "").unwrap();

        let ctx = make_ctx(&temp);
        let result = execute(&json!({"pattern": "*", "max_depth": 1}), &ctx);

        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["directories"], json!(["app"]));
        assert_eq!(data["files"], json!(["README.md"]));
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn max_depth_accepts_a_numeric_string_and_bounds_deep_trees() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("a/b/c")).unwrap();
        fs::write(temp.path().join("a/b/c/deep.rs"), "").unwrap();
        fs::write(temp.path().join("a/mid.rs"), "").unwrap();

        let ctx = make_ctx(&temp);
        let result = execute(&json!({"pattern": "*.rs", "max_depth": "2"}), &ctx);

        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["files"], json!(["a/mid.rs"]));
    }

    #[test]
    fn entry_type_still_narrows_results() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("docs")).unwrap();
        fs::write(temp.path().join("docs/spec.md"), "").unwrap();

        let ctx = make_ctx(&temp);
        let result = execute(&json!({"pattern": "*", "entry_type": "directory"}), &ctx);

        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["directories"], json!(["docs"]));
        assert_eq!(data["files"], json!([]));
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
        assert!(json_str.contains("one or more non-empty glob patterns"));
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
    fn test_combined_results_cap() {
        let temp = TempDir::new().unwrap();
        for i in 0..(MAX_RESULTS / 2 + 25) {
            fs::write(temp.path().join(format!("entry_{i:04}.txt")), "").unwrap();
            fs::create_dir(temp.path().join(format!("entry_{i:04}.dir"))).unwrap();
        }

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "entry_*", "entry_type": "any"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let total =
            data["files"].as_array().unwrap().len() + data["directories"].as_array().unwrap().len();
        assert_eq!(total, MAX_RESULTS);
        assert_eq!(data["truncated"], true);
        assert!(data["warning"].as_str().unwrap().contains("capped"));
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
    fn test_include_ignored_returns_ignored_and_visible_matches() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join(".ignore"), "ignored/\n").unwrap();
        fs::create_dir_all(temp.path().join("ignored")).unwrap();
        fs::create_dir_all(temp.path().join("visible")).unwrap();
        fs::write(temp.path().join("ignored/match.txt"), "").unwrap();
        fs::write(temp.path().join("visible/match.txt"), "").unwrap();

        let ctx = make_ctx(&temp);
        let result = execute(&json!({"pattern": "match.txt"}), &ctx);
        assert_eq!(
            result.data().unwrap()["files"],
            json!(["visible/match.txt"])
        );

        let result = execute(
            &json!({"pattern": "match.txt", "include_ignored": true}),
            &ctx,
        );
        assert_eq!(
            result.data().unwrap()["files"],
            json!(["ignored/match.txt", "visible/match.txt"])
        );
    }

    #[test]
    fn test_case_insensitive_alternative_directory_discovery() {
        let temp = TempDir::new().unwrap();
        let app_support = temp.path().join("Library/Application Support");
        for relative in [
            "Caches/SAMPLE Preview",
            "Caches/SAMPLE Preview/unrelated-child",
            "Products/Example2",
            "Workers/DEMO",
            "Apps/fixture-data",
            "Apps/unrelated",
        ] {
            fs::create_dir_all(app_support.join(relative)).unwrap();
        }

        let ctx = make_ctx(&temp);
        let result = execute(
            &json!({
                "path": "Library/Application Support",
                "pattern": ["*sample*", "*example2*", "*demo*", "*fixture*"],
                "case_insensitive": true,
                "match_target": "name",
                "entry_type": "directory"
            }),
            &ctx,
        );
        let data = result.data().unwrap();
        assert_eq!(data["files"], json!([]));
        assert_eq!(
            data["directories"],
            json!([
                "Library/Application Support/Apps/fixture-data",
                "Library/Application Support/Caches/SAMPLE Preview",
                "Library/Application Support/Products/Example2",
                "Library/Application Support/Workers/DEMO"
            ])
        );
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_directory_discovery_outside_root_with_spaces_is_absolute() {
        let workspace = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let app_support = external.path().join("Library/Application Support");
        let expected = app_support.join("SAMPLE Preview");
        fs::create_dir_all(&expected).unwrap();

        let result = execute(
            &json!({
                "path": app_support,
                "pattern": "*sample*",
                "case_insensitive": true,
                "match_target": "name",
                "entry_type": "directory"
            }),
            &make_ctx(&workspace),
        );
        assert_eq!(
            result.data().unwrap()["directories"],
            json!([expected.to_string_lossy()])
        );
    }

    #[test]
    fn test_alternative_patterns_do_not_duplicate_overlapping_matches() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.txt"), "").unwrap();

        let result = execute(&json!({"pattern": ["*.txt", "hello*"]}), &make_ctx(&temp));
        assert_eq!(result.data().unwrap()["files"], json!(["hello.txt"]));
    }

    #[test]
    fn test_string_pattern_that_looks_like_json_stays_a_glob() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("a"), "").unwrap();
        fs::write(temp.path().join("[\"a\"]"), "").unwrap();

        let result = execute(&json!({"pattern": "[\"a\"]"}), &make_ctx(&temp));
        assert_eq!(result.data().unwrap()["files"], json!(["a"]));
    }

    #[test]
    fn test_empty_pattern_array_and_member_are_rejected() {
        let temp = TempDir::new().unwrap();
        let ctx = make_ctx(&temp);

        for input in [json!({"pattern": []}), json!({"pattern": ["*.rs", " "]})] {
            let result = execute(&input, &ctx);
            assert!(!result.is_ok());
            assert!(
                result
                    .to_json_string()
                    .contains("one or more non-empty glob patterns")
            );
        }
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

        let result = execute(
            &json!({"pattern": ["no-match", ".GIT/CONFIG"], "case_insensitive": true}),
            &ctx,
        );
        let data = result.data().unwrap();
        assert_eq!(
            data["files"],
            json!([".git/config"]),
            "naming .git reaches it"
        );
    }

    #[test]
    fn test_git_like_alternative_does_not_unprune_git() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::create_dir_all(temp.path().join(".github")).unwrap();
        fs::write(temp.path().join(".git/config"), "").unwrap();
        fs::write(temp.path().join(".github/config"), "").unwrap();
        fs::write(temp.path().join("config"), "").unwrap();

        let result = execute(
            &json!({
                "pattern": ["config", ".GITHUB/**"],
                "case_insensitive": true,
                "include_ignored": true
            }),
            &make_ctx(&temp),
        );
        assert_eq!(
            result.data().unwrap()["files"],
            json!([".github/config", "config"])
        );
    }

    #[test]
    fn test_expired_budget_stops_traversal_and_reports_partial() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("a.txt"), "").unwrap();
        fs::write(temp.path().join("b.txt"), "").unwrap();

        let (matcher, _) =
            build_glob_set(&["*.txt".to_string()], false, MatchTarget::Path).unwrap();
        let policy = WalkPolicy {
            budget: WalkBudget::new(Duration::ZERO),
            ..WalkPolicy::for_pattern(Some("**/*.txt"))
        };

        let entries = collect_entries(
            temp.path(),
            temp.path(),
            &matcher,
            MatchTarget::Path,
            EntryType::File,
            &policy,
        );

        assert!(entries.is_empty(), "no entries visited past the deadline");
        assert!(
            policy.budget.is_partial(),
            "cutoff is reported, so the caller can mark the result partial"
        );
    }
}
