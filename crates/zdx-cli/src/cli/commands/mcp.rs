//! MCP helper CLI command handlers.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde_json::Value;
use zdx_core::mcp::{McpDiagnostic, McpServerState, McpServerStatus, McpTool, McpWorkspace};

pub async fn servers(root: &Path) -> Result<()> {
    let workspace = zdx_core::mcp::load_workspace(root).await;
    let output = ServersOutput {
        config_path: workspace.config_path().display().to_string(),
        config_exists: workspace.config_exists(),
        servers: workspace
            .server_statuses()
            .iter()
            .map(server_output)
            .collect(),
        diagnostics: workspace
            .diagnostics()
            .iter()
            .map(diagnostic_output)
            .collect(),
    };

    print_json(&output)
}

pub async fn tools(root: &Path, server_name: &str) -> Result<()> {
    let workspace = zdx_core::mcp::load_workspace(root).await;
    let status = require_loaded_server(&workspace, server_name)?;
    let tools = workspace
        .tools(server_name)
        .ok_or_else(|| server_lookup_error(&workspace, server_name))?;

    let output = ToolsOutput {
        server: status.name.clone(),
        transport: status.transport,
        tools: tools.iter().map(tool_listing).collect(),
    };

    print_json(&output)
}

pub async fn schema(root: &Path, server_name: &str, tool_name: &str) -> Result<()> {
    let workspace = zdx_core::mcp::load_workspace(root).await;
    let status = require_loaded_server(&workspace, server_name)?;
    let tool = workspace
        .tool(server_name, tool_name)
        .ok_or_else(|| tool_lookup_error(&workspace, server_name, tool_name))?;

    let output = ToolSchemaOutput {
        server: status.name.clone(),
        transport: status.transport,
        tool: tool.name.clone(),
        exposed_name: tool.exposed_name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    };

    print_json(&output)
}

pub async fn call(root: &Path, server_name: &str, tool_name: &str, input_json: &str) -> Result<()> {
    let workspace = zdx_core::mcp::load_workspace(root).await;
    let input: Value = serde_json::from_str(input_json)
        .with_context(|| format!("parse --json input for MCP tool '{tool_name}'"))?;
    let output = workspace
        .call_tool(server_name, tool_name, input, None)
        .await?;

    print_json(&output)
}

#[derive(Serialize)]
struct ServersOutput {
    config_path: String,
    config_exists: bool,
    servers: Vec<ServerOutput>,
    diagnostics: Vec<DiagnosticOutput>,
}

#[derive(Serialize)]
struct ServerOutput {
    name: String,
    transport: &'static str,
    status: &'static str,
    tool_count: Option<usize>,
    message: Option<String>,
}

#[derive(Serialize)]
struct DiagnosticOutput {
    kind: &'static str,
    level: &'static str,
    summary: String,
}

#[derive(Serialize)]
struct ToolsOutput {
    server: String,
    transport: &'static str,
    tools: Vec<ToolListing>,
}

#[derive(Serialize)]
struct ToolListing {
    name: String,
    exposed_name: String,
    description: Option<String>,
}

#[derive(Serialize)]
struct ToolSchemaOutput {
    server: String,
    transport: &'static str,
    tool: String,
    exposed_name: String,
    description: Option<String>,
    input_schema: Value,
}

fn server_output(status: &McpServerStatus) -> ServerOutput {
    match &status.status {
        McpServerState::Loaded { tool_count } => ServerOutput {
            name: status.name.clone(),
            transport: status.transport,
            status: "loaded",
            tool_count: Some(*tool_count),
            message: None,
        },
        McpServerState::Failed { message } => ServerOutput {
            name: status.name.clone(),
            transport: status.transport,
            status: "failed",
            tool_count: None,
            message: Some(message.clone()),
        },
    }
}

fn diagnostic_output(diagnostic: &McpDiagnostic) -> DiagnosticOutput {
    DiagnosticOutput {
        kind: diagnostic_kind(diagnostic),
        level: if diagnostic.is_error() {
            "error"
        } else {
            "info"
        },
        summary: diagnostic.summary(),
    }
}

fn diagnostic_kind(diagnostic: &McpDiagnostic) -> &'static str {
    match diagnostic {
        McpDiagnostic::ConfigLoaded { .. } => "config_loaded",
        McpDiagnostic::ConfigError { .. } => "config_error",
        McpDiagnostic::ServerLoaded { .. } => "server_loaded",
        McpDiagnostic::ServerFailed { .. } => "server_failed",
    }
}

fn tool_listing(tool: &McpTool) -> ToolListing {
    ToolListing {
        name: tool.name.clone(),
        exposed_name: tool.exposed_name.clone(),
        description: tool.description.clone(),
    }
}

fn require_loaded_server<'a>(
    workspace: &'a McpWorkspace,
    server_name: &str,
) -> Result<&'a McpServerStatus> {
    match workspace.server_status(server_name) {
        Some(
            status @ McpServerStatus {
                status: McpServerState::Loaded { .. },
                ..
            },
        ) => Ok(status),
        Some(McpServerStatus {
            name,
            transport,
            status: McpServerState::Failed { message },
        }) => Err(anyhow!(
            "MCP server '{name}' failed to load over {transport}: {message}"
        )),
        None => Err(server_lookup_error(workspace, server_name)),
    }
}

fn server_lookup_error(workspace: &McpWorkspace, server_name: &str) -> anyhow::Error {
    if !workspace.config_exists() {
        return anyhow!(
            "No MCP config found at {}. Add {} or pass --root to a project with one.",
            workspace.config_path().display(),
            workspace.config_path().display()
        );
    }

    let available: Vec<&str> = workspace
        .server_statuses()
        .iter()
        .map(|status| status.name.as_str())
        .collect();

    if available.is_empty() {
        anyhow!("No MCP servers are available. Run `zdx mcp servers` to inspect load diagnostics.")
    } else {
        anyhow!(
            "Unknown MCP server '{server_name}'. Available servers: {}",
            available.join(", ")
        )
    }
}

fn tool_lookup_error(
    workspace: &McpWorkspace,
    server_name: &str,
    tool_name: &str,
) -> anyhow::Error {
    let Some(tools) = workspace.tools(server_name) else {
        return server_lookup_error(workspace, server_name);
    };

    let available = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();

    anyhow!(
        "Unknown MCP tool '{tool_name}' on server '{server_name}'. Available tools: {}",
        available.join(", ")
    )
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("serialize MCP command output")?
    );
    Ok(())
}
