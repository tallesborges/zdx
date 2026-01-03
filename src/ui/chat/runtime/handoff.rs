//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from session context.

use tokio::sync::mpsc;

use super::handlers::spawn_agent_turn;
use crate::core::session::{self, SessionEvent};
use crate::ui::chat::state::{HandoffState, TuiState};
use crate::ui::transcript::HistoryCell;

/// Model to use for handoff generation (fast, cheap).
const HANDOFF_MODEL: &str = "claude-haiku-4-5";

/// Thinking level for handoff generation (minimal reasoning).
const HANDOFF_THINKING: &str = "minimal";

/// Timeout for handoff generation subagent (2 minutes).
const HANDOFF_TIMEOUT_SECS: u64 = 120;

/// Builds the prompt for handoff generation.
fn build_handoff_prompt(session_content: &str, goal: &str) -> String {
    format!(
        r#"Based on the following session transcript, generate a focused handoff prompt for the given goal.

<session>
{session_content}
</session>

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

/// Executes a handoff submit: creates session and starts agent turn.
///
/// State mutations (clearing conversation, adding user message to transcript/conversation)
/// happen in the reducer before this effect is executed.
/// This creates a new session, saves the message, and starts the agent turn.
///
/// Note: Session creation is done synchronously here. This is a minor
/// architectural compromise for the handoff flow simplicity.
pub fn execute_handoff_submit(state: &mut TuiState, prompt: &str) {
    // 1. Create new session (sync - this is fast I/O)
    match session::Session::new() {
        Ok(new_session) => {
            let session_path = new_session.path().display().to_string();
            state.conversation.session = Some(new_session);
            state
                .transcript
                .cells
                .push(HistoryCell::system(format!("Session path: {}", session_path)));
        }
        Err(e) => {
            state
                .transcript
                .cells
                .push(HistoryCell::system(format!(
                    "Warning: Failed to create session: {}",
                    e
                )));
            // Continue without session - user can still chat
        }
    }

    // 2. Save user message to session if exists
    if let Some(ref mut s) = state.conversation.session {
        let _ = s.append(&SessionEvent::user_message(prompt));
        // Errors are silently ignored for session persistence
    }

    // 3. Start agent turn
    spawn_agent_turn(state);
}

/// Spawns an async task to generate a handoff prompt using a subagent.
///
/// Note: This function still has some state mutations for error paths.
/// These are marked as medium-priority violations. The ideal pattern would be
/// to make session loading async and return errors via events.
pub fn spawn_handoff_generation(state: &mut TuiState, session_id: &str, goal: &str) {
    use tokio::process::Command;
    use tokio::sync::oneshot;

    // Load session content upfront to avoid tool call in subagent
    // Note: This is synchronous I/O + state mutation - a remaining violation
    let session_content = match session::load_session(session_id) {
        Ok(events) if !events.is_empty() => session::format_transcript(&events),
        Ok(_) => {
            state.input.handoff = HandoffState::Idle;
            state
                .transcript
                .cells
                .push(HistoryCell::system(format!(
                    "Handoff failed: Session '{}' is empty",
                    session_id
                )));
            return;
        }
        Err(e) => {
            state.input.handoff = HandoffState::Idle;
            state
                .transcript
                .cells
                .push(HistoryCell::system(format!(
                    "Handoff failed: Could not load session: {}",
                    e
                )));
            return;
        }
    };

    let generation_prompt = build_handoff_prompt(&session_content, goal);
    let root = state.agent_opts.root.clone();

    let (tx, rx) = mpsc::channel::<Result<String, String>>(1);
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Transition to Generating state with channel receivers for async polling
    state.input.handoff = HandoffState::Generating {
        goal: goal.to_string(),
        rx,
        cancel_tx,
    };

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
        // Args order: --no-save exec -m <model> -t <thinking> -p <prompt>
        let child = match Command::new(exe)
            .args([
                "--no-save",
                "exec",
                "-m",
                HANDOFF_MODEL,
                "-t",
                HANDOFF_THINKING,
                "-p",
                &generation_prompt,
            ])
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
