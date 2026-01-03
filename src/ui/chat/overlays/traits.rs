//! Overlay trait definition.
//!
//! This module defines the `Overlay` trait that formalizes the overlay contract.
//! The trait ensures compile-time enforcement of required overlay behaviors.
//!
//! ## Architecture
//!
//! Each overlay type consists of:
//! - **State struct** implementing the `Overlay` trait (e.g., `FilePickerState`)
//! - **Open function** - standalone, varying signatures (e.g., `open_file_picker()`)
//! - **Key handler** - implemented via `Overlay::handle_key()` method
//! - **Render** - implemented via the `Overlay::render()` method
//!
//! Closing is handled via `OverlayAction::Close` returned from `handle_key()`.
//!
//! Key handling and rendering are routed through `OverlayState` methods that
//! delegate to the appropriate overlay-specific implementation.
//!
//! ## Split State Architecture
//!
//! To avoid borrow checker conflicts, overlay state is separate from `TuiState`:
//! - `TuiState` contains all non-overlay UI state
//! - `OverlayState` is an enum of overlay variants
//! - `AppState` combines both: `{ tui: TuiState, overlay: OverlayState }`
//!
//! This allows handlers to get `&mut self` (overlay) and `&mut TuiState` simultaneously.
//!
//! ## Why a Trait?
//!
//! The trait provides:
//! 1. **Compile-time enforcement** - new overlay types must implement `render()` and `handle_key()`
//! 2. **Documentation** - the trait serves as executable documentation of the contract
//! 3. **Type safety** - ensures consistent signatures across overlays
//!
//! ## Why Not `Box<dyn Overlay>`?
//!
//! We keep the `OverlayState` enum rather than using `Box<dyn Overlay>` because:
//! - Overlays need variant-specific open/close logic with different parameters
//! - Pattern matching on the enum is ergonomic for accessor methods
//! - No runtime cost from dynamic dispatch

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;

/// Action returned by overlay key handlers.
///
/// Returned via `Option<OverlayAction>`:
/// - `None` = continue with overlay open, no effects
/// - `Some(Close(effects))` = close overlay, execute effects
/// - `Some(Transition { ... })` = transition to new overlay state
/// - `Some(Effects(effects))` = continue with effects (rare, e.g., session preview)
#[derive(Debug)]
pub enum OverlayAction {
    /// Close the overlay, returning effects to execute.
    Close(Vec<UiEffect>),

    /// Transition to a new overlay state (without closing).
    /// Used for state machine transitions (e.g., login flow).
    Transition {
        /// The new overlay state to transition to.
        new_state: super::OverlayStateInner,
        /// Effects to execute after transition.
        effects: Vec<UiEffect>,
    },

    /// Continue with the overlay open, but run these effects.
    /// Used rarely (e.g., session picker preview on navigation).
    Effects(Vec<UiEffect>),
}

impl OverlayAction {
    /// Creates a Close action with no effects.
    pub fn close() -> Self {
        OverlayAction::Close(vec![])
    }

    /// Creates a Close action with the given effects.
    pub fn close_with(effects: Vec<UiEffect>) -> Self {
        OverlayAction::Close(effects)
    }
}

/// Trait for overlay state structs.
///
/// Each overlay state type must implement this trait to provide rendering
/// and key handling. The split state architecture allows clean access to
/// both `&mut self` (the overlay) and `&mut TuiState` without borrow conflicts.
///
/// ## Contract
///
/// Implementors must:
/// - Be `Debug` and `Clone` for state management
/// - Provide a `render()` method with the standard signature
/// - Provide a `handle_key()` method that returns `Option<OverlayAction>`
///
/// Additionally, each overlay module should provide (not enforced by trait):
/// - `open_xxx()` function to initialize and show the overlay
///
/// ## Example
///
/// ```rust,ignore
/// impl Overlay for FilePickerState {
///     fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
///         render_file_picker(frame, self, area, input_y)
///     }
///
///     fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
///         match key.code {
///             KeyCode::Esc => Some(OverlayAction::close()),
///             KeyCode::Enter => {
///                 // Use tui to access/modify non-overlay state
///                 let text = tui.get_input_text();
///                 // ... handle selection ...
///                 Some(OverlayAction::close())
///             }
///             _ => None, // Continue with overlay open
///         }
///     }
/// }
/// ```
pub trait Overlay: std::fmt::Debug + Clone {
    /// Render this overlay to the frame.
    ///
    /// Overlays are rendered on top of the main UI, typically centered
    /// above the input area.
    ///
    /// # Arguments
    ///
    /// * `frame` - The ratatui frame to render to
    /// * `area` - The full terminal area (for calculating overlay position)
    /// * `input_y` - Y position of the input area top (for vertical positioning)
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16);

    /// Handle a key event for this overlay.
    ///
    /// Returns `Option<OverlayAction>`:
    /// - `None` = continue with overlay open, no side effects
    /// - `Some(Close(effects))` = close overlay and execute effects
    /// - `Some(Transition { ... })` = transition to new overlay state
    ///
    /// The split state architecture ensures no borrow conflicts:
    /// `self` is the overlay, `tui` is the rest of state.
    ///
    /// # Arguments
    ///
    /// * `tui` - Mutable reference to non-overlay TUI state
    /// * `key` - The key event to handle
    fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction>;
}
