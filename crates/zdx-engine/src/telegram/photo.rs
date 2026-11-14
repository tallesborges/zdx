//! Photos sent with `sendPhoto`, made to fit Telegram's limits: at most a
//! 20:1 aspect ratio (center crop), a 1920 px long edge, and 10 MiB, as JPEG
//! when anything had to change. Shared by every surface that sends photos.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{GenericImageView, ImageReader};

/// Telegram's `sendPhoto` size limit.
pub const TELEGRAM_PHOTO_MAX_BYTES: usize = 10 * 1024 * 1024;
const TELEGRAM_PHOTO_MAX_LONG_EDGE: u32 = 1920;
const TELEGRAM_PHOTO_MAX_ASPECT_RATIO: f64 = 20.0;
const TELEGRAM_PHOTO_MAX_ASPECT_RATIO_INT: u32 = 20;
const TELEGRAM_RESIZE_JPEG_QUALITIES: [u8; 3] = [85, 75, 65];

/// Reads `path` and prepares it as a photo, typed by its content.
///
/// # Errors
/// Returns an error when the file cannot be read or decoded where it must be
/// re-encoded, or when the result is still over [`TELEGRAM_PHOTO_MAX_BYTES`].
pub fn prepare_photo_file(path: &Path) -> Result<PreparedPhoto> {
    let photo_data =
        std::fs::read(path).with_context(|| format!("read photo file {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("photo.png");
    let mime_type = infer::get(&photo_data).map_or_else(
        || "application/octet-stream".to_string(),
        |kind| kind.mime_type().to_string(),
    );
    let prepared = prepare_photo(photo_data, file_name, &mime_type)
        .with_context(|| format!("prepare photo for Telegram {}", path.display()))?;
    if prepared.bytes.len() > TELEGRAM_PHOTO_MAX_BYTES {
        bail!(
            "photo exceeds Telegram limit ({} bytes > {} bytes)",
            prepared.bytes.len(),
            TELEGRAM_PHOTO_MAX_BYTES
        );
    }
    Ok(prepared)
}

/// A photo ready for `sendPhoto`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPhoto {
    pub bytes: Vec<u8>,
    pub file_name: String,
    pub mime_type: String,
    /// The bytes differ from the file (cropped, resized, or re-encoded).
    pub changed: bool,
}

/// Prepares photo bytes; unchanged when they already fit.
///
/// # Errors
/// Returns an error when an image that must change cannot be decoded or
/// encoded.
pub fn prepare_photo(
    photo_data: Vec<u8>,
    file_name: &str,
    mime_type: &str,
) -> Result<PreparedPhoto> {
    let Some((width, height)) = read_photo_dimensions(&photo_data) else {
        return Ok(PreparedPhoto {
            bytes: photo_data,
            file_name: file_name.to_string(),
            mime_type: mime_type.to_string(),
            changed: false,
        });
    };

    let needs_full_hd_resize = is_photo_long_edge_too_large(width, height);
    let needs_aspect_crop = is_photo_aspect_ratio_invalid(width, height);
    let needs_reencode_for_size = photo_data.len() > TELEGRAM_PHOTO_MAX_BYTES;
    let needs_reencode_for_format = !mime_type.eq_ignore_ascii_case("image/jpeg");
    if !needs_full_hd_resize
        && !needs_aspect_crop
        && !needs_reencode_for_size
        && !needs_reencode_for_format
    {
        return Ok(PreparedPhoto {
            bytes: photo_data,
            file_name: file_name.to_string(),
            mime_type: mime_type.to_string(),
            changed: false,
        });
    }

    let image = image::load_from_memory(&photo_data).context("decode photo")?;
    let normalized = normalize_photo_for_telegram(image);
    let resized_bytes = encode_jpeg_with_size_cap(&normalized).context("encode resized photo")?;
    let resized_name = jpeg_file_name(file_name);

    Ok(PreparedPhoto {
        bytes: resized_bytes,
        file_name: resized_name,
        mime_type: "image/jpeg".to_string(),
        changed: true,
    })
}

fn read_photo_dimensions(photo_data: &[u8]) -> Option<(u32, u32)> {
    let cursor = Cursor::new(photo_data);
    let reader = ImageReader::new(cursor).with_guessed_format().ok()?;
    reader.into_dimensions().ok()
}

fn normalize_photo_for_telegram(image: image::DynamicImage) -> image::DynamicImage {
    let mut normalized = image;

    let (mut width, mut height) = normalized.dimensions();
    if is_photo_aspect_ratio_invalid(width, height) {
        normalized = crop_photo_to_aspect_ratio_limit(&normalized);
        (width, height) = normalized.dimensions();
    }

    if is_photo_long_edge_too_large(width, height) {
        normalized = resize_photo_to_full_hd_limit(&normalized);
    }

    normalized
}

fn crop_photo_to_aspect_ratio_limit(image: &image::DynamicImage) -> image::DynamicImage {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return image.clone();
    }

    if width >= height {
        let target_width = (height.saturating_mul(TELEGRAM_PHOTO_MAX_ASPECT_RATIO_INT)).min(width);
        if target_width == width {
            return image.clone();
        }
        let x = (width - target_width) / 2;
        image.crop_imm(x, 0, target_width, height)
    } else {
        let target_height = (width.saturating_mul(TELEGRAM_PHOTO_MAX_ASPECT_RATIO_INT)).min(height);
        if target_height == height {
            return image.clone();
        }
        let y = (height - target_height) / 2;
        image.crop_imm(0, y, width, target_height)
    }
}

fn resize_photo_to_full_hd_limit(image: &image::DynamicImage) -> image::DynamicImage {
    let (width, height) = image.dimensions();
    let longest_edge = f64::from(width.max(height));
    let scale = f64::from(TELEGRAM_PHOTO_MAX_LONG_EDGE) / longest_edge;
    let scaled_width = ((f64::from(width) * scale).floor()).max(1.0) as u32;
    let scaled_height = ((f64::from(height) * scale).floor()).max(1.0) as u32;
    image.resize_exact(scaled_width, scaled_height, FilterType::Lanczos3)
}

fn encode_jpeg_with_size_cap(image: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut last = Vec::new();

    for quality in TELEGRAM_RESIZE_JPEG_QUALITIES {
        let bytes = encode_jpeg(image, quality)?;
        if bytes.len() <= TELEGRAM_PHOTO_MAX_BYTES {
            return Ok(bytes);
        }
        last = bytes;
    }

    Ok(last)
}

fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder
        .encode_image(image)
        .with_context(|| format!("encode JPEG (quality {quality})"))?;
    Ok(bytes)
}

fn jpeg_file_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(|| "photo.jpg".to_string(), |stem| format!("{stem}.jpg"))
}

fn is_photo_long_edge_too_large(width: u32, height: u32) -> bool {
    width.max(height) > TELEGRAM_PHOTO_MAX_LONG_EDGE
}

fn is_photo_aspect_ratio_invalid(width: u32, height: u32) -> bool {
    let min = width.min(height);
    if min == 0 {
        return true;
    }
    let max = width.max(height);
    f64::from(max) / f64::from(min) > TELEGRAM_PHOTO_MAX_ASPECT_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_full_hd_long_edge_limit() {
        assert!(is_photo_long_edge_too_large(2560, 1440));
        assert!(is_photo_long_edge_too_large(1080, 2400));
        assert!(!is_photo_long_edge_too_large(1920, 1080));
        assert!(!is_photo_long_edge_too_large(1080, 1920));
    }

    #[test]
    fn detects_aspect_ratio_limit() {
        assert!(is_photo_aspect_ratio_invalid(30000, 1000));
        assert!(!is_photo_aspect_ratio_invalid(3840, 2160));
    }

    #[test]
    fn small_jpeg_is_unchanged_and_large_png_becomes_a_fitting_jpeg() {
        let encode = |image: image::DynamicImage, format| {
            let mut bytes = Vec::new();
            image
                .write_to(&mut Cursor::new(&mut bytes), format)
                .unwrap();
            bytes
        };
        let small = encode(
            image::DynamicImage::new_rgb8(64, 32),
            image::ImageFormat::Jpeg,
        );
        let kept = prepare_photo(small.clone(), "a.jpg", "image/jpeg").unwrap();
        assert!(!kept.changed);
        assert_eq!(kept.bytes, small);

        let large = encode(
            image::DynamicImage::new_rgb8(4000, 1000),
            image::ImageFormat::Png,
        );
        let fitted = prepare_photo(large, "wide.png", "image/png").unwrap();
        assert!(fitted.changed);
        assert_eq!(fitted.mime_type, "image/jpeg");
        assert_eq!(fitted.file_name, "wide.jpg");
        let (width, height) = read_photo_dimensions(&fitted.bytes).unwrap();
        assert_eq!((width, height), (1920, 480));
    }
}
