use anyhow::Context;
use tokio_util::sync::CancellationToken;
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

/// Spawns an agent turn for the active tab.
///
/// For btw tabs, this prepends the forked base messages and creates a
/// persistent thread on the first send.
pub fn spawn_agent_turn(tui: &TuiState) -> UiEvent {
    // For btw tabs, handle thread creation and message merging
    if let TabKind::Btw { ref base_messages } = tui.tab_kind {
        return spawn_btw_tab_turn(tui, base_messages);
    }

    let (agent_tx, agent_rx) = zdx_engine::core::agent::create_event_channel();
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();

    let messages = tui.thread.messages.clone();
    let config = tui.config.clone();
    let agent_opts = tui.agent_opts.clone();
    let system_prompt = tui.system_prompt.clone();
    let thread_id = tui.thread.thread_handle.as_ref().map(|h| h.id.clone());

    let (tui_tx, tui_rx) = zdx_engine::core::agent::create_event_channel();

    if let Some(thread_handle) = tui.thread.thread_handle.clone() {
        let (persist_tx, persist_rx) = zdx_engine::core::agent::create_event_channel();
        let _broadcaster =
            zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx, persist_tx]);
        let _persist = thread_persistence::spawn_thread_persist_task_with_completed_messages(
            thread_handle,
            persist_rx,
            true,
        );
    } else {
        let _broadcaster = zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx]);
    }

    // Spawn the agent task - it will send TurnFinished when done
    tokio::spawn(async move {
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
    }
}

/// Spawns an agent turn for a btw tab.
///
/// On the first send, creates a persistent thread and writes the forked
/// base-message context to it. On subsequent sends, reuses the existing
/// thread. The agent always sees `base_messages + btw_messages` as the
/// conversation, but only btw-specific turns are persisted incrementally.
fn spawn_btw_tab_turn(tui: &TuiState, base_messages: &[ChatMessage]) -> UiEvent {
    // Prepare thread and messages (create thread on first send)
    let prepared = match prepare_btw_tab_thread(tui, base_messages) {
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
    let agent_opts = tui.agent_opts.clone();
    let system_prompt = tui.system_prompt.clone();
    let thread_id = prepared.thread_handle.id.clone();

    let (tui_tx, tui_rx) = zdx_engine::core::agent::create_event_channel();
    let (persist_tx, persist_rx) = zdx_engine::core::agent::create_event_channel();
    let _broadcaster =
        zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx, persist_tx]);
    let _persist = thread_persistence::spawn_thread_persist_task_with_completed_messages(
        prepared.thread_handle,
        persist_rx,
        true,
    );

    let run_messages = prepared.run_messages;
    tokio::spawn(async move {
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
    base_messages: &[ChatMessage],
) -> anyhow::Result<BtwTabPrepared> {
    if let Some(thread_handle) = tui.thread.thread_handle.clone() {
        // Subsequent turn — thread already exists, messages already contain base context
        let run_messages = tui.thread.messages.clone();
        Ok(BtwTabPrepared {
            thread_handle,
            run_messages,
            thread_update: None,
            messages_update: None,
        })
    } else {
        // First turn — create thread, persist base context
        let mut thread_handle = thread_persistence::Thread::new_with_root(&tui.agent_opts.root)
            .context("Failed to create btw thread")?;

        // Write base messages as thread events
        let events = thread_persistence::messages_to_events(base_messages);
        for event in &events {
            thread_handle
                .append(event)
                .context("Failed to persist btw thread context")?;
        }

        // Persist model/thinking overrides
        thread_handle
            .set_model_override(Some(tui.config.model.clone()))
            .context("Failed to persist btw thread model override")?;
        thread_handle
            .set_thinking_override(Some(tui.config.thinking_level))
            .context("Failed to persist btw thread thinking override")?;

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

        // Build the full message list: base_messages + the user's new prompt
        let mut full_messages: Vec<ChatMessage> = base_messages.to_vec();
        if let Some(ref prompt) = user_prompt {
            thread_handle
                .append(&ThreadEvent::user_message(prompt))
                .context("Failed to persist btw user message")?;
            full_messages.push(ChatMessage::user(prompt));
        }

        Ok(BtwTabPrepared {
            thread_handle: thread_handle.clone(),
            run_messages: full_messages.clone(),
            thread_update: Some(thread_handle),
            messages_update: Some(full_messages),
        })
    }
}
