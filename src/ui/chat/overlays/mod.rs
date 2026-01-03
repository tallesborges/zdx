//! Overlay modules for the TUI.
//!
//! Each overlay is self-contained with:
//! - **State struct** implementing the `Overlay` trait (e.g., `FilePickerState`)
//! - **Trait impl** with `type Config`, `open()`, `render()`, `handle_key()`
//! - **`From` impl** for conversion to `OverlayState`
//!
//! ## Opening Overlays
//!
//! Use `OverlayState::try_open::<T>(config)`:
//!
//! ```rust,ignore
//! let effects = overlay.try_open::<FilePickerState>(trigger_pos);
//! let effects = overlay.try_open::<LoginState>(());
//! let effects = overlay.try_open::<ModelPickerState>(current_model);
//! ```
//!
//! ## Split State Architecture
//!
//! To avoid borrow checker conflicts, state is split:
//! - `TuiState` contains all non-overlay UI state
//! - `OverlayState` is an enum of overlay variants (defined here)
//! - `AppState` combines both (defined in `state/mod.rs`)
//!
//! This allows overlay handlers to get clean access: `&mut self, &mut TuiState`.
//!
//! See `docs/ARCHITECTURE.md` "Overlay Contract" section for full details.

pub mod file_picker;
pub mod login;
pub mod model_picker;
pub mod palette;
pub mod session_picker;
pub mod thinking_picker;
mod traits;

// Re-export the Overlay trait and OverlayAction
// Re-export state types (open is now via the trait)
pub use file_picker::{FilePickerState, discover_files};
pub use login::{LoginState, handle_login_result};
pub use model_picker::ModelPickerState;
pub use palette::CommandPaletteState;
pub use session_picker::SessionPickerState;
pub use thinking_picker::ThinkingPickerState;
pub use traits::{Overlay, OverlayAction};

use crate::ui::chat::effects::UiEffect;

// ============================================================================
// OverlayState (unified overlay enum)
// ============================================================================

/// Unified overlay state.
///
/// Only one overlay can be active at a time. This eliminates the cascade of
/// `if palette.is_some() / if picker.is_some() / if login.is_active()` checks.
///
/// This type is separate from `TuiState` to enable the split state architecture
/// where overlay handlers get `&mut self` and `&mut TuiState` simultaneously.
#[derive(Debug, Clone)]
pub enum OverlayState {
    /// No overlay active.
    None,
    /// Command palette is open.
    CommandPalette(CommandPaletteState),
    /// Model picker is open.
    ModelPicker(ModelPickerState),
    /// Thinking level picker is open.
    ThinkingPicker(ThinkingPickerState),
    /// Session picker is open.
    SessionPicker(SessionPickerState),
    /// Login flow is active.
    Login(LoginState),
    /// File picker is open (triggered by `@`).
    FilePicker(FilePickerState),
}

// ============================================================================
// From impls for type-safe conversion
// ============================================================================

impl From<CommandPaletteState> for OverlayState {
    fn from(state: CommandPaletteState) -> Self {
        OverlayState::CommandPalette(state)
    }
}

impl From<ModelPickerState> for OverlayState {
    fn from(state: ModelPickerState) -> Self {
        OverlayState::ModelPicker(state)
    }
}

impl From<ThinkingPickerState> for OverlayState {
    fn from(state: ThinkingPickerState) -> Self {
        OverlayState::ThinkingPicker(state)
    }
}

impl From<SessionPickerState> for OverlayState {
    fn from(state: SessionPickerState) -> Self {
        OverlayState::SessionPicker(state)
    }
}

impl From<LoginState> for OverlayState {
    fn from(state: LoginState) -> Self {
        OverlayState::Login(state)
    }
}

impl From<FilePickerState> for OverlayState {
    fn from(state: FilePickerState) -> Self {
        OverlayState::FilePicker(state)
    }
}

// ============================================================================
// OverlayState methods
// ============================================================================

impl OverlayState {
    /// Opens an overlay if none is currently active.
    ///
    /// Uses the `Overlay` trait's `open()` method to create the state,
    /// then converts it to `OverlayState` via `From`.
    ///
    /// Returns effects to execute (e.g., async file discovery, open browser).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let effects = overlay.try_open::<FilePickerState>(trigger_pos);
    /// let effects = overlay.try_open::<LoginState>(());
    /// ```
    pub fn try_open<T>(&mut self, config: T::Config) -> Vec<UiEffect>
    where
        T: Overlay + Into<OverlayState>,
    {
        if !matches!(self, OverlayState::None) {
            return vec![];
        }

        let (state, effects) = T::open(config);
        *self = state.into();
        effects
    }

    /// Returns true if any overlay is active.
    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        !matches!(self, OverlayState::None)
    }

    /// Returns the command palette state if active.
    #[cfg(test)]
    pub fn as_command_palette(&self) -> Option<&CommandPaletteState> {
        match self {
            OverlayState::CommandPalette(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the model picker state if active.
    #[cfg(test)]
    pub fn as_model_picker(&self) -> Option<&ModelPickerState> {
        match self {
            OverlayState::ModelPicker(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the thinking picker state if active.
    #[cfg(test)]
    pub fn as_thinking_picker(&self) -> Option<&ThinkingPickerState> {
        match self {
            OverlayState::ThinkingPicker(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the login state if active.
    #[cfg(test)]
    pub fn as_login(&self) -> Option<&LoginState> {
        match self {
            OverlayState::Login(l) => Some(l),
            _ => None,
        }
    }

    /// Returns the file picker state mutably if active.
    pub fn as_file_picker_mut(&mut self) -> Option<&mut FilePickerState> {
        match self {
            OverlayState::FilePicker(p) => Some(p),
            _ => None,
        }
    }

    /// Renders the active overlay using the `Overlay` trait.
    ///
    /// Does nothing if no overlay is active. This provides a uniform
    /// rendering interface that delegates to each overlay's trait implementation.
    pub fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect, input_y: u16) {
        match self {
            OverlayState::None => {}
            OverlayState::CommandPalette(p) => p.render(frame, area, input_y),
            OverlayState::ModelPicker(p) => p.render(frame, area, input_y),
            OverlayState::ThinkingPicker(p) => p.render(frame, area, input_y),
            OverlayState::SessionPicker(p) => p.render(frame, area, input_y),
            OverlayState::FilePicker(p) => p.render(frame, area, input_y),
            OverlayState::Login(l) => l.render(frame, area, input_y),
        }
    }

    /// Handles a key event for the active overlay.
    ///
    /// Returns:
    /// - `None` if no overlay is active (key not consumed)
    /// - `Some(None)` if overlay consumed key but continues
    /// - `Some(Some(action))` if overlay returned an action (close/transition)
    pub fn handle_key(
        &mut self,
        tui: &mut crate::ui::chat::state::TuiState,
        key: crossterm::event::KeyEvent,
    ) -> Option<Option<OverlayAction>> {
        match self {
            OverlayState::None => None,
            OverlayState::CommandPalette(p) => Some(p.handle_key(tui, key)),
            OverlayState::ModelPicker(p) => Some(p.handle_key(tui, key)),
            OverlayState::ThinkingPicker(p) => Some(p.handle_key(tui, key)),
            OverlayState::SessionPicker(p) => Some(p.handle_key(tui, key)),
            OverlayState::FilePicker(p) => Some(p.handle_key(tui, key)),
            OverlayState::Login(l) => Some(l.handle_key(tui, key)),
        }
    }
}
