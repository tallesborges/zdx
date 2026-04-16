//! Tool system for agentic capabilities.
//!
//! This module provides a registry of tools that the agent can use,
//! along with schema definitions for the Anthropic API.

// Leaf tools re-exported from zdx-tools
pub use zdx_tools::{apply_patch, bash, edit, fetch_webpage, glob, grep, read, web_search, write};

// Engine-backed tools (need full ToolContext with config, threads, etc.)
pub mod read_thread;
pub mod subagent;
pub mod thread_search;
pub mod todo_write;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
// Re-export path helpers and serde helpers from zdx-tools for backward compat
pub use zdx_tools::{
    ResolvedPath, expand_env_vars, insert_file_path_fields, resolve_existing_path,
    resolve_input_path, resolve_path_against_root,
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

    /// Event sender for emitting streaming tool output events.
    /// Set by the engine before tool execution; used by `bash_handler`
    /// to bridge output chunks to `ToolOutputDelta` events.
    pub event_sender: Option<EventSender>,

    /// Tool use ID for the current execution (needed for `ToolOutputDelta` events).
    pub tool_use_id: Option<String>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("root", &self.root)
            .field("current_thread_id", &self.current_thread_id)
            .field("timeout", &self.timeout)
            .field("model", &self.model)
            .field("read_thread_model", &self.read_thread_model)
            .field("thinking_level", &self.thinking_level)
            .field("config", &self.config.is_some())
            .field("subagents_enabled", &self.subagents_enabled)
            .field("subagent_available_models", &self.subagent_available_models)
            .field("event_sender", &self.event_sender.as_ref().map(|_| ".."))
            .field("tool_use_id", &self.tool_use_id)
            .finish()
    }
}

impl ToolContext {
    pub fn new(root: PathBuf, timeout: Option<Duration>) -> Self {
        Self {
            root,
            current_thread_id: None,
            timeout,
            model: None,
            read_thread_model: None,
            thinking_level: None,
            config: None,
            subagents_enabled: true,
            subagent_available_models: Vec::new(),
            event_sender: None,
            tool_use_id: None,
        }
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
        zdx_tools::ToolContext::new(self.root.clone(), self.timeout)
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
                "edit",
                "fetch_webpage",
                "glob",
                "grep",
                "invoke_subagent",
                "read",
                "read_thread",
                "todo_write",
                "thread_search",
                "web_search",
                "write",
            ],
            ToolSet::OpenAICodex => &[
                "bash",
                "apply_patch",
                "fetch_webpage",
                "glob",
                "grep",
                "invoke_subagent",
                "read",
                "read_thread",
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
pub type ToolHandler = Arc<dyn Fn(&Value, &ToolContext) -> ToolFuture + Send + Sync>;
type ToolExecutor = fn(Value, ToolContext) -> ToolFuture;

/// Tool registry (definitions + executors).
#[derive(Clone, Default)]
pub struct ToolRegistry {
    definitions: Vec<ToolDefinition>,
    handlers: HashMap<String, ToolHandler>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("definitions", &self.definitions)
            .field("handlers_len", &self.handlers.len())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            definitions: Vec::new(),
            handlers: HashMap::new(),
        }
    }

    pub fn builtins() -> Self {
        let mut registry = Self::new();
        registry.register_builtin_tools();
        registry
    }

    #[must_use]
    pub fn with_tool(mut self, definition: ToolDefinition, handler: ToolHandler) -> Self {
        self.register(definition, handler);
        self
    }

    fn register_builtin_tool(&mut self, definition: ToolDefinition, executor: ToolExecutor) {
        self.register(
            definition,
            Arc::new(move |input, ctx| executor(input.clone(), ctx.clone())),
        );
    }

    pub fn register(&mut self, definition: ToolDefinition, handler: ToolHandler) {
        let name_lower = definition.name.to_ascii_lowercase();
        if let Some(pos) = self
            .definitions
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case(&definition.name))
        {
            self.definitions.remove(pos);
        }
        self.definitions.push(definition);
        self.handlers.insert(name_lower, handler);
    }

    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.definitions
            .iter()
            .map(|t| t.name.to_lowercase())
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

        self.definitions
            .iter()
            .filter(|t| include_set.contains(&t.name.to_lowercase()))
            .cloned()
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

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn execute_tool<S>(
        &self,
        name: &str,
        tool_use_id: &str,
        input: &Value,
        ctx: &ToolContext,
        enabled_tools: &std::collections::HashSet<String, S>,
    ) -> (ToolOutput, ToolResult)
    where
        S: std::hash::BuildHasher,
    {
        let name_lower = name.to_ascii_lowercase();
        let is_enabled = enabled_tools
            .iter()
            .any(|t| t.to_ascii_lowercase() == name_lower);

        if !is_enabled {
            let output = unknown_tool_output(name, enabled_tools);
            let result = ToolResult::from_output(tool_use_id.to_string(), &output);
            return (output, result);
        }

        let output = match self.handlers.get(&name_lower) {
            Some(handler) => handler(input, ctx).await,
            None => unknown_tool_output(name, enabled_tools),
        };

        let result = ToolResult::from_output(tool_use_id.to_string(), &output);
        (output, result)
    }

    fn register_builtin_tools(&mut self) {
        self.register_builtin_tool(bash::definition(), bash_handler);
        self.register_builtin_tool(apply_patch::definition(), apply_patch_handler);
        self.register_builtin_tool(edit::definition(), edit_handler);
        self.register_builtin_tool(read::definition(), read_handler);
        self.register_builtin_tool(read_thread::definition(), read_thread_handler);
        self.register_builtin_tool(todo_write::definition(), todo_write_handler);
        self.register_builtin_tool(thread_search::definition(), thread_search_handler);
        self.register_builtin_tool(subagent::definition(), subagent_handler);
        self.register_builtin_tool(write::definition(), write_handler);
        self.register_builtin_tool(web_search::definition(), web_search_handler);
        self.register_builtin_tool(fetch_webpage::definition(), fetch_webpage_handler);
        self.register_builtin_tool(grep::definition(), grep_handler);
        self.register_builtin_tool(glob::definition(), glob_handler);
    }
}

// -- Leaf tool handlers (bridge via as_leaf()) --

fn bash_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move {
        let event_sender = ctx.event_sender.clone();
        let tool_use_id = ctx.tool_use_id.clone();

        if let (Some(sender), Some(id)) = (event_sender, tool_use_id) {
            let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

            let leaf = ctx.as_leaf();
            let timeout = ctx.timeout;

            // Run bash and chunk forwarding concurrently.
            // bash owns output_tx and drops it when done, which closes the
            // channel and lets the receiver loop exit.
            tokio::join!(
                bash::execute(&input, &leaf, timeout, Some(output_tx)),
                async {
                    while let Some(chunk) = output_rx.recv().await {
                        sender.send(AgentEvent::ToolOutputDelta {
                            id: id.clone(),
                            chunk,
                        });
                    }
                }
            )
            .0
        } else {
            bash::execute(&input, &ctx.as_leaf(), ctx.timeout, None).await
        }
    })
}

fn apply_patch_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_apply_patch(&input, &ctx.as_leaf()).await })
}

fn edit_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_edit(&input, &ctx.as_leaf()).await })
}

fn read_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_read(&input, &ctx.as_leaf()).await })
}

fn write_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_write(&input, &ctx.as_leaf()).await })
}

fn web_search_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { web_search::execute(&input, &ctx.as_leaf()).await })
}

fn fetch_webpage_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { fetch_webpage::execute(&input, &ctx.as_leaf()).await })
}

fn grep_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_grep(&input, &ctx.as_leaf()).await })
}

fn glob_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_glob(&input, &ctx.as_leaf()).await })
}

// -- Engine tool handlers (use full ToolContext) --

fn read_thread_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { read_thread::execute(&input, &ctx).await })
}

fn todo_write_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_todo_write(&input, &ctx).await })
}

fn thread_search_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { execute_thread_search(&input, &ctx).await })
}

fn subagent_handler(input: Value, ctx: ToolContext) -> ToolFuture {
    Box::pin(async move { subagent::execute(&input, &ctx).await })
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
    ToolRegistry::builtins().definitions
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
    ToolRegistry::builtins()
        .execute_tool(name, tool_use_id, input, ctx, enabled_tools)
        .await
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

async fn execute_todo_write(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || todo_write::execute(&input, &ctx)
    })
    .await
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
    let mut handle = tokio::task::spawn_blocking(f);

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
        assert!(names.contains(&"edit".to_string()));
        assert!(names.contains(&"fetch_webpage".to_string()));
        assert!(names.contains(&"invoke_subagent".to_string()));
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
        assert!(names.contains(&"edit".to_string()));
        assert!(names.contains(&"fetch_webpage".to_string()));
        assert!(names.contains(&"invoke_subagent".to_string()));
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
