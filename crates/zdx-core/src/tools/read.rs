//! Read file tool.
//!
//! Allows the agent to read file contents from the filesystem.
//! Supports both text files and images (JPEG, PNG, GIF, WebP).

use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde_json::{Value, json};

/// Deserialize an optional usize that may be provided as either a number or a string.
///
/// This handles cases where the AI passes `"600"` instead of `600` for numeric fields.
fn deserialize_optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value: Option<Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| D::Error::custom("expected positive integer"))
            .and_then(|n| {
                usize::try_from(n)
                    .map(Some)
                    .map_err(|_| D::Error::custom("number too large"))
            }),
        Some(Value::String(s)) => s
            .parse::<usize>()
            .map(Some)
            .map_err(|_| D::Error::custom(format!("invalid number string: {s}"))),
        Some(_) => Err(D::Error::custom("expected number or numeric string")),
    }
}

use super::{ToolContext, ToolDefinition, resolve_existing_path};
use crate::core::events::{ImageContent, ToolOutput};

/// Maximum number of lines to return (truncation threshold).
const MAX_LINES: usize = 2000;

/// Maximum characters per line before silent truncation.
const MAX_LINE_LENGTH: usize = 500;

/// Maximum bytes to read per line (memory-safe buffer for huge single-line files).
/// Set to `MAX_LINE_LENGTH` * 4 to accommodate multi-byte UTF-8 characters.
const MAX_LINE_BYTES: usize = MAX_LINE_LENGTH * 4;

/// Maximum bytes per page (secondary safety limit).
/// Even within line-count constraints, a single page should not exceed 40KB
/// to prevent context window bloat from files with many long lines.
const MAX_PAGE_BYTES: usize = 40 * 1024; // 40KB

/// Maximum image file size (3.75MB).
/// Anthropic API limit is ~5MB for base64-encoded data.
/// Base64 expands by ~33% (4/3 ratio), so: 5MB ÷ 1.33 ≈ 3.75MB raw.
const MAX_IMAGE_BYTES: u64 = 3_932_160; // 3.75 * 1024 * 1024

/// Supported image MIME types for Anthropic vision API.
const SUPPORTED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Detects image MIME type from file magic bytes.
///
/// Returns `Some(mime_type)` if the file is a supported image format
/// (JPEG, PNG, GIF, WebP), otherwise returns `None`.
///
/// Detection is based on file content (magic bytes), not extension.
fn detect_image_mime(path: &Path) -> Option<String> {
    // Read first 4KB for magic byte detection
    let mut file = File::open(path).ok()?;
    let mut buffer = [0u8; 4096];
    let bytes_read = file.read(&mut buffer).ok()?;

    if bytes_read == 0 {
        return None;
    }

    // Use infer crate to detect MIME type from magic bytes
    let kind = infer::get(&buffer[..bytes_read])?;
    let mime = kind.mime_type();

    // Only return if it's a supported image format
    if SUPPORTED_IMAGE_MIMES.contains(&mime) {
        Some(mime.to_string())
    } else {
        None
    }
}

/// Returns the tool definition for the read tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Read".to_string(),
        description: "Read the contents of a file. Returns the file content as text. Also supports reading image files (JPEG, PNG, GIF, WebP) for visual analysis.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to root directory)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed, default: 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default: 2000)"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct ReadInput {
    path: String,
    /// Line number to start reading from (1-indexed, default: 1)
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    offset: Option<usize>,
    /// Maximum number of lines to return (default: `MAX_LINES`)
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    limit: Option<usize>,
}

/// Executes the read tool and returns a structured envelope.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: ReadInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for read tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let file_path = match resolve_existing_path(&input.path, &ctx.root) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Check if this is an image file
    if let Some(mime_type) = detect_image_mime(&file_path) {
        return read_image(&file_path, &mime_type);
    }

    // Read as text file with offset/limit
    let offset = input.offset.unwrap_or(1).max(1); // 1-indexed, minimum 1
    let limit = input.limit.unwrap_or(MAX_LINES).min(MAX_LINES); // Cap at MAX_LINES
    read_text(&file_path, offset, limit)
}

/// Reads an image file and returns it as base64-encoded content.
fn read_image(path: &Path, mime_type: &str) -> ToolOutput {
    // Check file size
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read metadata for '{}'", path.display()),
                Some(format!("OS error: {e}")),
            );
        }
    };

    let file_size = metadata.len();
    if file_size > MAX_IMAGE_BYTES {
        return ToolOutput::failure(
            "image_too_large",
            format!("Image file '{}' is too large", path.display()),
            Some(format!(
                "Size: {file_size} bytes, Maximum: 3932160 bytes (3.75 MB)"
            )),
        );
    }

    // Read binary content
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read image file '{}'", path.display()),
                Some(format!("OS error: {e}")),
            );
        }
    };

    // Base64 encode
    let base64_data = BASE64.encode(&data);

    let image = ImageContent {
        mime_type: mime_type.to_string(),
        data: base64_data,
    };

    ToolOutput::success_with_image(
        json!({
            "path": path.display().to_string(),
            "type": "image",
            "mime_type": mime_type,
            "bytes": file_size,
        }),
        image,
    )
}

/// Reads a text file with line-based truncation and offset/limit support.
///
/// - Skips to `offset` line (1-indexed)
/// - Returns at most `limit` lines
/// - Enforces a secondary `MAX_PAGE_BYTES` (40KB) limit per page
/// - Silently truncates individual lines at `MAX_LINE_LENGTH` characters
/// - Uses `MAX_LINE_BYTES` buffer to prevent OOM on huge single-line files
/// - Always scans entire file for `total_lines` count
fn read_text(path: &Path, offset: usize, limit: usize) -> ToolOutput {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read file '{}'", path.display()),
                Some(format!("OS error: {e}")),
            );
        }
    };

    let mut reader = BufReader::new(file);
    let mut collected_lines: Vec<String> = Vec::with_capacity(limit.min(1000));
    let mut total_lines: usize = 0;
    let mut accumulated_bytes: usize = 0;
    let mut byte_limited = false;
    let mut buffer = Vec::with_capacity(MAX_LINE_BYTES);

    // Convert 1-indexed offset to 0-indexed for comparison
    let start_line = offset.saturating_sub(1);

    loop {
        buffer.clear();
        let mut drained_line_ending_for_line: Option<&'static str> = None;

        // Read up to MAX_LINE_BYTES or until newline
        let bytes_read = match reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64)
            .read_until(b'\n', &mut buffer)
        {
            Ok(n) => n,
            Err(e) => {
                return ToolOutput::failure(
                    "read_error",
                    format!("Failed to read file '{}'", path.display()),
                    Some(format!("OS error: {e}")),
                );
            }
        };

        if bytes_read == 0 {
            break; // EOF
        }

        total_lines += 1;

        // Check if we hit MAX_LINE_BYTES without finding newline (huge line)
        // Need to drain remainder of line to count properly
        let found_newline = buffer.last() == Some(&b'\n');
        if !found_newline && bytes_read == MAX_LINE_BYTES {
            // Drain rest of this line without discarding bytes after newline.
            let mut drained_line_ending = None;
            loop {
                let available = match reader.fill_buf() {
                    Ok(buf) => buf,
                    Err(e) => {
                        return ToolOutput::failure(
                            "read_error",
                            format!("Failed to read file '{}'", path.display()),
                            Some(format!("OS error: {e}")),
                        );
                    }
                };

                if available.is_empty() {
                    break; // EOF
                }

                if let Some(pos) = available.iter().position(|&b| b == b'\n') {
                    drained_line_ending = Some(if pos > 0 && available[pos - 1] == b'\r' {
                        "\r\n"
                    } else {
                        "\n"
                    });
                    reader.consume(pos + 1);
                    break;
                }

                let len = available.len();
                reader.consume(len);
            }
            drained_line_ending_for_line = drained_line_ending;
        }

        // Current line index (0-indexed)
        let current_line_idx = total_lines - 1;

        // Collect line if within range [start_line, start_line + limit)
        // AND we haven't hit the byte limit yet
        if current_line_idx >= start_line && collected_lines.len() < limit && !byte_limited {
            // Convert to string (lossy for invalid UTF-8)
            let line_str = String::from_utf8_lossy(&buffer);
            let line_str = line_str.as_ref();
            let (line_body, line_ending) = if let Some(stripped) = line_str.strip_suffix("\r\n") {
                (stripped, "\r\n")
            } else if let Some(stripped) = line_str.strip_suffix('\n') {
                (stripped, "\n")
            } else {
                let ending = if bytes_read == MAX_LINE_BYTES {
                    // We drained the rest of this line; preserve detected line ending.
                    drained_line_ending_for_line.unwrap_or("")
                } else {
                    ""
                };
                (line_str, ending)
            };

            // Truncate at MAX_LINE_LENGTH chars (silent, no marker), preserve line ending.
            let mut truncated_line: String = line_body.chars().take(MAX_LINE_LENGTH).collect();
            truncated_line.push_str(line_ending);

            // Check if adding this line would exceed the byte limit
            let line_bytes = truncated_line.len();
            if accumulated_bytes + line_bytes > MAX_PAGE_BYTES {
                byte_limited = true;
                // Don't add this line; we've hit the byte limit
            } else {
                accumulated_bytes += line_bytes;
                collected_lines.push(truncated_line);
            }
        }
    }

    let lines_shown = collected_lines.len();
    // Truncated if there are more lines after our window OR we hit byte limit
    let truncated = (start_line + lines_shown) < total_lines || byte_limited;

    // Join lines (they already include trailing newlines if present in original)
    let content = collected_lines.concat();

    ToolOutput::success(json!({
        "path": path.display().to_string(),
        "content": content,
        "offset": offset,
        "lines_shown": lines_shown,
        "total_lines": total_lines,
        "truncated": truncated,
        "byte_limited": byte_limited
    }))
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_read_file_success() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "hello world");
        assert_eq!(data["truncated"], false);
        assert_eq!(data["lines_shown"], 1);
        assert_eq!(data["total_lines"], 1);
        assert_eq!(data["offset"], 1);
    }

    #[test]
    fn test_read_nested_file() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("subdir")).unwrap();
        let file_path = temp.path().join("subdir/nested.txt");
        fs::write(&file_path, "nested content").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "subdir/nested.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "nested content");
    }

    #[test]
    fn test_read_file_not_found() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "nonexistent.txt"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":false"#));
        assert!(json_str.contains(r#""code":"path_error""#));
    }

    #[test]
    fn test_read_outside_root_allowed() {
        let root = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("external.txt");
        fs::write(&outside_file, "external content").unwrap();

        let ctx = ToolContext::new(root.path().to_path_buf(), None);
        let input = json!({ "path": outside_file.to_str().unwrap() });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "external content");
    }

    #[test]
    fn test_read_huge_single_line() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("huge_line.txt");
        // Create a file with a 100KB single line (tests memory safety)
        let huge_line = "y".repeat(100 * 1024);
        fs::write(&file_path, &huge_line).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "huge_line.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["truncated"], false);
        assert_eq!(data["lines_shown"], 1);
        assert_eq!(data["total_lines"], 1);
        // Content should be truncated to MAX_LINE_LENGTH (500) chars
        let content = data["content"].as_str().unwrap();
        assert_eq!(content.len(), 500);
    }

    #[test]
    fn test_read_empty_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("empty.txt");
        fs::write(&file_path, "").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "empty.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "");
        assert_eq!(data["lines_shown"], 0);
        assert_eq!(data["total_lines"], 0);
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_read_preserves_line_endings() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("lines.txt");
        fs::write(&file_path, "line1\nline2\nline3").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "lines.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "line1\nline2\nline3");
        assert_eq!(data["lines_shown"], 3);
        assert_eq!(data["total_lines"], 3);
    }

    #[test]
    fn test_read_offset_beyond_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("lines.txt");
        fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "lines.txt", "offset": 100});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "");
        assert_eq!(data["offset"], 100);
        assert_eq!(data["lines_shown"], 0);
        assert_eq!(data["total_lines"], 3);
        assert_eq!(data["truncated"], false);
    }

    #[test]
    fn test_read_offset_zero_treated_as_one() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("lines.txt");
        fs::write(&file_path, "line1\nline2\n").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // offset: 0 should be treated as offset: 1
        let input = json!({"path": "lines.txt", "offset": 0});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "line1\nline2\n");
        assert_eq!(data["offset"], 1); // Normalized to 1
        assert_eq!(data["lines_shown"], 2);
    }

    #[test]
    fn test_read_limit_capped_at_max() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("large.txt");
        // Create a file with 100 lines
        let content = (0..100)
            .map(|i| {
                let mut line = i.to_string();
                line.insert_str(0, "line ");
                line.push('\n');
                line
            })
            .collect::<String>();
        fs::write(&file_path, &content).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Request more than MAX_LINES (2000) - should be capped
        let input = json!({"path": "large.txt", "limit": 10000});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        // Should return all 100 lines (under cap)
        assert_eq!(data["lines_shown"], 100);
        assert_eq!(data["total_lines"], 100);
    }

    #[test]
    fn test_read_paging_through_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("lines.txt");
        fs::write(&file_path, "a\nb\nc\nd\ne\nf\n").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);

        // Page 1: lines 1-2
        let input = json!({"path": "lines.txt", "offset": 1, "limit": 2});
        let result = execute(&input, &ctx);
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "a\nb\n");
        assert_eq!(data["truncated"], true);

        // Page 2: lines 3-4
        let input = json!({"path": "lines.txt", "offset": 3, "limit": 2});
        let result = execute(&input, &ctx);
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "c\nd\n");
        assert_eq!(data["truncated"], true);

        // Page 3: lines 5-6
        let input = json!({"path": "lines.txt", "offset": 5, "limit": 2});
        let result = execute(&input, &ctx);
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "e\nf\n");
        assert_eq!(data["truncated"], false); // Last page
    }

    #[test]
    fn test_read_byte_limit_with_long_lines() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("long_lines.txt");
        // Create file with 300 lines of 200 chars each = 60KB total
        // Should hit 40KB byte limit around line 204
        let mut content = String::new();
        for i in 0..300 {
            writeln!(content, "{i:0>199}").expect("write to string"); // 199 digits + newline = 200 bytes
        }
        fs::write(&file_path, &content).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "long_lines.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["truncated"], true);
        assert_eq!(data["byte_limited"], true);
        assert_eq!(data["total_lines"], 300);
        // 40KB / 200 bytes = 204 lines (last one puts us over)
        let lines_shown = data["lines_shown"].as_u64().unwrap();
        assert!((200..=210).contains(&lines_shown));
    }

    #[test]
    fn test_read_line_limit_before_byte_limit() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("tiny_lines.txt");
        // Create file with 3000 very short lines (2 bytes each = 6KB total)
        // Line limit (2000) should kick in before byte limit (40KB)
        let content: String = (0..3000).map(|_| "x\n").collect();
        fs::write(&file_path, &content).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "tiny_lines.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["truncated"], true);
        assert_eq!(data["byte_limited"], false); // Line limit hit first
        assert_eq!(data["lines_shown"], 2000);
        assert_eq!(data["total_lines"], 3000);
    }

    #[test]
    fn test_read_invalid_input() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"wrong_field": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
    }

    // String-to-number coercion tests (AI sometimes passes "600" instead of 600)

    #[test]
    fn test_read_offset_and_limit_as_strings() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("lines.txt");
        fs::write(&file_path, "line1\nline2\nline3\nline4\nline5\n").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        // Pass both offset and limit as strings
        let input = json!({"path": "lines.txt", "offset": "2", "limit": "2"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(data["content"], "line2\nline3\n");
        assert_eq!(data["offset"], 2);
        assert_eq!(data["lines_shown"], 2);
    }

    // MIME detection tests

    #[test]
    fn test_detect_jpeg() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.jpg");
        // Minimal JPEG: SOI marker + APP0 + EOI
        let jpeg_bytes: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
        ];
        fs::write(&path, jpeg_bytes).unwrap();

        assert_eq!(detect_image_mime(&path), Some("image/jpeg".to_string()));
    }

    #[test]
    fn test_detect_png() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.png");
        // Minimal PNG: signature + IHDR chunk + IEND chunk
        #[rustfmt::skip]
        let png_bytes: &[u8] = &[
            // PNG signature
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            // IHDR chunk
            0x00, 0x00, 0x00, 0x0D, // length
            0x49, 0x48, 0x44, 0x52, // "IHDR"
            0x00, 0x00, 0x00, 0x01, // width
            0x00, 0x00, 0x00, 0x01, // height
            0x08, 0x02,             // bit depth, color type
            0x00, 0x00, 0x00,       // compression, filter, interlace
            0x90, 0x77, 0x53, 0xDE, // CRC
            // IEND chunk
            0x00, 0x00, 0x00, 0x00,
            0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        fs::write(&path, png_bytes).unwrap();

        assert_eq!(detect_image_mime(&path), Some("image/png".to_string()));
    }

    #[test]
    fn test_detect_gif() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.gif");
        // Minimal GIF89a header
        #[rustfmt::skip]
        let gif_bytes: &[u8] = &[
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // "GIF89a"
            0x01, 0x00, 0x01, 0x00,             // width, height
            0x00, 0x00, 0x00,                   // flags, bg, aspect
            0x3B,                               // trailer
        ];
        fs::write(&path, gif_bytes).unwrap();

        assert_eq!(detect_image_mime(&path), Some("image/gif".to_string()));
    }

    #[test]
    fn test_detect_webp() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.webp");
        // Minimal WebP header (RIFF + WEBP)
        #[rustfmt::skip]
        let webp_bytes: &[u8] = &[
            0x52, 0x49, 0x46, 0x46, // "RIFF"
            0x1A, 0x00, 0x00, 0x00, // file size
            0x57, 0x45, 0x42, 0x50, // "WEBP"
            0x56, 0x50, 0x38, 0x20, // "VP8 "
            0x0E, 0x00, 0x00, 0x00, // chunk size
            0x30, 0x01, 0x00, 0x9D, // VP8 bitstream
            0x01, 0x2A, 0x01, 0x00,
            0x01, 0x00, 0x00, 0x34,
            0x25, 0x9F, 0x00,
        ];
        fs::write(&path, webp_bytes).unwrap();

        assert_eq!(detect_image_mime(&path), Some("image/webp".to_string()));
    }

    #[test]
    fn test_detect_text_file_returns_none() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.txt");
        fs::write(&path, "Hello, world!").unwrap();

        assert_eq!(detect_image_mime(&path), None);
    }

    #[test]
    fn test_detect_nonexistent_file_returns_none() {
        let path = Path::new("/nonexistent/path/to/file.jpg");
        assert_eq!(detect_image_mime(path), None);
    }

    #[test]
    fn test_detect_empty_file_returns_none() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("empty.jpg");
        File::create(&path).unwrap();

        assert_eq!(detect_image_mime(&path), None);
    }

    #[test]
    fn test_detect_unsupported_image_returns_none() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.bmp");
        // BMP header (not supported by Anthropic)
        #[rustfmt::skip]
        let bmp_bytes: &[u8] = &[
            0x42, 0x4D,             // "BM"
            0x46, 0x00, 0x00, 0x00, // file size
            0x00, 0x00, 0x00, 0x00, // reserved
            0x36, 0x00, 0x00, 0x00, // offset to pixel data
            0x28, 0x00, 0x00, 0x00, // DIB header size
            0x01, 0x00, 0x00, 0x00, // width
            0x01, 0x00, 0x00, 0x00, // height
            0x01, 0x00,             // planes
            0x18, 0x00,             // bits per pixel
        ];
        fs::write(&path, bmp_bytes).unwrap();

        assert_eq!(detect_image_mime(&path), None);
    }

    #[test]
    fn test_wrong_extension_detected_by_content() {
        // A PNG file with .txt extension should still be detected as PNG
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("actually_png.txt");
        #[rustfmt::skip]
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D,
            0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x01,
            0x08, 0x02,
            0x00, 0x00, 0x00,
            0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x00,
            0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        fs::write(&path, png_bytes).unwrap();

        assert_eq!(detect_image_mime(&path), Some("image/png".to_string()));
    }

    // Image reading tests

    #[test]
    fn test_read_image_returns_base64() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.png");

        // Minimal PNG
        #[rustfmt::skip]
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D,
            0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x01,
            0x08, 0x02,
            0x00, 0x00, 0x00,
            0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x00,
            0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        fs::write(&path, png_bytes).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.png"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        // Check JSON output (without image data)
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""type":"image""#));
        assert!(json_str.contains(r#""mime_type":"image/png""#));

        // Check image content is present
        let image = result.image().expect("should have image content");
        assert_eq!(image.mime_type, "image/png");

        // Verify base64 decodes back to original
        let decoded = BASE64.decode(&image.data).expect("should be valid base64");
        assert_eq!(decoded, png_bytes);
    }

    #[test]
    fn test_read_image_returns_correct_metadata() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.jpg");

        // Minimal JPEG
        let jpeg_bytes: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
        ];
        fs::write(&path, jpeg_bytes).unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.jpg"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        // Check the data field contains expected metadata
        let data = result.data().expect("should have data");
        assert_eq!(data["type"], "image");
        assert_eq!(data["mime_type"], "image/jpeg");
        assert_eq!(data["bytes"], jpeg_bytes.len());
    }

    #[test]
    fn test_read_image_too_large_returns_error() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("large.png");

        // Create a file with PNG header but larger than 3.75MB
        // We use a sparse approach: write PNG header then seek/write at end
        let mut file = File::create(&path).unwrap();

        // PNG header
        #[rustfmt::skip]
        let png_header: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D,
            0x49, 0x48, 0x44, 0x52,
        ];
        file.write_all(png_header).unwrap();

        // Extend to 4MB (just over the 3.75MB limit)
        file.set_len(4 * 1024 * 1024).unwrap();
        drop(file);

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "large.png"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());

        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"image_too_large""#));
        // Details now contains the size information
        assert!(json_str.contains("3.75 MB"));
    }

    #[test]
    fn test_read_text_file_no_image_content() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        // Text files should NOT have image content
        assert!(result.image().is_none());
    }
}
