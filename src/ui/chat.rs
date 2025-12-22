//! Interactive chat module for ZDX.
//!
//! Provides a full-screen TUI chat interface using TUI2.
//! The interface maintains conversation history and supports session persistence.

use std::io::{Write, stderr};
use std::path::PathBuf;

use anyhow::Result;

use crate::config::Config;
use crate::engine::session::Session;
use crate::providers::anthropic::ChatMessage;
use crate::ui::Tui2App;

/// Runs the interactive chat loop.
pub async fn run_interactive_chat(
    config: &Config,
    session: Option<Session>,
    root: PathBuf,
) -> Result<()> {
    run_interactive_chat_with_history(config, session, Vec::new(), root).await
}

/// Runs the interactive chat loop with pre-loaded history.
pub async fn run_interactive_chat_with_history(
    config: &Config,
    session: Option<Session>,
    history: Vec<ChatMessage>,
    root: PathBuf,
) -> Result<()> {
    // Chat mode requires a terminal to render the TUI
    use std::io::IsTerminal;
    if !stderr().is_terminal() {
        anyhow::bail!(
            "Chat mode requires a terminal.\n\
             Use `zdx exec --prompt '...'` for non-interactive execution."
        );
    }

    let effective =
        crate::shared::context::build_effective_system_prompt_with_paths(config, &root)?;

    // Print pre-TUI info to stderr (will be replaced by alternate screen)
    let mut err = stderr();
    writeln!(err, "ZDX Chat")?;
    writeln!(err, "Model: {}", config.model)?;
    if let Some(ref s) = session {
        writeln!(err, "Session: {}", s.id)?;
    }
    if !history.is_empty() {
        writeln!(err, "Loaded {} previous messages", history.len())?;
    }

    // Emit warnings from context loading (per SPEC §10)
    for warning in &effective.warnings {
        writeln!(err, "Warning: {}", warning.message)?;
    }

    // Show loaded AGENTS.md files
    if !effective.loaded_agents_paths.is_empty() {
        writeln!(err, "Loaded AGENTS.md from:")?;
        for path in &effective.loaded_agents_paths {
            writeln!(err, "  - {}", path.display())?;
        }
    }

    // Small delay so user can see the info before TUI takes over
    // (alternate screen will hide all this output)
    err.flush()?;

    // Create and run the TUI
    let mut app = if history.is_empty() {
        Tui2App::new(config.clone(), root, effective.prompt, session)?
    } else {
        Tui2App::with_history(config.clone(), root, effective.prompt, session, history)?
    };
    app.run()?;

    // Print goodbye after TUI exits (terminal restored)
    writeln!(stderr(), "Goodbye!")?;

    Ok(())
}
