//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from thread context.
//!
//! Uses the spawn_effect_pair pattern: returns (started_event, future) where
//! the started event contains the cancel token and the future does the work.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::oneshot;

use crate::core::thread_log::{self, ThreadEvent, ThreadLog};
use crate::modes::tui::events::UiEvent;

const HANDOFF_PROMPT_TEMPLATE: &str = crate::prompt_str!("handoff_prompt.md");

/// Model to use for handoff generation (fast, cheap).
/// Uses claude-cli prefix to route through OAuth auth (Claude CLI).
const HANDOFF_MODEL: &str = "gemini-cli:gemini-2.5-flash";

/// Thinking level for handoff generation (minimal reasoning).
const HANDOFF_THINKING: &str = "minimal";

/// Timeout for handoff generation subagent (2 minutes).
const HANDOFF_TIMEOUT_SECS: u64 = 120;

/// Builds the prompt for handoff generation.
fn build_handoff_prompt(thread_content: &str, goal: &str) -> String {
    HANDOFF_PROMPT_TEMPLATE
        .replace("{{THREAD_CONTENT}}", thread_content)
        .replace("{{GOAL}}", goal)
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
///
/// Pure async function - returns UiEvent::HandoffResult directly.
async fn run_subagent(
    mut cancel_rx: oneshot::Receiver<()>,
    generation_prompt: String,
    root: PathBuf,
) -> UiEvent {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            return UiEvent::HandoffResult(Err(format!("Failed to get executable: {}", e)));
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
            return UiEvent::HandoffResult(Err(format!("Failed to spawn subagent: {}", e)));
        }
    };

    // Cancel fires on explicit send(()) OR when cancel handle is dropped
    let result = tokio::select! {
        cancelled = &mut cancel_rx => match cancelled {
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

    UiEvent::HandoffResult(result)
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

/// Prepares handoff generation with cancellation support.
///
/// Returns (started_event, future) - started event contains cancel token,
/// future runs the subagent and returns HandoffResult.
pub fn handoff_generation(
    thread_id: &str,
    goal: &str,
    root: &Path,
) -> (
    UiEvent,
    impl std::future::Future<Output = UiEvent> + Send + 'static,
) {
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let goal_string = goal.to_string();

    let started = UiEvent::HandoffGenerationStarted {
        goal: goal_string.clone(),
        cancel: cancel_tx,
    };

    // Load thread content synchronously (it's quick I/O)
    let thread_content = load_thread_content(thread_id);
    let root = root.to_path_buf();

    let future = async move {
        let content = match thread_content {
            Ok(content) => content,
            Err(e) => return UiEvent::HandoffResult(Err(e)),
        };

        let generation_prompt = build_handoff_prompt(&content, &goal_string);
        run_subagent(cancel_rx, generation_prompt, root).await
    };

    (started, future)
}
