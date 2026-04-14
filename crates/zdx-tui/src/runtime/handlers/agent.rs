use tokio_util::sync::CancellationToken;
use zdx_engine::core::thread_persistence;

use crate::events::UiEvent;
use crate::state::TuiState;

/// Interrupts the running agent.
pub fn interrupt_agent(tui: &TuiState) {
    if let Some(cancel) = tui.agent_state.cancel_token() {
        cancel.cancel();
    }
}

/// Spawns an agent turn.
pub fn spawn_agent_turn(tui: &TuiState) -> UiEvent {
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

    UiEvent::AgentSpawned { rx: tui_rx, cancel }
}
