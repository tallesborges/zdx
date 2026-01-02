//! Exec command handler.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::{self, ThinkingLevel};
use crate::core::session::SessionPersistenceOptions;
use crate::ui;

pub async fn run(
    root: &str,
    session_opts: &SessionPersistenceOptions,
    prompt: &str,
    config: &config::Config,
    model_override: Option<&str>,
    thinking_override: Option<&str>,
) -> Result<()> {
    let session = session_opts.resolve().context("resolve session")?;

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

    let exec_opts = ui::exec::ExecOptions {
        root: PathBuf::from(root),
    };

    // Use streaming variant - response is printed incrementally, final newline added at end
    ui::exec::run_exec(prompt, &config, session, &exec_opts)
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
        _ => anyhow::bail!("Invalid thinking level '{}'. Valid options: off, minimal, low, medium, high", s),
    }
}
