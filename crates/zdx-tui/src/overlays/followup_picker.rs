use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, ListState};

use super::OverlayUpdate;
use crate::mutations::StateMutation;
use crate::state::TuiState;

/// Picker for the most recent reply's end-of-turn follow-up suggestions.
///
/// Enter sends the selected suggestion as the next user message (the normal
/// submit path); Esc dismisses the picker without sending.
#[derive(Debug, Clone)]
pub struct FollowupPickerState {
    thread_id: Option<String>,
    items: Vec<String>,
    pub selected: usize,
}

impl FollowupPickerState {
    pub(crate) fn open(thread_id: Option<String>, items: Vec<String>) -> Self {
        Self {
            thread_id,
            items,
            selected: 0,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_followup_picker(frame, self, area, input_y);
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
                if self.selected + 1 < self.items.len() {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter => {
                let Some(text) = self.items.get(self.selected).cloned() else {
                    return OverlayUpdate::close();
                };
                // Reuse the normal submission path so the selected suggestion
                // becomes a real user message + agent turn.
                let (effects, mutations) =
                    crate::input::build_send_effects(&text, self.thread_id.clone(), false, vec![]);
                let mut all_mutations = vec![StateMutation::SetLastFollowups(Vec::new())];
                all_mutations.extend(mutations);
                OverlayUpdate::close()
                    .with_ui_effects(effects)
                    .with_mutations(all_mutations)
            }
            _ => OverlayUpdate::stay(),
        }
    }
}

fn render_followup_picker(
    frame: &mut Frame,
    picker: &FollowupPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{InputHint, OverlayConfig, render_overlay, render_separator};

    let picker_height = (picker.items.len() as u16 + 5).max(7);
    let hints = [
        InputHint::new("↑↓", "navigate"),
        InputHint::new("Enter", "send"),
        InputHint::new("Esc", "dismiss"),
    ];
    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: "Follow-up suggestions",
            border_color: Color::Green,
            width: 60,
            height: picker_height,
            hints: &hints,
        },
    );

    let list_height = layout.body.height.saturating_sub(1);
    let list_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, list_height);

    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|item| ListItem::new(Line::from(item.clone())))
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Green)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, layout.body, list_height);
}
