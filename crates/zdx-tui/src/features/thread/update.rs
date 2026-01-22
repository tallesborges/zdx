//! Thread feature reducer.
//!
//! Handles thread-related state transitions: loading, switching, creating, renaming.

use std::path::PathBuf;

use zdx_core::core::thread_log::{ThreadLog, ThreadSummary, Usage, short_thread_id};
use zdx_core::providers::ChatMessage;

use crate::effects::UiEffect;
use crate::events::ThreadUiEvent;
use crate::mutations::{InputMutation, StateMutation, ThreadMutation, TranscriptMutation};
use crate::transcript::HistoryCell;

/// Handles thread UI events.
///
/// Dispatches to specific handlers based on event type.
/// Returns effects for the runtime to execute.
#[derive(Debug)]
pub enum ThreadOverlayAction {
    OpenThreadPicker {
        threads: Vec<ThreadSummary>,
        original_cells: Vec<HistoryCell>,
        mode: crate::overlays::ThreadPickerMode,
    },
    None,
}

pub fn handle_thread_event(
    event: ThreadUiEvent,
) -> (Vec<UiEffect>, Vec<StateMutation>, ThreadOverlayAction) {
    let mut mutations = Vec::new();
    let mut overlay_action = ThreadOverlayAction::None;
    let effects = match event {
        ThreadUiEvent::ListLoaded {
            threads,
            original_cells,
            mode,
        } => {
            overlay_action = ThreadOverlayAction::OpenThreadPicker {
                threads,
                original_cells,
                mode,
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
            title,
            usage,
        } => {
            handle_thread_loaded(
                thread_log,
                &thread_id,
                cells,
                messages,
                history,
                title,
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
        ThreadUiEvent::PreviewLoaded { cells } => {
            handle_thread_preview_loaded(cells, &mut mutations);
            vec![]
        }
        ThreadUiEvent::PreviewFailed => {
            // Silent failure for preview - errors shown on actual load
            vec![]
        }
        ThreadUiEvent::Created {
            thread_log,
            context_paths,
            skills,
        } => {
            handle_thread_created(thread_log, context_paths, skills, &mut mutations);
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
            title,
        } => {
            if let Some(title) = title {
                mutations.push(StateMutation::Thread(ThreadMutation::SetTitle(Some(title))));
            }
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
#[allow(clippy::too_many_arguments)]
fn handle_thread_loaded(
    thread_log: Option<ThreadLog>,
    thread_id: &str,
    cells: Vec<HistoryCell>,
    messages: Vec<ChatMessage>,
    history: Vec<String>,
    title: Option<String>,
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
    mutations.push(StateMutation::Thread(ThreadMutation::SetTitle(title)));
    mutations.push(StateMutation::Thread(ThreadMutation::SetThread(thread_log)));
    mutations.push(StateMutation::Thread(ThreadMutation::SetUsage {
        cumulative,
        latest,
    }));
    mutations.push(StateMutation::Input(InputMutation::SetHistory(history)));
    mutations.push(StateMutation::Input(InputMutation::ClearQueue));

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
    skills: Vec<zdx_core::skills::Skill>,
    mutations: &mut Vec<StateMutation>,
) {
    let startup_messages =
        crate::thread_startup_messages(Some(thread_log.path()), &context_paths, &skills);
    mutations.push(StateMutation::Thread(ThreadMutation::SetThread(Some(
        thread_log,
    ))));
    mutations.push(StateMutation::Thread(ThreadMutation::SetTitle(None)));
    mutations.push(StateMutation::Input(InputMutation::ClearQueue));

    // Show startup messages
    for message in startup_messages {
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
    mutations.push(StateMutation::Thread(ThreadMutation::SetTitle(None)));
    mutations.push(StateMutation::Thread(ThreadMutation::SetUsage {
        cumulative,
        latest,
    }));
    mutations.push(StateMutation::Input(InputMutation::SetHistory(history)));
    mutations.push(StateMutation::Input(InputMutation::ClearQueue));
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
    let display_title = title.clone().unwrap_or_else(|| short_id.clone());
    mutations.push(StateMutation::Transcript(
        TranscriptMutation::AppendSystemMessage(format!(
            "Thread {} renamed to \"{}\".",
            short_id, display_title
        )),
    ));
    mutations.push(StateMutation::Thread(ThreadMutation::SetTitle(title)));
}
