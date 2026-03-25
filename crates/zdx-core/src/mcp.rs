use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_util::future::join_all;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde::{Deserialize, Deserializer, de};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::core::events::ToolOutput;

pub const MCP_CONFIG_FILE_NAME: &str = ".mcp.json";

const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MCP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MCP_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

type McpClient = RunningService<RoleClient, ClientInfo>;
type SharedMcpClient = Arc<Mutex<McpClient>>;

#[derive(Clone)]
pub struct McpWorkspace {
    config_path: PathBuf,
    config_exists: bool,
    diagnostics: Vec<McpDiagnostic>,
    server_statuses: Vec<McpServerStatus>,
    servers: BTreeMap<String, LoadedMcpServer>,
}

impl McpWorkspace {
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    #[must_use]
    pub fn config_exists(&self) -> bool {
        self.config_exists
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[McpDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn server_statuses(&self) -> &[McpServerStatus] {
        &self.server_statuses
    }

    #[must_use]
    pub fn server_status(&self, server_name: &str) -> Option<&McpServerStatus> {
        self.server_statuses
            .iter()
            .find(|status| status.name.eq_ignore_ascii_case(server_name))
    }

    #[must_use]
    pub fn tools(&self, server_name: &str) -> Option<&[McpTool]> {
        self.loaded_server(server_name)
            .map(|server| server.tools.as_slice())
    }

    #[must_use]
    pub fn tool(&self, server_name: &str, tool_name: &str) -> Option<&McpTool> {
        self.tools(server_name)?
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(tool_name))
    }

    ///
    /// # Errors
    /// Returns an error when the requested server or tool is unavailable.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        input: Value,
        timeout: Option<Duration>,
    ) -> Result<ToolOutput> {
        let server = self.loaded_server(server_name).ok_or_else(|| {
            anyhow!("MCP server '{server_name}' is not loaded. Run `zdx mcp servers` for status.")
        })?;
        let tool = server
            .tools
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(tool_name))
            .ok_or_else(|| {
                let available = server
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow!(
                    "MCP tool '{tool_name}' was not found on server '{}'. Available tools: {available}",
                    server.name
                )
            })?;

        Ok(call_mcp_tool(&server.client, &server.name, &tool.name, input, timeout).await)
    }

    fn loaded_server(&self, server_name: &str) -> Option<&LoadedMcpServer> {
        self.servers
            .values()
            .find(|server| server.name.eq_ignore_ascii_case(server_name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub transport: &'static str,
    pub status: McpServerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerState {
    Loaded { tool_count: usize },
    Failed { message: String },
}

#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub exposed_name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Clone)]
struct LoadedMcpServer {
    name: String,
    client: SharedMcpClient,
    tools: Vec<McpTool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpDiagnostic {
    ConfigLoaded {
        path: PathBuf,
        server_count: usize,
    },
    ConfigError {
        path: PathBuf,
        message: String,
    },
    ServerLoaded {
        server: String,
        transport: &'static str,
        tool_count: usize,
    },
    ServerFailed {
        server: String,
        transport: &'static str,
        message: String,
    },
}

impl McpDiagnostic {
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            McpDiagnostic::ConfigLoaded { path, server_count } => format!(
                "Loaded MCP config {} with {} server{}",
                path.display(),
                server_count,
                if *server_count == 1 { "" } else { "s" }
            ),
            McpDiagnostic::ConfigError { path, message } => {
                format!("Failed to load MCP config {}: {message}", path.display())
            }
            McpDiagnostic::ServerLoaded {
                server,
                transport,
                tool_count,
            } => format!(
                "Loaded MCP server '{server}' over {transport} with {tool_count} tool{}",
                if *tool_count == 1 { "" } else { "s" }
            ),
            McpDiagnostic::ServerFailed {
                server,
                transport,
                message,
            } => format!("Failed MCP server '{server}' over {transport}: {message}"),
        }
    }

    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            McpDiagnostic::ConfigError { .. } | McpDiagnostic::ServerFailed { .. }
        )
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct McpConfigFile {
    #[serde(rename = "mcpServers")]
    mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone)]
enum McpServerConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawMcpServerConfig {
    #[serde(rename = "type")]
    server_type: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    env: HashMap<String, String>,
    url: Option<String>,
    headers: HashMap<String, String>,
}

impl<'de> Deserialize<'de> for McpServerConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawMcpServerConfig::deserialize(deserializer)?;

        match raw.server_type.as_deref() {
            Some("stdio") => {
                let command = raw
                    .command
                    .ok_or_else(|| de::Error::missing_field("command"))?;
                Ok(Self::Stdio {
                    command,
                    args: raw.args,
                    env: raw.env,
                })
            }
            Some("http") => {
                let url = raw.url.ok_or_else(|| de::Error::missing_field("url"))?;
                Ok(Self::Http {
                    url,
                    headers: raw.headers,
                })
            }
            Some(other) => Err(de::Error::unknown_variant(other, &["stdio", "http"])),
            None => {
                if let Some(url) = raw.url {
                    Ok(Self::Http {
                        url,
                        headers: raw.headers,
                    })
                } else if raw.command.is_some() {
                    Err(de::Error::custom(
                        "MCP stdio servers must set \"type\": \"stdio\"",
                    ))
                } else {
                    Err(de::Error::custom(
                        "MCP server must set \"type\" or provide \"url\" for default HTTP transport",
                    ))
                }
            }
        }
    }
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join(MCP_CONFIG_FILE_NAME)
}

pub async fn load_workspace(root: &Path) -> McpWorkspace {
    let path = config_path(root);
    let mut workspace = empty_workspace(path);

    if !workspace.config_exists {
        return workspace;
    }

    let Some(config) = load_workspace_config(&mut workspace) else {
        return workspace;
    };

    for discovery in discover_servers(config).await {
        apply_discovery(&mut workspace, discovery);
    }

    workspace
}

fn load_config_file(path: &Path) -> Result<McpConfigFile> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read MCP config {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("parse MCP config {}", path.display()))
}

fn empty_workspace(path: PathBuf) -> McpWorkspace {
    let config_exists = path.exists();
    McpWorkspace {
        config_path: path,
        config_exists,
        diagnostics: Vec::new(),
        server_statuses: Vec::new(),
        servers: BTreeMap::new(),
    }
}

fn load_workspace_config(workspace: &mut McpWorkspace) -> Option<McpConfigFile> {
    match load_config_file(&workspace.config_path) {
        Ok(config) => {
            workspace.diagnostics.push(McpDiagnostic::ConfigLoaded {
                path: workspace.config_path.clone(),
                server_count: config.mcp_servers.len(),
            });
            Some(config)
        }
        Err(error) => {
            workspace.diagnostics.push(McpDiagnostic::ConfigError {
                path: workspace.config_path.clone(),
                message: error.to_string(),
            });
            None
        }
    }
}

async fn discover_servers(config: McpConfigFile) -> Vec<DiscoveryOutcome> {
    join_all(
        config
            .mcp_servers
            .into_iter()
            .map(|(server_name, server_config)| async move {
                match server_config {
                    McpServerConfig::Stdio { command, args, env } => {
                        match discover_stdio_tools(&server_name, &command, &args, &env).await {
                            Ok(server) => DiscoveryOutcome::Loaded {
                                server_name,
                                transport: "stdio",
                                server,
                            },
                            Err(error) => DiscoveryOutcome::Failed {
                                server_name,
                                transport: "stdio",
                                message: error.to_string(),
                            },
                        }
                    }
                    McpServerConfig::Http { url, headers } => {
                        match discover_http_tools(&server_name, &url, &headers).await {
                            Ok(server) => DiscoveryOutcome::Loaded {
                                server_name,
                                transport: "http",
                                server,
                            },
                            Err(error) => DiscoveryOutcome::Failed {
                                server_name,
                                transport: "http",
                                message: error.to_string(),
                            },
                        }
                    }
                }
            }),
    )
    .await
}

fn apply_discovery(workspace: &mut McpWorkspace, discovery: DiscoveryOutcome) {
    match discovery {
        DiscoveryOutcome::Loaded {
            server_name,
            transport,
            server,
        } => {
            let tools = build_server_tools(&server_name, &server.tools);
            let tool_count = tools.len();
            workspace.server_statuses.push(McpServerStatus {
                name: server_name.clone(),
                transport,
                status: McpServerState::Loaded { tool_count },
            });
            workspace.diagnostics.push(McpDiagnostic::ServerLoaded {
                server: server_name.clone(),
                transport,
                tool_count,
            });
            workspace.servers.insert(
                server_name.clone(),
                LoadedMcpServer {
                    name: server_name,
                    client: server.client,
                    tools,
                },
            );
        }
        DiscoveryOutcome::Failed {
            server_name,
            transport,
            message,
        } => {
            workspace.server_statuses.push(McpServerStatus {
                name: server_name.clone(),
                transport,
                status: McpServerState::Failed {
                    message: message.clone(),
                },
            });
            workspace.diagnostics.push(McpDiagnostic::ServerFailed {
                server: server_name,
                transport,
                message,
            });
        }
    }
}

struct DiscoveredServer {
    client: SharedMcpClient,
    tools: Vec<Value>,
}

enum DiscoveryOutcome {
    Loaded {
        server_name: String,
        transport: &'static str,
        server: DiscoveredServer,
    },
    Failed {
        server_name: String,
        transport: &'static str,
        message: String,
    },
}

async fn discover_stdio_tools(
    server_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<DiscoveredServer> {
    let client_info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("zdx", env!("CARGO_PKG_VERSION")),
    );

    let transport = TokioChildProcess::new(Command::new(command).configure(|cmd| {
        cmd.args(args);
        if !env.is_empty() {
            cmd.envs(env.iter());
        }
    }))
    .with_context(|| format!("spawn MCP stdio server '{server_name}'"))?;

    let client = run_mcp_op_with_timeout(
        format!("initialize MCP stdio server '{server_name}'"),
        MCP_CONNECT_TIMEOUT,
        client_info.serve(transport),
    )
    .await?;

    let tools = run_mcp_op_with_timeout(
        format!("list MCP tools for stdio server '{server_name}'"),
        MCP_DISCOVERY_TIMEOUT,
        client.list_all_tools(),
    )
    .await?
    .into_iter()
    .map(|tool| serde_json::to_value(tool).context("serialize discovered MCP tool"))
    .collect::<Result<Vec<_>>>()?;

    Ok(DiscoveredServer {
        client: Arc::new(Mutex::new(client)),
        tools,
    })
}

async fn discover_http_tools(
    server_name: &str,
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<DiscoveredServer> {
    let client_info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("zdx", env!("CARGO_PKG_VERSION")),
    );
    let header_map = build_http_headers(headers)
        .with_context(|| format!("build HTTP headers for MCP server '{server_name}'"))?;
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_string()).custom_headers(header_map),
    );

    let client = run_mcp_op_with_timeout(
        format!("initialize MCP HTTP server '{server_name}'"),
        MCP_CONNECT_TIMEOUT,
        client_info.serve(transport),
    )
    .await?;

    let tools = run_mcp_op_with_timeout(
        format!("list MCP tools for HTTP server '{server_name}'"),
        MCP_DISCOVERY_TIMEOUT,
        client.list_all_tools(),
    )
    .await?
    .into_iter()
    .map(|tool| serde_json::to_value(tool).context("serialize discovered MCP tool"))
    .collect::<Result<Vec<_>>>()?;

    Ok(DiscoveredServer {
        client: Arc::new(Mutex::new(client)),
        tools,
    })
}

fn build_server_tools(server_name: &str, serialized_tools: &[Value]) -> Vec<McpTool> {
    let mut exposed_names = HashSet::new();
    let mut tools = Vec::new();

    for tool in serialized_tools {
        let Some(raw_tool_name) = extract_tool_name(tool) else {
            tracing::warn!(server = server_name, tool = ?tool, "Skipping MCP tool without name");
            continue;
        };

        let exposed_name =
            stable_exposed_tool_name(server_name, &raw_tool_name, &mut exposed_names);
        tools.push(McpTool {
            name: raw_tool_name,
            exposed_name,
            description: extract_tool_description(tool),
            input_schema: extract_input_schema(tool),
        });
    }

    tools
}

fn extract_tool_name(tool: &Value) -> Option<String> {
    tool.get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(std::string::ToString::to_string)
}

fn extract_input_schema(serialized_tool: &Value) -> Value {
    serialized_tool
        .get("inputSchema")
        .or_else(|| serialized_tool.get("input_schema"))
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true,
            })
        })
}

fn extract_tool_description(serialized_tool: &Value) -> Option<String> {
    serialized_tool
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(std::string::ToString::to_string)
}

async fn call_mcp_tool(
    client: &SharedMcpClient,
    server_name: &str,
    raw_tool_name: &str,
    input: Value,
    timeout: Option<Duration>,
) -> ToolOutput {
    let arguments = match input_to_arguments(input) {
        Ok(arguments) => arguments,
        Err(output) => return output,
    };

    let timeout = timeout.unwrap_or(DEFAULT_MCP_TOOL_CALL_TIMEOUT);

    let call_result = {
        let client = client.lock().await;
        tokio::time::timeout(
            timeout,
            client.call_tool(
                CallToolRequestParams::new(raw_tool_name.to_string()).with_arguments(arguments),
            ),
        )
        .await
    };

    let Ok(result) = call_result else {
        return ToolOutput::failure(
            "mcp_tool_timeout",
            format!(
                "MCP tool '{raw_tool_name}' on server '{server_name}' timed out after {}s",
                timeout.as_secs()
            ),
            None,
        );
    };

    let result = match result {
        Ok(result) => result,
        Err(error) => {
            return ToolOutput::failure(
                "mcp_tool_call_failed",
                format!("Failed to call MCP tool '{raw_tool_name}' on server '{server_name}'"),
                Some(error.to_string()),
            );
        }
    };

    let serialized_result = match serde_json::to_value(&result) {
        Ok(value) => value,
        Err(error) => {
            return ToolOutput::failure(
                "mcp_result_serialize_failed",
                format!(
                    "Failed to serialize MCP tool result for '{raw_tool_name}' on server '{server_name}'"
                ),
                Some(error.to_string()),
            );
        }
    };

    if result_is_error(&serialized_result) {
        return ToolOutput::failure(
            "mcp_tool_error",
            format!("MCP tool '{raw_tool_name}' on server '{server_name}' returned an error"),
            Some(serialized_result.to_string()),
        );
    }

    ToolOutput::success(json!({
        "server": server_name,
        "tool": raw_tool_name,
        "result": serialized_result,
    }))
}

fn input_to_arguments(input: Value) -> Result<Map<String, Value>, ToolOutput> {
    match input {
        Value::Object(map) => Ok(map),
        other => Err(ToolOutput::failure(
            "invalid_input",
            "MCP tool input must be a JSON object",
            Some(format!("received: {other}")),
        )),
    }
}

fn result_is_error(value: &Value) -> bool {
    value
        .get("isError")
        .or_else(|| value.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn build_http_headers(
    headers: &HashMap<String, String>,
) -> Result<HashMap<HeaderName, HeaderValue>> {
    headers
        .iter()
        .map(|(name, value)| {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid HTTP header name '{name}'"))?;
            let header_value = HeaderValue::from_str(value)
                .with_context(|| format!("invalid HTTP header value for '{name}'"))?;
            Ok((header_name, header_value))
        })
        .collect()
}

async fn run_mcp_op_with_timeout<T, E, F>(action: String, timeout: Duration, future: F) -> Result<T>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
    E: std::error::Error + Send + Sync + 'static,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result.with_context(|| action.clone()),
        Err(_) => Err(anyhow!(
            "timed out after {}s while {action}",
            timeout.as_secs()
        )),
    }
}

fn stable_exposed_tool_name(
    server_name: &str,
    raw_tool_name: &str,
    exposed_names: &mut HashSet<String>,
) -> String {
    let server_slug = sanitize_name_component(server_name, "server");
    let tool_slug = sanitize_name_component(raw_tool_name, "tool");
    let base_name = format!("mcp__{server_slug}__{tool_slug}");

    if base_name.len() <= 64 && exposed_names.insert(base_name.clone()) {
        return base_name;
    }

    let hash = short_hash(&format!("{server_name}\0{raw_tool_name}"));
    let max_server_len = 16usize;
    let max_tool_len = 30usize;
    let fallback = format!(
        "mcp__{}__{}__{hash}",
        truncate_component(&server_slug, max_server_len),
        truncate_component(&tool_slug, max_tool_len)
    );

    if exposed_names.insert(fallback.clone()) {
        return fallback;
    }

    let mut counter = 2usize;
    loop {
        let candidate = format!("{fallback}__{counter}");
        if candidate.len() <= 64 && exposed_names.insert(candidate.clone()) {
            return candidate;
        }
        counter += 1;
    }
}

fn sanitize_name_component(input: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_separator = false;

    for ch in input.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else {
            Some('_')
        };

        if let Some(ch) = mapped {
            if ch == '_' {
                if out.is_empty() || last_was_separator {
                    continue;
                }
                last_was_separator = true;
            } else {
                last_was_separator = false;
            }
            out.push(ch);
        }
    }

    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn truncate_component(input: &str, max_len: usize) -> &str {
    if input.len() <= max_len {
        input
    } else {
        &input[..max_len]
    }
}

fn short_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(8);
    for byte in digest.iter().take(4) {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_standard_mcp_config_shape() {
        let config: McpConfigFile = serde_json::from_str(
            r#"{
                "mcpServers": {
                    "xcode": {
                        "type": "stdio",
                        "command": "xcrun",
                        "args": ["mcpbridge"],
                        "env": {"DEMO": "1"}
                    },
                    "figma": {
                        "type": "http",
                        "url": "https://mcp.figma.com/mcp",
                        "headers": {"authorization": "Bearer token"}
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(config.mcp_servers.len(), 2);
        assert!(matches!(
            config.mcp_servers.get("xcode"),
            Some(McpServerConfig::Stdio { command, args, env })
                if command == "xcrun" && args == &vec!["mcpbridge".to_string()] && env.get("DEMO") == Some(&"1".to_string())
        ));
        assert!(matches!(
            config.mcp_servers.get("figma"),
            Some(McpServerConfig::Http { url, headers })
                if url == "https://mcp.figma.com/mcp" && headers.get("authorization") == Some(&"Bearer token".to_string())
        ));
    }

    #[test]
    fn parses_http_server_without_explicit_type_as_http() {
        let config: McpConfigFile = serde_json::from_str(
            r#"{
                "mcpServers": {
                    "deepwiki": {
                        "url": "https://mcp.deepwiki.com/mcp"
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(matches!(
            config.mcp_servers.get("deepwiki"),
            Some(McpServerConfig::Http { url, headers })
                if url == "https://mcp.deepwiki.com/mcp" && headers.is_empty()
        ));
    }

    #[test]
    fn missing_config_returns_empty_workspace() {
        let root = tempdir().unwrap();
        let workspace = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(load_workspace(root.path()));

        assert!(!workspace.config_exists());
        assert!(workspace.server_statuses().is_empty());
        assert!(workspace.diagnostics().is_empty());
    }

    #[test]
    fn invalid_config_is_non_fatal() {
        let root = tempdir().unwrap();
        let path = config_path(root.path());
        std::fs::write(&path, "not json").unwrap();

        let workspace = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(load_workspace(root.path()));

        assert!(workspace.config_exists());
        assert!(workspace.server_statuses().is_empty());
        assert!(matches!(
            workspace.diagnostics(),
            [McpDiagnostic::ConfigError { path: found_path, .. }] if found_path == &path
        ));
    }

    #[test]
    fn exposed_tool_names_are_stable_and_collision_safe() {
        let mut seen = HashSet::new();

        let first = stable_exposed_tool_name("Xcode", "Build App", &mut seen);
        let second = stable_exposed_tool_name("xcode", "build-app", &mut seen);
        let third = stable_exposed_tool_name(
            "a-very-long-server-name-that-keeps-going",
            "a-very-long-tool-name-that-keeps-going-and-going-and-going",
            &mut seen,
        );

        assert_eq!(first, "mcp__xcode__build_app");
        assert_ne!(first, second);
        assert!(second.starts_with("mcp__xcode__build_app__"));
        assert!(third.starts_with("mcp__a_very_long_serv__a_very_long_tool_name_that_kee__"));
        assert!(third.len() <= 64);
    }

    #[test]
    fn build_http_headers_validates_names_and_values() {
        let headers = HashMap::from([
            ("authorization".to_string(), "Bearer token".to_string()),
            ("x-demo".to_string(), "123".to_string()),
        ]);

        let built = build_http_headers(&headers).unwrap();

        assert_eq!(built.len(), 2);
        assert_eq!(
            built
                .get(&HeaderName::from_static("authorization"))
                .unwrap(),
            &HeaderValue::from_static("Bearer token")
        );
    }
}
