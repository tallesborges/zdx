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
use zdx_core::core::thread_persistence::Thread;
use zdx_core::providers::ChatMessage;
use zdx_core::skills::Skill;

use crate::transcript::HistoryCell;

/// Runs the interactive chat loop.
pub async fn run_interactive_chat(
    config: &Config,
    thread_handle: Option<Thread>,
    root: PathBuf,
) -> Result<()> {
    run_interactive_chat_with_history(config, thread_handle, Vec::new(), root).await
}

/// Runs the interactive chat loop with pre-loaded history.
pub async fn run_interactive_chat_with_history(
    config: &Config,
    thread_handle: Option<Thread>,
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
    if let Some(ref s) = thread_handle {
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
        TuiRuntime::new(config.clone(), root, effective.prompt, thread_handle)?
    } else {
        TuiRuntime::with_history(
            config.clone(),
            root,
            effective.prompt,
            thread_handle,
            history,
        )?
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

    let thread_path = runtime
        .state
        .tui
        .thread
        .thread_handle
        .as_ref()
        .map(|log| log.path().as_path());
    for message in thread_startup_messages(
        thread_path,
        &effective.loaded_agents_paths,
        &effective.loaded_skills,
    ) {
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

pub(crate) fn thread_startup_messages(
    thread_path: Option<&std::path::Path>,
    context_paths: &[PathBuf],
    skills: &[Skill],
) -> Vec<String> {
    let mut messages = Vec::new();

    if let Some(path) = thread_path {
        messages.push(format!("Thread path: {}", path.display()));
    }

    if !context_paths.is_empty() {
        let paths_list: Vec<String> = context_paths
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect();
        messages.push(format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n")));
    }

    if !skills.is_empty() {
        let skills_list: Vec<String> = skills
            .iter()
            .map(|skill| format!("  - {} ({})", skill.name, skill.source))
            .collect();
        messages.push(format!("Loaded skills:\n{}", skills_list.join("\n")));
    }

    messages
}
