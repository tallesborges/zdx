//! UI effect types.
//!
//! Effects are commands returned by the reducer that the runtime executes.
//! They represent side effects like spawning async tasks, persisting state, etc.
//!
//! This keeps the reducer pure: it only mutates state and returns effects,
//! never performs I/O or spawns tasks directly.

use crate::config::ThinkingLevel;
use crate::core::session::SessionEvent;

/// Effects returned by the reducer for the runtime to execute.
///
/// The reducer returns `Vec<UiEffect>` from each update call.
/// The runtime executes these effects after rendering.
#[derive(Debug)]
pub enum UiEffect {
    /// Quit the application.
    Quit,

    /// Start an agent turn with the current input.
    StartAgentTurn,

    /// Interrupt the running agent task.
    InterruptAgent,

    /// Spawn async token exchange for login.
    SpawnTokenExchange { code: String, verifier: String },

    /// Open a URL in the system browser.
    OpenBrowser { url: String },

    /// Append an event to the session log.
    SaveSession { event: SessionEvent },

    /// Persist the model preference to config.
    PersistModel { model: String },

    /// Persist the thinking level preference to config.
    PersistThinking { level: ThinkingLevel },

    /// Create a new session (for /new command).
    CreateNewSession,

    /// Open config file in default system editor/app.
    OpenConfig,

    /// Start handoff generation with a goal.
    StartHandoff { goal: String },

    /// Submit handoff prompt: create new session and send prompt as first message.
    HandoffSubmit { prompt: String },

    /// Open the session picker overlay (loads session list via I/O).
    OpenSessionPicker,

    /// Load a session by ID (switch to that session).
    LoadSession { session_id: String },

    /// Preview a session (show transcript without full switch).
    /// Used during session picker navigation.
    PreviewSession { session_id: String },

    /// Discover project files for the file picker.
    DiscoverFiles,

    /// Open the model picker overlay.
    OpenModelPicker,

    /// Open the thinking level picker overlay.
    OpenThinkingPicker,

    /// Open the login overlay and start OAuth flow.
    OpenLogin,
}
