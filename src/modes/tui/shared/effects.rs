//! UI effect types.
//!
//! Effects are commands returned by the reducer that the runtime executes.
//! They represent I/O and task spawning only (no direct UI mutations).
//!
//! This keeps the reducer pure: it only mutates state and returns effects,
//! never performs I/O or spawns tasks directly.

use crate::config::ThinkingLevel;
use crate::core::thread_log::ThreadEvent;
use crate::modes::tui::shared::RequestId;
use crate::providers::ProviderKind;

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

    /// Interrupt the running direct bash command.
    InterruptBash,

    /// Spawn async token exchange for login.
    SpawnTokenExchange {
        provider: ProviderKind,
        code: String,
        verifier: String,
        redirect_uri: Option<String>,
        req: RequestId,
    },

    /// Start a local OAuth callback listener (if supported).
    StartLocalAuthCallback {
        provider: ProviderKind,
        state: Option<String>,
        port: Option<u16>,
    },

    /// Open a URL in the system browser.
    OpenBrowser { url: String },

    /// Append an event to the thread log.
    SaveThread { event: ThreadEvent },

    /// Rename the current thread.
    RenameThread {
        thread_id: String,
        title: Option<String>,
    },

    /// Persist the model preference to config.
    PersistModel { model: String },

    /// Persist the thinking level preference to config.
    PersistThinking { level: ThinkingLevel },

    /// Create a new thread (for /new command).
    CreateNewThread,

    /// Open config file in default system editor/app.
    OpenConfig,

    /// Start handoff generation with a goal.
    StartHandoff { goal: String },

    /// Submit handoff prompt: create new thread and send prompt as first message.
    HandoffSubmit { prompt: String },

    /// Open the thread picker overlay (loads thread list via I/O).
    OpenThreadPicker,

    /// Load a thread by ID (switch to that thread).
    LoadThread { thread_id: String },

    /// Preview a thread (show transcript without full switch).
    /// Used during thread picker navigation.
    PreviewThread { thread_id: String, req: RequestId },

    /// Discover project files for the file picker.
    DiscoverFiles,

    /// Copy text to clipboard.
    CopyToClipboard {
        /// Text to copy.
        text: String,
    },

    /// Create a new thread from a truncated set of events.
    ForkThread {
        events: Vec<ThreadEvent>,
        user_input: Option<String>,
        turn_number: usize,
    },

    /// Execute a bash command directly (user `!` shortcut).
    ExecuteBash { command: String },
}
