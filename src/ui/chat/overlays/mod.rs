//! Overlay modules for the TUI.
//!
//! Each overlay is self-contained: state, update handlers, and render function.

pub mod login;
pub mod model_picker;
pub mod palette;
pub mod thinking_picker;

pub use login::{
    LoginEvent, LoginState, handle_login_key, handle_login_result, render_login_overlay,
};
pub use model_picker::{
    ModelPickerState, handle_model_picker_key, open_model_picker, render_model_picker,
};
pub use palette::{
    CommandPaletteState, handle_palette_key, open_command_palette, render_command_palette,
};
pub use thinking_picker::{
    ThinkingPickerState, handle_thinking_picker_key, open_thinking_picker, render_thinking_picker,
};
