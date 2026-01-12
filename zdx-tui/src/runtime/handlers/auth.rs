use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use crate::common::RequestId;
use crate::events::UiEvent;

/// Exchanges an OAuth code for credentials.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn token_exchange(
    provider: zdx_core::providers::ProviderKind,
    code: String,
    verifier: String,
    redirect_uri: Option<String>,
    req: RequestId,
) -> UiEvent {
    use zdx_core::providers::oauth::{claude_cli, gemini_cli, openai_codex};

    let result = match provider {
        zdx_core::providers::ProviderKind::ClaudeCli => {
            let pkce = claude_cli::Pkce {
                verifier,
                challenge: String::new(),
            };
            let redirect_uri = match redirect_uri {
                Some(value) => value,
                None => {
                    return UiEvent::LoginResult {
                        req,
                        result: Err("Missing redirect URI for Claude CLI OAuth.".to_string()),
                    };
                }
            };
            match claude_cli::exchange_code(&code, &pkce, &redirect_uri).await {
                Ok(creds) => claude_cli::save_credentials(&creds)
                    .map_err(|e| format!("Failed to save: {}", e)),
                Err(e) => Err(e.to_string()),
            }
        }
        zdx_core::providers::ProviderKind::OpenAICodex => {
            let pkce = openai_codex::Pkce {
                verifier,
                challenge: String::new(),
            };
            match openai_codex::exchange_code(&code, &pkce).await {
                Ok(creds) => openai_codex::save_credentials(&creds)
                    .map_err(|e| format!("Failed to save: {}", e)),
                Err(e) => Err(e.to_string()),
            }
        }
        zdx_core::providers::ProviderKind::GeminiCli => {
            let pkce = gemini_cli::Pkce {
                verifier,
                challenge: String::new(),
            };
            match gemini_cli::exchange_code(&code, &pkce).await {
                Ok(mut creds) => {
                    // Discover project ID after getting tokens
                    match gemini_cli::discover_project(&creds.access).await {
                        Ok(project_id) => {
                            creds.account_id = Some(project_id);
                            gemini_cli::save_credentials(&creds)
                                .map_err(|e| format!("Failed to save: {}", e))
                        }
                        Err(e) => Err(format!("Failed to discover project: {}", e)),
                    }
                }
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Err("OAuth is not supported for this provider.".to_string()),
    };
    UiEvent::LoginResult { req, result }
}

/// Listens for a local OAuth callback.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn local_auth_callback(
    provider: zdx_core::providers::ProviderKind,
    state: Option<String>,
    port: Option<u16>,
) -> UiEvent {
    let code = match provider {
        zdx_core::providers::ProviderKind::ClaudeCli => {
            use zdx_core::providers::oauth::claude_cli;
            port.and_then(|port| {
                wait_for_local_code(port, claude_cli::LOCAL_CALLBACK_PATH, state.as_deref())
            })
        }
        zdx_core::providers::ProviderKind::OpenAICodex => {
            wait_for_local_code(1455, "/auth/callback", state.as_deref())
        }
        zdx_core::providers::ProviderKind::GeminiCli => {
            wait_for_local_code(8085, "/oauth2callback", state.as_deref())
        }
        _ => None,
    };
    UiEvent::LoginCallbackResult(code)
}

fn wait_for_local_code(
    port: u16,
    callback_path: &str,
    expected_state: Option<&str>,
) -> Option<String> {
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)) {
        Ok(listener) => listener,
        Err(_) => return None,
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let expected_state = expected_state.map(|s| s.to_string());
    let callback_path = callback_path.to_string();

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let code = extract_code_from_request(
                        &request,
                        &callback_path,
                        expected_state.as_deref(),
                    );
                    let response = match code.is_some() {
                        true => oauth_success_response(),
                        false => oauth_error_response(),
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

fn extract_code_from_request(
    request: &str,
    callback_path: &str,
    expected_state: Option<&str>,
) -> Option<String> {
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;

    let url = url::Url::parse(&format!("http://localhost{}", path)).ok()?;
    if url.path() != callback_path {
        return None;
    }
    if let Some(expected) = expected_state {
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string())?;
        if state != expected {
            return None;
        }
    }
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
}

fn oauth_success_response() -> String {
    let body = "<html><body><h3>Login complete</h3><p>You can close this window.</p></body></html>";
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn oauth_error_response() -> String {
    let body = "<html><body><h3>Login failed</h3><p>Please return to the terminal and paste the code.</p></body></html>";
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}
