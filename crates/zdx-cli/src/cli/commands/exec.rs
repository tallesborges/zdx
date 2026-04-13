//! Exec command handler.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_engine::config::{self, ThinkingLevel};
use zdx_engine::core::agent::{ToolConfig, ToolSelection};
use zdx_engine::core::thread_persistence::ThreadPersistenceOptions;
use zdx_engine::tools::ToolRegistry;

use crate::modes;

pub struct ExecRunOptions<'a> {
    pub root: &'a str,
    pub thread_opts: &'a ThreadPersistenceOptions,
    pub prompt: &'a str,
    pub config: &'a config::Config,
    pub model_override: Option<&'a str>,
    pub effective_system_prompt_override: Option<&'a str>,
    pub tool_timeout_override: Option<u32>,
    pub thinking_override: Option<&'a str>,
    pub event_filter_override: Option<&'a str>,
    pub tools_override: Option<&'a str>,
    pub no_tools: bool,
    pub no_system_prompt: bool,
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
        if let Some(timeout_secs) = options.tool_timeout_override {
            c.tool_timeout_secs = timeout_secs;
        }
        if let Some(thinking) = options.thinking_override {
            c.thinking_level = parse_thinking_level(thinking)?;
        }
        c
    };

    let tool_registry = ToolRegistry::builtins();
    let available_tool_names = tool_registry.tool_names();

    let exec_opts = modes::exec::ExecOptions {
        root: root_path,
        tool_config: ToolConfig::new(
            tool_registry,
            if options.no_tools {
                ToolSelection::Explicit(Vec::new())
            } else if let Some(raw) = options.tools_override {
                ToolSelection::Explicit(parse_tools_override(raw, &available_tool_names)?)
            } else {
                ToolSelection::default()
            },
        ),
        event_filter: options
            .event_filter_override
            .map(parse_event_filter)
            .transpose()?
            .unwrap_or_default(),
        effective_system_prompt: options
            .effective_system_prompt_override
            .map(std::string::ToString::to_string),
        no_system_prompt: options.no_system_prompt,
    };

    // Use streaming variant - response is printed incrementally, final newline added at end
    modes::exec::run_exec(options.prompt, &config, thread, &exec_opts)
        .await
        .context("execute prompt")?;

    Ok(())
}

pub(super) fn parse_thinking_level(s: &str) -> Result<ThinkingLevel> {
    match s.to_lowercase().as_str() {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        _ => anyhow::bail!(
            "Invalid thinking level '{s}'. Valid options: off, minimal, low, medium, high, xhigh"
        ),
    }
}

fn parse_tools_override(raw: &str, available: &[String]) -> Result<Vec<String>> {
    let tools: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(std::string::ToString::to_string)
        .collect();

    if tools.is_empty() {
        anyhow::bail!("--tools requires a comma-separated list of tool names");
    }

    let available_set: std::collections::HashSet<String> =
        available.iter().map(|t| t.to_ascii_lowercase()).collect();
    let mut unknown: Vec<String> = tools
        .iter()
        .filter(|t| !available_set.contains(&t.to_ascii_lowercase()))
        .cloned()
        .collect();

    if !unknown.is_empty() {
        unknown.sort();
        let mut available_sorted = available.to_vec();
        available_sorted.sort();
        anyhow::bail!(
            "Unknown tool(s): {}. Available tools: {}",
            unknown.join(", "),
            available_sorted.join(", ")
        );
    }

    Ok(tools)
}

fn parse_event_filter(raw: &str) -> Result<Vec<String>> {
    let filters: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .collect();

    if filters.is_empty() {
        anyhow::bail!("--filter requires a comma-separated list of event names");
    }

    Ok(filters)
}
