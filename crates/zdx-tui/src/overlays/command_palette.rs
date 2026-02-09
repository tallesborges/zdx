use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::{OverlayRequest, OverlayUpdate};
use crate::common::TaskKind;
use crate::common::clipboard::Clipboard;
use crate::common::commands::{COMMANDS, Command, command_available};
use crate::effects::UiEffect;
use crate::input::HandoffState;
use crate::mutations::{
    AuthMutation, InputMutation, StateMutation, ThreadMutation, TranscriptMutation,
};
use crate::state::TuiState;

#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    pub filter: String,
    pub selected: usize,
    pub provider: zdx_core::providers::ProviderKind,
    pub model_id: String,
}

impl CommandPaletteState {
    pub fn open(
        provider: zdx_core::providers::ProviderKind,
        model_id: String,
    ) -> (Self, Vec<UiEffect>) {
        (
            Self {
                filter: String::new(),
                selected: 0,
                provider,
                model_id,
            },
            vec![],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_command_palette(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => OverlayUpdate::close(),
            KeyCode::Char('c') if ctrl => OverlayUpdate::close(),
            KeyCode::Up => {
                let count = self.filtered_commands().len();
                if count > 0 && self.selected > 0 {
                    self.selected -= 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down => {
                let count = self.filtered_commands().len();
                if count > 0 && self.selected < count - 1 {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter | KeyCode::Tab => {
                if let Some(cmd_name) = self.get_selected_command_name() {
                    let (open_overlay, effects, mutations) = execute_command(tui, cmd_name);
                    let update = match open_overlay {
                        Some(request) => OverlayUpdate::open(request),
                        None => OverlayUpdate::close(),
                    };
                    update.with_ui_effects(effects).with_mutations(mutations)
                } else {
                    OverlayUpdate::close()
                }
            }
            KeyCode::Backspace => {
                self.filter.pop();
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

    pub fn filtered_commands(&self) -> Vec<&'static Command> {
        if self.filter.is_empty() {
            COMMANDS
                .iter()
                .filter(|cmd| command_available(cmd, &self.model_id))
                .collect()
        } else {
            COMMANDS
                .iter()
                .filter(|cmd| command_available(cmd, &self.model_id) && cmd.matches(&self.filter))
                .collect()
        }
    }

    pub fn clamp_selection(&mut self) {
        let count = self.filtered_commands().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(count - 1);
        }
    }

    fn get_selected_command_name(&self) -> Option<&'static str> {
        let filtered = self.filtered_commands();
        filtered.get(self.selected).map(|cmd| cmd.name)
    }
}

fn execute_command(
    tui: &TuiState,
    cmd_name: &str,
) -> (Option<OverlayRequest>, Vec<UiEffect>, Vec<StateMutation>) {
    match cmd_name {
        "config" => (None, vec![UiEffect::OpenConfig], vec![]),
        "debug" => (None, vec![], vec![StateMutation::ToggleDebugStatus]),
        "models" => (None, vec![UiEffect::OpenModelsConfig], vec![]),
        "copy-id" => {
            let (effects, mutations) = execute_copy_id(tui);
            (None, effects, mutations)
        }
        "login" => (Some(OverlayRequest::Login), vec![], vec![]),
        "logout" => {
            let (effects, mutations) = execute_logout(tui);
            (None, effects, mutations)
        }
        "rename" => {
            if tui.thread.thread_handle.is_none() {
                (
                    None,
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "No active thread to rename.".to_string(),
                        ),
                    )],
                )
            } else {
                (Some(OverlayRequest::Rename), vec![], vec![])
            }
        }
        "model" => (Some(OverlayRequest::ModelPicker), vec![], vec![]),
        "skills" => (Some(OverlayRequest::SkillPicker), vec![], vec![]),
        "threads" => {
            if tui.tasks.state(TaskKind::ThreadList).is_running() {
                return (None, vec![], vec![]);
            }
            (
                None,
                vec![UiEffect::OpenThreadPicker {
                    mode: crate::overlays::ThreadPickerMode::Switch,
                }],
                vec![],
            )
        }
        "worktree" => {
            if tui.tasks.state(TaskKind::ThreadWorktree).is_running() {
                (None, vec![], vec![])
            } else if tui.thread.thread_handle.is_none() {
                (
                    None,
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Worktree requires an active thread.".to_string(),
                        ),
                    )],
                )
            } else {
                (None, vec![UiEffect::EnsureWorktree], vec![])
            }
        }
        "root-new" => {
            let (effects, mutations) = execute_root_new(tui);
            (None, effects, mutations)
        }
        "thinking" => (Some(OverlayRequest::ThinkingPicker), vec![], vec![]),
        "timeline" => (Some(OverlayRequest::Timeline), vec![], vec![]),
        "handoff" => {
            let (effects, mutations) = execute_handoff(tui);
            (None, effects, mutations)
        }
        "new" => {
            let (effects, mutations) = execute_new(tui);
            (None, effects, mutations)
        }
        "quit" => (None, execute_quit(tui), vec![]),
        _ => (None, vec![], vec![]),
    }
}

fn execute_logout(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    use zdx_core::providers::oauth::{claude_cli, openai_codex};
    use zdx_core::providers::provider_for_model;

    let mut mutations = Vec::new();
    let provider = provider_for_model(&tui.config.model);
    let result = match provider {
        zdx_core::providers::ProviderKind::ClaudeCli => {
            claude_cli::clear_credentials().map(|had| (had, "Claude CLI"))
        }
        zdx_core::providers::ProviderKind::OpenAICodex => {
            openai_codex::clear_credentials().map(|had| (had, "OpenAI Codex"))
        }
        _ => {
            let message = provider
                .api_key_env_var()
                .map(|env| format!("Unset {} to log out.", env))
                .unwrap_or_else(|| "No OAuth credentials to clear.".to_string());
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(message),
            ));
            return (vec![], mutations);
        }
    };

    match result {
        Ok((true, label)) => {
            mutations.push(StateMutation::Auth(AuthMutation::RefreshStatus));
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(format!(
                    "Logged out from {} OAuth.",
                    label
                )),
            ));
        }
        Ok((false, _)) => {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "No OAuth credentials to clear.".to_string(),
                ),
            ));
        }
        Err(e) => {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(format!("Logout failed: {}", e)),
            ));
        }
    }

    (vec![], mutations)
}

fn execute_copy_id(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    match &tui.thread.thread_handle {
        Some(thread_handle) => {
            let id = thread_handle.id.clone();
            match Clipboard::copy(&id) {
                Ok(()) => (
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(format!(
                            "Thread ID copied: {}",
                            id
                        )),
                    )],
                ),
                Err(e) => (
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(format!(
                            "Failed to copy thread ID: {}",
                            e
                        )),
                    )],
                ),
            }
        }
        None => (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage("No active thread.".to_string()),
            )],
        ),
    }
}

fn execute_handoff(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    if tui.thread.thread_handle.is_none() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Handoff requires an active thread.".to_string(),
                ),
            )],
        );
    }

    (
        vec![],
        vec![
            StateMutation::Input(InputMutation::SetHandoffState(HandoffState::Pending)),
            StateMutation::Input(InputMutation::Clear),
        ],
    )
}

fn execute_new(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    if let Some(result) = prepare_new_thread_transition(tui) {
        return result;
    }

    let mut mutations = new_thread_reset_mutations();

    if tui.thread.thread_handle.is_some() {
        (vec![UiEffect::CreateNewThread], mutations)
    } else {
        mutations.push(StateMutation::Transcript(
            TranscriptMutation::AppendSystemMessage("Thread cleared.".to_string()),
        ));
        (vec![], mutations)
    }
}

fn execute_root_new(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    if let Some(result) = prepare_new_thread_transition(tui) {
        return result;
    }

    let mutations = new_thread_reset_mutations();

    (vec![UiEffect::CreateNewThreadFromProjectRoot], mutations)
}

fn prepare_new_thread_transition(tui: &TuiState) -> Option<(Vec<UiEffect>, Vec<StateMutation>)> {
    if tui.agent_state.is_running() {
        return Some((
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Cannot clear while streaming.".to_string(),
                ),
            )],
        ));
    }

    if tui.tasks.state(TaskKind::ThreadCreate).is_running() {
        return Some((vec![], vec![]));
    }

    None
}

fn new_thread_reset_mutations() -> Vec<StateMutation> {
    vec![
        StateMutation::Transcript(TranscriptMutation::Clear),
        StateMutation::Thread(ThreadMutation::ClearMessages),
        StateMutation::Thread(ThreadMutation::ResetUsage),
        StateMutation::Input(InputMutation::ClearHistory),
        StateMutation::Input(InputMutation::ClearQueue),
    ]
}

fn execute_quit(tui: &TuiState) -> Vec<UiEffect> {
    if tui.agent_state.is_running() {
        vec![UiEffect::InterruptAgent, UiEffect::Quit]
    } else {
        vec![UiEffect::Quit]
    }
}

pub fn render_command_palette(
    frame: &mut Frame,
    palette: &CommandPaletteState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{
        InputHint, InputLine, OverlayConfig, render_input_line, render_overlay, render_separator,
    };

    let commands = palette.filtered_commands();

    let max_width = area.width.saturating_sub(4);
    let palette_width = max_width.clamp(20, 80);
    // +1 for description line
    let palette_height = (commands.len() as u16 + 7).max(8);

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
            title: "Command Palette",
            border_color: Color::Magenta,
            width: palette_width,
            height: palette_height,
            hints: &hints,
        },
    );

    let filter_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, 1);
    render_input_line(
        frame,
        filter_area,
        &InputLine {
            value: &palette.filter,
            placeholder: None,
            prompt: "> ",
            prompt_color: Color::DarkGray,
            text_color: Color::Magenta,
            placeholder_color: Color::DarkGray,
            cursor_color: Color::Magenta,
        },
    );

    render_separator(frame, layout.body, 1);

    // -1 for description line
    let list_height = layout.body.height.saturating_sub(4);
    let list_area = Rect::new(
        layout.body.x,
        layout.body.y + 2,
        layout.body.width,
        list_height,
    );

    let items: Vec<ListItem> = if commands.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  No matching commands",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        // Calculate column widths
        let max_category_len = commands.iter().map(|c| c.category.len()).max().unwrap_or(0);
        let max_name_len = commands.iter().map(|c| c.name.len()).max().unwrap_or(0);

        commands
            .iter()
            .enumerate()
            .map(|(idx, cmd)| {
                let is_selected = idx == palette.selected;
                let name_style = if is_selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                let mut spans = vec![
                    // Category column (dimmed, right-aligned)
                    Span::styled(
                        format!("{:>width$}  ", cmd.category, width = max_category_len),
                        Style::default()
                            .fg(if is_selected {
                                Color::Black
                            } else {
                                Color::DarkGray
                            })
                            .add_modifier(Modifier::DIM),
                    ),
                    // Command name column
                    Span::styled(
                        format!("{:<width$}", cmd.name, width = max_name_len),
                        name_style,
                    ),
                ];

                // Shortcut column (dimmed, right side)
                if let Some(shortcut) = cmd.shortcut {
                    // Calculate remaining space for right-alignment
                    let used = max_category_len + 2 + max_name_len;
                    let available = list_area.width.saturating_sub(4) as usize;
                    let shortcut_len = shortcut.len();
                    let padding = available.saturating_sub(used + shortcut_len);

                    spans.push(Span::styled(
                        format!("{:>width$}", shortcut, width = padding + shortcut_len),
                        Style::default().fg(Color::DarkGray),
                    ));
                }

                ListItem::new(Line::from(spans))
            })
            .collect()
    };

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Magenta))
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !commands.is_empty() {
        list_state.select(Some(palette.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, layout.body, 2 + list_height);

    // Render selected command description (centered)
    let description = commands
        .get(palette.selected)
        .map(|cmd| cmd.description)
        .unwrap_or("");
    let desc_area = Rect::new(
        layout.body.x,
        layout.body.y + 3 + list_height,
        layout.body.width,
        1,
    );
    let desc_paragraph = Paragraph::new(Line::from(Span::styled(
        description,
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(desc_paragraph, desc_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_palette_state_filtered_commands_empty_filter() {
        let (state, _) = CommandPaletteState::open(
            zdx_core::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), COMMANDS.len());
        let names: Vec<&str> = filtered.iter().map(|command| command.name).collect();
        assert!(names.contains(&"thinking"));
    }

    #[test]
    fn test_palette_state_filtered_commands_with_filter() {
        let (mut state, _) = CommandPaletteState::open(
            zdx_core::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.filter = "ne".to_string();
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), 3);
        let names: Vec<&str> = filtered.iter().map(|command| command.name).collect();
        assert!(names.contains(&"new"));
        assert!(names.contains(&"root-new"));
        assert!(names.contains(&"timeline"));
    }

    #[test]
    fn test_palette_state_filtered_commands_hides_thinking_when_unsupported() {
        let (state, _) = CommandPaletteState::open(
            zdx_core::providers::ProviderKind::OpenAI,
            "openai:gpt-4.1".to_string(),
        );
        let filtered = state.filtered_commands();
        let names: Vec<&str> = filtered.iter().map(|command| command.name).collect();
        assert!(!names.contains(&"thinking"));
    }

    #[test]
    fn test_palette_state_filtered_commands_no_match() {
        let (mut state, _) = CommandPaletteState::open(
            zdx_core::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.filter = "xyz".to_string();
        let filtered = state.filtered_commands();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_palette_state_clamp_selection() {
        let (mut state, _) = CommandPaletteState::open(
            zdx_core::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.selected = COMMANDS.len() + 10;
        state.clamp_selection();
        assert_eq!(state.selected, COMMANDS.len() - 1);
    }

    #[test]
    fn test_palette_state_clamp_selection_empty_filter() {
        let (mut state, _) = CommandPaletteState::open(
            zdx_core::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.filter = "xyz".to_string();
        state.selected = 5;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }
}
