//! BTW overlay for background side questions.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use zdx_engine::config::{ProvidersConfig, ThinkingLevel};
use zdx_engine::core::events::AgentEvent;
use zdx_engine::core::thread_persistence::Thread;
use zdx_engine::models::{ModelOption, available_models, model_supports_reasoning};
use zdx_engine::providers::{ChatMessage, ProviderKind, resolve_provider};

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::input::TextBuffer;
use crate::mutations::{StateMutation, ThreadMutation};
use crate::state::{AgentState, TuiState};
use crate::transcript::{HistoryCell, TranscriptState};

const OVERLAY_WIDTH: u16 = 96;
const OVERLAY_HEIGHT: u16 = 28;
const MOUSE_SCROLL_LINES: usize = 3;

#[derive(Debug)]
enum BtwPickerMode {
    None,
    Model { selected: usize },
    Thinking { selected: usize },
}

/// State for the BTW side-question overlay.
#[derive(Debug)]
pub struct BtwState {
    pub input: TextBuffer,
    pub base_messages: Vec<ChatMessage>,
    pub thread_handle: Option<Thread>,
    pub messages: Vec<ChatMessage>,
    pub transcript: TranscriptState,
    pub agent_state: AgentState,
    pub scroll_from_bottom: usize,
    pub model: String,
    pub thinking_level: ThinkingLevel,
    enabled_models: Vec<&'static ModelOption>,
    picker_mode: BtwPickerMode,
    pub error: Option<String>,
}

impl BtwState {
    /// # Errors
    /// Returns an error if `base_messages` is empty.
    pub fn open(
        base_messages: Vec<ChatMessage>,
        current_model: &str,
        current_thinking: ThinkingLevel,
        providers: &ProvidersConfig,
    ) -> Result<(Self, Vec<UiEffect>), String> {
        if base_messages.is_empty() {
            return Err("No stable thread context available for a side question yet.".to_string());
        }
        let enabled_models = collect_enabled_models(providers);
        Ok((
            Self {
                input: TextBuffer::default(),
                base_messages,
                thread_handle: None,
                messages: Vec::new(),
                transcript: TranscriptState::with_cells(vec![HistoryCell::system(
                    "BTW: this is a forked side chat. The main thread keeps running underneath.",
                )]),
                agent_state: AgentState::Idle,
                scroll_from_bottom: 0,
                model: current_model.to_string(),
                thinking_level: current_thinking,
                enabled_models,
                picker_mode: BtwPickerMode::None,
                error: None,
            },
            vec![],
        ))
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_btw_overlay(frame, self, area, input_y);
    }

    pub fn handle_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        area: Rect,
        input_y: u16,
    ) -> bool {
        let areas = compute_btw_areas(area, input_y, self.input.lines().len().clamp(1, 3) as u16);
        if !rect_contains(areas.popup, mouse.column, mouse.row) {
            return false;
        }
        if !rect_contains(areas.transcript, mouse.column, mouse.row) {
            return true;
        }

        let total_lines = render_transcript_lines(self, areas.transcript.width as usize).len();
        let viewport = usize::from(areas.transcript.height.max(1));
        let max_scroll = total_lines.saturating_sub(viewport);

        match mouse.kind {
            crossterm::event::MouseEventKind::ScrollUp => {
                self.scroll_from_bottom =
                    (self.scroll_from_bottom + MOUSE_SCROLL_LINES).min(max_scroll);
            }
            crossterm::event::MouseEventKind::ScrollDown => {
                self.scroll_from_bottom =
                    self.scroll_from_bottom.saturating_sub(MOUSE_SCROLL_LINES);
            }
            _ => {}
        }
        true
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if !matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
            self.error = None;
        }

        if !matches!(self.picker_mode, BtwPickerMode::None) {
            return self.handle_picker_key(key, ctrl);
        }

        match key.code {
            KeyCode::Esc => {
                if text_value(&self.input).is_empty() {
                    OverlayUpdate::close()
                } else {
                    self.input = TextBuffer::default();
                    OverlayUpdate::stay()
                }
            }
            KeyCode::Char('c') if ctrl => {
                if self.agent_state.is_running() {
                    OverlayUpdate::stay().with_ui_effects(vec![UiEffect::InterruptBtwAgent])
                } else {
                    OverlayUpdate::close()
                }
            }
            KeyCode::Char('p') if ctrl => {
                self.picker_mode = BtwPickerMode::Model {
                    selected: self.current_model_index(),
                };
                OverlayUpdate::stay()
            }
            KeyCode::Char('t') if ctrl => {
                if model_supports_reasoning(&self.model) {
                    self.picker_mode = BtwPickerMode::Thinking {
                        selected: self.current_thinking_index(),
                    };
                } else {
                    self.error = Some("Current model does not support reasoning.".to_string());
                }
                OverlayUpdate::stay()
            }
            KeyCode::PageUp => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(8);
                OverlayUpdate::stay()
            }
            KeyCode::PageDown => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(8);
                OverlayUpdate::stay()
            }
            KeyCode::Enter => {
                if self.agent_state.is_running() {
                    self.error = Some("Wait for the current side reply to finish.".to_string());
                    return OverlayUpdate::stay();
                }
                let prompt = text_value(&self.input);
                let trimmed = prompt.trim();
                if trimmed.is_empty() {
                    self.error = Some("Enter a side question first.".to_string());
                    OverlayUpdate::stay()
                } else {
                    OverlayUpdate::stay().with_ui_effects(vec![UiEffect::StartBtwTurn {
                        base_messages: self.base_messages.clone(),
                        thread_handle: self.thread_handle.clone(),
                        messages: self.messages.clone(),
                        prompt: trimmed.to_string(),
                        model: self.model.clone(),
                        thinking_level: self.thinking_level,
                    }])
                }
            }
            _ => {
                self.input.input(key);
                OverlayUpdate::stay()
            }
        }
    }

    fn handle_picker_key(&mut self, key: KeyEvent, ctrl: bool) -> OverlayUpdate {
        match (&mut self.picker_mode, key.code) {
            (_, KeyCode::Esc | KeyCode::Char('c')) if ctrl => {
                self.picker_mode = BtwPickerMode::None;
                OverlayUpdate::stay()
            }
            (
                BtwPickerMode::Model { selected } | BtwPickerMode::Thinking { selected },
                KeyCode::Up,
            ) => {
                *selected = selected.saturating_sub(1);
                OverlayUpdate::stay()
            }
            (BtwPickerMode::Model { selected }, KeyCode::Down) => {
                let max = self.enabled_models.len().saturating_sub(1);
                *selected = (*selected + 1).min(max);
                OverlayUpdate::stay()
            }
            (BtwPickerMode::Model { selected }, KeyCode::Enter) => {
                if let Some(model) = self.enabled_models.get(*selected) {
                    self.model = format!("{}:{}", model.provider, model.id);
                    if !model.capabilities.reasoning {
                        self.thinking_level = ThinkingLevel::Off;
                    }
                }
                self.picker_mode = BtwPickerMode::None;
                OverlayUpdate::stay()
            }
            (BtwPickerMode::Thinking { selected }, KeyCode::Down) => {
                let max = ThinkingLevel::all().len().saturating_sub(1);
                *selected = (*selected + 1).min(max);
                OverlayUpdate::stay()
            }
            (BtwPickerMode::Thinking { selected }, KeyCode::Enter) => {
                if let Some(level) = ThinkingLevel::all().get(*selected).copied() {
                    self.thinking_level = level;
                }
                self.picker_mode = BtwPickerMode::None;
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    fn current_model_index(&self) -> usize {
        let current = resolve_provider(&self.model);
        self.enabled_models
            .iter()
            .position(|model| {
                let candidate = resolve_provider(&format!("{}:{}", model.provider, model.id));
                candidate.kind == current.kind && candidate.model == current.model
            })
            .unwrap_or(0)
    }

    fn current_thinking_index(&self) -> usize {
        ThinkingLevel::all()
            .iter()
            .position(|level| *level == self.thinking_level)
            .unwrap_or(0)
    }

    pub fn on_turn_spawned(
        &mut self,
        thread_handle: Thread,
        prompt: String,
        messages: Vec<ChatMessage>,
        rx: tokio::sync::mpsc::UnboundedReceiver<std::sync::Arc<AgentEvent>>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        self.thread_handle = Some(thread_handle);
        self.messages = messages;
        self.agent_state = AgentState::Waiting { rx, cancel };
        self.transcript.push_cell(HistoryCell::user(prompt));
        self.transcript.activate_pending_user_cell();
        self.scroll_from_bottom = 0;
        self.input = TextBuffer::default();
        self.error = None;
    }

    pub fn handle_agent_event(&mut self, event: &AgentEvent) {
        let (_effects, mutations) = crate::transcript::handle_agent_event(
            &mut self.transcript,
            &mut self.agent_state,
            false,
            event,
        );
        self.apply_mutations(mutations);
    }

    fn apply_mutations(&mut self, mutations: Vec<StateMutation>) {
        for mutation in mutations {
            if let StateMutation::Thread(ThreadMutation::SetMessages(messages)) = mutation {
                self.messages = messages;
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn render_btw_overlay(frame: &mut Frame, state: &BtwState, area: Rect, input_top_y: u16) {
    use super::render_utils::{InputHint, OverlayConfig, render_overlay, render_separator};

    let idle_hints = [
        InputHint::new("Enter", "send"),
        InputHint::new("Ctrl+L", "model"),
        InputHint::new("Ctrl+T", "thinking"),
        InputHint::new("PgUp/PgDn", "scroll"),
        InputHint::new("Esc", "clear/close"),
    ];
    let running_hints = [
        InputHint::new("Ctrl+C", "stop"),
        InputHint::new("Ctrl+L", "model"),
        InputHint::new("Ctrl+T", "thinking"),
        InputHint::new("PgUp/PgDn", "scroll"),
        InputHint::new("Esc", "close"),
    ];
    let picker_hints = [
        InputHint::new("↑↓", "navigate"),
        InputHint::new("Enter", "select"),
        InputHint::new("Esc", "back"),
    ];
    let hints = match state.picker_mode {
        BtwPickerMode::None if state.agent_state.is_running() => &running_hints[..],
        BtwPickerMode::None => &idle_hints[..],
        _ => &picker_hints[..],
    };
    let input_height = state.input.lines().len().clamp(1, 3) as u16;
    let areas = compute_btw_areas(area, input_top_y, input_height);
    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: "BTW — side chat",
            border_color: Color::Cyan,
            width: OVERLAY_WIDTH,
            height: OVERLAY_HEIGHT,
            hints,
        },
    );

    let info = Paragraph::new(vec![
        Line::from(Span::styled(
            "Forked from the latest stable context. This popup has its own thread.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
            Span::styled(display_model_label(state), Style::default().fg(Color::Cyan)),
            Span::styled("  Thinking: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                state.thinking_level.display_name(),
                Style::default().fg(Color::Yellow),
            ),
        ]),
    ]);
    frame.render_widget(
        info,
        Rect::new(layout.body.x, layout.body.y, layout.body.width, 2),
    );

    render_separator(frame, layout.body, 2);

    match state.picker_mode {
        BtwPickerMode::None => frame.render_widget(
            Paragraph::new(render_visible_transcript_lines(
                state,
                areas.transcript.width as usize,
                areas.transcript.height as usize,
            ))
            .block(Block::default().borders(Borders::NONE)),
            areas.transcript,
        ),
        BtwPickerMode::Model { selected } => {
            render_model_list(frame, state, areas.transcript, selected);
        }
        BtwPickerMode::Thinking { selected } => {
            render_thinking_list(frame, areas.transcript, selected);
        }
    }

    let prompt_separator_y = 3 + areas.transcript.height;
    render_separator(frame, layout.body, prompt_separator_y);

    if matches!(state.picker_mode, BtwPickerMode::None) {
        let prompt = format!("{}█", text_value(&state.input));
        let prompt_para = Paragraph::new(prompt).style(Style::default().fg(Color::Cyan));
        frame.render_widget(prompt_para, areas.input);
        frame.render_widget(
            Paragraph::new(">").style(Style::default().fg(Color::Cyan)),
            Rect::new(areas.input.x.saturating_sub(2), areas.input.y, 2, 1),
        );
    }

    let (help_text, help_style) = if let Some(error) = &state.error {
        (error.as_str(), Style::default().fg(Color::Red))
    } else if state.agent_state.is_running() {
        (
            "Thinking… you can close this popup and reopen the thread later from /threads.",
            Style::default().fg(Color::DarkGray),
        )
    } else if matches!(state.picker_mode, BtwPickerMode::Model { .. }) {
        (
            "Select a model for this popup only.",
            Style::default().fg(Color::DarkGray),
        )
    } else if matches!(state.picker_mode, BtwPickerMode::Thinking { .. }) {
        (
            "Select a reasoning level for this popup only.",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (
            "Send a follow-up here or close it and reopen the thread later from /threads.",
            Style::default().fg(Color::DarkGray),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(help_text, help_style))),
        areas.help,
    );
}

fn text_value(buffer: &TextBuffer) -> String {
    buffer.lines().join("\n")
}

fn render_transcript_lines(state: &BtwState, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let effective_width = width.saturating_sub(1);

    for cell in state.transcript.cells() {
        for styled in cell.display_lines_cached(effective_width, 0, &state.transcript.wrap_cache) {
            let spans: Vec<_> = styled
                .spans
                .into_iter()
                .map(|span| Span::styled(span.text, map_style(span.style)))
                .collect();
            lines.push(Line::from(spans));
        }
        lines.push(Line::default());
    }

    lines
}

fn render_visible_transcript_lines(
    state: &BtwState,
    width: usize,
    viewport_height: usize,
) -> Vec<Line<'static>> {
    let all_lines = render_transcript_lines(state, width);
    let total_lines = all_lines.len();
    let start = total_lines
        .saturating_sub(viewport_height)
        .saturating_sub(state.scroll_from_bottom);
    all_lines
        .into_iter()
        .skip(start)
        .take(viewport_height)
        .collect()
}

fn render_model_list(frame: &mut Frame, state: &BtwState, area: Rect, selected: usize) {
    let items: Vec<ListItem> = state
        .enabled_models
        .iter()
        .map(|model| {
            let label = format!(
                "{} · {}",
                provider_label(model.provider),
                cleaned_display_name(model, model.provider)
            );
            ListItem::new(Line::from(Span::styled(
                label,
                Style::default().fg(Color::White),
            )))
        })
        .collect();
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black));
    let mut list_state = ListState::default();
    if !state.enabled_models.is_empty() {
        list_state.select(Some(
            selected.min(state.enabled_models.len().saturating_sub(1)),
        ));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_thinking_list(frame: &mut Frame, area: Rect, selected: usize) {
    let levels = ThinkingLevel::all();
    let items: Vec<ListItem> = levels
        .iter()
        .map(|level| {
            ListItem::new(Line::from(vec![
                Span::styled(level.display_name(), Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!(" — {}", level.description()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().bg(Color::Yellow).fg(Color::Black));
    let mut list_state = ListState::default();
    list_state.select(Some(selected.min(levels.len().saturating_sub(1))));
    frame.render_stateful_widget(list, area, &mut list_state);
}

struct BtwAreas {
    popup: Rect,
    transcript: Rect,
    input: Rect,
    help: Rect,
}

fn compute_btw_areas(area: Rect, input_top_y: u16, input_height: u16) -> BtwAreas {
    use super::render_utils::calculate_overlay_area;

    let popup = calculate_overlay_area(area, input_top_y, OVERLAY_WIDTH, OVERLAY_HEIGHT);
    let body = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(3),
    );
    let transcript_height = body.height.saturating_sub(5 + input_height).max(1);
    let transcript = Rect::new(body.x, body.y + 3, body.width, transcript_height);
    let input = Rect::new(
        body.x + 2,
        body.y + 4 + transcript_height,
        body.width.saturating_sub(2),
        input_height,
    );
    let help = Rect::new(body.x, input.y + input_height, body.width, 1);

    BtwAreas {
        popup,
        transcript,
        input,
        help,
    }
}

fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn display_model_label(state: &BtwState) -> String {
    let current = resolve_provider(&state.model);
    state
        .enabled_models
        .iter()
        .find(|model| model.provider == current.kind.id() && model.id == current.model)
        .map_or_else(
            || state.model.clone(),
            |model| {
                format!(
                    "{} · {}",
                    provider_label(model.provider),
                    cleaned_display_name(model, model.provider)
                )
            },
        )
}

fn collect_enabled_models(providers: &ProvidersConfig) -> Vec<&'static ModelOption> {
    available_models()
        .iter()
        .filter(|model| providers.is_enabled(model.provider))
        .collect()
}

fn cleaned_display_name(model: &ModelOption, provider: &str) -> String {
    let mut name = model.display_name.to_string();
    if provider == "anthropic" {
        name = name.replace(" (latest)", "");
    }
    let prefix = format!("{} · ", provider_label(provider));
    name.strip_prefix(&prefix).unwrap_or(&name).to_string()
}

fn provider_label(provider_id: &str) -> String {
    ProviderKind::all()
        .iter()
        .find(|kind| kind.id() == provider_id)
        .map_or_else(|| provider_id.to_string(), |kind| kind.label().to_string())
}

fn map_style(style: crate::transcript::Style) -> Style {
    use ratatui::style::Modifier;

    use crate::transcript::Style as TranscriptStyle;

    match style {
        TranscriptStyle::Plain | TranscriptStyle::Assistant | TranscriptStyle::ToolOutput => {
            Style::default().fg(Color::White)
        }
        TranscriptStyle::UserPrefix | TranscriptStyle::User => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::ITALIC),
        TranscriptStyle::StreamingCursor | TranscriptStyle::Link => {
            Style::default().fg(Color::Cyan)
        }
        TranscriptStyle::SystemPrefix
        | TranscriptStyle::System
        | TranscriptStyle::Timing
        | TranscriptStyle::ToolCancelled
        | TranscriptStyle::Interrupted
        | TranscriptStyle::ListBullet
        | TranscriptStyle::ListNumber
        | TranscriptStyle::BlockQuote => Style::default().fg(Color::DarkGray),
        TranscriptStyle::ToolBracket | TranscriptStyle::ToolStatus => {
            Style::default().fg(Color::Blue)
        }
        TranscriptStyle::ToolError => Style::default().fg(Color::Red),
        TranscriptStyle::ToolRunning
        | TranscriptStyle::ToolTruncation
        | TranscriptStyle::ThinkingPrefix
        | TranscriptStyle::ImagePlaceholder => Style::default().fg(Color::Yellow),
        TranscriptStyle::ToolSuccess => Style::default().fg(Color::Green),
        TranscriptStyle::Thinking => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        TranscriptStyle::CodeInline | TranscriptStyle::CodeBlock | TranscriptStyle::CodeFence => {
            Style::default().fg(Color::Green)
        }
        TranscriptStyle::Emphasis => Style::default().add_modifier(Modifier::ITALIC),
        TranscriptStyle::Strong => Style::default().add_modifier(Modifier::BOLD),
        TranscriptStyle::H1 | TranscriptStyle::H2 | TranscriptStyle::H3 => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

    use super::*;

    #[test]
    fn btw_opens_with_system_cell() {
        let cells = vec![
            ChatMessage::user("first"),
            ChatMessage::assistant_text("done", None),
        ];
        let (state, _) = BtwState::open(
            cells,
            "openai:gpt-4.1",
            ThinkingLevel::Off,
            &ProvidersConfig::default(),
        )
        .expect("btw opens");
        assert_eq!(state.transcript.cells().len(), 1);
    }

    #[test]
    fn mouse_scroll_updates_popup_scroll_offset() {
        let cells = vec![
            ChatMessage::user("first"),
            ChatMessage::assistant_text("done", None),
        ];
        let (mut state, _) = BtwState::open(
            cells,
            "openai:gpt-4.1",
            ThinkingLevel::Off,
            &ProvidersConfig::default(),
        )
        .expect("btw opens");
        for i in 0..20 {
            state
                .transcript
                .push_cell(HistoryCell::assistant(format!("line {i}")));
        }

        let area = Rect::new(0, 0, 120, 40);
        let areas = compute_btw_areas(area, 30, 1);
        let handled = state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: areas.transcript.x + 1,
                row: areas.transcript.y + 1,
                modifiers: KeyModifiers::empty(),
            },
            area,
            30,
        );

        assert!(handled);
        assert!(state.scroll_from_bottom > 0);
    }
}
