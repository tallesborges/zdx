//! Overlay modules for the TUI.
//!
//! Overlays are modal UI components that temporarily take over keyboard input.
//! Each overlay is self-contained: it owns its state, key handler, and render function.
//!
//! ## Module Structure
//!
//! - `command_palette.rs`: Command palette (Ctrl+O or `/` when input empty)
//! - `model_picker.rs`: Model selection picker
//! - `skill_picker.rs`: Skill installer picker
//! - `thinking_picker.rs`: Thinking level selection picker
//! - `thread_picker.rs`: Thread history picker
//! - `login.rs`: OAuth login flow overlay
//! - `file_picker.rs`: File picker triggered by `@`
//! - `rename.rs`: Thread rename overlay
//! - `render_utils.rs`: Shared rendering utilities for overlays
//! - `update.rs`: Overlay key handling and update logic
//!
//! ## Extension Trait
//!
//! `OverlayExt` provides convenience methods for `Option<Overlay>` to encapsulate
//! the common patterns used in the reducer.

pub mod command_palette;
pub mod file_picker;
pub mod image_preview;
pub mod login;
pub mod model_picker;
pub mod rename;
pub mod render_utils;
pub mod skill_picker;
pub mod thinking_picker;
pub mod thread_picker;
pub mod timeline;
mod update;

pub use command_palette::CommandPaletteState;
use crossterm::event::KeyEvent;
pub use file_picker::{FilePickerState, discover_files};
pub use image_preview::ImagePreviewState;
pub use login::LoginState;
pub use model_picker::ModelPickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
pub use rename::RenameState;
pub use skill_picker::SkillPickerState;
pub use thinking_picker::ThinkingPickerState;
pub use thread_picker::{ThreadPickerMode, ThreadPickerState, ThreadScope};
pub use timeline::TimelineState;
// Re-export update functions
pub use update::{handle_files_discovered, handle_overlay_key};

use crate::common::{TaskKind, Tasks};
use crate::effects::UiEffect;
use crate::mutations::StateMutation;
use crate::state::TuiState;

// ============================================================================
// OverlayRequest / OverlayTransition / OverlayUpdate
// ============================================================================

/// Requests to open a new overlay.
#[derive(Debug)]
pub enum OverlayRequest {
    CommandPalette,
    ModelPicker,
    SkillPicker,
    ThinkingPicker,
    Login,
    FilePicker {
        trigger_pos: usize,
    },
    Timeline,
    Rename,
    ImagePreview {
        image_path: String,
        image_index: usize,
    },
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
    pub effects: Vec<UiEffect>,
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

    #[must_use]
    pub fn with_mutations(mut self, mutations: Vec<StateMutation>) -> Self {
        self.mutations = mutations;
        self
    }

    #[must_use]
    pub fn with_ui_effects(mut self, effects: Vec<UiEffect>) -> Self {
        self.effects = effects;
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
    SkillPicker(SkillPickerState),
    ThinkingPicker(ThinkingPickerState),
    ThreadPicker(ThreadPickerState),
    Login(LoginState),
    FilePicker(FilePickerState),
    Timeline(TimelineState),
    Rename(RenameState),
    ImagePreview(ImagePreviewState),
}

impl Overlay {
    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16, tasks: &Tasks) {
        match self {
            Overlay::CommandPalette(p) => p.render(frame, area, input_y),
            Overlay::ModelPicker(p) => p.render(frame, area, input_y),
            Overlay::SkillPicker(p) => p.render(frame, area, input_y),
            Overlay::ThinkingPicker(p) => p.render(frame, area, input_y),
            Overlay::ThreadPicker(p) => p.render(frame, area, input_y),
            Overlay::FilePicker(p) => p.render(frame, area, input_y),
            Overlay::Login(l) => l.render(frame, area, input_y),
            Overlay::Timeline(t) => t.render(frame, area, input_y),
            Overlay::Rename(r) => r.render(frame, area, input_y),
            Overlay::ImagePreview(p) => p.render(
                frame,
                area,
                input_y,
                tasks.state(TaskKind::ImageDecode).is_running(),
            ),
        }
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        match self {
            Overlay::CommandPalette(p) => p.handle_key(tui, key),
            Overlay::ModelPicker(p) => p.handle_key(tui, key),
            Overlay::SkillPicker(p) => p.handle_key(tui, key),
            Overlay::ThinkingPicker(p) => p.handle_key(tui, key),
            Overlay::ThreadPicker(p) => p.handle_key(tui, key),
            Overlay::FilePicker(p) => p.handle_key(&tui.input, key),
            Overlay::Login(l) => l.handle_key(tui, key),
            Overlay::Timeline(t) => t.handle_key(tui, key),
            Overlay::Rename(r) => r.handle_key(tui, key),
            Overlay::ImagePreview(p) => p.handle_key(tui, key),
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
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16, tasks: &Tasks);
}

impl OverlayExt for Option<Overlay> {
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16, tasks: &Tasks) {
        if let Some(overlay) = self {
            overlay.render(frame, area, input_y, tasks);
        }
    }
}

#[cfg(test)]
mod tests {
    use zdx_core::config::ThinkingLevel;

    use super::*;
    use crate::transcript::ScrollState;

    #[test]
    fn test_overlay_is_some() {
        use crate::transcript::ScrollMode;

        let none: Option<Overlay> = None;
        assert!(none.is_none());

        let (palette, _) = CommandPaletteState::open(
            zdx_core::providers::ProviderKind::Anthropic,
            "claude-haiku-4-5".to_string(),
        );
        let overlay: Option<Overlay> = Some(Overlay::CommandPalette(palette));
        assert!(overlay.is_some());

        let providers = zdx_core::config::ProvidersConfig::default();
        let (picker, _) = ModelPickerState::open("test", &providers);
        let overlay: Option<Overlay> = Some(Overlay::ModelPicker(picker));
        assert!(overlay.is_some());

        let (skill_picker, _) = SkillPickerState::open(vec!["test/repo".to_string()], None);
        let overlay: Option<Overlay> = Some(Overlay::SkillPicker(skill_picker));
        assert!(overlay.is_some());

        let (thinking, _) = ThinkingPickerState::open(ThinkingLevel::Off);
        let overlay: Option<Overlay> = Some(Overlay::ThinkingPicker(thinking));
        assert!(overlay.is_some());

        let overlay: Option<Overlay> = Some(Overlay::Login(LoginState::Exchanging {
            provider: zdx_core::providers::ProviderKind::Anthropic,
        }));
        assert!(overlay.is_some());

        let (file_picker, _) = FilePickerState::open(0);
        let overlay: Option<Overlay> = Some(Overlay::FilePicker(file_picker));
        assert!(overlay.is_some());

        let scroll = ScrollState::default();
        let (timeline, _, _) = TimelineState::open(&[], &scroll, ScrollMode::FollowLatest);
        let overlay: Option<Overlay> = Some(Overlay::Timeline(timeline));
        assert!(overlay.is_some());

        let (rename, _) = RenameState::open("test-id".to_string(), Some("Test Title".to_string()));
        let overlay: Option<Overlay> = Some(Overlay::Rename(rename));
        assert!(overlay.is_some());
    }
}
