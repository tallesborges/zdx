//! Tool system for agentic capabilities.
//!
//! This module provides a registry of tools that the agent can use,
//! along with schema definitions for the Anthropic API.

// Leaf tools re-exported from zdx-tools
pub use zdx_tools::{apply_patch, bash, edit, fetch_webpage, glob, grep, read, web_search, write};

// Engine-backed tools (need full ToolContext with config, threads, etc.)
pub mod ask_media;
pub mod background;
pub mod gh_api;
pub mod git;
pub mod list_models;
pub mod memory_search;
pub mod orchestrator;
pub mod read_thread;
pub mod skill;
pub mod subagent;
pub mod subagent_job;
pub mod telegram;
pub mod thread_search;
pub mod todo_write;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;
// Re-export path helpers and serde helpers from zdx-tools for backward compat
pub use zdx_tools::{
    ResolvedPath, ToolEnv, expand_env_vars, insert_file_path_fields, resolve_existing_path,
    resolve_existing_path_in, resolve_input_path, resolve_path_against_root,
};
use zdx_types::events::AgentEvent;
pub use zdx_types::{ToolDefinition, ToolResult, ToolResultBlock, ToolResultContent};

use crate::core::agent::EventSender;
use crate::core::events::ToolOutput;

/// Context for tool execution.
#[derive(Clone)]
pub struct ToolContext {
    /// Root directory for file operations.
    pub root: PathBuf,

    /// Current persisted thread id for this run, when available.
    pub current_thread_id: Option<String>,

    /// Optional timeout for tool execution.
    pub timeout: Option<Duration>,

    /// Cancellation for work owned by the current invocation.
    pub cancel_token: Option<CancellationToken>,

    /// Optional model override for tool subagents.
    pub model: Option<String>,

    /// Optional model override for `read_thread` subagent.
    pub read_thread_model: Option<String>,

    /// Optional thinking level for tool subagents.
    pub thinking_level: Option<crate::config::ThinkingLevel>,

    /// Full config snapshot for advanced tool behaviors.
    pub config: Option<crate::config::Config>,

    /// Whether subagent delegation is enabled.
    pub subagents_enabled: bool,

    /// Available model list for subagent delegation.
    pub subagent_available_models: Vec<String>,

    /// Restricts which subagents `invoke_subagent` may reach this turn.
    /// `None` is unrestricted; `Some(list)` is enforced at execute time.
    pub allowed_subagents: Option<Vec<String>>,

    /// Event sender for emitting streaming tool output events.
    /// Set by the engine before tool execution; used by `bash_handler`
    /// to bridge output chunks to `ToolOutputDelta` events.
    pub event_sender: Option<EventSender>,

    /// Tool use ID for the current execution (needed for `ToolOutputDelta` events).
    pub tool_use_id: Option<String>,

    /// Whether this run may run work in the background. Resolved once per run
    /// by [`BackgroundExecution::resolve`]; runtime enforcement reads it here.
    pub background_execution: BackgroundExecution,

    /// Background work owned by the agent turn running this tool call. Set
    /// for every engine turn; work registered here is stopped when the turn
    /// ends. `None` outside an engine turn, where nothing may be started that
    /// needs an owner.
    pub turn_tasks: Option<Arc<subagent_job::TurnTasks>>,

    /// Skills the `skill` and `skill_exec` tools may reach in this run.
    pub skill_scope: crate::skills::SkillScope,

    /// The run's own environment (`ZDX_HOME`, `ZDX_ARTIFACT_DIR`,
    /// `ZDX_THREAD_ID`, `ZDX_MEMORY_ROOT`), set per turn from its config and
    /// thread. Every child process a tool starts gets it, and `$VAR` in tool
    /// paths reads it before the process environment. Empty outside an
    /// engine turn, where only the process environment applies.
    pub env: ToolEnv,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("root", &self.root)
            .field("current_thread_id", &self.current_thread_id)
            .field("timeout", &self.timeout)
            .field("cancel_token", &self.cancel_token.as_ref().map(|_| ".."))
            .field("model", &self.model)
            .field("read_thread_model", &self.read_thread_model)
            .field("thinking_level", &self.thinking_level)
            .field("config", &self.config.is_some())
            .field("subagents_enabled", &self.subagents_enabled)
            .field("subagent_available_models", &self.subagent_available_models)
            .field("allowed_subagents", &self.allowed_subagents)
            .field("event_sender", &self.event_sender.as_ref().map(|_| ".."))
            .field("tool_use_id", &self.tool_use_id)
            .field("background_execution", &self.background_execution)
            .field("turn_tasks", &self.turn_tasks.is_some())
            .field("skill_scope", &self.skill_scope)
            .field("env", &self.env)
            .finish()
    }
}

/// Names of the tools that read and stop background jobs. They are present
/// exactly when a run has background execution and a tool that can start
/// background work ([`BACKGROUND_STARTING_TOOLS`]).
pub const BACKGROUND_MANAGEMENT_TOOLS: [&str; 2] = ["background_output", "background_kill"];

/// Tools that can start background work: `bash` (`background: true` and the
/// auto-background handoff) and `invoke_subagent` (`background: true`).
pub const BACKGROUND_STARTING_TOOLS: [&str; 2] = ["bash", "invoke_subagent"];

/// Whether a run may run work in the background: the single policy that the
/// tool list, the `Bash` and `invoke_subagent` schemas and text, and runtime
/// enforcement derive from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundExecution {
    /// Background execution is available. `Bash` accepts `background: true`,
    /// `invoke_subagent` accepts `background: true`, and
    /// `background_output`/`background_kill` manage the jobs.
    ///
    /// `handoff_bound` is when a foreground `Bash` command that is still
    /// running moves to the background. `None` means a foreground command is
    /// waited on however long it runs: the surface cannot keep a job alive,
    /// the bound is configured as 0, or the platform has no handoff.
    Enabled { handoff_bound: Option<Duration> },
    /// No background execution. `background: true` is rejected by both tools,
    /// the management tools are absent, nothing is ever moved to the
    /// background, and a normal `invoke_subagent` call blocks. A `Bash`
    /// command stays in the foreground until it finishes; there is no implicit
    /// time limit, only the caller's `timeout_secs` and the run's deadline.
    Disabled,
}

impl BackgroundExecution {
    /// Resolves the policy for one run.
    ///
    /// `background_commands` is the run's explicit permission (a subagent
    /// definition's `background_commands`, default off for subagents and on
    /// everywhere else). Without it the run is [`Self::Disabled`], whatever
    /// the surface. With it, the `Bash` handoff applies only where the surface
    /// can keep a relocated job alive (see [`surface_keeps_background_jobs`]).
    ///
    /// The handoff bound is only ever a relocation bound: nothing in this
    /// policy kills a command.
    #[must_use]
    pub fn resolve(
        background_commands: bool,
        surface: Option<&str>,
        config: &crate::config::Config,
    ) -> Self {
        if !background_commands {
            return Self::Disabled;
        }
        let handoff_bound = (cfg!(unix) && surface_keeps_background_jobs(surface))
            .then(|| config.bash_foreground_bound())
            .flatten();
        Self::Enabled { handoff_bound }
    }

    #[must_use]
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled { .. })
    }
}

/// Whether a run on `surface` can keep an adopted background job alive past
/// the end of the run.
///
/// An adopted job is held by its owning process's supervisor lease, so the
/// process must not exit while the job is still running. A surface qualifies in
/// one of two ways:
///
/// - it outlives the run: the interactive TUI (`chat`), the bot service
///   (`telegram`), and the engine service (`orchestrator`, which also
///   runs Telegram-routed turns as `telegram`) keep running and can poll or
///   kill the job afterwards;
/// - it drains before exiting: `zdx exec` — which is also how orchestrator
///   workers and the `invoke_subagent` children that opt into background
///   commands run — waits for adopted jobs to finish at the end of the
///   command, holding the lease throughout. See `zdx_tools::adopted::drain`.
///
/// Unknown and absent surfaces are excluded: an embedder that neither outlives
/// its runs nor drains would kill relocated work at exit, so the failure
/// direction stays "wait in the foreground like before".
#[must_use]
pub fn surface_keeps_background_jobs(surface: Option<&str>) -> bool {
    matches!(surface, Some("chat" | "telegram" | "exec" | "orchestrator"))
}

impl ToolContext {
    pub fn new(root: PathBuf, timeout: Option<Duration>) -> Self {
        Self {
            root,
            current_thread_id: None,
            timeout,
            cancel_token: None,
            model: None,
            read_thread_model: None,
            thinking_level: None,
            config: None,
            subagents_enabled: true,
            subagent_available_models: Vec::new(),
            allowed_subagents: None,
            event_sender: None,
            tool_use_id: None,
            background_execution: BackgroundExecution::Enabled {
                handoff_bound: None,
            },
            turn_tasks: None,
            skill_scope: crate::skills::SkillScope::default(),
            env: ToolEnv::default(),
        }
    }

    /// Sets the run's own environment; see [`ToolContext::env`].
    #[must_use]
    pub fn with_env(mut self, env: ToolEnv) -> Self {
        self.env = env;
        self
    }

    /// [`resolve_existing_path`] with the run's environment.
    ///
    /// # Errors
    /// Returns an error if the path is empty or does not exist.
    pub fn resolve_existing_path(&self, path: &str) -> Result<ResolvedPath, ToolOutput> {
        resolve_existing_path_in(path, &self.root, &self.env)
    }

    #[must_use]
    pub fn with_config(mut self, config: &crate::config::Config) -> Self {
        self.config = Some(config.clone());
        self.model = Some(config.model.clone());
        self.read_thread_model = Some(config.read_thread_model.clone());
        self.thinking_level = Some(config.thinking_level);
        self.subagents_enabled = config.subagents.enabled;
        self.subagent_available_models = config.subagent_available_models();
        self
    }

    #[must_use]
    pub fn with_allowed_subagents(mut self, allowed: Option<Vec<String>>) -> Self {
        self.allowed_subagents = allowed;
        self
    }

    /// Sets the run's resolved `Bash` execution policy.
    #[must_use]
    pub fn with_background_execution(mut self, background_execution: BackgroundExecution) -> Self {
        self.background_execution = background_execution;
        self
    }

    #[must_use]
    pub fn with_skill_scope(mut self, scope: crate::skills::SkillScope) -> Self {
        self.skill_scope = scope;
        self
    }

    #[must_use]
    pub fn with_current_thread_id(mut self, thread_id: Option<&str>) -> Self {
        self.current_thread_id = thread_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        self
    }

    /// Convert to a leaf tool context (for zdx-tools).
    #[must_use]
    pub fn as_leaf(&self) -> zdx_tools::ToolContext {
        let mut ctx =
            zdx_tools::ToolContext::new(self.root.clone(), self.timeout).with_env(self.env.clone());
        ctx.cancel_token.clone_from(&self.cancel_token);
        ctx
    }
}

/// Named tool sets for common configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSet {
    Default,
    OpenAICodex,
}

impl ToolSet {
    pub fn tool_names(self) -> &'static [&'static str] {
        match self {
            ToolSet::Default => &[
                "bash",
                "background_output",
                "background_kill",
                "ask_media",
                "edit",
                "fetch_webpage",
                "gh_api",
                "git",
                "glob",
                "grep",
                "invoke_subagent",
                "memory_search",
                "read",
                "read_thread",
                "telegram",
                "todo_write",
                "thread_search",
                "web_search",
                "write",
            ],
            ToolSet::OpenAICodex => &[
                "bash",
                "background_output",
                "background_kill",
                "apply_patch",
                "ask_media",
                "fetch_webpage",
                "gh_api",
                "git",
                "glob",
                "grep",
                "invoke_subagent",
                "memory_search",
                "read",
                "read_thread",
                "telegram",
                "todo_write",
                "thread_search",
                "web_search",
            ],
        }
    }
}

pub fn toolset_tool_names(set: ToolSet) -> Vec<String> {
    set.tool_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

/// Async tool handler function.
pub type ToolFuture = Pin<Box<dyn Future<Output = ToolOutput> + Send>>;

/// Trait for agent tools.
///
/// Each tool provides its schema definition and an async execute method.
/// The registry stores `Arc<dyn Tool>` for cheap cloning and heterogeneous
/// dispatch.
pub trait Tool: Send + Sync {
    /// Returns the schema definition for this tool.
    fn definition(&self) -> ToolDefinition;

    /// Executes the tool with the given JSON input and context.
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture;
}

/// Tool registry (definitions + executors).
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools_len", &self.tools.len())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn builtins() -> Self {
        let mut registry = Self::new();
        registry.register_builtin_tools();
        registry
    }

    /// Registers a concrete [`Tool`] implementation.
    pub fn register_tool<T: Tool + 'static>(&mut self, tool: T) {
        self.register_boxed(Arc::new(tool));
    }

    /// Registers a boxed trait object directly.
    pub fn register_boxed(&mut self, tool: Arc<dyn Tool>) {
        // Remove any existing tool with the same name (case-insensitive).
        let name = tool.definition().name.to_ascii_lowercase();
        self.tools
            .retain(|t| t.definition().name.to_ascii_lowercase() != name);
        self.tools.push(tool);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|t| t.definition()).collect()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|t| t.definition().name.to_lowercase())
            .collect()
    }

    pub fn tools_from_names<'a, I>(&self, names: I) -> Vec<ToolDefinition>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let include_set: std::collections::HashSet<_> = names
            .into_iter()
            .map(|name| name.trim().to_lowercase())
            .filter(|name| !name.is_empty())
            .collect();

        self.tools
            .iter()
            .map(|t| t.definition())
            .filter(|t| include_set.contains(&t.name.to_lowercase()))
            .collect()
    }

    pub fn tools_for_set(&self, tool_set: ToolSet) -> Vec<ToolDefinition> {
        self.tools_from_names(tool_set.tool_names().iter().copied())
    }

    pub fn tools_for_provider(
        &self,
        provider_config: &crate::config::ProviderConfig,
    ) -> Vec<ToolDefinition> {
        let all_names = self.tool_names();
        let all_names_refs: Vec<&str> = all_names.iter().map(std::string::String::as_str).collect();
        let enabled_names = provider_config.filter_tools(&all_names_refs);
        self.tools_from_names(enabled_names)
    }

    /// # Errors
    /// Returns an error if the operation fails.
    #[tracing::instrument(
        name = "tool",
        skip_all,
        fields(tool = %name, tool_use_id = %tool_use_id)
    )]
    pub async fn execute_tool<S>(
        &self,
        name: &str,
        tool_use_id: &str,
        input: &Value,
        ctx: &ToolContext,
        enabled_tools: &std::collections::HashSet<String, S>,
    ) -> (ToolOutput, ToolResult, Option<u64>)
    where
        S: std::hash::BuildHasher,
    {
        let started_at = std::time::Instant::now();
        let name_lower = name.to_ascii_lowercase();
        let is_enabled = enabled_tools
            .iter()
            .any(|t| t.to_ascii_lowercase() == name_lower);

        if !is_enabled {
            tracing::warn!("Tool call rejected: not enabled for this run");
            let output = unknown_tool_output(name, enabled_tools);
            let result = ToolResult::from_output(tool_use_id.to_string(), &output);
            return (output, result, None);
        }

        let output = if let Some(tool) = self
            .tools
            .iter()
            .find(|t| t.definition().name.eq_ignore_ascii_case(&name_lower))
        {
            tool.execute(input, ctx).await
        } else {
            tracing::warn!("Tool call rejected: unknown tool");
            unknown_tool_output(name, enabled_tools)
        };

        let duration_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        match &output {
            ToolOutput::Failure { .. } => {
                let (code, message, _) = output.error_info().unwrap_or(("unknown", "", None));
                tracing::warn!(
                    duration_ms,
                    code,
                    error = %zdx_types::log_field(message, 300),
                    "Tool failed"
                );
            }
            ToolOutput::Canceled { .. } => tracing::debug!(duration_ms, "Tool canceled"),
            ToolOutput::Success { .. } => tracing::debug!(duration_ms, "Tool completed"),
        }

        let result = ToolResult::from_output(tool_use_id.to_string(), &output);
        (output, result, Some(duration_ms))
    }

    fn register_builtin_tools(&mut self) {
        self.register_tool(Bash);
        self.register_tool(background::BackgroundOutput);
        self.register_tool(background::BackgroundKill);
        self.register_tool(ApplyPatch);
        self.register_tool(Edit);
        self.register_tool(Read);
        self.register_tool(AskMedia);
        self.register_tool(GhApi);
        self.register_tool(Git);
        self.register_tool(ListModels);
        self.register_tool(MemorySearch);
        self.register_tool(ReadThread);
        self.register_tool(SkillTool);
        self.register_tool(SkillExec);
        self.register_tool(Telegram);
        self.register_tool(TodoWrite);
        self.register_tool(ThreadSearch);
        self.register_tool(Subagent);
        self.register_tool(Write);
        self.register_tool(WebSearch);
        self.register_tool(FetchWebpage);
        self.register_tool(Grep);
        self.register_tool(Glob);
        // Orchestrator thread controls: unbound stubs so schemas and tool-name
        // validation exist everywhere; the engine service rebinds them
        // to its live WorkerManager via `register_boxed`.
        for tool in orchestrator::OrchestratorTool::stubs() {
            self.register_tool(tool);
        }
    }
}

// -- Builtin tool implementations --

/// Reads the bash `background` flag, accepting a real JSON boolean or a
/// boolean-like string. Models sometimes emit `"background": "true"`, which
/// `Value::as_bool` rejects — that silently ran long-lived servers on the
/// blocking path until the turn timed out. Vocabulary mirrors
/// `zdx_tools::bool_or_string`.
pub(crate) fn wants_background(input: &Value) -> bool {
    match input.get("background") {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(raw)) => matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "y" | "on"
        ),
        _ => false,
    }
}

struct Bash;
impl Tool for Bash {
    fn definition(&self) -> ToolDefinition {
        bash::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move {
            let background = ctx.background_execution.is_enabled();
            if wants_background(&input) {
                if !background {
                    return background_disabled_output();
                }
                // Background mode: spawn a detached, tracked process and
                // return a bg_id instead of blocking the turn.
                return background::run_background(&input, &ctx).await;
            }

            // With background execution, a foreground command is
            // auto-backgrounded rather than killed when it outruns its bound;
            // this supplies the registry hooks needed if that happens.
            #[cfg(unix)]
            let handoff = input
                .get("command")
                .and_then(Value::as_str)
                .and_then(|command| background::prepare_handoff(command, &ctx));

            let leaf = ctx.as_leaf();
            let timeout = ctx.timeout;
            let run = move |output_tx| async move {
                if background {
                    bash::execute(
                        &input,
                        &leaf,
                        timeout,
                        output_tx,
                        #[cfg(unix)]
                        handoff,
                    )
                    .await
                } else {
                    bash::execute_without_background(&input, &leaf, timeout, output_tx).await
                }
            };

            run_with_output_deltas(&ctx, run).await
        })
    }
}

/// Runs `run`, forwarding the output lines it sends to the agent as
/// `ToolOutputDelta` events when the engine provided a sender for this call.
pub(crate) async fn run_with_output_deltas<F, Fut>(ctx: &ToolContext, run: F) -> ToolOutput
where
    F: FnOnce(Option<tokio::sync::mpsc::UnboundedSender<String>>) -> Fut,
    Fut: Future<Output = ToolOutput>,
{
    let (Some(sender), Some(id)) = (ctx.event_sender.clone(), ctx.tool_use_id.clone()) else {
        return run(None).await;
    };
    let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::join!(run(Some(output_tx)), async {
        while let Some(chunk) = output_rx.recv().await {
            sender.send(AgentEvent::ToolOutputDelta {
                id: id.clone(),
                chunk,
            });
        }
    })
    .0
}

/// The result of `background: true` in a run without background execution.
fn background_disabled_output() -> ToolOutput {
    ToolOutput::failure(
        "background_disabled",
        "background: true is not available: this run has no background execution. Run the \
         command in the foreground instead; this turn waits until it finishes, so do not start \
         servers, watchers, or other processes that never exit, and set timeout_secs on a \
         command that might hang.",
        None,
    )
}

struct SkillTool;
impl Tool for SkillTool {
    fn definition(&self) -> ToolDefinition {
        skill::skill_definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { skill::execute_skill(&input, &ctx).await })
    }
}

struct SkillExec;
impl Tool for SkillExec {
    fn definition(&self) -> ToolDefinition {
        skill::skill_exec_definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { skill::execute_skill_exec(&input, &ctx).await })
    }
}

struct ApplyPatch;
impl Tool for ApplyPatch {
    fn definition(&self) -> ToolDefinition {
        apply_patch::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_apply_patch(&input, &ctx.as_leaf()).await })
    }
}

struct Edit;
impl Tool for Edit {
    fn definition(&self) -> ToolDefinition {
        edit::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_edit(&input, &ctx.as_leaf()).await })
    }
}

struct Read;
impl Tool for Read {
    fn definition(&self) -> ToolDefinition {
        read::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_read(&input, &ctx.as_leaf()).await })
    }
}

struct Write;
impl Tool for Write {
    fn definition(&self) -> ToolDefinition {
        write::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_write(&input, &ctx.as_leaf()).await })
    }
}

struct WebSearch;
impl Tool for WebSearch {
    fn definition(&self) -> ToolDefinition {
        web_search::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { web_search::execute(&input, &ctx.as_leaf()).await })
    }
}

struct FetchWebpage;
impl Tool for FetchWebpage {
    fn definition(&self) -> ToolDefinition {
        fetch_webpage::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { fetch_webpage::execute(&input, &ctx.as_leaf()).await })
    }
}

struct Grep;
impl Tool for Grep {
    fn definition(&self) -> ToolDefinition {
        grep::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_grep(&input, &ctx.as_leaf()).await })
    }
}

struct Glob;
impl Tool for Glob {
    fn definition(&self) -> ToolDefinition {
        glob::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_glob(&input, &ctx.as_leaf()).await })
    }
}

struct GhApi;
impl Tool for GhApi {
    fn definition(&self) -> ToolDefinition {
        gh_api::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { gh_api::execute(&input, &ctx).await })
    }
}

struct Git;
impl Tool for Git {
    fn definition(&self) -> ToolDefinition {
        git::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { git::execute(&input, &ctx).await })
    }
}

struct ListModels;
impl Tool for ListModels {
    fn definition(&self) -> ToolDefinition {
        list_models::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { list_models::execute(&input, &ctx) })
    }
}

struct AskMedia;
impl Tool for AskMedia {
    fn definition(&self) -> ToolDefinition {
        ask_media::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { ask_media::execute(&input, &ctx).await })
    }
}

struct Telegram;
impl Tool for Telegram {
    fn definition(&self) -> ToolDefinition {
        telegram::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { telegram::execute(&input, &ctx).await })
    }
}

struct ReadThread;
impl Tool for ReadThread {
    fn definition(&self) -> ToolDefinition {
        read_thread::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { read_thread::execute(&input, &ctx).await })
    }
}

struct MemorySearch;
impl Tool for MemorySearch {
    fn definition(&self) -> ToolDefinition {
        memory_search::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_memory_search(&input, &ctx).await })
    }
}

struct TodoWrite;
impl Tool for TodoWrite {
    fn definition(&self) -> ToolDefinition {
        todo_write::definition()
    }
    fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        Box::pin(async move { todo_write::execute(&input) })
    }
}

struct ThreadSearch;
impl Tool for ThreadSearch {
    fn definition(&self) -> ToolDefinition {
        thread_search::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { execute_thread_search(&input, &ctx).await })
    }
}

struct Subagent;
impl Tool for Subagent {
    fn definition(&self) -> ToolDefinition {
        subagent::definition()
    }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move { subagent::execute(&input, &ctx).await })
    }
}

fn unknown_tool_output<S>(
    name: &str,
    enabled_tools: &std::collections::HashSet<String, S>,
) -> ToolOutput
where
    S: std::hash::BuildHasher,
{
    let mut available: Vec<_> = enabled_tools.iter().cloned().collect();
    available.sort();
    ToolOutput::failure_with_details(
        "unknown_tool",
        format!("Unknown tool: {name}"),
        format!("Available tools: {}", available.join(", ")),
    )
}

/// Returns all available tool definitions.
pub fn all_tools() -> Vec<ToolDefinition> {
    ToolRegistry::builtins().definitions()
}

/// Returns all tool names (lowercase), derived from `all_tools()` to stay in sync.
pub fn all_tool_names() -> Vec<String> {
    ToolRegistry::builtins().tool_names()
}

/// Returns tool definitions filtered by provider configuration.
///
/// Uses `ProviderConfig::filter_tools()` to determine which tools to include.
pub fn tools_for_provider(provider_config: &crate::config::ProviderConfig) -> Vec<ToolDefinition> {
    ToolRegistry::builtins().tools_for_provider(provider_config)
}

/// Executes a tool by name with the given input.
/// Returns the structured `ToolOutput` (envelope format).
///
/// Validates that the tool is in the enabled set before execution.
/// If the tool is unknown or not enabled, returns an error with the
/// list of actually enabled tools (shown in canonical casing).
///
/// Tool names are matched case-insensitively, making the API resilient
/// to provider casing differences.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn execute_tool<S>(
    name: &str,
    tool_use_id: &str,
    input: &Value,
    ctx: &ToolContext,
    enabled_tools: &std::collections::HashSet<String, S>,
) -> (ToolOutput, ToolResult)
where
    S: std::hash::BuildHasher,
{
    let (output, result, _) = ToolRegistry::builtins()
        .execute_tool(name, tool_use_id, input, ctx, enabled_tools)
        .await;
    (output, result)
}

// -- Blocking wrappers for leaf tools --

async fn execute_edit(input: &Value, ctx: &zdx_tools::ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || edit::execute(&input, &ctx)
    })
    .await
}

async fn execute_apply_patch(input: &Value, ctx: &zdx_tools::ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || apply_patch::execute(&input, &ctx)
    })
    .await
}

async fn execute_read(input: &Value, ctx: &zdx_tools::ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || read::execute(&input, &ctx)
    })
    .await
}

async fn execute_write(input: &Value, ctx: &zdx_tools::ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || write::execute(&input, &ctx)
    })
    .await
}

// -- Blocking wrappers for engine tools --

async fn execute_thread_search(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || thread_search::execute(&input, &ctx)
    })
    .await
}

async fn execute_memory_search(input: &Value, ctx: &ToolContext) -> ToolOutput {
    memory_search::execute(input, ctx).await
}

async fn execute_grep(input: &Value, ctx: &zdx_tools::ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || grep::execute(&input, &ctx)
    })
    .await
}

async fn execute_glob(input: &Value, ctx: &zdx_tools::ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || glob::execute(&input, &ctx)
    })
    .await
}

/// Execute a blocking tool function with optional timeout.
async fn execute_blocking<F>(timeout: Option<Duration>, f: F) -> ToolOutput
where
    F: FnOnce() -> ToolOutput + Send + 'static,
{
    // `spawn_blocking` does not inherit the current span, so enter it inside
    // the blocking closure or the tool's own log lines lose their turn/thread
    // context (same hazard as `spawn` in the concurrent tool path).
    let span = tracing::Span::current();
    let mut handle = tokio::task::spawn_blocking(move || {
        let _entered = span.enter();
        f()
    });

    match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, &mut handle).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => ToolOutput::failure(
                "panic",
                "Tool execution panicked",
                Some("The tool task panicked during execution".to_string()),
            ),
            Err(_) => {
                handle.abort();
                ToolOutput::failure(
                    "timeout",
                    format!(
                        "Tool execution timed out after {} seconds",
                        timeout.as_secs()
                    ),
                    Some("Consider breaking up large tasks or increasing the timeout".to_string()),
                )
            }
        },
        None => match handle.await {
            Ok(output) => output,
            Err(_) => ToolOutput::failure(
                "panic",
                "Tool execution panicked",
                Some("The tool task panicked or was cancelled".to_string()),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    /// Helper to create `enabled_tools` set with all tools (canonical names)
    fn all_enabled_tools() -> std::collections::HashSet<String> {
        all_tools().into_iter().map(|t| t.name).collect()
    }

    /// `background` must be honored as a real bool *and* as a boolean-like
    /// string; models emit `"true"`, which previously fell through to the
    /// blocking path and timed out for long-lived servers.
    #[test]
    fn wants_background_accepts_bool_and_stringy_values() {
        for truthy in [json!(true), json!("true"), json!("True"), json!(" yes ")] {
            assert!(
                wants_background(&json!({ "command": "x", "background": truthy })),
                "expected truthy for {truthy:?}"
            );
        }
        for falsy in [json!(false), json!("false"), json!("0"), json!("")] {
            assert!(
                !wants_background(&json!({ "command": "x", "background": falsy })),
                "expected falsy for {falsy:?}"
            );
        }
        // Absent or unparseable values stay on the blocking path.
        assert!(!wants_background(&json!({ "command": "x" })));
        assert!(!wants_background(
            &json!({ "command": "x", "background": "maybe" })
        ));
    }

    /// Without explicit permission a run has no background execution on any
    /// surface, whatever the bound; the bound is never a kill limit. With
    /// permission, the handoff applies only where the surface can keep a job
    /// alive.
    #[test]
    fn bash_execution_resolves_from_permission_surface_and_bound() {
        let bound = |secs| crate::config::Config {
            bash_foreground_bound_secs: secs,
            ..crate::config::Config::default()
        };

        for surface in [Some("exec"), Some("chat"), Some("telegram"), None] {
            for secs in [0, 7] {
                assert_eq!(
                    BackgroundExecution::resolve(false, surface, &bound(secs)),
                    BackgroundExecution::Disabled
                );
            }
        }

        let handoff = if cfg!(unix) {
            Some(Duration::from_secs(7))
        } else {
            None
        };
        assert_eq!(
            BackgroundExecution::resolve(true, Some("exec"), &bound(7)),
            BackgroundExecution::Enabled {
                handoff_bound: handoff
            }
        );
        assert_eq!(
            BackgroundExecution::resolve(true, Some("unknown-surface"), &bound(7)),
            BackgroundExecution::Enabled {
                handoff_bound: None
            }
        );
        assert_eq!(
            BackgroundExecution::resolve(true, Some("exec"), &bound(0)),
            BackgroundExecution::Enabled {
                handoff_bound: None
            }
        );
    }

    /// `background: true` is refused in a run without background execution,
    /// and nothing is spawned or registered.
    #[tokio::test]
    async fn bash_rejects_explicit_background_without_background_execution() {
        let home = crate::test_support::temp_zdx_home();
        let marker = home.path().join("must-not-exist");
        let ctx = ToolContext::new(home.path().to_path_buf(), None)
            .with_background_execution(BackgroundExecution::Disabled);
        let input = json!({
            "command": format!("touch {}", marker.display()),
            "background": true,
        });

        let output = Bash.execute(&input, &ctx).await;
        let (code, message, _) = output.error_info().expect("must fail");
        assert_eq!(code, "background_disabled");
        assert!(message.contains("in the foreground"), "{message}");
        assert!(!message.contains("seconds"), "no implicit limit: {message}");
        assert!(!message.contains("background_output"), "{message}");

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!marker.exists(), "the command must not run");
        assert!(crate::background_activity::list_background().is_empty());
    }

    /// Without background execution a foreground command outlives a short
    /// configured handoff bound: it is neither killed nor relocated.
    #[tokio::test]
    async fn bash_without_background_execution_outlives_the_bound() {
        let home = crate::test_support::temp_zdx_home();
        let ctx = ToolContext::new(home.path().to_path_buf(), None)
            .with_config(&crate::config::Config {
                bash_foreground_bound_secs: 1,
                ..crate::config::Config::default()
            })
            .with_background_execution(BackgroundExecution::Disabled);

        let output = Bash
            .execute(
                &json!({ "command": "echo started; sleep 2; echo finished" }),
                &ctx,
            )
            .await;
        let data = output.data().expect("success envelope");
        assert_eq!(data["timed_out"], false);
        assert_eq!(data["exit_code"], 0);
        assert!(data.get("backgrounded").is_none());
        assert!(data["stdout"].as_str().unwrap().contains("finished"));
        assert!(crate::background_activity::list_background().is_empty());
    }

    #[test]
    fn test_resolve_input_path_expands_env_vars() {
        let home = std::env::var("HOME").expect("HOME must be set for tests");
        let temp = TempDir::new_in(&home).unwrap();
        let root = TempDir::new().unwrap();

        let relative_to_home = temp.path().strip_prefix(&home).unwrap();
        let requested = format!("$HOME/{}/nested.txt", relative_to_home.display());

        let resolved = resolve_input_path(&requested, root.path()).unwrap();
        assert_eq!(resolved, temp.path().join("nested.txt"));
    }

    #[test]
    fn test_resolve_input_path_expands_leading_tilde() {
        let home = std::env::var("HOME").expect("HOME must be set for tests");
        let temp = TempDir::new_in(&home).unwrap();
        let root = TempDir::new().unwrap();

        let relative_to_home = temp.path().strip_prefix(&home).unwrap();
        let requested = format!("~/{}/nested.txt", relative_to_home.display());

        let resolved = resolve_input_path(&requested, root.path()).unwrap();
        assert_eq!(resolved, temp.path().join("nested.txt"));
    }

    #[test]
    fn test_resolve_input_path_expands_bare_tilde() {
        let home = std::env::var("HOME").expect("HOME must be set for tests");
        let root = TempDir::new().unwrap();

        let resolved = resolve_input_path("~", root.path()).unwrap();
        assert_eq!(resolved, std::path::PathBuf::from(home));
    }

    #[tokio::test]
    async fn test_execute_tool_times_out() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf(), Some(Duration::from_secs(1)));
        let enabled = all_enabled_tools();
        let input = json!({"command": "sleep 2"});

        let (output, result) = execute_tool("bash", "toolu_timeout", &input, &ctx, &enabled).await;
        // Timeout is still a success envelope with timed_out=true
        assert!(output.is_ok());
        assert!(
            result
                .content
                .as_text()
                .unwrap()
                .contains(r#""timed_out":true"#)
        );
    }

    #[tokio::test]
    async fn test_execute_tool_respects_enabled_tools() {
        let temp = TempDir::new().unwrap();
        // Only enable Bash and Read (canonical names) - NOT Apply_Patch
        let enabled: std::collections::HashSet<String> =
            vec!["Bash".to_string(), "Read".to_string()]
                .into_iter()
                .collect();
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let input = json!({});

        // Try to call apply_patch which is not enabled (lowercase, as model might return)
        let (output, result) =
            execute_tool("apply_patch", "toolu_test", &input, &ctx, &enabled).await;

        // Should fail as unknown_tool
        assert!(!output.is_ok());
        assert!(result.is_error);

        let content = result.content.as_text().unwrap();
        assert!(content.contains(r#""code":"unknown_tool""#));
        // Error message mentions the unknown tool (preserves original casing from caller)
        assert!(content.contains("Unknown tool: apply_patch"));
        // Available tools should list canonical names (PascalCase)
        assert!(content.contains("Available tools: Bash, Read"));
        // Should NOT include tools that weren't enabled
        assert!(!content.contains("Edit"));
        assert!(!content.contains("Write"));
    }

    #[tokio::test]
    async fn test_execute_tool_case_insensitive() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let enabled = all_enabled_tools();
        let input = json!({"file_path": "test.txt"});

        // Call with PascalCase (as Anthropic might return)
        let (output, _) = execute_tool("Read", "toolu_test", &input, &ctx, &enabled).await;
        assert!(output.is_ok());

        // Call with lowercase
        let (output, _) = execute_tool("read", "toolu_test", &input, &ctx, &enabled).await;
        assert!(output.is_ok());

        // Call with UPPERCASE
        let (output, _) = execute_tool("READ", "toolu_test", &input, &ctx, &enabled).await;
        assert!(output.is_ok());
    }

    #[test]
    fn test_all_tool_names_derived_from_definitions() {
        let names = all_tool_names();
        let tools = all_tools();

        // Verify names are derived from definitions (same count)
        assert_eq!(names.len(), tools.len());

        // Verify all expected tools are present
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"apply_patch".to_string()));
        assert!(names.contains(&"ask_media".to_string()));
        assert!(names.contains(&"gh_api".to_string()));
        assert!(names.contains(&"git".to_string()));
        assert!(names.contains(&"list_models".to_string()));
        assert!(names.contains(&"telegram".to_string()));
        assert!(names.contains(&"edit".to_string()));
        assert!(names.contains(&"fetch_webpage".to_string()));
        assert!(names.contains(&"invoke_subagent".to_string()));
        assert!(names.contains(&"memory_search".to_string()));
        assert!(names.contains(&"read".to_string()));
        assert!(names.contains(&"read_thread".to_string()));
        assert!(names.contains(&"todo_write".to_string()));
        assert!(names.contains(&"thread_search".to_string()));
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"write".to_string()));
    }

    #[test]
    fn test_tools_for_provider_no_filtering() {
        let config = crate::config::ProviderConfig::default();
        let tools = tools_for_provider(&config);

        let names: Vec<_> = tools.iter().map(|t| t.name.to_lowercase()).collect();
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"apply_patch".to_string()));
        assert!(names.contains(&"ask_media".to_string()));
        assert!(names.contains(&"gh_api".to_string()));
        assert!(names.contains(&"git".to_string()));
        assert!(names.contains(&"telegram".to_string()));
        assert!(names.contains(&"edit".to_string()));
        assert!(names.contains(&"fetch_webpage".to_string()));
        assert!(names.contains(&"invoke_subagent".to_string()));
        assert!(names.contains(&"memory_search".to_string()));
        assert!(names.contains(&"read".to_string()));
        assert!(names.contains(&"read_thread".to_string()));
        assert!(names.contains(&"todo_write".to_string()));
        assert!(names.contains(&"thread_search".to_string()));
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"write".to_string()));
    }

    #[test]
    fn test_tools_for_provider_with_filter() {
        let config = crate::config::ProviderConfig {
            tools: Some(vec!["bash".to_string(), "read".to_string()]),
            ..Default::default()
        };
        let tools = tools_for_provider(&config);

        let names: Vec<_> = tools.iter().map(|t| t.name.to_lowercase()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"read".to_string()));
        assert!(!names.contains(&"apply_patch".to_string()));
    }

    #[test]
    fn test_read_bundled_skill_via_env_var_path() {
        let _temp = crate::test_support::temp_zdx_home();
        let bundled_root = crate::skills::ensure_bundled_skills_materialized().unwrap();
        unsafe {
            std::env::set_var("ZDX_HOME", bundled_root.parent().unwrap().as_os_str());
        }

        let ctx = zdx_tools::ToolContext::new(std::env::current_dir().unwrap(), None);
        let input = json!({"file_path": "${ZDX_HOME}/bundled-skills/memory/SKILL.md"});

        let result = read::execute(&input, &ctx);
        assert!(result.is_ok());
        let data = result.data().expect("should have data");
        assert_eq!(
            data["file_path"],
            "${ZDX_HOME}/bundled-skills/memory/SKILL.md"
        );
        assert_eq!(
            data["resolved_file_path"],
            bundled_root
                .join("memory")
                .join("SKILL.md")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );
        assert!(data["content"].as_str().unwrap().contains("# Memory"));
    }
}
