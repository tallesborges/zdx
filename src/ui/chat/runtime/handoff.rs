//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from session context.

use tokio::sync::mpsc;

use super::handlers::{
    create_session_and_show_context, push_system, push_warning, spawn_agent_turn,
};
use crate::core::session::SessionEvent;
use crate::providers::anthropic::ChatMessage;
use crate::ui::chat::state::{HandoffState, SessionUsage, TuiState};
use crate::ui::transcript::HistoryCell;

/// Timeout for handoff generation subagent (2 minutes).
const HANDOFF_TIMEOUT_SECS: u64 = 120;

/// Builds the prompt for handoff generation.
fn build_handoff_prompt(session_id: &str, goal: &str) -> String {
    format!(
        r#"Read session {session_id} using this command:
zdx sessions show {session_id}

Based on that session, generate a focused handoff prompt for the following goal:

<goal>
{goal}
</goal>

Include:
- Relevant context and decisions made
- Key files or code discussed
- The specific goal/direction

Output ONLY the handoff prompt text, nothing else. The prompt should be
written as if the user is starting a fresh conversation with a new agent."#
    )
}

/// Executes a handoff submit: creates new session and sends prompt as first message.
pub fn execute_handoff_submit(state: &mut TuiState, prompt: &str) {
    // 1. Clear state (like /new)
    state.transcript.cells.clear();
    state.conversation.messages.clear();
    state.input.history.clear();
    state.transcript.scroll.reset();
    state.conversation.usage = SessionUsage::new();
    state.transcript.wrap_cache.clear();

    // 2. Create new session (continue even if it fails - user can still chat)
    let _ = create_session_and_show_context(state);

    // 3. Add user message to transcript and conversation
    state.input.history.push(prompt.to_string());
    state.transcript.cells.push(HistoryCell::user(prompt));
    state.conversation.messages.push(ChatMessage::user(prompt));

    // 4. Save user message to session
    if let Some(ref mut s) = state.conversation.session
        && let Err(e) = s.append(&SessionEvent::user_message(prompt))
    {
        push_warning(state, "Warning: Failed to save session", e);
    }

    // 5. Start agent turn
    spawn_agent_turn(state);
}

/// Spawns an async task to generate a handoff prompt using a subagent.
pub fn spawn_handoff_generation(state: &mut TuiState, session_id: &str, goal: &str) {
    use tokio::process::Command;
    use tokio::sync::oneshot;

    let generation_prompt = build_handoff_prompt(session_id, goal);
    let root = state.agent_opts.root.clone();

    let (tx, rx) = mpsc::channel::<Result<String, String>>(1);
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Transition to Generating state with all necessary data
    state.input.handoff = HandoffState::Generating {
        goal: goal.to_string(),
        rx,
        cancel_tx,
    };

    // Show status in transcript
    push_system(
        state,
        format!("Generating handoff for goal: \"{}\"...", goal),
    );

    tokio::spawn(async move {
        // Get the current executable path
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                let _ = tx
                    .send(Err(format!("Failed to get executable: {}", e)))
                    .await;
                return;
            }
        };

        // Spawn the subagent process (async)
        let child = match Command::new(exe)
            .args(["--no-save", "exec", "-p", &generation_prompt])
            .current_dir(&root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true) // Kill child if task is dropped/cancelled
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx
                    .send(Err(format!("Failed to spawn subagent: {}", e)))
                    .await;
                return;
            }
        };

        // Wait for output with timeout and cancellation support
        let result = tokio::select! {
            // Cancellation signal (user pressed Esc)
            _ = cancel_rx => {
                // kill_on_drop will handle cleanup when child is dropped
                Err("Handoff cancelled".to_string())
            }
            // Timeout
            output_result = async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(HANDOFF_TIMEOUT_SECS),
                    child.wait_with_output()
                ).await
            } => {
                match output_result {
                    Ok(Ok(output)) => {
                        if output.status.success() {
                            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                            if stdout.is_empty() {
                                Err("Handoff generation returned empty output".to_string())
                            } else {
                                Ok(stdout)
                            }
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                            Err(format!("Handoff generation failed: {}", stderr.trim()))
                        }
                    }
                    Ok(Err(e)) => Err(format!("Failed to get subagent output: {}", e)),
                    Err(_) => {
                        // Timeout elapsed - child will be killed on drop
                        Err(format!("Handoff generation timed out after {} seconds", HANDOFF_TIMEOUT_SECS))
                    }
                }
            }
        };

        let _ = tx.send(result).await;
    });
}
