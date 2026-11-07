//! Exec command handler.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_engine::config;
use zdx_engine::core::agent::{ToolConfig, ToolSelection};
use zdx_engine::core::context::{EffectivePrompt, PromptContextInclusion};
use zdx_engine::core::thread_persistence::ThreadPersistenceOptions;
use zdx_engine::skills::SkillScope;
use zdx_engine::subagents::{self, SubagentDefinition};
use zdx_engine::tools::ToolRegistry;

use crate::modes;

// Mirrors the `exec` clap flags one-to-one; grouping them into enums would
// only add indirection over what the CLI already exposes as separate switches.
#[allow(clippy::struct_excessive_bools)]
pub struct ExecRunOptions<'a> {
    pub root: &'a str,
    pub thread_opts: &'a ThreadPersistenceOptions,
    pub prompt: &'a str,
    pub config: &'a config::Config,
    pub model_override: Option<&'a str>,
    /// Prebuilt prompt (system prompt plus runtime-context snapshot). Ignored
    /// when `subagent` is set.
    pub effective_prompt_override: Option<&'a EffectivePrompt>,
    pub tool_timeout_override: Option<u32>,
    pub event_filter_override: Option<&'a str>,
    pub stream: bool,
    pub tools_override: Option<&'a str>,
    pub no_tools: bool,
    /// Forces the run off background execution. A `subagent` definition
    /// that does not opt in turns it off as well.
    pub no_background_commands: bool,
    pub no_system_prompt: bool,
    pub no_skills: bool,
    pub subagent: Option<&'a str>,
    /// Skills the `skill` and `skill_exec` tools may reach. `None` derives it
    /// from `subagent` (its declared skills, else none) or, without one, the
    /// configured catalog.
    pub skill_scope: Option<SkillScope>,
    pub activity_kind: Option<&'a str>,
    pub activity_parent_thread_id: Option<&'a str>,
    pub activity_subagent_name: Option<&'a str>,
}

pub async fn run(options: ExecRunOptions<'_>) -> Result<()> {
    if let Some(thread_id) = options.thread_opts.thread_id.as_deref() {
        zdx_engine::orchestration::ensure_direct_run_allowed(thread_id)?;
    }
    let root_path = PathBuf::from(options.root);
    let thread = options
        .thread_opts
        .resolve(&root_path)
        .context("resolve thread")?;

    let subagent = resolve_subagent(&root_path, options.subagent)?;

    // Apply overrides if provided
    let config = {
        let mut c = options.config.clone();
        if let Some(model) = subagent.as_ref().and_then(|d| d.model.as_deref()) {
            c.apply_model_spec(model);
        }
        if let Some(model) = subagent
            .as_ref()
            .and_then(|d| c.subagents.overrides.get(&d.name))
            .and_then(|over| over.model.as_deref())
        {
            let model = model.to_string();
            c.apply_model_spec(&model);
        }
        if let Some(model) = options.model_override {
            c.apply_model_spec(model);
        }
        if options.no_skills {
            c.skills.enabled = false;
        }
        c
    };

    let effective_prompt = match subagent.as_ref() {
        Some(definition) => Some(EffectivePrompt {
            prompt: Some(
                subagents::render_prompt(
                    &config,
                    &root_path,
                    definition,
                    &config.model,
                    PromptContextInclusion::default(),
                )
                .with_context(|| format!("render subagent '{}'", definition.name))?,
            ),
            ..EffectivePrompt::default()
        }),
        None => options.effective_prompt_override.cloned(),
    };

    let subagent_tools = subagent
        .as_ref()
        .and_then(|definition| definition.tools.clone())
        .map(|tools| tools.join(","));
    let tools_override = options.tools_override.or(subagent_tools.as_deref());

    let tool_registry = ToolRegistry::builtins();
    let available_tool_names = tool_registry.tool_names();
    let background_commands = !options.no_background_commands
        && subagent
            .as_ref()
            .is_none_or(|definition| definition.background_commands);

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
        )
        .with_background_commands(background_commands)
        .with_skill_scope(run_skill_scope(&options, subagent.as_ref())),
        event_filter: options
            .event_filter_override
            .map(parse_event_filter)
            .transpose()?
            .unwrap_or_default(),
        stream: options.stream,
        effective_prompt,
        no_system_prompt: options.no_system_prompt,
        activity_kind: options.activity_kind.map(std::string::ToString::to_string),
        activity_parent_thread_id: options
            .activity_parent_thread_id
            .map(std::string::ToString::to_string),
        activity_subagent_name: options
            .activity_subagent_name
            .or(subagent.as_ref().map(|definition| definition.name.as_str()))
            .map(std::string::ToString::to_string),
        tool_timeout: options
            .tool_timeout_override
            .map(|secs| std::time::Duration::from_secs(u64::from(secs))),
    };

    // Use streaming variant - response is printed incrementally, final newline added at end
    let result = modes::exec::run_exec(options.prompt, &config, thread, &exec_opts).await;

    // A foreground command that outran its bound was moved to the background
    // rather than killed; this process owns its lease, so it must wait for the
    // job to finish before exiting. Draining runs on every exit path, including
    // a failed turn, and must complete inside the runtime: dropping the runtime
    // would drop the task holding the lease and kill the job.
    zdx_engine::tools::background::drain_adopted_jobs().await;

    result.context("execute prompt")?;

    Ok(())
}

/// Skills the run's `skill` and `skill_exec` tools may reach: the caller's
/// explicit scope, else the subagent's declared skills (none when it declares
/// none, matching its prompt), else the configured catalog.
fn run_skill_scope(
    options: &ExecRunOptions<'_>,
    subagent: Option<&SubagentDefinition>,
) -> SkillScope {
    options
        .skill_scope
        .clone()
        .or_else(|| subagent.map(|definition| definition.skill_scope(false)))
        .unwrap_or_default()
}

/// Resolves an explicit `--subagent` name into a definition.
fn resolve_subagent(
    root: &std::path::Path,
    requested: Option<&str>,
) -> Result<Option<SubagentDefinition>> {
    let Some(name) = requested else {
        return Ok(None);
    };

    subagents::resolve_named(root, name, None)
        .with_context(|| format!("load subagent '{name}'"))
        .map(Some)
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
