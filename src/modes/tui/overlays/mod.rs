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
pub mod render_utils;
pub mod session_picker;
pub mod thinking_picker;
pub mod timeline;
mod update;

pub use command_palette::CommandPaletteState;
use crossterm::event::KeyEvent;
pub use file_picker::{FilePickerState, discover_files};
pub use login::LoginState;
pub use model_picker::ModelPickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
pub use session_picker::SessionPickerState;
pub use thinking_picker::ThinkingPickerState;
pub use timeline::TimelineState;
// Re-export update functions
pub use update::{handle_files_discovered, handle_overlay_key};

use crate::modes::tui::app::TuiState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::StateCommand;

// ============================================================================
// OverlayRequest / OverlayAction
// ============================================================================

/// Requests to open a new overlay.
#[derive(Debug)]
pub enum OverlayRequest {
    CommandPalette { command_mode: bool },
    ModelPicker,
    ThinkingPicker,
    Login,
    FilePicker { trigger_pos: usize },
    Timeline,
}

/// Action returned by overlay key handlers.
///
/// - `None` = continue with overlay open, no effects
/// - `Some(Close(effects))` = close overlay, execute effects
/// - `Some(Effects(effects))` = stay open but run effects
/// - `Some(Open(request))` = replace overlay with a new one
#[derive(Debug)]
pub enum OverlayAction {
    Close(Vec<UiEffect>),
    Effects(Vec<UiEffect>),
    Open(OverlayRequest),
}

impl OverlayAction {
    pub fn close() -> Self {
        OverlayAction::Close(vec![])
    }

    pub fn close_with(effects: Vec<UiEffect>) -> Self {
        OverlayAction::Close(effects)
    }

    pub fn open(request: OverlayRequest) -> Self {
        OverlayAction::Open(request)
    }
}

// ============================================================================
// Overlay
// ============================================================================

#[derive(Debug)]
pub enum Overlay {
    CommandPalette(CommandPaletteState),
    ModelPicker(ModelPickerState),
    ThinkingPicker(ThinkingPickerState),
    SessionPicker(SessionPickerState),
    Login(LoginState),
    FilePicker(FilePickerState),
    Timeline(TimelineState),
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
            Overlay::Timeline(t) => t.render(frame, area, input_y),
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
            Overlay::Timeline(t) => t.handle_key(tui, key),
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

/// Extension trait for `Option<Overlay>` providing convenience render helpers.
pub trait OverlayExt {
    /// Renders the overlay if one is active.
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16);
}

impl OverlayExt for Option<Overlay> {
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
        use crate::modes::tui::transcript::ScrollMode;

        let none: Option<Overlay> = None;
        assert!(none.is_none());

        let (palette, _) =
            CommandPaletteState::open(true, crate::providers::ProviderKind::Anthropic);
        let overlay: Option<Overlay> = Some(Overlay::CommandPalette(palette));
        assert!(overlay.is_some());

        let (picker, _) = ModelPickerState::open("test");
        let overlay: Option<Overlay> = Some(Overlay::ModelPicker(picker));
        assert!(overlay.is_some());

        let (thinking, _) = ThinkingPickerState::open(ThinkingLevel::Off);
        let overlay: Option<Overlay> = Some(Overlay::ThinkingPicker(thinking));
        assert!(overlay.is_some());

        let overlay: Option<Overlay> = Some(Overlay::Login(LoginState::Exchanging {
            provider: crate::providers::ProviderKind::Anthropic,
        }));
        assert!(overlay.is_some());

        let (file_picker, _) = FilePickerState::open(0);
        let overlay: Option<Overlay> = Some(Overlay::FilePicker(file_picker));
        assert!(overlay.is_some());

        let (timeline, _, _) = TimelineState::open(&[], &[], ScrollMode::FollowLatest);
        let overlay: Option<Overlay> = Some(Overlay::Timeline(timeline));
        assert!(overlay.is_some());
    }
}
