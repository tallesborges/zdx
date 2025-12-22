mod config;
mod engine;
mod providers;
mod shared;
mod tools;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;

use crate::engine::session::{self, SessionOptions};
use crate::shared::context;
use crate::shared::interrupt;

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
    /// Development/debug commands
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        command: DevCommands,
    },
}

#[derive(clap::Subcommand)]
enum DevCommands {
    /// Test the full-screen TUI2 (work in progress)
    Tui2,
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
        if e.downcast_ref::<interrupt::InterruptedError>()
            .is_some()
        {
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

            Commands::Dev { command } => match command {
                DevCommands::Tui2 => {
                    // Get root path (current directory)
                    let root = std::env::current_dir().context("get current dir")?;

                    // Build effective system prompt
                    let effective =
                        context::build_effective_system_prompt_with_paths(&config, &root)?;

                    // Print warnings like the normal chat does
                    for warning in &effective.warnings {
                        eprintln!("Warning: {}", warning.message);
                    }
                    if !effective.loaded_agents_paths.is_empty() {
                        eprintln!("Loaded AGENTS.md from:");
                        for path in &effective.loaded_agents_paths {
                            eprintln!("  - {}", path.display());
                        }
                    }

                    let mut app =
                        ui::Tui2App::new(config, root, effective.prompt).context("create TUI2")?;
                    app.run().context("run TUI2")?;
                    Ok(())
                }
            },
        }
    })
}

async fn run_chat(
    root: &str,
    session_args: &SessionArgs,
    config: &config::Config,
) -> Result<()> {
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
    ui::chat::run_interactive_chat(config, session, root_path)
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
    ui::chat::run_interactive_chat_with_history(config, Some(session), history, root_path)
        .await
        .context("resume chat failed")?;

    Ok(())
}
