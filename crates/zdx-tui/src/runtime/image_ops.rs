//! Shared image loading/transform helpers for runtime features.
//!
//! Used by:
//! - image preview overlay
//! - image attachment ingestion

/// Upper bound for preview image size before Kitty transfer.
const PREVIEW_MAX_LONG_EDGE_PX: u32 = 1024;

/// Max attachment image size for input attachments.
const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

/// Reads an image file for prompt attachment and returns `(mime_type, base64_data)`.
pub(crate) fn read_and_encode_image(path: &str) -> anyhow::Result<(String, String)> {
    use anyhow::Context;
    use base64::Engine;

    let path = zdx_core::images::path_mime::normalize_input_path(path);

    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("Cannot read image: {}", path.display()))?;

    if metadata.len() > MAX_ATTACHMENT_BYTES {
        anyhow::bail!("Image too large (max 20MB)");
    }

    let data = std::fs::read(&path)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    let mime_type =
        zdx_core::images::path_mime::mime_type_for_extension(path.to_str().unwrap_or(""))
            .unwrap_or("image/png")
            .to_string();

    Ok((mime_type, encoded))
}

/// Decodes an image file for Kitty preview (base64 PNG + original dimensions).
pub(crate) fn decode_image_preview(
    image_path: &str,
) -> Result<crate::events::KittyImageData, String> {
    use base64::Engine;

    let max_dims = preview_target_max_dims();
    let decoded =
        zdx_core::images::decode::decode_image_to_png(std::path::Path::new(image_path), max_dims)?;

    Ok(crate::events::KittyImageData {
        base64_png: base64::engine::general_purpose::STANDARD.encode(decoded.png_bytes),
        width: decoded.source_width,
        height: decoded.source_height,
    })
}

fn preview_target_max_dims() -> (u32, u32) {
    let fallback = (PREVIEW_MAX_LONG_EDGE_PX, PREVIEW_MAX_LONG_EDGE_PX);
    let Ok(ws) = crossterm::terminal::window_size() else {
        return fallback;
    };

    if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
        return fallback;
    }

    let cell_w = u32::from((ws.width / ws.columns).max(1));
    let cell_h = u32::from((ws.height / ws.rows).max(1));

    // Reuse overlay geometry source-of-truth to avoid drift.
    let terminal_cells = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: ws.columns,
        height: ws.rows,
    };
    let inner_cells = crate::overlays::image_preview::overlay_inner_area(terminal_cells);

    let inner_w = u32::from(inner_cells.width).saturating_mul(cell_w).max(1);
    let inner_h = u32::from(inner_cells.height).saturating_mul(cell_h).max(1);

    (
        inner_w.min(PREVIEW_MAX_LONG_EDGE_PX),
        inner_h.min(PREVIEW_MAX_LONG_EDGE_PX),
    )
}
