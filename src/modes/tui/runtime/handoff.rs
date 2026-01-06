//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from session context.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::oneshot;

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

/// Loads and validates session content for handoff.
fn load_session_content(session_id: &str) -> Result<String, String> {
    let events = session::load_session(session_id)
        .map_err(|e| format!("Handoff failed: Could not load session: {}", e))?;

    if events.is_empty() {
        return Err(format!("Handoff failed: Session '{}' is empty", session_id));
    }

    Ok(session::format_transcript(&events))
}

/// Processes subagent output into a Result.
fn process_subagent_output(output: std::process::Output) -> Result<String, String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Handoff generation failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("Handoff generation returned empty output".to_string());
    }

    Ok(stdout)
}

/// Runs the subagent process with timeout and cancellation support.
async fn run_subagent(
    tx: oneshot::Sender<Result<String, String>>,
    cancel: oneshot::Receiver<()>,
    generation_prompt: String,
    root: PathBuf,
) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            let _ = tx.send(Err(format!("Failed to get executable: {}", e)));
            return;
        }
    };

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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Err(format!("Failed to spawn subagent: {}", e)));
            return;
        }
    };

    // Cancel fires on explicit send(()) OR when cancel handle is dropped
    let result = tokio::select! {
        cancelled = cancel => match cancelled {
            Ok(()) | Err(_) => Err("Handoff cancelled".to_string()),
        },
        output = tokio::time::timeout(
            Duration::from_secs(HANDOFF_TIMEOUT_SECS),
            child.wait_with_output()
        ) => {
            output
                .map_err(|_| format!("Handoff generation timed out after {} seconds", HANDOFF_TIMEOUT_SECS))
                .and_then(|r| r.map_err(|e| format!("Failed to get subagent output: {}", e)))
                .and_then(process_subagent_output)
        }
    };

    let _ = tx.send(result);
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
pub fn spawn_handoff_generation(session_id: &str, goal: &str, root: &Path) -> UiEvent {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();
    let (cancel, cancel_recv) = oneshot::channel::<()>();

    let session_content = match load_session_content(session_id) {
        Ok(content) => content,
        Err(e) => {
            let _ = tx.send(Err(e));
            return UiEvent::HandoffGenerationStarted {
                goal: goal.to_string(),
                rx,
                cancel,
            };
        }
    };

    let generation_prompt = build_handoff_prompt(&session_content, goal);
    let root = root.to_path_buf();

    tokio::spawn(run_subagent(tx, cancel_recv, generation_prompt, root));

    UiEvent::HandoffGenerationStarted {
        goal: goal.to_string(),
        rx,
        cancel,
    }
}
