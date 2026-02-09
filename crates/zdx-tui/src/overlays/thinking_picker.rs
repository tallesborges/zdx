use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};
use zdx_core::config::ThinkingLevel;

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::mutations::{ConfigMutation, StateMutation, TranscriptMutation};
use crate::state::TuiState;

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
        render_thinking_picker(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close()
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down => {
                if self.selected < ThinkingLevel::all().len() - 1 {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter => {
                let levels = ThinkingLevel::all();
                let Some(&level) = levels.get(self.selected) else {
                    return OverlayUpdate::close();
                };

                let message = if level == ThinkingLevel::Off {
                    "Thinking disabled".to_string()
                } else {
                    format!("Thinking level set to {}", level.display_name())
                };
                OverlayUpdate::close()
                    .with_ui_effects(vec![UiEffect::PersistThinking { level }])
                    .with_mutations(vec![
                        StateMutation::Config(ConfigMutation::SetThinkingLevel(level)),
                        StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(message)),
                    ])
            }
            _ => OverlayUpdate::stay(),
        }
    }
}

pub fn render_thinking_picker(
    frame: &mut Frame,
    picker: &ThinkingPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{InputHint, OverlayConfig, render_overlay, render_separator};

    let levels = ThinkingLevel::all();

    let picker_width = 45;
    let picker_height = (levels.len() as u16 + 5).max(7);

    let hints = [
        InputHint::new("↑↓", "navigate"),
        InputHint::new("Enter", "select"),
        InputHint::new("Esc", "cancel"),
    ];
    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: "Thinking Level",
            border_color: Color::Magenta,
            width: picker_width,
            height: picker_height,
            hints: &hints,
        },
    );

    let list_height = layout.body.height.saturating_sub(1);
    let list_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, list_height);

    let items: Vec<ListItem> = levels
        .iter()
        .map(|level| {
            let name_width = 10;
            let name = format!("{:<width$}", level.display_name(), width = name_width);
            let desc = level.description();

            // Calculate available width for description (account for borders, highlight symbol, name, right padding)
            // inner_area.width - 2 (highlight "▶ ") - name_width - 1 (right padding)
            let desc_width = layout.body.width.saturating_sub(2 + name_width as u16 + 1) as usize;
            let desc_padded = format!("{desc:>desc_width$}");

            let line = Line::from(vec![
                Span::styled(
                    name,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(desc_padded, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, layout.body, list_height);
}
