//! Read file tool.
//!
//! Allows the agent to read file contents from the filesystem.
//! Supports both text files and images (JPEG, PNG, GIF, WebP).

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::{fs, path::PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::{ImageContent, ToolOutput};

/// Maximum text file size before truncation (50KB).
const MAX_TEXT_BYTES: usize = 50 * 1024;

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
        name: "read".to_string(),
        description: "Read the contents of a file. Returns the file content as text. Also supports reading image files (JPEG, PNG, GIF, WebP) for visual analysis.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to root directory)"
                }
            },
            "required": ["path"]
        }),
    }
}

#[derive(Debug, Deserialize)]
struct ReadInput {
    path: String,
}

/// Executes the read tool and returns a structured envelope.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: ReadInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Invalid input for read tool: {}", e),
            );
        }
    };

    let file_path = match resolve_path(&input.path, &ctx.root) {
        Ok(p) => p,
        Err(e) => return ToolOutput::failure("path_error", e),
    };

    // Check if this is an image file
    if let Some(mime_type) = detect_image_mime(&file_path) {
        return read_image(&file_path, &mime_type);
    }

    // Read as text file
    read_text(&file_path)
}

/// Reads an image file and returns it as base64-encoded content.
fn read_image(path: &Path, mime_type: &str) -> ToolOutput {
    // Check file size
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read file metadata '{}': {}", path.display(), e),
            );
        }
    };

    let file_size = metadata.len();
    if file_size > MAX_IMAGE_BYTES {
        return ToolOutput::failure(
            "image_too_large",
            format!(
                "Image file '{}' is too large ({:.2} MB). Maximum size is 3.75 MB.",
                path.display(),
                file_size as f64 / (1024.0 * 1024.0)
            ),
        );
    }

    // Read binary content
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read image file '{}': {}", path.display(), e),
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

/// Reads a text file with truncation for large files.
fn read_text(path: &Path) -> ToolOutput {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput::failure(
                "read_error",
                format!("Failed to read file '{}': {}", path.display(), e),
            );
        }
    };

    let bytes = content.len();
    let (content, truncated) = if bytes > MAX_TEXT_BYTES {
        (content[..MAX_TEXT_BYTES].to_string(), true)
    } else {
        (content, false)
    };

    ToolOutput::success(json!({
        "path": path.display().to_string(),
        "content": content,
        "truncated": truncated,
        "bytes": bytes
    }))
}

/// Resolves a path relative to the root directory.
fn resolve_path(path: &str, root: &Path) -> Result<PathBuf, String> {
    let requested = Path::new(path);

    // Join with root (handles both absolute and relative paths)
    let full_path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };

    // Canonicalize to resolve any .. or symlinks
    let canonical = full_path
        .canonicalize()
        .map_err(|e| format!("Path does not exist '{}': {}", full_path.display(), e))?;

    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_read_file_success() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""ok":true"#));
        assert!(json_str.contains(r#""content":"hello world""#));
        assert!(json_str.contains(r#""truncated":false"#));
        assert!(json_str.contains(r#""bytes":11"#));
    }

    #[test]
    fn test_read_nested_file() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("subdir")).unwrap();
        let file_path = temp.path().join("subdir/nested.txt");
        fs::write(&file_path, "nested content").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "subdir/nested.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""content":"nested content""#));
    }

    #[test]
    fn test_read_file_not_found() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
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

        let ctx = ToolContext::with_timeout(root.path().to_path_buf(), None);
        let input = json!({ "path": outside_file.to_str().unwrap() });

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""content":"external content""#));
    }

    #[test]
    fn test_read_large_file_truncated() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("large.txt");
        // Create a file larger than MAX_BYTES (50KB)
        let content = "x".repeat(60 * 1024);
        fs::write(&file_path, &content).unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "large.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""truncated":true"#));
        // bytes should reflect original size
        assert!(json_str.contains(r#""bytes":61440"#));
    }

    #[test]
    fn test_read_invalid_input() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"wrong_field": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());
        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"invalid_input""#));
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

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
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

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
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
        use std::io::Write;
        file.write_all(png_header).unwrap();

        // Extend to 4MB (just over the 3.75MB limit)
        file.set_len(4 * 1024 * 1024).unwrap();
        drop(file);

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "large.png"});

        let result = execute(&input, &ctx);
        assert!(!result.is_ok());

        let json_str = result.to_json_string();
        assert!(json_str.contains(r#""code":"image_too_large""#));
        assert!(json_str.contains("Maximum size is 3.75 MB"));
    }

    #[test]
    fn test_read_text_file_no_image_content() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({"path": "test.txt"});

        let result = execute(&input, &ctx);
        assert!(result.is_ok());

        // Text files should NOT have image content
        assert!(result.image().is_none());
    }
}
