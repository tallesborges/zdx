mod config;
mod core;
mod providers;
mod tools;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;

use crate::core::interrupt;
use crate::core::session::{self, SessionOptions};

#[derive(Parser)]
#[command(name = "zdx")]
#[command(version = "0.1")]
#[command(author = "Talles Borges <talles.borges92@gmail.com>")]
#[command(about = "ZDX Agentic CLI Tool")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Root directory for file operations (default: current directory)
    #[arg(long, default_value = ".")]
    root: String,

    /// Override the system prompt from config
    #[arg(long)]
    system_prompt: Option<String>,

    #[command(flatten)]
    session_args: SessionArgs,
}

/// Common session arguments for commands that support session persistence.
#[derive(clap::Args, Debug, Clone, Default)]
struct SessionArgs {
    /// Append to an existing session by ID
    #[arg(long, value_name = "ID")]
    session: Option<String>,

    /// Do not save the session
    #[arg(long)]
    no_save: bool,
}

impl From<&SessionArgs> for SessionOptions {
    fn from(args: &SessionArgs) -> Self {
        SessionOptions {
            session_id: args.session.clone(),
            no_save: args.no_save,
        }
    }
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Executes a command with a prompt
    Exec {
        /// The prompt to send to the agent
        #[arg(short, long)]
        prompt: String,
    },

    /// Manage saved sessions
    Sessions {
        #[command(subcommand)]
        command: SessionCommands,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Log in to a provider (authenticate)
    Login {
        /// Provider to log in to
        #[arg(long)]
        anthropic: bool,
    },

    /// Log out from a provider (clear cached token)
    Logout {
        /// Provider to log out from
        #[arg(long)]
        anthropic: bool,
    },
}

#[derive(clap::Subcommand)]
enum SessionCommands {
    /// Lists saved sessions
    List,
    /// Shows a specific session
    Show {
        /// The ID of the session to show
        #[arg(value_name = "SESSION_ID")]
        id: String,
    },
    /// Resume a previous session
    Resume {
        /// The ID of the session to resume (uses latest if not provided)
        #[arg(value_name = "SESSION_ID")]
        id: Option<String>,
    },
}

#[derive(clap::Subcommand)]
enum ConfigCommands {
    /// Show the path to the config file
    Path,
    /// Initialize a default config file (if not present)
    Init,
}

fn main() {
    if let Err(e) = main_result() {
        if e.downcast_ref::<interrupt::InterruptedError>().is_some() {
            std::process::exit(130);
        }
        eprintln!("{:#}", e); // pretty anyhow chain
        std::process::exit(1);
    }
}

fn main_result() -> Result<()> {
    let cli = Cli::parse();

    interrupt::init();

    // one tokio runtime for everything
    let rt = tokio::runtime::Runtime::new().context("create tokio runtime")?;

    rt.block_on(async move {
        let mut config = config::Config::load().context("load config")?;

        if let Some(sp) = cli.system_prompt.as_deref() {
            let trimmed = sp.trim();
            if trimmed.is_empty() {
                config.system_prompt = None;
                config.system_prompt_file = None;
            } else {
                config.system_prompt = Some(trimmed.to_string());
                config.system_prompt_file = None;
            }
        }

        // default to chat mode
        let Some(command) = cli.command else {
            return run_chat(&cli.root, &cli.session_args, &config).await;
        };

        match command {
            Commands::Exec { prompt } => {
                run_exec(&cli.root, &cli.session_args, &prompt, &config).await
            }

            Commands::Sessions { command } => match command {
                SessionCommands::List => {
                    let sessions = session::list_sessions().context("list sessions")?;
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        for info in sessions {
                            let modified_str = info
                                .modified
                                .and_then(session::format_timestamp)
                                .unwrap_or_else(|| "unknown".to_string());
                            println!("{}  {}", info.id, modified_str);
                        }
                    }
                    Ok(())
                }
                SessionCommands::Show { id } => {
                    let events = session::load_session(&id)
                        .with_context(|| format!("load session '{id}'"))?;
                    if events.is_empty() {
                        println!("Session '{}' is empty or not found.", id);
                    } else {
                        println!("{}", session::format_transcript(&events));
                    }
                    Ok(())
                }
                SessionCommands::Resume { id } => run_resume(id, &config).await,
            },

            Commands::Config { command } => match command {
                ConfigCommands::Path => {
                    println!("{}", config::paths::config_path().display());
                    Ok(())
                }
                ConfigCommands::Init => {
                    let config_path = config::paths::config_path();
                    config::Config::init(&config_path)
                        .with_context(|| format!("init config at {}", config_path.display()))?;
                    println!("Created config at {}", config_path.display());
                    Ok(())
                }
            },

            Commands::Login { anthropic } => {
                if !anthropic {
                    anyhow::bail!("Please specify a provider: --anthropic");
                }
                run_login_anthropic().await
            }

            Commands::Logout { anthropic } => {
                if !anthropic {
                    anyhow::bail!("Please specify a provider: --anthropic");
                }
                run_logout_anthropic()
            }
        }
    })
}

async fn run_chat(root: &str, session_args: &SessionArgs, config: &config::Config) -> Result<()> {
    use std::io::{IsTerminal, Read};

    // If stdin is piped, run exec mode instead
    if !std::io::stdin().is_terminal() {
        let mut prompt = String::new();
        std::io::stdin().lock().read_to_string(&mut prompt)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            anyhow::bail!("No input provided via pipe");
        }
        return run_exec(root, session_args, prompt, config).await;
    }

    let session_opts: SessionOptions = session_args.into();
    let session = session_opts.resolve().context("resolve session")?;

    let root_path = std::path::PathBuf::from(root);
    ui::run_interactive_chat(config, session, root_path)
        .await
        .context("interactive chat failed")?;

    Ok(())
}

async fn run_exec(
    root: &str,
    session_args: &SessionArgs,
    prompt: &str,
    config: &config::Config,
) -> Result<()> {
    let session_opts: SessionOptions = session_args.into();
    let session = session_opts.resolve().context("resolve session")?;

    let exec_opts = ui::stream::ExecOptions {
        root: std::path::PathBuf::from(root),
    };

    // Use streaming variant - response is printed incrementally, final newline added at end
    ui::stream::execute_prompt_streaming(prompt, config, session, &exec_opts)
        .await
        .context("execute prompt")?;

    Ok(())
}

async fn run_resume(id: Option<String>, config: &config::Config) -> Result<()> {
    let session_id = match id {
        Some(id) => id,
        None => session::latest_session_id()
            .context("find latest session id")?
            .context("No sessions found to resume")?,
    };

    let history = session::load_session_as_messages(&session_id)
        .with_context(|| format!("load history for '{session_id}'"))?;

    let session = session::Session::with_id(session_id.clone())
        .with_context(|| format!("open session '{session_id}'"))?;

    let root_path = std::path::PathBuf::from(".");
    ui::run_interactive_chat_with_history(config, Some(session), history, root_path)
        .await
        .context("resume chat failed")?;

    Ok(())
}

async fn run_login_anthropic() -> Result<()> {
    use crate::providers::oauth::{OAuthCache, anthropic as oauth_anthropic};
    use std::io::{self, BufRead, Write};

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

fn run_logout_anthropic() -> Result<()> {
    use crate::providers::oauth::{OAuthCache, anthropic as oauth_anthropic};

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
