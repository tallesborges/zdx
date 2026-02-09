//! Handoff generation handlers.
//!
//! Handles spawning a subagent to generate handoff prompts from thread context.
//!
//! Uses `CancellationToken` for unified cancellation model.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use zdx_core::config::Config;
use zdx_core::core::thread_persistence::{self, Thread};
use zdx_core::prompts::HANDOFF_PROMPT_TEMPLATE;

use crate::events::UiEvent;

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
    let events = thread_persistence::load_thread_events(thread_id)
        .map_err(|e| format!("Handoff failed: Could not load thread: {}", e))?;

    if events.is_empty() {
        return Err(format!("Handoff failed: Thread '{}' is empty", thread_id));
    }

    Ok(thread_persistence::format_transcript(&events))
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
/// Pure async function - returns the generated prompt or error.
/// Uses `CancellationToken` for unified cancellation.
async fn run_subagent(
    cancel: CancellationToken,
    handoff_model: String,
    generation_prompt: String,
    root: PathBuf,
) -> Result<String, String> {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Err(format!("Failed to get executable: {}", e)),
    };

    let child = match Command::new(exe)
        .args([
            "--no-thread",
            "exec",
            "-m",
            &handoff_model,
            "-t",
            HANDOFF_THINKING,
            "--tools",
            "read",
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
        Err(e) => return Err(format!("Failed to spawn subagent: {}", e)),
    };

    // Use select! to race cancellation against the subagent completion
    tokio::select! {
        _ = cancel.cancelled() => Err("Handoff cancelled".to_string()),
        output = tokio::time::timeout(
            Duration::from_secs(HANDOFF_TIMEOUT_SECS),
            child.wait_with_output()
        ) => {
            output
                .map_err(|_| format!("Handoff generation timed out after {} seconds", HANDOFF_TIMEOUT_SECS))
                .and_then(|r| r.map_err(|e| format!("Failed to get subagent output: {}", e)))
                .and_then(process_subagent_output)
        }
    }
}

/// Executes a handoff submit: creates a new thread with handoff source.
///
/// Returns the new thread for the reducer to store, or an error string.
pub fn execute_handoff_submit(
    config: &Config,
    root: &Path,
    handoff_from: Option<String>,
) -> Result<(Thread, Vec<PathBuf>), String> {
    let thread_handle = thread_persistence::Thread::new_with_root_and_source(root, handoff_from)
        .map_err(|e| e.to_string())?;

    let context_paths =
        match zdx_core::core::context::build_effective_system_prompt_with_paths(config, root) {
            Ok(effective) => effective.loaded_agents_paths,
            Err(_) => Vec::new(),
        };

    Ok((thread_handle, context_paths))
}

/// Runs handoff generation with cancellation support.
///
/// Returns HandoffResult; cancellation is cooperative via token.
pub async fn handoff_generation(
    thread_id: String,
    goal: String,
    handoff_model: String,
    root: PathBuf,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let cancel = cancel.unwrap_or_default();

    // Load thread content synchronously (it's quick I/O)
    let thread_content = load_thread_content(&thread_id);

    let content = match thread_content {
        Ok(content) => content,
        Err(e) => {
            return UiEvent::HandoffResult {
                goal,
                result: Err(e),
            };
        }
    };

    let generation_prompt = build_handoff_prompt(&content, &goal);
    let result = run_subagent(cancel, handoff_model, generation_prompt, root).await;
    UiEvent::HandoffResult { goal, result }
}
