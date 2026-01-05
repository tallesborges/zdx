//! Cross-slice state commands.
//!
//! Feature reducers and overlays return these commands to request mutations
//! outside their own slice. The main reducer applies them in order.

use crate::config::ThinkingLevel;
use crate::core::session::Session;
use crate::modes::tui::input::HandoffState;
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::anthropic::ChatMessage;

/// Commands for cross-slice state mutations.
#[derive(Debug)]
pub enum StateCommand {
    Transcript(TranscriptCommand),
    Input(InputCommand),
    Session(SessionCommand),
    Auth(AuthCommand),
    Config(ConfigCommand),
}

/// Transcript slice mutations requested by other slices.
#[derive(Debug)]
pub enum TranscriptCommand {
    AppendCell(HistoryCell),
    AppendSystemMessage(String),
    Clear,
    ReplaceCells(Vec<HistoryCell>),
    ResetScroll,
    ClearWrapCache,
    ScrollToTop,
    ScrollToBottom,
    PageUp,
    PageDown,
}

/// Input slice mutations requested by other slices.
#[derive(Debug)]
pub enum InputCommand {
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
    SetHandoffState(HandoffState),
}

/// Session slice mutations requested by other slices.
#[derive(Debug)]
pub enum SessionCommand {
    ClearMessages,
    SetMessages(Vec<ChatMessage>),
    AppendMessage(ChatMessage),
    SetSession(Option<Session>),
    ResetUsage,
    UpdateUsage {
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
    },
}

/// Auth slice mutations requested by other slices.
#[derive(Debug)]
pub enum AuthCommand {
    RefreshStatus,
    ClearLoginRx,
}

/// Config mutations requested by overlays.
#[derive(Debug)]
pub enum ConfigCommand {
    SetModel(String),
    SetThinkingLevel(ThinkingLevel),
}
