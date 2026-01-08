use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::OverlayUpdate;
use crate::models::{ModelOption, available_models};
use crate::modes::tui::app::TuiState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{ConfigMutation, StateMutation, TranscriptMutation};
use crate::providers::{ProviderKind, resolve_provider};

#[derive(Debug, Clone)]
pub struct ModelPickerState {
    pub selected: usize,
}

impl ModelPickerState {
    pub fn open(current_model: &str) -> (Self, Vec<UiEffect>) {
        let selected = available_models()
            .iter()
            .position(|m| m.id == current_model)
            .or_else(|| {
                let target = resolve_provider(current_model);
                available_models().iter().position(|m| {
                    let candidate = resolve_provider(m.id);
                    candidate.kind == target.kind && candidate.model == target.model
                })
            })
            .unwrap_or(0);
        (Self { selected }, vec![])
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_model_picker(frame, self, area, input_y)
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
                if self.selected < available_models().len().saturating_sub(1) {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter => {
                let Some(model) = available_models().get(self.selected) else {
                    return OverlayUpdate::close();
                };

                let model_id = model.id.to_string();
                let display_name = model_label(model);

                OverlayUpdate::close()
                    .with_ui_effects(vec![UiEffect::PersistModel {
                        model: model_id.clone(),
                    }])
                    .with_mutations(vec![
                        StateMutation::Config(ConfigMutation::SetModel(model_id)),
                        StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(
                            format!("Switched to {}", display_name),
                        )),
                    ])
            }
            _ => OverlayUpdate::stay(),
        }
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

    let max_label_len = available_models()
        .iter()
        .map(|model| model_label(model).len() as u16)
        .max()
        .unwrap_or(0);
    let max_width = area.width.saturating_sub(4);
    let base_width = max_label_len.saturating_add(6).max(30);
    let picker_width = if max_width < 30 {
        max_width.max(10)
    } else {
        base_width.min(max_width)
    };
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
        .map(|model| ListItem::new(model_line(model)))
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

fn model_label(model: &ModelOption) -> String {
    let label = provider_label(model.provider);
    let name = cleaned_display_name(model, model.provider);
    format!("{} · {}", label, name)
}

fn cleaned_display_name(model: &ModelOption, provider: &str) -> String {
    let mut name = model.display_name.to_string();
    if provider == "anthropic" {
        name = name.replace(" (latest)", "");
    }

    let prefix = format!("{} · ", provider_label(provider));
    if let Some(stripped) = name.strip_prefix(&prefix) {
        return stripped.to_string();
    }

    name
}

fn model_line(model: &ModelOption) -> Line<'static> {
    let label = provider_label(model.provider);
    let name = cleaned_display_name(model, model.provider);
    Line::from(vec![
        Span::styled(
            format!("{} · ", label),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            name,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn provider_label(provider_id: &str) -> String {
    match provider_id {
        "anthropic" => ProviderKind::Anthropic.label().to_string(),
        "openai" => ProviderKind::OpenAI.label().to_string(),
        "openrouter" => ProviderKind::OpenRouter.label().to_string(),
        "gemini" => ProviderKind::Gemini.label().to_string(),
        "openai-codex" => ProviderKind::OpenAICodex.label().to_string(),
        _ => provider_id.to_string(),
    }
}
