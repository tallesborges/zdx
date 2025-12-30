//! Exec command handler.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::core::session::SessionPersistenceOptions;
use crate::{config, ui};

pub async fn run(
    root: &str,
    session_opts: &SessionPersistenceOptions,
    prompt: &str,
    config: &config::Config,
) -> Result<()> {
    let session = session_opts.resolve().context("resolve session")?;

    let exec_opts = ui::exec::ExecOptions {
        root: PathBuf::from(root),
    };

    // Use streaming variant - response is printed incrementally, final newline added at end
    ui::exec::run_exec(prompt, config, session, &exec_opts)
        .await
        .context("execute prompt")?;

    Ok(())
}
