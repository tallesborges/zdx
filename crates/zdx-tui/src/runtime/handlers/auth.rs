use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use crate::events::UiEvent;

/// Exchanges an OAuth code for credentials.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn token_exchange(
    provider: zdx_engine::providers::ProviderKind,
    code: String,
    verifier: String,
    redirect_uri: Option<String>,
) -> UiEvent {
    use zdx_engine::providers::oauth::{claude_cli, google_antigravity, grok_build, openai_codex};

    let result = match provider {
        zdx_engine::providers::ProviderKind::ClaudeCli => {
            let pkce = claude_cli::Pkce {
                verifier,
                challenge: String::new(),
            };
            let Some(redirect_uri) = redirect_uri else {
                return UiEvent::LoginResult {
                    result: Err("Missing redirect URI for Claude CLI OAuth.".to_string()),
                };
            };
            match claude_cli::exchange_code(&code, &pkce, &redirect_uri).await {
                Ok(creds) => {
                    claude_cli::save_credentials(&creds).map_err(|e| format!("Failed to save: {e}"))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        zdx_engine::providers::ProviderKind::OpenAICodex => {
            let pkce = openai_codex::Pkce {
                verifier,
                challenge: String::new(),
            };
            match openai_codex::exchange_code(&code, &pkce).await {
                Ok(creds) => openai_codex::save_credentials(&creds)
                    .map_err(|e| format!("Failed to save: {e}")),
                Err(e) => Err(e.to_string()),
            }
        }
        zdx_engine::providers::ProviderKind::GoogleAntigravity => {
            let pkce = google_antigravity::Pkce {
                verifier,
                challenge: String::new(),
            };
            match google_antigravity::exchange_code(&code, &pkce).await {
                Ok(mut creds) => match google_antigravity::discover_project(&creds.access).await {
                    Ok(project_id) => {
                        creds.account_id = Some(project_id);
                        google_antigravity::save_credentials(&creds)
                            .map_err(|e| format!("Failed to save: {e}"))
                    }
                    Err(e) => Err(format!("Failed to discover project: {e}")),
                },
                Err(e) => Err(e.to_string()),
            }
        }
        zdx_engine::providers::ProviderKind::GrokBuild => {
            let pkce = grok_build::Pkce {
                verifier,
                challenge: String::new(),
            };
            match grok_build::exchange_code(&code, &pkce).await {
                Ok(creds) => {
                    grok_build::save_credentials(&creds).map_err(|e| format!("Failed to save: {e}"))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Err("OAuth is not supported for this provider.".to_string()),
    };
    UiEvent::LoginResult { result }
}

/// Listens for a local OAuth callback.
///
/// Pure async function - runtime spawns and sends result to inbox.
pub async fn local_auth_callback(
    provider: zdx_engine::providers::ProviderKind,
    state: Option<String>,
    port: Option<u16>,
) -> UiEvent {
    tokio::task::yield_now().await;
    let code = match provider {
        zdx_engine::providers::ProviderKind::ClaudeCli => {
            use zdx_engine::providers::oauth::claude_cli;
            port.and_then(|port| {
                wait_for_local_code(port, claude_cli::LOCAL_CALLBACK_PATH, state.as_deref())
            })
        }
        zdx_engine::providers::ProviderKind::OpenAICodex => {
            wait_for_local_code(1455, "/auth/callback", state.as_deref())
        }
        zdx_engine::providers::ProviderKind::GoogleAntigravity => {
            wait_for_local_code(51121, "/oauth-callback", state.as_deref())
        }
        zdx_engine::providers::ProviderKind::GrokBuild => {
            use zdx_engine::providers::oauth::grok_build;
            wait_for_local_code(
                grok_build::LOCAL_CALLBACK_PORT,
                grok_build::LOCAL_CALLBACK_PATH,
                state.as_deref(),
            )
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
    let Ok(listener) = TcpListener::bind(format!("127.0.0.1:{port}")) else {
        return None;
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let expected_state = expected_state.map(std::string::ToString::to_string);
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
                    if start.elapsed() > Duration::from_mins(2) {
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

    rx.recv_timeout(Duration::from_mins(2)).ok().flatten()
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

    let url = url::Url::parse(&format!("http://localhost{path}")).ok()?;
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
