//! Auth command handlers.

use std::io::{self, BufRead, Write};

use anyhow::Result;

use crate::providers::oauth::{OAuthCache, anthropic as oauth_anthropic};

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
