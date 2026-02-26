//! Image preview overlay.
//!
//! Displays an image in the terminal using ratatui-image protocols
//! (Kitty, Sixel, halfblock fallback).
//!
//! Loading is async: the overlay opens immediately with a "Loading…" state,
//! then a background task decodes the image and delivers the protocol.

use std::cell::RefCell;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui_image::StatefulImage;
use ratatui_image::protocol::StatefulProtocol;

use super::OverlayUpdate;
use crate::state::TuiState;

pub struct ImagePreviewState {
    pub image_path: String,
    pub image_index: usize,
    image_protocol: Option<RefCell<StatefulProtocol>>,
    error: Option<String>,
    loading: bool,
}

impl fmt::Debug for ImagePreviewState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImagePreviewState")
            .field("image_path", &self.image_path)
            .field("image_index", &self.image_index)
            .field("has_protocol", &self.image_protocol.is_some())
            .field("error", &self.error)
            .field("loading", &self.loading)
            .finish()
    }
}

impl ImagePreviewState {
    /// Creates the overlay in loading state. Call `set_protocol` or `set_error` when ready.
    pub fn open(image_path: &str, image_index: usize) -> Self {
        Self {
            image_path: image_path.to_string(),
            image_index,
            image_protocol: None,
            error: None,
            loading: true,
        }
    }

    /// Sets the loaded protocol (called from the runtime after background decode).
    pub fn set_protocol(&mut self, protocol: StatefulProtocol) {
        self.image_protocol = Some(RefCell::new(protocol));
        self.loading = false;
    }

    /// Sets an error (called from the runtime if decode fails).
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.loading = false;
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
        } else if let Some(protocol) = &self.image_protocol {
            let image_widget = StatefulImage::default();
            frame.render_stateful_widget(image_widget, inner, &mut *protocol.borrow_mut());
            // Check for encoding errors (recommended by ratatui-image)
            if let Some(Err(e)) = protocol.borrow_mut().last_encoding_result() {
                let err = Paragraph::new(format!("Encoding error: {e}"))
                    .style(Style::default().fg(Color::Yellow));
                frame.render_widget(err, inner);
            }
        }
    }
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
/// Used to pre-encode the image at the expected render size before delivering it to state.
pub fn overlay_inner_area(terminal_area: Rect) -> Rect {
    let popup = centered_rect(90, 85, terminal_area);
    // Shrink by the Block borders (1 cell on each side)
    Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    }
}
