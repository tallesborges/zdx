use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::OverlayAction;
use crate::ui::chat::commands::COMMANDS;
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;

#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    pub filter: String,
    pub selected: usize,
    /// Whether to insert "/" on Escape (true if opened via "/", false if via Ctrl+P).
    pub insert_slash_on_escape: bool,
}

impl CommandPaletteState {
    pub fn open(insert_slash_on_escape: bool) -> (Self, Vec<UiEffect>) {
        (
            Self {
                filter: String::new(),
                selected: 0,
                insert_slash_on_escape,
            },
            vec![],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_command_palette(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => {
                if self.insert_slash_on_escape {
                    tui.input.textarea.insert_char('/');
                }
                Some(OverlayAction::close())
            }
            KeyCode::Char('c') if ctrl => Some(OverlayAction::close()),
            KeyCode::Up => {
                let count = self.filtered_commands().len();
                if count > 0 && self.selected > 0 {
                    self.selected -= 1;
                }
                None
            }
            KeyCode::Down => {
                let count = self.filtered_commands().len();
                if count > 0 && self.selected < count - 1 {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter | KeyCode::Tab => {
                if let Some(cmd_name) = self.get_selected_command_name() {
                    let effects = execute_command(tui, cmd_name);
                    Some(OverlayAction::close_with(effects))
                } else {
                    Some(OverlayAction::close())
                }
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.clamp_selection();
                None
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.clamp_selection();
                None
            }
            _ => None,
        }
    }

    pub fn filtered_commands(&self) -> Vec<&'static crate::ui::chat::commands::Command> {
        if self.filter.is_empty() {
            COMMANDS.iter().collect()
        } else {
            COMMANDS
                .iter()
                .filter(|cmd| cmd.matches(&self.filter))
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

fn execute_command(tui: &mut TuiState, cmd_name: &str) -> Vec<UiEffect> {
    use crate::ui::transcript::HistoryCell;

    match cmd_name {
        "config" => vec![UiEffect::OpenConfig],
        "login" => vec![UiEffect::OpenLogin],
        "logout" => {
            use crate::providers::oauth::anthropic;

            match anthropic::clear_credentials() {
                Ok(true) => {
                    tui.refresh_auth_type();
                    tui.transcript
                        .cells
                        .push(HistoryCell::system("Logged out from Anthropic OAuth."));
                }
                Ok(false) => {
                    tui.transcript
                        .cells
                        .push(HistoryCell::system("No OAuth credentials to clear."));
                }
                Err(e) => {
                    tui.transcript
                        .cells
                        .push(HistoryCell::system(format!("Logout failed: {}", e)));
                }
            }
            vec![]
        }
        "rename" => {
            if tui.conversation.session.is_none() {
                tui.transcript
                    .cells
                    .push(HistoryCell::system("No active session to rename."));
                vec![]
            } else {
                tui.input.set_text("/rename ");
                vec![]
            }
        }
        "model" => vec![UiEffect::OpenModelPicker],
        "sessions" => vec![UiEffect::OpenSessionPicker],
        "thinking" => vec![UiEffect::OpenThinkingPicker],
        "handoff" => execute_handoff(tui),
        "new" => execute_new(tui),
        "quit" => execute_quit(tui),
        _ => vec![],
    }
}

fn execute_handoff(tui: &mut TuiState) -> Vec<UiEffect> {
    use crate::ui::chat::state::HandoffState;
    use crate::ui::transcript::HistoryCell;

    if tui.conversation.session.is_none() {
        tui.transcript
            .cells
            .push(HistoryCell::system("Handoff requires an active session."));
        return vec![];
    }

    tui.input.handoff.cancel();
    tui.clear_input();
    tui.input.handoff = HandoffState::Pending;
    vec![]
}

fn execute_new(tui: &mut TuiState) -> Vec<UiEffect> {
    use crate::ui::transcript::HistoryCell;

    if tui.agent_state.is_running() {
        tui.transcript
            .cells
            .push(HistoryCell::system("Cannot clear while streaming."));
        return vec![];
    }

    tui.reset_conversation();

    if tui.conversation.session.is_some() {
        vec![UiEffect::CreateNewSession]
    } else {
        tui.transcript
            .cells
            .push(HistoryCell::system("Conversation cleared."));
        vec![]
    }
}

fn execute_quit(tui: &mut TuiState) -> Vec<UiEffect> {
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
    let commands = palette.filtered_commands();

    let palette_width = 50.min(area.width.saturating_sub(4));
    let palette_height = (commands.len() as u16 + 6).max(7).min(area.height / 2);

    let available_height = input_top_y;
    let palette_x = (area.width.saturating_sub(palette_width)) / 2;
    let palette_y = (available_height.saturating_sub(palette_height)) / 2;

    let palette_area = Rect::new(palette_x, palette_y, palette_width, palette_height);

    frame.render_widget(Clear, palette_area);

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Commands ")
        .title_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(outer_block, palette_area);

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

    let separator = "─".repeat(inner_area.width as usize);
    let separator_line = Paragraph::new(Line::from(Span::styled(
        &separator,
        Style::default().fg(Color::DarkGray),
    )));
    let separator_area = Rect::new(inner_area.x, inner_area.y + 1, inner_area.width, 1);
    frame.render_widget(separator_line, separator_area);

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
                let desc = cmd.description;
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

    let bottom_sep_y = inner_area.y + 2 + list_height;
    if bottom_sep_y < inner_area.y + inner_area.height {
        let bottom_separator_area = Rect::new(inner_area.x, bottom_sep_y, inner_area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                &separator,
                Style::default().fg(Color::DarkGray),
            ))),
            bottom_separator_area,
        );
    }

    let hints_y = inner_area.y + inner_area.height.saturating_sub(1);
    let hints_area = Rect::new(inner_area.x, hints_y, inner_area.width, 1);
    let hints_line = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Enter", Style::default().fg(Color::Yellow)),
        Span::styled(" select ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_palette_state_filtered_commands_empty_filter() {
        let (state, _) = CommandPaletteState::open(true);
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), COMMANDS.len());
    }

    #[test]
    fn test_palette_state_filtered_commands_with_filter() {
        let (mut state, _) = CommandPaletteState::open(true);
        state.filter = "ne".to_string();
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "new");
    }

    #[test]
    fn test_palette_state_filtered_commands_no_match() {
        let (mut state, _) = CommandPaletteState::open(true);
        state.filter = "xyz".to_string();
        let filtered = state.filtered_commands();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_palette_state_clamp_selection() {
        let (mut state, _) = CommandPaletteState::open(true);
        state.selected = 10;
        state.clamp_selection();
        assert_eq!(state.selected, COMMANDS.len() - 1);
    }

    #[test]
    fn test_palette_state_clamp_selection_empty_filter() {
        let (mut state, _) = CommandPaletteState::open(true);
        state.filter = "xyz".to_string();
        state.selected = 5;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }
}
