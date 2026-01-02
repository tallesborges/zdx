//! CLI entry and dispatch.

use anyhow::{Context, Result};
use clap::Parser;

use crate::config;
use crate::core::interrupt;
use crate::core::session::SessionPersistenceOptions;

mod commands;

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

impl From<&SessionArgs> for SessionPersistenceOptions {
    fn from(args: &SessionArgs) -> Self {
        SessionPersistenceOptions {
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

        /// Override the model from config
        #[arg(short, long)]
        model: Option<String>,

        /// Override the thinking level (off, minimal, low, medium, high)
        #[arg(short, long)]
        thinking: Option<String>,
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

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    interrupt::init();

    // one tokio runtime for everything
    let rt = tokio::runtime::Runtime::new().context("create tokio runtime")?;

    rt.block_on(async move { dispatch(cli).await })
}

async fn dispatch(cli: Cli) -> Result<()> {
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
        let session_opts: SessionPersistenceOptions = (&cli.session_args).into();
        return commands::chat::run(&cli.root, &session_opts, &config).await;
    };

    match command {
        Commands::Exec { prompt, model, thinking } => {
            let session_opts: SessionPersistenceOptions = (&cli.session_args).into();
            commands::exec::run(&cli.root, &session_opts, &prompt, &config, model.as_deref(), thinking.as_deref()).await
        }

        Commands::Sessions { command } => match command {
            SessionCommands::List => commands::sessions::list(),
            SessionCommands::Show { id } => commands::sessions::show(&id),
            SessionCommands::Resume { id } => commands::sessions::resume(id, &config).await,
        },

        Commands::Config { command } => match command {
            ConfigCommands::Path => commands::config::path(),
            ConfigCommands::Init => commands::config::init(),
        },

        Commands::Login { anthropic } => {
            if !anthropic {
                anyhow::bail!("Please specify a provider: --anthropic");
            }
            commands::auth::login_anthropic().await
        }

        Commands::Logout { anthropic } => {
            if !anthropic {
                anyhow::bail!("Please specify a provider: --anthropic");
            }
            commands::auth::logout_anthropic()
        }
    }
}
