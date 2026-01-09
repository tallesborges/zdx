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
//! - `thread_picker.rs`: Thread history picker
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
pub mod thinking_picker;
pub mod thread_picker;
pub mod timeline;
mod update;

pub use command_palette::CommandPaletteState;
use crossterm::event::KeyEvent;
pub use file_picker::{FilePickerState, discover_files};
pub use login::LoginState;
pub use model_picker::ModelPickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
pub use thinking_picker::ThinkingPickerState;
pub use thread_picker::{ThreadPickerState, ThreadScope};
pub use timeline::TimelineState;
// Re-export update functions
pub use update::{handle_files_discovered, handle_overlay_key};

use crate::modes::tui::app::TuiState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::StateMutation;

// ============================================================================
// OverlayRequest / OverlayTransition / OverlayUpdate
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

/// Transition returned by overlay key handlers.
#[derive(Debug)]
pub enum OverlayTransition {
    Stay,
    Close,
    Open(OverlayRequest),
}

/// Update returned by overlay key handlers.
#[derive(Debug)]
pub struct OverlayUpdate {
    pub transition: OverlayTransition,
    pub mutations: Vec<StateMutation>,
    pub effects: Vec<OverlayEffect>,
}

impl OverlayUpdate {
    fn new(transition: OverlayTransition) -> Self {
        Self {
            transition,
            mutations: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub fn stay() -> Self {
        Self::new(OverlayTransition::Stay)
    }

    pub fn close() -> Self {
        Self::new(OverlayTransition::Close)
    }

    pub fn open(request: OverlayRequest) -> Self {
        Self::new(OverlayTransition::Open(request))
    }

    pub fn with_mutations(mut self, mutations: Vec<StateMutation>) -> Self {
        self.mutations = mutations;
        self
    }

    pub fn with_ui_effects(mut self, effects: Vec<UiEffect>) -> Self {
        self.effects = effects.into_iter().map(OverlayEffect::Ui).collect();
        self
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
    ThreadPicker(ThreadPickerState),
    Login(LoginState),
    FilePicker(FilePickerState),
    Timeline(TimelineState),
}

/// Effects that can be emitted by overlays.
#[derive(Debug)]
pub enum OverlayEffect {
    Ui(UiEffect),
}

impl From<UiEffect> for OverlayEffect {
    fn from(effect: UiEffect) -> Self {
        OverlayEffect::Ui(effect)
    }
}

impl Overlay {
    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        match self {
            Overlay::CommandPalette(p) => p.render(frame, area, input_y),
            Overlay::ModelPicker(p) => p.render(frame, area, input_y),
            Overlay::ThinkingPicker(p) => p.render(frame, area, input_y),
            Overlay::ThreadPicker(p) => p.render(frame, area, input_y),
            Overlay::FilePicker(p) => p.render(frame, area, input_y),
            Overlay::Login(l) => l.render(frame, area, input_y),
            Overlay::Timeline(t) => t.render(frame, area, input_y),
        }
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        match self {
            Overlay::CommandPalette(p) => p.handle_key(tui, key),
            Overlay::ModelPicker(p) => p.handle_key(tui, key),
            Overlay::ThinkingPicker(p) => p.handle_key(tui, key),
            Overlay::ThreadPicker(p) => p.handle_key(tui, key),
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

        let (palette, _) = CommandPaletteState::open(
            true,
            crate::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
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

        let scroll = crate::modes::tui::transcript::ScrollState::default();
        let (timeline, _, _) = TimelineState::open(&[], &scroll, ScrollMode::FollowLatest);
        let overlay: Option<Overlay> = Some(Overlay::Timeline(timeline));
        assert!(overlay.is_some());
    }
}
