//! Auth command handlers.

use std::io::{self, BufRead, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use anyhow::Result;

use crate::providers::oauth::{
    OAuthCache, anthropic as oauth_anthropic, openai_codex as oauth_codex,
};

pub async fn login_anthropic() -> Result<()> {
    // Check if already logged in
    if let Some(existing) = oauth_anthropic::load_credentials()? {
        println!(
            "Already logged in to Anthropic (token: {})",
            oauth_anthropic::mask_token(&existing.access)
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
    let pkce = oauth_anthropic::generate_pkce();
    let auth_url = oauth_anthropic::build_auth_url(&pkce);

    // Show instructions
    println!("To log in to Anthropic with OAuth:");
    println!();
    println!("  1. A browser window will open (or visit the URL below)");
    println!("  2. Log in to your Anthropic account and authorize access");
    println!("  3. After authorization, you'll see a code - copy it");
    println!("  4. Paste the code below (format: code#state)");
    println!();
    println!("Authorization URL:");
    println!("  {}", auth_url);
    println!();

    // Try to open browser (best effort, skip in tests)
    if std::env::var("ZDX_NO_BROWSER").is_err() {
        let _ = open::that(&auth_url);
    }

    // Read authorization code
    print!("Paste authorization code: ");
    io::stdout().flush()?;

    let mut auth_code = String::new();
    io::stdin().lock().read_line(&mut auth_code)?;
    let auth_code = auth_code.trim();

    if auth_code.is_empty() {
        anyhow::bail!("Authorization code cannot be empty");
    }

    // Exchange code for tokens
    println!("Exchanging code for tokens...");
    let credentials = oauth_anthropic::exchange_code(auth_code, &pkce).await?;

    // Save credentials
    oauth_anthropic::save_credentials(&credentials)?;

    let cache_path = OAuthCache::cache_path();
    println!();
    println!(
        "✓ Logged in to Anthropic (token: {})",
        oauth_anthropic::mask_token(&credentials.access)
    );
    println!("  Credentials saved to: {}", cache_path.display());

    Ok(())
}

pub fn logout_anthropic() -> Result<()> {
    let had_creds = oauth_anthropic::clear_credentials()?;

    if had_creds {
        let cache_path = OAuthCache::cache_path();
        println!("✓ Logged out from Anthropic");
        println!("  Credentials removed from: {}", cache_path.display());
    } else {
        println!("Not logged in to Anthropic (no credentials found).");
    }

    Ok(())
}

pub async fn login_openai_codex() -> Result<()> {
    if let Some(existing) = oauth_codex::load_credentials()? {
        println!(
            "Already logged in to OpenAI Codex (token: {})",
            oauth_anthropic::mask_token(&existing.access)
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
        oauth_anthropic::mask_token(&credentials.access)
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
