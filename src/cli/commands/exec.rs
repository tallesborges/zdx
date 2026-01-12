//! Exec command handler.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_core::config::{self, ThinkingLevel};
use zdx_core::core::thread_log::ThreadPersistenceOptions;

use crate::modes;

pub async fn run(
    root: &str,
    thread_opts: &ThreadPersistenceOptions,
    prompt: &str,
    config: &config::Config,
    model_override: Option<&str>,
    thinking_override: Option<&str>,
) -> Result<()> {
    let root_path = PathBuf::from(root);
    let thread = thread_opts.resolve(&root_path).context("resolve thread")?;

    // Apply overrides if provided
    let config = {
        let mut c = config.clone();
        if let Some(model) = model_override {
            c.model = model.to_string();
        }
        if let Some(thinking) = thinking_override {
            c.thinking_level = parse_thinking_level(thinking)?;
        }
        c
    };

    let exec_opts = modes::exec::ExecOptions { root: root_path };

    // Use streaming variant - response is printed incrementally, final newline added at end
    modes::exec::run_exec(prompt, &config, thread, &exec_opts)
        .await
        .context("execute prompt")?;

    Ok(())
}

fn parse_thinking_level(s: &str) -> Result<ThinkingLevel> {
    match s.to_lowercase().as_str() {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        _ => anyhow::bail!(
            "Invalid thinking level '{}'. Valid options: off, minimal, low, medium, high",
            s
        ),
    }
}
