//! Path normalization + MIME helpers for image files.

/// Normalizes user-provided file paths.
///
/// Handles common drag-and-drop shell escaping (`\ `, `\(`, `\)`) and
/// expands `~/` to the HOME directory when available.
#[must_use]
pub fn normalize_input_path(path: &str) -> std::path::PathBuf {
    // Unescape shell-escaped characters (e.g., "\ " → " ").
    let unescaped = path
        .replace("\\ ", " ")
        .replace("\\(", "(")
        .replace("\\)", ")");

    let path = std::path::Path::new(&unescaped);
    if let Some(rest) = path.to_str().and_then(|s| s.strip_prefix("~/"))
        && let Ok(home) = std::env::var("HOME")
    {
        return std::path::PathBuf::from(home).join(rest);
    }

    path.to_path_buf()
}

/// Returns MIME type inferred from file extension for supported image formats.
#[must_use]
pub fn mime_type_for_extension(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?;

    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Returns file extension inferred from MIME type for supported image formats.
#[must_use]
pub fn extension_for_mime_type(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_mime_to_extension() {
        assert_eq!(extension_for_mime_type("image/png"), Some("png"));
        assert_eq!(extension_for_mime_type("image/jpeg"), Some("jpg"));
        assert_eq!(extension_for_mime_type("image/gif"), Some("gif"));
        assert_eq!(extension_for_mime_type("image/webp"), Some("webp"));
        assert_eq!(extension_for_mime_type("application/pdf"), None);
    }
}
