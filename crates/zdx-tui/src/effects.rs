//! UI effect types.
//!
//! Effects are commands returned by the reducer that the runtime executes.
//! They represent I/O and task spawning only (no direct UI mutations).
//!
//! This keeps the reducer pure: it only mutates state and returns effects,
//! never performs I/O or spawns tasks directly.
//!
//! ## Cancellation Effects
//!
//! Cancellation is initiated from the reducer via `UiEffect::CancelTask`.
//! The runtime executes these by calling `token.cancel()` on the provided token.
//! This preserves the architecture: reducer decides when to cancel, runtime executes.

use tokio_util::sync::CancellationToken;
use zdx_core::config::ThinkingLevel;
use zdx_core::core::thread_log::ThreadEvent;
use zdx_core::providers::ProviderKind;

use crate::common::{TaskId, TaskKind};

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
        task: Option<TaskId>,
        provider: ProviderKind,
        code: String,
        verifier: String,
        redirect_uri: Option<String>,
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
        task: Option<TaskId>,
        thread_id: String,
        title: Option<String>,
    },

    /// Suggest a thread title from the first user message.
    SuggestThreadTitle { thread_id: String, message: String },

    /// Persist the model preference to config.
    PersistModel { model: String },

    /// Persist the thinking level preference to config.
    PersistThinking { level: ThinkingLevel },

    /// Create a new thread (for /new command).
    CreateNewThread { task: Option<TaskId> },

    /// Open config file in default system editor/app.
    OpenConfig,

    /// Open models config file in default system editor/app.
    OpenModelsConfig,

    /// Start handoff generation with a goal.
    StartHandoff { task: Option<TaskId>, goal: String },

    /// Submit handoff prompt: create new thread and send prompt as first message.
    HandoffSubmit {
        prompt: String,
        /// The source thread ID this handoff originated from.
        handoff_from: Option<String>,
    },

    /// Open the thread picker overlay (loads thread list via I/O).
    OpenThreadPicker {
        task: Option<TaskId>,
        mode: crate::overlays::ThreadPickerMode,
    },

    /// Load a thread by ID (switch to that thread).
    LoadThread {
        task: Option<TaskId>,
        thread_id: String,
    },

    /// Preview a thread (show transcript without full switch).
    /// Used during thread picker navigation.
    PreviewThread {
        task: Option<TaskId>,
        thread_id: String,
    },

    /// Discover project files for the file picker.
    DiscoverFiles { task: Option<TaskId> },

    /// Copy text to clipboard.
    CopyToClipboard {
        /// Text to copy.
        text: String,
    },

    /// Create a new thread from a truncated set of events.
    ForkThread {
        task: Option<TaskId>,
        events: Vec<ThreadEvent>,
        user_input: Option<String>,
        turn_number: usize,
    },

    /// Execute a bash command directly (user `!` shortcut).
    ExecuteBash {
        task: Option<TaskId>,
        command: String,
    },

    // ========================================================================
    // Cancellation Effects
    // ========================================================================
    // These effects trigger cancellation of in-progress async operations.
    // The reducer emits these when user presses Esc or otherwise cancels.
    // The runtime executes by calling `token.cancel()` on the stored token.
    /// Cancel an in-progress task.
    CancelTask {
        kind: TaskKind,
        token: Option<CancellationToken>,
    },
}
