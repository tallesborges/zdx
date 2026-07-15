//! Cross-slice state mutations.
//!
//! Feature reducers and overlays return these mutations to request changes
//! outside their own slice. The main reducer applies them in order.

use std::path::PathBuf;

use zdx_engine::config::ThinkingLevel;
use zdx_engine::core::thread_persistence::{Thread, Usage};
use zdx_engine::providers::{ChatMessage, ProviderKind};

use crate::input::{HandoffState, PromptBuilderState};
use crate::transcript::{HistoryCell, ScrollMode};

/// Mutations for cross-slice state changes.
#[derive(Debug)]
pub enum StateMutation {
    Transcript(TranscriptMutation),
    Input(InputMutation),
    Thread(ThreadMutation),
    Auth(AuthMutation),
    Config(ConfigMutation),
    SetRootDisplay {
        path: PathBuf,
        git_branch: Option<String>,
        display_path: String,
    },
    SetActiveThreadOverrides {
        model_override: Option<String>,
        thinking_override: Option<ThinkingLevel>,
    },
    SetSystemPrompt(Option<String>),
    SetLastSkillRepo(String),
    SetLoadedSkills(Vec<zdx_engine::skills::Skill>),
    /// Replace the active tab's suggested replies (empty clears).
    SetLastFollowups(Vec<String>),
    /// Toggle the debug status line visibility.
    ToggleDebugStatus,
}

/// Transcript slice mutations requested by other slices.
#[derive(Debug)]
pub enum TranscriptMutation {
    AppendCell(Box<HistoryCell>),
    AppendSystemMessage(String),
    /// Appends a system notice, but replaces the previous one in place when it
    /// is still the last cell (no message since). Used to coalesce repeated
    /// model/preset switches instead of stacking one banner per switch.
    AppendOrReplaceSwitchNotice(String),
    Clear,
    ReplaceCells(Vec<HistoryCell>),
    ResetScroll,
    ClearWrapCache,
    SetScrollOffset { offset: usize },
    SetScrollMode(ScrollMode),
    ScrollToTop,
    ScrollToBottom,
    PageUp,
    PageDown,
}

/// Input slice mutations requested by other slices.
#[derive(Debug)]
pub enum InputMutation {
    Clear,
    SetText(String),
    InsertText(String),
    InsertChar(char),
    SetTextAndCursor {
        text: String,
        cursor_row: usize,
        cursor_col: usize,
    },
    SetHistory(Vec<String>),
    ClearHistory,
    ClearQueue,
    SetHandoffState(HandoffState),
    SetPromptBuilderState(PromptBuilderState),
    /// Attach an image (`mime_type`, `base64_data`, `source_path`).
    AttachImage {
        mime_type: String,
        data: String,
        source_path: Option<String>,
    },
    /// Reset image counter (on new thread).
    ResetImageCounter,
}

/// Thread slice mutations requested by other slices.
#[derive(Debug)]
pub enum ThreadMutation {
    ClearMessages,
    SetMessages(Vec<ChatMessage>),
    AppendMessage(ChatMessage),
    SetThread(Option<Thread>),
    SetOverrides {
        model_override: Option<String>,
        thinking_override: Option<ThinkingLevel>,
    },
    ResetUsage,
    /// Restore usage from persisted thread (cumulative + latest for context %)
    SetUsage {
        cumulative: Usage,
        latest: Usage,
    },
    /// Set the thread title (if any).
    SetTitle(Option<String>),
    UpdateUsage {
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
    },
}

/// Auth slice mutations requested by other slices.
#[derive(Debug)]
pub enum AuthMutation {
    RefreshStatus,
}

/// Config mutations requested by overlays.
#[derive(Debug)]
pub enum ConfigMutation {
    SetModel(String),
    SetThinkingLevel(ThinkingLevel),
    SetFastMode {
        provider: ProviderKind,
        enabled: bool,
    },
}
