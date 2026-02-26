//! Image preview overlay.
//!
//! Displays an image in the terminal using the Kitty graphics protocol.
//! Uses cursor positioning + `a=T` (transmit and display) with tmux DCS
//! passthrough wrapping when running inside tmux.
//!
//! Loading is async: the overlay opens immediately with a "Loading…" state,
//! then a background task reads/encodes the image and delivers base64 PNG data.

use std::fmt::Write as _;
use std::io::Write;

use crossterm::event::{KeyCode, KeyEvent};
use crossterm::{QueueableCommand, cursor};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::OverlayUpdate;
use super::render_utils::centered_rect;
use crate::state::TuiState;

/// Kitty image placement ID used for the preview overlay.
const KITTY_IMAGE_ID: u32 = 31;

/// Maximum base64 bytes per Kitty graphics chunk.
const CHUNK_SIZE: usize = 4096;

#[derive(Debug)]
pub struct ImagePreviewState {
    pub image_path: String,
    pub image_index: usize,
    /// Base64-encoded PNG data ready to send via Kitty graphics protocol.
    kitty_data: Option<String>,
    /// Image dimensions in pixels (for aspect ratio calculation).
    image_dims: Option<(u32, u32)>,
    error: Option<String>,
}

impl ImagePreviewState {
    /// Creates the overlay in loading state. Call `set_image_data` or `set_error` when ready.
    pub fn open(image_path: &str, image_index: usize) -> Self {
        Self {
            image_path: image_path.to_string(),
            image_index,
            kitty_data: None,
            image_dims: None,
            error: None,
        }
    }

    /// Sets the base64-encoded PNG data and image dimensions.
    pub fn set_image_data(&mut self, base64_png: String, width: u32, height: u32) {
        self.kitty_data = Some(base64_png);
        self.image_dims = Some((width, height));
    }

    /// Sets an error (called from the runtime if decode fails).
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    /// Returns the base64 PNG data if loaded.
    pub fn kitty_data(&self) -> Option<&str> {
        self.kitty_data.as_deref()
    }

    /// Returns the image dimensions (width, height) in pixels if loaded.
    pub fn image_dims(&self) -> Option<(u32, u32)> {
        self.image_dims
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => OverlayUpdate::close(),
            _ => OverlayUpdate::stay(),
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16, is_loading: bool) {
        let popup_area = centered_rect(90, 85, area);

        frame.render_widget(Clear, popup_area);

        let filename = std::path::Path::new(&self.image_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.image_path);

        let mut title = format!(" Image #{} — {} ", self.image_index, filename);
        if let Some((w, h)) = self.image_dims {
            let _ = write!(&mut title, "({w}x{h}) ");
        }
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title_bottom(" Esc/q to close ");

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if is_loading {
            let loading =
                Paragraph::new(Line::from("Loading…")).style(Style::default().fg(Color::DarkGray));
            frame.render_widget(loading, inner);
        } else if let Some(error) = &self.error {
            let error_text = Paragraph::new(error.as_str()).style(Style::default().fg(Color::Red));
            frame.render_widget(error_text, inner);
        } else if self.kitty_data.is_some() {
            // Clear the inner area — the Kitty image is rendered post-draw
            // by the runtime via `send_kitty_image`.
            frame.render_widget(Clear, inner);
        }
    }
}

// ============================================================================
// Kitty Graphics Protocol — Direct Placement with tmux DCS Passthrough
// ============================================================================

/// Whether we are running inside tmux (cached on first check).
fn in_tmux() -> bool {
    use std::sync::OnceLock;
    static IN_TMUX: OnceLock<bool> = OnceLock::new();
    *IN_TMUX.get_or_init(|| std::env::var_os("TMUX").is_some())
}

/// Sends a Kitty graphics protocol image to the terminal, centered and
/// aspect-ratio-correct within the given cell area.
///
/// `image_dims` is `(width, height)` in pixels.
/// `cell_size` is `(cell_width, cell_height)` in pixels.
///
/// # Errors
/// Returns an error if writing to stdout fails.
pub fn send_kitty_image(
    base64_png: &str,
    area: Rect,
    image_dims: (u32, u32),
    cell_size: (u16, u16),
) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    let tmux = in_tmux();

    let (img_w, img_h) = image_dims;
    let (cell_w, cell_h) = cell_size;

    // Calculate aspect-ratio-correct display size in cells
    let (cols, rows, x_off, y_off) = if img_w == 0 || img_h == 0 || cell_w == 0 || cell_h == 0 {
        (area.width, area.height, 0u16, 0u16)
    } else {
        // Available pixel dimensions
        let area_px_w = f64::from(area.width) * f64::from(cell_w);
        let area_px_h = f64::from(area.height) * f64::from(cell_h);
        let img_aspect = f64::from(img_w) / f64::from(img_h);
        let area_aspect = area_px_w / area_px_h;

        let (fit_cols, fit_rows) = if img_aspect > area_aspect {
            // Image is wider than area — width-constrained
            let c = area.width;
            let r = (f64::from(c) * f64::from(cell_w) / (img_aspect * f64::from(cell_h))).round()
                as u16;
            (c, r.max(1))
        } else {
            // Image is taller than area — height-constrained
            let r = area.height;
            let c =
                (f64::from(r) * f64::from(cell_h) * img_aspect / f64::from(cell_w)).round() as u16;
            (c.max(1), r)
        };

        let x_off = (area.width.saturating_sub(fit_cols)) / 2;
        let y_off = (area.height.saturating_sub(fit_rows)) / 2;
        (fit_cols, fit_rows, x_off, y_off)
    };

    // Cursor positioning — handled by tmux natively (translated to pane coords).
    // Always try to restore cursor even if sending the image fails.
    stdout.queue(cursor::SavePosition)?;
    let send_result = (|| -> std::io::Result<()> {
        stdout.queue(cursor::MoveTo(area.x + x_off, area.y + y_off))?;
        stdout.flush()?;

        let data = base64_png.as_bytes();

        if data.len() <= CHUNK_SIZE {
            // Single chunk
            let mut seq = Vec::with_capacity(data.len() + 80);
            write!(
                seq,
                "\x1b_Ga=T,f=100,q=2,i={KITTY_IMAGE_ID},c={cols},r={rows},z=1,m=0;"
            )?;
            seq.extend_from_slice(data);
            seq.extend_from_slice(b"\x1b\\");
            write_passthrough(&mut stdout, &seq, tmux)?;
        } else {
            // Multi-chunk transfer
            let mut offset = 0;
            let mut first = true;
            while offset < data.len() {
                let end = (offset + CHUNK_SIZE).min(data.len());
                let chunk = &data[offset..end];
                let more = u8::from(end < data.len());

                let mut seq = Vec::with_capacity(chunk.len() + 80);
                if first {
                    write!(
                        seq,
                        "\x1b_Ga=T,f=100,q=2,i={KITTY_IMAGE_ID},c={cols},r={rows},z=1,m={more};"
                    )?;
                    first = false;
                } else {
                    write!(seq, "\x1b_Gm={more};")?;
                }
                seq.extend_from_slice(chunk);
                seq.extend_from_slice(b"\x1b\\");
                write_passthrough(&mut stdout, &seq, tmux)?;

                offset = end;
            }
        }

        Ok(())
    })();

    let restore_result = stdout
        .queue(cursor::RestorePosition)
        .and_then(std::io::Write::flush);

    match (send_result, restore_result) {
        (Err(send_err), _) => Err(send_err),
        (Ok(()), Err(restore_err)) => Err(restore_err),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Deletes the Kitty graphics image with ID [`KITTY_IMAGE_ID`].
///
/// # Errors
/// Returns an error if writing to stdout fails.
pub fn delete_kitty_image() -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    let tmux = in_tmux();
    let mut seq = Vec::with_capacity(32);
    write!(seq, "\x1b_Ga=d,d=I,i={KITTY_IMAGE_ID},q=2\x1b\\")?;
    write_passthrough(&mut stdout, &seq, tmux)?;
    stdout.flush()?;
    Ok(())
}

/// Writes a Kitty APC sequence, wrapping in tmux DCS passthrough if needed.
///
/// When running inside tmux, the APC is wrapped in:
///   `ESC P tmux ; <content-with-doubled-ESCs> ESC \`
/// so that tmux forwards it to the underlying terminal (Ghostty/Kitty).
fn write_passthrough(stdout: &mut impl Write, payload: &[u8], tmux: bool) -> std::io::Result<()> {
    if tmux {
        // Build a single wrapped DCS payload to avoid many tiny writes.
        let mut wrapped = Vec::with_capacity(payload.len() + 64);
        wrapped.extend_from_slice(b"\x1bPtmux;");
        for &byte in payload {
            if byte == 0x1b {
                // Double every ESC inside the DCS passthrough content
                wrapped.push(0x1b);
            }
            wrapped.push(byte);
        }
        wrapped.extend_from_slice(b"\x1b\\");
        stdout.write_all(&wrapped)?;
    } else {
        stdout.write_all(payload)?;
    }
    Ok(())
}

/// Returns the inner content area of the image preview overlay for a given terminal area.
pub fn overlay_inner_area(terminal_area: Rect) -> Rect {
    let popup = centered_rect(90, 85, terminal_area);
    Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    }
}
