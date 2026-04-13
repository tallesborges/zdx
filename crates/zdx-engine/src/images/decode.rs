//! Generic decode/resize/encode helpers for image workflows.

/// Decoded image payload encoded as PNG bytes, with original dimensions.
#[derive(Debug, Clone)]
pub struct DecodedImagePng {
    pub png_bytes: Vec<u8>,
    pub source_width: u32,
    pub source_height: u32,
}

/// Decodes an image file and returns PNG bytes, optionally downscaled to `max_dims`.
///
/// If the source file is already PNG and does not exceed `max_dims`, bytes are
/// returned as-is (fast path).
///
/// # Errors
/// Returns an error string if file I/O, format detection/decoding, resizing,
/// or PNG encoding fails.
pub fn decode_image_to_png(
    image_path: &std::path::Path,
    max_dims: (u32, u32),
) -> Result<DecodedImagePng, String> {
    let path_display = image_path.display();
    let data = std::fs::read(image_path).map_err(|e| format!("{path_display}: {e}"))?;
    let is_png = data.len() >= 8 && data[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    let (width, height) =
        image::image_dimensions(image_path).map_err(|e| format!("dimensions: {e}"))?;

    let (max_w, max_h) = (max_dims.0.max(1), max_dims.1.max(1));
    let needs_resize = width > max_w || height > max_h;

    let png_bytes = if is_png && !needs_resize {
        data
    } else {
        let reader = image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| format!("decode: {e}"))?;

        let dyn_img = reader.decode().map_err(|e| format!("decode: {e}"))?;
        let resized = if needs_resize {
            resize_image_fast(&dyn_img, max_w, max_h)?
        } else {
            dyn_img
        };

        encode_png_fast(&resized)?
    };

    Ok(DecodedImagePng {
        png_bytes,
        source_width: width,
        source_height: height,
    })
}

fn resize_image_fast(
    src: &image::DynamicImage,
    dst_w: u32,
    dst_h: u32,
) -> Result<image::DynamicImage, String> {
    use fast_image_resize as fir;

    if src.width() == dst_w && src.height() == dst_h {
        return Ok(src.clone());
    }

    let src_rgba = src.to_rgba8();
    let src_w = src_rgba.width();
    let src_h = src_rgba.height();
    let src_pixels = src_rgba.into_raw();

    let src_image = fir::images::Image::from_vec_u8(src_w, src_h, src_pixels, fir::PixelType::U8x4)
        .map_err(|e| format!("resize: {e}"))?;

    let mut dst_image = fir::images::Image::new(dst_w, dst_h, fir::PixelType::U8x4);
    let mut resizer = fir::Resizer::new();
    let options = fir::ResizeOptions::new().resize_alg(fir::ResizeAlg::Nearest);
    resizer
        .resize(&src_image, &mut dst_image, Some(&options))
        .map_err(|e| format!("resize: {e}"))?;

    let dst_pixels = dst_image.into_vec();
    let rgba = image::RgbaImage::from_raw(dst_w, dst_h, dst_pixels)
        .ok_or_else(|| "resize: invalid output buffer".to_string())?;
    Ok(image::DynamicImage::ImageRgba8(rgba))
}

fn encode_png_fast(img: &image::DynamicImage) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};

    let has_alpha = img.color().has_alpha();
    let mut buf = Vec::new();

    let encoder =
        PngEncoder::new_with_quality(&mut buf, CompressionType::Fast, FilterType::Adaptive);

    if has_alpha {
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        encoder
            .write_image(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("encode: {e}"))?;
    } else {
        let rgb = img.to_rgb8();
        let (w, h) = rgb.dimensions();
        encoder
            .write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .map_err(|e| format!("encode: {e}"))?;
    }

    Ok(buf)
}
