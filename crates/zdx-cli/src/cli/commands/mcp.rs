//! MCP helper CLI command handlers.

use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde_json::Value;
use zdx_core::mcp::{
    McpAuthRequirement, McpAuthorizationServerMetadata, McpDiagnostic, McpHttpServerConfig,
    McpOAuthCache, McpOAuthClientConfig, McpServerState, McpServerStatus, McpTool, McpWorkspace,
};

const DEFAULT_MCP_REDIRECT_URI: &str = "http://127.0.0.1:8787/callback";

struct ResolvedMcpAuth {
    server: McpHttpServerConfig,
    requirement: McpAuthRequirement,
    metadata: McpAuthorizationServerMetadata,
    authorization_server: Option<String>,
}

pub async fn auth(root: &Path, server_name: &str) -> Result<()> {
    let resolved = resolve_auth_target(root, server_name).await?;
    let mut client_config = resolve_client_config(&resolved).await?;
    let redirect_uri = ensure_redirect_uri(&mut client_config);
    let scopes = resolve_scopes(&resolved, &client_config)?;
    let resource = resolved
        .requirement
        .resource
        .clone()
        .unwrap_or_else(|| resolved.server.url.clone());
    let (pkce_verifier, pkce_challenge) = zdx_core::mcp::generate_pkce();
    let state = uuid::Uuid::new_v4().to_string();
    let auth_url = zdx_core::mcp::build_oauth_authorization_url(
        &resolved.metadata,
        &client_config,
        &resource,
        &redirect_uri,
        &scopes,
        &state,
        &pkce_challenge,
    );

    print_auth_instructions(&resolved.server.name, &auth_url);
    let code = read_authorization_code(&auth_url, &redirect_uri, &state)?;

    println!("Exchanging code for tokens...");
    let mut credentials = zdx_core::mcp::exchange_oauth_code(
        &resolved.metadata,
        &client_config,
        &resource,
        &code,
        &pkce_verifier,
        &redirect_uri,
        &scopes,
    )
    .await?;
    if credentials.authorization_server.is_none() {
        credentials.authorization_server = resolved.authorization_server;
    }

    let mut cache = McpOAuthCache::load()?;
    cache.set(&resource, credentials);
    cache.save()?;

    println!();
    println!("✓ Authenticated MCP server '{}'", resolved.server.name);
    println!("  Resource: {resource}");
    println!(
        "  Credentials saved to: {}",
        McpOAuthCache::cache_path().display()
    );
    println!(
        "  Next: rerun `zdx mcp servers` or `zdx mcp tools {}`",
        resolved.server.name
    );

    Ok(())
}

pub async fn logout(root: &Path, server_name: &str) -> Result<()> {
    let server = zdx_core::mcp::load_http_server_config(root, server_name)?;
    let removed = zdx_core::mcp::clear_http_auth_credentials(&server).await?;

    if removed {
        println!(
            "✓ Cleared cached MCP OAuth credentials for '{}'",
            server.name
        );
        println!("  Cache file: {}", McpOAuthCache::cache_path().display());
    } else {
        println!(
            "No cached MCP OAuth credentials found for '{}'",
            server.name
        );
    }

    Ok(())
}

async fn resolve_auth_target(root: &Path, server_name: &str) -> Result<ResolvedMcpAuth> {
    let server = zdx_core::mcp::load_http_server_config(root, server_name)?;
    let inspection = zdx_core::mcp::inspect_http_auth(&server).await?;
    let requirement = inspection.requirement.ok_or_else(|| {
        anyhow!(
            "MCP server '{}' did not advertise OAuth auth requirements. If it uses static headers, configure them directly in .mcp.json.",
            server.name
        )
    })?;
    let metadata = inspection.metadata.ok_or_else(|| {
        anyhow!(
            "MCP server '{}' requires authentication, but no authorization server metadata could be discovered.",
            server.name
        )
    })?;

    if !metadata.code_challenge_methods_supported.is_empty()
        && !metadata
            .code_challenge_methods_supported
            .iter()
            .any(|method| method.eq_ignore_ascii_case("S256"))
    {
        anyhow::bail!(
            "Authorization server for '{}' does not advertise PKCE S256 support.",
            server.name
        );
    }

    Ok(ResolvedMcpAuth {
        server,
        requirement,
        metadata,
        authorization_server: inspection.authorization_server,
    })
}

async fn resolve_client_config(resolved: &ResolvedMcpAuth) -> Result<McpOAuthClientConfig> {
    match resolved.server.oauth.clone() {
        Some(oauth) => Ok(oauth),
        None => zdx_core::mcp::register_dynamic_client(
            &resolved.metadata,
            DEFAULT_MCP_REDIRECT_URI,
        )
        .await
        .map_err(|error| {
            anyhow!(
                "MCP server '{}' requires an OAuth client before ZDX can authenticate. Dynamic client registration failed: {error}\nConfigure an OAuth client in .mcp.json under mcpServers.{}.oauth.",
                resolved.server.name,
                resolved.server.name
            )
        }),
    }
}

fn ensure_redirect_uri(client_config: &mut McpOAuthClientConfig) -> String {
    let redirect_uri = client_config
        .redirect_uri
        .clone()
        .unwrap_or_else(|| DEFAULT_MCP_REDIRECT_URI.to_string());
    client_config.redirect_uri = Some(redirect_uri.clone());
    redirect_uri
}

fn resolve_scopes(
    resolved: &ResolvedMcpAuth,
    client_config: &McpOAuthClientConfig,
) -> Result<Vec<String>> {
    if !client_config.scopes.is_empty() {
        return Ok(client_config.scopes.clone());
    }
    if !resolved.requirement.scopes.is_empty() {
        return Ok(resolved.requirement.scopes.clone());
    }
    if !resolved.metadata.scopes_supported.is_empty() {
        return Ok(resolved.metadata.scopes_supported.clone());
    }

    anyhow::bail!(
        "Could not determine OAuth scopes for MCP server '{}'. Configure scopes in .mcp.json under mcpServers.{}.oauth.scopes.",
        resolved.server.name,
        resolved.server.name
    )
}

fn print_auth_instructions(server_name: &str, auth_url: &str) {
    println!("To authenticate MCP server '{server_name}':");
    println!();
    println!("  1. A browser window will open (or visit the URL below)");
    println!("  2. Log in and authorize access for this MCP server");
    println!("  3. If redirected to localhost, return here to continue");
    println!("  4. Otherwise, paste the authorization code or redirect URL");
    println!();
    println!("Authorization URL:");
    println!("  {auth_url}");
    println!();
}

fn read_authorization_code(auth_url: &str, redirect_uri: &str, state: &str) -> Result<String> {
    if std::env::var("ZDX_NO_BROWSER").is_err() {
        let _ = open::that(auth_url);
    }

    if io::stdin().is_terminal()
        && let Some(code) =
            localhost_callback_target(redirect_uri).and_then(|(port, callback_path)| {
                wait_for_local_oauth_code(state, port, &callback_path)
            })
    {
        return Ok(code);
    }

    print!("Paste authorization code (or full redirect URL): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().lock().read_line(&mut input)?;
    let (code, provided_state) = zdx_core::mcp::parse_authorization_input(&input);
    if let Some(provided_state) = provided_state
        && provided_state != state
    {
        anyhow::bail!("State mismatch");
    }
    code.ok_or_else(|| anyhow!("Authorization code cannot be empty"))
}

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
    auth: Option<AuthRequirementOutput>,
}

#[derive(Serialize)]
struct DiagnosticOutput {
    kind: &'static str,
    level: &'static str,
    summary: String,
}

#[derive(Serialize)]
struct AuthRequirementOutput {
    resource: Option<String>,
    resource_name: Option<String>,
    resource_metadata_url: Option<String>,
    authorization_servers: Vec<String>,
    scopes: Vec<String>,
    resource_documentation: Option<String>,
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
            auth: None,
        },
        McpServerState::AuthRequired { requirement } => ServerOutput {
            name: status.name.clone(),
            transport: status.transport,
            status: "auth_required",
            tool_count: None,
            message: Some(format!(
                "{} Run `zdx mcp auth {}` to authenticate.",
                requirement.summary(&status.name),
                status.name
            )),
            auth: Some(auth_requirement_output(requirement)),
        },
        McpServerState::Failed { message } => ServerOutput {
            name: status.name.clone(),
            transport: status.transport,
            status: "failed",
            tool_count: None,
            message: Some(message.clone()),
            auth: None,
        },
    }
}

fn diagnostic_output(diagnostic: &McpDiagnostic) -> DiagnosticOutput {
    DiagnosticOutput {
        kind: diagnostic_kind(diagnostic),
        level: diagnostic_level(diagnostic),
        summary: diagnostic.summary(),
    }
}

fn diagnostic_level(diagnostic: &McpDiagnostic) -> &'static str {
    if diagnostic.is_error() {
        return "error";
    }
    match diagnostic {
        McpDiagnostic::ServerAuthRequired { .. } => "warn",
        _ => "info",
    }
}

fn diagnostic_kind(diagnostic: &McpDiagnostic) -> &'static str {
    match diagnostic {
        McpDiagnostic::ConfigLoaded { .. } => "config_loaded",
        McpDiagnostic::ConfigError { .. } => "config_error",
        McpDiagnostic::ServerLoaded { .. } => "server_loaded",
        McpDiagnostic::ServerAuthRequired { .. } => "server_auth_required",
        McpDiagnostic::ServerFailed { .. } => "server_failed",
    }
}

fn auth_requirement_output(requirement: &McpAuthRequirement) -> AuthRequirementOutput {
    AuthRequirementOutput {
        resource: requirement.resource.clone(),
        resource_name: requirement.resource_name.clone(),
        resource_metadata_url: requirement.resource_metadata_url.clone(),
        authorization_servers: requirement.authorization_servers.clone(),
        scopes: requirement.scopes.clone(),
        resource_documentation: requirement.resource_documentation.clone(),
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
            status: McpServerState::AuthRequired { requirement },
        }) => Err(anyhow!(
            "MCP server '{name}' requires authentication over {transport}: {}. Run `zdx mcp auth {name}` to authenticate.",
            requirement.summary(name)
        )),
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

fn localhost_callback_target(redirect_uri: &str) -> Option<(u16, String)> {
    let parsed = url::Url::parse(redirect_uri).ok()?;
    let host = parsed.host_str()?;
    if host != "127.0.0.1" && host != "localhost" {
        return None;
    }
    Some((parsed.port_or_known_default()?, parsed.path().to_string()))
}

fn wait_for_local_oauth_code(
    expected_state: &str,
    port: u16,
    callback_path: &str,
) -> Option<String> {
    let Ok(listener) = TcpListener::bind(format!("127.0.0.1:{port}")) else {
        return None;
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let expected_state = expected_state.to_string();
    let callback_path = callback_path.to_string();

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let code =
                        extract_oauth_code_from_request(&request, &expected_state, &callback_path);
                    let response = if code.is_some() {
                        oauth_success_response()
                    } else {
                        oauth_error_response()
                    };
                    let _ = stream.write_all(response.as_bytes());
                    let _ = tx.send(code);
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(120) {
                        let _ = tx.send(None);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => {
                    let _ = tx.send(None);
                    break;
                }
            }
        }
    });

    rx.recv_timeout(Duration::from_secs(120)).ok().flatten()
}

fn extract_oauth_code_from_request(
    request: &str,
    expected_state: &str,
    callback_path: &str,
) -> Option<String> {
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;

    let url = url::Url::parse(&format!("http://localhost{path}")).ok()?;
    if url.path() != callback_path {
        return None;
    }
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())?;
    if state != expected_state {
        return None;
    }
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
}

fn oauth_success_response() -> String {
    let body = "<!doctype html><html><head><meta charset=\"utf-8\" /><title>Authentication successful</title></head><body><p>Authentication successful. Return to your terminal to continue.</p></body></html>";
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn oauth_error_response() -> String {
    let body = "Invalid OAuth callback";
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("serialize MCP command output")?
    );
    Ok(())
}
