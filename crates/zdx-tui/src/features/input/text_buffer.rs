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

    /// Moves the cursor left by one word.
    pub fn move_word_left(&mut self) {
        self.ensure_line();

        while self.cursor_row > 0 && self.cursor_col == 0 {
            self.cursor_row -= 1;
            self.cursor_col = line_char_len(&self.lines[self.cursor_row]);
        }

        if self.cursor_col == 0 {
            return;
        }

        let line = &self.lines[self.cursor_row];
        let chars: Vec<char> = line.chars().collect();
        let mut idx = self.cursor_col.min(chars.len());

        if idx == 0 {
            return;
        }

        idx = scan_left_segment(&chars, idx);

        self.cursor_col = idx;
    }

    /// Moves the cursor right by one word.
    pub fn move_word_right(&mut self) {
        self.ensure_line();

        let mut row = self.cursor_row;
        let mut col = self.cursor_col;

        loop {
            let line_len = line_char_len(&self.lines[row]);
            if col < line_len {
                break;
            }
            if row + 1 >= self.lines.len() {
                return;
            }
            row += 1;
            col = 0;
        }

        let line = &self.lines[row];
        let chars: Vec<char> = line.chars().collect();
        let mut idx = col.min(chars.len());

        if idx >= chars.len() {
            self.cursor_row = row;
            self.cursor_col = idx;
            return;
        }

        idx = scan_right_segment(&chars, idx);

        self.cursor_row = row;
        self.cursor_col = idx;
    }

    /// Deletes the word immediately to the left of the cursor.
    pub fn delete_word_left(&mut self) {
        if self.selection_all {
            self.clear();
            return;
        }

        self.ensure_line();
        if self.cursor_row == 0 && self.cursor_col == 0 {
            return;
        }

        let (start_row, start_col) = self.word_left_target();
        let end_row = self.cursor_row;
        let end_col = self.cursor_col;

        self.delete_range(start_row, start_col, end_row, end_col);
        self.cursor_row = start_row;
        self.cursor_col = start_col;
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

    fn word_left_target(&self) -> (usize, usize) {
        let mut row = self.cursor_row;
        let mut col = self.cursor_col;

        while row > 0 && col == 0 {
            row -= 1;
            col = line_char_len(&self.lines[row]);
        }

        if col == 0 {
            return (row, 0);
        }

        let line = &self.lines[row];
        let chars: Vec<char> = line.chars().collect();
        let mut idx = col.min(chars.len());

        if idx == 0 {
            return (row, 0);
        }

        idx = scan_left_segment(&chars, idx);

        (row, idx)
    }

    fn delete_range(&mut self, start_row: usize, start_col: usize, end_row: usize, end_col: usize) {
        if start_row > end_row || (start_row == end_row && start_col >= end_col) {
            return;
        }

        if start_row == end_row {
            let line = &mut self.lines[start_row];
            let start = char_to_byte_index(line, start_col);
            let end = char_to_byte_index(line, end_col);
            line.replace_range(start..end, "");
            return;
        }

        let start_line = self.lines[start_row].clone();
        let end_line = self.lines[end_row].clone();
        let start_byte = char_to_byte_index(&start_line, start_col);
        let end_byte = char_to_byte_index(&end_line, end_col);

        let prefix = &start_line[..start_byte];
        let suffix = &end_line[end_byte..];
        let merged = format!("{}{}", prefix, suffix);

        self.lines.splice(start_row..=end_row, [merged]);
    }
}

fn line_char_len(line: &str) -> usize {
    line.chars().count()
}

/// Returns true if the character is a word character (alphanumeric or underscore).
/// Punctuation and other symbols are treated as word boundaries.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CharClass {
    Whitespace,
    Word,
    Punct,
}

fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if is_word_char(c) {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

fn scan_left_segment(chars: &[char], mut idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    let class = char_class(chars[idx - 1]);
    while idx > 0 && char_class(chars[idx - 1]) == class {
        idx -= 1;
    }
    idx
}

fn scan_right_segment(chars: &[char], mut idx: usize) -> usize {
    if idx >= chars.len() {
        return idx;
    }
    let class = char_class(chars[idx]);
    while idx < chars.len() && char_class(chars[idx]) == class {
        idx += 1;
    }
    idx
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_word_left_url_segments() {
        // URL: https://github.com/openai/codex/blob/main/README.md
        // Option+Backspace should delete one segment at a time, not the whole URL
        let mut buf = TextBuffer::default();
        buf.insert_str("https://github.com/openai/codex/blob/main/README.md");

        // cursor is at the end: "...README.md|"
        buf.delete_word_left(); // delete "md" (word chars)
        assert_eq!(
            buf.lines()[0],
            "https://github.com/openai/codex/blob/main/README."
        );

        buf.delete_word_left(); // delete "." (punctuation)
        assert_eq!(
            buf.lines()[0],
            "https://github.com/openai/codex/blob/main/README"
        );

        buf.delete_word_left(); // delete "README" (word chars)
        assert_eq!(buf.lines()[0], "https://github.com/openai/codex/blob/main/");

        buf.delete_word_left(); // delete "/" (punctuation)
        assert_eq!(buf.lines()[0], "https://github.com/openai/codex/blob/main");

        buf.delete_word_left(); // delete "main" (word chars)
        assert_eq!(buf.lines()[0], "https://github.com/openai/codex/blob/");

        buf.delete_word_left(); // delete "/" (punctuation)
        assert_eq!(buf.lines()[0], "https://github.com/openai/codex/blob");
    }

    #[test]
    fn move_word_left_url_segments() {
        let mut buf = TextBuffer::default();
        buf.insert_str("https://example.com/path");
        // len = 24, cursor at 24

        // cursor at end (24)
        buf.move_word_left(); // skip "path" (word)
        assert_eq!(buf.cursor(), (0, 20)); // after "/"

        buf.move_word_left(); // skip "/" (punctuation)
        assert_eq!(buf.cursor(), (0, 19)); // after "com"

        buf.move_word_left(); // skip "com" (word)
        assert_eq!(buf.cursor(), (0, 16)); // after "."

        buf.move_word_left(); // skip "." (punctuation)
        assert_eq!(buf.cursor(), (0, 15)); // after "example"

        buf.move_word_left(); // skip "example" (word)
        assert_eq!(buf.cursor(), (0, 8)); // after "://"

        buf.move_word_left(); // skip "://" (punctuation)
        assert_eq!(buf.cursor(), (0, 5)); // after "https"

        buf.move_word_left(); // skip "https" (word)
        assert_eq!(buf.cursor(), (0, 0)); // at start
    }

    #[test]
    fn move_word_right_url_segments() {
        let mut buf = TextBuffer::default();
        buf.insert_str("https://example.com");
        buf.move_cursor(CursorMove::Head); // go to start (0)

        buf.move_word_right(); // "https" (word)
        assert_eq!(buf.cursor(), (0, 5));

        buf.move_word_right(); // "://" (punctuation)
        assert_eq!(buf.cursor(), (0, 8));

        buf.move_word_right(); // "example" (word)
        assert_eq!(buf.cursor(), (0, 15));

        buf.move_word_right(); // "." (punctuation)
        assert_eq!(buf.cursor(), (0, 16));

        buf.move_word_right(); // "com" (word)
        assert_eq!(buf.cursor(), (0, 19));
    }

    #[test]
    fn delete_word_left_with_whitespace() {
        let mut buf = TextBuffer::default();
        buf.insert_str("hello world");

        buf.delete_word_left(); // delete "world" (word)
        assert_eq!(buf.lines()[0], "hello ");

        buf.delete_word_left(); // delete " " (whitespace)
        assert_eq!(buf.lines()[0], "hello");

        buf.delete_word_left(); // delete "hello" (word)
        assert_eq!(buf.lines()[0], "");
    }
}
