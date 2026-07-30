use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use zdx_engine::config::ProvidersConfig;
use zdx_engine::models::{
    ModelOption, available_models, bare_model_id, custom_provider_models, fast_variant,
    model_id_matches_patterns,
};
use zdx_engine::providers::{ProviderKind, resolve_provider};

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::mutations::{ConfigMutation, StateMutation, TranscriptMutation};
use crate::state::TuiState;

/// One selectable row: a registry model, optionally as its `@fast` variant.
#[derive(Debug, Clone, Copy)]
struct ModelRow {
    model: &'static ModelOption,
    fast: bool,
}

impl ModelRow {
    /// Fully qualified spec persisted when this row is selected.
    fn spec(&self) -> String {
        let id = self.model.qualified_id();
        if self.fast {
            return fast_variant(&id).unwrap_or(id);
        }
        id
    }
}

#[derive(Debug, Clone)]
pub struct ModelPickerState {
    pub selected: usize,
    pub filter: String,
    /// Per-provider allow-list of model patterns, captured at open time.
    /// Only providers with `enabled = true` are present here; the value is
    /// the provider's `[providers.X].models` list (may be empty, which means
    /// "no filter — show every registered model for this provider").
    enabled_providers: HashMap<String, Vec<String>>,
    /// Candidate rows captured at open time: the static registry plus
    /// synthesized custom-provider entries, each followed by its `@fast`
    /// variant when the provider supports the priority service tier.
    models: Vec<ModelRow>,
}

impl ModelPickerState {
    pub fn open(current_model: &str, providers: &ProvidersConfig) -> (Self, Vec<UiEffect>) {
        // Collect enabled providers (with their configured model patterns)
        let enabled_providers = collect_enabled_providers(providers);

        // Registry models plus synthesized custom-provider models, each with
        // an extra `@fast` row when the provider supports the priority tier.
        let models: Vec<ModelRow> = available_models()
            .iter()
            .chain(custom_provider_models(providers).iter())
            .flat_map(|model| {
                let base = ModelRow { model, fast: false };
                let fast = fast_variant(&model.qualified_id())
                    .is_some()
                    .then_some(ModelRow { model, fast: true });
                std::iter::once(base).chain(fast)
            })
            .collect();

        // Filter available models by enabled providers + their pattern lists
        let enabled_models: Vec<_> = models
            .iter()
            .copied()
            .filter(|row| model_passes_provider_filter(row.model, &enabled_providers))
            .collect();

        let target = resolve_provider(current_model);
        let selected = enabled_models
            .iter()
            .position(|row| {
                let candidate = resolve_provider(row.model.id);
                row.fast == target.fast
                    && candidate.kind == target.kind
                    && candidate.model == target.model
            })
            .unwrap_or(0);
        (
            Self {
                selected,
                filter: String::new(),
                enabled_providers,
                models,
            },
            vec![],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_model_picker(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
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
                let Some(row) = self.filtered_models().get(self.selected).copied() else {
                    return OverlayUpdate::close();
                };

                // Include provider (and OAuth account) prefix so we don't rely
                // on auto-detection
                let model_id = row.spec();
                let display_name = row_label(row);

                let root = tui.agent_opts.root.clone();
                let use_thread_override = tui.thread.thread_handle.is_some()
                    && (tui.thread.model_override.is_some()
                        || tui.thread.thinking_override.is_some());
                if use_thread_override && tui.agent_state.is_running() {
                    return OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Stop the current task first before changing this thread's model override."
                                .to_string(),
                        ),
                    )]);
                }
                OverlayUpdate::close()
                    .with_ui_effects(vec![
                        if use_thread_override {
                            UiEffect::PersistThreadModelOverride {
                                model: model_id.clone(),
                            }
                        } else {
                            UiEffect::PersistModel {
                                model: model_id.clone(),
                            }
                        },
                        UiEffect::RefreshSystemPrompt { path: root },
                    ])
                    .with_mutations(vec![
                        if use_thread_override {
                            StateMutation::Thread(crate::mutations::ThreadMutation::SetOverrides {
                                model_override: Some(model_id.clone()),
                                thinking_override: tui.thread.thinking_override,
                            })
                        } else {
                            StateMutation::Config(ConfigMutation::SetModel(model_id.clone()))
                        },
                        StateMutation::SetActiveThreadOverrides {
                            model_override: if use_thread_override {
                                Some(model_id)
                            } else {
                                tui.thread.model_override.clone()
                            },
                            thinking_override: tui.thread.thinking_override,
                        },
                        StateMutation::Transcript(TranscriptMutation::AppendOrReplaceSwitchNotice(
                            format!("Switched to {display_name}"),
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
                    super::render_utils::clear_word_left(&mut self.filter);
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

    fn filtered_models(&self) -> Vec<ModelRow> {
        self.models
            .iter()
            .copied()
            .filter(|row| model_passes_provider_filter(row.model, &self.enabled_providers))
            .filter(|row| self.filter.is_empty() || row_matches_filter(*row, &self.filter))
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
        InputHint, InputLine, OverlayConfig, render_input_line, render_overlay, render_separator,
    };

    let filtered = picker.filtered_models();
    let max_label_len = filtered
        .iter()
        .map(|row| row_label(*row).len() as u16)
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
            title: "Select Model",
            border_color: Color::Magenta,
            width: picker_width,
            height: picker_height,
            hints: &hints,
        },
    );

    let filter_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, 1);
    render_input_line(
        frame,
        filter_area,
        &InputLine {
            value: &picker.filter,
            placeholder: None,
            prompt: "> ",
            prompt_color: Color::DarkGray,
            text_color: Color::Magenta,
            placeholder_color: Color::DarkGray,
            cursor_color: Color::Magenta,
        },
    );

    render_separator(frame, layout.body, 1);

    let list_height = layout.body.height.saturating_sub(4);
    let list_area = Rect::new(
        layout.body.x,
        layout.body.y + 2,
        layout.body.width,
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
            .map(|row| ListItem::new(row_line(*row, line_width)))
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

    render_separator(frame, layout.body, 2 + list_height);
    let selected_model = filtered.get(picker.selected).map(|row| row.model);
    render_capabilities_line(frame, layout.body, 3 + list_height, selected_model);
}

fn model_label(model: &ModelOption) -> String {
    let label = zdx_engine::providers::provider_account_label(model.provider, model.account);
    let name = cleaned_display_name(model, model.provider);
    format!("{label} · {name}")
}

/// Row label, tagging the `@fast` variant so it is distinguishable in the list.
fn row_label(row: ModelRow) -> String {
    let label = model_label(row.model);
    if row.fast {
        format!("{label} @fast")
    } else {
        label
    }
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

/// Renders one picker row; `@fast` rows carry the priority-tier cost hint.
fn row_line(row: ModelRow, width: u16) -> Line<'static> {
    if row.fast {
        return fast_row_line(row.model, width);
    }
    model_line(row.model, width)
}

/// `@fast` row: same left label plus the suffix, and a right side that states
/// the 2× priority-tier cost instead of the base pricing.
fn fast_row_line(model: &ModelOption, width: u16) -> Line<'static> {
    let label = zdx_engine::providers::provider_account_label(model.provider, model.account);
    let name = cleaned_display_name(model, model.provider);
    let right_text = "priority tier · 2× cost".to_string();

    let left_width = (label.len() + 3 + name.len() + 6) as u16;
    let right_width = right_text.len() as u16;
    let spacing = if width <= left_width + right_width {
        1
    } else {
        width - left_width - right_width
    } as usize;

    Line::from(vec![
        Span::styled(format!("{label} · "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            name,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" @fast", Style::default().fg(Color::Yellow)),
        Span::raw(" ".repeat(spacing)),
        Span::styled(right_text, Style::default().fg(Color::Yellow)),
    ])
}

fn model_line(model: &ModelOption, width: u16) -> Line<'static> {
    let label = zdx_engine::providers::provider_account_label(model.provider, model.account);
    let name = cleaned_display_name(model, model.provider);
    let context = format_context(model.context_limit);
    let pricing = format_pricing(model.pricing.input, model.pricing.output);

    // Check if this provider is subscription-based
    let is_subscription = ProviderKind::all()
        .iter()
        .find(|kind| kind.id() == model.provider)
        .is_some_and(|kind| zdx_engine::providers::ProviderKind::is_subscription(*kind));

    // Build the right side with pricing and context
    let (pricing_text, pricing_suffix) = if is_subscription && !pricing.is_empty() {
        (pricing.clone(), " (subs)")
    } else {
        (pricing.clone(), "")
    };

    let right_text = if context.is_empty() && pricing_text.is_empty() {
        String::new()
    } else if pricing_text.is_empty() {
        context.clone()
    } else if context.is_empty() {
        format!("{pricing_text}{pricing_suffix}")
    } else {
        format!("{pricing_text}{pricing_suffix} · {context}")
    };

    let left_width = (label.len() + 3 + name.len()) as u16;
    let right_width = right_text.len() as u16;
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
        format!("{label} · "),
        Style::default().fg(Color::DarkGray),
    ));
    spans.push(Span::styled(name, left_style));
    spans.push(Span::raw(" ".repeat(spacing)));

    // For subscription providers, show pricing with strikethrough
    if is_subscription && !pricing.is_empty() {
        let pricing_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::CROSSED_OUT);
        spans.push(Span::styled(pricing, pricing_style));
        spans.push(Span::styled(
            " (subs)",
            Style::default().fg(Color::DarkGray),
        ));
        if !context.is_empty() {
            spans.push(Span::styled(
                format!(" · {context}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    } else {
        spans.push(Span::styled(
            right_text,
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::from(spans)
}

fn format_context(context_limit: u64) -> String {
    if context_limit == 0 {
        return String::new();
    }

    if context_limit >= 1_000_000 {
        let millions = context_limit as f64 / 1_000_000.0;
        if (millions - millions.round()).abs() < 0.05 {
            format!("{millions:.0}M")
        } else {
            format!("{millions:.1}M")
        }
    } else {
        format!("{}k", context_limit / 1_000)
    }
}

fn format_pricing(input: f64, output: f64) -> String {
    let input = if input.abs() <= f64::EPSILON {
        0.0
    } else {
        input
    };
    let output = if output.abs() <= f64::EPSILON {
        0.0
    } else {
        output
    };
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
    let mut text = format!("{value:.3}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn row_matches_filter(row: ModelRow, filter: &str) -> bool {
    let filter = filter.to_lowercase();
    if filter.is_empty() {
        return true;
    }

    let label = row_label(row).to_lowercase();
    let id = row.model.id.to_lowercase();
    label.contains(&filter) || id.contains(&filter)
}

fn provider_label(provider_id: &str) -> String {
    ProviderKind::all()
        .iter()
        .find(|kind| kind.id() == provider_id)
        .map_or_else(|| provider_id.to_string(), |kind| kind.label().to_string())
}

/// Collects the set of enabled provider IDs from the config, paired with
/// each provider's `[providers.X].models` allow-list. An empty allow-list
/// is preserved as-is and interpreted downstream as "no filter".
fn collect_enabled_providers(providers: &ProvidersConfig) -> HashMap<String, Vec<String>> {
    let mut enabled: HashMap<String, Vec<String>> = ProviderKind::all()
        .iter()
        .filter(|kind| providers.is_enabled(kind.id()))
        .map(|kind| (kind.id().to_string(), providers.get(*kind).models.clone()))
        .collect();
    for (name, cfg) in &providers.custom {
        enabled.insert(name.clone(), cfg.models.clone());
    }
    enabled
}

/// Returns true if a registry entry should be shown in the picker given the
/// current enabled-provider map: the provider must be enabled, and the
/// model's bare id (i.e. without the `provider:` prefix) must match the
/// provider's configured `models` allow-list (empty list = no filter).
fn model_passes_provider_filter(
    model: &ModelOption,
    enabled_providers: &HashMap<String, Vec<String>>,
) -> bool {
    let Some(patterns) = enabled_providers.get(model.provider) else {
        return false;
    };
    let bare = bare_model_id(model.provider, model.id);
    model_id_matches_patterns(bare, patterns)
}

#[cfg(test)]
mod tests {
    use zdx_engine::models::{ModelCapabilities, ModelOption, ModelPricing};

    use super::{ModelRow, model_line, row_line};

    fn subscription_model(account: Option<&'static str>) -> ModelOption {
        ModelOption {
            id: "claude-fable-5",
            provider: "claude-cli",
            account,
            display_name: "Claude Fable 5",
            pricing: ModelPricing {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_limit: 1_000_000,
            capabilities: ModelCapabilities::default(),
        }
    }

    fn rendered(model: &ModelOption) -> String {
        model_line(model, 80)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn fast_row_carries_the_suffix_spec_and_cost_hint() {
        let model: &'static ModelOption = Box::leak(Box::new(ModelOption {
            id: "gpt-5.2",
            provider: "openai",
            account: None,
            display_name: "GPT-5.2",
            pricing: ModelPricing {
                input: 1.25,
                output: 10.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_limit: 400_000,
            capabilities: ModelCapabilities::default(),
        }));
        let row = ModelRow { model, fast: true };

        assert_eq!(row.spec(), "openai:gpt-5.2@fast");

        let line: String = row_line(row, 80)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(line.contains("@fast"), "got: {line}");
        assert!(line.contains("2× cost"), "got: {line}");
    }

    #[test]
    fn row_tags_the_named_account_on_the_provider_label() {
        let line = rendered(&subscription_model(Some("parity")));

        assert!(line.contains("Claude CLI @parity · "), "got: {line}");
        assert!(line.contains("(subs)"), "got: {line}");
    }
}
