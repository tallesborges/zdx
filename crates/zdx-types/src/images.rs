//! Pure image byte helpers.

/// Detects a raster image MIME type (PNG, JPEG, GIF, WebP) from the leading
/// magic bytes. Returns `None` for anything else.
///
/// File extensions and provider metadata can disagree with the actual bytes, so
/// callers that label image data should trust this over either.
#[must_use]
pub fn sniff_raster_image_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::sniff_raster_image_mime;

    #[test]
    fn sniffs_raster_signatures() {
        assert_eq!(
            sniff_raster_image_mime(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(
            sniff_raster_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_raster_image_mime(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            sniff_raster_image_mime(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_raster_image_mime(b"<svg/>"), None);
        assert_eq!(sniff_raster_image_mime(b""), None);
    }
}
