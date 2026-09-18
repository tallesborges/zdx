//! Pixel-dimension clamping for provider-bound images.
//!
//! Vision APIs cap how large an image may be in pixels, independently of its
//! byte size. A screenshot of a long page compresses to well under any byte
//! budget while still being tens of thousands of pixels tall, so byte-size
//! checks alone let invalid images through.
//!
//! Anthropic rejects an oversized `tool_result` image with a validation error
//! instead of downscaling it server-side, which fails the whole turn. Clamping
//! here, at every point where bytes become base64 bound for a provider, is what
//! keeps that turn valid.

use std::io::Cursor;

/// Maximum pixel length of either edge of an image sent to a provider.
///
/// Anthropic's hard cap is 8000px, but a stricter per-image cap applies to
/// every image in a request once it carries more than 20 image blocks, and
/// 2000px is the value documented as safe on all platforms. Staying there also
/// bounds visual-token cost, since both the standard (1568px long edge) and
/// high-resolution (2576px) tiers downscale anything larger anyway.
pub const MAX_PROVIDER_IMAGE_EDGE: u32 = 2000;

/// Maximum encoded byte size for an image sent to a provider.
///
/// Matches the cap `read.rs` applies to the original file, so re-encoding can
/// never smuggle a payload past a check the caller already made.
pub const MAX_PROVIDER_IMAGE_BYTES: usize = 3_932_160; // 3.75 MiB

/// Quality ladder used when re-encoding to JPEG, tried highest first.
///
/// 90 keeps screenshot text legible and is what any real screenshot or photo
/// settles on. The lower rungs only engage for near-incompressible content,
/// where a single fixed quality would blow the byte budget: pure 2000x2000
/// noise encodes to ~5.7 MB at quality 90.
const JPEG_QUALITY_LADDER: [u8; 3] = [90, 75, 60];

/// Downscales `data` so neither edge exceeds [`MAX_PROVIDER_IMAGE_EDGE`] and
/// the encoded result is at most [`MAX_PROVIDER_IMAGE_BYTES`], preserving
/// aspect ratio.
///
/// Returns the re-encoded bytes and their MIME type when the image was
/// oversized, and `None` when it already fits or cannot be decoded. Leaving an
/// undecodable image untouched is deliberate: the provider's own error names the
/// real problem, where a local failure here would hide it.
///
/// Opaque images re-encode to JPEG rather than PNG. PNG balloons on
/// photographic content — a downscaled 2000x1333 noise frame measures ~6.4 MB
/// as PNG against ~1.6 MB as JPEG — which would push the result past the byte
/// budget even though the caller checked the original file against it. Alpha
/// still takes the lossless path, falling back to a flattened JPEG if the PNG
/// exceeds the budget anyway.
///
/// Both bounds are hard. Quality is lowered first, then the image is shrunk
/// below the edge limit, because dropping pixels is what actually bounds
/// near-incompressible content: pure 2000x2000 noise is still ~5.7 MB at
/// quality 60.
#[must_use]
pub fn downscale_for_provider(data: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    let (width, height) = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;

    if width <= MAX_PROVIDER_IMAGE_EDGE && height <= MAX_PROVIDER_IMAGE_EDGE {
        return None;
    }

    let decoded = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;

    encode_within(&decoded, MAX_PROVIDER_IMAGE_EDGE, MAX_PROVIDER_IMAGE_BYTES)
}

/// Encodes `decoded` within both an edge limit and a byte budget.
///
/// Halves the edge limit until the encoding fits. Terminates because the edge
/// floor is 1px, whose encoding is a few hundred bytes — far below any budget a
/// caller would set — so the loop always returns from inside.
fn encode_within(
    decoded: &image::DynamicImage,
    max_edge: u32,
    max_bytes: usize,
) -> Option<(Vec<u8>, &'static str)> {
    let mut edge = max_edge;

    loop {
        let resized = decoded.resize(edge, edge, image::imageops::FilterType::Triangle);

        if resized.color().has_alpha() {
            let mut png = Vec::new();
            resized
                .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                .ok()?;
            if png.len() <= max_bytes {
                return Some((png, "image/png"));
            }
        }

        let rgb = resized.to_rgb8();
        for quality in JPEG_QUALITY_LADDER {
            let jpeg = encode_jpeg(&rgb, quality)?;
            if jpeg.len() <= max_bytes {
                return Some((jpeg, "image/jpeg"));
            }
        }

        if edge == 1 {
            return None;
        }
        edge = (edge / 2).max(1);
    }
}

fn encode_jpeg(img: &image::RgbImage, quality: u8) -> Option<Vec<u8>> {
    let mut jpeg = Vec::new();
    let mut cursor = Cursor::new(&mut jpeg);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality)
        .encode_image(img)
        .ok()?;
    Some(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(width, height));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn dimensions(data: &[u8]) -> (u32, u32) {
        image::ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap()
    }

    #[test]
    fn leaves_images_within_the_limit_untouched() {
        assert!(downscale_for_provider(&png_bytes(800, 600)).is_none());
        assert!(
            downscale_for_provider(&png_bytes(MAX_PROVIDER_IMAGE_EDGE, MAX_PROVIDER_IMAGE_EDGE))
                .is_none()
        );
    }

    #[test]
    fn clamps_a_tall_screenshot_and_preserves_aspect_ratio() {
        let (resized, mime) = downscale_for_provider(&png_bytes(600, 6000))
            .expect("oversized image should be downscaled");

        assert_eq!(mime, "image/jpeg");
        let (width, height) = dimensions(&resized);
        assert_eq!(height, MAX_PROVIDER_IMAGE_EDGE);
        assert_eq!(width, 200);
    }

    /// A downscaled photo must not re-encode into something larger than the
    /// byte cap the caller already checked the original file against.
    ///
    /// The source only slightly exceeds the edge limit on purpose: a mild
    /// downscale preserves more high-frequency noise than a steep one, so this
    /// is the harder case for the encoder as well as the cheaper one to build.
    #[test]
    fn keeps_incompressible_content_under_the_byte_budget() {
        let mut img = image::RgbImage::new(2100, 2100);
        let mut seed: u32 = 12_345;
        for pixel in img.pixels_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *pixel = image::Rgb([(seed >> 16) as u8, (seed >> 8) as u8, seed as u8]);
        }
        let mut source = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut source), image::ImageFormat::Png)
            .unwrap();

        let (resized, mime) = downscale_for_provider(&source).expect("should be downscaled");

        assert_eq!(mime, "image/jpeg");
        assert!(
            resized.len() <= MAX_PROVIDER_IMAGE_BYTES,
            "re-encoded image is {} bytes, over the {MAX_PROVIDER_IMAGE_BYTES} budget",
            resized.len()
        );
    }

    #[test]
    fn keeps_alpha_lossless_when_it_fits_the_budget() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(3000, 500));
        let mut source = Vec::new();
        img.write_to(&mut Cursor::new(&mut source), image::ImageFormat::Png)
            .unwrap();

        let (resized, mime) = downscale_for_provider(&source).expect("should be downscaled");

        assert_eq!(mime, "image/png");
        assert!(resized.len() <= MAX_PROVIDER_IMAGE_BYTES);
        assert_eq!(dimensions(&resized).0, MAX_PROVIDER_IMAGE_EDGE);
    }

    /// The byte budget is a hard bound, not best effort: when no quality rung
    /// fits at the edge limit, the image is shrunk below it until one does.
    ///
    /// Driven through `encode_within` with a deliberately tiny budget so the
    /// shrink path is exercised in milliseconds rather than by building an
    /// image large enough to defeat quality 60 at 2000px.
    #[test]
    fn shrinks_below_the_edge_limit_when_quality_alone_cannot_fit() {
        let mut img = image::RgbImage::new(600, 600);
        let mut seed: u32 = 99;
        for pixel in img.pixels_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *pixel = image::Rgb([(seed >> 16) as u8, (seed >> 8) as u8, seed as u8]);
        }
        let decoded = image::DynamicImage::ImageRgb8(img);

        let budget = 5_000;
        let (encoded, mime) =
            encode_within(&decoded, 600, budget).expect("must satisfy the budget by shrinking");

        assert_eq!(mime, "image/jpeg");
        assert!(
            encoded.len() <= budget,
            "encoded {} bytes exceeds the {budget} byte budget",
            encoded.len()
        );
        let (width, height) = dimensions(&encoded);
        assert!(
            width < 600 && height < 600,
            "expected a shrink below the edge limit, got {width}x{height}"
        );
    }

    /// A normal oversized image must not be shrunk past the edge limit just
    /// because the budget exists.
    #[test]
    fn does_not_shrink_below_the_edge_limit_unnecessarily() {
        let (resized, _) =
            downscale_for_provider(&png_bytes(2400, 2400)).expect("should be downscaled");

        assert_eq!(dimensions(&resized).0, MAX_PROVIDER_IMAGE_EDGE);
    }

    #[test]
    fn clamps_the_long_edge_of_a_wide_image() {
        let (resized, _) =
            downscale_for_provider(&png_bytes(3000, 1000)).expect("should be downscaled");

        let (width, height) = dimensions(&resized);
        assert_eq!(width, MAX_PROVIDER_IMAGE_EDGE);
        assert!(height <= MAX_PROVIDER_IMAGE_EDGE);
    }

    #[test]
    fn passes_undecodable_bytes_through() {
        assert!(downscale_for_provider(b"not an image at all").is_none());
    }
}
