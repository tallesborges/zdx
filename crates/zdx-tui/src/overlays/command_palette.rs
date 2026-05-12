use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
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
use crate::input::{HandoffState, PromptBuilderState, build_fast_mode_toggle_actions};
use crate::mutations::{
    AuthMutation, InputMutation, StateMutation, ThreadMutation, TranscriptMutation,
};
use crate::state::TuiState;

/// Display category shown in the palette for custom user commands.
const COMMANDS_CATEGORY: &str = "commands";
/// Default description shown when a custom command has no frontmatter.
const CUSTOM_DEFAULT_DESCRIPTION: &str = "(custom)";
/// Divisor applied to the description fuzzy-match score so descriptions act
/// as a fallback haystack rather than competing with name/category/alias
/// matches for the top rank. Half weight is enough to keep e.g. typing
/// `clipboard` to find `copy-id`/`pwd` working without flooding the
/// short-filter ranking with weak description hits.
const DESCRIPTION_SCORE_DIVISOR: u32 = 2;

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
            PaletteEntry::Custom(_) => COMMANDS_CATEGORY,
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

    /// Extra haystacks (beyond name + category) the fuzzy matcher should score
    /// against. For built-ins this surfaces declared aliases (e.g. `q`/`exit`
    /// → `quit`, `clear` → `new`, `wt` → `worktree`). Custom commands have
    /// none today.
    fn aliases(&self) -> &[&str] {
        match self {
            PaletteEntry::Builtin(cmd) => cmd.aliases,
            PaletteEntry::Custom(_) => &[],
        }
    }

    /// Returns a fuzzy match score against `filter`, or `None` if no haystack
    /// fuzzy-matches.
    ///
    /// Haystacks (highest signal first):
    /// - name, category, and every alias score at full weight
    /// - description scores at half weight, so it acts as a fallback for
    ///   discovery (e.g. typing `clipboard` to find `copy-id`) without
    ///   competing for top rank against direct name/alias matches
    ///
    /// Uses nucleo fuzzy matching (same engine as the file picker and thread
    /// picker). An empty filter returns `Some(0)` so callers can treat
    /// "no filter" as "everything matches with the default ordering".
    fn fuzzy_score(&self, filter: &str) -> Option<u32> {
        if filter.is_empty() {
            return Some(0);
        }

        let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);

        let mut score_of = |haystack: &str| -> Option<u32> {
            let mut buf = Vec::new();
            let utf32 = Utf32Str::new(haystack, &mut buf);
            pattern.score(utf32, &mut matcher)
        };

        let primary = [self.name(), self.category()]
            .into_iter()
            .chain(self.aliases().iter().copied())
            .filter_map(&mut score_of)
            .max();

        let description = score_of(self.description()).map(|s| s / DESCRIPTION_SCORE_DIVISOR);

        match (primary, description) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(s), None) | (None, Some(s)) => Some(s),
            (None, None) => None,
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
                        // Same logic for prompt-builder: the input is being
                        // used to capture the builder intent, replacing it
                        // would silently re-route the next Enter through the
                        // builder submission path.
                        if tui.input.prompt_builder.is_active() {
                            return OverlayUpdate::close().with_mutations(vec![
                                StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(
                                    "Cancel prompt-builder before inserting a custom command."
                                        .to_string(),
                                )),
                            ]);
                        }
                        // Markdown custom commands synchronously update the
                        // input and close the palette. Executable custom
                        // commands are intentionally not supported in this
                        // iteration (see the deferred slice in
                        // `docs/plans/active/custom-commands.md`).
                        let content = cmd.content.clone();
                        let mutation = match tui.input.handoff {
                            HandoffState::Generating => {
                                return OverlayUpdate::close().with_mutations(vec![
                                    StateMutation::Transcript(
                                        TranscriptMutation::AppendSystemMessage(
                                            "Wait for handoff generation to finish before inserting a custom command."
                                                .to_string(),
                                        ),
                                    ),
                                ]);
                            }
                            HandoffState::Pending | HandoffState::Ready => {
                                InputMutation::InsertText(content)
                            }
                            HandoffState::Idle => InputMutation::SetText(content),
                        };
                        OverlayUpdate::close().with_mutations(vec![StateMutation::Input(mutation)])
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
    ///
    /// When `filter` is non-empty, entries are ranked by descending nucleo
    /// fuzzy-match score across name and category. The sort is stable, so
    /// ties preserve the underlying built-ins-before-customs ordering.
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
            let mut ranked: Vec<_> = all
                .filter_map(|entry| entry.fuzzy_score(&self.filter).map(|score| (entry, score)))
                .collect();
            ranked.sort_by(|(_, a), (_, b)| b.cmp(a));
            ranked.into_iter().map(|(entry, _)| entry).collect()
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
        "tldr" => (Some(OverlayRequest::Tldr), vec![], vec![]),
        "context" => (Some(OverlayRequest::Context), vec![], vec![]),
        "handoff" => {
            let (effects, mutations) = execute_handoff(tui);
            (None, effects, mutations)
        }
        "prompt-builder" => {
            let (effects, mutations) = execute_prompt_builder(tui);
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
    // Prompt-builder owns the input field while active; starting handoff on
    // top would leave both modal flows live at once and silently drop the
    // pending builder generation. Bail out cleanly instead.
    if tui.input.prompt_builder.is_active() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Cancel prompt-builder before starting handoff.".to_string(),
                ),
            )],
        );
    }

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

fn execute_prompt_builder(tui: &TuiState) -> (Vec<UiEffect>, Vec<StateMutation>) {
    // Handoff owns the input field while active; entering prompt-builder
    // would silently re-route Enter through prompt-builder submission and
    // strand the in-progress handoff. Bail out cleanly instead.
    if tui.input.handoff.is_active() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Cancel the handoff before starting prompt-builder.".to_string(),
                ),
            )],
        );
    }

    // Self-guard: re-entering prompt-builder while a session is already
    // pending or generating would clobber the in-flight intent (Pending) or
    // silently drop the pending generation result (Generating).
    if tui.input.prompt_builder.is_active() {
        return (
            vec![],
            vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "Prompt-builder is already active. Press Esc to cancel.".to_string(),
                ),
            )],
        );
    }

    // Preserve any text already typed in the composer so it becomes the seed
    // intent for prompt-builder. Pressing Enter then forwards it to
    // generation; the user can also edit or clear it first.
    (
        vec![],
        vec![StateMutation::Input(InputMutation::SetPromptBuilderState(
            PromptBuilderState::Pending,
        ))],
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
        let names: Vec<&str> = filtered.iter().map(PaletteEntry::name).collect();
        // Contiguous "ne" matches must surface; fuzzy scoring may also bring
        // in scattered-subsequence matches (e.g. `rename`, `open`), which is
        // fine — we only pin the strong matches here.
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
        // Repeated rare letter ensures no subsequence exists anywhere in any
        // name, alias, or category.
        state.filter = "zzzz".to_string();
        let filtered = state.filtered_entries();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_palette_filter_matches_fuzzy_subsequence() {
        // Non-contiguous subsequence ("pmt") must surface `prompt-builder`.
        // This is the core fuzzy-matching contract the user asked for.
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        state.filter = "pmt".to_string();
        let entries = state.filtered_entries();
        let names: Vec<&str> = entries.iter().map(PaletteEntry::name).collect();
        assert!(
            names.contains(&"prompt-builder"),
            "fuzzy filter 'pmt' should match 'prompt-builder'; got {names:?}"
        );
    }

    #[test]
    fn test_palette_filter_ranks_better_matches_first() {
        // Contiguous matches must outrank scattered subsequence matches.
        // `new` contains "ne" verbatim and should rank above `rename`.
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        state.filter = "ne".to_string();
        let entries = state.filtered_entries();
        let names: Vec<&str> = entries.iter().map(PaletteEntry::name).collect();
        let pos = |n: &str| names.iter().position(|x| *x == n);
        let new_pos = pos("new").expect("'new' present");
        if let Some(rename_pos) = pos("rename") {
            assert!(
                new_pos < rename_pos,
                "'new' must rank above 'rename' for filter 'ne'; got {names:?}"
            );
        }
    }

    #[test]
    fn test_palette_filter_matches_aliases() {
        // Aliases must remain fuzzy-matchable (`exit` and `q` → `quit`,
        // `clear` → `new`, `wt` → `worktree`). This is a regression guard:
        // moving to fuzzy scoring must not drop alias matching.
        let cases = [("exit", "quit"), ("clear", "new"), ("wt", "worktree")];
        for (filter, expected) in cases {
            let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
            state.filter = filter.to_string();
            let entries = state.filtered_entries();
            let names: Vec<&str> = entries.iter().map(PaletteEntry::name).collect();
            assert!(
                names.contains(&expected),
                "alias filter {filter:?} should surface {expected:?}; got {names:?}"
            );
        }
    }

    #[test]
    fn test_palette_filter_matches_description_as_fallback() {
        // Words that only appear in descriptions must still surface their
        // commands (e.g. `clipboard` → `copy-id` / `pwd`). Description hits
        // are weighted lower than name/category/alias hits, but they still
        // count as a match.
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        state.filter = "clipboard".to_string();
        let entries = state.filtered_entries();
        let names: Vec<&str> = entries.iter().map(PaletteEntry::name).collect();
        assert!(
            names.contains(&"copy-id"),
            "description filter 'clipboard' should surface 'copy-id'; got {names:?}"
        );
        assert!(
            names.contains(&"pwd"),
            "description filter 'clipboard' should surface 'pwd'; got {names:?}"
        );
    }

    #[test]
    fn test_palette_description_does_not_outrank_name_match() {
        // Description weight must stay below name weight: filtering `new`
        // should put the literal `new` command above any command whose
        // description merely mentions "new" (e.g. `btw`, `new-tab`, `tabs`).
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), Vec::new());
        state.filter = "new".to_string();
        let entries = state.filtered_entries();
        let names: Vec<&str> = entries.iter().map(PaletteEntry::name).collect();
        assert_eq!(
            names.first().copied(),
            Some("new"),
            "literal name match must outrank description matches; got {names:?}"
        );
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
        state.filter = "zzzz".to_string();
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
        assert_eq!(review.category(), COMMANDS_CATEGORY);
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
    fn test_palette_filter_matches_commands_category() {
        let customs = vec![
            sample_custom("review", None),
            sample_custom("explain", None),
        ];
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), customs);

        // Filtering by any commands-category alias must surface every custom
        // command. (Built-ins whose name happens to contain the alias — e.g.
        // `commands-refresh` — may also appear; that's fine.)
        for filter in ["commands", "command", "cmd", "co"] {
            state.filter = filter.to_string();
            let entries = state.filtered_entries();
            let names: Vec<&str> = entries
                .iter()
                .filter(|e| matches!(e, PaletteEntry::Custom(_)))
                .map(PaletteEntry::name)
                .collect();
            assert!(
                names.contains(&"review") && names.contains(&"explain"),
                "filter {filter:?} did not surface all custom commands; got {names:?}"
            );
        }
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
    fn test_palette_custom_selection_inserts_during_handoff() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::input::HandoffState;
        use crate::mutations::InputMutation;
        use crate::overlays::OverlayTransition;
        use crate::state::AppState;

        let mut custom = sample_custom("review", Some("Review code"));
        custom.content = "Review the handoff goal.".to_string();
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), vec![custom]);
        state.filter = "review".to_string();
        state.clamp_selection();

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.handoff = HandoffState::Pending;

        let update = state.handle_key(&app.tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Handoff owns the composer while pending, so custom commands insert
        // into the current handoff goal instead of replacing it or blocking.
        assert!(matches!(update.transition, OverlayTransition::Close));
        assert!(update.effects.is_empty());
        assert_eq!(update.mutations.len(), 1);
        match &update.mutations[0] {
            StateMutation::Input(InputMutation::InsertText(text)) => {
                assert_eq!(text, "Review the handoff goal.");
            }
            other => panic!("expected InsertText mutation, got {other:?}"),
        }
    }

    #[test]
    fn test_palette_custom_selection_inserts_during_handoff_ready() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::input::HandoffState;
        use crate::mutations::InputMutation;
        use crate::overlays::OverlayTransition;
        use crate::state::AppState;

        let mut custom = sample_custom("review", Some("Review code"));
        custom.content = "Review the ready handoff prompt.".to_string();
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), vec![custom]);
        state.filter = "review".to_string();
        state.clamp_selection();

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.handoff = HandoffState::Ready;

        let update = state.handle_key(&app.tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(update.transition, OverlayTransition::Close));
        assert!(update.effects.is_empty());
        assert_eq!(update.mutations.len(), 1);
        match &update.mutations[0] {
            StateMutation::Input(InputMutation::InsertText(text)) => {
                assert_eq!(text, "Review the ready handoff prompt.");
            }
            other => panic!("expected InsertText mutation, got {other:?}"),
        }
    }

    #[test]
    fn test_palette_custom_selection_blocked_during_handoff_generation() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::input::HandoffState;
        use crate::mutations::InputMutation;
        use crate::overlays::OverlayTransition;
        use crate::state::AppState;

        let custom = sample_custom("review", Some("Review code"));
        let mut state = CommandPaletteState::open("claude-haiku-4-5".to_string(), vec![custom]);
        state.filter = "review".to_string();
        state.clamp_selection();

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.handoff = HandoffState::Generating;

        let update = state.handle_key(&app.tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(update.transition, OverlayTransition::Close));
        assert!(update.effects.is_empty());
        assert!(update.mutations.iter().any(|m| matches!(
            m,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(text))
                if text == "Wait for handoff generation to finish before inserting a custom command."
        )));
        assert!(!update.mutations.iter().any(|m| matches!(
            m,
            StateMutation::Input(InputMutation::SetText(_) | InputMutation::InsertText(_))
        )));
    }

    #[test]
    fn test_palette_prompt_builder_command_arms_pending_state() {
        use crate::mutations::InputMutation;
        use crate::state::AppState;

        let config = zdx_engine::config::Config::default();
        let app = AppState::new(config, PathBuf::new(), None, None);

        let (overlay, effects, mutations) = execute_command(&app.tui, "prompt-builder");

        // Selecting `/prompt-builder` does not auto-open another overlay or
        // emit side-effects; it just arms the pending state. Any existing
        // composer text is preserved so it becomes the seed intent.
        assert!(overlay.is_none());
        assert!(effects.is_empty());

        let armed_pending = mutations.iter().any(|m| {
            matches!(
                m,
                StateMutation::Input(InputMutation::SetPromptBuilderState(
                    PromptBuilderState::Pending
                ))
            )
        });
        assert!(armed_pending, "prompt-builder must transition to Pending");

        let cleared_input = mutations
            .iter()
            .any(|m| matches!(m, StateMutation::Input(InputMutation::Clear)));
        assert!(
            !cleared_input,
            "prompt-builder must preserve existing composer text as seed intent"
        );
    }

    #[test]
    fn test_palette_prompt_builder_blocked_during_handoff() {
        use crate::input::HandoffState;
        use crate::state::AppState;

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.handoff = HandoffState::Pending;

        let (overlay, effects, mutations) = execute_command(&app.tui, "prompt-builder");

        assert!(overlay.is_none());
        assert!(effects.is_empty());
        // No input mutation, only an advisory.
        assert!(!mutations.iter().any(|m| matches!(
            m,
            StateMutation::Input(crate::mutations::InputMutation::SetPromptBuilderState(_))
        )));
        assert!(mutations.iter().any(|m| matches!(
            m,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(text))
                if text.to_lowercase().contains("handoff")
        )));
    }

    #[test]
    fn test_palette_handoff_blocked_during_prompt_builder() {
        use crate::state::AppState;

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.prompt_builder = PromptBuilderState::Pending;
        // Note: no active thread is required to exercise the guard — the
        // prompt-builder check runs before the active-thread check, so an
        // attempt to start handoff while prompt-builder owns the composer
        // is rejected with a builder-specific advisory regardless.

        let (overlay, effects, mutations) = execute_command(&app.tui, "handoff");

        assert!(overlay.is_none());
        assert!(effects.is_empty());
        assert!(!mutations.iter().any(|m| matches!(
            m,
            StateMutation::Input(crate::mutations::InputMutation::SetHandoffState(_))
        )));
        assert!(mutations.iter().any(|m| matches!(
            m,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(text))
                if text.to_lowercase().contains("prompt-builder")
        )));
    }

    #[test]
    fn test_palette_prompt_builder_blocked_when_already_active() {
        use crate::state::AppState;

        let config = zdx_engine::config::Config::default();
        let mut app = AppState::new(config, PathBuf::new(), None, None);
        app.tui.input.prompt_builder = PromptBuilderState::Generating {
            intent: "in-flight".to_string(),
        };

        let (overlay, effects, mutations) = execute_command(&app.tui, "prompt-builder");

        assert!(overlay.is_none());
        assert!(effects.is_empty());
        // Self-guard: must not re-arm pending while a session is in flight.
        assert!(!mutations.iter().any(|m| matches!(
            m,
            StateMutation::Input(crate::mutations::InputMutation::SetPromptBuilderState(_))
        )));
        assert!(mutations.iter().any(|m| matches!(
            m,
            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(text))
                if text.to_lowercase().contains("prompt-builder")
        )));
    }
}
