//! CLI entry and dispatch.

use anyhow::{Context, Result};
use clap::Parser;
use zdx_core::config;
use zdx_core::core::thread_log::ThreadPersistenceOptions;
use zdx_core::core::{interrupt, worktree};

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

    /// Use a git worktree for this ID (auto-create if missing)
    #[arg(long, value_name = "ID")]
    worktree: Option<String>,

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
    /// Run the Telegram bot (long-polling)
    Bot,
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

        /// Comma-separated list of tools to enable (full override)
        #[arg(long, value_name = "TOOLS")]
        tools: Option<String>,

        /// Disable all tools
        #[arg(long = "no-tools", conflicts_with = "tools")]
        no_tools: bool,
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
        /// Provider to log in to (Claude CLI OAuth)
        #[arg(long = "claude-cli")]
        claude_cli: bool,
        /// Provider to log in to
        #[arg(long = "openai-codex")]
        openai_codex: bool,
        /// Provider to log in to (Google Cloud Code Assist)
        #[arg(long = "gemini-cli")]
        gemini_cli: bool,
    },

    /// Log out from a provider (clear cached token)
    Logout {
        /// Provider to log out from
        #[arg(long)]
        anthropic: bool,
        /// Provider to log out from (Claude CLI OAuth)
        #[arg(long = "claude-cli")]
        claude_cli: bool,
        /// Provider to log out from
        #[arg(long = "openai-codex")]
        openai_codex: bool,
        /// Provider to log out from (Google Cloud Code Assist)
        #[arg(long = "gemini-cli")]
        gemini_cli: bool,
    },

    /// Manage model registry
    Models {
        #[command(subcommand)]
        command: ModelsCommands,
    },

    /// Manage git worktrees
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommands,
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
    /// Generate a fresh config from Rust defaults (for xtask)
    Generate,
}

#[derive(clap::Subcommand)]
enum ModelsCommands {
    /// Fetch and update the models registry from models.dev
    Update,
}

#[derive(clap::Subcommand)]
enum WorktreeCommands {
    /// Ensure a worktree exists for an ID
    Ensure {
        /// Stable identifier
        #[arg(value_name = "ID")]
        id: String,
    },
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

    let resolve_root = |root: &str, worktree: Option<&str>| -> Result<std::path::PathBuf> {
        let root_path = std::path::PathBuf::from(root);
        if let Some(id) = worktree {
            worktree::ensure_worktree(&root_path, id)
                .with_context(|| format!("ensure worktree for '{}'", id))
        } else {
            Ok(root_path)
        }
    };

    let Cli {
        command,
        root,
        system_prompt: _,
        thread_args,
        worktree,
    } = cli;

    // default to chat mode
    let Some(command) = command else {
        let thread_opts: ThreadPersistenceOptions = (&thread_args).into();
        let root_path = resolve_root(&root, worktree.as_deref())?;
        let root_string = root_path.to_string_lossy().to_string();
        return commands::chat::run(&root_string, &thread_opts, &config).await;
    };

    match command {
        Commands::Bot => {
            let root_path = resolve_root(&root, worktree.as_deref())?;
            zdx_bot::run_with_root(root_path).await
        }
        Commands::Exec {
            prompt,
            model,
            thinking,
            tools,
            no_tools,
        } => {
            let thread_opts: ThreadPersistenceOptions = (&thread_args).into();
            let root_path = resolve_root(&root, worktree.as_deref())?;
            let root_string = root_path.to_string_lossy().to_string();
            commands::exec::run(commands::exec::ExecRunOptions {
                root: &root_string,
                thread_opts: &thread_opts,
                prompt: &prompt,
                config: &config,
                model_override: model.as_deref(),
                thinking_override: thinking.as_deref(),
                tools_override: tools.as_deref(),
                no_tools,
            })
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
            ConfigCommands::Generate => commands::config::generate(),
        },

        Commands::Login {
            anthropic,
            claude_cli,
            openai_codex,
            gemini_cli,
        } => match (anthropic, claude_cli, openai_codex, gemini_cli) {
            (true, false, false, false) => commands::auth::login_anthropic().await,
            (false, true, false, false) => commands::auth::login_claude_cli().await,
            (false, false, true, false) => commands::auth::login_openai_codex().await,
            (false, false, false, true) => commands::auth::login_gemini_cli().await,
            _ => anyhow::bail!(
                "Please specify a provider: --anthropic, --claude-cli, --openai-codex, or --gemini-cli"
            ),
        },

        Commands::Logout {
            anthropic,
            claude_cli,
            openai_codex,
            gemini_cli,
        } => match (anthropic, claude_cli, openai_codex, gemini_cli) {
            (true, false, false, false) => commands::auth::logout_anthropic(),
            (false, true, false, false) => commands::auth::logout_claude_cli(),
            (false, false, true, false) => commands::auth::logout_openai_codex(),
            (false, false, false, true) => commands::auth::logout_gemini_cli(),
            _ => anyhow::bail!(
                "Please specify a provider: --anthropic, --claude-cli, --openai-codex, or --gemini-cli"
            ),
        },

        Commands::Models { command } => match command {
            ModelsCommands::Update => commands::models::update(&config).await,
        },

        Commands::Worktree { command } => match command {
            WorktreeCommands::Ensure { id } => commands::worktree::ensure(&root, &id),
        },
    }
}
