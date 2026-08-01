//! Exec command handler.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_engine::config::{self, ThinkingLevel};
use zdx_engine::core::agent::{ToolConfig, ToolSelection};
use zdx_engine::core::context::PromptContextInclusion;
use zdx_engine::core::thread_persistence::ThreadPersistenceOptions;
use zdx_engine::subagents::{self, RuntimeSubagentSelection, SubagentDefinition};
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
    pub subagent: Option<&'a str>,
    pub activity_kind: Option<&'a str>,
    pub activity_parent_thread_id: Option<&'a str>,
    pub activity_subagent_name: Option<&'a str>,
}

pub async fn run(options: ExecRunOptions<'_>) -> Result<()> {
    let root_path = PathBuf::from(options.root);
    let thread = options
        .thread_opts
        .resolve(&root_path)
        .context("resolve thread")?;

    let subagent = resolve_subagent(&root_path, options.subagent)?;

    // Apply overrides if provided
    let config = {
        let mut c = options.config.clone();
        if let Some(model) = options.model_override {
            c.model = model.to_string();
        } else if let Some(model) = subagent.as_ref().and_then(|d| d.model.clone()) {
            c.model = model;
        }
        if let Some(timeout_secs) = options.tool_timeout_override {
            c.tool_timeout_secs = timeout_secs;
        }
        if let Some(thinking) = options.thinking_override {
            c.thinking_level = parse_thinking_level(thinking)?;
        } else if let Some(level) = subagent.as_ref().and_then(|d| d.thinking_level) {
            c.thinking_level = level;
        }
        c
    };

    let effective_system_prompt = match subagent.as_ref() {
        Some(definition) => Some(
            subagents::render_prompt(
                &config,
                &root_path,
                definition,
                &config.model,
                PromptContextInclusion::default(),
            )
            .with_context(|| format!("render subagent '{}'", definition.name))?,
        ),
        None => options
            .effective_system_prompt_override
            .map(std::string::ToString::to_string),
    };

    let subagent_tools = subagent
        .as_ref()
        .and_then(|definition| definition.tools.clone())
        .map(|tools| tools.join(","));
    let tools_override = options.tools_override.or(subagent_tools.as_deref());

    let tool_registry = ToolRegistry::builtins();
    let available_tool_names = tool_registry.tool_names();

    let exec_opts = modes::exec::ExecOptions {
        root: root_path,
        tool_config: ToolConfig::new(
            tool_registry,
            if options.no_tools {
                ToolSelection::Explicit(Vec::new())
            } else if let Some(raw) = tools_override {
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
        effective_system_prompt,
        no_system_prompt: options.no_system_prompt,
        activity_kind: options.activity_kind.map(std::string::ToString::to_string),
        activity_parent_thread_id: options
            .activity_parent_thread_id
            .map(std::string::ToString::to_string),
        activity_subagent_name: options
            .activity_subagent_name
            .or(subagent.as_ref().map(|definition| definition.name.as_str()))
            .map(std::string::ToString::to_string),
    };

    // Use streaming variant - response is printed incrementally, final newline added at end
    modes::exec::run_exec(options.prompt, &config, thread, &exec_opts)
        .await
        .context("execute prompt")?;

    Ok(())
}

/// Resolves an explicit `--subagent` name into a definition.
///
/// The reserved `task` alias resolves to default exec behavior, so it yields
/// `None` and leaves prompt/model/tool composition untouched.
fn resolve_subagent(
    root: &std::path::Path,
    requested: Option<&str>,
) -> Result<Option<SubagentDefinition>> {
    let Some(name) = requested else {
        return Ok(None);
    };

    match subagents::resolve_runtime_selection(root, Some(name))
        .with_context(|| format!("load subagent '{name}'"))?
    {
        RuntimeSubagentSelection::Default => Ok(None),
        RuntimeSubagentSelection::Named(definition) => Ok(Some(definition)),
    }
}

pub(super) fn parse_thinking_level(s: &str) -> Result<ThinkingLevel> {
    match s.to_lowercase().as_str() {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" | "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        "max" => Ok(ThinkingLevel::Max),
        _ => anyhow::bail!(
            "Invalid thinking level '{s}'. Valid options: off, low, medium, high, xhigh, max"
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
