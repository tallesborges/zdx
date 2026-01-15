//! Chat command handler.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_core::config;
use zdx_core::core::thread_log::ThreadPersistenceOptions;

use super::exec;
use crate::modes;

pub async fn run(
    root: &str,
    thread_opts: &ThreadPersistenceOptions,
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
        return exec::run(root, thread_opts, prompt, config, None, None, None).await;
    }

    let root_path = PathBuf::from(root);
    let thread = thread_opts.resolve(&root_path).context("resolve thread")?;

    modes::run_interactive_chat(config, thread, root_path)
        .await
        .context("interactive chat failed")?;

    Ok(())
}
