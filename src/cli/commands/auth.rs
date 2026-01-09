//! Auth command handlers.

use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use anyhow::Result;

use crate::providers::oauth::{
    OAuthCache, claude_cli as oauth_claude_cli, gemini_cli as oauth_gemini_cli,
    openai_codex as oauth_codex,
};

pub async fn login_anthropic() -> Result<()> {
    println!("Anthropic uses API keys.");
    println!("Set ANTHROPIC_API_KEY in your shell to authenticate.");
    Ok(())
}

pub async fn login_claude_cli() -> Result<()> {
    // Check if already logged in
    if let Some(existing) = oauth_claude_cli::load_credentials()? {
        println!(
            "Already logged in to Claude CLI (token: {})",
            oauth_claude_cli::mask_token(&existing.access)
        );
        print!("Do you want to replace the existing credentials? [y/N] ");
        io::stdout().flush()?;

        let mut response = String::new();
        io::stdin().lock().read_line(&mut response)?;
        if !response.trim().eq_ignore_ascii_case("y") {
            println!("Login cancelled.");
            return Ok(());
        }
    }

    // Generate PKCE challenge
    let pkce = oauth_claude_cli::generate_pkce();
    let oauth_state = uuid::Uuid::new_v4().to_string();
    let callback_port = oauth_claude_cli::random_local_port();
    let redirect_uri = oauth_claude_cli::build_redirect_uri(callback_port);
    let auth_url = oauth_claude_cli::build_auth_url(&pkce, &oauth_state, &redirect_uri);

    // Show instructions
    println!("To log in to Claude CLI with OAuth:");
    println!();
    println!("  1. A browser window will open (or visit the URL below)");
    println!("  2. Log in to your Anthropic account and authorize access");
    println!("  3. If redirected to localhost, return here to continue");
    println!("  4. Otherwise, paste the authorization code or URL");
    println!();
    println!("Authorization URL:");
    println!("  {}", auth_url);
    println!();

    // Try to open browser (best effort, skip in tests)
    if std::env::var("ZDX_NO_BROWSER").is_err() {
        let _ = open::that(&auth_url);
    }

    // Prefer local callback in interactive sessions, fall back to manual paste.
    let expected_state = oauth_state.clone();
    let local_code = if io::stdin().is_terminal() {
        wait_for_claude_cli_code(&expected_state, callback_port)
    } else {
        None
    };
    let auth_code = match local_code {
        Some(code) => format!("{}#{}", code, expected_state),
        None => {
            print!("Paste authorization code (or full redirect URL): ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().lock().read_line(&mut input)?;
            let (code, provided_state) = oauth_claude_cli::parse_authorization_input(&input);
            if let Some(provided) = provided_state
                && provided != expected_state
            {
                anyhow::bail!("State mismatch");
            }
            let code = code.ok_or_else(|| anyhow::anyhow!("Authorization code cannot be empty"))?;
            format!("{}#{}", code, expected_state)
        }
    };

    // Exchange code for tokens
    println!("Exchanging code for tokens...");
    let credentials = oauth_claude_cli::exchange_code(&auth_code, &pkce, &redirect_uri).await?;

    // Save credentials
    oauth_claude_cli::save_credentials(&credentials)?;

    let cache_path = OAuthCache::cache_path();
    println!();
    println!(
        "✓ Logged in to Claude CLI (token: {})",
        oauth_claude_cli::mask_token(&credentials.access)
    );
    println!("  Credentials saved to: {}", cache_path.display());

    Ok(())
}

pub fn logout_anthropic() -> Result<()> {
    println!("Anthropic uses API keys.");
    println!("Unset ANTHROPIC_API_KEY to remove authentication.");
    Ok(())
}

pub fn logout_claude_cli() -> Result<()> {
    let had_creds = oauth_claude_cli::clear_credentials()?;

    if had_creds {
        let cache_path = OAuthCache::cache_path();
        println!("✓ Logged out from Claude CLI");
        println!("  Credentials removed from: {}", cache_path.display());
    } else {
        println!("Not logged in to Claude CLI (no credentials found).");
    }

    Ok(())
}

pub async fn login_openai_codex() -> Result<()> {
    if let Some(existing) = oauth_codex::load_credentials()? {
        println!(
            "Already logged in to OpenAI Codex (token: {})",
            oauth_claude_cli::mask_token(&existing.access)
        );
        print!("Do you want to replace the existing credentials? [y/N] ");
        io::stdout().flush()?;

        let mut response = String::new();
        io::stdin().lock().read_line(&mut response)?;
        if !response.trim().eq_ignore_ascii_case("y") {
            println!("Login cancelled.");
            return Ok(());
        }
    }

    let pkce = oauth_codex::generate_pkce();
    let state = uuid::Uuid::new_v4().to_string();
    let auth_url = oauth_codex::build_auth_url(&pkce, &state);

    println!("To log in to OpenAI Codex with OAuth:");
    println!();
    println!("  1. A browser window will open (or visit the URL below)");
    println!("  2. Log in to your OpenAI account and authorize access");
    println!("  3. If redirected to localhost, return here to continue");
    println!("  4. Otherwise, paste the authorization code or URL");
    println!();
    println!("Authorization URL:");
    println!("  {}", auth_url);
    println!();

    if std::env::var("ZDX_NO_BROWSER").is_err() {
        let _ = open::that(&auth_url);
    }

    let code = match wait_for_local_code(&state) {
        Some(code) => code,
        None => {
            print!("Paste authorization code (or full redirect URL): ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().lock().read_line(&mut input)?;
            let (code, provided_state) = oauth_codex::parse_authorization_input(&input);
            if let Some(provided) = provided_state
                && provided != state
            {
                anyhow::bail!("State mismatch");
            }
            code.ok_or_else(|| anyhow::anyhow!("Authorization code cannot be empty"))?
        }
    };

    println!("Exchanging code for tokens...");
    let credentials = oauth_codex::exchange_code(&code, &pkce).await?;
    oauth_codex::save_credentials(&credentials)?;

    let cache_path = OAuthCache::cache_path();
    println!();
    println!(
        "✓ Logged in to OpenAI Codex (token: {})",
        oauth_claude_cli::mask_token(&credentials.access)
    );
    println!("  Credentials saved to: {}", cache_path.display());

    Ok(())
}

pub fn logout_openai_codex() -> Result<()> {
    let had_creds = oauth_codex::clear_credentials()?;

    if had_creds {
        let cache_path = OAuthCache::cache_path();
        println!("✓ Logged out from OpenAI Codex");
        println!("  Credentials removed from: {}", cache_path.display());
    } else {
        println!("Not logged in to OpenAI Codex (no credentials found).");
    }

    Ok(())
}

pub async fn login_gemini_cli() -> Result<()> {
    if let Some(existing) = oauth_gemini_cli::load_credentials()? {
        println!(
            "Already logged in to Gemini CLI (token: {})",
            oauth_claude_cli::mask_token(&existing.access)
        );
        print!("Do you want to replace the existing credentials? [y/N] ");
        io::stdout().flush()?;

        let mut response = String::new();
        io::stdin().lock().read_line(&mut response)?;
        if !response.trim().eq_ignore_ascii_case("y") {
            println!("Login cancelled.");
            return Ok(());
        }
    }

    let pkce = oauth_gemini_cli::generate_pkce();
    let state = uuid::Uuid::new_v4().to_string();
    let auth_url = oauth_gemini_cli::build_auth_url(&pkce, &state);

    println!("To log in to Gemini CLI with Google OAuth:");
    println!();
    println!("  1. A browser window will open (or visit the URL below)");
    println!("  2. Log in with your Google account and authorize access");
    println!("  3. If redirected to localhost, return here to continue");
    println!("  4. Otherwise, paste the authorization code or URL");
    println!();
    println!("Authorization URL:");
    println!("  {}", auth_url);
    println!();

    if std::env::var("ZDX_NO_BROWSER").is_err() {
        let _ = open::that(&auth_url);
    }

    let code = match wait_for_gemini_cli_code(&state) {
        Some(code) => code,
        None => {
            print!("Paste authorization code (or full redirect URL): ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().lock().read_line(&mut input)?;
            let (code, provided_state) = oauth_gemini_cli::parse_authorization_input(&input);
            if let Some(provided) = provided_state
                && provided != state
            {
                anyhow::bail!("State mismatch");
            }
            code.ok_or_else(|| anyhow::anyhow!("Authorization code cannot be empty"))?
        }
    };

    println!("Exchanging code for tokens...");
    let mut credentials = oauth_gemini_cli::exchange_code(&code, &pkce).await?;

    println!("Discovering Cloud Code Assist project...");
    let project_id = oauth_gemini_cli::discover_project(&credentials.access).await?;
    credentials.account_id = Some(project_id.clone());

    oauth_gemini_cli::save_credentials(&credentials)?;

    let cache_path = OAuthCache::cache_path();
    println!();
    println!("✓ Logged in to Gemini CLI (project: {})", project_id);
    println!("  Credentials saved to: {}", cache_path.display());

    Ok(())
}

pub fn logout_gemini_cli() -> Result<()> {
    let had_creds = oauth_gemini_cli::clear_credentials()?;

    if had_creds {
        let cache_path = OAuthCache::cache_path();
        println!("✓ Logged out from Gemini CLI");
        println!("  Credentials removed from: {}", cache_path.display());
    } else {
        println!("Not logged in to Gemini CLI (no credentials found).");
    }

    Ok(())
}

fn wait_for_gemini_cli_code(state: &str) -> Option<String> {
    let listener = match TcpListener::bind("127.0.0.1:8085") {
        Ok(listener) => listener,
        Err(_) => return None,
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let state = state.to_string();

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let code = extract_gemini_cli_code_from_request(&request, &state);
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

fn extract_gemini_cli_code_from_request(request: &str, expected_state: &str) -> Option<String> {
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;

    let url = url::Url::parse(&format!("http://localhost{}", path)).ok()?;
    if url.path() != "/oauth2callback" {
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

fn wait_for_claude_cli_code(state: &str, port: u16) -> Option<String> {
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)) {
        Ok(listener) => listener,
        Err(_) => return None,
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let state = state.to_string();

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let code = extract_claude_cli_code_from_request(&request, &state);
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

fn extract_claude_cli_code_from_request(request: &str, expected_state: &str) -> Option<String> {
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;

    let url = url::Url::parse(&format!("http://localhost{}", path)).ok()?;
    if url.path() != oauth_claude_cli::LOCAL_CALLBACK_PATH {
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

fn wait_for_local_code(state: &str) -> Option<String> {
    let listener = match TcpListener::bind("127.0.0.1:1455") {
        Ok(listener) => listener,
        Err(_) => return None,
    };
    let _ = listener.set_nonblocking(true);

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let state = state.to_string();

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    let request = String::from_utf8_lossy(&buffer);
                    let code = extract_code_from_request(&request, &state);
                    let response = match code.is_some() {
                        true => oauth_success_response(),
                        false => oauth_error_response(),
                    };
                    let _ = stream.write_all(response.as_bytes());
                    let _ = tx.send(code);
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(60) {
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

    rx.recv_timeout(Duration::from_secs(60)).ok().flatten()
}

fn extract_code_from_request(request: &str, expected_state: &str) -> Option<String> {
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;

    let url = url::Url::parse(&format!("http://localhost{}", path)).ok()?;
    if url.path() != "/auth/callback" {
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
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn oauth_error_response() -> String {
    let body = "Invalid OAuth callback";
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}
