use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::OverlayAction;
use crate::models::available_models;
use crate::modes::tui::app::TuiState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{ConfigMutation, StateMutation, TranscriptMutation};

#[derive(Debug, Clone)]
pub struct ModelPickerState {
    pub selected: usize,
}

impl ModelPickerState {
    pub fn open(current_model: &str) -> (Self, Vec<UiEffect>) {
        let selected = available_models()
            .iter()
            .position(|m| m.id == current_model)
            .unwrap_or(0);
        (Self { selected }, vec![])
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_model_picker(frame, self, area, input_y)
    }

    pub fn handle_key(
        &mut self,
        _tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<OverlayAction>, Vec<StateMutation>) {
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
                if self.selected < available_models().len().saturating_sub(1) {
                    self.selected += 1;
                }
                (None, vec![])
            }
            KeyCode::Enter => {
                let Some(model) = available_models().get(self.selected) else {
                    return (Some(OverlayAction::close()), vec![]);
                };

                let model_id = model.id.to_string();
                let display_name = model.display_name;

                (
                    Some(OverlayAction::close_with(vec![UiEffect::PersistModel {
                        model: model_id.clone(),
                    }])),
                    vec![
                        StateMutation::Config(ConfigMutation::SetModel(model_id)),
                        StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(
                            format!("Switched to {}", display_name),
                        )),
                    ],
                )
            }
            _ => (None, vec![]),
        };

        (action, commands)
    }
}

pub fn render_model_picker(
    frame: &mut Frame,
    picker: &ModelPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let picker_width = 30;
    let picker_height = (available_models().len() as u16 + 5).max(7);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    render_overlay_container(frame, picker_area, "Select Model", Color::Magenta);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    let list_height = inner_area.height.saturating_sub(2);
    let list_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, list_height);

    let items: Vec<ListItem> = available_models()
        .iter()
        .map(|model| {
            let line = Line::from(Span::styled(
                model.display_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
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
