//! Cross-slice state mutations.
//!
//! Feature reducers and overlays return these mutations to request changes
//! outside their own slice. The main reducer applies them in order.

use zdx_core::config::ThinkingLevel;
use zdx_core::core::thread_log::{ThreadLog, Usage};
use zdx_core::providers::ChatMessage;

use crate::input::HandoffState;
use crate::transcript::{HistoryCell, ScrollMode};

/// Mutations for cross-slice state changes.
#[derive(Debug)]
pub enum StateMutation {
    Transcript(TranscriptMutation),
    Input(InputMutation),
    Thread(ThreadMutation),
    Auth(AuthMutation),
    Config(ConfigMutation),
}

/// Transcript slice mutations requested by other slices.
#[derive(Debug)]
pub enum TranscriptMutation {
    AppendCell(HistoryCell),
    AppendSystemMessage(String),
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
}

/// Thread slice mutations requested by other slices.
#[derive(Debug)]
pub enum ThreadMutation {
    ClearMessages,
    SetMessages(Vec<ChatMessage>),
    AppendMessage(ChatMessage),
    SetThread(Option<ThreadLog>),
    ResetUsage,
    /// Restore usage from persisted thread (cumulative + latest for context %)
    SetUsage {
        cumulative: Usage,
        latest: Usage,
    },
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
    SetCallbackInProgress(bool),
    CancelLoginRequest,
}

/// Config mutations requested by overlays.
#[derive(Debug)]
pub enum ConfigMutation {
    SetModel(String),
    SetThinkingLevel(ThinkingLevel),
}
