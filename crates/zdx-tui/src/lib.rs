//! Full-screen TUI implementation for ZDX.

pub mod common;
pub mod effects;
pub mod events;
pub mod features;
pub mod mutations;
pub mod overlays;
pub mod render;
pub mod runtime;
pub mod state;
pub mod terminal;
pub mod update;

use std::io::{IsTerminal, Write, stderr};
use std::path::PathBuf;

use anyhow::Result;
pub use features::transcript::markdown;
pub use features::{auth, input, statusline, thread, transcript};
pub use runtime::TuiRuntime;
use zdx_core::config::Config;
use zdx_core::core::thread_log::ThreadLog;
use zdx_core::providers::ChatMessage;

use crate::transcript::HistoryCell;

/// Runs the interactive chat loop.
pub async fn run_interactive_chat(
    config: &Config,
    thread_log: Option<ThreadLog>,
    root: PathBuf,
) -> Result<()> {
    run_interactive_chat_with_history(config, thread_log, Vec::new(), root).await
}

/// Runs the interactive chat loop with pre-loaded history.
pub async fn run_interactive_chat_with_history(
    config: &Config,
    thread_log: Option<ThreadLog>,
    history: Vec<ChatMessage>,
    root: PathBuf,
) -> Result<()> {
    // Chat mode requires a terminal to render the TUI
    if !stderr().is_terminal() {
        anyhow::bail!(
            "Chat mode requires a terminal.\n\
             Use `zdx exec --prompt '...'` for non-interactive execution."
        );
    }

    let effective =
        zdx_core::core::context::build_effective_system_prompt_with_paths(config, &root)?;

    // Print pre-TUI info to stderr (will be replaced by alternate screen)
    let mut err = stderr();
    writeln!(err, "ZDX Chat")?;
    writeln!(err, "Model: {}", config.model)?;
    if let Some(ref s) = thread_log {
        writeln!(err, "Thread: {}", s.id)?;
    }
    if !history.is_empty() {
        writeln!(err, "Loaded {} previous messages", history.len())?;
    }

    // Emit warnings from context loading (per SPEC §10)
    for warning in &effective.warnings {
        writeln!(err, "Warning: {}", warning.message)?;
    }

    // Small delay so user can see the info before TUI takes over
    err.flush()?;

    // Create and run the TUI
    let mut runtime = if history.is_empty() {
        TuiRuntime::new(config.clone(), root, effective.prompt, thread_log)?
    } else {
        TuiRuntime::with_history(config.clone(), root, effective.prompt, thread_log, history)?
    };

    // Add system message for config path (only if config exists on disk).
    let config_path = zdx_core::config::paths::config_path();
    if config_path.exists() {
        let message = format!("Config file: {}", config_path.display());
        runtime
            .state
            .tui
            .transcript
            .push_cell(HistoryCell::system(message));
    }

    // Add system message for thread path
    if let Some(ref s) = runtime.state.tui.thread.thread_log {
        let thread_path_msg = format!("Thread path: {}", s.path().display());
        runtime
            .state
            .tui
            .transcript
            .push_cell(HistoryCell::system(thread_path_msg));
    }

    // Add system message for loaded AGENTS.md files to transcript
    if !effective.loaded_agents_paths.is_empty() {
        let paths_list: Vec<String> = effective
            .loaded_agents_paths
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect();
        let message = format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n"));
        runtime
            .state
            .tui
            .transcript
            .push_cell(HistoryCell::system(message));
    }

    // Add system message for loaded skills to transcript
    if !effective.loaded_skills.is_empty() {
        let names_list: Vec<String> = effective
            .loaded_skills
            .iter()
            .map(|skill| format!("  - {}", skill.name))
            .collect();
        let message = format!("Loaded skills:\n{}", names_list.join("\n"));
        runtime
            .state
            .tui
            .transcript
            .push_cell(HistoryCell::system(message));
    }

    runtime.run()?;

    // Print goodbye after TUI exits (terminal restored)
    writeln!(stderr(), "Goodbye!")?;

    Ok(())
}
