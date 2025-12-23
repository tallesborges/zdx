//! Overlay modules for the TUI.
//!
//! Each overlay is self-contained: state, update handlers, and render function.

pub mod login;
pub mod model_picker;
pub mod palette;

/// Height of header area (lines: title + status + border).
/// Used by overlays for positioning calculations.
pub const HEADER_HEIGHT: u16 = 3;

pub use login::{
    LoginEvent, LoginState, handle_login_key, handle_login_result, render_login_overlay,
};
pub use model_picker::{
    ModelPickerState, handle_model_picker_key, open_model_picker, render_model_picker,
};
pub use palette::{
    CommandPaletteState, handle_palette_key, open_command_palette, render_command_palette,
};
