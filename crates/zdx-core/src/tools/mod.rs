//! Tool system for agentic capabilities.
//!
//! This module provides a registry of tools that the agent can use,
//! along with schema definitions for the Anthropic API.

pub mod apply_patch;
pub mod bash;
pub mod edit;
pub mod fetch_webpage;
pub mod read;
pub mod read_thread;
pub mod subagent;
pub mod web_search;
pub mod write;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::events::ToolOutput;

// ============================================================================
// Serde helpers for LLM-resilient deserialization
// ============================================================================

/// Serde helper that accepts either a JSON array of strings or a single string.
///
/// LLMs sometimes send `"search_queries": "single query"` instead of
/// `"search_queries": ["single query"]`. This module gracefully coerces
/// a bare string into a one-element `Vec<String>`.
pub(crate) mod string_or_vec {
    use serde::{Deserialize, Deserializer, de};

    /// Deserializes a `Vec<String>` that also accepts a single string.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StringOrVec {
            String(String),
            Vec(Vec<String>),
        }

        // Option wrapper: if the field is missing (None via serde(default)),
        // this function won't be called – serde returns None directly.
        // When called, the value is present so we parse it.
        let value: Option<StringOrVec> = Option::deserialize(deserializer)?;
        match value {
            None => Ok(None),
            Some(StringOrVec::String(s)) => {
                if s.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(vec![s]))
                }
            }
            Some(StringOrVec::Vec(v)) => {
                // Validate every element is a non-empty string
                for item in &v {
                    if item.is_empty() {
                        return Err(de::Error::custom(
                            "search_queries array contains empty string",
                        ));
                    }
                }
                if v.is_empty() { Ok(None) } else { Ok(Some(v)) }
            }
        }
    }
}

/// Serde helper that accepts either a JSON boolean or a boolean-like string.
///
/// LLMs sometimes send `"full_content": "true"` instead of
/// `"full_content": true`. This module gracefully coerces common string
/// representations into `bool`.
pub(crate) mod bool_or_string {
    use serde::{Deserialize, Deserializer, de};

    /// Deserializes a `bool` that also accepts string values like
    /// `"true"`, `"false"`, `"1"`, `"0"`, `"yes"`, `"no"`.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<bool, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum BoolOrString {
            Bool(bool),
            String(String),
        }

        match BoolOrString::deserialize(deserializer)? {
            BoolOrString::Bool(v) => Ok(v),
            BoolOrString::String(raw) => {
                let normalized = raw.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "true" | "1" | "yes" | "y" | "on" => Ok(true),
                    "false" | "0" | "no" | "n" | "off" | "" => Ok(false),
                    _ => Err(de::Error::custom(format!(
                        "expected boolean or boolean-like string, got '{raw}'"
                    ))),
                }
            }
        }
    }
}

// ============================================================================
// Path Resolution Helpers
// ============================================================================

/// Resolves a path for reading/editing an existing file.
///
/// - Joins relative paths with root
/// - Canonicalizes the path (resolves symlinks, `..`, etc.)
/// - Returns error if the file doesn't exist
///
/// Use this for `read` and `edit` tools where the file must exist.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn resolve_existing_path(path: &str, root: &Path) -> Result<PathBuf, ToolOutput> {
    let requested = Path::new(path);

    // Join with root (handles both absolute and relative paths)
    let full_path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };

    // Canonicalize to resolve any .. or symlinks (requires file to exist)
    full_path.canonicalize().map_err(|e| {
        ToolOutput::failure(
            "path_error",
            format!("Path does not exist '{}'", full_path.display()),
            Some(format!("OS error: {e}")),
        )
    })
}

/// Tool definition for the Anthropic API.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolDefinition {
    /// Returns a copy with the name lowercased.
    ///
    /// Anthropic requires `PascalCase` tool names, but other providers
    /// (`OpenAI`, Gemini, `OpenRouter`) work better with lowercase.
    #[must_use]
    pub fn with_lowercase_name(&self) -> Self {
        Self {
            name: self.name.to_ascii_lowercase(),
            ..self.clone()
        }
    }
}

/// Content block within a tool result.
///
/// Anthropic API requires `tool_result` content to be an array of blocks
/// when including images: `[{type: "text", ...}, {type: "image", ...}]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultBlock {
    /// Text content block.
    Text { text: String },
    /// Image content block (base64 encoded).
    Image { mime_type: String, data: String },
}

/// Content of a tool result - either simple text or structured blocks.
///
/// - `Text`: Simple string content (backwards compatible, serializes as string)
/// - `Blocks`: Array of content blocks (required for images)
///
/// Uses `#[serde(untagged)]` so `Text` serializes as a plain string and
/// `Blocks` serializes as an array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Simple text content (serializes as string for backwards compatibility).
    Text(String),
    /// Array of content blocks (required when including images).
    Blocks(Vec<ToolResultBlock>),
}

// Test-only helper for ToolResultContent
#[cfg(test)]
impl ToolResultContent {
    /// Returns the text content if this is Text variant, or the first text block's content.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolResultContent::Text(s) => Some(s),
            ToolResultContent::Blocks(blocks) => blocks.iter().find_map(|b| match b {
                ToolResultBlock::Text { text } => Some(text.as_str()),
                ToolResultBlock::Image { .. } => None,
            }),
        }
    }
}

/// Result of executing a tool (for API compatibility).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: ToolResultContent,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// Creates a `ToolResult` from a `ToolOutput`.
    ///
    /// If the output contains image content, creates a Blocks content with
    /// both text (JSON envelope) and image blocks. Otherwise, creates Text content.
    pub fn from_output(tool_use_id: String, output: &ToolOutput) -> Self {
        let content = match output.image() {
            Some(image) => {
                // Create blocks with text description + image
                let text_block = ToolResultBlock::Text {
                    text: output.to_json_string(),
                };
                let image_block = ToolResultBlock::Image {
                    mime_type: image.mime_type.clone(),
                    data: image.data.clone(),
                };
                ToolResultContent::Blocks(vec![text_block, image_block])
            }
            None => ToolResultContent::Text(output.to_json_string()),
        };

        Self {
            tool_use_id,
            content,
            is_error: !output.is_ok(),
        }
    }
}

/// Context for tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Root directory for file operations.
    pub root: PathBuf,

    /// Optional timeout for tool execution.
    pub timeout: Option<Duration>,

    /// Optional model override for tool subagents.
    pub model: Option<String>,

    /// Optional thinking level for tool subagents.
    pub thinking_level: Option<crate::config::ThinkingLevel>,

    /// Whether subagent delegation is enabled.
    pub subagents_enabled: bool,

    /// Available model list for subagent delegation.
    pub subagent_available_models: Vec<String>,
}

impl ToolContext {
    pub fn new(root: PathBuf, timeout: Option<Duration>) -> Self {
        Self {
            root,
            timeout,
            model: None,
            thinking_level: None,
            subagents_enabled: true,
            subagent_available_models: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_config(mut self, config: &crate::config::Config) -> Self {
        self.model = Some(config.model.clone());
        self.thinking_level = Some(config.thinking_level);
        self.subagents_enabled = config.subagents.enabled;
        self.subagent_available_models = config.subagent_available_models();
        self
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
                "invoke_subagent",
                "read",
                "read_thread",
                "web_search",
                "write",
            ],
            ToolSet::OpenAICodex => &[
                "bash",
                "apply_patch",
                "fetch_webpage",
                "invoke_subagent",
                "read",
                "read_thread",
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
        self.register(
            bash::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { bash::execute(&input, &ctx, ctx.timeout).await })
            }),
        );

        self.register(
            apply_patch::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { execute_apply_patch(&input, &ctx).await })
            }),
        );

        self.register(
            edit::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { execute_edit(&input, &ctx).await })
            }),
        );

        self.register(
            read::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { execute_read(&input, &ctx).await })
            }),
        );

        self.register(
            read_thread::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { read_thread::execute(&input, &ctx).await })
            }),
        );

        self.register(
            subagent::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { subagent::execute(&input, &ctx).await })
            }),
        );

        self.register(
            write::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { execute_write(&input, &ctx).await })
            }),
        );

        self.register(
            web_search::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { web_search::execute(&input, &ctx).await })
            }),
        );

        self.register(
            fetch_webpage::definition(),
            Arc::new(|input, ctx| {
                let input = input.clone();
                let ctx = ctx.clone();
                Box::pin(async move { fetch_webpage::execute(&input, &ctx).await })
            }),
        );
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

async fn execute_edit(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || edit::execute(&input, &ctx)
    })
    .await
}

async fn execute_apply_patch(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || apply_patch::execute(&input, &ctx)
    })
    .await
}

async fn execute_read(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || read::execute(&input, &ctx)
    })
    .await
}

async fn execute_write(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || write::execute(&input, &ctx)
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
        let input = json!({"path": "test.txt"});

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
}
