//! Runtime execution modes.
//!
//! - `exec`: Non-interactive streaming mode (stdout/stderr)
//! - `tui`: Full-screen interactive terminal UI (optional feature)

pub mod exec;

#[cfg(feature = "tui")]
pub use zdx_tui::{run_interactive_chat, run_interactive_chat_with_history};

#[cfg(not(feature = "tui"))]
pub async fn run_interactive_chat(
    _config: &zdx_core::config::Config,
    _thread_log: Option<zdx_core::core::thread_persistence::ThreadLog>,
    _root: std::path::PathBuf,
) -> anyhow::Result<()> {
    anyhow::bail!("TUI support is disabled in this build (feature \"tui\").");
}

#[cfg(not(feature = "tui"))]
pub async fn run_interactive_chat_with_history(
    _config: &zdx_core::config::Config,
    _thread_log: Option<zdx_core::core::thread_persistence::ThreadLog>,
    _history: Vec<zdx_core::providers::ChatMessage>,
    _root: std::path::PathBuf,
) -> anyhow::Result<()> {
    anyhow::bail!("TUI support is disabled in this build (feature \"tui\").");
}
