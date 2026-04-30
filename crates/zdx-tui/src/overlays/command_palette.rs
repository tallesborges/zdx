use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use zdx_engine::custom_commands::CustomCommand;

use super::{OverlayRequest, OverlayUpdate};
use crate::common::TaskKind;
use crate::common::clipboard::Clipboard;
use crate::common::commands::{COMMANDS, Command, command_available};
use crate::effects::UiEffect;
use crate::input::{HandoffState, build_fast_mode_toggle_actions};
use crate::mutations::{
    AuthMutation, InputMutation, StateMutation, ThreadMutation, TranscriptMutation,
};
use crate::state::TuiState;

/// Display category shown in the palette for custom user commands.
const CUSTOM_CATEGORY: &str = "custom";
/// Default description shown when a custom command has no frontmatter.
const CUSTOM_DEFAULT_DESCRIPTION: &str = "(custom)";

/// One row in the command palette: either a built-in or a user-defined custom
/// command. The custom variant borrows from `CommandPaletteState::custom_commands`.
#[derive(Debug, Clone, Copy)]
pub enum PaletteEntry<'a> {
    Builtin(&'static Command),
    Custom(&'a CustomCommand),
}

impl PaletteEntry<'_> {
    fn name(&self) -> &str {
        match self {
            PaletteEntry::Builtin(cmd) => cmd.name,
            PaletteEntry::Custom(cmd) => cmd.name.as_str(),
        }
    }

    fn category(&self) -> &str {
        match self {
            PaletteEntry::Builtin(cmd) => cmd.category,
            PaletteEntry::Custom(_) => CUSTOM_CATEGORY,
        }
    }

    fn description(&self) -> &str {
        match self {
            PaletteEntry::Builtin(cmd) => cmd.description,
            PaletteEntry::Custom(cmd) => cmd
                .description
                .as_deref()
                .unwrap_or(CUSTOM_DEFAULT_DESCRIPTION),
        }
    }

    fn shortcut(&self) -> Option<&str> {
        match self {
            PaletteEntry::Builtin(cmd) => cmd.shortcut,
            PaletteEntry::Custom(_) => None,
        }
    }

    fn matches_filter(&self, filter: &str) -> bool {
        match self {
            PaletteEntry::Builtin(cmd) => cmd.matches(filter),
            PaletteEntry::Custom(cmd) => {
                let filter_lower = filter.to_lowercase();
                cmd.name.to_lowercase().contains(&filter_lower)
                    || CUSTOM_CATEGORY.contains(&filter_lower)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    pub filter: String,
    pub selected: usize,
    pub model_id: String,
    pub custom_commands: Vec<CustomCommand>,
}

impl CommandPaletteState {
    pub fn open(model_id: String, custom_commands: Vec<CustomCommand>) -> Self {
        Self {
            filter: String::new(),
            selected: 0,
            model_id,
            custom_commands,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_command_palette(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => OverlayUpdate::close(),
            KeyCode::Char('c') if ctrl => OverlayUpdate::close(),
            KeyCode::Up => {
                let count = self.filtered_entries().len();
                if count > 0 && self.selected > 0 {
                    self.selected -= 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down => {
                let count = self.filtered_entries().len();
                if count > 0 && self.selected < count - 1 {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter | KeyCode::Tab => {
                let entries = self.filtered_entries();
                let Some(entry) = entries.get(self.selected).copied() else {
                    return OverlayUpdate::close();
                };
                match entry {
                    PaletteEntry::Builtin(cmd) => {
                        let (open_overlay, effects, mutations) = execute_command(tui, cmd.name);
                        let update = match open_overlay {
                            Some(request) => OverlayUpdate::open(request),
                            None => OverlayUpdate::close(),
                        };
                        update.with_ui_effects(effects).with_mutations(mutations)
                    }
                    PaletteEntry::Custom(cmd) => {
                        // Handoff owns the input field while active; replacing
                        // it would silently re-route Enter through handoff
                        // submission. Bail out cleanly instead.
                        if tui.input.handoff.is_active() {
                            return OverlayUpdate::close().with_mutations(vec![
                                StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(
                                    "Cancel the handoff before inserting a custom command."
                                        .to_string(),
                                )),
                            ]);
                        }
                        // Markdown custom commands replace the input contents
                        // synchronously and close the palette. Executable
                        // custom commands are intentionally not supported in
                        // this iteration (see the deferred slice in
                        // `docs/plans/active/custom-commands.md`).
                        let content = cmd.content.clone();
                        OverlayUpdate::close().with_mutations(vec![StateMutation::Input(
                            InputMutation::SetText(content),
                        )])
                    }
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

    /// Returns the merged, filtered list of palette entries (built-ins first,
    /// then custom commands). Built-in availability is gated on the active
    /// model (e.g. `thinking` only for reasoning models).
    pub fn filtered_entries(&self) -> Vec<PaletteEntry<'_>> {
        let builtins = COMMANDS
            .iter()
            .filter(|cmd| command_available(cmd, &self.model_id))
            .map(PaletteEntry::Builtin);
        let customs = self.custom_commands.iter().map(PaletteEntry::Custom);
        let all = builtins.chain(customs);

        if self.filter.is_empty() {
            all.collect()
        } else {
            all.filter(|entry| entry.matches_filter(&self.filter))
                .collect()
        }
    }

    pub fn clamp_selection(&mut self) {
        let count = self.filtered_entries().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(count - 1);
        }
    }
}

#[allow(clippy::too_many_lines)]
fn execute_command(
    tui: &TuiState,
    cmd_name: &str,
) -> (Option<OverlayRequest>, Vec<UiEffect>, Vec<StateMutation>) {
    match cmd_name {
        "btw" => {
            if tui.thread.thread_handle.is_none() {
                (
                    None,
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "BTW requires an active thread.".to_string(),
                        ),
                    )],
                )
            } else {
                (Some(OverlayRequest::Btw), vec![], vec![])
            }
        }
        "close-tab" => (None, vec![UiEffect::CloseCurrentTab], vec![]),
        "commands-refresh" => (None, vec![UiEffect::ReloadCustomCommands], vec![]),
        "config" => (None, vec![UiEffect::OpenConfig], vec![]),
        "debug" => (None, vec![], vec![StateMutation::ToggleDebugStatus]),
        "fast" => match build_fast_mode_toggle_actions(&tui.config, &tui.config.model) {
            Ok((effects, mutations)) => (None, effects, mutations),
            Err(message) => (
                None,
                vec![],
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(message.to_string()),
                )],
            ),
        },
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
        "tabs" => (None, vec![UiEffect::CycleTab], vec![]),
        "threads" => {
            if tui.tasks.state(TaskKind::ThreadList).is_running() {
                return (None, vec![], vec![]);
            }
            if tui.agent_state.is_running() {
                return (
                    None,
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Stop the current task first.".to_string(),
                        ),
                    )],
                );
            }
            (
                None,
                vec![UiEffect::OpenThreadPicker {
                    mode: crate::overlays::ThreadPickerMode::Switch,
                }],
                vec![],
            )
        }
        "pwd" => {
            let path = tui.agent_opts.root.display().to_string();
            match Clipboard::copy(&path) {
                Ok(()) => (
                    None,
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(format!("Root: {path} (copied)")),
                    )],
                ),
                Err(e) => (
                    None,
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(format!(
                            "Root: {path} (copy failed: {e})"
                        )),
                    )],
                ),
            }
        }
        "open" => (None, vec![UiEffect::OpenTerminal], vec![]),
        "worktree-remove" => {
            if tui.tasks.state(TaskKind::ThreadWorktree).is_running() {
                (None, vec![], vec![])
            } else {
                (None, vec![UiEffect::RemoveWorktree], vec![])
            }
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
        "new-tab" => (Some(OverlayRequest::NewTab), vec![], vec![]),
        "quit" => (None, execute_quit(tui), vec![]),
        _ => (None, vec![], vec![]),
    }
}

fn execute_logout(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    use zdx_engine::providers::oauth::{claude_cli, openai_codex};
    use zdx_engine::providers::provider_for_model;

    let mut mutations = Vec::new();
    let provider = provider_for_model(&tui.config.model);
    let result = match provider {
        zdx_engine::providers::ProviderKind::ClaudeCli => {
            claude_cli::clear_credentials().map(|had| (had, "Claude CLI"))
        }
        zdx_engine::providers::ProviderKind::OpenAICodex => {
            openai_codex::clear_credentials().map(|had| (had, "OpenAI Codex"))
        }
        _ => {
            let message = provider.api_key_env_var().map_or_else(
                || "No OAuth credentials to clear.".to_string(),
                |env| format!("Unset {env} to log out."),
            );
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
                TranscriptMutation::AppendSystemMessage(format!("Logged out from {label} OAuth.")),
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
                TranscriptMutation::AppendSystemMessage(format!("Logout failed: {e}")),
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
                        TranscriptMutation::AppendSystemMessage(format!("Thread ID copied: {id}")),
                    )],
                ),
                Err(e) => (
                    vec![],
                    vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(format!(
                            "Failed to copy thread ID: {e}"
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
        StateMutation::Thread(ThreadMutation::SetThread(None)),
        StateMutation::Thread(ThreadMutation::ResetUsage),
        StateMutation::Input(InputMutation::ClearHistory),
        StateMutation::Input(InputMutation::ClearQueue),
        StateMutation::Input(InputMutation::ResetImageCounter),
        StateMutation::SetActiveThreadOverrides {
            model_override: None,
            thinking_override: None,
        },
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

    let entries = palette.filtered_entries();

    let max_width = area.width.saturating_sub(4);
    let palette_width = max_width.clamp(20, 80);
    // +1 for description line
    let palette_height = (entries.len() as u16 + 7).max(8);

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

    let items = build_command_items(&entries, palette.selected, list_area.width);

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Magenta))
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !entries.is_empty() {
        list_state.select(Some(palette.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, layout.body, 2 + list_height);

    render_command_description(frame, &entries, palette.selected, layout.body, list_height);
}

fn build_command_items(
    entries: &[PaletteEntry<'_>],
    selected: usize,
    list_width: u16,
) -> Vec<ListItem<'static>> {
    if entries.is_empty() {
        return vec![ListItem::new(Line::from(Span::styled(
            "  No matching commands",
            Style::default().fg(Color::DarkGray),
        )))];
    }

    let max_category_len = entries
        .iter()
        .map(|e| e.category().len())
        .max()
        .unwrap_or(0);
    let max_name_len = entries.iter().map(|e| e.name().len()).max().unwrap_or(0);
    entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let is_selected = idx == selected;
            let mut spans = vec![
                Span::styled(
                    format!("{:>width$}  ", entry.category(), width = max_category_len),
                    Style::default()
                        .fg(if is_selected {
                            Color::Black
                        } else {
                            Color::DarkGray
                        })
                        .add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    format!("{:<width$}", entry.name(), width = max_name_len),
                    command_name_style(is_selected),
                ),
            ];

            if let Some(shortcut) = entry.shortcut() {
                let used = max_category_len + 2 + max_name_len;
                let available = list_width.saturating_sub(4) as usize;
                let padding = available.saturating_sub(used + shortcut.len());
                spans.push(Span::styled(
                    format!("{:>width$}", shortcut, width = padding + shortcut.len()),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect()
}

fn command_name_style(is_selected: bool) -> Style {
    if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_command_description(
    frame: &mut Frame,
    entries: &[PaletteEntry<'_>],
    selected: usize,
    body: Rect,
    list_height: u16,
) {
    let description = entries.get(selected).map_or("", PaletteEntry::description);
    let desc_area = Rect::new(body.x, body.y + 3 + list_height, body.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            description,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center),
        desc_area,
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use zdx_engine::custom_commands::{CustomCommand, CustomCommandSource};

    use super::*;
    use crate::mutations::ThreadMutation;

    fn sample_custom(name: &str, description: Option<&str>) -> CustomCommand {
        CustomCommand {
            name: name.to_string(),
            description: description.map(str::to_string),
            source: CustomCommandSource::Project,
            path: PathBuf::from(format!(".zdx/commands/{name}.md")),
            content: format!("custom content for {name}"),
            is_executable: false,
        }
    }

    #[test]
    fn test_palette_state_filtered_commands_empty_filter() {
        let state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        let filtered = state.filtered_entries();
        // fast is hidden for non-OpenAI providers
        assert!(!filtered.iter().any(|e| e.name() == "fast"));
        let names: Vec<&str> = filtered.iter().map(PaletteEntry::name).collect();
        assert!(names.contains(&"thinking"));
    }

    #[test]
    fn test_palette_state_filtered_commands_with_filter() {
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        state.filter = "ne".to_string();
        let filtered = state.filtered_entries();
        assert_eq!(filtered.len(), 4);
        let names: Vec<&str> = filtered.iter().map(PaletteEntry::name).collect();
        assert!(names.contains(&"new"));
        assert!(names.contains(&"new-tab"));
        assert!(names.contains(&"root-new"));
        assert!(names.contains(&"timeline"));
    }

    #[test]
    fn test_palette_state_filtered_commands_respects_reasoning_support() {
        let model_id = "openai:gpt-4.1";
        let supports_reasoning = zdx_engine::models::model_supports_reasoning(model_id);
        let state = CommandPaletteState::open(model_id.to_string(), Vec::new());
        let filtered = state.filtered_entries();
        let names: Vec<&str> = filtered.iter().map(PaletteEntry::name).collect();
        assert_eq!(names.contains(&"thinking"), supports_reasoning);
    }

    #[test]
    fn test_palette_state_filtered_commands_no_match() {
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        state.filter = "xyz".to_string();
        let filtered = state.filtered_entries();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_palette_state_clamp_selection() {
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        let available = state.filtered_entries().len();
        state.selected = available + 10;
        state.clamp_selection();
        assert_eq!(state.selected, available - 1);
    }

    #[test]
    fn test_palette_state_clamp_selection_empty_filter() {
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        state.filter = "xyz".to_string();
        state.selected = 5;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_new_thread_reset_mutations_clear_active_thread_identity() {
        let mutations = new_thread_reset_mutations();

        assert!(mutations.iter().any(|mutation| matches!(
            mutation,
            StateMutation::Thread(ThreadMutation::SetThread(None))
        )));
        assert!(mutations.iter().any(|mutation| matches!(
            mutation,
            StateMutation::SetActiveThreadOverrides {
                model_override: None,
                thinking_override: None,
            }
        )));
    }

    #[test]
    fn test_palette_includes_custom_commands_after_builtins() {
        let customs = vec![
            sample_custom("review", Some("Review code for bugs")),
            sample_custom("explain", None),
        ];
        let state = CommandPaletteState::open("claude-haiku-4-5".to_string(), customs);
        let entries = state.filtered_entries();

        // Built-ins come first.
        assert!(matches!(entries.first(), Some(PaletteEntry::Builtin(_))));
        // Both custom commands present.
        let names: Vec<&str> = entries.iter().map(PaletteEntry::name).collect();
        assert!(names.contains(&"review"));
        assert!(names.contains(&"explain"));

        // Custom entries report the dedicated category and surface their
        // descriptions (or the default placeholder when missing).
        let review = entries
            .iter()
            .find(|e| e.name() == "review")
            .expect("review entry");
        assert!(matches!(review, PaletteEntry::Custom(_)));
        assert_eq!(review.category(), CUSTOM_CATEGORY);
        assert_eq!(review.description(), "Review code for bugs");

        let explain = entries
            .iter()
            .find(|e| e.name() == "explain")
            .expect("explain entry");
        assert_eq!(explain.description(), CUSTOM_DEFAULT_DESCRIPTION);
    }

    #[test]
    fn test_palette_filter_matches_custom_command_name() {
        let customs = vec![sample_custom("review", Some("Review code"))];
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), customs);
        state.filter = "rev".to_string();

        let entries = state.filtered_entries();
        let names: Vec<&str> = entries.iter().map(PaletteEntry::name).collect();
        assert!(names.contains(&"review"));
    }

    #[test]
    fn test_palette_filter_matches_custom_category() {
        let customs = vec![
            sample_custom("review", None),
            sample_custom("explain", None),
        ];
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), customs);
        state.filter = CUSTOM_CATEGORY.to_string();

        let entries = state.filtered_entries();
        // Filtering by "custom" should surface every custom command but no
        // built-in (which lives in a different category).
        assert!(entries.iter().all(|e| matches!(e, PaletteEntry::Custom(_))));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_palette_filter_no_match_includes_no_custom() {
        let customs = vec![sample_custom("review", None)];
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), customs);
        state.filter = "zzzzz-no-match".to_string();
        assert!(state.filtered_entries().is_empty());
    }

    #[test]
    fn test_palette_custom_markdown_selection_inserts_content() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::mutations::InputMutation;
        use crate::overlays::OverlayTransition;
        use crate::state::AppState;

        let mut custom = sample_custom("review", Some("Review code"));
        custom.content = "Review the current diff for bugs.".to_string();
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), vec![custom]);
        state.filter = "review".to_string();
        state.clamp_selection();
        let entries = state.filtered_entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], PaletteEntry::Custom(_)));

        let config = zdx_engine::config::Config::default();
        let app = AppState::new(config, PathBuf::new(), None, None);

        let update = state.handle_key(&app.tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Slice 3 contract: selecting a Markdown custom command closes the
        // palette and emits a single `InputMutation::SetText` carrying the
        // command's body so the input field is replaced with the prompt.
        assert!(matches!(update.transition, OverlayTransition::Close));
        assert!(update.effects.is_empty());
        assert_eq!(update.mutations.len(), 1);
        match &update.mutations[0] {
            StateMutation::Input(InputMutation::SetText(text)) => {
                assert_eq!(text, "Review the current diff for bugs.");
            }
            other => panic!("expected SetText mutation, got {other:?}"),
        }
    }

    #[test]
    fn test_palette_custom_selection_blocked_during_handoff() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::input::HandoffState;
        use crate::overlays::OverlayTransition;
        use crate::state::AppState;

        let custom = sample_custom("review", Some("Review code"));
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), vec![custom]);
        state.filter = "review".to_string();
        state.clamp_selection();

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.handoff = HandoffState::Pending;

        let update = state.handle_key(&app.tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Palette still closes, but no input mutation is emitted; instead the
        // user gets a system-message advisory and handoff state is left
        // intact so they can finish or cancel it deliberately.
        assert!(matches!(update.transition, OverlayTransition::Close));
        assert!(update.effects.is_empty());
        assert!(update.mutations.iter().any(|m| matches!(
            m,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(text))
                if text.contains("handoff")
        )));
        assert!(!update.mutations.iter().any(|m| matches!(
            m,
            StateMutation::Input(crate::mutations::InputMutation::SetText(_))
        )));
    }
}
