//! Image preview overlay.
//!
//! Displays an image in the terminal using the Kitty graphics protocol.
//! Uses cursor positioning + `a=T` (transmit and display) with tmux DCS
//! passthrough wrapping when running inside tmux.
//!
//! Loading is async: the overlay opens immediately with a "Loading…" state,
//! then a background task reads/encodes the image and delivers base64 PNG data.

use std::io::Write;

use crossterm::QueueableCommand;
use crossterm::cursor;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::OverlayUpdate;
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
    error: Option<String>,
    loading: bool,
}

impl ImagePreviewState {
    /// Creates the overlay in loading state. Call `set_image_data` or `set_error` when ready.
    pub fn open(image_path: &str, image_index: usize) -> Self {
        Self {
            image_path: image_path.to_string(),
            image_index,
            kitty_data: None,
            error: None,
            loading: true,
        }
    }

    /// Sets the base64-encoded PNG data (called from the runtime after background encode).
    pub fn set_image_data(&mut self, base64_png: String) {
        self.kitty_data = Some(base64_png);
        self.loading = false;
    }

    /// Sets an error (called from the runtime if decode fails).
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.loading = false;
    }

    /// Returns the base64 PNG data if loaded.
    pub fn kitty_data(&self) -> Option<&str> {
        self.kitty_data.as_deref()
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => OverlayUpdate::close(),
            _ => OverlayUpdate::stay(),
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16) {
        let popup_area = centered_rect(90, 85, area);

        frame.render_widget(Clear, popup_area);

        let filename = std::path::Path::new(&self.image_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.image_path);

        let title = format!(" Image #{} — {} ", self.image_index, filename);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title_bottom(" Esc/q to close ");

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if self.loading {
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

/// Whether we are running inside tmux (cached on first call).
fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Sends a Kitty graphics protocol image to the terminal at the given cell area.
///
/// Cursor positioning goes to tmux natively (pane-relative coordinates).
/// The Kitty APC sequences are wrapped in tmux DCS passthrough when needed.
///
/// # Errors
/// Returns an error if writing to stdout fails.
pub fn send_kitty_image(base64_png: &str, area: Rect) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    let tmux = in_tmux();

    // Cursor positioning — handled by tmux natively (translated to pane coords)
    stdout.queue(cursor::SavePosition)?;
    stdout.queue(cursor::MoveTo(area.x, area.y))?;
    stdout.flush()?;

    let data = base64_png.as_bytes();
    let cols = area.width;
    let rows = area.height;

    if data.len() <= CHUNK_SIZE {
        // Single chunk
        let mut seq = Vec::with_capacity(data.len() + 80);
        write!(
            seq,
            "\x1b_Ga=T,f=100,q=2,i={KITTY_IMAGE_ID},c={cols},r={rows},m=0;"
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
                    "\x1b_Ga=T,f=100,q=2,i={KITTY_IMAGE_ID},c={cols},r={rows},m={more};"
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

    // Restore cursor
    stdout.queue(cursor::RestorePosition)?;
    stdout.flush()?;

    Ok(())
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
fn write_passthrough(
    stdout: &mut impl Write,
    payload: &[u8],
    tmux: bool,
) -> std::io::Result<()> {
    if tmux {
        stdout.write_all(b"\x1bPtmux;")?;
        for &byte in payload {
            if byte == 0x1b {
                // Double every ESC inside the DCS passthrough content
                stdout.write_all(&[0x1b])?;
            }
            stdout.write_all(&[byte])?;
        }
        stdout.write_all(b"\x1b\\")?;
    } else {
        stdout.write_all(payload)?;
    }
    Ok(())
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
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
