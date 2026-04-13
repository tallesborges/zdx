use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use futures_util::future::join_all;
use reqwest::StatusCode;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::core::events::ToolOutput;

pub const MCP_CONFIG_FILE_NAME: &str = ".mcp.json";

const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MCP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MCP_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

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
    AuthRequired { requirement: McpAuthRequirement },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthRequirement {
    pub resource: Option<String>,
    pub resource_name: Option<String>,
    pub resource_metadata_url: Option<String>,
    pub authorization_servers: Vec<String>,
    pub scopes: Vec<String>,
    pub resource_documentation: Option<String>,
}

impl McpAuthRequirement {
    #[must_use]
    pub fn summary(&self, server: &str) -> String {
        let mut summary = format!("MCP server '{server}' requires OAuth authentication");
        if !self.scopes.is_empty() {
            let scopes = self.scopes.join(", ");
            let _ = write!(&mut summary, " (scopes: {scopes})");
        }
        if let Some(resource_name) = &self.resource_name {
            let _ = write!(&mut summary, " for {resource_name}");
        }
        summary
    }
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
    ServerAuthRequired {
        server: String,
        transport: &'static str,
        requirement: McpAuthRequirement,
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
            McpDiagnostic::ServerAuthRequired {
                server,
                transport,
                requirement,
            } => format!("{} over {transport}", requirement.summary(server)),
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
struct ProtectedResourceMetadata {
    resource: Option<String>,
    authorization_servers: Vec<String>,
    scopes_supported: Vec<String>,
    resource_name: Option<String>,
    resource_documentation: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct WwwAuthenticateChallenge {
    resource_metadata: Option<String>,
    authorization_uri: Option<String>,
    scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTokenEndpointAuthMethod {
    #[default]
    None,
    ClientSecretPost,
    ClientSecretBasic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOAuthClientConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
    pub token_endpoint_auth_method: McpTokenEndpointAuthMethod,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpHttpServerConfig {
    pub name: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub oauth: Option<McpOAuthClientConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthorizationServerMetadata {
    pub issuer: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scopes_supported: Vec<String>,
    pub code_challenge_methods_supported: Vec<String>,
    pub token_endpoint_auth_methods_supported: Vec<String>,
    pub require_state_parameter: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthInspection {
    pub server_name: String,
    pub server_url: String,
    pub requirement: Option<McpAuthRequirement>,
    pub authorization_server: Option<String>,
    pub metadata: Option<McpAuthorizationServerMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOAuthCredentials {
    #[serde(rename = "type")]
    pub cred_type: String,
    pub access: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    pub expires: u64,
    pub resource: String,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    #[serde(default)]
    pub token_endpoint_auth_method: McpTokenEndpointAuthMethod,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_server: Option<String>,
}

impl McpOAuthCredentials {
    #[must_use]
    pub fn is_expired(&self) -> bool {
        now_millis_u64() >= self.expires
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct McpOAuthCache {
    #[serde(flatten)]
    pub servers: HashMap<String, McpOAuthCredentials>,
}

impl McpOAuthCache {
    pub fn cache_path() -> PathBuf {
        crate::config::paths::zdx_home().join("mcp_oauth.json")
    }

    ///
    /// # Errors
    /// Returns an error if the cache file exists but cannot be read or parsed.
    pub fn load() -> Result<Self> {
        let path = Self::cache_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read MCP OAuth cache from {}", path.display()))?;

        serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse MCP OAuth cache from {}", path.display()))
    }

    ///
    /// # Errors
    /// Returns an error if the cache cannot be serialized or written to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::cache_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        let contents =
            serde_json::to_string_pretty(self).context("Failed to serialize MCP OAuth cache")?;

        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt;

            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .with_context(|| format!("Failed to open {} for writing", path.display()))?;
            file.write_all(contents.as_bytes())
                .with_context(|| format!("Failed to write to {}", path.display()))?;
        }

        #[cfg(not(unix))]
        {
            std::fs::write(&path, contents)
                .with_context(|| format!("Failed to write to {}", path.display()))?;
        }

        Ok(())
    }

    pub fn get(&self, resource: &str) -> Option<&McpOAuthCredentials> {
        self.servers.get(resource)
    }

    pub fn set(&mut self, resource: &str, creds: McpOAuthCredentials) {
        self.servers.insert(resource.to_string(), creds);
    }

    pub fn remove(&mut self, resource: &str) -> Option<McpOAuthCredentials> {
        self.servers.remove(resource)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawMcpOAuthClientConfig {
    #[serde(rename = "clientId")]
    client_id: Option<String>,
    #[serde(rename = "clientSecret")]
    client_secret: Option<String>,
    #[serde(rename = "redirectUri")]
    redirect_uri: Option<String>,
    #[serde(rename = "tokenEndpointAuthMethod")]
    token_endpoint_auth_method: Option<McpTokenEndpointAuthMethod>,
    scopes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawAuthorizationServerMetadata {
    issuer: Option<String>,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
    require_state_parameter: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct DynamicClientRegistrationResponse {
    client_id: String,
    client_secret: Option<String>,
    token_endpoint_auth_method: Option<McpTokenEndpointAuthMethod>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpOAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
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
        oauth: Option<McpOAuthClientConfig>,
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
    oauth: Option<RawMcpOAuthClientConfig>,
}

impl TryFrom<RawMcpOAuthClientConfig> for McpOAuthClientConfig {
    type Error = de::value::Error;

    fn try_from(raw: RawMcpOAuthClientConfig) -> std::result::Result<Self, Self::Error> {
        let client_id = raw
            .client_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| de::Error::missing_field("oauth.clientId"))?;

        Ok(Self {
            client_id,
            client_secret: raw.client_secret.filter(|value| !value.trim().is_empty()),
            redirect_uri: raw.redirect_uri.filter(|value| !value.trim().is_empty()),
            token_endpoint_auth_method: raw.token_endpoint_auth_method.unwrap_or_default(),
            scopes: raw
                .scopes
                .into_iter()
                .map(|scope| scope.trim().to_string())
                .filter(|scope| !scope.is_empty())
                .collect(),
        })
    }
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
                    oauth: raw
                        .oauth
                        .map(McpOAuthClientConfig::try_from)
                        .transpose()
                        .map_err(de::Error::custom)?,
                })
            }
            Some(other) => Err(de::Error::unknown_variant(other, &["stdio", "http"])),
            None => {
                if let Some(url) = raw.url {
                    Ok(Self::Http {
                        url,
                        headers: raw.headers,
                        oauth: raw
                            .oauth
                            .map(McpOAuthClientConfig::try_from)
                            .transpose()
                            .map_err(de::Error::custom)?,
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

///
/// # Errors
/// Returns an error if the MCP config is missing, invalid, the server is unknown,
/// or the named server does not use HTTP transport.
pub fn load_http_server_config(root: &Path, server_name: &str) -> Result<McpHttpServerConfig> {
    let path = config_path(root);
    let config = load_config_file(&path)?;
    let Some(server) = config.mcp_servers.get(server_name) else {
        let available = config
            .mcp_servers
            .keys()
            .map(std::string::String::as_str)
            .collect::<Vec<_>>();
        if available.is_empty() {
            anyhow::bail!("No MCP servers are configured in {}.", path.display());
        }
        anyhow::bail!(
            "Unknown MCP server '{server_name}'. Available servers: {}",
            available.join(", ")
        );
    };

    match server {
        McpServerConfig::Http {
            url,
            headers,
            oauth,
        } => Ok(McpHttpServerConfig {
            name: server_name.to_string(),
            url: url.clone(),
            headers: headers.clone(),
            oauth: oauth.clone(),
        }),
        McpServerConfig::Stdio { .. } => {
            anyhow::bail!(
                "MCP server '{server_name}' uses stdio transport; OAuth auth is only supported for HTTP MCP servers"
            )
        }
    }
}

///
/// # Errors
/// Returns an error if auth requirement or authorization server discovery fails.
pub async fn inspect_http_auth(server: &McpHttpServerConfig) -> Result<McpAuthInspection> {
    let requirement = probe_http_auth_requirement(&server.url, &server.headers).await?;
    let authorization_server = requirement
        .as_ref()
        .and_then(|requirement| requirement.authorization_servers.first().cloned());
    let metadata = if let Some(authorization_server) = authorization_server.as_deref() {
        Some(fetch_authorization_server_metadata(authorization_server).await?)
    } else {
        None
    };

    Ok(McpAuthInspection {
        server_name: server.name.clone(),
        server_url: server.url.clone(),
        requirement,
        authorization_server,
        metadata,
    })
}

///
/// # Errors
/// Returns an error if the authorization server does not support registration or rejects the request.
pub async fn register_dynamic_client(
    metadata: &McpAuthorizationServerMetadata,
    redirect_uri: &str,
) -> Result<McpOAuthClientConfig> {
    let registration_endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
        anyhow!("Authorization server does not advertise a registration endpoint")
    })?;

    let response = reqwest::Client::new()
        .post(registration_endpoint)
        .header("Content-Type", "application/json")
        .json(&json!({
            "client_name": "zdx",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .with_context(|| format!("register OAuth client at {registration_endpoint}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Dynamic client registration failed (HTTP {status}): {body}");
    }

    let registered: DynamicClientRegistrationResponse = response
        .json()
        .await
        .context("parse dynamic client registration response")?;

    Ok(McpOAuthClientConfig {
        client_id: registered.client_id,
        client_secret: registered.client_secret,
        redirect_uri: Some(redirect_uri.to_string()),
        token_endpoint_auth_method: registered.token_endpoint_auth_method.unwrap_or_default(),
        scopes: Vec::new(),
    })
}

///
/// # Errors
/// Returns an error if the token exchange request fails or returns an invalid response.
pub async fn exchange_oauth_code(
    metadata: &McpAuthorizationServerMetadata,
    client: &McpOAuthClientConfig,
    resource: &str,
    code: &str,
    pkce_verifier: &str,
    redirect_uri: &str,
    scopes: &[String],
) -> Result<McpOAuthCredentials> {
    let request = build_token_request(
        &metadata.token_endpoint,
        client,
        &json!({
            "grant_type": "authorization_code",
            "code": code,
            "code_verifier": pkce_verifier,
            "redirect_uri": redirect_uri,
            "resource": resource,
        }),
    )?;
    let response = request
        .send()
        .await
        .with_context(|| format!("exchange OAuth code at {}", metadata.token_endpoint))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OAuth token exchange failed (HTTP {status}): {body}");
    }

    let token_data: McpOAuthTokenResponse = response
        .json()
        .await
        .context("parse OAuth token exchange response")?;
    Ok(build_mcp_oauth_credentials(
        metadata,
        client,
        resource,
        redirect_uri,
        scopes,
        token_data,
    ))
}

///
/// # Errors
/// Returns an error if no refresh token is available or the refresh request fails.
pub async fn refresh_oauth_credentials(creds: &McpOAuthCredentials) -> Result<McpOAuthCredentials> {
    let refresh_token = creds.refresh.as_deref().ok_or_else(|| {
        anyhow!(
            "No refresh token available for MCP server {}",
            creds.resource
        )
    })?;
    let client_config = McpOAuthClientConfig {
        client_id: creds.client_id.clone(),
        client_secret: creds.client_secret.clone(),
        redirect_uri: Some(creds.redirect_uri.clone()),
        token_endpoint_auth_method: creds.token_endpoint_auth_method.clone(),
        scopes: creds.scopes.clone(),
    };
    let metadata = McpAuthorizationServerMetadata {
        issuer: None,
        authorization_endpoint: creds.authorization_endpoint.clone().unwrap_or_default(),
        token_endpoint: creds.token_endpoint.clone(),
        registration_endpoint: None,
        scopes_supported: creds.scopes.clone(),
        code_challenge_methods_supported: vec!["S256".to_string()],
        token_endpoint_auth_methods_supported: Vec::new(),
        require_state_parameter: true,
    };

    let request = build_token_request(
        &creds.token_endpoint,
        &client_config,
        &json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "resource": creds.resource,
        }),
    )?;
    let response = request
        .send()
        .await
        .with_context(|| format!("refresh OAuth token at {}", creds.token_endpoint))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OAuth token refresh failed (HTTP {status}): {body}");
    }

    let token_data: McpOAuthTokenResponse = response
        .json()
        .await
        .context("parse OAuth token refresh response")?;
    let mut refreshed = build_mcp_oauth_credentials(
        &metadata,
        &client_config,
        &creds.resource,
        &creds.redirect_uri,
        &creds.scopes,
        token_data,
    );
    if refreshed.authorization_server.is_none() {
        refreshed
            .authorization_server
            .clone_from(&creds.authorization_server);
    }
    Ok(refreshed)
}

///
/// # Errors
/// Returns an error if the OAuth cache cannot be loaded or written.
pub async fn clear_http_auth_credentials(server: &McpHttpServerConfig) -> Result<bool> {
    let mut cache = McpOAuthCache::load()?;
    let mut removed = false;

    if let Some(metadata) = fetch_protected_resource_metadata(&server.url, None).await
        && let Some(resource) = metadata.resource
    {
        removed |= cache.remove(&resource).is_some();
    }
    removed |= cache.remove(&server.url).is_some();

    if removed {
        cache.save()?;
    }

    Ok(removed)
}

pub fn build_oauth_authorization_url(
    metadata: &McpAuthorizationServerMetadata,
    client: &McpOAuthClientConfig,
    resource: &str,
    redirect_uri: &str,
    scope: &[String],
    state: &str,
    code_challenge: &str,
) -> String {
    let scope_value = scope.join(" ");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scope_value)
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", resource)
        .finish();

    format!("{}?{query}", metadata.authorization_endpoint)
}

pub fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }

    if let Ok(url) = url::Url::parse(value) {
        let code = url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v);
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v);
        return (code.map(|v| v.to_string()), state.map(|v| v.to_string()));
    }

    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_string()), Some(state.to_string()));
    }

    if value.contains("code=") {
        let params = url::form_urlencoded::parse(value.as_bytes()).collect::<Vec<_>>();
        let code = params.iter().find(|(k, _)| k == "code").map(|(_, v)| v);
        let state = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v);
        return (
            code.map(std::string::ToString::to_string),
            state.map(std::string::ToString::to_string),
        );
    }

    (Some(value.to_string()), None)
}

pub fn generate_pkce() -> (String, String) {
    let verifier_bytes = [
        uuid::Uuid::new_v4().into_bytes(),
        uuid::Uuid::new_v4().into_bytes(),
    ]
    .concat();
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
    (verifier, challenge)
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
                    McpServerConfig::Http { url, headers, .. } => {
                        match discover_http_tools(&server_name, &url, &headers).await {
                            Ok(server) => DiscoveryOutcome::Loaded {
                                server_name,
                                transport: "http",
                                server,
                            },
                            Err(error) => match probe_http_auth_requirement(&url, &headers).await {
                                Ok(Some(requirement)) => DiscoveryOutcome::AuthRequired {
                                    server_name,
                                    transport: "http",
                                    requirement,
                                },
                                Ok(None) => DiscoveryOutcome::Failed {
                                    server_name,
                                    transport: "http",
                                    message: error.to_string(),
                                },
                                Err(probe_error) => DiscoveryOutcome::Failed {
                                    server_name,
                                    transport: "http",
                                    message: format!(
                                        "{error}; additionally failed to inspect authentication requirements: {probe_error}"
                                    ),
                                },
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
        DiscoveryOutcome::AuthRequired {
            server_name,
            transport,
            requirement,
        } => {
            workspace.server_statuses.push(McpServerStatus {
                name: server_name.clone(),
                transport,
                status: McpServerState::AuthRequired {
                    requirement: requirement.clone(),
                },
            });
            workspace
                .diagnostics
                .push(McpDiagnostic::ServerAuthRequired {
                    server: server_name,
                    transport,
                    requirement,
                });
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
    AuthRequired {
        server_name: String,
        transport: &'static str,
        requirement: McpAuthRequirement,
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
    let resolved_headers = resolve_http_headers(url, headers).await?;
    let header_map = build_http_headers(&resolved_headers)
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

async fn resolve_http_headers(
    endpoint_url: &str,
    headers: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut resolved = headers.clone();
    if resolved
        .keys()
        .any(|name| name.eq_ignore_ascii_case("authorization"))
    {
        return Ok(resolved);
    }

    if let Some(credentials) = load_cached_oauth_credentials(endpoint_url).await? {
        resolved.insert(
            "authorization".to_string(),
            format!("Bearer {}", credentials.access),
        );
    }

    Ok(resolved)
}

async fn load_cached_oauth_credentials(endpoint_url: &str) -> Result<Option<McpOAuthCredentials>> {
    let mut cache = McpOAuthCache::load()?;
    let mut candidates = Vec::new();
    if let Some(metadata) = fetch_protected_resource_metadata(endpoint_url, None).await
        && let Some(resource) = metadata.resource
    {
        candidates.push(resource);
    }
    candidates.push(endpoint_url.to_string());

    for candidate in candidates {
        if let Some(credentials) = cache.get(&candidate).cloned() {
            if credentials.is_expired() {
                match refresh_oauth_credentials(&credentials).await {
                    Ok(refreshed) => {
                        cache.set(&candidate, refreshed.clone());
                        cache.save()?;
                        return Ok(Some(refreshed));
                    }
                    Err(error) => {
                        tracing::warn!(
                            resource = candidate,
                            error = %error,
                            "Failed to refresh cached MCP OAuth credentials; clearing stale entry"
                        );
                        cache.remove(&candidate);
                        cache.save()?;
                        return Ok(None);
                    }
                }
            }
            return Ok(Some(credentials));
        }
    }

    Ok(None)
}

async fn probe_http_auth_requirement(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<Option<McpAuthRequirement>> {
    let client = reqwest::Client::new();
    let mut request = client
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "zdx",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        }));

    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request
        .send()
        .await
        .context("send MCP auth probe request")?;
    if response.status() != StatusCode::UNAUTHORIZED {
        return Ok(None);
    }

    let response_headers = response.headers().clone();
    let challenge = parse_www_authenticate_header(&response_headers);
    let resource_metadata_url = challenge
        .as_ref()
        .and_then(|challenge| challenge.resource_metadata.clone());

    let metadata = fetch_protected_resource_metadata(url, resource_metadata_url.as_deref()).await;
    let authorization_servers = metadata
        .as_ref()
        .map(|metadata| metadata.authorization_servers.clone())
        .filter(|servers| !servers.is_empty())
        .or_else(|| {
            challenge
                .as_ref()
                .and_then(|challenge| challenge.authorization_uri.clone())
                .map(|authorization_uri| vec![authorization_uri])
        })
        .unwrap_or_default();
    let scopes = metadata
        .as_ref()
        .map(|metadata| metadata.scopes_supported.clone())
        .filter(|scopes| !scopes.is_empty())
        .or_else(|| {
            challenge
                .as_ref()
                .and_then(|challenge| challenge.scope.as_deref())
                .map(parse_scope_list)
        })
        .unwrap_or_default();

    Ok(Some(McpAuthRequirement {
        resource: metadata
            .as_ref()
            .and_then(|metadata| metadata.resource.clone()),
        resource_name: metadata
            .as_ref()
            .and_then(|metadata| metadata.resource_name.clone()),
        resource_metadata_url,
        authorization_servers,
        scopes,
        resource_documentation: metadata
            .as_ref()
            .and_then(|metadata| metadata.resource_documentation.clone()),
    }))
}

async fn fetch_protected_resource_metadata(
    endpoint_url: &str,
    preferred_url: Option<&str>,
) -> Option<ProtectedResourceMetadata> {
    let client = reqwest::Client::new();
    let mut candidates = Vec::new();
    if let Some(preferred_url) = preferred_url {
        candidates.push(preferred_url.to_string());
    }
    for candidate in protected_resource_metadata_candidates(endpoint_url) {
        if !candidates.iter().any(|existing| existing == &candidate) {
            candidates.push(candidate);
        }
    }

    for candidate in candidates {
        let Ok(response) = client.get(&candidate).send().await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        if let Ok(metadata) = response.json::<ProtectedResourceMetadata>().await {
            return Some(metadata);
        }
    }

    None
}

fn protected_resource_metadata_candidates(endpoint_url: &str) -> Vec<String> {
    let Ok(mut parsed) = url::Url::parse(endpoint_url) else {
        return Vec::new();
    };

    let original_path = parsed.path().trim_end_matches('/');
    if original_path.is_empty() {
        parsed.set_path("/.well-known/oauth-protected-resource");
        return vec![parsed.to_string()];
    }

    parsed.set_path(&format!(
        "/.well-known/oauth-protected-resource{original_path}"
    ));
    let path_candidate = parsed.to_string();
    parsed.set_path("/.well-known/oauth-protected-resource");
    let root_candidate = parsed.to_string();
    vec![path_candidate, root_candidate]
}

fn parse_scope_list(scope: &str) -> Vec<String> {
    scope
        .split_whitespace()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

fn parse_www_authenticate_header(
    headers: &reqwest::header::HeaderMap,
) -> Option<WwwAuthenticateChallenge> {
    headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(parse_www_authenticate_value)
}

fn parse_www_authenticate_value(value: &str) -> Option<WwwAuthenticateChallenge> {
    let (scheme, params) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }

    let mut challenge = WwwAuthenticateChallenge::default();
    for part in split_auth_header_params(params) {
        let (name, raw_value) = part.split_once('=')?;
        let name = name.trim();
        let value = raw_value.trim().trim_matches('"');
        match name {
            "resource_metadata" => challenge.resource_metadata = Some(value.to_string()),
            "authorization_uri" => challenge.authorization_uri = Some(value.to_string()),
            "scope" => challenge.scope = Some(value.to_string()),
            _ => {}
        }
    }

    Some(challenge)
}

fn split_auth_header_params(params: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;

    for (idx, ch) in params.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                let part = params[start..idx].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = idx + 1;
            }
            _ => {}
        }
    }

    let tail = params[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }

    parts
}

async fn fetch_authorization_server_metadata(
    authorization_server: &str,
) -> Result<McpAuthorizationServerMetadata> {
    let metadata_url = format!(
        "{}/.well-known/oauth-authorization-server",
        authorization_server.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .get(&metadata_url)
        .send()
        .await
        .with_context(|| format!("fetch authorization server metadata from {metadata_url}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Authorization server metadata request failed (HTTP {status}): {body}");
    }

    let raw: RawAuthorizationServerMetadata = response
        .json()
        .await
        .context("parse authorization server metadata response")?;
    Ok(McpAuthorizationServerMetadata {
        issuer: raw.issuer,
        authorization_endpoint: raw.authorization_endpoint,
        token_endpoint: raw.token_endpoint,
        registration_endpoint: raw.registration_endpoint,
        scopes_supported: raw.scopes_supported,
        code_challenge_methods_supported: raw.code_challenge_methods_supported,
        token_endpoint_auth_methods_supported: raw.token_endpoint_auth_methods_supported,
        require_state_parameter: raw.require_state_parameter.unwrap_or(true),
    })
}

fn build_token_request(
    token_endpoint: &str,
    client: &McpOAuthClientConfig,
    params: &Value,
) -> Result<reqwest::RequestBuilder> {
    let mut form = params
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("OAuth token request params must be a JSON object"))?;
    form.insert(
        "client_id".to_string(),
        Value::String(client.client_id.clone()),
    );

    if matches!(
        client.token_endpoint_auth_method,
        McpTokenEndpointAuthMethod::ClientSecretPost
    ) && let Some(client_secret) = &client.client_secret
    {
        form.insert(
            "client_secret".to_string(),
            Value::String(client_secret.clone()),
        );
    }

    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in form {
        match value {
            Value::Null => {}
            Value::String(value) => {
                serializer.append_pair(&name, &value);
            }
            other => {
                serializer.append_pair(&name, &other.to_string());
            }
        }
    }
    let body = serializer.finish();

    let client_builder = reqwest::Client::new().post(token_endpoint);
    let client_builder = if matches!(
        client.token_endpoint_auth_method,
        McpTokenEndpointAuthMethod::ClientSecretBasic
    ) {
        let client_secret = client.client_secret.as_deref().ok_or_else(|| {
            anyhow!("token_endpoint_auth_method=client_secret_basic requires client_secret")
        })?;
        client_builder.basic_auth(&client.client_id, Some(client_secret))
    } else {
        client_builder
    };

    Ok(client_builder
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body))
}

fn build_mcp_oauth_credentials(
    metadata: &McpAuthorizationServerMetadata,
    client: &McpOAuthClientConfig,
    resource: &str,
    redirect_uri: &str,
    scopes: &[String],
    token_data: McpOAuthTokenResponse,
) -> McpOAuthCredentials {
    let expires_at = now_millis_u64() + token_data.expires_in.unwrap_or(3600) * 1000;
    McpOAuthCredentials {
        cred_type: "oauth".to_string(),
        access: token_data.access_token,
        refresh: token_data.refresh_token,
        expires: expires_at.saturating_sub(5 * 60 * 1000),
        resource: resource.to_string(),
        token_endpoint: metadata.token_endpoint.clone(),
        client_id: client.client_id.clone(),
        client_secret: client.client_secret.clone(),
        redirect_uri: redirect_uri.to_string(),
        token_endpoint_auth_method: client.token_endpoint_auth_method.clone(),
        scopes: scopes.to_vec(),
        authorization_endpoint: Some(metadata.authorization_endpoint.clone()),
        authorization_server: metadata.issuer.clone(),
    }
}

fn now_millis_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
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
            Some(McpServerConfig::Http { url, headers, oauth })
                if url == "https://mcp.figma.com/mcp" && headers.get("authorization") == Some(&"Bearer token".to_string()) && oauth.is_none()
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
            Some(McpServerConfig::Http { url, headers, oauth })
                if url == "https://mcp.deepwiki.com/mcp" && headers.is_empty() && oauth.is_none()
        ));
    }

    #[test]
    fn parses_http_server_with_oauth_client_config() {
        let config: McpConfigFile = serde_json::from_str(
            r#"{
                "mcpServers": {
                    "figma": {
                        "url": "https://mcp.figma.com/mcp",
                        "oauth": {
                            "clientId": "client-123",
                            "redirectUri": "http://127.0.0.1:8787/callback",
                            "tokenEndpointAuthMethod": "none",
                            "scopes": ["mcp:connect"]
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(matches!(
            config.mcp_servers.get("figma"),
            Some(McpServerConfig::Http { oauth: Some(oauth), .. })
                if oauth.client_id == "client-123"
                    && oauth.redirect_uri.as_deref() == Some("http://127.0.0.1:8787/callback")
                    && oauth.scopes == vec!["mcp:connect".to_string()]
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

    #[test]
    fn parses_www_authenticate_bearer_challenge() {
        let challenge = parse_www_authenticate_value(
            "Bearer resource_metadata=\"https://mcp.figma.com/.well-known/oauth-protected-resource\",scope=\"mcp:connect\",authorization_uri=\"https://api.figma.com/.well-known/oauth-authorization-server\"",
        )
        .unwrap();

        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://mcp.figma.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(challenge.scope.as_deref(), Some("mcp:connect"));
        assert_eq!(
            challenge.authorization_uri.as_deref(),
            Some("https://api.figma.com/.well-known/oauth-authorization-server")
        );
    }

    #[test]
    fn builds_protected_resource_metadata_fallback_candidates() {
        let candidates =
            protected_resource_metadata_candidates("https://mcp.example.com/public/mcp");

        assert_eq!(
            candidates,
            vec![
                "https://mcp.example.com/.well-known/oauth-protected-resource/public/mcp"
                    .to_string(),
                "https://mcp.example.com/.well-known/oauth-protected-resource".to_string(),
            ]
        );
    }

    #[test]
    fn parse_authorization_input_extracts_code_and_state_from_url() {
        let (code, state) =
            parse_authorization_input("http://127.0.0.1:8787/callback?code=demo&state=demo-state");

        assert_eq!(code.as_deref(), Some("demo"));
        assert_eq!(state.as_deref(), Some("demo-state"));
    }
}
