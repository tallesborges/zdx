//! UI effect types.
//!
//! Effects are commands returned by the reducer that the runtime executes.
//! They represent I/O and task spawning only (no direct UI mutations).
//!
//! This keeps the reducer pure: it only mutates state and returns effects,
//! never performs I/O or spawns tasks directly.
//!
//! ## Task ID Allocation
//!
//! Task IDs are allocated by the runtime, not the reducer. Effects that spawn
//! tasks simply describe _what_ to do; the runtime assigns IDs when executing.
//! This keeps reducers fully deterministic and simplifies effect creation.
//!
//! ## Cancellation Effects
//!
//! Cancellation is initiated from the reducer via `UiEffect::CancelTask`.
//! The runtime executes these by calling `token.cancel()` on the provided token.
//! This preserves the architecture: reducer decides when to cancel, runtime executes.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;
use zdx_engine::config::ThinkingLevel;
use zdx_engine::core::thread_persistence::{Thread, ThreadEvent};
use zdx_engine::providers::{ChatMessage, ProviderKind};

use crate::common::TaskKind;
use crate::events::RecordedAudio;

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

    /// Interrupt the running BTW popup agent task.
    InterruptBtwAgent,

    /// Interrupt the running direct bash command.
    InterruptBash,

    /// Spawn async token exchange for login.
    SpawnTokenExchange {
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
        thread_id: String,
        title: Option<String>,
    },

    /// Suggest a thread title from the first user message.
    SuggestThreadTitle { thread_id: String, message: String },

    /// Persist the model preference to config.
    PersistModel { model: String },

    /// Persist the active thread's model override.
    PersistThreadModelOverride { model: String },

    /// Persist the thinking level preference to config.
    PersistThinking { level: ThinkingLevel },

    /// Persist the active thread's thinking override.
    PersistThreadThinkingOverride { level: ThinkingLevel },

    /// Create a new thread (for /new command).
    CreateNewThread,

    /// Open config file in default system editor/app.
    OpenConfig,

    /// Open models config file in default system editor/app.
    OpenModelsConfig,

    /// Start handoff generation with a goal.
    StartHandoff { goal: String },

    /// Start or continue the live BTW popup chat in its forked thread.
    StartBtwTurn {
        base_messages: Vec<ChatMessage>,
        thread_handle: Option<Thread>,
        messages: Vec<ChatMessage>,
        prompt: String,
        model: String,
        thinking_level: ThinkingLevel,
    },

    /// Submit handoff prompt: create new thread and send prompt as first message.
    HandoffSubmit {
        prompt: String,
        /// The source thread ID this handoff originated from.
        handoff_from: Option<String>,
    },

    /// Open the thread picker overlay (loads thread list via I/O).
    OpenThreadPicker {
        mode: crate::overlays::ThreadPickerMode,
    },

    /// Load a thread by ID (switch to that thread).
    LoadThread { thread_id: String },

    /// Ensure a git worktree for the active thread and switch root to it.
    EnsureWorktree,

    /// Open a new terminal window at the given path.
    OpenTerminal,

    /// Remove the current worktree and switch root to project root.
    RemoveWorktree,

    /// Create a new thread using the original project root.
    CreateNewThreadFromProjectRoot,

    /// Resolve root-derived display state (branch/path) and apply it.
    ResolveRootDisplay { path: PathBuf },

    /// Rebuild effective system prompt for a new root.
    RefreshSystemPrompt { path: PathBuf },

    /// Preview a thread (show transcript without full switch).
    /// Used during thread picker navigation.
    PreviewThread { thread_id: String },

    /// Discover project files for the file picker.
    DiscoverFiles,

    /// Fetch available skills from a GitHub repository.
    FetchSkillsList { repo: String },

    /// Install a skill from a GitHub repository.
    InstallSkill { repo: String, skill_path: String },

    /// Fetch the SKILL.md content for a skill from a GitHub repository.
    FetchSkillInstructions { repo: String, skill_path: String },

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

    /// Execute a bash command directly (user `$` shortcut).
    ExecuteBash { command: String },

    // ========================================================================
    // Cancellation Effects
    // ========================================================================
    // These effects trigger cancellation of in-progress async operations.
    // The reducer emits these when user presses Esc or otherwise cancels.
    // The runtime executes by calling `token.cancel()` on the stored token.
    /// Attach an image from a file path (drag-and-drop).
    AttachImage { path: String },

    /// Cancel an in-progress task.
    CancelTask {
        kind: TaskKind,
        token: Option<CancellationToken>,
    },

    /// Decode an image for preview on a background thread.
    /// Reads the file, converts to PNG if needed, and base64-encodes for Kitty protocol.
    DecodeImagePreview { image_path: String },

    /// Start microphone capture for voice dictation.
    StartVoiceRecording,

    /// Stop microphone capture and finalize the recording.
    StopVoiceRecording,

    /// Transcribe a completed voice recording into text.
    StartVoiceTranscription { audio: RecordedAudio },
}
