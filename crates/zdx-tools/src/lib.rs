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

/// Resolve a path against the tool root after expanding environment variables.
#[must_use]
pub fn resolve_path_against_root(path: &Path, root: &Path) -> PathBuf {
    let expanded = expand_env_vars(path.to_string_lossy().as_ref());
    let requested = Path::new(&expanded);

    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    }
}

/// Resolve a non-empty user path string against the tool root.
///
/// Expands environment variables and joins relative paths with `root`.
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

// ============================================================================
// Image path helpers (moved from zdx-core images::path_mime)
// ============================================================================

/// Normalizes user-provided file paths.
///
/// Handles common drag-and-drop shell escaping (`\ `, `\(`, `\)`) and
/// expands `~/` to the HOME directory when available.
#[must_use]
pub fn normalize_input_path(path: &str) -> PathBuf {
    // Unescape shell-escaped characters (e.g., "\ " → " ").
    let unescaped = path
        .replace("\\ ", " ")
        .replace("\\(", "(")
        .replace("\\)", ")");

    let path = Path::new(&unescaped);
    if let Some(rest) = path.to_str().and_then(|s| s.strip_prefix("~/"))
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }

    path.to_path_buf()
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
