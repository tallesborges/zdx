//! Leaf tool implementations for zdx.
//!
//! This crate contains pure/leaf tools that only need a root directory
//! and optional timeout — no engine, config, or thread state.

pub mod apply_patch;
pub mod bash;
pub mod edit;
pub mod fetch_webpage;
pub mod glob;
pub mod grep;
pub mod read;
pub mod web_search;
pub mod write;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
pub use zdx_types::{
    ImageContent, ToolDefinition, ToolOutput, ToolResult, ToolResultBlock, ToolResultContent,
};

// ============================================================================
// Minimal ToolContext for leaf tools
// ============================================================================

/// Context for leaf tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Root directory for file operations.
    pub root: PathBuf,
    /// Optional timeout for tool execution.
    pub timeout: Option<Duration>,
}

impl ToolContext {
    pub fn new(root: PathBuf, timeout: Option<Duration>) -> Self {
        Self { root, timeout }
    }
}

// ============================================================================
// Serde helpers for LLM-resilient deserialization
// ============================================================================

/// Serde helper that accepts either a JSON array of strings or a single string.
///
/// LLMs sometimes send `"search_queries": "single query"` instead of
/// `"search_queries": ["single query"]`. Some manual tool-entry flows also
/// stringify JSON arrays, producing values like
/// `"search_queries": "[\"alpha\",\"beta\"]"`. This module gracefully
/// coerces both forms into a normalized `Vec<String>`.
pub mod string_or_vec {
    use serde::{Deserialize, Deserializer, de};

    fn normalize_vec<E>(values: Vec<String>) -> Result<Option<Vec<String>>, E>
    where
        E: de::Error,
    {
        let mut normalized = Vec::with_capacity(values.len());
        for item in values {
            let trimmed = item.trim();
            if trimmed.is_empty() {
                return Err(de::Error::custom(
                    "search_queries array contains empty string",
                ));
            }
            normalized.push(trimmed.to_string());
        }

        if normalized.is_empty() {
            Ok(None)
        } else {
            Ok(Some(normalized))
        }
    }

    /// Deserializes a `Vec<String>` that also accepts a single string.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StringOrVec {
            String(String),
            Vec(Vec<String>),
        }

        // Option wrapper: if the field is missing (None via serde(default)),
        // this function won't be called – serde returns None directly.
        // When called, the value is present so we parse it.
        let value: Option<StringOrVec> = Option::deserialize(deserializer)?;
        match value {
            None => Ok(None),
            Some(StringOrVec::String(s)) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
                        normalize_vec(values)
                    } else {
                        Ok(Some(vec![trimmed.to_string()]))
                    }
                } else {
                    Ok(Some(vec![trimmed.to_string()]))
                }
            }
            Some(StringOrVec::Vec(v)) => normalize_vec(v),
        }
    }
}

/// Serde helper that accepts either a JSON boolean or a boolean-like string.
///
/// LLMs sometimes send `"full_content": "true"` instead of
/// `"full_content": true`. This module gracefully coerces common string
/// representations into `bool`.
pub mod bool_or_string {
    use serde::{Deserialize, Deserializer, de};

    /// Deserializes a `bool` that also accepts string values like
    /// `"true"`, `"false"`, `"1"`, `"0"`, `"yes"`, `"no"`.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<bool, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum BoolOrString {
            Bool(bool),
            String(String),
        }

        match BoolOrString::deserialize(deserializer)? {
            BoolOrString::Bool(v) => Ok(v),
            BoolOrString::String(raw) => {
                let normalized = raw.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "true" | "1" | "yes" | "y" | "on" => Ok(true),
                    "false" | "0" | "no" | "n" | "off" | "" => Ok(false),
                    _ => Err(de::Error::custom(format!(
                        "expected boolean or boolean-like string, got '{raw}'"
                    ))),
                }
            }
        }
    }
}

pub mod i64_or_string {
    use serde::{Deserialize, Deserializer, de};

    /// Deserializes an `i64` that also accepts a numeric string like `"2"`.
    ///
    /// # Errors
    /// Returns an error if the value cannot be parsed as an integer.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum I64OrString {
            Int(i64),
            String(String),
        }

        match I64OrString::deserialize(deserializer)? {
            I64OrString::Int(v) => Ok(v),
            I64OrString::String(raw) => raw.trim().parse::<i64>().map_err(|_err| {
                de::Error::custom(format!("expected integer or integer string, got '{raw}'"))
            }),
        }
    }
}

// ============================================================================
// Tool input parsing
// ============================================================================

/// Deserializes tool input JSON into `T`, returning a uniform `invalid_input`
/// failure when parsing fails.
///
/// # Errors
/// Returns a `ToolOutput` failure when `value` does not deserialize into `T`.
pub(crate) fn parse_tool_input<T: DeserializeOwned>(
    value: &Value,
    tool_name: &str,
) -> Result<T, ToolOutput> {
    serde_json::from_value(value.clone()).map_err(|e| {
        ToolOutput::failure(
            "invalid_input",
            format!("Invalid input for {tool_name} tool"),
            Some(format!("Parse error: {e}")),
        )
    })
}

// ============================================================================
// Path Resolution Helpers
// ============================================================================

/// Expand environment variables in a path string.
///
/// Supports `$VAR` and `${VAR}` syntax. Unknown variables are left as-is.
pub fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            let braced = chars.peek() == Some(&'{');
            if braced {
                chars.next(); // consume '{'
            }
            let mut var_name = String::new();
            while let Some(&c) = chars.peek() {
                if braced {
                    if c == '}' {
                        chars.next();
                        break;
                    }
                } else if !c.is_ascii_alphanumeric() && c != '_' {
                    break;
                }
                var_name.push(c);
                chars.next();
            }
            if var_name.is_empty() {
                result.push('$');
                if braced {
                    result.push('{');
                }
            } else if let Ok(val) = std::env::var(&var_name) {
                result.push_str(&val);
            } else {
                // Leave unknown vars as-is
                result.push('$');
                if braced {
                    result.push('{');
                }
                result.push_str(&var_name);
                if braced {
                    result.push('}');
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Expand a leading `~` or `~/` in `path` to `$HOME` when available.
///
/// Returns the unmodified path if it does not start with `~`, if `HOME` is
/// not set, or if the leading `~` is followed by characters other than `/`
/// (for example `~user/foo`, which is not supported here).
#[must_use]
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if s != "~" && !s.starts_with("~/") {
        return path.to_path_buf();
    }
    let Ok(home) = std::env::var("HOME") else {
        return path.to_path_buf();
    };
    if s == "~" {
        return PathBuf::from(home);
    }
    PathBuf::from(home).join(&s[2..])
}

/// Resolve a path against the tool root after expanding environment variables
/// and a leading `~`/`~/` to `$HOME`.
#[must_use]
pub fn resolve_path_against_root(path: &Path, root: &Path) -> PathBuf {
    let expanded = expand_env_vars(path.to_string_lossy().as_ref());
    let requested = expand_tilde(Path::new(&expanded));

    if requested.is_absolute() {
        requested
    } else {
        root.join(requested)
    }
}

/// Resolve a non-empty user path string against the tool root.
///
/// Expands environment variables, expands a leading `~`/`~/` to `$HOME`,
/// and joins relative paths with `root`.
///
/// # Errors
/// Returns an error if the path is empty.
pub fn resolve_input_path(path: &str, root: &Path) -> Result<PathBuf, ToolOutput> {
    let display_path = path.trim();
    if display_path.is_empty() {
        return Err(ToolOutput::failure(
            "invalid_input",
            "path cannot be empty",
            None,
        ));
    }

    Ok(resolve_path_against_root(Path::new(display_path), root))
}

/// A user-facing path plus its canonical filesystem resolution.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    pub path: String,
    pub resolved_path: PathBuf,
}

/// Insert `file_path` and optional `resolved_file_path` fields into a JSON object.
pub fn insert_file_path_fields(
    object: &mut Map<String, Value>,
    file_path: &str,
    resolved_file_path: Option<&Path>,
) {
    object.insert(
        "file_path".to_string(),
        Value::String(file_path.to_string()),
    );

    if let Some(resolved_file_path) = resolved_file_path {
        let resolved_display = resolved_file_path.display().to_string();
        if resolved_display != file_path {
            object.insert(
                "resolved_file_path".to_string(),
                Value::String(resolved_display),
            );
        }
    }
}

/// Resolves a path for reading/editing an existing file.
///
/// - Joins relative paths with root
/// - Canonicalizes the path (resolves symlinks, `..`, etc.)
/// - Returns error if the file doesn't exist
///
/// Use this for `read` and `edit` tools where the file must exist.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn resolve_existing_path(path: &str, root: &Path) -> Result<ResolvedPath, ToolOutput> {
    let display_path = path.trim().to_string();
    let full_path = resolve_input_path(&display_path, root)?;

    // Canonicalize to resolve any .. or symlinks (requires file to exist)
    full_path
        .canonicalize()
        .map(|resolved_path| ResolvedPath {
            path: display_path,
            resolved_path,
        })
        .map_err(|e| {
            ToolOutput::failure(
                "path_error",
                format!("Path does not exist '{}'", full_path.display()),
                Some(format!("OS error: {e}")),
            )
        })
}

/// Resolve the search path from user input + context root.
///
/// Returns `root` when the user path is absent or blank; otherwise resolves
/// the path against `root` and errors when it does not exist.
pub(crate) fn resolve_search_path(
    user_path: Option<&str>,
    root: &Path,
) -> Result<PathBuf, ToolOutput> {
    match user_path {
        Some(p) => {
            let p = p.trim();
            if p.is_empty() {
                return Ok(root.to_path_buf());
            }
            let full = resolve_input_path(p, root)?;
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

// ============================================================================
// Image path helpers (moved from zdx-core images::path_mime)
// ============================================================================

/// Normalizes user-provided file paths.
///
/// Handles common drag-and-drop shell escaping (`\ `, `\(`, `\)`) and
/// expands a leading `~`/`~/` to `$HOME` via [`expand_tilde`].
#[must_use]
pub fn normalize_input_path(path: &str) -> PathBuf {
    let unescaped = path
        .replace("\\ ", " ")
        .replace("\\(", "(")
        .replace("\\)", ")");

    expand_tilde(Path::new(&unescaped))
}

/// Returns MIME type inferred from file extension for supported image formats.
#[must_use]
pub fn mime_type_for_extension(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str())?;

    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

// ============================================================================
// Output truncation helpers
// ============================================================================

/// Truncates a byte slice at a valid UTF-8 character boundary at or before
/// `max_bytes`, replacing any invalid UTF-8 via lossy conversion.
///
/// Returns the truncated string, whether truncation occurred, and the original
/// total byte length.
pub(crate) fn truncate_bytes_to_byte_limit(
    bytes: &[u8],
    max_bytes: usize,
) -> (String, bool, usize) {
    let total_bytes = bytes.len();

    if total_bytes <= max_bytes {
        return (
            String::from_utf8_lossy(bytes).into_owned(),
            false,
            total_bytes,
        );
    }

    let truncated_bytes = &bytes[..max_bytes];

    // Walk backwards over UTF-8 continuation bytes (10xxxxxx, 0x80-0xBF).
    let mut end = max_bytes;
    while end > 0 && (truncated_bytes[end - 1] & 0xC0) == 0x80 {
        end -= 1;
    }

    // If the last kept byte starts a multi-byte sequence that would extend
    // past max_bytes, drop it so we never cut mid-character.
    if end > 0 && truncated_bytes[end - 1] >= 0x80 {
        let byte = truncated_bytes[end - 1];
        let char_len = if byte >= 0xF0 {
            4
        } else if byte >= 0xE0 {
            3
        } else if byte >= 0xC0 {
            2
        } else {
            1
        };

        if end - 1 + char_len > max_bytes {
            end -= 1;
        }
    }

    let truncated = String::from_utf8_lossy(&bytes[..end]).into_owned();
    (truncated, true, total_bytes)
}

/// Truncates a string at a valid UTF-8 character boundary at or before
/// `max_bytes`.
///
/// Returns the truncated string and whether truncation occurred.
pub(crate) fn truncate_str_to_byte_limit(text: &str, max_bytes: usize) -> (String, bool) {
    let (truncated, was_truncated, _) = truncate_bytes_to_byte_limit(text.as_bytes(), max_bytes);
    (truncated, was_truncated)
}

#[cfg(test)]
mod truncation_tests {
    use super::truncate_bytes_to_byte_limit;

    #[test]
    fn truncate_bytes_no_truncation() {
        let input = "Hello, world!".as_bytes();
        let (result, truncated, total) = truncate_bytes_to_byte_limit(input, 100);
        assert_eq!(result, "Hello, world!");
        assert!(!truncated);
        assert_eq!(total, 13);
    }

    #[test]
    fn truncate_bytes_multibyte() {
        // "こんにちは" - each character is 3 bytes in UTF-8
        let input = "こんにちは".as_bytes();
        assert_eq!(input.len(), 15); // 5 chars * 3 bytes

        // Truncate at 10 bytes - should keep 3 full characters (9 bytes)
        let (result, truncated, total) = truncate_bytes_to_byte_limit(input, 10);
        assert_eq!(result, "こんに");
        assert!(truncated);
        assert_eq!(total, 15);
    }

    #[test]
    fn truncate_bytes_emoji() {
        // Emoji "😀" is 4 bytes in UTF-8
        let input = "Hi😀there".as_bytes();
        // "Hi" = 2 bytes, "😀" = 4 bytes, "there" = 5 bytes = 11 total

        // Truncate at 5 bytes - should keep "Hi" (2 bytes), skip partial emoji
        let (result, truncated, total) = truncate_bytes_to_byte_limit(input, 5);
        assert_eq!(result, "Hi");
        assert!(truncated);
        assert_eq!(total, 11);
    }
}
