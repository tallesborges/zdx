use anyhow::Context;
use tokio_util::sync::CancellationToken;
use zdx_engine::core::handoff_generation;
use zdx_engine::core::steer::{SteerInbox, SteerSource};
use zdx_engine::core::thread_persistence::{self, ThreadEvent};
use zdx_engine::providers::ChatMessage;

use crate::events::UiEvent;
use crate::state::{TabKind, TuiState};

/// Interrupts the running agent.
pub fn interrupt_agent(tui: &TuiState) {
    if let Some(cancel) = tui.agent_state.cancel_token() {
        cancel.cancel();
    }
}

/// Why the tab's stored selection cannot run with its config: it names a mode
/// the config does not define. The turn is refused then; running the default
/// in its place could use another provider or account.
fn selection_error(tui: &TuiState) -> Option<String> {
    let selection = tui.thread.model_override.as_deref()?;
    tui.config
        .check_model_selection(selection)
        .err()
        .map(|err| format!("{err}. Pick another model with /model or Tab."))
}

/// A turn that failed before it started; it carries no messages, so the
/// thread keeps the user's input for a retry.
fn refused_turn(message: String) -> std::sync::Arc<zdx_engine::core::events::AgentEvent> {
    use zdx_engine::core::events::{AgentEvent, ErrorKind, TurnStatus};
    std::sync::Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Failed {
            kind: ErrorKind::Internal,
            message,
            details: None,
        },
        final_text: String::new(),
        messages: Vec::new(),
        prior_message_count: 0,
    })
}

/// Spawns an agent turn for the active tab.
///
/// For btw tabs, this seeds a parent-thread pointer and creates a persistent
/// thread on the first send.
pub fn spawn_agent_turn(tui: &TuiState) -> UiEvent {
    // For btw tabs, handle thread creation and pointer seeding
    if let TabKind::Btw {
        ref parent_thread_id,
    } = tui.tab_kind
    {
        return spawn_btw_tab_turn(tui, parent_thread_id.as_deref());
    }

    let (agent_tx, agent_rx) = zdx_engine::core::agent::create_event_channel();
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();

    let messages = tui.thread.messages.clone();
    let config = tui.config.clone();
    let refused = selection_error(tui);
    let mut agent_opts = tui.agent_opts.clone();
    agent_opts.conversation_id = Some(tui.thread.conversation_id.clone());
    let steer = SteerInbox::new();
    agent_opts.steer = Some(SteerSource::Inbox(steer.clone()));
    let system_prompt = tui.system_prompt.clone();
    let thread_id = tui.thread.thread_handle.as_ref().map(|h| h.id.clone());

    let (tui_tx, tui_rx) = zdx_engine::core::agent::create_event_channel();

    if let Some(thread_handle) = tui.thread.thread_handle.clone() {
        let (persist_tx, persist_rx) = zdx_engine::core::agent::create_event_channel();
        let _broadcaster =
            zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx, persist_tx]);
        let _persist = thread_persistence::spawn_thread_persist_task(thread_handle, persist_rx);
    } else {
        let _broadcaster = zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx]);
    }

    // Spawn the agent task - it will send TurnFinished when done
    tokio::spawn(async move {
        if let Some(message) = refused {
            let _ = agent_tx.send(refused_turn(message));
            return;
        }
        let _ = zdx_engine::core::agent::run_turn_with_cancel(
            messages,
            &config,
            &agent_opts,
            system_prompt.as_deref(),
            thread_id.as_deref(),
            agent_tx.clone(),
            Some(run_cancel),
        )
        .await;
    });

    UiEvent::AgentSpawned {
        rx: tui_rx,
        cancel,
        thread_handle: None,
        messages: None,
        steer: Some(steer),
    }
}

/// Spawns an agent turn for a btw tab.
///
/// On the first send, creates a persistent thread and seeds it with the user's
/// message plus a pointer at `parent_thread_id`, so the agent can pull context
/// on demand via `Read_Thread` instead of receiving a copied transcript. On
/// subsequent sends, reuses the existing thread.
fn spawn_btw_tab_turn(tui: &TuiState, parent_thread_id: Option<&str>) -> UiEvent {
    // Prepare thread and messages (create thread on first send)
    let prepared = match prepare_btw_tab_thread(tui, parent_thread_id) {
        Ok(result) => result,
        Err(e) => {
            return UiEvent::Thread(crate::events::ThreadUiEvent::ForkFailed {
                error: format!("Failed to start btw tab: {e}"),
            });
        }
    };

    let (agent_tx, agent_rx) = zdx_engine::core::agent::create_event_channel();
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();

    let config = tui.config.clone();
    let refused = selection_error(tui);
    let mut agent_opts = tui.agent_opts.clone();
    let steer = SteerInbox::new();
    agent_opts.steer = Some(SteerSource::Inbox(steer.clone()));
    let system_prompt = tui.system_prompt.clone();
    let thread_id = prepared.thread_handle.id.clone();

    let (tui_tx, tui_rx) = zdx_engine::core::agent::create_event_channel();
    let (persist_tx, persist_rx) = zdx_engine::core::agent::create_event_channel();
    let _broadcaster =
        zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx, persist_tx]);
    let _persist =
        thread_persistence::spawn_thread_persist_task(prepared.thread_handle, persist_rx);

    let run_messages = prepared.run_messages;
    tokio::spawn(async move {
        if let Some(message) = refused {
            let _ = agent_tx.send(refused_turn(message));
            return;
        }
        let _ = zdx_engine::core::agent::run_turn_with_cancel(
            run_messages,
            &config,
            &agent_opts,
            system_prompt.as_deref(),
            Some(&thread_id),
            agent_tx.clone(),
            Some(run_cancel),
        )
        .await;
    });

    UiEvent::AgentSpawned {
        rx: tui_rx,
        cancel,
        thread_handle: prepared.thread_update,
        messages: prepared.messages_update,
        steer: Some(steer),
    }
}

/// Result of preparing a btw tab thread for an agent turn.
struct BtwTabPrepared {
    thread_handle: thread_persistence::Thread,
    run_messages: Vec<ChatMessage>,
    thread_update: Option<thread_persistence::Thread>,
    messages_update: Option<Vec<ChatMessage>>,
}

/// Prepares the btw tab's thread and messages for an agent turn.
fn prepare_btw_tab_thread(
    tui: &TuiState,
    parent_thread_id: Option<&str>,
) -> anyhow::Result<BtwTabPrepared> {
    if let Some(thread_handle) = tui.thread.thread_handle.clone() {
        // Subsequent turn — thread already exists, messages already seeded
        let run_messages = tui.thread.messages.clone();
        Ok(BtwTabPrepared {
            thread_handle,
            run_messages,
            thread_update: None,
            messages_update: None,
        })
    } else {
        // First turn — create the thread. No parent transcript is copied; the
        // seed below points the agent at the parent thread instead.
        let mut thread_handle = thread_persistence::Thread::new_with_root(&tui.agent_opts.root)
            .context("Failed to create btw thread")?;

        // Persist the effective selection: one model spec, or the
        // `mode:<name>` reference it came from.
        thread_handle
            .set_model_override(Some(tui.current_selection()))
            .context("Failed to persist btw thread model override")?;

        // Find the last user message (the one the user just typed) from thread.messages
        // It was added by the input handler before StartAgentTurn was emitted.
        let user_prompt = tui.thread.messages.last().and_then(|m| {
            if m.role == "user" {
                match &m.content {
                    zdx_engine::providers::MessageContent::Text(t) => Some(t.clone()),
                    zdx_engine::providers::MessageContent::Blocks(_) => None,
                }
            } else {
                None
            }
        });

        // Seed the thread with the user's prompt plus a parent-thread pointer.
        // Without a persisted parent there is nothing to point at, so the tab
        // behaves like a plain new thread.
        let mut full_messages: Vec<ChatMessage> = Vec::new();
        if let Some(prompt) = user_prompt {
            let seed = match parent_thread_id {
                Some(parent) => handoff_generation::build_side_thread_seed(parent, &prompt),
                None => prompt,
            };
            thread_handle
                .append(&ThreadEvent::user_message(&seed))
                .context("Failed to persist btw user message")?;
            full_messages.push(ChatMessage::user(seed));
        }

        Ok(BtwTabPrepared {
            thread_handle: thread_handle.clone(),
            run_messages: full_messages.clone(),
            thread_update: Some(thread_handle),
            messages_update: Some(full_messages),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use zdx_engine::config::{Config, ModelMode, ThinkingLevel};

    use super::selection_error;
    use crate::state::AppState;

    /// A tab kept on a mode its config does not define is refused at turn
    /// start; a defined mode and a concrete pin run.
    #[test]
    fn turns_on_an_undefined_mode_are_refused() {
        let config = Config {
            model_modes: vec![ModelMode {
                name: "smart".to_string(),
                description: String::new(),
                primary: "openai:big@high".to_string(),
                alternatives: Vec::new(),
                thinking: ThinkingLevel::High,
            }],
            ..Config::default()
        };
        let mut tui = AppState::new(config, PathBuf::new(), None, None).tui;
        assert_eq!(selection_error(&tui), None);

        tui.thread.model_override = Some("mode:smart".to_string());
        assert_eq!(selection_error(&tui), None);
        tui.thread.model_override = Some("openai:other@low".to_string());
        assert_eq!(selection_error(&tui), None);

        tui.thread.model_override = Some("mode:gone".to_string());
        let refused = selection_error(&tui).expect("refused");
        assert!(refused.contains("`mode:gone`"), "{refused}");
        assert!(refused.contains("configured: smart"), "{refused}");
    }
}
