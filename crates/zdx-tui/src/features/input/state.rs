//! User input state.
//!
//! Manages the text area, command history, and history navigation.

use super::{CursorMove, TextBuffer};
use crate::mutations::InputMutation;

/// Threshold for replacing large pastes with placeholders (in chars).
pub const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

/// A pending paste stored for later expansion on submission.
#[derive(Debug, Clone)]
pub struct PendingPaste {
    /// Unique numeric identifier for this paste (e.g., "1", "2", "3").
    pub id: String,
    /// The placeholder text shown in the textarea.
    pub placeholder: String,
    /// The original pasted content.
    pub content: String,
}

/// A pending image attached to the input.
#[derive(Debug, Clone)]
pub struct PendingImage {
    /// Unique numeric identifier for this image.
    pub id: String,
    /// The placeholder text shown in the textarea.
    pub placeholder: String,
    /// MIME type of the image (e.g., "image/png").
    pub mime_type: String,
    /// Base64-encoded image data.
    pub data: String,
    /// Optional source file path.
    pub source_path: Option<String>,
}

/// Handoff feature state machine.
///
/// Models the lifecycle of a handoff operation:
/// - `Idle`: No handoff in progress
/// - `Pending`: User is typing the goal in textarea
/// - `Generating`: Async subagent is generating the handoff prompt
/// - `Ready`: Generated prompt is in textarea, awaiting confirmation
#[derive(Default, Debug)]
pub enum HandoffState {
    #[default]
    Idle,

    /// User is typing the handoff goal in the textarea.
    Pending,

    /// Handoff generation is in progress.
    Generating,

    /// Generated prompt is in textarea, ready for user to review and submit.
    Ready,
}

impl HandoffState {
    /// Returns true if handoff is in any active state (not Idle).
    pub fn is_active(&self) -> bool {
        !matches!(self, HandoffState::Idle)
    }

    /// Returns true if currently generating.
    pub fn is_generating(&self) -> bool {
        matches!(self, HandoffState::Generating)
    }

    /// Returns true if in pending state (awaiting goal input).
    pub fn is_pending(&self) -> bool {
        matches!(self, HandoffState::Pending)
    }

    /// Returns true if ready for confirmation.
    pub fn is_ready(&self) -> bool {
        matches!(self, HandoffState::Ready)
    }

    /// Cancels any in-progress generation and resets to Idle.
    ///
    /// Note: This is called from `InputMutation::SetHandoffState`. For explicit
    /// cancellation via hotkey, the reducer should emit `UiEffect::CancelTask`.
    pub fn cancel(&mut self) {
        *self = HandoffState::Idle;
    }
}

/// User input state.
///
/// Encapsulates the text area, command history, and navigation state.
pub struct InputState {
    /// Text area for user input.
    pub textarea: TextBuffer,

    /// Command history for ↑/↓ navigation.
    pub history: Vec<String>,

    /// Current position in history (None = not navigating).
    pub history_index: Option<usize>,

    /// Draft text saved when navigating history.
    pub draft: Option<String>,

    /// Handoff feature state.
    pub handoff: HandoffState,

    /// Queued prompts to send after the current turn completes.
    pub queued: std::collections::VecDeque<String>,

    /// Pending pastes waiting for expansion on submission.
    pub pending_pastes: Vec<PendingPaste>,

    /// Monotonic counter for generating unique paste IDs.
    paste_counter: u32,

    /// Pending images attached to the input.
    pub pending_images: Vec<PendingImage>,

    /// Monotonic counter for generating unique image IDs.
    image_counter: u32,
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

impl InputState {
    /// Creates a new `InputState` with default textarea styling.
    pub fn new() -> Self {
        let textarea = TextBuffer::default();

        Self {
            textarea,
            history: Vec::new(),
            history_index: None,
            draft: None,
            handoff: HandoffState::Idle,
            queued: std::collections::VecDeque::new(),
            pending_pastes: Vec::new(),
            paste_counter: 0,
            pending_images: Vec::new(),
            image_counter: 0,
        }
    }

    /// Gets the current input text.
    pub fn get_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Gets the current input text with pending paste placeholders expanded.
    ///
    /// Replaces all placeholder strings with their original pasted content,
    /// then clears `pending_pastes`. Should only be called at submission time.
    ///
    /// If a placeholder appears multiple times (e.g., user copy-pasted it),
    /// all occurrences are expanded to the same content.
    pub fn get_text_with_pending(&mut self) -> String {
        let text = self.get_text();
        // Expand paste placeholders to their original content
        let expanded = self.pending_pastes.iter().fold(text, |acc, paste| {
            acc.replace(&paste.placeholder, &paste.content)
        });
        // Replace image placeholders with short references (e.g., "[Image #1]" → "[image 1]")
        let expanded = self.pending_images.iter().fold(expanded, |acc, img| {
            acc.replace(&img.placeholder, &format!("[Image {}]", img.id))
        });
        self.clear_pending_pastes();
        expanded.trim().to_string()
    }

    /// Clears pending pastes but keeps the counter.
    pub fn clear_pending_pastes(&mut self) {
        self.pending_pastes.clear();
    }

    /// Removes pending pastes whose placeholders no longer exist in the textarea.
    ///
    /// Called after any text mutation that might delete or modify placeholders.
    /// This keeps `pending_pastes` in sync with what's actually in the textarea.
    pub fn sync_pending_pastes(&mut self) {
        if self.pending_pastes.is_empty() {
            return;
        }
        let text = self.get_text();
        self.pending_pastes
            .retain(|paste| text.contains(&paste.placeholder));
    }

    /// Attempts to delete an entire placeholder if the user is deleting its closing bracket.
    ///
    /// When the user presses Backspace or Delete on the `]` of a placeholder like
    /// `[Pasted Content 1234 chars #a1b2c3d4]`, this removes the entire placeholder
    /// from the textarea rather than leaving orphaned text.
    ///
    /// Returns `true` if a placeholder was deleted (caller should skip normal key handling),
    /// `false` otherwise (caller should proceed with normal key handling).
    pub fn try_delete_placeholder_at_bracket(&mut self, is_backspace: bool) -> bool {
        if self.pending_pastes.is_empty() && self.pending_images.is_empty() {
            return false;
        }

        let text = self.get_text();
        let (row, col) = self.textarea.cursor();

        // Convert cursor position (row, col) to byte offset in text
        let cursor_byte_offset = Self::cursor_to_byte_offset(&text, row, col);

        // Determine the byte offset of the character being deleted
        let delete_byte_offset = if is_backspace {
            // Backspace deletes the char immediately before cursor
            if cursor_byte_offset == 0 {
                return false;
            }
            // Find start of previous char (handle multi-byte UTF-8)
            text[..cursor_byte_offset]
                .char_indices()
                .last()
                .map_or(0, |(i, _)| i)
        } else {
            // Delete deletes the char at cursor
            if cursor_byte_offset >= text.len() {
                return false;
            }
            cursor_byte_offset
        };

        // Check if the char at delete_byte_offset is `]`
        if !text[delete_byte_offset..].starts_with(']') {
            return false;
        }

        // Find if this `]` is the closing bracket of any paste placeholder
        for paste_idx in 0..self.pending_pastes.len() {
            let placeholder = self.pending_pastes[paste_idx].placeholder.clone();

            let mut search_start = 0;
            while let Some(pos) = text[search_start..].find(&placeholder) {
                let abs_pos = search_start + pos;
                let placeholder_end = abs_pos + placeholder.len();

                if delete_byte_offset == placeholder_end - 1 {
                    return self.delete_placeholder_at(paste_idx, abs_pos, &text);
                }

                search_start = abs_pos + 1;
            }
        }

        // Find if this `]` is the closing bracket of any image placeholder
        for img_idx in 0..self.pending_images.len() {
            let placeholder = self.pending_images[img_idx].placeholder.clone();

            let mut search_start = 0;
            while let Some(pos) = text[search_start..].find(&placeholder) {
                let abs_pos = search_start + pos;
                let placeholder_end = abs_pos + placeholder.len();

                if delete_byte_offset == placeholder_end - 1 {
                    return self.delete_image_placeholder_at(img_idx, abs_pos, &text);
                }

                search_start = abs_pos + 1;
            }
        }

        false
    }

    /// Converts cursor position (row, col) to byte offset in text.
    fn cursor_to_byte_offset(text: &str, row: usize, col: usize) -> usize {
        let mut offset = 0;
        for (i, line) in text.lines().enumerate() {
            if i < row {
                offset += line.len() + 1; // +1 for newline
            } else {
                // col is in char units, need to convert to bytes
                offset += line.chars().take(col).map(char::len_utf8).sum::<usize>();
                break;
            }
        }
        // Handle case where cursor is on a line past the last line (empty line at end)
        if text.ends_with('\n') && row == text.lines().count() {
            offset = text.len();
        }
        offset
    }

    /// Converts byte offset to cursor position (row, col).
    fn byte_offset_to_cursor(text: &str, byte_offset: usize) -> (usize, usize) {
        let mut row = 0;
        let mut current_line_start = 0;

        for (i, line) in text.lines().enumerate() {
            let line_end = current_line_start + line.len();
            if byte_offset <= line_end {
                // Cursor is on this line
                let col = text[current_line_start..byte_offset].chars().count();
                return (i, col);
            }
            current_line_start = line_end + 1; // +1 for newline
            row = i + 1;
        }

        // If byte_offset is at or past end, put cursor at end of last line
        let last_line_chars = text.lines().last().map_or(0, |l| l.chars().count());
        (row.saturating_sub(1), last_line_chars)
    }

    /// Finds if cursor should jump over a placeholder when navigating.
    ///
    /// For Right arrow: if cursor is at or inside a placeholder [start, end),
    /// returns the byte position after the placeholder (end).
    ///
    /// For Left arrow: if cursor is inside or at end of placeholder (start, end],
    /// returns the byte position at the start of the placeholder.
    ///
    /// Returns `None` if cursor is not in a position that requires jumping.
    fn find_placeholder_jump_target(
        &self,
        text: &str,
        cursor_byte: usize,
        is_left: bool,
    ) -> Option<usize> {
        let all_placeholders = self.all_placeholder_strings();
        for placeholder in &all_placeholders {
            let mut search_start = 0;
            while let Some(pos) = text[search_start..].find(placeholder) {
                let start = search_start + pos;
                let end = start + placeholder.len();

                if is_left {
                    // Moving left: jump to start if cursor is in (start, end]
                    if cursor_byte > start && cursor_byte <= end {
                        return Some(start);
                    }
                } else {
                    // Moving right: jump to end if cursor is in [start, end)
                    if cursor_byte >= start && cursor_byte < end {
                        return Some(end);
                    }
                }

                search_start = start + 1;
            }
        }
        None
    }

    /// Finds if cursor is inside a placeholder and returns the end byte position.
    ///
    /// Used after Up/Down cursor movement to snap cursor to placeholder end.
    /// Returns `Some(end)` if cursor is strictly inside a placeholder [start, end),
    /// `None` otherwise.
    fn find_placeholder_end_if_inside(&self, text: &str, cursor_byte: usize) -> Option<usize> {
        let all_placeholders = self.all_placeholder_strings();
        for placeholder in &all_placeholders {
            let mut search_start = 0;
            while let Some(pos) = text[search_start..].find(placeholder) {
                let start = search_start + pos;
                let end = start + placeholder.len();

                // Cursor is inside placeholder if in [start, end)
                if cursor_byte >= start && cursor_byte < end {
                    return Some(end);
                }

                search_start = start + 1;
            }
        }
        None
    }

    /// Finds which placeholder the cursor is inside, if any.
    ///
    /// Returns `Some((paste_idx, byte_start, byte_end))` if cursor is inside a placeholder,
    /// where `paste_idx` is the index in `pending_pastes` and byte positions are for the
    /// placeholder occurrence in the text.
    fn find_placeholder_at_cursor(
        &self,
        text: &str,
        cursor_byte: usize,
    ) -> Option<(usize, usize, usize)> {
        for (paste_idx, paste) in self.pending_pastes.iter().enumerate() {
            let mut search_start = 0;
            while let Some(pos) = text[search_start..].find(&paste.placeholder) {
                let start = search_start + pos;
                let end = start + paste.placeholder.len();

                // Cursor is inside placeholder if in [start, end)
                if cursor_byte >= start && cursor_byte < end {
                    return Some((paste_idx, start, end));
                }

                search_start = start + 1;
            }
        }
        None
    }

    /// Snaps cursor to placeholder end if it landed inside a placeholder.
    ///
    /// Called after Up/Down arrow movement to ensure the cursor doesn't
    /// land in the middle of a placeholder. If cursor is inside a placeholder,
    /// moves it to the position after the closing `]`.
    ///
    /// Returns `true` if cursor was snapped, `false` otherwise.
    pub fn snap_to_placeholder_end(&mut self) -> bool {
        if self.pending_pastes.is_empty() && self.pending_images.is_empty() {
            return false;
        }

        let text = self.get_text();
        let (row, col) = self.textarea.cursor();
        let cursor_byte = Self::cursor_to_byte_offset(&text, row, col);

        let Some(target_byte) = self.find_placeholder_end_if_inside(&text, cursor_byte) else {
            return false;
        };

        // Move cursor to end of placeholder
        let (new_row, new_col) = Self::byte_offset_to_cursor(&text, target_byte);

        self.textarea.move_cursor(CursorMove::Top);
        self.textarea.move_cursor(CursorMove::Head);
        for _ in 0..new_row {
            self.textarea.move_cursor(CursorMove::Down);
        }
        for _ in 0..new_col {
            self.textarea.move_cursor(CursorMove::Forward);
        }

        true
    }

    /// Attempts to jump the cursor over a placeholder when navigating with arrow keys.
    ///
    /// When the cursor is inside or at the boundary of a placeholder:
    /// - Right arrow: jumps cursor to position after the closing `]`
    /// - Left arrow: jumps cursor to position before the opening `[`
    ///
    /// Returns `true` if a jump occurred (caller should skip normal arrow handling),
    /// `false` otherwise (caller should proceed with normal cursor movement).
    pub fn try_jump_over_placeholder(&mut self, is_left: bool) -> bool {
        if self.pending_pastes.is_empty() && self.pending_images.is_empty() {
            return false;
        }

        let text = self.get_text();
        let (row, col) = self.textarea.cursor();
        let cursor_byte = Self::cursor_to_byte_offset(&text, row, col);

        let Some(target_byte) = self.find_placeholder_jump_target(&text, cursor_byte, is_left)
        else {
            return false;
        };

        // Move cursor to target position
        let (new_row, new_col) = Self::byte_offset_to_cursor(&text, target_byte);

        self.textarea.move_cursor(CursorMove::Top);
        self.textarea.move_cursor(CursorMove::Head);
        for _ in 0..new_row {
            self.textarea.move_cursor(CursorMove::Down);
        }
        for _ in 0..new_col {
            self.textarea.move_cursor(CursorMove::Forward);
        }

        true
    }

    /// Deletes a placeholder at the given byte position and updates cursor.
    fn delete_placeholder_at(&mut self, paste_idx: usize, byte_start: usize, text: &str) -> bool {
        let placeholder = &self.pending_pastes[paste_idx].placeholder;
        let byte_end = byte_start + placeholder.len();

        // Build new text without the placeholder
        let new_text = format!("{}{}", &text[..byte_start], &text[byte_end..]);

        // Remove from pending_pastes
        self.pending_pastes.remove(paste_idx);

        // Calculate new cursor position (at the start of where placeholder was)
        let (new_row, new_col) = Self::byte_offset_to_cursor(&new_text, byte_start);

        // Set the new text
        self.textarea.select_all();
        self.textarea.cut();
        if !new_text.is_empty() {
            self.textarea.insert_str(&new_text);
        }

        // Position cursor at where the placeholder started
        self.textarea.move_cursor(CursorMove::Top);
        self.textarea.move_cursor(CursorMove::Head);
        for _ in 0..new_row {
            self.textarea.move_cursor(CursorMove::Down);
        }
        for _ in 0..new_col {
            self.textarea.move_cursor(CursorMove::Forward);
        }

        true
    }

    /// Deletes an image placeholder at the given byte position and updates cursor.
    fn delete_image_placeholder_at(
        &mut self,
        img_idx: usize,
        byte_start: usize,
        text: &str,
    ) -> bool {
        let placeholder = &self.pending_images[img_idx].placeholder;
        let byte_end = byte_start + placeholder.len();

        // Build new text without the placeholder
        let new_text = format!("{}{}", &text[..byte_start], &text[byte_end..]);

        // Remove from pending_images
        self.pending_images.remove(img_idx);

        // Calculate new cursor position (at the start of where placeholder was)
        let (new_row, new_col) = Self::byte_offset_to_cursor(&new_text, byte_start);

        // Set the new text
        self.textarea.select_all();
        self.textarea.cut();
        if !new_text.is_empty() {
            self.textarea.insert_str(&new_text);
        }

        // Position cursor at where the placeholder started
        self.textarea.move_cursor(CursorMove::Top);
        self.textarea.move_cursor(CursorMove::Head);
        for _ in 0..new_row {
            self.textarea.move_cursor(CursorMove::Down);
        }
        for _ in 0..new_col {
            self.textarea.move_cursor(CursorMove::Forward);
        }

        true
    }

    /// Expands a placeholder at the given byte position to its original content.
    ///
    /// Replaces the placeholder text with the stored original content and
    /// positions the cursor at the end of the expanded content.
    fn expand_placeholder_at(&mut self, paste_idx: usize, byte_start: usize, text: &str) -> bool {
        let placeholder = &self.pending_pastes[paste_idx].placeholder;
        let content = &self.pending_pastes[paste_idx].content;
        let byte_end = byte_start + placeholder.len();

        // Build new text with placeholder replaced by original content
        let new_text = format!("{}{}{}", &text[..byte_start], content, &text[byte_end..]);

        // Calculate cursor position at end of expanded content
        let cursor_byte = byte_start + content.len();

        // Remove from pending_pastes
        self.pending_pastes.remove(paste_idx);

        // Calculate new cursor position (at end of expanded content)
        let (new_row, new_col) = Self::byte_offset_to_cursor(&new_text, cursor_byte);

        // Set the new text
        self.textarea.select_all();
        self.textarea.cut();
        if !new_text.is_empty() {
            self.textarea.insert_str(&new_text);
        }

        // Position cursor at end of expanded content
        self.textarea.move_cursor(CursorMove::Top);
        self.textarea.move_cursor(CursorMove::Head);
        for _ in 0..new_row {
            self.textarea.move_cursor(CursorMove::Down);
        }
        for _ in 0..new_col {
            self.textarea.move_cursor(CursorMove::Forward);
        }

        true
    }

    /// Attempts to expand a placeholder if the cursor is inside one.
    ///
    /// When the user presses Space while the cursor is inside a placeholder like
    /// `[Pasted Content 1234 chars #1]`, this replaces the placeholder with its
    /// original content and positions the cursor at the end of the expanded text.
    ///
    /// Returns `true` if a placeholder was expanded (caller should skip normal key handling),
    /// `false` otherwise (caller should proceed with normal Space handling).
    pub fn try_expand_placeholder_at_cursor(&mut self) -> bool {
        if self.pending_pastes.is_empty() {
            return false;
        }

        let text = self.get_text();
        let (row, col) = self.textarea.cursor();
        let cursor_byte = Self::cursor_to_byte_offset(&text, row, col);

        let Some((paste_idx, byte_start, _byte_end)) =
            self.find_placeholder_at_cursor(&text, cursor_byte)
        else {
            return false;
        };

        self.expand_placeholder_at(paste_idx, byte_start, &text)
    }

    /// Generates the next unique paste ID (simple incrementing number).
    pub fn next_paste_id(&mut self) -> String {
        self.paste_counter = self.paste_counter.wrapping_add(1);
        self.paste_counter.to_string()
    }

    pub fn all_placeholder_strings(&self) -> Vec<String> {
        let mut placeholders: Vec<String> = self
            .pending_pastes
            .iter()
            .map(|p| p.placeholder.clone())
            .collect();
        placeholders.extend(
            self.pending_images
                .iter()
                .map(|img| img.placeholder.clone()),
        );
        placeholders
    }

    pub fn next_image_id(&mut self) -> String {
        self.image_counter = self.image_counter.wrapping_add(1);
        self.image_counter.to_string()
    }

    pub fn generate_image_placeholder(id: &str) -> String {
        format!("[Image #{id}]")
    }

    pub fn attach_image(&mut self, mime_type: String, data: String, source_path: Option<String>) {
        let id = self.next_image_id();
        let placeholder = Self::generate_image_placeholder(&id);
        self.pending_images.push(PendingImage {
            id,
            placeholder: placeholder.clone(),
            mime_type,
            data,
            source_path,
        });
        self.textarea.insert_str(&placeholder);
    }

    pub fn has_images(&self) -> bool {
        !self.pending_images.is_empty()
    }

    pub fn take_images(&mut self) -> Vec<PendingImage> {
        std::mem::take(&mut self.pending_images)
    }

    pub fn clear_images(&mut self) {
        self.pending_images.clear();
    }

    /// Resets the image counter (call on new thread only).
    pub fn reset_image_counter(&mut self) {
        self.image_counter = 0;
    }

    pub fn sync_pending_images(&mut self) {
        if self.pending_images.is_empty() {
            return;
        }
        let text = self.get_text();
        self.pending_images
            .retain(|img| text.contains(&img.placeholder));
    }

    /// Generates a placeholder string for a large paste.
    ///
    /// Format: `[Pasted Content N chars #xxxxxxxx]`
    pub fn generate_placeholder(char_count: usize, id: &str) -> String {
        format!("[Pasted Content {char_count} chars #{id}]")
    }

    /// Clears the input textarea.
    pub fn clear(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
        self.reset_navigation();
        self.clear_pending_pastes();
    }

    /// Sets the input textarea to the given text.
    pub fn set_text(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
        self.clear_pending_pastes();
    }

    /// Applies a cross-slice input mutation.
    pub fn apply(&mut self, mutation: InputMutation) {
        match mutation {
            InputMutation::Clear => self.clear(),
            InputMutation::SetText(text) => self.set_text(&text),
            InputMutation::InsertChar(ch) => {
                self.textarea.insert_char(ch);
                self.sync_pending_pastes();
            }
            InputMutation::SetTextAndCursor {
                text,
                cursor_row,
                cursor_col,
            } => {
                self.set_text(&text);
                self.textarea.move_cursor(CursorMove::Top);
                self.textarea.move_cursor(CursorMove::Head);
                for _ in 0..cursor_row {
                    self.textarea.move_cursor(CursorMove::Down);
                }
                for _ in 0..cursor_col {
                    self.textarea.move_cursor(CursorMove::Forward);
                }
            }
            InputMutation::SetHistory(history) => {
                self.history = history;
                self.reset_navigation();
            }
            InputMutation::ClearHistory => self.clear_history(),
            InputMutation::ClearQueue => self.queued.clear(),
            InputMutation::SetHandoffState(state) => {
                self.handoff.cancel();
                self.handoff = state;
            }
            InputMutation::AttachImage {
                mime_type,
                data,
                source_path,
            } => {
                self.attach_image(mime_type, data, source_path);
            }
            InputMutation::ResetImageCounter => {
                self.reset_image_counter();
            }
        }
    }

    /// Resets history navigation state.
    pub fn reset_navigation(&mut self) {
        self.history_index = None;
        self.draft = None;
    }

    /// Clears command history (for /new, handoff submit).
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.reset_navigation();
    }

    /// Enqueues a prompt for later sending.
    pub fn enqueue_prompt(&mut self, text: String) {
        self.queued.push_back(text);
    }

    /// Pops the next queued prompt, if any.
    pub fn pop_queued_prompt(&mut self) -> Option<String> {
        self.queued.pop_front()
    }

    /// Returns true if there are queued prompts.
    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty()
    }

    /// Returns a display-friendly summary of queued prompts.
    ///
    /// Returns the first line of each queued prompt without truncation.
    /// Truncation is handled at render time using unicode-aware width calculation.
    pub fn queued_summaries(&self, max_items: usize) -> Vec<String> {
        self.queued
            .iter()
            .take(max_items)
            .map(|item| item.lines().next().unwrap_or("").to_string())
            .collect()
    }

    /// Returns true if up arrow should navigate history (not move cursor).
    pub fn should_navigate_up(&self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        if self.history_index.is_some() {
            return true;
        }
        if self.get_text().is_empty() {
            return true;
        }
        let (row, _col) = self.textarea.cursor();
        row == 0
    }

    /// Returns true if down arrow should navigate history (not move cursor).
    pub fn should_navigate_down(&self) -> bool {
        if self.history_index.is_none() {
            return false;
        }
        let (row, _col) = self.textarea.cursor();
        let line_count = self.textarea.lines().len();
        row >= line_count.saturating_sub(1)
    }

    /// Navigates up in command history.
    pub fn navigate_up(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if self.history_index.is_none() {
            let current = self.get_text();
            self.draft = Some(current);
            self.history_index = Some(self.history.len() - 1);
        } else if let Some(idx) = self.history_index
            && idx > 0
        {
            self.history_index = Some(idx - 1);
        }

        if let Some(idx) = self.history_index
            && let Some(entry) = self.history.get(idx).cloned()
        {
            self.set_text(&entry);
        }
    }

    /// Navigates down in command history.
    pub fn navigate_down(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };

        if idx + 1 < self.history.len() {
            self.history_index = Some(idx + 1);
            if let Some(entry) = self.history.get(idx + 1).cloned() {
                self.set_text(&entry);
            }
        } else {
            let draft = self.draft.take().unwrap_or_default();
            self.history_index = None;
            self.set_text(&draft);
        }
    }
}
