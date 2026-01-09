//! Thread feature reducer.
//!
//! Handles thread-related state transitions: loading, switching, creating, renaming.

use std::path::PathBuf;

use crate::core::thread_log::{ThreadLog, ThreadSummary, Usage, short_thread_id};
use crate::modes::tui::events::ThreadUiEvent;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{
    InputMutation, StateMutation, ThreadMutation, TranscriptMutation,
};
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::ChatMessage;

/// Handles thread UI events.
///
/// Dispatches to specific handlers based on event type.
/// Returns effects for the runtime to execute.
#[derive(Debug)]
pub enum ThreadOverlayAction {
    OpenThreadPicker {
        threads: Vec<ThreadSummary>,
        original_cells: Vec<HistoryCell>,
    },
    None,
}

pub fn handle_thread_event(
    event: ThreadUiEvent,
) -> (Vec<UiEffect>, Vec<StateMutation>, ThreadOverlayAction) {
    let mut mutations = Vec::new();
    let mut overlay_action = ThreadOverlayAction::None;
    let effects = match event {
        ThreadUiEvent::ListStarted
        | ThreadUiEvent::LoadStarted
        | ThreadUiEvent::PreviewStarted
        | ThreadUiEvent::CreateStarted
        | ThreadUiEvent::ForkStarted
        | ThreadUiEvent::RenameStarted => vec![],
        ThreadUiEvent::ListLoaded {
            threads,
            original_cells,
        } => {
            overlay_action = ThreadOverlayAction::OpenThreadPicker {
                threads,
                original_cells,
            };
            vec![]
        }
        ThreadUiEvent::ListFailed { error } => {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(error),
            ));
            vec![]
        }
        ThreadUiEvent::Loaded {
            thread_id,
            cells,
            messages,
            history,
            thread_log,
            usage,
        } => {
            handle_thread_loaded(
                thread_log,
                &thread_id,
                cells,
                messages,
                history,
                usage,
                &mut mutations,
            );
            vec![]
        }
        ThreadUiEvent::LoadFailed { error } => {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(error),
            ));
            vec![]
        }
        ThreadUiEvent::PreviewLoaded { req: _, cells } => {
            handle_thread_preview_loaded(cells, &mut mutations);
            vec![]
        }
        ThreadUiEvent::PreviewFailed { .. } => {
            // Silent failure for preview - errors shown on actual load
            vec![]
        }
        ThreadUiEvent::Created {
            thread_log,
            context_paths,
        } => {
            handle_thread_created(thread_log, context_paths, &mut mutations);
            vec![]
        }
        ThreadUiEvent::ForkedLoaded {
            thread_id: _,
            cells,
            messages,
            history,
            thread_log,
            usage,
            user_input,
            turn_number,
        } => {
            handle_thread_forked(
                thread_log,
                cells,
                messages,
                history,
                usage,
                user_input,
                turn_number,
                &mut mutations,
            );
            vec![]
        }
        ThreadUiEvent::CreateFailed { error } => {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(error),
            ));
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage("Thread cleared.".to_string()),
            ));
            vec![]
        }
        ThreadUiEvent::ForkFailed { error } => {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(error),
            ));
            vec![]
        }
        ThreadUiEvent::Renamed { thread_id, title } => {
            handle_thread_renamed(&thread_id, title, &mut mutations);
            vec![]
        }
        ThreadUiEvent::TitleSuggested {
            thread_id: _,
            title: _,
        } => {
            vec![]
        }
        ThreadUiEvent::RenameFailed { error } => {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(error),
            ));
            vec![]
        }
    };

    (effects, mutations, overlay_action)
}

/// Handles thread list loaded - opens thread picker overlay.
/// Handles thread loaded - switches to the thread.
fn handle_thread_loaded(
    thread_log: Option<ThreadLog>,
    thread_id: &str,
    cells: Vec<HistoryCell>,
    messages: Vec<ChatMessage>,
    history: Vec<String>,
    usage: (Usage, Usage),
    mutations: &mut Vec<StateMutation>,
) {
    let (cumulative, latest) = usage;
    mutations.push(StateMutation::Transcript(TranscriptMutation::ReplaceCells(
        cells,
    )));
    mutations.push(StateMutation::Transcript(TranscriptMutation::ResetScroll));
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::ClearWrapCache,
    ));
    mutations.push(StateMutation::Thread(ThreadMutation::SetMessages(messages)));
    mutations.push(StateMutation::Thread(ThreadMutation::SetThread(thread_log)));
    mutations.push(StateMutation::Thread(ThreadMutation::SetUsage {
        cumulative,
        latest,
    }));
    mutations.push(StateMutation::Input(InputMutation::SetHistory(history)));

    // Show confirmation message
    let short_id = if thread_id.len() > 8 {
        format!("{}…", &thread_id[..8])
    } else {
        thread_id.to_string()
    };
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::AppendSystemMessage(format!("Switched to thread {}", short_id)),
    ));
}

/// Handles thread preview loaded - shows transcript without full switch.
fn handle_thread_preview_loaded(cells: Vec<HistoryCell>, mutations: &mut Vec<StateMutation>) {
    mutations.push(StateMutation::Transcript(TranscriptMutation::ReplaceCells(
        cells,
    )));
    mutations.push(StateMutation::Transcript(TranscriptMutation::ResetScroll));
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::ClearWrapCache,
    ));
}

/// Handles thread created - sets up the new thread.
fn handle_thread_created(
    thread_log: ThreadLog,
    context_paths: Vec<PathBuf>,
    mutations: &mut Vec<StateMutation>,
) {
    let thread_path = thread_log.path().display().to_string();
    mutations.push(StateMutation::Thread(ThreadMutation::SetThread(Some(
        thread_log,
    ))));

    // Show thread path
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::AppendSystemMessage(format!("Thread path: {}", thread_path)),
    ));

    // Show loaded AGENTS.md files
    if !context_paths.is_empty() {
        let paths_list: Vec<String> = context_paths
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect();
        let message = format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n"));
        mutations.push(StateMutation::Transcript(
            TranscriptMutation::AppendSystemMessage(message),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_thread_forked(
    thread_log: ThreadLog,
    cells: Vec<HistoryCell>,
    messages: Vec<ChatMessage>,
    history: Vec<String>,
    usage: (Usage, Usage),
    user_input: Option<String>,
    turn_number: usize,
    mutations: &mut Vec<StateMutation>,
) {
    let (cumulative, latest) = usage;
    mutations.push(StateMutation::Transcript(TranscriptMutation::ReplaceCells(
        cells,
    )));
    mutations.push(StateMutation::Transcript(TranscriptMutation::ResetScroll));
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::ClearWrapCache,
    ));
    mutations.push(StateMutation::Thread(ThreadMutation::SetMessages(messages)));
    mutations.push(StateMutation::Thread(ThreadMutation::SetThread(Some(
        thread_log,
    ))));
    mutations.push(StateMutation::Thread(ThreadMutation::SetUsage {
        cumulative,
        latest,
    }));
    mutations.push(StateMutation::Input(InputMutation::SetHistory(history)));
    mutations.push(StateMutation::Input(InputMutation::Clear));
    if let Some(text) = user_input {
        mutations.push(StateMutation::Input(InputMutation::SetText(text)));
    }
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::AppendSystemMessage(format!("Forked from turn {}.", turn_number)),
    ));
}

/// Handles thread rename success.
fn handle_thread_renamed(
    thread_id: &str,
    title: Option<String>,
    mutations: &mut Vec<StateMutation>,
) {
    let short_id = short_thread_id(thread_id);
    let display_title = title.unwrap_or_else(|| short_id.clone());
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::AppendSystemMessage(format!(
            "Thread {} renamed to \"{}\".",
            short_id, display_title
        )),
    ));
}
