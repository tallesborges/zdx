use zdx_core::core::{interrupt, thread_log};

use crate::modes::tui::app::TuiState;
use crate::modes::tui::events::UiEvent;

/// Interrupts the running agent.
pub fn interrupt_agent(tui: &TuiState) {
    if tui.agent_state.is_running() {
        interrupt::trigger_ctrl_c();
    }
}

/// Spawns an agent turn.
pub fn spawn_agent_turn(tui: &TuiState) -> UiEvent {
    let (agent_tx, agent_rx) = zdx_core::core::agent::create_event_channel();

    let messages = tui.thread.messages.clone();
    let config = tui.config.clone();
    let agent_opts = tui.agent_opts.clone();
    let system_prompt = tui.system_prompt.clone();
    let thread_id = tui.thread.thread_log.as_ref().map(|h| h.id.clone());

    let (tui_tx, tui_rx) = zdx_core::core::agent::create_event_channel();

    if let Some(thread_log_handle) = tui.thread.thread_log.clone() {
        let (persist_tx, persist_rx) = zdx_core::core::agent::create_event_channel();
        let _broadcaster =
            zdx_core::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx, persist_tx]);
        let _persist = thread_log::spawn_thread_persist_task(thread_log_handle, persist_rx);
    } else {
        let _broadcaster = zdx_core::core::agent::spawn_broadcaster(agent_rx, vec![tui_tx]);
    }

    // Spawn the agent task - it will send TurnCompleted when done
    tokio::spawn(async move {
        let _ = zdx_core::core::agent::run_turn(
            messages,
            &config,
            &agent_opts,
            system_prompt.as_deref(),
            thread_id.as_deref(),
            agent_tx,
        )
        .await;
    });

    UiEvent::AgentSpawned { rx: tui_rx }
}
