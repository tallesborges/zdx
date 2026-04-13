//! Path normalization + MIME helpers for image files.
//!
//! `normalize_input_path` and `mime_type_for_extension` are canonical in
//! `zdx_tools` and re-exported here for engine/surface convenience.

pub use zdx_tools::{mime_type_for_extension, normalize_input_path};

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
