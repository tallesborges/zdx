//! Grep tool for structured regex search.
//!
//! Uses ripgrep internals (`grep-regex`, `grep-searcher`) and `ignore::WalkBuilder`
//! for fast, `.gitignore`-respecting searches that return structured JSON results.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::SearcherBuilder;
use grep_searcher::sinks::UTF8;
use ignore::WalkBuilder;
use regex::RegexBuilder;
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

/// Maximum bytes of match/context text to return per line.
const MAX_SNIPPET_BYTES: usize = 500;

/// Maximum total bytes of textual grep payload to return.
const MAX_OUTPUT_TEXT_BYTES: usize = 40 * 1024; // 40KB

/// Maximum allowed value for `context_lines`.
const MAX_CONTEXT_LINES: usize = 5;

/// Returns the tool definition for the grep tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Grep".to_string(),
        description: "Search file contents for text matching a regex pattern. Prefer this over running rg or grep through Bash when you need to find text in files. Use `glob` to narrow file types and `path` to scope the search. Returns structured JSON results with file paths, line numbers, matched text, and optional context. Large files are skipped above 4MB, long match/context lines are truncated to safe snippets, and oversized result sets include a warning so the model can narrow the search or paginate with offset/max_count. Respects .gitignore by default."
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
                    "description": "Directory or file to search in. Relative paths resolve from the current working directory. Defaults to the current working directory. Supports $VAR/${VAR} env vars."
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
                },
                "extract_unique": {
                    "type": "boolean",
                    "description": "When true, extract only the matching text (capture group 1 if present, otherwise full match), deduplicate, and return sorted unique values. Useful for discovery queries like listing all unique tags."
                },
                "type": {
                    "type": "string",
                    "description": "Ripgrep file-type filter (e.g. \"rust\", \"ts\", \"py\"). Restricts search to files matching the named type's extensions. Returns invalid_input for unrecognized type names."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
    #[serde(default, deserialize_with = "super::bool_or_string::deserialize")]
    extract_unique: bool,
    #[serde(rename = "type")]
    file_type: Option<String>,
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

#[derive(Debug, Clone, Copy, Default)]
struct GrepOutputStats {
    text_truncated: bool,
    payload_truncated: bool,
    skipped_large_files: usize,
}

/// Sanitize patterns that contain `${` (common in shell-like patterns from LLMs).
///
/// Bare `${` is invalid regex syntax; escape it to `\$\{`.
fn sanitize_pattern(pattern: &str) -> String {
    pattern.replace("${", r"\$\{")
}

/// Build a pre-validated `ignore::types::Types` filter from a user-supplied type name.
///
/// Returns `Ok(Some(types))` on success, `Ok(None)` when no type is specified,
/// and `Err(ToolOutput)` with `invalid_input` when the type name is unrecognized.
fn build_type_filter(file_type: Option<&str>) -> Result<Option<ignore::types::Types>, ToolOutput> {
    let Some(ft) = file_type else {
        return Ok(None);
    };
    let ft = ft.trim();
    if ft.is_empty() {
        return Ok(None);
    }
    let mut tb = ignore::types::TypesBuilder::new();
    tb.add_defaults();
    tb.select(ft);
    tb.build().map(Some).map_err(|_| {
        ToolOutput::failure(
            "invalid_input",
            format!(
                "Unknown file type '{ft}'. Use a ripgrep type name (e.g. rust, ts, py, go, json)."
            ),
            None,
        )
    })
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

    // Validate and build type filter before any walking.
    let file_type_filter = match build_type_filter(input.file_type.as_deref()) {
        Ok(f) => f,
        Err(output) => return output,
    };

    // Extract-unique mode: return sorted deduplicated capture values.
    if input.extract_unique {
        return execute_extract_unique(
            &sanitized,
            input.case_insensitive,
            &search_path,
            &ctx.root,
            glob_matcher.as_ref(),
            file_type_filter,
            max_count,
        );
    }

    let offset = input.offset.unwrap_or(0);

    let (per_file, skipped_large_files) = collect_matches(
        &search_path,
        &matcher,
        &ctx.root,
        glob_matcher.as_ref(),
        input.context_lines,
        file_type_filter,
    );

    let (all_matches, truncated_by_cap) = round_robin_select(per_file, MAX_MATCHES);

    // Apply offset then max_count.
    let after_offset: Vec<Match> = all_matches.into_iter().skip(offset).collect();
    let truncated_by_pagination = after_offset.len() > max_count;
    let selected: Vec<Match> = after_offset.into_iter().take(max_count).collect();
    let (selected, output_stats) = cap_matches_for_output(selected, skipped_large_files);
    let total_matches = selected.len();
    let truncated = truncated_by_cap
        || truncated_by_pagination
        || output_stats.text_truncated
        || output_stats.payload_truncated
        || output_stats.skipped_large_files > 0;

    let mut data = serde_json::Map::new();
    data.insert(
        "matches".to_string(),
        serde_json::to_value(selected).unwrap_or(Value::Null),
    );
    data.insert("total_matches".to_string(), Value::from(total_matches));
    data.insert("truncated".to_string(), Value::from(truncated));
    if truncated && total_matches > 0 {
        data.insert(
            "next_offset".to_string(),
            Value::from(offset + total_matches),
        );
    }
    if let Some(warning) = build_grep_warning(
        offset,
        total_matches,
        truncated_by_cap || truncated_by_pagination,
        output_stats,
    ) {
        data.insert("warning".to_string(), Value::String(warning));
    }

    ToolOutput::success(Value::Object(data))
}

/// Collect matches grouped by file for round-robin selection.
/// Extract unique matching values (capture group 1 or full match) from files.
///
/// Walks the file tree, applies the regex to each file's content, and collects
/// unique extracted values into a sorted set.
fn execute_extract_unique(
    pattern: &str,
    case_insensitive: bool,
    search_path: &Path,
    root: &Path,
    glob_matcher: Option<&GlobMatcher>,
    file_type_filter: Option<ignore::types::Types>,
    max_count: usize,
) -> ToolOutput {
    let re = match RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_pattern",
                format!("Invalid regex pattern: {e}"),
                None,
            );
        }
    };

    let has_captures = re.captures_len() > 1;
    let mut unique_values = BTreeSet::new();

    let (files, skipped_large_files) =
        walk_files(search_path, root, glob_matcher, file_type_filter);

    for path in &files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };

        if has_captures {
            for caps in re.captures_iter(&content) {
                if let Some(m) = caps.get(1) {
                    let val = m.as_str().trim().to_string();
                    if !val.is_empty() {
                        unique_values.insert(val);
                    }
                }
            }
        } else {
            for m in re.find_iter(&content) {
                let val = m.as_str().trim().to_string();
                if !val.is_empty() {
                    unique_values.insert(val);
                }
            }
        }
    }

    let total_unique = unique_values.len();
    let (values, output_stats) =
        cap_unique_values_for_output(unique_values, max_count, skipped_large_files);
    let truncated = total_unique > values.len()
        || output_stats.text_truncated
        || output_stats.payload_truncated
        || output_stats.skipped_large_files > 0;
    let returned_unique = values.len();

    let mut data = serde_json::Map::new();
    data.insert(
        "values".to_string(),
        serde_json::to_value(values).unwrap_or(Value::Null),
    );
    data.insert("total_unique".to_string(), Value::from(returned_unique));
    data.insert("truncated".to_string(), Value::from(truncated));
    if let Some(warning) = build_extract_unique_warning(output_stats, total_unique) {
        data.insert("warning".to_string(), Value::String(warning));
    }

    ToolOutput::success(Value::Object(data))
}

/// Walk the file tree and return paths to search, respecting glob filters and size limits.
fn walk_files(
    search_path: &Path,
    root: &Path,
    glob_matcher: Option<&GlobMatcher>,
    file_type_filter: Option<ignore::types::Types>,
) -> (Vec<PathBuf>, usize) {
    if search_path.is_file() {
        let skipped = search_path
            .metadata()
            .ok()
            .filter(|meta| meta.len() > MAX_FILE_SIZE)
            .map_or(0, |_| 1);
        return if skipped > 0 {
            (Vec::new(), skipped)
        } else {
            (vec![search_path.to_path_buf()], 0)
        };
    }

    let mut files = Vec::new();
    let mut skipped_large_files = 0;
    let mut wb = WalkBuilder::new(search_path);
    if let Some(t) = file_type_filter {
        wb.types(t);
    }
    let walker = wb.build();

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
            skipped_large_files += 1;
            continue;
        }

        files.push(entry.into_path());
    }

    (files, skipped_large_files)
}

fn truncate_snippet(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }

    let mut end = 0;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }

    (text[..end].to_string(), true)
}

fn truncate_match_for_output(m: Match) -> (Match, bool) {
    let (text, text_truncated) = truncate_snippet(&m.text, MAX_SNIPPET_BYTES);
    let mut any_truncated = text_truncated;

    let context_before = m
        .context_before
        .into_iter()
        .map(|line| {
            let (line, truncated) = truncate_snippet(&line, MAX_SNIPPET_BYTES);
            any_truncated |= truncated;
            line
        })
        .collect();

    let context_after = m
        .context_after
        .into_iter()
        .map(|line| {
            let (line, truncated) = truncate_snippet(&line, MAX_SNIPPET_BYTES);
            any_truncated |= truncated;
            line
        })
        .collect();

    (
        Match {
            file: m.file,
            line_number: m.line_number,
            text,
            context_before,
            context_after,
        },
        any_truncated,
    )
}

fn match_output_text_bytes(m: &Match) -> usize {
    m.text.len()
        + m.context_before.iter().map(String::len).sum::<usize>()
        + m.context_after.iter().map(String::len).sum::<usize>()
}

fn cap_matches_for_output(
    matches: Vec<Match>,
    skipped_large_files: usize,
) -> (Vec<Match>, GrepOutputStats) {
    let mut stats = GrepOutputStats {
        skipped_large_files,
        ..GrepOutputStats::default()
    };
    let mut selected = Vec::new();
    let mut used_bytes = 0;

    for m in matches {
        let (m, line_truncated) = truncate_match_for_output(m);
        stats.text_truncated |= line_truncated;
        let size = match_output_text_bytes(&m);
        if used_bytes + size > MAX_OUTPUT_TEXT_BYTES {
            stats.payload_truncated = true;
            break;
        }
        used_bytes += size;
        selected.push(m);
    }

    (selected, stats)
}

fn cap_unique_values_for_output(
    unique_values: BTreeSet<String>,
    max_count: usize,
    skipped_large_files: usize,
) -> (Vec<String>, GrepOutputStats) {
    let mut stats = GrepOutputStats {
        skipped_large_files,
        ..GrepOutputStats::default()
    };
    let mut values = Vec::new();
    let mut used_bytes = 0;

    for value in unique_values {
        let (value, truncated) = truncate_snippet(&value, MAX_SNIPPET_BYTES);
        stats.text_truncated |= truncated;
        if used_bytes + value.len() > MAX_OUTPUT_TEXT_BYTES {
            stats.payload_truncated = true;
            break;
        }
        used_bytes += value.len();
        values.push(value);
        if values.len() >= max_count {
            break;
        }
    }

    (values, stats)
}

fn build_grep_warning(
    offset: usize,
    returned: usize,
    paginated_or_capped: bool,
    stats: GrepOutputStats,
) -> Option<String> {
    let mut parts = Vec::new();

    if stats.skipped_large_files > 0 {
        let noun = if stats.skipped_large_files == 1 {
            "file"
        } else {
            "files"
        };
        parts.push(format!(
            "Skipped {} large {noun} above 4MB.",
            stats.skipped_large_files
        ));
    }

    if stats.text_truncated {
        parts.push(
            "Long match/context lines were truncated to 500 bytes each to avoid flooding the context window."
                .to_string(),
        );
    }

    if stats.payload_truncated {
        let next_offset = offset + returned;
        parts.push(format!(
            "Grep output was capped at ~40KB. Narrow the search with path/glob/context_lines or continue with offset={next_offset}."
        ));
    } else if paginated_or_capped && returned > 0 {
        let next_offset = offset + returned;
        parts.push(format!(
            "More matches are available. Continue with offset={next_offset} or reduce max_count/context_lines to focus the search."
        ));
    }

    (!parts.is_empty()).then(|| parts.join(" "))
}

fn build_extract_unique_warning(stats: GrepOutputStats, total_unique: usize) -> Option<String> {
    let mut parts = Vec::new();

    if stats.skipped_large_files > 0 {
        let noun = if stats.skipped_large_files == 1 {
            "file"
        } else {
            "files"
        };
        parts.push(format!(
            "Skipped {} large {noun} above 4MB.",
            stats.skipped_large_files
        ));
    }

    if stats.text_truncated {
        parts.push(
            "Some extracted values were truncated to 500 bytes each to avoid flooding the context window."
                .to_string(),
        );
    }

    if stats.payload_truncated {
        parts.push(format!(
            "Extracted values were capped at ~40KB. Narrow the pattern if you need all {total_unique} unique values."
        ));
    }

    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Collect matches grouped by file for round-robin selection.
fn collect_matches(
    search_path: &Path,
    matcher: &grep_regex::RegexMatcher,
    root: &Path,
    glob_matcher: Option<&GlobMatcher>,
    context_lines: usize,
    file_type_filter: Option<ignore::types::Types>,
) -> (Vec<Vec<Match>>, usize) {
    let mut per_file: Vec<Vec<Match>> = Vec::new();
    let mut total_collected: usize = 0;
    let (files, skipped_large_files) =
        walk_files(search_path, root, glob_matcher, file_type_filter);

    for path in files {
        let mut file_matches = Vec::new();
        search_file(
            &path,
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

    (per_file, skipped_large_files)
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
            let text = lines.get(idx).map(|l| (*l).to_string()).unwrap_or_default();

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
            let text = lines.get(idx).map(|l| (*l).to_string()).unwrap_or_default();

            let ctx_start = idx.saturating_sub(context_lines);
            let ctx_end = (idx + context_lines + 1).min(lines.len());

            let context_before: Vec<String> = lines[ctx_start..idx]
                .iter()
                .map(|l| (*l).to_string())
                .collect();
            let context_after: Vec<String> = lines[(idx + 1).min(lines.len())..ctx_end]
                .iter()
                .map(|l| (*l).to_string())
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
    fn test_long_line_truncated_to_safe_snippet() {
        let temp = TempDir::new().unwrap();
        let long_line = format!("prefix {}\n", "x".repeat(1000));
        fs::write(temp.path().join("test.txt"), &long_line).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "prefix"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["truncated"], true);
        let text = data["matches"][0]["text"].as_str().unwrap();
        assert_eq!(text.len(), MAX_SNIPPET_BYTES);
        assert!(data["warning"].as_str().unwrap().contains("500 bytes"));
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
    fn test_huge_match_line_truncated_with_warning() {
        let temp = TempDir::new().unwrap();
        let huge_line = format!("match-{}", "x".repeat(100 * 1024));
        fs::write(temp.path().join("huge.jsonl"), format!("{huge_line}\n")).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "match-"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);
        assert_eq!(data["truncated"], true);
        let text = data["matches"][0]["text"].as_str().unwrap();
        assert_eq!(text.len(), MAX_SNIPPET_BYTES);
        assert!(data["warning"].as_str().unwrap().contains("500 bytes"));
    }

    #[test]
    fn test_large_file_skipped_with_warning() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("small.txt"), "match here\n").unwrap();
        fs::write(
            temp.path().join("large.txt"),
            format!("{}match\n", "x".repeat((MAX_FILE_SIZE as usize) + 32)),
        )
        .unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "match"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);
        assert_eq!(data["truncated"], true);
        assert!(
            data["warning"]
                .as_str()
                .unwrap()
                .contains("Skipped 1 large file")
        );
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

    // --- extract_unique tests ---

    #[test]
    fn test_extract_unique_with_capture_group() {
        let temp = TempDir::new().unwrap();
        let content = "Hello #rust and #python\nMore #rust and #go\nAlso #python\n";
        fs::write(temp.path().join("notes.md"), content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"#([a-zA-Z][a-zA-Z0-9_-]*)",
            "extract_unique": true
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let values: Vec<&str> = data["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // Capture group 1 extracts tag name without #
        assert_eq!(values, vec!["go", "python", "rust"]);
        assert_eq!(data["total_unique"], 3);
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_extract_unique_full_match_no_capture() {
        let temp = TempDir::new().unwrap();
        let content = "Hello #rust and #python\nMore #rust and #go\n";
        fs::write(temp.path().join("notes.md"), content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"#[a-zA-Z][a-zA-Z0-9_-]*",
            "extract_unique": true
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let values: Vec<&str> = data["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // No capture group -> full match including #
        assert_eq!(values, vec!["#go", "#python", "#rust"]);
        assert_eq!(data["total_unique"], 3);
    }

    #[test]
    fn test_extract_unique_across_files() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("a.md"), "tags: #alpha #beta\n").unwrap();
        fs::write(temp.path().join("b.md"), "tags: #beta #gamma\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"#([a-zA-Z]+)",
            "extract_unique": true,
            "glob": "*.md"
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let values: Vec<&str> = data["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // Deduplicated across both files
        assert_eq!(values, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn test_extract_unique_case_insensitive() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.md"), "#Rust #rust #RUST\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"#([a-zA-Z]+)",
            "extract_unique": true,
            "case_insensitive": true
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let values = data["values"].as_array().unwrap();
        // Case-insensitive matching but BTreeSet dedup is case-sensitive on extracted text
        assert_eq!(values.len(), 3); // "RUST", "Rust", "rust" are distinct strings
    }

    #[test]
    fn test_extract_unique_max_count() {
        let temp = TempDir::new().unwrap();
        let mut content = String::new();
        for i in 0..20 {
            let _ = writeln!(content, "tag_{i:02}");
        }
        fs::write(temp.path().join("test.txt"), &content).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"tag_\d+",
            "extract_unique": true,
            "max_count": 5
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_unique"], 5);
        assert_eq!(data["truncated"], true);
        assert_eq!(data["values"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn test_extract_unique_no_matches() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "nothing here\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"#[a-z]+",
            "extract_unique": true
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_unique"], 0);
        assert_eq!(data["values"].as_array().unwrap().len(), 0);
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_extract_unique_with_glob_filter() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("notes.md"), "#keep\n").unwrap();
        fs::write(temp.path().join("code.rs"), "#skip\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"#([a-z]+)",
            "extract_unique": true,
            "glob": "*.md"
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let values: Vec<&str> = data["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(values, vec!["keep"]);
    }

    #[test]
    fn test_extract_unique_long_value_truncated_with_warning() {
        let temp = TempDir::new().unwrap();
        let long_value = format!("tag-{}", "x".repeat(10 * 1024));
        fs::write(temp.path().join("values.txt"), format!("{long_value}\n")).unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({
            "pattern": r"(tag-[^\n]+)",
            "extract_unique": true
        });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_unique"], 1);
        assert_eq!(data["truncated"], true);
        let value = data["values"][0].as_str().unwrap();
        assert_eq!(value.len(), MAX_SNIPPET_BYTES);
        assert!(data["warning"].as_str().unwrap().contains("500 bytes"));
    }
}

#[cfg(test)]
mod type_filter_tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn make_ctx(dir: &TempDir) -> ToolContext {
        ToolContext::new(dir.path().to_path_buf(), None)
    }

    #[test]
    fn test_type_filter_rust_restricts_to_rs_files() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("code.rs"), "fn main() {}\n").unwrap();
        fs::write(temp.path().join("notes.md"), "fn docs\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "fn", "type": "rust"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        let files: Vec<&str> = data["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["file"].as_str().unwrap())
            .collect();
        assert!(
            files.iter().all(|f| f.ends_with(".rs")),
            "expected only .rs, got: {files:?}"
        );
        assert_eq!(data["total_matches"], 1);
    }

    #[test]
    fn test_type_filter_unknown_type_returns_invalid_input() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("code.rs"), "fn main() {}\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "fn", "type": "not_a_real_type_xyz"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(
            json_str.contains(r#""code":"invalid_input""#),
            "got: {json_str}"
        );
        assert!(
            json_str.contains("not_a_real_type_xyz"),
            "error should name the bad type"
        );
    }

    #[test]
    fn test_type_filter_absent_searches_all_files() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("code.rs"), "fn main() {}\n").unwrap();
        fs::write(temp.path().join("notes.md"), "fn docs\n").unwrap();

        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "fn"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 2);
    }

    #[test]
    fn test_type_filter_bypassed_for_explicit_file_path() {
        // When path points directly to a file, that file is always searched
        // regardless of the type filter — explicit paths act as an override.
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("script.py"), "fn fake_rust_fn() {}\n").unwrap();

        let ctx = make_ctx(&temp);
        let file = temp.path().join("script.py");
        let input = json!({
            "pattern": "fn",
            "path": file.to_str().unwrap(),
            "type": "rust"
        });

        let result = execute(&input, &ctx);
        assert!(
            result.is_ok(),
            "explicit file path should bypass type filter"
        );
        let data = result.data().unwrap();
        assert_eq!(data["total_matches"], 1);
    }

    #[test]
    fn test_deny_unknown_fields_rejects_extra_keys() {
        let temp = TempDir::new().unwrap();
        let ctx = make_ctx(&temp);
        let input = json!({"pattern": "fn", "unknown_extra_field": true});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }
}
