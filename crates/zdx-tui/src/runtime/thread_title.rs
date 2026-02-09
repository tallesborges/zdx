//! Auto thread title generation.
//!
//! Spawns a subagent to suggest a thread title from the first user message.
//! The result is written to the thread meta without emitting UI messages.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use zdx_core::core::thread_persistence;
use zdx_core::prompts::THREAD_TITLE_PROMPT_TEMPLATE;

use crate::events::{ThreadUiEvent, UiEvent};

/// Thinking level for auto-title generation (minimal reasoning).
const TITLE_THINKING: &str = "minimal";

/// Timeout for auto-title generation subagent (1 minute).
const TITLE_TIMEOUT_SECS: u64 = 60;

fn build_title_prompt(message: &str) -> String {
    THREAD_TITLE_PROMPT_TEMPLATE.replace("{{MESSAGE}}", message)
}

fn sanitize_title(raw: &str) -> Option<String> {
    let mut line = raw
        .lines()
        .find(|l| !l.trim().is_empty())?
        .trim()
        .to_string();

    for prefix in ["title:", "Title:"] {
        if let Some(rest) = line.strip_prefix(prefix) {
            line = rest.trim().to_string();
            break;
        }
    }

    let trimmed = line
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string();

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

async fn run_subagent(
    prompt: String,
    title_model: String,
    root: PathBuf,
) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Failed to get executable: {e}"))?;

    let child = Command::new(exe)
        .args([
            "--no-thread",
            "exec",
            "-m",
            &title_model,
            "-t",
            TITLE_THINKING,
            "--no-tools",
            "-p",
            &prompt,
        ])
        .current_dir(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to spawn subagent: {e}"))?;

    let output = tokio::time::timeout(
        Duration::from_secs(TITLE_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_elapsed| format!("Auto-title generation timed out after {TITLE_TIMEOUT_SECS} seconds"))
    .and_then(|r| r.map_err(|e| format!("Failed to get subagent output: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Auto-title generation failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Generates a thread title and persists it (if still unset).
///
/// Returns `UiEvent::Thread(ThreadUiEvent::TitleSuggested)` (title None on failure or skip).
pub async fn suggest_thread_title(
    thread_id: String,
    message: String,
    title_model: String,
    root: PathBuf,
) -> UiEvent {
    let prompt = build_title_prompt(&message);

    let title = match run_subagent(prompt, title_model, root).await {
        Ok(output) => sanitize_title(&output),
        Err(_) => None,
    };

    if title.is_none() {
        return UiEvent::Thread(ThreadUiEvent::TitleSuggested {
            thread_id,
            title: None,
        });
    }

    let title = title
        .and_then(|title| thread_persistence::set_thread_title(&thread_id, Some(title)).ok())
        .flatten();

    UiEvent::Thread(ThreadUiEvent::TitleSuggested { thread_id, title })
}
