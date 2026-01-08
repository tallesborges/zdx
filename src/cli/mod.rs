//! CLI entry and dispatch.

use anyhow::{Context, Result};
use clap::Parser;

use crate::config;
use crate::core::interrupt;
use crate::core::thread_log::ThreadPersistenceOptions;

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
    thread_args: ThreadArgs,
}

/// Common thread arguments for commands that support thread persistence.
#[derive(clap::Args, Debug, Clone, Default)]
struct ThreadArgs {
    /// Append to an existing thread by ID
    #[arg(long, value_name = "ID")]
    thread: Option<String>,

    /// Do not save the thread
    #[arg(long = "no-thread")]
    no_save: bool,
}

impl From<&ThreadArgs> for ThreadPersistenceOptions {
    fn from(args: &ThreadArgs) -> Self {
        ThreadPersistenceOptions {
            thread_id: args.thread.clone(),
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

    /// Manage saved threads
    Threads {
        #[command(subcommand)]
        command: ThreadCommands,
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
        /// Provider to log in to
        #[arg(long = "openai-codex")]
        openai_codex: bool,
    },

    /// Log out from a provider (clear cached token)
    Logout {
        /// Provider to log out from
        #[arg(long)]
        anthropic: bool,
        /// Provider to log out from
        #[arg(long = "openai-codex")]
        openai_codex: bool,
    },

    /// Manage model registry
    Models {
        #[command(subcommand)]
        command: ModelsCommands,
    },
}

#[derive(clap::Subcommand)]
enum ThreadCommands {
    /// Lists saved threads
    List,
    /// Shows a specific thread
    Show {
        /// The ID of the thread to show
        #[arg(value_name = "THREAD_ID")]
        id: String,
    },
    /// Resume a previous thread
    Resume {
        /// The ID of the thread to resume (uses latest if not provided)
        #[arg(value_name = "THREAD_ID")]
        id: Option<String>,
    },
    /// Rename a thread
    Rename {
        /// The ID of the thread to rename
        #[arg(value_name = "THREAD_ID")]
        id: String,
        /// New title for the thread
        #[arg(value_name = "TITLE")]
        title: String,
    },
}

#[derive(clap::Subcommand)]
enum ConfigCommands {
    /// Show the path to the config file
    Path,
    /// Initialize a default config file (if not present)
    Init,
}

#[derive(clap::Subcommand)]
enum ModelsCommands {
    /// Fetch and update the models registry from models.dev
    Update,
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
        let thread_opts: ThreadPersistenceOptions = (&cli.thread_args).into();
        return commands::chat::run(&cli.root, &thread_opts, &config).await;
    };

    match command {
        Commands::Exec {
            prompt,
            model,
            thinking,
        } => {
            let thread_opts: ThreadPersistenceOptions = (&cli.thread_args).into();
            commands::exec::run(
                &cli.root,
                &thread_opts,
                &prompt,
                &config,
                model.as_deref(),
                thinking.as_deref(),
            )
            .await
        }

        Commands::Threads { command } => match command {
            ThreadCommands::List => commands::threads::list(),
            ThreadCommands::Show { id } => commands::threads::show(&id),
            ThreadCommands::Resume { id } => commands::threads::resume(id, &config).await,
            ThreadCommands::Rename { id, title } => commands::threads::rename(&id, &title),
        },

        Commands::Config { command } => match command {
            ConfigCommands::Path => commands::config::path(),
            ConfigCommands::Init => commands::config::init(),
        },

        Commands::Login {
            anthropic,
            openai_codex,
        } => match (anthropic, openai_codex) {
            (true, false) => commands::auth::login_anthropic().await,
            (false, true) => commands::auth::login_openai_codex().await,
            _ => anyhow::bail!("Please specify a provider: --anthropic or --openai-codex"),
        },

        Commands::Logout {
            anthropic,
            openai_codex,
        } => match (anthropic, openai_codex) {
            (true, false) => commands::auth::logout_anthropic(),
            (false, true) => commands::auth::logout_openai_codex(),
            _ => anyhow::bail!("Please specify a provider: --anthropic or --openai-codex"),
        },

        Commands::Models { command } => match command {
            ModelsCommands::Update => commands::models::update(&config).await,
        },
    }
}
