//! Grep tool for structured regex search.
//!
//! Uses ripgrep internals (`grep-regex`, `grep-searcher`) and `ignore::WalkBuilder`
//! for fast, `.gitignore`-respecting searches that return structured JSON results.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::SearcherBuilder;
use grep_searcher::sinks::UTF8;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

/// Maximum number of matches to return (prevents context flooding).
const MAX_MATCHES: usize = 2000;

/// Default number of matches when `max_count` is not specified.
const DEFAULT_MAX_COUNT: usize = 200;

/// Internal per-file cap when collecting before round-robin selection.
const INTERNAL_CAP_PER_FILE: usize = MAX_MATCHES;

/// Maximum file size to search (skip files larger than this).
const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024; // 4MB

/// Maximum characters per matched line (truncate long lines).
const MAX_LINE_LENGTH: usize = 500;

/// Maximum allowed value for `context_lines`.
const MAX_CONTEXT_LINES: usize = 5;

/// Returns the tool definition for the grep tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Grep".to_string(),
        description: "Search the file system for text matching a regex pattern. \
            Returns structured JSON results with file paths, line numbers, and matched text. \
            Respects .gitignore by default."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search in (defaults to project root)"
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. \"*.rs\", \"src/**/*.ts\")"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Whether to search case-insensitively (default: false)"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of context lines before and after each match (0-5, default: 0)"
                },
                "max_count": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default: 200, max: 2000)",
                    "default": 200,
                    "minimum": 1,
                    "maximum": 2000
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of matches to skip before collecting results (default: 0). Use with max_count for pagination.",
                    "default": 0,
                    "minimum": 0
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default, deserialize_with = "super::bool_or_string::deserialize")]
    case_insensitive: bool,
    #[serde(default, deserialize_with = "deserialize_context_lines")]
    context_lines: usize,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    max_count: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    offset: Option<usize>,
}

/// Deserialize `context_lines` from integer, string, or null, clamped to `0..=MAX_CONTEXT_LINES`.
fn deserialize_context_lines<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntOrString {
        Int(i64),
        String(String),
        Null,
    }

    let val = Option::<IntOrString>::deserialize(deserializer)?.unwrap_or(IntOrString::Null);
    let n = match val {
        IntOrString::Int(v) => v.max(0) as usize,
        IntOrString::String(s) => s.trim().parse::<usize>().unwrap_or(0),
        IntOrString::Null => 0,
    };
    Ok(n.min(MAX_CONTEXT_LINES))
}

/// Deserialize an optional `usize` from integer, string, or null.
fn deserialize_optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntOrString {
        Int(i64),
        String(String),
        Null,
    }

    let val = Option::<IntOrString>::deserialize(deserializer)?;
    match val {
        Some(IntOrString::Int(v)) => Ok(Some(v.max(0) as usize)),
        Some(IntOrString::String(s)) => Ok(s.trim().parse::<usize>().ok()),
        Some(IntOrString::Null) | None => Ok(None),
    }
}

/// A single search match.
#[derive(Debug, Clone, serde::Serialize)]
struct Match {
    file: String,
    line_number: u64,
    text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_before: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_after: Vec<String>,
}

/// Sanitize patterns that contain `${` (common in shell-like patterns from LLMs).
///
/// Bare `${` is invalid regex syntax; escape it to `\$\{`.
fn sanitize_pattern(pattern: &str) -> String {
    pattern.replace("${", r"\$\{")
}

/// Truncate a line to `MAX_LINE_LENGTH` characters and trim trailing whitespace.
fn truncate_line(line: &str) -> String {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.chars().count() > MAX_LINE_LENGTH {
        let truncated: String = trimmed.chars().take(MAX_LINE_LENGTH).collect();
        truncated
    } else {
        trimmed.to_string()
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
            let requested = Path::new(p);
            let full = if requested.is_absolute() {
                requested.to_path_buf()
            } else {
                root.join(requested)
            };
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

/// Build a `GlobMatcher` from a user-supplied glob string.
fn build_glob_matcher(glob_pattern: &str) -> Result<GlobMatcher, ToolOutput> {
    Glob::new(glob_pattern)
        .map(|g| g.compile_matcher())
        .map_err(|e| {
            ToolOutput::failure("invalid_glob", format!("Invalid glob pattern: {e}"), None)
        })
}

/// Select up to `max` matches via round-robin across files.
///
/// Returns `(selected_matches, truncated)`.
fn round_robin_select(per_file: Vec<Vec<Match>>, max: usize) -> (Vec<Match>, bool) {
    let total: usize = per_file.iter().map(Vec::len).sum();
    let truncated = total > max;

    let mut result = Vec::with_capacity(max.min(total));
    let mut iters: Vec<std::vec::IntoIter<Match>> =
        per_file.into_iter().map(IntoIterator::into_iter).collect();

    'outer: loop {
        let mut any_remaining = false;
        for iter in &mut iters {
            if let Some(m) = iter.next() {
                result.push(m);
                any_remaining = true;
                if result.len() >= max {
                    break 'outer;
                }
            }
        }
        if !any_remaining {
            break;
        }
    }

    (result, truncated)
}

/// Executes the grep tool and returns structured results.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: GrepInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for grep tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    if input.pattern.trim().is_empty() {
        return ToolOutput::failure("invalid_input", "pattern cannot be empty", None);
    }

    let sanitized = sanitize_pattern(&input.pattern);

    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(input.case_insensitive)
        .build(&sanitized)
    {
        Ok(m) => m,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_pattern",
                format!("Invalid regex pattern: {e}"),
                None,
            );
        }
    };

    let search_path = match resolve_search_path(input.path.as_deref(), &ctx.root) {
        Ok(p) => p,
        Err(output) => return output,
    };

    let glob_matcher: Option<GlobMatcher> = match &input.glob {
        Some(g) => match build_glob_matcher(g) {
            Ok(gm) => Some(gm),
            Err(output) => return output,
        },
        None => None,
    };

    let max_count = input
        .max_count
        .unwrap_or(DEFAULT_MAX_COUNT)
        .clamp(1, MAX_MATCHES);
    let offset = input.offset.unwrap_or(0);

    let per_file = collect_matches(
        &search_path,
        &matcher,
        &ctx.root,
        glob_matcher.as_ref(),
        input.context_lines,
    );

    let (all_matches, truncated_by_cap) = round_robin_select(per_file, MAX_MATCHES);

    // Apply offset then max_count.
    let after_offset: Vec<Match> = all_matches.into_iter().skip(offset).collect();
    let truncated = truncated_by_cap || after_offset.len() > max_count;
    let selected: Vec<Match> = after_offset.into_iter().take(max_count).collect();
    let total_matches = selected.len();

    ToolOutput::success(json!({
        "matches": selected,
        "total_matches": total_matches,
        "truncated": truncated,
    }))
}

/// Collect matches grouped by file for round-robin selection.
fn collect_matches(
    search_path: &Path,
    matcher: &grep_regex::RegexMatcher,
    root: &Path,
    glob_matcher: Option<&GlobMatcher>,
    context_lines: usize,
) -> Vec<Vec<Match>> {
    let mut per_file: Vec<Vec<Match>> = Vec::new();
    let mut total_collected: usize = 0;

    if search_path.is_file() {
        let mut file_matches = Vec::new();
        search_file(
            search_path,
            matcher,
            root,
            context_lines,
            &mut file_matches,
            &mut total_collected,
        );
        if !file_matches.is_empty() {
            per_file.push(file_matches);
        }
        return per_file;
    }

    let walker = WalkBuilder::new(search_path).build();

    for entry in walker {
        let Ok(entry) = entry else { continue };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        if let Some(gm) = glob_matcher {
            let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
            if !gm.is_match(rel) {
                continue;
            }
        }

        if let Ok(metadata) = entry.metadata()
            && metadata.len() > MAX_FILE_SIZE
        {
            continue;
        }

        let mut file_matches = Vec::new();
        search_file(
            entry.path(),
            matcher,
            root,
            context_lines,
            &mut file_matches,
            &mut total_collected,
        );
        if !file_matches.is_empty() {
            per_file.push(file_matches);
        }
    }

    per_file
}

/// Search a single file and append matches to the result vec.
fn search_file(
    path: &Path,
    matcher: &grep_regex::RegexMatcher,
    root: &Path,
    context_lines: usize,
    file_matches: &mut Vec<Match>,
    total_collected: &mut usize,
) {
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    // First pass: collect line numbers of matches.
    let mut match_line_numbers: Vec<u64> = Vec::new();
    let mut searcher = SearcherBuilder::new().line_number(true).build();
    let _ = searcher.search_path(
        matcher,
        path,
        UTF8(|lnum, _line| {
            if *total_collected + match_line_numbers.len() < INTERNAL_CAP_PER_FILE {
                match_line_numbers.push(lnum);
            }
            Ok(true)
        }),
    );

    if match_line_numbers.is_empty() {
        return;
    }

    if context_lines == 0 {
        // Fast path: re-read only if we need the text (we already have line numbers).
        // Actually, we need the text too. Re-read the file lines.
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let lines: Vec<&str> = content.lines().collect();

        for &lnum in &match_line_numbers {
            let idx = (lnum - 1) as usize;
            let text = lines.get(idx).map(|l| truncate_line(l)).unwrap_or_default();

            file_matches.push(Match {
                file: relative_path.clone(),
                line_number: lnum,
                text,
                context_before: Vec::new(),
                context_after: Vec::new(),
            });
            *total_collected += 1;
        }
    } else {
        // Context path: read file lines and build context windows.
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let lines: Vec<&str> = content.lines().collect();

        for &lnum in &match_line_numbers {
            let idx = (lnum - 1) as usize;
            let text = lines.get(idx).map(|l| truncate_line(l)).unwrap_or_default();

            let ctx_start = idx.saturating_sub(context_lines);
            let ctx_end = (idx + context_lines + 1).min(lines.len());

            let context_before: Vec<String> = lines[ctx_start..idx]
                .iter()
                .map(|l| truncate_line(l))
                .collect();
            let context_after: Vec<String> = lines[(idx + 1).min(lines.len())..ctx_end]
                .iter()
                .map(|l| truncate_line(l))
                .collect();

            file_matches.push(Match {
                file: relative_path.clone(),
                line_number: lnum,
                text,
                context_before,
                context_after,
            });
            *total_collected += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn make_ctx(dir: &TempDir) -> ToolContext {
        ToolContext::new(dir.path().to_path_buf(), None)
    }

    #[test]
    fn test_basic_search() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("hello.txt"),
            "Hello World\nGoodbye World\n",
        )
        .unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "Hello"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);
        assert_eq!(data["truncated"], false);
        assert_eq!(data["matches"][0]["file"], "hello.txt");
        assert_eq!(data["matches"][0]["line_number"], 1);
        assert_eq!(data["matches"][0]["text"], "Hello World");
    }

    #[test]
    fn test_case_insensitive() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "Hello\nhello\nHELLO\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "hello", "case_insensitive": true});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 3);
    }

    #[test]
    fn test_case_insensitive_as_string() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "Hello\nhello\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "hello", "case_insensitive": "true"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 2);
    }

    #[test]
    fn test_search_specific_path() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("sub")).unwrap();
        fs::write(temp.path().join("root.txt"), "match here\n").unwrap();
        fs::write(temp.path().join("sub/nested.txt"), "match here too\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "match", "path": "sub"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);
        assert_eq!(data["matches"][0]["file"], "sub/nested.txt");
    }

    #[test]
    fn test_search_specific_file() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("a.txt"), "target line\n").unwrap();
        fs::write(temp.path().join("b.txt"), "target line\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "target", "path": "a.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);
        assert_eq!(data["matches"][0]["file"], "a.txt");
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
    fn test_invalid_pattern() {
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
        let input = json!({"pattern": "test", "path": "nonexistent"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"path_error""#));
    }

    #[test]
    fn test_no_matches_returns_empty() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "nothing here\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "xyz123"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 0);
        assert_eq!(data["matches"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_brace_sanitization() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "const x = ${VAR};\n").unwrap();

        let ctx = make_ctx(&temp);
        // LLM sends a pattern with ${...} which is invalid regex
        let input = json!({"pattern": "${VAR}"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);
    }

    #[test]
    fn test_long_line_truncation() {
        let temp = TempDir::new().unwrap();
        let long_line = format!("prefix {}\n", "x".repeat(1000));
        fs::write(temp.path().join("test.txt"), &long_line).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "prefix"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let text = data["matches"][0]["text"].as_str().unwrap();
        assert!(text.chars().count() <= MAX_LINE_LENGTH);
    }

    #[test]
    fn test_max_matches_cap() {
        let temp = TempDir::new().unwrap();
        // Create a file with more lines than DEFAULT_MAX_COUNT
        let content: String = (0..DEFAULT_MAX_COUNT + 50).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "match line {i}");
            s
        });
        fs::write(temp.path().join("big.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "match"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], DEFAULT_MAX_COUNT);
        assert_eq!(data["truncated"], true);
    }

    #[test]
    fn test_skips_large_files() {
        let temp = TempDir::new().unwrap();
        let big_file = temp.path().join("big.txt");
        // Create a file larger than MAX_FILE_SIZE (4MB + 1 byte)
        let f = fs::File::create(&big_file).unwrap();
        f.set_len(MAX_FILE_SIZE + 1).unwrap();

        // Also create a small file that should match
        fs::write(temp.path().join("small.txt"), "findme\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "findme"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        // Only the small file should produce matches
        assert_eq!(data["total_matches"], 1);
        assert_eq!(data["matches"][0]["file"], "small.txt");
    }

    #[test]
    fn test_sanitize_pattern_function() {
        assert_eq!(sanitize_pattern("${VAR}"), r"\$\{VAR}");
        assert_eq!(sanitize_pattern("normal"), "normal");
        assert_eq!(sanitize_pattern("${a}${b}"), r"\$\{a}\$\{b}");
    }

    #[test]
    fn test_relative_paths() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("a/b")).unwrap();
        fs::write(temp.path().join("a/b/deep.txt"), "found it\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "found"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["matches"][0]["file"], "a/b/deep.txt");
    }

    // --- Slice 2 tests ---

    #[test]
    fn test_glob_filter_rs_files() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("code.rs"), "fn main() {}\n").unwrap();
        fs::write(temp.path().join("readme.md"), "fn docs\n").unwrap();
        fs::write(temp.path().join("lib.rs"), "fn lib() {}\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "fn", "glob": "*.rs"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let files: Vec<&str> = data["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["file"].as_str().unwrap())
            .collect();
        assert!(files.iter().all(|f| {
            Path::new(f)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        }));
        assert_eq!(data["total_matches"], 2);
    }

    #[test]
    fn test_glob_filter_nested() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("src/sub")).unwrap();
        fs::write(temp.path().join("src/main.rs"), "fn main\n").unwrap();
        fs::write(temp.path().join("src/sub/lib.rs"), "fn lib\n").unwrap();
        fs::write(temp.path().join("other.rs"), "fn other\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "fn", "glob": "src/**/*.rs"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let files: Vec<&str> = data["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["file"].as_str().unwrap())
            .collect();
        assert!(files.iter().all(|f| f.starts_with("src/")));
        // other.rs should be excluded
        assert!(!files.contains(&"other.rs"));
        assert_eq!(data["total_matches"], 2);
    }

    #[test]
    fn test_invalid_glob() {
        let temp = TempDir::new().unwrap();
        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "test", "glob": "[invalid"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_glob""#));
    }

    #[test]
    fn test_context_lines() {
        let temp = TempDir::new().unwrap();
        let content = "line1\nline2\nline3\nTARGET\nline5\nline6\nline7\n";
        fs::write(temp.path().join("ctx.txt"), content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "TARGET", "context_lines": 2});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);

        let m = &data["matches"][0];
        assert_eq!(m["text"], "TARGET");
        assert_eq!(m["line_number"], 4);

        let before: Vec<&str> = m["context_before"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(before, vec!["line2", "line3"]);

        let after: Vec<&str> = m["context_after"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(after, vec!["line5", "line6"]);
    }

    #[test]
    fn test_context_lines_at_file_boundaries() {
        let temp = TempDir::new().unwrap();
        let content = "FIRST\nline2\nline3\n";
        fs::write(temp.path().join("edge.txt"), content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "FIRST", "context_lines": 3});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let m = &data["matches"][0];

        // No context_before for first line — field absent (skip_serializing_if)
        assert!(m.get("context_before").is_none());

        // Only 2 lines after (line2, line3)
        let after: Vec<&str> = m["context_after"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(after, vec!["line2", "line3"]);
    }

    #[test]
    fn test_context_lines_zero() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "line1\nTARGET\nline3\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "TARGET", "context_lines": 0});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let m = &data["matches"][0];
        // With skip_serializing_if = "Vec::is_empty", these keys should be absent
        assert!(m.get("context_before").is_none());
        assert!(m.get("context_after").is_none());
    }

    #[test]
    fn test_context_lines_default_omitted() {
        // When context_lines is not provided, context fields should be absent from output
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "line1\nTARGET\nline3\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "TARGET"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let m = &data["matches"][0];
        assert!(m.get("context_before").is_none());
        assert!(m.get("context_after").is_none());
    }

    #[test]
    fn test_context_lines_clamped() {
        // Values > MAX_CONTEXT_LINES should be clamped
        let temp = TempDir::new().unwrap();
        let mut content = String::new();
        for i in 1..=20 {
            let _ = writeln!(content, "line{i}");
        }
        fs::write(temp.path().join("test.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);
        // Request 99 context lines, should be clamped to MAX_CONTEXT_LINES (5)
        let input = json!({"pattern": "line10", "context_lines": 99});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let m = &data["matches"][0];
        let before = m["context_before"].as_array().unwrap();
        let after = m["context_after"].as_array().unwrap();
        assert_eq!(before.len(), MAX_CONTEXT_LINES);
        assert_eq!(after.len(), MAX_CONTEXT_LINES);
    }

    #[test]
    fn test_context_lines_as_string() {
        // LLMs may send context_lines as a string
        let temp = TempDir::new().unwrap();
        let content = "line1\nline2\nTARGET\nline4\nline5\n";
        fs::write(temp.path().join("test.txt"), content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "TARGET", "context_lines": "1"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let m = &data["matches"][0];
        let before: Vec<&str> = m["context_before"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(before, vec!["line2"]);
        let after: Vec<&str> = m["context_after"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(after, vec!["line4"]);
    }

    #[test]
    fn test_round_robin_distribution() {
        let temp = TempDir::new().unwrap();
        // Create two files, each with many matches
        let content_a: String = (0..150).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "match_a_{i}");
            s
        });
        let content_b: String = (0..150).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "match_b_{i}");
            s
        });
        fs::write(temp.path().join("aaa.txt"), &content_a).unwrap();
        fs::write(temp.path().join("bbb.txt"), &content_b).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "match"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], DEFAULT_MAX_COUNT);
        assert_eq!(data["truncated"], true);

        // Count matches per file — should be roughly balanced (100 each for 200 total)
        let matches = data["matches"].as_array().unwrap();
        let count_a = matches.iter().filter(|m| m["file"] == "aaa.txt").count();
        let count_b = matches.iter().filter(|m| m["file"] == "bbb.txt").count();

        // Each file had 150 matches, round-robin should pick 100 from each
        assert_eq!(count_a, 100);
        assert_eq!(count_b, 100);
    }

    #[test]
    fn test_round_robin_unequal_files() {
        let temp = TempDir::new().unwrap();
        // File A has 10 matches, file B has 300
        let content_a: String = (0..10).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "hit_{i}");
            s
        });
        let content_b: String = (0..300).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "hit_{i}");
            s
        });
        fs::write(temp.path().join("aaa.txt"), &content_a).unwrap();
        fs::write(temp.path().join("bbb.txt"), &content_b).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "hit"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], DEFAULT_MAX_COUNT);
        assert_eq!(data["truncated"], true);

        let matches = data["matches"].as_array().unwrap();
        let count_a = matches.iter().filter(|m| m["file"] == "aaa.txt").count();
        let count_b = matches.iter().filter(|m| m["file"] == "bbb.txt").count();

        // All 10 from A should be included, rest from B
        assert_eq!(count_a, 10);
        assert_eq!(count_b, DEFAULT_MAX_COUNT - 10);
    }

    #[test]
    fn test_max_count_custom() {
        let temp = TempDir::new().unwrap();
        let content: String = (0..50).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line {i}");
            s
        });
        fs::write(temp.path().join("test.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "line", "max_count": 10});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 10);
        assert_eq!(data["truncated"], true);
    }

    #[test]
    fn test_max_count_clamped_to_max() {
        let temp = TempDir::new().unwrap();
        let content: String = (0..10).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line {i}");
            s
        });
        fs::write(temp.path().join("test.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);
        // Request more than MAX_MATCHES — should be clamped
        let input = json!({"pattern": "line", "max_count": 99999});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        // All 10 lines match, well under the clamped cap
        assert_eq!(data["total_matches"], 10);
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_offset_skips_matches() {
        let temp = TempDir::new().unwrap();
        let content: String = (0..20).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line_{i:02}");
            s
        });
        fs::write(temp.path().join("test.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "line", "offset": 15, "max_count": 100});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        // 20 total matches, skip 15, get 5
        assert_eq!(data["total_matches"], 5);
        assert_eq!(data["truncated"], false);

        let matches = data["matches"].as_array().unwrap();
        assert_eq!(matches[0]["text"], "line_15");
    }

    #[test]
    fn test_offset_and_max_count_pagination() {
        let temp = TempDir::new().unwrap();
        let content: String = (0..30).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "item_{i:02}");
            s
        });
        fs::write(temp.path().join("test.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);

        // Page 1: first 10
        let input = json!({"pattern": "item", "offset": 0, "max_count": 10});
        let result = execute(&input, &ctx);
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 10);
        assert_eq!(data["truncated"], true);
        let matches = data["matches"].as_array().unwrap();
        assert_eq!(matches[0]["text"], "item_00");
        assert_eq!(matches[9]["text"], "item_09");

        // Page 2: next 10
        let input = json!({"pattern": "item", "offset": 10, "max_count": 10});
        let result = execute(&input, &ctx);
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 10);
        assert_eq!(data["truncated"], true);
        let matches = data["matches"].as_array().unwrap();
        assert_eq!(matches[0]["text"], "item_10");

        // Page 3: last 10
        let input = json!({"pattern": "item", "offset": 20, "max_count": 10});
        let result = execute(&input, &ctx);
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 10);
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_offset_beyond_results() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "match\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "match", "offset": 100});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 0);
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_max_count_as_string() {
        let temp = TempDir::new().unwrap();
        let content: String = (0..20).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line {i}");
            s
        });
        fs::write(temp.path().join("test.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "line", "max_count": "5"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 5);
        assert_eq!(data["truncated"], true);
    }
}
