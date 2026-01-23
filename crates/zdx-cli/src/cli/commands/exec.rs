//! Exec command handler.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_core::config::{self, ThinkingLevel};
use zdx_core::core::agent::{ToolConfig, ToolSelection};
use zdx_core::core::thread_log::ThreadPersistenceOptions;
use zdx_core::tools;

use crate::modes;

pub struct ExecRunOptions<'a> {
    pub root: &'a str,
    pub thread_opts: &'a ThreadPersistenceOptions,
    pub prompt: &'a str,
    pub config: &'a config::Config,
    pub model_override: Option<&'a str>,
    pub thinking_override: Option<&'a str>,
    pub tools_override: Option<&'a str>,
    pub no_tools: bool,
}

pub async fn run(options: ExecRunOptions<'_>) -> Result<()> {
    let root_path = PathBuf::from(options.root);
    let thread = options
        .thread_opts
        .resolve(&root_path)
        .context("resolve thread")?;

    // Apply overrides if provided
    let config = {
        let mut c = options.config.clone();
        if let Some(model) = options.model_override {
            c.model = model.to_string();
        }
        if let Some(thinking) = options.thinking_override {
            c.thinking_level = parse_thinking_level(thinking)?;
        }
        c
    };

    let exec_opts = modes::exec::ExecOptions {
        root: root_path,
        tool_config: ToolConfig::default().with_selection(if options.no_tools {
            ToolSelection::Explicit(Vec::new())
        } else if let Some(raw) = options.tools_override {
            ToolSelection::Explicit(parse_tools_override(raw)?)
        } else {
            ToolSelection::default()
        }),
    };

    // Use streaming variant - response is printed incrementally, final newline added at end
    modes::exec::run_exec(options.prompt, &config, thread, &exec_opts)
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

fn parse_tools_override(raw: &str) -> Result<Vec<String>> {
    let tools: Vec<String> = raw
        .split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();

    if tools.is_empty() {
        anyhow::bail!("--tools requires a comma-separated list of tool names");
    }

    let available = tools::all_tool_names();
    let available_set: std::collections::HashSet<String> =
        available.iter().map(|t| t.to_ascii_lowercase()).collect();
    let mut unknown: Vec<String> = tools
        .iter()
        .filter(|t| !available_set.contains(&t.to_ascii_lowercase()))
        .cloned()
        .collect();

    if !unknown.is_empty() {
        unknown.sort();
        let mut available_sorted = available;
        available_sorted.sort();
        anyhow::bail!(
            "Unknown tool(s): {}. Available tools: {}",
            unknown.join(", "),
            available_sorted.join(", ")
        );
    }

    Ok(tools)
}
