//! Full-screen alternate-screen TUI.
//!
//! This module provides a full-screen terminal UI using ratatui.
//! Uses the alternate screen buffer for a persistent, scrollable interface.
//!
//! Architecture (Elm-like):
//! - `TuiRuntime` (in runtime/): Owns terminal + state, runs event loop, executes effects
//!   - `runtime/mod.rs`: Core event loop and effect dispatch
//!   - `runtime/handlers.rs`: Effect handler implementations (I/O, spawning)
//! - `TuiState` (in state/): All app state, no terminal
//! - `update()` (in update.rs): The reducer - all state mutations happen here
//! - `render()` (in render.rs): Pure render, no mutations

// App state composition (see app.rs for state hierarchy)
pub mod app;

// Feature slices (see docs/ARCHITECTURE.md for Elm-like architecture)
pub mod auth;
pub mod input;
pub mod shared;
pub mod thread;

// Core modules
pub mod events;
pub mod markdown;
pub mod overlays;
pub mod render;
pub mod runtime;
pub mod terminal;
pub mod transcript;
pub mod update;

use std::io::{IsTerminal, Write, stderr};
use std::path::PathBuf;

use anyhow::Result;
// Re-export TuiRuntime for external use
pub use runtime::TuiRuntime;

use crate::config::Config;
use crate::core::thread_log::ThreadLog;
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::ChatMessage;

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

    let effective = crate::core::context::build_effective_system_prompt_with_paths(config, &root)?;

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
    let config_path = crate::config::paths::config_path();
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

    runtime.run()?;

    // Print goodbye after TUI exits (terminal restored)
    writeln!(stderr(), "Goodbye!")?;

    Ok(())
}
