use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};
use std::{fs, thread};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn test_config_path_command() {
    let dir = tempdir().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("config.toml"));
}

#[test]
fn test_config_init_creates_file() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    assert!(!config_path.exists());

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["config", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created config at"));

    assert!(config_path.exists());

    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("model ="));
    assert!(contents.contains("# max_tokens ="));
}

#[test]
fn test_config_init_fails_if_exists() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    fs::write(&config_path, "# existing config").unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["config", "init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn test_config_help_shows_subcommands() {
    cargo_bin_cmd!("zdx")
        .args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("path"))
        .stdout(predicate::str::contains("init"));
}

#[test]
fn test_bot_init_creates_named_bot_in_zdx_home() {
    let zdx_home = tempdir().unwrap();
    let root = tempdir().unwrap();
    let bots_path = zdx_home.path().join("bots.toml");

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "bot",
            "init",
            "--name",
            "zdx",
            "--bot-token",
            "123456:abc",
            "--user-id",
            "42",
            "--chat-id",
            "-1009876543210",
            "--model",
            "claude-cli:claude-sonnet-4-6",
            "--thinking",
            "high",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved bot 'zdx'"));

    assert!(bots_path.exists());
    let contents = fs::read_to_string(&bots_path).unwrap();
    assert!(contents.contains("[bots.zdx]"));
    assert!(contents.contains("bot_token = \"123456:abc\""));
    assert!(contents.contains("allowlist_user_ids = [42]"));
    assert!(contents.contains("allowlist_chat_ids = [-1009876543210]"));
    assert!(contents.contains("thinking_level = \"high\""));
    let parsed: toml::Value = toml::from_str(&contents).unwrap();
    let canonical_root = root.path().canonicalize().unwrap().display().to_string();
    assert_eq!(parsed["bots"]["zdx"]["root"].as_str(), Some(canonical_root.as_str()));
}

#[test]
fn test_bot_command_fails_when_bot_name_is_missing() {
    let zdx_home = tempdir().unwrap();
    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .args(["bot"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bot name is required"));
}

#[test]
fn test_bot_command_fails_when_named_bot_is_missing() {
    let zdx_home = tempdir().unwrap();
    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .args(["bot", "--bot", "missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No bot named 'missing'"));
}

#[test]
fn test_telegram_command_requires_bot_name() {
    let zdx_home = tempdir().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", zdx_home.path())
        .args([
            "telegram",
            "create-topic",
            "--chat-id",
            "-100123",
            "--name",
            "Test Topic",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--bot <NAME>"));
}

#[test]
fn test_automations_list_empty() {
    let dir = tempdir().unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["automations", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No automations found."));
}

#[test]
fn test_automations_validate_single_file() {
    let user_home = tempdir().unwrap();
    let automations_dir = user_home.path().join("automations");
    fs::create_dir_all(&automations_dir).unwrap();
    fs::write(
        automations_dir.join("morning-report.md"),
        "---\nschedule: \"0 8 * * *\"\n---\nGenerate morning report.",
    )
    .unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", user_home.path())
        .args(["automations", "validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validated 1 automation(s)."))
        .stdout(predicate::str::contains("morning-report"));
}

#[test]
fn test_automations_validate_fails_for_missing_subagent() {
    let user_home = tempdir().unwrap();
    let automations_dir = user_home.path().join("automations");
    fs::create_dir_all(&automations_dir).unwrap();
    fs::write(
        automations_dir.join("morning-report.md"),
        "---\nsubagent: missing-subagent\n---\nGenerate morning report.",
    )
    .unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", user_home.path())
        .args(["automations", "validate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing-subagent"));
}

#[test]
fn test_automations_runs_reads_jsonl_log() {
    let dir = tempdir().unwrap();
    let runs_path = dir.path().join("automations_runs.jsonl");

    fs::write(
        &runs_path,
        concat!(
            r#"{"automation":"morning-report","trigger":"manual","attempt":1,"max_attempts":1,"started_at":"2026-02-11T08:00:00Z","finished_at":"2026-02-11T08:00:01Z","duration_ms":1000,"ok":true,"error":null,"schedule":"0 8 * * *","model":"gemini-cli:gemini-2.5-flash"}"#,
            "\n"
        ),
    )
    .unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", dir.path())
        .args(["automations", "runs", "morning-report"])
        .assert()
        .success()
        .stdout(predicate::str::contains("morning-report"))
        .stdout(predicate::str::contains("manual"))
        .stdout(predicate::str::contains("ok"));
}

#[test]
fn test_mcp_servers_reports_missing_config() {
    let root = tempdir().unwrap();

    let output = cargo_bin_cmd!("zdx")
        .args(["--root", root.path().to_str().unwrap(), "mcp", "servers"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["config_exists"], Value::Bool(false));
    assert_eq!(parsed["servers"], Value::Array(Vec::new()));
}

#[test]
fn test_mcp_servers_reports_invalid_config() {
    let root = tempdir().unwrap();
    fs::write(root.path().join(".mcp.json"), "not json").unwrap();

    let output = cargo_bin_cmd!("zdx")
        .args(["--root", root.path().to_str().unwrap(), "mcp", "servers"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["config_exists"], Value::Bool(true));
    assert_eq!(parsed["servers"], Value::Array(Vec::new()));
    assert_eq!(
        parsed["diagnostics"][0]["kind"],
        Value::String("config_error".to_string())
    );
}

#[test]
fn test_mcp_call_rejects_invalid_json() {
    let root = tempdir().unwrap();

    cargo_bin_cmd!("zdx")
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "mcp",
            "call",
            "demo",
            "tool",
            "--json",
            "not-json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("parse --json input"));
}

#[test]
fn test_mcp_servers_reports_auth_required_http_server() {
    let root = tempdir().unwrap();
    let (url, handle) = spawn_auth_required_mcp_server();

    fs::write(
        root.path().join(".mcp.json"),
        format!(
            r#"{{
                "mcpServers": {{
                    "figma": {{
                        "url": "{url}"
                    }}
                }}
            }}"#
        ),
    )
    .unwrap();

    let output = cargo_bin_cmd!("zdx")
        .args(["--root", root.path().to_str().unwrap(), "mcp", "servers"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    handle.join().unwrap();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        parsed["servers"][0]["status"],
        Value::String("auth_required".to_string())
    );
    assert_eq!(
        parsed["diagnostics"][1]["kind"],
        Value::String("server_auth_required".to_string())
    );
    assert_eq!(
        parsed["servers"][0]["auth"]["scopes"][0],
        Value::String("mcp:connect".to_string())
    );
}

#[test]
fn test_mcp_auth_reports_dcr_failure_with_guidance() {
    let root = tempdir().unwrap();
    let (url, handle) = spawn_dcr_forbidden_mcp_auth_server();

    fs::write(
        root.path().join(".mcp.json"),
        format!(
            r#"{{
                "mcpServers": {{
                    "figma": {{
                        "url": "{url}"
                    }}
                }}
            }}"#
        ),
    )
    .unwrap();

    cargo_bin_cmd!("zdx")
        .env("ZDX_NO_BROWSER", "1")
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "mcp",
            "auth",
            "figma",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Dynamic client registration failed",
        ))
        .stderr(predicate::str::contains("mcpServers.figma.oauth"));

    handle.join().unwrap();
}

#[test]
fn test_mcp_tools_uses_cached_oauth_credentials_for_http_server() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let (url, handle) = spawn_authenticated_mcp_server("demo-access-token");

    fs::write(
        root.path().join(".mcp.json"),
        format!(
            r#"{{
                "mcpServers": {{
                    "figma": {{
                        "url": "{url}"
                    }}
                }}
            }}"#
        ),
    )
    .unwrap();

    fs::write(
        home.path().join("mcp_oauth.json"),
        format!(
            r#"{{"{url}":{{"type":"oauth","access":"demo-access-token","refresh":"demo-refresh-token","expires":9999999999999,"resource":"{url}","token_endpoint":"{url}/token","client_id":"client-123","redirect_uri":"http://127.0.0.1:8787/callback","token_endpoint_auth_method":"none","scopes":["mcp:connect"],"authorization_endpoint":"{url}/authorize","authorization_server":"{url}"}}}}"#
        ),
    )
    .unwrap();

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", home.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "mcp",
            "tools",
            "figma",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    handle.join().unwrap();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["server"], Value::String("figma".to_string()));
    assert_eq!(
        parsed["tools"][0]["name"],
        Value::String("whoami".to_string())
    );
}

#[test]
fn test_mcp_call_uses_cached_oauth_credentials_for_http_server() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let (url, handle) = spawn_authenticated_mcp_server("demo-access-token");

    fs::write(
        root.path().join(".mcp.json"),
        format!(
            r#"{{
                "mcpServers": {{
                    "figma": {{
                        "url": "{url}"
                    }}
                }}
            }}"#
        ),
    )
    .unwrap();

    fs::write(
        home.path().join("mcp_oauth.json"),
        format!(
            r#"{{"{url}":{{"type":"oauth","access":"demo-access-token","refresh":"demo-refresh-token","expires":9999999999999,"resource":"{url}","token_endpoint":"{url}/token","client_id":"client-123","redirect_uri":"http://127.0.0.1:8787/callback","token_endpoint_auth_method":"none","scopes":["mcp:connect"],"authorization_endpoint":"{url}/authorize","authorization_server":"{url}"}}}}"#
        ),
    )
    .unwrap();

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", home.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "mcp",
            "call",
            "figma",
            "whoami",
            "--json",
            "{}",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    handle.join().unwrap();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["ok"], Value::Bool(true));
    assert_eq!(
        parsed["data"]["result"]["content"][0]["text"],
        Value::String("authenticated".to_string())
    );
}

#[test]
fn test_mcp_tools_refreshes_expired_cached_oauth_credentials() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let (url, base_url, handle) =
        spawn_refreshing_authenticated_mcp_server("fresh-access-token", "demo-refresh-token");

    fs::write(
        root.path().join(".mcp.json"),
        format!(
            r#"{{
                "mcpServers": {{
                    "figma": {{
                        "url": "{url}"
                    }}
                }}
            }}"#
        ),
    )
    .unwrap();

    write_mcp_oauth_cache(
        home.path(),
        &url,
        &format!("{base_url}/token"),
        "stale-access-token",
        0,
    );

    let output = cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", home.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "mcp",
            "tools",
            "figma",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    handle.join().unwrap();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        parsed["tools"][0]["name"],
        Value::String("whoami".to_string())
    );

    let cache_contents = fs::read_to_string(home.path().join("mcp_oauth.json")).unwrap();
    assert!(cache_contents.contains("fresh-access-token"));
    assert!(!cache_contents.contains("stale-access-token"));
}

#[test]
fn test_mcp_logout_clears_cached_oauth_credentials() {
    let root = tempdir().unwrap();
    let home = tempdir().unwrap();
    let (url, handle) = spawn_dcr_forbidden_mcp_auth_server();

    fs::write(
        root.path().join(".mcp.json"),
        format!(
            r#"{{
                "mcpServers": {{
                    "figma": {{
                        "url": "{url}"
                    }}
                }}
            }}"#
        ),
    )
    .unwrap();

    write_mcp_oauth_cache(
        home.path(),
        &url,
        &format!("{url}/token"),
        "demo-access-token",
        9_999_999_999_999,
    );

    cargo_bin_cmd!("zdx")
        .env("ZDX_HOME", home.path())
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "mcp",
            "logout",
            "figma",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cleared cached MCP OAuth credentials",
        ));

    handle.join().unwrap();

    let cache_contents = fs::read_to_string(home.path().join("mcp_oauth.json")).unwrap();
    assert!(!cache_contents.contains("demo-access-token"));
}

fn spawn_auth_required_mcp_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());
    let metadata_json = format!(
        r#"{{"resource":"{base_url}/mcp","authorization_servers":["{base_url}/oauth"],"scopes_supported":["mcp:connect"],"resource_name":"Demo MCP","resource_documentation":"{base_url}/docs"}}"#
    );
    let challenge = format!(
        "Bearer resource_metadata=\"{base_url}/.well-known/oauth-protected-resource\",scope=\"mcp:connect\",authorization_uri=\"{base_url}/oauth\""
    );

    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut saw_request = false;
        let mut idle_since = Instant::now();

        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    saw_request = true;
                    idle_since = Instant::now();

                    let mut buffer = [0u8; 4096];
                    let bytes = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..bytes]);
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/");

                    if path == "/.well-known/oauth-protected-resource" {
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            metadata_json.len(),
                            metadata_json
                        );
                        let _ = stream.write_all(response.as_bytes());
                    } else {
                        let body = "Unauthorized";
                        let response = format!(
                            "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nWWW-Authenticate: {challenge}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if saw_request && idle_since.elapsed() > Duration::from_millis(500) {
                        break;
                    }
                    if !saw_request && idle_since.elapsed() > Duration::from_secs(5) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    (format!("{base_url}/mcp"), handle)
}

fn spawn_dcr_forbidden_mcp_auth_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());
    let resource_metadata = format!(
        r#"{{"resource":"{base_url}/mcp","authorization_servers":["{base_url}"],"scopes_supported":["mcp:connect"],"resource_name":"Demo MCP"}}"#
    );
    let auth_metadata = format!(
        r#"{{"issuer":"{base_url}","authorization_endpoint":"{base_url}/authorize","token_endpoint":"{base_url}/token","registration_endpoint":"{base_url}/register","grant_types_supported":["authorization_code","refresh_token"],"response_types_supported":["code"],"code_challenge_methods_supported":["S256"],"scopes_supported":["mcp:connect"],"require_state_parameter":true}}"#
    );
    let challenge = format!(
        "Bearer resource_metadata=\"{base_url}/.well-known/oauth-protected-resource\",scope=\"mcp:connect\",authorization_uri=\"{base_url}/.well-known/oauth-authorization-server\""
    );

    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut saw_request = false;
        let mut idle_since = Instant::now();

        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    saw_request = true;
                    idle_since = Instant::now();

                    let mut buffer = [0u8; 4096];
                    let bytes = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..bytes]);
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/");

                    let (status, content_type, body, extra_headers) = match path {
                        "/.well-known/oauth-protected-resource" => (
                            "200 OK",
                            "application/json",
                            resource_metadata.as_str(),
                            String::new(),
                        ),
                        "/.well-known/oauth-authorization-server" => (
                            "200 OK",
                            "application/json",
                            auth_metadata.as_str(),
                            String::new(),
                        ),
                        "/register" => ("403 Forbidden", "text/plain", "Forbidden", String::new()),
                        _ => (
                            "401 Unauthorized",
                            "text/plain",
                            "Unauthorized",
                            format!("WWW-Authenticate: {challenge}\r\n"),
                        ),
                    };

                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if saw_request && idle_since.elapsed() > Duration::from_millis(500) {
                        break;
                    }
                    if !saw_request && idle_since.elapsed() > Duration::from_secs(5) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    (format!("{base_url}/mcp"), handle)
}

fn spawn_authenticated_mcp_server(access_token: &str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());
    let resource_metadata = format!(
        r#"{{"resource":"{base_url}/mcp","authorization_servers":["{base_url}"],"scopes_supported":["mcp:connect"],"resource_name":"Demo MCP"}}"#
    );
    let challenge = format!(
        "Bearer resource_metadata=\"{base_url}/.well-known/oauth-protected-resource\",scope=\"mcp:connect\",authorization_uri=\"{base_url}/.well-known/oauth-authorization-server\""
    );
    let access_token = access_token.to_string();

    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut saw_request = false;
        let mut idle_since = Instant::now();

        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    saw_request = true;
                    idle_since = Instant::now();

                    let mut buffer = [0u8; 8192];
                    let bytes = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..bytes]);
                    let response = authenticated_mcp_server_response(
                        &request,
                        &resource_metadata,
                        &challenge,
                        &access_token,
                    );

                    let _ = stream.write_all(response.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if saw_request && idle_since.elapsed() > Duration::from_millis(500) {
                        break;
                    }
                    if !saw_request && idle_since.elapsed() > Duration::from_secs(5) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    (format!("{base_url}/mcp"), handle)
}

fn spawn_refreshing_authenticated_mcp_server(
    refreshed_access_token: &str,
    refresh_token: &str,
) -> (String, String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());
    let resource_metadata = format!(
        r#"{{"resource":"{base_url}/mcp","authorization_servers":["{base_url}"],"scopes_supported":["mcp:connect"],"resource_name":"Demo MCP"}}"#
    );
    let challenge = format!(
        "Bearer resource_metadata=\"{base_url}/.well-known/oauth-protected-resource\",scope=\"mcp:connect\",authorization_uri=\"{base_url}/.well-known/oauth-authorization-server\""
    );
    let refreshed_access_token = refreshed_access_token.to_string();
    let refresh_token = refresh_token.to_string();

    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut saw_request = false;
        let mut idle_since = Instant::now();

        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    saw_request = true;
                    idle_since = Instant::now();

                    let mut buffer = [0u8; 8192];
                    let bytes = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..bytes]);
                    let request_line = request.lines().next().unwrap_or_default().to_string();
                    let path = request_line.split_whitespace().nth(1).unwrap_or("/");

                    let response = if path == "/token" {
                        http_response(
                            "200 OK",
                            "application/json",
                            &serde_json::json!({
                                "access_token": refreshed_access_token,
                                "refresh_token": refresh_token,
                                "expires_in": 3600,
                            })
                            .to_string(),
                            None,
                        )
                    } else {
                        authenticated_mcp_server_response(
                            &request,
                            &resource_metadata,
                            &challenge,
                            &refreshed_access_token,
                        )
                    };

                    let _ = stream.write_all(response.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if saw_request && idle_since.elapsed() > Duration::from_millis(500) {
                        break;
                    }
                    if !saw_request && idle_since.elapsed() > Duration::from_secs(5) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    (format!("{base_url}/mcp"), base_url, handle)
}

fn authenticated_mcp_server_response(
    request: &str,
    resource_metadata: &str,
    challenge: &str,
    access_token: &str,
) -> String {
    let request_line = request.lines().next().unwrap_or_default().to_string();
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let auth_header = request
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
        .map(|line| line.split_once(':').unwrap().1.trim().to_string());

    if path == "/.well-known/oauth-protected-resource" {
        return http_response("200 OK", "application/json", resource_metadata, None);
    }
    if auth_header.as_deref() != Some(&format!("Bearer {access_token}")) {
        return http_response(
            "401 Unauthorized",
            "text/plain",
            "Unauthorized",
            Some(&format!("WWW-Authenticate: {challenge}\r\n")),
        );
    }
    if request_line.starts_with("GET ") {
        return http_response(
            "405 Method Not Allowed",
            "text/plain",
            "Method Not Allowed",
            None,
        );
    }

    authenticated_mcp_jsonrpc_response(request)
}

fn authenticated_mcp_jsonrpc_response(request: &str) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let json: Value = serde_json::from_str(body).unwrap_or_default();
    let id = json.get("id").cloned().unwrap_or(Value::Null);
    let method = json
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match method {
        "initialize" => http_response(
            "200 OK",
            "application/json",
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "demo-mcp", "version": "1.0.0"}
                }
            })
            .to_string(),
            None,
        ),
        "notifications/initialized" | "initialized" => {
            "HTTP/1.1 202 Accepted\r\nConnection: close\r\n\r\n".to_string()
        }
        "tools/list" => http_response(
            "200 OK",
            "application/json",
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": "whoami",
                        "description": "Show identity",
                        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
                    }]
                }
            })
            .to_string(),
            None,
        ),
        "tools/call" => http_response(
            "200 OK",
            "application/json",
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": "authenticated"}],
                    "isError": false
                }
            })
            .to_string(),
            None,
        ),
        _ => http_response(
            "400 Bad Request",
            "text/plain",
            "Unsupported method",
            None,
        ),
    }
}

fn http_response(
    status: &str,
    content_type: &str,
    body: &str,
    extra_headers: Option<&str>,
) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        extra_headers.unwrap_or(""),
        body.len()
    )
}

fn write_mcp_oauth_cache(
    home: &std::path::Path,
    resource: &str,
    token_endpoint: &str,
    access_token: &str,
    expires: u64,
) {
    fs::write(
        home.join("mcp_oauth.json"),
        format!(
            r#"{{"{resource}":{{"type":"oauth","access":"{access_token}","refresh":"demo-refresh-token","expires":{expires},"resource":"{resource}","token_endpoint":"{token_endpoint}","client_id":"client-123","redirect_uri":"http://127.0.0.1:8787/callback","token_endpoint_auth_method":"none","scopes":["mcp:connect"],"authorization_endpoint":"{resource}/authorize","authorization_server":"{resource}"}}}}"#
        ),
    )
    .unwrap();
}
