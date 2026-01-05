use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::OverlayAction;
use crate::config::ThinkingLevel;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{ConfigCommand, StateCommand, TranscriptCommand};
use crate::modes::tui::app::TuiState;

#[derive(Debug, Clone)]
pub struct ThinkingPickerState {
    pub selected: usize,
}

impl ThinkingPickerState {
    pub fn open(current: ThinkingLevel) -> (Self, Vec<UiEffect>) {
        let selected = ThinkingLevel::all()
            .iter()
            .position(|l| *l == current)
            .unwrap_or(0);
        (Self { selected }, vec![])
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_thinking_picker(frame, self, area, input_y)
    }

    pub fn handle_key(
        &mut self,
        _tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<OverlayAction>, Vec<StateCommand>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        let (action, commands) = match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                (Some(OverlayAction::close()), vec![])
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                (None, vec![])
            }
            KeyCode::Down => {
                if self.selected < ThinkingLevel::all().len() - 1 {
                    self.selected += 1;
                }
                (None, vec![])
            }
            KeyCode::Enter => {
                let levels = ThinkingLevel::all();
                let Some(&level) = levels.get(self.selected) else {
                    return (Some(OverlayAction::close()), vec![]);
                };

                let message = if level == ThinkingLevel::Off {
                    "Thinking disabled".to_string()
                } else {
                    format!("Thinking level set to {}", level.display_name())
                };
                (
                    Some(OverlayAction::close_with(vec![UiEffect::PersistThinking {
                        level,
                    }])),
                    vec![
                        StateCommand::Config(ConfigCommand::SetThinkingLevel(level)),
                        StateCommand::Transcript(TranscriptCommand::AppendSystemMessage(message)),
                    ],
                )
            }
            _ => (None, vec![]),
        };

        (action, commands)
    }
}

pub fn render_thinking_picker(
    frame: &mut Frame,
    picker: &ThinkingPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::view::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let levels = ThinkingLevel::all();

    let picker_width = 45;
    let picker_height = (levels.len() as u16 + 5).max(7);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    render_overlay_container(frame, picker_area, "Thinking Level", Color::Magenta);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    let list_height = inner_area.height.saturating_sub(2);
    let list_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, list_height);

    let items: Vec<ListItem> = levels
        .iter()
        .map(|level| {
            let name_width = 10;
            let name = format!("{:<width$}", level.display_name(), width = name_width);

            let line = Line::from(vec![
                Span::styled(
                    name,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(level.description(), Style::default().fg(Color::DarkGray)),
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

    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, list_height);

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "navigate"),
            InputHint::new("Enter", "select"),
            InputHint::new("Esc", "cancel"),
        ],
        Color::Magenta,
    );
}
