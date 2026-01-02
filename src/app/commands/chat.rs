//! Chat command handler.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::exec;
use crate::core::session::SessionPersistenceOptions;
use crate::{config, ui};

pub async fn run(
    root: &str,
    session_opts: &SessionPersistenceOptions,
    config: &config::Config,
) -> Result<()> {
    // If stdin is piped, run exec mode instead
    if !std::io::stdin().is_terminal() {
        let mut prompt = String::new();
        std::io::stdin().lock().read_to_string(&mut prompt)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            anyhow::bail!("No input provided via pipe");
        }
        return exec::run(root, session_opts, prompt, config, None, None).await;
    }

    let session = session_opts.resolve().context("resolve session")?;

    let root_path = PathBuf::from(root);
    ui::run_interactive_chat(config, session, root_path)
        .await
        .context("interactive chat failed")?;

    Ok(())
}
