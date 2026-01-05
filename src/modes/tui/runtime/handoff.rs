//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from session context.

use tokio::sync::{mpsc, oneshot};

use crate::core::session::{self, Session, SessionEvent};
use crate::modes::tui::events::UiEvent;

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

/// Executes a handoff submit: creates a new session and persists the prompt.
///
/// Returns the new session for the reducer to store, or an error string.
pub fn execute_handoff_submit(prompt: &str) -> Result<Session, String> {
    let mut session = session::Session::new().map_err(|e| e.to_string())?;

    if let Err(_err) = session.append(&SessionEvent::user_message(prompt)) {
        // Errors are silently ignored for session persistence
    }

    Ok(session)
}

/// Spawns an async task to generate a handoff prompt using a subagent.
pub fn spawn_handoff_generation(
    session_id: &str,
    goal: &str,
    root: &std::path::Path,
) -> UiEvent {
    use tokio::process::Command;

    let (tx, rx) = mpsc::channel::<Result<String, String>>(1);
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Load session content upfront to avoid tool call in subagent
    let session_content = match session::load_session(session_id) {
        Ok(events) if !events.is_empty() => session::format_transcript(&events),
        Ok(_) => {
            let _ = tx.try_send(Err(format!(
                "Handoff failed: Session '{}' is empty",
                session_id
            )));
            return UiEvent::HandoffGenerationStarted {
                goal: goal.to_string(),
                rx,
                cancel_tx,
            };
        }
        Err(e) => {
            let _ = tx.try_send(Err(format!(
                "Handoff failed: Could not load session: {}",
                e
            )));
            return UiEvent::HandoffGenerationStarted {
                goal: goal.to_string(),
                rx,
                cancel_tx,
            };
        }
    };

    let generation_prompt = build_handoff_prompt(&session_content, goal);
    let root = root.to_path_buf();

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

    UiEvent::HandoffGenerationStarted {
        goal: goal.to_string(),
        rx,
        cancel_tx,
    }
}
