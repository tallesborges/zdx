//! Minimal text buffer for input editing.
//!
//! This is a lightweight replacement for external textarea helpers.
//! It supports the subset of editing operations used by the input slice.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Cursor movement commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMove {
    Up,
    Down,
    Forward,
    Back,
    Head,
    End,
    Top,
    Bottom,
}

/// Simple text buffer with line storage and a (row, col) cursor.
#[derive(Debug, Clone)]
pub struct TextBuffer {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    selection_all: bool,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            selection_all: false,
        }
    }
}

impl TextBuffer {
    /// Returns all lines in the buffer.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Returns the current cursor position as (row, col) in char units.
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// Inserts a string at the cursor, advancing the cursor.
    pub fn insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        if self.selection_all {
            self.clear();
        }
        self.selection_all = false;

        self.ensure_line();
        let row = self.cursor_row;

        if !text.contains('\n') {
            let line = &mut self.lines[row];
            let byte_idx = char_to_byte_index(line, self.cursor_col);
            line.insert_str(byte_idx, text);
            self.cursor_col += text.chars().count();
            return;
        }

        let current_line = self.lines[row].clone();
        let byte_idx = char_to_byte_index(&current_line, self.cursor_col);
        let (prefix, suffix) = current_line.split_at(byte_idx);

        let parts: Vec<&str> = text.split('\n').collect();

        let mut new_lines: Vec<String> = Vec::with_capacity(parts.len());
        new_lines.push(format!("{}{}", prefix, parts[0]));
        if parts.len() > 2 {
            for part in &parts[1..parts.len() - 1] {
                new_lines.push((*part).to_string());
            }
        }
        new_lines.push(format!("{}{}", parts[parts.len() - 1], suffix));

        self.lines.splice(row..=row, new_lines);
        self.cursor_row = row + parts.len() - 1;
        self.cursor_col = parts[parts.len() - 1].chars().count();
    }

    /// Inserts a single character at the cursor.
    pub fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        let mut buf = [0u8; 4];
        self.insert_str(ch.encode_utf8(&mut buf));
    }

    /// Inserts a newline at the cursor.
    pub fn insert_newline(&mut self) {
        self.insert_str("\n");
    }

    /// Deletes the character at the cursor (Delete key semantics).
    pub fn delete_next_char(&mut self) {
        self.selection_all = false;
        self.ensure_line();

        let row = self.cursor_row;
        let col = self.cursor_col;
        let line_len = line_char_len(&self.lines[row]);

        if col >= line_len {
            if row + 1 < self.lines.len() {
                let next = self.lines.remove(row + 1);
                self.lines[row].push_str(&next);
            }
            return;
        }

        let line = &mut self.lines[row];
        let start = char_to_byte_index(line, col);
        let end = char_to_byte_index(line, col + 1);
        line.replace_range(start..end, "");
    }

    /// Deletes the character before the cursor (Backspace semantics).
    pub fn delete_prev_char(&mut self) {
        self.selection_all = false;
        self.ensure_line();

        if self.cursor_col > 0 {
            let row = self.cursor_row;
            let col = self.cursor_col - 1;
            let line = &mut self.lines[row];
            let start = char_to_byte_index(line, col);
            let end = char_to_byte_index(line, col + 1);
            line.replace_range(start..end, "");
            self.cursor_col = col;
            return;
        }

        if self.cursor_row == 0 {
            return;
        }

        let row = self.cursor_row;
        let prev_len = line_char_len(&self.lines[row - 1]);
        let current = self.lines.remove(row);
        self.lines[row - 1].push_str(&current);
        self.cursor_row -= 1;
        self.cursor_col = prev_len;
    }

    /// Deletes from the cursor to the end of the line.
    pub fn delete_line_by_end(&mut self) {
        self.selection_all = false;
        self.ensure_line();

        let row = self.cursor_row;
        let line = &mut self.lines[row];
        let byte_idx = char_to_byte_index(line, self.cursor_col);
        line.truncate(byte_idx);
    }

    /// Selects all text.
    pub fn select_all(&mut self) {
        self.selection_all = true;
    }

    /// Cuts the selected text (currently only supports select-all).
    pub fn cut(&mut self) {
        if self.selection_all {
            self.clear();
        }
        self.selection_all = false;
    }

    /// Moves the cursor according to a movement command.
    pub fn move_cursor(&mut self, movement: CursorMove) {
        self.ensure_line();
        match movement {
            CursorMove::Up => {
                if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    let len = line_char_len(&self.lines[self.cursor_row]);
                    self.cursor_col = self.cursor_col.min(len);
                }
            }
            CursorMove::Down => {
                if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    let len = line_char_len(&self.lines[self.cursor_row]);
                    self.cursor_col = self.cursor_col.min(len);
                }
            }
            CursorMove::Forward => {
                let len = line_char_len(&self.lines[self.cursor_row]);
                if self.cursor_col < len {
                    self.cursor_col += 1;
                } else if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    self.cursor_col = 0;
                }
            }
            CursorMove::Back => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = line_char_len(&self.lines[self.cursor_row]);
                }
            }
            CursorMove::Head => {
                self.cursor_col = 0;
            }
            CursorMove::End => {
                self.cursor_col = line_char_len(&self.lines[self.cursor_row]);
            }
            CursorMove::Top => {
                self.cursor_row = 0;
                let len = line_char_len(&self.lines[self.cursor_row]);
                self.cursor_col = self.cursor_col.min(len);
            }
            CursorMove::Bottom => {
                self.cursor_row = self.lines.len().saturating_sub(1);
                let len = line_char_len(&self.lines[self.cursor_row]);
                self.cursor_col = self.cursor_col.min(len);
            }
        }
    }

    /// Handles a key input for basic editing.
    pub fn input(&mut self, key: KeyEvent) {
        if matches!(key.kind, KeyEventKind::Release) {
            return;
        }

        match key.code {
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_char(ch);
            }
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Backspace => self.delete_prev_char(),
            KeyCode::Delete => self.delete_next_char(),
            KeyCode::Left => self.move_cursor(CursorMove::Back),
            KeyCode::Right => self.move_cursor(CursorMove::Forward),
            KeyCode::Up => self.move_cursor(CursorMove::Up),
            KeyCode::Down => self.move_cursor(CursorMove::Down),
            KeyCode::Home => self.move_cursor(CursorMove::Head),
            KeyCode::End => self.move_cursor(CursorMove::End),
            _ => {}
        }
    }

    fn clear(&mut self) {
        self.lines.clear();
        self.lines.push(String::new());
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn ensure_line(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
            self.cursor_row = 0;
            self.cursor_col = 0;
            return;
        }

        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        let len = line_char_len(&self.lines[self.cursor_row]);
        self.cursor_col = self.cursor_col.min(len);
    }
}

fn line_char_len(line: &str) -> usize {
    line.chars().count()
}

fn char_to_byte_index(line: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    line.char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}
