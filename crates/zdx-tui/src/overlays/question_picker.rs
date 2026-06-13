use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::OverlayUpdate;
use crate::ask_user::QuestionView;
use crate::effects::UiEffect;
use crate::mutations::{StateMutation, TranscriptMutation};
use crate::state::TuiState;

/// Picker overlay for a mid-run `ask_user_question`.
///
/// Enter selects an option and resolves the pending question; Esc dismisses
/// the overlay only, leaving the question pending so the user can type a
/// free-form answer instead.
#[derive(Debug, Clone)]
pub struct QuestionPickerState {
    thread_id: String,
    tool_use_id: String,
    question: String,
    options: Vec<(String, String)>,
    pub selected: usize,
}

impl QuestionPickerState {
    pub(crate) fn open(thread_id: String, view: QuestionView) -> Self {
        Self {
            thread_id,
            tool_use_id: view.tool_use_id,
            question: view.question,
            options: view
                .options
                .into_iter()
                .map(|o| (o.label, o.description))
                .collect(),
            selected: 0,
        }
    }

    /// The tool-use id this picker is showing, used to auto-close when the
    /// matching question completes.
    pub fn tool_use_id(&self) -> &str {
        &self.tool_use_id
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_question_picker(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                // Dismiss the overlay but keep the question pending: the user
                // can type a free-form answer.
                OverlayUpdate::close().with_mutations(vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(
                        "Picker dismissed — type your answer to continue.".to_string(),
                    ),
                )])
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down => {
                if self.selected + 1 < self.options.len() {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let idx = (c as usize) - ('1' as usize);
                self.confirm(idx)
            }
            KeyCode::Enter => self.confirm(self.selected),
            _ => OverlayUpdate::stay(),
        }
    }

    /// Resolves the pending question with the option at `idx`.
    fn confirm(&self, idx: usize) -> OverlayUpdate {
        let Some((label, _)) = self.options.get(idx) else {
            return OverlayUpdate::stay();
        };
        OverlayUpdate::close()
            .with_ui_effects(vec![UiEffect::AnswerPendingQuestion {
                thread_id: self.thread_id.clone(),
                text: label.clone(),
            }])
            .with_mutations(vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(format!("↩️ Answered: {label}")),
            )])
    }
}

fn render_question_picker(
    frame: &mut Frame,
    picker: &QuestionPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{InputHint, OverlayConfig, render_overlay, render_separator};

    // Width: use most of the screen so option descriptions aren't clipped.
    let width = area.width.saturating_sub(6).clamp(40, 96);
    // Inner text width available to wrapped description lines (border + symbol).
    let wrap_width = width.saturating_sub(6) as usize;

    // Pre-build each option's lines (label + wrapped description) so we can
    // size the overlay to fit and avoid mid-word clipping.
    let option_lines: Vec<Vec<Line>> = picker
        .options
        .iter()
        .enumerate()
        .map(|(idx, (label, desc))| {
            let mut lines = vec![Line::from(Span::styled(
                format!("{}. {label}", idx + 1),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))];
            for chunk in wrap_text(desc.trim(), wrap_width) {
                lines.push(Line::from(Span::styled(
                    format!("   {chunk}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines
        })
        .collect();

    let total_lines: usize = option_lines.iter().map(Vec::len).sum();
    let title = truncate(&picker.question, width.saturating_sub(4) as usize);
    let picker_height = (total_lines as u16 + 4).max(7);

    let hints = [
        InputHint::new("1-9", "pick"),
        InputHint::new("↑↓", "navigate"),
        InputHint::new("Esc", "type instead"),
    ];
    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: &title,
            border_color: Color::Cyan,
            width,
            height: picker_height,
            hints: &hints,
        },
    );

    let list_height = layout.body.height.saturating_sub(1);
    let list_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, list_height);

    let items: Vec<ListItem> = option_lines.into_iter().map(ListItem::new).collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, layout.body, list_height);
}

/// Word-wraps `text` to `width` columns, returning the wrapped lines (empty
/// when `text` is empty).
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() || width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::wrap_text;

    #[test]
    fn wraps_words_within_width() {
        let wrapped = wrap_text("the quick brown fox jumps", 10);
        assert!(wrapped.iter().all(|l| l.chars().count() <= 10));
        assert_eq!(wrapped.join(" "), "the quick brown fox jumps");
    }

    #[test]
    fn empty_or_zero_width_yields_no_lines() {
        assert!(wrap_text("", 10).is_empty());
        assert!(wrap_text("hello", 0).is_empty());
    }
}
