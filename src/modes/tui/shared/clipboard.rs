//! Clipboard utilities for the TUI.
//!
//! Provides clipboard access with multiple transport fallbacks:
//! 1. OSC 52 - Terminal clipboard escape sequence (works over SSH)
//! 2. System clipboard via `arboard` crate

use std::io::Write;

/// Clipboard interface with multiple transport fallbacks.
pub struct Clipboard;

impl Clipboard {
    /// Copies text to the clipboard.
    ///
    /// Tries in order:
    /// 1. OSC 52 escape sequence (works over SSH)
    /// 2. System clipboard via arboard
    ///
    /// Returns `Ok(())` if any method succeeded.
    pub fn copy(text: &str) -> Result<(), ClipboardError> {
        // Try OSC 52 first (best for terminal apps, works over SSH)
        if Self::copy_osc52(text).is_ok() {
            return Ok(());
        }

        // Fall back to system clipboard
        Self::copy_system(text)
    }

    /// Copies text using OSC 52 escape sequence.
    ///
    /// This writes directly to stdout, which the terminal intercepts
    /// and copies to the system clipboard.
    fn copy_osc52(text: &str) -> Result<(), ClipboardError> {
        use base64::Engine;

        let encoded = base64::engine::general_purpose::STANDARD.encode(text);

        // OSC 52 format: ESC ] 52 ; c ; <base64-data> ESC \
        // - 'c' specifies the clipboard selection (system clipboard)
        let mut stdout = std::io::stdout();
        write!(stdout, "\x1b]52;c;{}\x1b\\", encoded)
            .map_err(|e| ClipboardError::Osc52(e.to_string()))?;
        stdout
            .flush()
            .map_err(|e| ClipboardError::Osc52(e.to_string()))?;

        Ok(())
    }

    /// Copies text using the system clipboard.
    fn copy_system(text: &str) -> Result<(), ClipboardError> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| ClipboardError::System(e.to_string()))?;

        clipboard
            .set_text(text)
            .map_err(|e| ClipboardError::System(e.to_string()))?;

        Ok(())
    }
}

/// Clipboard operation errors.
#[derive(Debug)]
pub enum ClipboardError {
    /// OSC 52 write failed.
    Osc52(String),
    /// System clipboard operation failed.
    System(String),
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipboardError::Osc52(msg) => write!(f, "OSC 52 clipboard failed: {}", msg),
            ClipboardError::System(msg) => write!(f, "System clipboard failed: {}", msg),
        }
    }
}

impl std::error::Error for ClipboardError {}
