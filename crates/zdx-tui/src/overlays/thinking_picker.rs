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
fn switch_label(display_name: &str, level: ThinkingLevel) -> String {
    if level == ThinkingLevel::Off {
        display_name.to_string()
    } else {
        format!("{display_name} [{}]", level.display_name())
    }
}

/// Where a committed model selection goes.
pub struct ModelSelection<'a> {
    /// Canonical model spec (thinking level folded in).
    pub model: String,
    /// Human label for the switch notice, e.g. `Sonnet 4.5 [high]`.
    pub label: String,
    /// A handoff composer is open: the pick belongs to the thread it will
    /// create, not to this one.
    pub staged_for_handoff: bool,
    /// The current thread exists on disk, so an applied pick is persisted.
    pub thread_exists: bool,
    pub root: &'a std::path::Path,
}

/// The single place a model selection turns into effects + mutations.
///
/// Applied picks write session state, the thread's metadata, and refresh the
/// system prompt (which depends on the model) — never the workspace config,
/// which only `/model-save` writes. While a handoff is open the pick is staged
/// on the input state instead: the source thread keeps its model no matter how
/// the handoff ends, and `HandoffSubmit` carries the staged spec to the new
/// thread.
pub fn model_selection_updates(
    selection: ModelSelection<'_>,
) -> (Vec<UiEffect>, Vec<StateMutation>) {
    let ModelSelection {
        model,
        label,
        staged_for_handoff,
        thread_exists,
        root,
    } = selection;

    if staged_for_handoff {
        return (
            vec![],
            vec![
                StateMutation::Input(crate::mutations::InputMutation::SetHandoffModel(Some(
                    model,
                ))),
                StateMutation::Transcript(TranscriptMutation::AppendOrReplaceSwitchNotice(
                    format!("Handoff will open on {label}"),
                )),
            ],
        );
    }

    let mut effects = Vec::new();
    if thread_exists {
        effects.push(UiEffect::PersistThreadModelOverride {
            model: model.clone(),
        });
    }
    effects.push(UiEffect::RefreshSystemPrompt {
        path: root.to_path_buf(),
    });

    (
        effects,
        vec![
            StateMutation::Thread(crate::mutations::ThreadMutation::SetOverrides {
                model_override: Some(model.clone()),
                thinking_override: None,
            }),
            StateMutation::SetActiveThreadOverrides {
                model_override: Some(model),
                thinking_override: None,
            },
            StateMutation::Transcript(TranscriptMutation::AppendOrReplaceSwitchNotice(format!(
                "Switched to {label}"
            ))),
        ],
    )
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
                let (effects, mutations) = model_selection_updates(ModelSelection {
                    model,
                    label: switch_label(&self.display_name, level),
                    staged_for_handoff: tui.input.handoff.is_active(),
                    thread_exists: tui.thread.thread_handle.is_some(),
                    root: tui.agent_opts.root.as_path(),
                });

                OverlayUpdate::close()
                    .with_ui_effects(effects)
                    .with_mutations(mutations)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutations::InputMutation;

    fn selection(staged: bool, thread_exists: bool) -> ModelSelection<'static> {
        ModelSelection {
            model: "openai:gpt-5.1-codex@high".to_string(),
            label: "GPT-5.1 Codex [high]".to_string(),
            staged_for_handoff: staged,
            thread_exists,
            root: std::path::Path::new("."),
        }
    }

    /// An applied pick belongs to this thread: session state, the thread's
    /// metadata, and the system prompt — never the workspace config.
    #[test]
    fn applied_selection_writes_session_and_thread_state() {
        let (effects, mutations) = model_selection_updates(selection(false, true));

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, UiEffect::PersistThreadModelOverride { .. }))
        );
        assert!(
            mutations
                .iter()
                .any(|m| matches!(m, StateMutation::SetActiveThreadOverrides { .. }))
        );
    }

    /// A tab whose thread does not exist yet (a fresh `/btw` tab before its
    /// first send) has nothing on disk to write to: the pick stays in session
    /// state and is pinned when that thread is created.
    #[test]
    fn selection_without_a_thread_persists_nothing() {
        let (effects, mutations) = model_selection_updates(selection(false, false));

        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, UiEffect::PersistThreadModelOverride { .. }))
        );
        assert!(
            mutations
                .iter()
                .any(|m| matches!(m, StateMutation::SetActiveThreadOverrides { .. }))
        );
    }

    /// A pick made while the handoff composer is open is staged for the thread
    /// the handoff will create: nothing about the current thread changes.
    #[test]
    fn staged_selection_touches_neither_thread_nor_session() {
        let (effects, mutations) = model_selection_updates(selection(true, true));

        assert!(effects.is_empty(), "got: {effects:?}");
        assert!(mutations.iter().any(|m| matches!(
            m,
            StateMutation::Input(InputMutation::SetHandoffModel(Some(model))) if model == "openai:gpt-5.1-codex@high"
        )));
        assert!(!mutations.iter().any(|m| matches!(
            m,
            StateMutation::SetActiveThreadOverrides { .. }
                | StateMutation::Thread(crate::mutations::ThreadMutation::SetOverrides { .. })
        )));
    }
}
