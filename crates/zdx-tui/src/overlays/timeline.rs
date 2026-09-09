#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::match_same_arms
)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use zdx_engine::providers::ChatMessage;

use super::{GPrefix, OverlayUpdate};
use crate::common::{TaskKind, sanitize_for_display, truncate_with_ellipsis};
use crate::effects::UiEffect;
use crate::mutations::{StateMutation, TranscriptMutation};
use crate::state::TuiState;
use crate::transcript::{HistoryCell, ScrollMode, ScrollState};

const MAX_VISIBLE_TURNS: usize = 12;
const OVERLAY_WIDTH: u16 = 70;

#[derive(Debug, Clone, Copy)]
pub enum TimelineRole {
    User,
    Assistant,
}

impl TimelineRole {
    fn badge(self) -> &'static str {
        match self {
            TimelineRole::User => "U",
            TimelineRole::Assistant => "A",
        }
    }

    fn color(self) -> Color {
        match self {
            TimelineRole::User => Color::Cyan,
            TimelineRole::Assistant => Color::Magenta,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub cell_index: usize,
    pub role: TimelineRole,
    pub preview: String,
}

#[derive(Debug, Clone)]
pub struct TimelineState {
    pub entries: Vec<TimelineEntry>,
    pub selected: usize,
    pub offset: usize,
    initial_scroll: ScrollMode,
    /// Vim `g` prefix (`gg` ⇒ top).
    g_prefix: GPrefix,
}

impl TimelineState {
    pub fn open(
        cells: &[HistoryCell],
        scroll: &ScrollState,
        initial_scroll: ScrollMode,
    ) -> (Self, Vec<UiEffect>, Vec<StateMutation>) {
        let entries = build_entries(cells);
        let initial_offset = entries
            .first()
            .and_then(|entry| scroll.cell_start_line(entry.cell_index));
        let mut mutations = Vec::new();
        if let Some(offset) = initial_offset {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::SetScrollOffset { offset },
            ));
        }
        (
            Self {
                entries,
                selected: 0,
                offset: 0,
                initial_scroll,
                g_prefix: GPrefix::default(),
            },
            vec![],
            mutations,
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_timeline(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(code) = self.g_prefix.translate(key) else {
            return OverlayUpdate::stay();
        };

        match code {
            KeyCode::Esc | KeyCode::Char('c') if code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close().with_mutations(vec![StateMutation::Transcript(
                    TranscriptMutation::SetScrollMode(self.initial_scroll.clone()),
                )])
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::PageUp => {
                let delta = -(self.visible_height() as isize).max(1);
                self.move_selection(delta);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::PageDown => {
                let delta = (self.visible_height() as isize).max(1);
                self.move_selection(delta);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::Home => {
                if !self.entries.is_empty() {
                    self.selected = 0;
                    self.offset = 0;
                }
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::End | KeyCode::Char('G') => {
                if !self.entries.is_empty() {
                    self.selected = self.entries.len().saturating_sub(1);
                    self.ensure_visible();
                }
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::Enter | KeyCode::Right => {
                if tui.agent_state.is_running() {
                    return OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Stop the current task first.".to_string(),
                        ),
                    )]);
                }

                match self.jump_command(tui) {
                    Some(command) => OverlayUpdate::close()
                        .with_mutations(vec![StateMutation::Transcript(command)]),
                    None => OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "No timeline entry selected.".to_string(),
                        ),
                    )]),
                }
            }
            KeyCode::Char('f') => {
                if tui.tasks.state(TaskKind::ThreadFork).is_running() {
                    return OverlayUpdate::stay();
                }

                match self.fork_effect(tui) {
                    ForkRequest::Fork(effect) => OverlayUpdate::close()
                        .with_ui_effects(vec![*effect])
                        .with_mutations(vec![]),
                    ForkRequest::Refused(reason) => {
                        OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                            TranscriptMutation::AppendSystemMessage(reason.to_string()),
                        )])
                    }
                    ForkRequest::None => {
                        OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                            TranscriptMutation::AppendSystemMessage(
                                "No timeline entry selected.".to_string(),
                            ),
                        )])
                    }
                }
            }
            _ => OverlayUpdate::stay(),
        }
    }

    fn visible_height(&self) -> usize {
        if self.entries.is_empty() {
            1
        } else {
            self.entries.len().min(MAX_VISIBLE_TURNS)
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }

        let max_index = self.entries.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta).clamp(0, max_index);
        self.selected = next as usize;
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.entries.is_empty() {
            self.offset = 0;
            return;
        }

        let visible_height = self.visible_height();
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + visible_height {
            self.offset = self.selected - visible_height + 1;
        }
    }

    fn selected_entry(&self) -> Option<&TimelineEntry> {
        self.entries.get(self.selected)
    }

    fn preview_scroll_command(&self, tui: &TuiState) -> Vec<StateMutation> {
        self.jump_command(tui)
            .map(|command| vec![StateMutation::Transcript(command)])
            .unwrap_or_default()
    }

    fn jump_command(&self, tui: &TuiState) -> Option<TranscriptMutation> {
        let entry = self.selected_entry()?;
        let info = tui.transcript.scroll.cell_line_info.get(entry.cell_index)?;
        Some(TranscriptMutation::SetScrollOffset {
            offset: info.start_line,
        })
    }

    /// Builds the fork effect for the selected timeline entry.
    ///
    /// Forking always opens a new tab, so the tab this was invoked from (and any
    /// turn still streaming into it) is left untouched.
    fn fork_effect(&self, tui: &TuiState) -> ForkRequest {
        let Some(entry) = self.selected_entry() else {
            return ForkRequest::None;
        };
        let cells = tui.transcript.cells();
        let Some(selected_cell) = cells.get(entry.cell_index) else {
            return ForkRequest::None;
        };
        // Fork from canonical messages (not display cells) so a user message's
        // runtime-context snapshot survives into the fork's saved history.
        let messages = &tui.thread.messages;
        let Some(owner) = fork_owner_for_cell(
            messages,
            cells,
            entry.cell_index,
            tui.agent_state.is_running(),
        ) else {
            return ForkRequest::None;
        };
        let (events, user_input) = match owner {
            ForkOwner::Refuse(reason) => return ForkRequest::Refused(reason),
            ForkOwner::Message(owner) => match selected_cell {
                HistoryCell::User { content, .. } => (
                    zdx_engine::core::thread_persistence::messages_to_events(&messages[..owner]),
                    Some(content.clone()),
                ),
                _ => (
                    zdx_engine::core::thread_persistence::messages_to_events(&messages[..=owner]),
                    None,
                ),
            },
        };
        if events.is_empty() && user_input.is_none() {
            return ForkRequest::None;
        }

        ForkRequest::Fork(Box::new(UiEffect::ForkThread {
            events,
            user_input,
            turn_number: self.selected + 1,
        }))
    }
}

/// Outcome of a fork request from the timeline overlay.
enum ForkRequest {
    /// Fork the selected entry through its owning message.
    Fork(Box<UiEffect>),
    /// The selected entry cannot be forked safely; show the reason.
    Refused(&'static str),
    /// No forkable entry selected.
    None,
}

/// Where a selected transcript cell resolves in canonical history.
enum ForkOwner {
    /// Fork canonical messages up to (inclusive for assistant, exclusive for
    /// user) this index.
    Message(usize),
    /// The selected cell cannot be safely attributed to one assistant response.
    Refuse(&'static str),
}

/// Resolves a selected transcript cell to the canonical message that produced
/// it, without guessing per-message cell counts (which differ between the
/// loaded, event, and live transcript layouts):
///
/// - **User cells**: user cells map 1:1 to real user messages in every layout
///   (tool-result user messages render no cell), so the selected cell's ordinal
///   among user cells equals its ordinal among real user messages.
/// - **Assistant / thinking / tool cells**: a visible-user interval — between
///   the preceding and following user cell — can contain **more than one**
///   canonical assistant message (a tool loop commits an assistant per tool
///   call plus a final response), so ownership is only unambiguous when exactly
///   one assistant message exists in the interval. Anything else — zero
///   (uncommitted or a fully failed turn) or several — refuses to fork rather
///   than truncating at the wrong response. The open (last) interval is also
///   refused while the agent is still generating.
///
/// Returns `None` only when the selection itself is unresolvable; ambiguous or
/// uncommitted non-user selections return [`ForkOwner::Refuse`].
fn fork_owner_for_cell(
    messages: &[ChatMessage],
    cells: &[HistoryCell],
    cell_index: usize,
    agent_running: bool,
) -> Option<ForkOwner> {
    let selected = cells.get(cell_index)?;
    // Ordinal (1-based) of the selected cell among user cells. For a user cell
    // this includes itself; for assistant cells it is the count of preceding
    // user cells (i.e. the ordinal of the owning user message).
    let user_ordinal = cells[..=cell_index]
        .iter()
        .filter(|cell| matches!(cell, HistoryCell::User { .. }))
        .count();
    let user_k = user_ordinal.checked_sub(1)?;
    let owner_user = messages
        .iter()
        .enumerate()
        .filter(|(_, msg)| is_real_user_message(msg))
        .nth(user_k)
        .map(|(idx, _)| idx)?;
    if let HistoryCell::User { .. } = selected {
        return Some(ForkOwner::Message(owner_user));
    }

    // Non-user cell: the visible-user interval is [owner_user, next_user).
    let next_user = if cells[cell_index + 1..]
        .iter()
        .any(|cell| matches!(cell, HistoryCell::User { .. }))
    {
        messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| is_real_user_message(msg))
            .nth(user_k + 1)
            .map(|(idx, _)| idx)
    } else {
        None
    };
    let end = next_user.unwrap_or(messages.len());
    let assistants_in_interval: Vec<usize> = (owner_user + 1..end)
        .filter(|&idx| messages[idx].role == "assistant")
        .collect();

    if next_user.is_none() && agent_running {
        return Some(ForkOwner::Refuse(
            "This response is still generating and can't be forked safely yet. Wait for it to finish, or select the user message instead.",
        ));
    }
    match assistants_in_interval.len() {
        1 => Some(ForkOwner::Message(assistants_in_interval[0])),
        0 => Some(ForkOwner::Refuse(
            "No complete response to fork yet. Select the user message instead.",
        )),
        _ => Some(ForkOwner::Refuse(
            "This response spans multiple model turns, so it can't be forked safely yet. Select the user message above it instead.",
        )),
    }
}

/// A user message that renders a transcript cell: a plain non-empty text
/// message, or a block message carrying anything other than tool results
/// (tool-result user messages render no cell in any layout).
fn is_real_user_message(msg: &ChatMessage) -> bool {
    use zdx_engine::providers::{ChatContentBlock, MessageContent};

    if msg.role != "user" {
        return false;
    }
    match &msg.content {
        MessageContent::Text(text) => !text.is_empty(),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .any(|block| !matches!(block, ChatContentBlock::ToolResult(_))),
    }
}

fn build_entries(cells: &[HistoryCell]) -> Vec<TimelineEntry> {
    cells
        .iter()
        .enumerate()
        .filter_map(|(idx, cell)| match cell {
            HistoryCell::User { content, .. } => Some((idx, TimelineRole::User, content)),
            HistoryCell::Assistant { content, .. } => Some((idx, TimelineRole::Assistant, content)),
            _ => None,
        })
        .map(|(idx, role, content)| {
            let sanitized = sanitize_for_display(content);
            let line = sanitized.lines().next().unwrap_or("").trim();
            TimelineEntry {
                cell_index: idx,
                role,
                preview: line.to_string(),
            }
        })
        .collect()
}

fn render_timeline(frame: &mut Frame, state: &TimelineState, area: Rect, input_y: u16) {
    use super::render_utils::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let visible_rows = state.entries.len().clamp(1, MAX_VISIBLE_TURNS) as u16;
    let overlay_height = (visible_rows + 5).max(7);
    let overlay_area = calculate_overlay_area(area, input_y, OVERLAY_WIDTH, overlay_height);

    render_overlay_container(frame, overlay_area, "Timeline", Color::Green);

    let inner_area = Rect::new(
        overlay_area.x + 1,
        overlay_area.y + 1,
        overlay_area.width.saturating_sub(2),
        overlay_area.height.saturating_sub(2),
    );

    if state.entries.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(Span::styled(
                "No turns yet",
                Style::default().fg(Color::DarkGray),
            )),
            Line::default(),
            Line::from(Span::styled(
                "Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(msg, inner_area);
        return;
    }

    let list_height = inner_area.height.saturating_sub(2) as usize;
    let list_area = Rect::new(
        inner_area.x,
        inner_area.y,
        inner_area.width,
        list_height as u16,
    );

    let mut items = Vec::new();
    let max_content_width = inner_area.width.saturating_sub(6).max(1) as usize;

    for entry in state.entries.iter().skip(state.offset).take(list_height) {
        let role_label = format!("[{}] ", entry.role.badge());
        let preview = truncate_with_ellipsis(&entry.preview, max_content_width);
        let line = Line::from(vec![
            Span::styled(role_label, Style::default().fg(entry.role.color())),
            Span::styled(preview, Style::default().fg(Color::White)),
        ]);
        items.push(ListItem::new(line));
    }

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    let visible_selected = state.selected.saturating_sub(state.offset);
    list_state.select(Some(visible_selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, list_height as u16);

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "navigate"),
            InputHint::new("Enter", "jump"),
            InputHint::new("f", "fork to new tab"),
            InputHint::new("Esc", "close"),
        ],
        Color::Green,
    );
}

#[cfg(test)]
mod tests {
    use zdx_engine::core::thread_persistence::ThreadEvent;

    use super::*;

    #[test]
    fn fork_events_never_emits_usage_or_meta() {
        // Forks reconstruct a thread's context from canonical messages, not
        // from the raw thread JSONL. Usage events are not messages, so they
        // are never reproduced — which is exactly why a forked thread does
        // not inherit (and double-count) the parent's usage. Guard that
        // invariant: message -> event reconstruction must yield only
        // conversation events, never `usage`/`meta`.
        let messages = vec![
            ChatMessage::user("question"),
            ChatMessage::assistant_text("answer", None),
        ];
        let events = zdx_engine::core::thread_persistence::messages_to_events(&messages);
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, ThreadEvent::Usage { .. } | ThreadEvent::Meta { .. })),
            "fork event reconstruction must not carry usage/meta events"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ThreadEvent::Message { .. })),
            "conversation messages should survive reconstruction"
        );
    }

    /// The fork resolves a selected cell to its owning canonical message by
    /// user-cell ordinal and assistant intervals — independent of how each
    /// transcript layout renders assistant text/tools. A closed visible-user
    /// interval with exactly one assistant message forks unambiguously.
    #[test]
    fn fork_owner_resolves_by_user_ordinal_and_assistant_interval() {
        use zdx_engine::providers::{ChatContentBlock, ReasoningBlock};
        use zdx_engine::tools::{ToolResult, ToolResultContent};

        // Canonical messages: user0, assistant0 (thinking + text), tool results
        // (renders NO user cell in any layout), user1, assistant1.
        let messages = vec![
            ChatMessage::user("q1"),
            ChatMessage::assistant_blocks(vec![
                ChatContentBlock::Reasoning(ReasoningBlock {
                    text: Some("thinking".to_string()),
                    replay: None,
                }),
                ChatContentBlock::text("a1"),
                ChatContentBlock::tool_use("t1", "bash", serde_json::json!({})),
            ]),
            ChatMessage::tool_results(vec![ToolResult {
                tool_use_id: "t1".to_string(),
                content: ToolResultContent::Text("{\"ok\":true}".to_string()),
                is_error: false,
            }]),
            ChatMessage::user("q2"),
            ChatMessage::assistant_text("a2", None),
        ];
        // Any layout renders cells as: [user0, thinking, assistant, tool,
        // user1, assistant] — the exact assistant-cell split (coalesced vs
        // per-run) must not matter to the fork.
        let cells = vec![
            HistoryCell::user("q1"),
            HistoryCell::assistant("a1"),
            HistoryCell::thinking_streaming("thinking"),
            HistoryCell::assistant("a1 more"),
            HistoryCell::tool_running("t1", "bash", serde_json::json!({})),
            HistoryCell::user("q2"),
            HistoryCell::assistant("a2"),
        ];

        // User cells: q1 → msg 0, q2 → msg 3 (skips the tool-result message).
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 0, false),
            Some(ForkOwner::Message(0))
        ));
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 5, false),
            Some(ForkOwner::Message(3))
        ));
        // Assistant/thinking/tool cells between q1 and q2 → the single
        // assistant0 (msg 1).
        for idx in [1usize, 2, 3, 4] {
            assert!(
                matches!(
                    fork_owner_for_cell(&messages, &cells, idx, false),
                    Some(ForkOwner::Message(1))
                ),
                "cell {idx} should resolve to assistant0"
            );
        }
        // Last assistant cell → the single assistant1 (msg 4).
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 6, false),
            Some(ForkOwner::Message(4))
        ));
    }

    /// The reviewer's tool-loop sequence: a visible-user interval contains
    /// THREE canonical assistant messages (per-tool-call responses + final), so
    /// any non-user cell in it must refuse rather than truncate at the first
    /// assistant.
    #[test]
    fn fork_owner_refuses_multi_assistant_tool_loop() {
        use zdx_engine::providers::ChatContentBlock;
        use zdx_engine::tools::{ToolResult, ToolResultContent};

        // [user q1, assistant tool1, user tool-result1, assistant tool2,
        //  user tool-result2, assistant final, user q2]
        let messages = vec![
            ChatMessage::user("q1"),
            ChatMessage::assistant_blocks(vec![ChatContentBlock::tool_use(
                "t1",
                "bash",
                serde_json::json!({}),
            )]),
            ChatMessage::tool_results(vec![ToolResult {
                tool_use_id: "t1".to_string(),
                content: ToolResultContent::Text("{}".to_string()),
                is_error: false,
            }]),
            ChatMessage::assistant_blocks(vec![ChatContentBlock::tool_use(
                "t2",
                "bash",
                serde_json::json!({}),
            )]),
            ChatMessage::tool_results(vec![ToolResult {
                tool_use_id: "t2".to_string(),
                content: ToolResultContent::Text("{}".to_string()),
                is_error: false,
            }]),
            ChatMessage::assistant_text("final answer", None),
            ChatMessage::user("q2"),
        ];
        // cells: [q1 user, tool1, tool2, final, q2 user]
        let cells = vec![
            HistoryCell::user("q1"),
            HistoryCell::tool_running("t1", "bash", serde_json::json!({})),
            HistoryCell::tool_running("t2", "bash", serde_json::json!({})),
            HistoryCell::assistant("final answer"),
            HistoryCell::user("q2"),
        ];

        // Non-user cells in the q1..q2 interval (three assistants) refuse.
        for idx in [1usize, 2, 3] {
            assert!(
                matches!(
                    fork_owner_for_cell(&messages, &cells, idx, false),
                    Some(ForkOwner::Refuse(_))
                ),
                "cell {idx} must refuse a multi-assistant interval"
            );
        }
        // User-cell forks stay exact.
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 0, false),
            Some(ForkOwner::Message(0))
        ));
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 4, false),
            Some(ForkOwner::Message(6))
        ));
    }

    /// A selected assistant cell whose turn has not committed yet (assistant
    /// message not yet in canonical history) refuses with a visible reason
    /// instead of guessing the next index.
    #[test]
    fn fork_owner_refuses_uncommitted_assistant_turn() {
        let messages = vec![ChatMessage::user("q1")];
        let cells = vec![
            HistoryCell::user("q1"),
            HistoryCell::assistant("streaming…"),
        ];
        // assistant cell at index 1 → no committed assistant yet and the agent
        // is running → refuse.
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 1, true),
            Some(ForkOwner::Refuse(_))
        ));
        // The user cell still resolves.
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 0, true),
            Some(ForkOwner::Message(0))
        ));
    }

    /// Tool-result-only user messages render no cell and are skipped when
    /// resolving user-cell ordinals.
    #[test]
    fn fork_owner_skips_tool_result_user_messages() {
        use zdx_engine::providers::ChatContentBlock;
        use zdx_engine::tools::{ToolResult, ToolResultContent};

        let messages = vec![
            ChatMessage::user("q1"),
            ChatMessage::assistant_blocks(vec![ChatContentBlock::tool_use(
                "t1",
                "bash",
                serde_json::json!({}),
            )]),
            ChatMessage::tool_results(vec![ToolResult {
                tool_use_id: "t1".to_string(),
                content: ToolResultContent::Text("{}".to_string()),
                is_error: false,
            }]),
            ChatMessage::user("q2"),
        ];
        let cells = vec![
            HistoryCell::user("q1"),
            HistoryCell::assistant("a"),
            HistoryCell::user("q2"),
        ];
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 0, false),
            Some(ForkOwner::Message(0))
        ));
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 1, false),
            Some(ForkOwner::Message(1))
        ));
        assert!(matches!(
            fork_owner_for_cell(&messages, &cells, 2, false),
            Some(ForkOwner::Message(3))
        ));
    }

    /// Forking from canonical messages preserves the first user message's
    /// runtime-context block and change key in the saved fork history.
    #[test]
    fn fork_events_preserve_runtime_context() {
        let block = "<runtime_context>snapshot</runtime_context>".to_string();
        let messages = vec![
            ChatMessage::user("q1")
                .with_runtime_context(Some(block.clone()), Some("k1".to_string())),
            ChatMessage::assistant_text("a1", None),
            ChatMessage::user("q2"),
        ];

        let events = zdx_engine::core::thread_persistence::messages_to_events(&messages);
        assert_eq!(
            zdx_engine::core::thread_persistence::last_attached_context_key_from_events(&events)
                .as_deref(),
            Some("k1"),
            "the forked history must keep the last attached change key"
        );

        // Replaying the fork reconstructs the context + key on the user message.
        let replayed = zdx_engine::core::thread_persistence::thread_events_to_messages(events);
        let first = &replayed[0];
        assert_eq!(first.context.as_deref(), Some(block.as_str()));
        assert_eq!(first.context_key.as_deref(), Some("k1"));
    }

    #[test]
    fn build_entries_filters_and_trims() {
        let cells = vec![
            HistoryCell::system("skip"),
            HistoryCell::user("Hello\nSecond"),
            HistoryCell::assistant("Reply"),
        ];
        let entries = build_entries(&cells);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].preview, "Hello");
        assert_eq!(entries[1].preview, "Reply");
    }
}
