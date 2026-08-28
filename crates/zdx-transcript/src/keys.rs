//! Keyboard interaction helpers shared by the ZDX TUIs.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Vim-style `g` prefix shared by scrollable surfaces: `gg` is delivered as
/// `Home` (every scroll surface binds `Home`/`End`), and an unknown `g`
/// sequence is swallowed like vim aborts it. Keys with Ctrl/Alt bypass the
/// prefix so chords like Ctrl+C keep working mid-sequence.
///
/// Surfaces with a type-to-filter input (pickers, palettes) must not use it.
#[derive(Debug, Clone, Copy, Default)]
pub struct GPrefix {
    pending: bool,
}

impl GPrefix {
    /// Translates a key; `None` means the prefix consumed it.
    pub fn translate(&mut self, key: KeyEvent) -> Option<KeyCode> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.pending = false;
            return Some(key.code);
        }
        if self.pending {
            self.pending = false;
            return (key.code == KeyCode::Char('g')).then_some(KeyCode::Home);
        }
        if key.code == KeyCode::Char('g') {
            self.pending = true;
            return None;
        }
        Some(key.code)
    }

    /// Drops any pending prefix (e.g. when focus moves to a text input).
    pub fn reset(&mut self) {
        self.pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// `gg` must arrive as a single `Home`, and an unknown `g` sequence must
    /// be swallowed like vim aborts it.
    #[test]
    fn translates_gg_and_aborts_unknown_sequences() {
        let mut prefix = GPrefix::default();
        assert_eq!(prefix.translate(key(KeyCode::Char('g'))), None);
        assert_eq!(
            prefix.translate(key(KeyCode::Char('g'))),
            Some(KeyCode::Home)
        );

        assert_eq!(prefix.translate(key(KeyCode::Char('g'))), None);
        assert_eq!(prefix.translate(key(KeyCode::Char('j'))), None);

        assert_eq!(
            prefix.translate(key(KeyCode::Char('G'))),
            Some(KeyCode::Char('G'))
        );
    }

    /// Ctrl/Alt chords bypass and clear a pending prefix.
    #[test]
    fn control_chords_bypass_pending_prefix() {
        let mut prefix = GPrefix::default();
        assert_eq!(prefix.translate(key(KeyCode::Char('g'))), None);
        let chord = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(prefix.translate(chord), Some(KeyCode::Char('c')));
        // Prefix was cleared by the chord.
        assert_eq!(
            prefix.translate(key(KeyCode::Char('g'))),
            None,
            "fresh prefix restarts"
        );
    }
}
