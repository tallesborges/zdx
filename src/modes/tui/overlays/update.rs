//! Overlay update/reducer logic.
//!
//! This module provides centralized overlay key handling that the main
//! reducer delegates to when an overlay is active.

use std::path::PathBuf;

use crossterm::event::KeyEvent;

use super::{Overlay, OverlayExt};
use crate::modes::tui::app::TuiState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::StateCommand;

/// Handles a key event for the active overlay.
///
/// This function encapsulates the overlay key handling logic:
/// - Dispatches to the active overlay's key handler
/// - Closes the overlay if `OverlayAction::Close` is returned
/// - Collects and returns any effects
///
/// Returns `Some(effects)` if an overlay was active and handled the key,
/// or `None` if no overlay was active.
///
/// # Example
///
/// ```ignore
/// let (effects, commands) = overlays::handle_overlay_key(&app.tui, &mut app.overlay, key);
/// apply_state_commands(&mut app.tui, commands);
/// if let Some(effects) = effects {
///     return effects;
/// }
/// // No overlay active - handle key normally
/// ```
pub fn handle_overlay_key(
    tui: &TuiState,
    overlay: &mut Option<Overlay>,
    key: KeyEvent,
) -> (Option<Vec<UiEffect>>, Vec<StateCommand>) {
    overlay.handle_key(tui, key)
}

/// Handles discovered files for the file picker overlay.
///
/// Updates the file picker with discovered files if one is active.
pub fn handle_files_discovered(overlay: &mut Option<Overlay>, files: Vec<PathBuf>) {
    if let Some(picker) = overlay.as_mut().and_then(|o| o.as_file_picker_mut()) {
        picker.set_files(files);
    }
}
