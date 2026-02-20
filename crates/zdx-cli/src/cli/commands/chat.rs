//! Chat command handler.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_core::config;
use zdx_core::core::thread_persistence::ThreadPersistenceOptions;

use super::exec;
use crate::modes;

pub async fn run(
    root: &str,
    thread_opts: &ThreadPersistenceOptions,
    config: &config::Config,
    model_override: Option<&str>,
    thinking_override: Option<&str>,
) -> Result<()> {
    // If stdin is piped, run exec mode instead
    if !std::io::stdin().is_terminal() {
        let mut prompt = String::new();
        std::io::stdin().lock().read_to_string(&mut prompt)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            anyhow::bail!("No input provided via pipe");
        }
        return exec::run(exec::ExecRunOptions {
            root,
            thread_opts,
            prompt,
            config,
            model_override,
            tool_timeout_override: None,
            thinking_override,
            tools_override: None,
            no_tools: false,
        })
        .await;
    }

    let mut config = config.clone();
    if let Some(model) = model_override {
        config.model = model.to_string();
    }
    if let Some(thinking) = thinking_override {
        config.thinking_level = exec::parse_thinking_level(thinking)?;
    }

    let root_path = PathBuf::from(root);
    let thread = thread_opts.resolve(&root_path).context("resolve thread")?;

    modes::run_interactive_chat(&config, thread, root_path)
        .await
        .context("interactive chat failed")?;

    Ok(())
}
