use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};
use zdx_engine::config::ThinkingLevel;

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::mutations::{StateMutation, TranscriptMutation};
use crate::state::TuiState;

/// Message shown when a thinking-only switch is requested for a model that has
/// no reasoning capability.
pub const NO_REASONING_NOTICE: &str = "This model has no thinking levels.";

/// Builds the request that opens the picker on `model`, changing only the
/// thinking level. Shared by `/thinking` and Ctrl+T so both apply the same
/// reasoning-capability guard. Returns `None` when the model cannot reason.
pub fn thinking_request_for_model(model: &str) -> Option<super::OverlayRequest> {
    if !zdx_engine::models::model_supports_reasoning(model) {
        return None;
    }
    Some(super::OverlayRequest::ThinkingPicker {
        model: model.to_string(),
        display_name: super::model_picker::label_for_model_spec(model),
    })
}

/// Switch notice for a committed selection. The level is spelled out because
/// `/thinking` keeps the model, so the model name alone would not show what
/// actually changed.
fn switch_notice(display_name: &str, level: ThinkingLevel) -> String {
    if level == ThinkingLevel::Off {
        format!("Switched to {display_name}")
    } else {
        format!("Switched to {display_name} [{}]", level.display_name())
    }
}

#[derive(Debug, Clone)]
pub struct ThinkingPickerState {
    pub selected: usize,
    model: String,
    display_name: String,
}

impl ThinkingPickerState {
    pub fn open(
        model: String,
        display_name: String,
        current: ThinkingLevel,
    ) -> (Self, Vec<UiEffect>) {
        let selected = ThinkingLevel::all()
            .iter()
            .position(|l| *l == current)
            .unwrap_or(0);
        (
            Self {
                selected,
                model,
                display_name,
            },
            vec![],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_thinking_picker(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
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

                let model = zdx_engine::models::format_model_thinking(&self.model, level);
                let message = switch_notice(&self.display_name, level);
                let mut effects = Vec::new();
                // The selection lives in this tab's session state and, when the
                // thread already exists on disk, in its metadata. It never
                // reaches the workspace config; `/model-save` does that.
                if tui.thread.thread_handle.is_some() {
                    effects.push(UiEffect::PersistThreadModelOverride {
                        model: model.clone(),
                    });
                }
                effects.push(UiEffect::RefreshSystemPrompt {
                    path: tui.agent_opts.root.clone(),
                });

                OverlayUpdate::close()
                    .with_ui_effects(effects)
                    .with_mutations(vec![
                        StateMutation::Thread(crate::mutations::ThreadMutation::SetOverrides {
                            model_override: Some(model.clone()),
                            thinking_override: None,
                        }),
                        StateMutation::SetActiveThreadOverrides {
                            model_override: Some(model),
                            thinking_override: None,
                        },
                        StateMutation::Transcript(TranscriptMutation::AppendOrReplaceSwitchNotice(
                            message,
                        )),
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
            title: "Select Thinking Level",
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
