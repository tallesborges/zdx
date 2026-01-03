//! Overlay modules for the TUI.

pub mod command_palette;
pub mod file_picker;
pub mod login;
pub mod model_picker;
pub mod session_picker;
pub mod thinking_picker;

pub use command_palette::CommandPaletteState;
use crossterm::event::KeyEvent;
pub use file_picker::{FilePickerState, discover_files};
pub use login::{LoginState, handle_login_result};
pub use model_picker::ModelPickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
pub use session_picker::SessionPickerState;
pub use thinking_picker::ThinkingPickerState;

use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;

// ============================================================================
// OverlayAction
// ============================================================================

/// Action returned by overlay key handlers.
///
/// - `None` = continue with overlay open, no effects
/// - `Some(Close(effects))` = close overlay, execute effects
/// - `Some(Transition { ... })` = transition to new overlay state
/// - `Some(Effects(effects))` = stay open but run effects
#[derive(Debug)]
pub enum OverlayAction {
    Close(Vec<UiEffect>),
    Transition {
        new_overlay: Overlay,
        effects: Vec<UiEffect>,
    },
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

impl From<CommandPaletteState> for Overlay {
    fn from(state: CommandPaletteState) -> Self {
        Overlay::CommandPalette(state)
    }
}

impl From<ModelPickerState> for Overlay {
    fn from(state: ModelPickerState) -> Self {
        Overlay::ModelPicker(state)
    }
}

impl From<ThinkingPickerState> for Overlay {
    fn from(state: ThinkingPickerState) -> Self {
        Overlay::ThinkingPicker(state)
    }
}

impl From<SessionPickerState> for Overlay {
    fn from(state: SessionPickerState) -> Self {
        Overlay::SessionPicker(state)
    }
}

impl From<LoginState> for Overlay {
    fn from(state: LoginState) -> Self {
        Overlay::Login(state)
    }
}

impl From<FilePickerState> for Overlay {
    fn from(state: FilePickerState) -> Self {
        Overlay::FilePicker(state)
    }
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

    pub fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        match self {
            Overlay::CommandPalette(p) => p.handle_key(tui, key),
            Overlay::ModelPicker(p) => p.handle_key(tui, key),
            Overlay::ThinkingPicker(p) => p.handle_key(tui, key),
            Overlay::SessionPicker(p) => p.handle_key(tui, key),
            Overlay::FilePicker(p) => p.handle_key(tui, key),
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
// Test helpers
// ============================================================================

#[cfg(test)]
impl Overlay {
    pub fn as_command_palette(&self) -> Option<&CommandPaletteState> {
        match self {
            Overlay::CommandPalette(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_model_picker(&self) -> Option<&ModelPickerState> {
        match self {
            Overlay::ModelPicker(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_thinking_picker(&self) -> Option<&ThinkingPickerState> {
        match self {
            Overlay::ThinkingPicker(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_login(&self) -> Option<&LoginState> {
        match self {
            Overlay::Login(l) => Some(l),
            _ => None,
        }
    }
}
