//! Overlay modules for the TUI.
//!
//! Overlays are modal UI components that temporarily take over keyboard input.
//! Each overlay is self-contained: it owns its state, key handler, and render function.
//!
//! ## Module Structure
//!
//! - `command_palette.rs`: Command palette for slash commands
//! - `model_picker.rs`: Model selection picker
//! - `thinking_picker.rs`: Thinking level selection picker
//! - `session_picker.rs`: Session history picker
//! - `login.rs`: OAuth login flow overlay
//! - `file_picker.rs`: File picker triggered by `@`
//! - `view.rs`: Shared rendering utilities for overlays
//! - `update.rs`: Overlay key handling and update logic
//!
//! ## Extension Trait
//!
//! `OverlayExt` provides convenience methods for `Option<Overlay>` to encapsulate
//! the common patterns used in the reducer.

pub mod command_palette;
pub mod file_picker;
pub mod login;
pub mod model_picker;
pub mod session_picker;
pub mod thinking_picker;
mod update;
pub mod view;

pub use command_palette::CommandPaletteState;
use crossterm::event::KeyEvent;
pub use file_picker::{FilePickerState, discover_files};
pub use login::LoginState;
pub use model_picker::ModelPickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
pub use session_picker::SessionPickerState;
pub use thinking_picker::ThinkingPickerState;
// Re-export update functions
pub use update::{handle_files_discovered, handle_overlay_key};

use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::StateCommand;
use crate::modes::tui::app::TuiState;

// ============================================================================
// OverlayAction
// ============================================================================

/// Action returned by overlay key handlers.
///
/// - `None` = continue with overlay open, no effects
/// - `Some(Close(effects))` = close overlay, execute effects
/// - `Some(Effects(effects))` = stay open but run effects
#[derive(Debug)]
pub enum OverlayAction {
    Close(Vec<UiEffect>),
    Effects(Vec<UiEffect>),
}

impl OverlayAction {
    pub fn close() -> Self {
        OverlayAction::Close(vec![])
    }

    pub fn close_with(effects: Vec<UiEffect>) -> Self {
        OverlayAction::Close(effects)
    }
}

// ============================================================================
// Overlay
// ============================================================================

#[derive(Debug, Clone)]
pub enum Overlay {
    CommandPalette(CommandPaletteState),
    ModelPicker(ModelPickerState),
    ThinkingPicker(ThinkingPickerState),
    SessionPicker(SessionPickerState),
    Login(LoginState),
    FilePicker(FilePickerState),
}

impl Overlay {
    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        match self {
            Overlay::CommandPalette(p) => p.render(frame, area, input_y),
            Overlay::ModelPicker(p) => p.render(frame, area, input_y),
            Overlay::ThinkingPicker(p) => p.render(frame, area, input_y),
            Overlay::SessionPicker(p) => p.render(frame, area, input_y),
            Overlay::FilePicker(p) => p.render(frame, area, input_y),
            Overlay::Login(l) => l.render(frame, area, input_y),
        }
    }

    pub fn handle_key(
        &mut self,
        tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<OverlayAction>, Vec<StateCommand>) {
        match self {
            Overlay::CommandPalette(p) => p.handle_key(tui, key),
            Overlay::ModelPicker(p) => p.handle_key(tui, key),
            Overlay::ThinkingPicker(p) => p.handle_key(tui, key),
            Overlay::SessionPicker(p) => p.handle_key(tui, key),
            Overlay::FilePicker(p) => p.handle_key(&tui.input, key),
            Overlay::Login(l) => l.handle_key(tui, key),
        }
    }

    pub fn as_file_picker_mut(&mut self) -> Option<&mut FilePickerState> {
        match self {
            Overlay::FilePicker(p) => Some(p),
            _ => None,
        }
    }
}

// ============================================================================
// OverlayExt - Extension trait for Option<Overlay>
// ============================================================================

/// Extension trait for `Option<Overlay>` providing convenience methods.
///
/// This trait encapsulates the common overlay handling patterns used in the
/// reducer, making the main key handling logic cleaner.
pub trait OverlayExt {
    /// Handles a key event if an overlay is active.
    ///
    /// Returns `(Some(effects), commands)` if the overlay handled the key (the
    /// overlay may have been closed), or `(None, [])` if no overlay was active.
    ///
    /// This method:
    /// - Dispatches the key to the active overlay's handler
    /// - Closes the overlay if `OverlayAction::Close` is returned
    /// - Returns any effects to be executed
    fn handle_key(
        &mut self,
        tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<Vec<UiEffect>>, Vec<StateCommand>);

    /// Renders the overlay if one is active.
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16);
}

impl OverlayExt for Option<Overlay> {
    fn handle_key(
        &mut self,
        tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<Vec<UiEffect>>, Vec<StateCommand>) {
        let Some(overlay) = self.as_mut() else {
            return (None, vec![]);
        };

        let (action, commands) = overlay.handle_key(tui, key);
        let effects = match action {
            None => vec![],
            Some(OverlayAction::Close(effects)) => {
                *self = None;
                effects
            }
            Some(OverlayAction::Effects(effects)) => effects,
        };

        (Some(effects), commands)
    }

    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        if let Some(overlay) = self {
            overlay.render(frame, area, input_y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThinkingLevel;

    #[test]
    fn test_overlay_is_some() {
        let none: Option<Overlay> = None;
        assert!(none.is_none());

        let (palette, _) = CommandPaletteState::open(true);
        let overlay: Option<Overlay> = Some(Overlay::CommandPalette(palette));
        assert!(overlay.is_some());

        let (picker, _) = ModelPickerState::open("test");
        let overlay: Option<Overlay> = Some(Overlay::ModelPicker(picker));
        assert!(overlay.is_some());

        let (thinking, _) = ThinkingPickerState::open(ThinkingLevel::Off);
        let overlay: Option<Overlay> = Some(Overlay::ThinkingPicker(thinking));
        assert!(overlay.is_some());

        let overlay: Option<Overlay> = Some(Overlay::Login(LoginState::Exchanging));
        assert!(overlay.is_some());

        let (file_picker, _) = FilePickerState::open(0);
        let overlay: Option<Overlay> = Some(Overlay::FilePicker(file_picker));
        assert!(overlay.is_some());
    }
}
