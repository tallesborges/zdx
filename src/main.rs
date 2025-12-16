mod agent;
mod chat;
mod cli;
mod config;
mod paths;
mod providers;
mod session;
mod tools;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Commands, ConfigCommands, SessionCommands};
use session::SessionOptions;

fn main() {
    if let Err(e) = main_result() {
        eprintln!("{:#}", e); // pretty anyhow chain
        std::process::exit(1);
    }
}

fn main_result() -> Result<()> {
    let cli = Cli::parse();

    // one tokio runtime for everything
    let rt = tokio::runtime::Runtime::new().context("create tokio runtime")?;

    rt.block_on(async move {
        // default to chat mode
        let Some(command) = cli.command else {
            return run_chat(&cli.root, &cli.session_args).await;
        };

        match command {
            Commands::Exec { prompt } => run_exec(&cli.root, &cli.session_args, &prompt).await,

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
            },

            Commands::Resume { id } => run_resume(id).await,

            Commands::Config { command } => match command {
                ConfigCommands::Path => {
                    println!("{}", paths::config_path().display());
                    Ok(())
                }
                ConfigCommands::Init => {
                    let config_path = paths::config_path();
                    config::Config::init(&config_path)
                        .with_context(|| format!("init config at {}", config_path.display()))?;
                    println!("Created config at {}", config_path.display());
                    Ok(())
                }
            },
        }
    })
}

async fn run_chat(root: &str, session_args: &cli::SessionArgs) -> Result<()> {
    let config = config::Config::load().context("load config")?;

    let session_opts: SessionOptions = session_args.into();
    let session = session_opts.resolve().context("resolve session")?;

    let root_path = std::path::PathBuf::from(root);
    chat::run_interactive_chat(&config, session, root_path)
        .await
        .context("interactive chat failed")?;

    Ok(())
}

async fn run_exec(root: &str, session_args: &cli::SessionArgs, prompt: &str) -> Result<()> {
    let config = config::Config::load().context("load config")?;

    let session_opts: SessionOptions = session_args.into();
    let session = session_opts.resolve().context("resolve session")?;

    let agent_opts = agent::AgentOptions {
        root: std::path::PathBuf::from(root),
    };

    let response = agent::execute_prompt(prompt, &config, session.as_ref(), &agent_opts)
        .await
        .context("execute prompt")?;

    println!("{response}");
    Ok(())
}

async fn run_resume(id: Option<String>) -> Result<()> {
    let config = config::Config::load().context("load config")?;

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
    chat::run_interactive_chat_with_history(&config, Some(session), history, root_path)
        .await
        .context("resume chat failed")?;

    Ok(())
}
