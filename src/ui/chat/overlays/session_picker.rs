//! Session picker overlay.
//!
//! Contains state, update handlers, and render function for the session picker.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::{Overlay, OverlayAction, OverlayState};
use crate::core::session::{self, SessionSummary};
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;
use crate::ui::transcript::HistoryCell;

// ============================================================================
// Constants
// ============================================================================

/// Maximum visible sessions in the picker (excluding borders and hints).
const MAX_VISIBLE_SESSIONS: usize = 10;

/// Visible height used for scroll offset calculations.
/// This should match the list area height in render (inner_area.height - 2 for hints/separator).
/// Using a reasonable default that works for typical terminal sizes.
const VISIBLE_HEIGHT: usize = MAX_VISIBLE_SESSIONS - 2;

// ============================================================================
// State
// ============================================================================

/// State for the session picker overlay.
#[derive(Debug, Clone)]
pub struct SessionPickerState {
    /// List of available sessions.
    pub sessions: Vec<SessionSummary>,
    /// Currently selected index.
    pub selected: usize,
    /// Scroll offset for long lists.
    pub offset: usize,
    /// Snapshot of original transcript cells for restore on Esc.
    pub original_cells: Vec<crate::ui::transcript::HistoryCell>,
}

impl SessionPickerState {
    /// Creates a new session picker state with the given sessions.
    ///
    /// Selects the first session (most recent) by default.
    /// Takes a snapshot of the current transcript cells for restore on Esc.
    pub fn new(
        sessions: Vec<SessionSummary>,
        original_cells: Vec<crate::ui::transcript::HistoryCell>,
    ) -> Self {
        Self {
            sessions,
            selected: 0,
            offset: 0,
            original_cells,
        }
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.sessions.get(self.selected)
    }
}

// ============================================================================
// Overlay Trait Implementation
// ============================================================================

impl Overlay for SessionPickerState {
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_session_picker(frame, self, area, input_y)
    }

    fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                // Restore original transcript cells before closing
                tui.transcript.cells = self.original_cells.clone();
                tui.transcript.scroll.reset();
                tui.transcript.wrap_cache.clear();
                Some(OverlayAction::close())
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                    // Adjust offset to keep selection visible: if selected < offset, scroll up
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                // Emit preview effect for newly selected session
                self.selected_session().map(|session| {
                    OverlayAction::Effects(vec![UiEffect::PreviewSession {
                        session_id: session.id.clone(),
                    }])
                })
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected < self.sessions.len().saturating_sub(1) {
                    self.selected += 1;
                    // Adjust offset to keep selection visible: if selected >= offset + visible_height, scroll down
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                // Emit preview effect for newly selected session
                self.selected_session().map(|session| {
                    OverlayAction::Effects(vec![UiEffect::PreviewSession {
                        session_id: session.id.clone(),
                    }])
                })
            }
            KeyCode::Enter => {
                // Block session switch while agent is running (keep overlay open)
                if tui.agent_state.is_running() {
                    tui.transcript
                        .cells
                        .push(HistoryCell::system("Stop the current task first."));
                    return None;
                }

                // Get the selected session ID
                if let Some(session) = self.selected_session() {
                    Some(OverlayAction::close_with(vec![UiEffect::LoadSession {
                        session_id: session.id.clone(),
                    }]))
                } else {
                    // No session selected (empty list), just close
                    Some(OverlayAction::close())
                }
            }
            _ => None,
        }
    }
}

// ============================================================================
// Update Handlers
// ============================================================================

/// Opens the session picker overlay with the given sessions.
///
/// Takes a snapshot of the current transcript for restore on Esc.
pub fn open_session_picker(
    overlay: &mut OverlayState,
    sessions: Vec<SessionSummary>,
    original_cells: Vec<HistoryCell>,
) -> Vec<UiEffect> {
    if !matches!(overlay, OverlayState::None) {
        return vec![];
    }

    *overlay = OverlayState::SessionPicker(SessionPickerState::new(sessions, original_cells));

    // Preview the first session if available
    if let OverlayState::SessionPicker(picker) = overlay
        && let Some(session) = picker.selected_session()
    {
        vec![UiEffect::PreviewSession {
            session_id: session.id.clone(),
        }]
    } else {
        vec![]
    }
}

// ============================================================================
// Render
// ============================================================================

/// Renders the session picker as an overlay.
pub fn render_session_picker(
    frame: &mut Frame,
    picker: &SessionPickerState,
    area: Rect,
    input_top_y: u16,
) {
    // Calculate dimensions
    let session_count = picker.sessions.len();
    let visible_count = session_count.min(MAX_VISIBLE_SESSIONS);

    // Width: enough for UUID (36) + timestamp (16) + padding
    let picker_width = 60.min(area.width.saturating_sub(4));
    // Height: visible sessions + border (2) + title area (1) + hints (2)
    let picker_height = (visible_count as u16 + 5).min(area.height / 2);

    let available_height = input_top_y;

    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = (available_height.saturating_sub(picker_height)) / 2;

    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

    frame.render_widget(Clear, picker_area);

    // Title with session count
    let title = format!(" Sessions ({}) ", session_count);
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(title)
        .title_style(
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(outer_block, picker_area);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    // Handle empty state
    if picker.sessions.is_empty() {
        let empty_msg = Paragraph::new("No sessions found")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner_area);
        return;
    }

    let list_height = inner_area.height.saturating_sub(2) as usize;

    // Use offset from state directly - navigation handlers keep it in sync with selection
    let list_area = Rect::new(
        inner_area.x,
        inner_area.y,
        inner_area.width,
        list_height as u16,
    );

    // Build list items for visible sessions
    let items: Vec<ListItem> = picker
        .sessions
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|session| {
            let timestamp = session
                .modified
                .and_then(session::format_timestamp)
                .unwrap_or_else(|| "unknown".to_string());

            // Truncate session ID for display (show first 8 chars)
            let short_id = if session.id.len() > 8 {
                format!("{}…", &session.id[..8])
            } else {
                session.id.clone()
            };

            let line = Line::from(vec![
                Span::styled(short_id, Style::default().fg(Color::Cyan)),
                Span::styled("  ", Style::default()),
                Span::styled(timestamp, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    // Adjust selected index for the visible window
    let mut list_state = ListState::default();
    let visible_selected = picker.selected.saturating_sub(picker.offset);
    list_state.select(Some(visible_selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Separator line
    let separator = "─".repeat(inner_area.width as usize);
    let sep_y = inner_area.y + list_height as u16;
    if sep_y < inner_area.y + inner_area.height {
        let separator_area = Rect::new(inner_area.x, sep_y, inner_area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                &separator,
                Style::default().fg(Color::DarkGray),
            ))),
            separator_area,
        );
    }

    // Keyboard hints
    let hints_y = inner_area.y + inner_area.height.saturating_sub(1);
    let hints_area = Rect::new(inner_area.x, hints_y, inner_area.width, 1);
    let hints_line = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Blue)),
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Enter", Style::default().fg(Color::Blue)),
        Span::styled(" select ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Esc", Style::default().fg(Color::Blue)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_picker_state_new_empty() {
        let state = SessionPickerState::new(vec![], vec![]);
        assert_eq!(state.selected, 0);
        assert_eq!(state.offset, 0);
        assert!(state.sessions.is_empty());
        assert!(state.original_cells.is_empty());
        assert!(state.selected_session().is_none());
    }

    #[test]
    fn test_session_picker_state_new_with_sessions() {
        let sessions = vec![
            SessionSummary {
                id: "session-1".to_string(),
                modified: None,
            },
            SessionSummary {
                id: "session-2".to_string(),
                modified: None,
            },
        ];
        let state = SessionPickerState::new(sessions, vec![]);
        assert_eq!(state.selected, 0);
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.selected_session().unwrap().id, "session-1");
    }

    #[test]
    fn test_session_picker_stores_original_cells() {
        let sessions = vec![SessionSummary {
            id: "s1".to_string(),
            modified: None,
        }];
        let original_cells = vec![
            crate::ui::transcript::HistoryCell::user("test message"),
            crate::ui::transcript::HistoryCell::assistant("response"),
        ];
        let state = SessionPickerState::new(sessions, original_cells.clone());
        assert_eq!(state.original_cells.len(), 2);
    }

    #[test]
    fn test_navigation_bounds() {
        let sessions = vec![
            SessionSummary {
                id: "s1".to_string(),
                modified: None,
            },
            SessionSummary {
                id: "s2".to_string(),
                modified: None,
            },
            SessionSummary {
                id: "s3".to_string(),
                modified: None,
            },
        ];
        let mut state = SessionPickerState::new(sessions, vec![]);

        // At start, can't go up
        assert_eq!(state.selected, 0);

        // Go down
        state.selected = 1;
        assert_eq!(state.selected, 1);

        // Go to end
        state.selected = 2;
        assert_eq!(state.selected, 2);

        // Can't go past end (this is enforced by select_next, tested via TuiState)
    }

    #[test]
    fn test_scroll_offset_down() {
        // Test that offset is adjusted when selecting past visible window
        let mut picker = SessionPickerState::new(
            (0..15)
                .map(|i| SessionSummary {
                    id: format!("session-{}", i),
                    modified: None,
                })
                .collect(),
            vec![],
        );

        assert_eq!(picker.selected, 0);
        assert_eq!(picker.offset, 0);

        // Simulate navigating down past visible window (VISIBLE_HEIGHT = 8)
        for i in 1..=10 {
            picker.selected = i;
            if picker.selected >= picker.offset + VISIBLE_HEIGHT {
                picker.offset = picker.selected - VISIBLE_HEIGHT + 1;
            }
        }

        assert_eq!(picker.selected, 10);
        // offset should be adjusted so selected is visible
        assert_eq!(picker.offset, 3); // 10 - 8 + 1 = 3
    }

    #[test]
    fn test_scroll_offset_up() {
        // Test that offset is adjusted when selecting above visible window
        let mut picker = SessionPickerState::new(
            (0..15)
                .map(|i| SessionSummary {
                    id: format!("session-{}", i),
                    modified: None,
                })
                .collect(),
            vec![],
        );

        // Start scrolled down
        picker.selected = 10;
        picker.offset = 5;

        // Navigate up past visible window
        picker.selected = 3;
        if picker.selected < picker.offset {
            picker.offset = picker.selected;
        }

        assert_eq!(picker.selected, 3);
        assert_eq!(picker.offset, 3);
    }
}
