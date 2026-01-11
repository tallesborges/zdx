use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::{OverlayRequest, OverlayUpdate};
use crate::modes::tui::app::TuiState;
use crate::modes::tui::input::HandoffState;
use crate::modes::tui::shared::clipboard::Clipboard;
use crate::modes::tui::shared::commands::{COMMANDS, Command, command_available};
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{
    AuthMutation, InputMutation, StateMutation, ThreadMutation, TranscriptMutation,
};

#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    pub filter: String,
    pub selected: usize,
    pub provider: crate::providers::ProviderKind,
    pub model_id: String,
    /// Whether to insert "/" on Escape (true if opened via "/", false if via Ctrl+P).
    pub insert_slash_on_escape: bool,
}

impl CommandPaletteState {
    pub fn open(
        insert_slash_on_escape: bool,
        provider: crate::providers::ProviderKind,
        model_id: String,
    ) -> (Self, Vec<UiEffect>) {
        (
            Self {
                filter: String::new(),
                selected: 0,
                provider,
                model_id,
                insert_slash_on_escape,
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
            KeyCode::Esc => {
                let mut mutations = Vec::new();
                if self.insert_slash_on_escape {
                    mutations.push(StateMutation::Input(InputMutation::InsertChar('/')));
                }
                OverlayUpdate::close().with_mutations(mutations)
            }
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
            let (effects, mutations) = execute_rename(tui);
            (None, effects, mutations)
        }
        "model" => (Some(OverlayRequest::ModelPicker), vec![], vec![]),
        "threads" => (None, vec![UiEffect::OpenThreadPicker], vec![]),
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
    use crate::providers::oauth::{claude_cli, openai_codex};
    use crate::providers::provider_for_model;

    let mut mutations = Vec::new();
    let provider = provider_for_model(&tui.config.model);
    let result = match provider {
        crate::providers::ProviderKind::ClaudeCli => {
            claude_cli::clear_credentials().map(|had| (had, "Claude CLI"))
        }
        crate::providers::ProviderKind::OpenAICodex => {
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
    match &tui.thread.thread_log {
        Some(thread_log) => {
            let id = thread_log.id.clone();
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

fn execute_rename(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    if tui.thread.thread_log.is_none() {
        (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage("No active thread to rename.".to_string()),
            )],
        )
    } else {
        (
            vec![],
            vec![StateMutation::Input(InputMutation::SetText(
                "/rename ".to_string(),
            ))],
        )
    }
}

fn execute_handoff(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    if tui.thread.thread_log.is_none() {
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
    if tui.agent_state.is_running() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Cannot clear while streaming.".to_string(),
                ),
            )],
        );
    }

    let mut mutations = vec![
        StateMutation::Transcript(TranscriptMutation::Clear),
        StateMutation::Thread(ThreadMutation::ClearMessages),
        StateMutation::Thread(ThreadMutation::ResetUsage),
        StateMutation::Input(InputMutation::ClearHistory),
    ];

    if tui.thread.thread_log.is_some() {
        (vec![UiEffect::CreateNewThread], mutations)
    } else {
        mutations.push(StateMutation::Transcript(
            TranscriptMutation::AppendSystemMessage("Thread cleared.".to_string()),
        ));
        (vec![], mutations)
    }
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
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let commands = palette.filtered_commands();

    let palette_width = 50;
    let palette_height = (commands.len() as u16 + 6).max(7);

    let palette_area = calculate_overlay_area(area, input_top_y, palette_width, palette_height);
    render_overlay_container(frame, palette_area, "Commands", Color::Yellow);

    let inner_area = Rect::new(
        palette_area.x + 1,
        palette_area.y + 1,
        palette_area.width.saturating_sub(2),
        palette_area.height.saturating_sub(2),
    );

    let max_filter_len = inner_area.width.saturating_sub(4) as usize;
    let filter_display = if palette.filter.is_empty() {
        "/".to_string()
    } else if palette.filter.len() > max_filter_len {
        let truncated = &palette.filter[palette.filter.len() - max_filter_len..];
        format!("/…{}", truncated)
    } else {
        format!("/{}", palette.filter)
    };
    let filter_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::styled(&filter_display, Style::default().fg(Color::Yellow)),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ]);
    let filter_para = Paragraph::new(filter_line);
    let filter_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
    frame.render_widget(filter_para, filter_area);

    render_separator(frame, inner_area, 1);

    let list_height = inner_area.height.saturating_sub(4);
    let list_area = Rect::new(
        inner_area.x,
        inner_area.y + 2,
        inner_area.width,
        list_height,
    );

    let items: Vec<ListItem> = if commands.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  No matching commands",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        commands
            .iter()
            .map(|cmd| {
                let name = cmd.display_name();
                let desc = match cmd.name {
                    "login" => {
                        if palette.provider.supports_oauth() {
                            format!("Login with {} OAuth", palette.provider.label())
                        } else if let Some(env) = palette.provider.api_key_env_var() {
                            format!("Set {} to authenticate", env)
                        } else {
                            "Login".to_string()
                        }
                    }
                    "logout" => {
                        if palette.provider.supports_oauth() {
                            format!("Logout from {} OAuth", palette.provider.label())
                        } else if let Some(env) = palette.provider.api_key_env_var() {
                            format!("Unset {} to log out", env)
                        } else {
                            "Logout".to_string()
                        }
                    }
                    _ => cmd.description.to_string(),
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!("{:<16}", name),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(desc, Style::default().fg(Color::White)),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !commands.is_empty() {
        list_state.select(Some(palette.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, 2 + list_height);

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "navigate"),
            InputHint::new("Enter", "select"),
            InputHint::new("Esc", "cancel"),
        ],
        Color::Yellow,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_palette_state_filtered_commands_empty_filter() {
        let (state, _) = CommandPaletteState::open(
            true,
            crate::providers::ProviderKind::Anthropic,
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
            true,
            crate::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.filter = "ne".to_string();
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), 2);
        let names: Vec<&str> = filtered.iter().map(|command| command.name).collect();
        assert!(names.contains(&"new"));
        assert!(names.contains(&"timeline"));
    }

    #[test]
    fn test_palette_state_filtered_commands_hides_thinking_when_unsupported() {
        let (state, _) = CommandPaletteState::open(
            true,
            crate::providers::ProviderKind::OpenAI,
            "openai:gpt-4.1".to_string(),
        );
        let filtered = state.filtered_commands();
        let names: Vec<&str> = filtered.iter().map(|command| command.name).collect();
        assert!(!names.contains(&"thinking"));
    }

    #[test]
    fn test_palette_state_filtered_commands_no_match() {
        let (mut state, _) = CommandPaletteState::open(
            true,
            crate::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.filter = "xyz".to_string();
        let filtered = state.filtered_commands();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_palette_state_clamp_selection() {
        let (mut state, _) = CommandPaletteState::open(
            true,
            crate::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.selected = COMMANDS.len() + 10;
        state.clamp_selection();
        assert_eq!(state.selected, COMMANDS.len() - 1);
    }

    #[test]
    fn test_palette_state_clamp_selection_empty_filter() {
        let (mut state, _) = CommandPaletteState::open(
            true,
            crate::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        state.filter = "xyz".to_string();
        state.selected = 5;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }
}
