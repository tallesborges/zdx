use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use zdx_core::config::ProvidersConfig;
use zdx_core::models::{ModelOption, available_models};
use zdx_core::providers::{ProviderKind, resolve_provider};

use super::OverlayUpdate;
use crate::modes::tui::app::TuiState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{ConfigMutation, StateMutation, TranscriptMutation};

#[derive(Debug, Clone)]
pub struct ModelPickerState {
    pub selected: usize,
    pub filter: String,
    /// Set of enabled provider IDs (captured at open time from config).
    enabled_providers: HashSet<String>,
}

impl ModelPickerState {
    pub fn open(current_model: &str, providers: &ProvidersConfig) -> (Self, Vec<UiEffect>) {
        // Collect enabled providers
        let enabled_providers = collect_enabled_providers(providers);

        // Filter available models by enabled providers, then find selection
        let enabled_models: Vec<_> = available_models()
            .iter()
            .filter(|m| enabled_providers.contains(m.provider))
            .collect();

        let selected = enabled_models
            .iter()
            .position(|m| m.id == current_model)
            .or_else(|| {
                let target = resolve_provider(current_model);
                enabled_models.iter().position(|m| {
                    let candidate = resolve_provider(m.id);
                    candidate.kind == target.kind && candidate.model == target.model
                })
            })
            .unwrap_or(0);
        (
            Self {
                selected,
                filter: String::new(),
                enabled_providers,
            },
            vec![],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_model_picker(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

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
                let count = self.filtered_models().len();
                if count > 0 && self.selected < count.saturating_sub(1) {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter => {
                let Some(model) = self.filtered_models().get(self.selected).copied() else {
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
            // Ctrl+U (or Command+Backspace on macOS): clear the current line
            KeyCode::Char('u') if ctrl && !shift && !alt => {
                self.filter.clear();
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            KeyCode::Backspace => {
                if alt {
                    clear_word_left(&mut self.filter);
                } else {
                    self.filter.pop();
                }
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    fn filtered_models(&self) -> Vec<&'static ModelOption> {
        available_models()
            .iter()
            .filter(|model| self.enabled_providers.contains(model.provider))
            .filter(|model| self.filter.is_empty() || model_matches_filter(model, &self.filter))
            .collect()
    }

    fn clamp_selection(&mut self) {
        let count = self.filtered_models().len();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
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

    let filtered = picker.filtered_models();
    let max_label_len = filtered
        .iter()
        .map(|model| model_label(model).len() as u16)
        .max()
        .unwrap_or(0);
    let max_width = area.width.saturating_sub(4);
    let base_width = max_label_len.saturating_add(36).max(56);
    let picker_width = if max_width < 56 {
        max_width.max(10)
    } else {
        base_width.min(max_width)
    };
    let picker_height = (filtered.len() as u16 + 7).max(7);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    render_overlay_container(frame, picker_area, "Select Model", Color::Magenta);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    let max_filter_len = inner_area.width.saturating_sub(4) as usize;
    let filter_display = if picker.filter.len() > max_filter_len {
        let truncated = &picker.filter[picker.filter.len() - max_filter_len..];
        format!("…{}", truncated)
    } else {
        picker.filter.clone()
    };
    let filter_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::styled(filter_display, Style::default().fg(Color::Magenta)),
        Span::styled("█", Style::default().fg(Color::Magenta)),
    ]);
    let filter_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
    frame.render_widget(Paragraph::new(filter_line), filter_area);

    render_separator(frame, inner_area, 1);

    let list_height = inner_area.height.saturating_sub(5);
    let list_area = Rect::new(
        inner_area.x,
        inner_area.y + 2,
        inner_area.width,
        list_height,
    );

    let items: Vec<ListItem> = if filtered.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  No matches",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        let line_width = list_area.width.saturating_sub(2);
        filtered
            .iter()
            .map(|model| ListItem::new(model_line(model, line_width)))
            .collect()
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !filtered.is_empty() {
        list_state.select(Some(picker.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, 2 + list_height);
    let selected_model = filtered.get(picker.selected).copied();
    render_capabilities_line(frame, inner_area, 3 + list_height, selected_model);

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

fn model_line(model: &ModelOption, width: u16) -> Line<'static> {
    let label = provider_label(model.provider);
    let name = cleaned_display_name(model, model.provider);
    let context = format_context(model.context_limit);
    let pricing = format_pricing(model.pricing.input, model.pricing.output);
    let right = if context.is_empty() && pricing.is_empty() {
        String::new()
    } else if pricing.is_empty() {
        context
    } else if context.is_empty() {
        pricing
    } else {
        format!("{} · {}", pricing, context)
    };

    let left_width = (label.len() + 3 + name.len()) as u16;
    let right_width = right.len() as u16;
    let spacing = if right_width == 0 || width <= left_width + right_width {
        1
    } else {
        width - left_width - right_width
    } as usize;

    let left_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    spans.push(Span::styled(
        format!("{} · ", label),
        Style::default().fg(Color::DarkGray),
    ));
    spans.push(Span::styled(name, left_style));
    spans.push(Span::raw(" ".repeat(spacing)));
    spans.push(Span::styled(right, Style::default().fg(Color::DarkGray)));

    Line::from(spans)
}

fn format_context(context_limit: u64) -> String {
    if context_limit == 0 {
        return String::new();
    }

    if context_limit >= 1_000_000 {
        let millions = context_limit as f64 / 1_000_000.0;
        if (millions - millions.round()).abs() < 0.05 {
            format!("{:.0}M", millions)
        } else {
            format!("{:.1}M", millions)
        }
    } else {
        format!("{}k", context_limit / 1_000)
    }
}

fn format_pricing(input: f64, output: f64) -> String {
    let input = if input == 0.0 { 0.0 } else { input };
    let output = if output == 0.0 { 0.0 } else { output };
    format!("${}/{}", trim_price(input), trim_price(output))
}

fn render_capabilities_line(
    frame: &mut Frame,
    area: Rect,
    y_offset: u16,
    model: Option<&ModelOption>,
) {
    if y_offset >= area.height {
        return;
    }

    let Some(model) = model else {
        return;
    };

    let line_area = Rect::new(area.x, area.y + y_offset, area.width, 1);
    frame.render_widget(
        Paragraph::new(capability_line(model)).alignment(Alignment::Center),
        line_area,
    );
}

fn capability_line(model: &ModelOption) -> Line<'static> {
    let label_style = Style::default().fg(Color::DarkGray);
    let ok_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let err_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);

    let image_icon = if model.capabilities.input_images {
        Span::styled("✓", ok_style)
    } else {
        Span::styled("✗", err_style)
    };
    let reasoning_icon = if model.capabilities.reasoning {
        Span::styled("✓", ok_style)
    } else {
        Span::styled("✗", err_style)
    };

    Line::from(vec![
        Span::styled("Image ", label_style),
        image_icon,
        Span::styled("  ", label_style),
        Span::styled("Reasoning ", label_style),
        reasoning_icon,
    ])
}

fn trim_price(value: f64) -> String {
    let mut text = format!("{:.3}", value);
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn clear_word_left(input: &mut String) {
    let trimmed_len = input.trim_end().len();
    if trimmed_len == 0 {
        input.clear();
        return;
    }

    input.truncate(trimmed_len);
    let mut chars: Vec<char> = input.chars().collect();
    while let Some(&ch) = chars.last() {
        if ch.is_whitespace() {
            break;
        }
        chars.pop();
    }
    input.clear();
    input.extend(chars);
}

fn model_matches_filter(model: &ModelOption, filter: &str) -> bool {
    let filter = filter.to_lowercase();
    if filter.is_empty() {
        return true;
    }

    let label = model_label(model).to_lowercase();
    let id = model.id.to_lowercase();
    label.contains(&filter) || id.contains(&filter)
}

fn provider_label(provider_id: &str) -> String {
    ProviderKind::all()
        .iter()
        .find(|kind| kind.id() == provider_id)
        .map(|kind| kind.label().to_string())
        .unwrap_or_else(|| provider_id.to_string())
}

/// Collects the set of enabled provider IDs from the config.
fn collect_enabled_providers(providers: &ProvidersConfig) -> HashSet<String> {
    ProviderKind::all()
        .iter()
        .filter(|kind| providers.is_enabled(kind.id()))
        .map(|kind| kind.id().to_string())
        .collect()
}
