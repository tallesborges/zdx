//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from thread context.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::oneshot;

use crate::core::thread_log::{self, ThreadEvent, ThreadLog};
use crate::modes::tui::events::UiEvent;

/// Model to use for handoff generation (fast, cheap).
const HANDOFF_MODEL: &str = "claude-haiku-4-5";

/// Thinking level for handoff generation (minimal reasoning).
const HANDOFF_THINKING: &str = "minimal";

/// Timeout for handoff generation subagent (2 minutes).
const HANDOFF_TIMEOUT_SECS: u64 = 120;

/// Builds the prompt for handoff generation.
fn build_handoff_prompt(thread_content: &str, goal: &str) -> String {
    format!(
        r#"Based on the following thread transcript, generate a focused handoff prompt for the given goal.

<thread>
{thread_content}
</thread>

<goal>
{goal}
</goal>

Include:
- Relevant context and decisions made
- Key files or code discussed
- The specific goal/direction

Output ONLY the handoff prompt text, nothing else. The prompt should be
written as if the user is starting a fresh thread with a new agent."#
    )
}

/// Loads and validates thread content for handoff.
fn load_thread_content(thread_id: &str) -> Result<String, String> {
    let events = thread_log::load_thread_events(thread_id)
        .map_err(|e| format!("Handoff failed: Could not load thread: {}", e))?;

    if events.is_empty() {
        return Err(format!("Handoff failed: Thread '{}' is empty", thread_id));
    }

    Ok(thread_log::format_transcript(&events))
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
            "--no-thread",
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

/// Executes a handoff submit: creates a new thread and persists the prompt.
///
/// Returns the new thread for the reducer to store, or an error string.
pub fn execute_handoff_submit(prompt: &str, root: &Path) -> Result<ThreadLog, String> {
    let mut thread_log_handle =
        thread_log::ThreadLog::new_with_root(root).map_err(|e| e.to_string())?;

    if let Err(_err) = thread_log_handle.append(&ThreadEvent::user_message(prompt)) {
        // Errors are silently ignored for thread persistence
    }

    Ok(thread_log_handle)
}

/// Spawns an async task to generate a handoff prompt using a subagent.
pub fn spawn_handoff_generation(thread_id: &str, goal: &str, root: &Path) -> UiEvent {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();
    let (cancel, cancel_recv) = oneshot::channel::<()>();

    let thread_content = match load_thread_content(thread_id) {
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

    let generation_prompt = build_handoff_prompt(&thread_content, goal);
    let root = root.to_path_buf();

    tokio::spawn(run_subagent(tx, cancel_recv, generation_prompt, root));

    UiEvent::HandoffGenerationStarted {
        goal: goal.to_string(),
        rx,
        cancel,
    }
}
