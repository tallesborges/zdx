//! CLI entry and dispatch.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use zdx_core::config;
use zdx_core::core::thread_persistence::ThreadPersistenceOptions;
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

    /// Override the model from config (chat launch)
    #[arg(long)]
    model: Option<String>,

    /// Override the thinking level (off, minimal, low, medium, high, xhigh)
    #[arg(long)]
    thinking: Option<String>,

    /// Capture raw request/response traces (optional path)
    #[arg(
        long,
        value_name = "DIR",
        num_args = 0..=1,
        default_missing_value = "1"
    )]
    debug_trace: Option<String>,

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

        /// Override the thinking level (off, minimal, low, medium, high, xhigh)
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
    /// Manage automations
    Automations {
        #[command(subcommand)]
        command: AutomationCommands,
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

    /// Telegram utility commands
    Telegram {
        #[command(subcommand)]
        command: TelegramCommands,
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
    /// Append a message to an existing thread
    Append {
        /// The thread ID to append to
        #[arg(value_name = "THREAD_ID")]
        id: String,
        /// Message role (user or assistant)
        #[arg(long, default_value = "assistant")]
        role: String,
        /// Message text
        #[arg(long)]
        text: String,
    },
    /// Search threads by date and/or query text
    Search {
        /// Optional query text to match in titles and thread content
        #[arg(value_name = "QUERY")]
        query: Option<String>,

        /// Filter to threads active on this date (YYYY-MM-DD)
        #[arg(long, value_name = "YYYY-MM-DD")]
        date: Option<String>,

        /// Filter to threads active on/after this date (YYYY-MM-DD)
        #[arg(long = "date-start", value_name = "YYYY-MM-DD")]
        date_start: Option<String>,

        /// Filter to threads active on/before this date (YYYY-MM-DD)
        #[arg(long = "date-end", value_name = "YYYY-MM-DD")]
        date_end: Option<String>,

        /// Maximum number of results to return
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Output as JSON for automation/script usage
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Subcommand)]
enum AutomationCommands {
    /// List discovered automations
    List,
    /// Validate automation files
    Validate,
    /// Run scheduled automations daemon
    Daemon {
        /// Poll interval in seconds (minimum 1)
        #[arg(long, default_value_t = 30)]
        poll_interval_secs: u64,
    },
    /// Show automation run history (from JSONL log)
    Runs {
        /// Optional automation name (file stem)
        #[arg(value_name = "NAME")]
        name: Option<String>,

        /// Filter by finished date (YYYY-MM-DD)
        #[arg(long, value_name = "YYYY-MM-DD")]
        date: Option<String>,

        /// Filter finished date on/after (YYYY-MM-DD)
        #[arg(long = "date-start", value_name = "YYYY-MM-DD")]
        date_start: Option<String>,

        /// Filter finished date on/before (YYYY-MM-DD)
        #[arg(long = "date-end", value_name = "YYYY-MM-DD")]
        date_end: Option<String>,

        /// Output JSON
        #[arg(long)]
        json: bool,
    },
    /// Run one automation by name (file stem)
    Run {
        /// Automation name (file stem)
        #[arg(value_name = "NAME")]
        name: String,
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
enum TelegramCommands {
    /// Create a forum topic in a supergroup with topics enabled
    CreateTopic {
        /// Telegram chat ID (supergroup)
        #[arg(long, value_name = "CHAT_ID")]
        chat_id: i64,

        /// Forum topic name
        #[arg(long, value_name = "NAME")]
        name: String,

        /// Bot token override
        #[arg(long, value_name = "TOKEN")]
        bot_token: Option<String>,
    },

    /// Send a message to a chat (optionally to a forum topic)
    SendMessage {
        /// Telegram chat ID
        #[arg(long, value_name = "CHAT_ID")]
        chat_id: i64,

        /// Message body
        #[arg(long, value_name = "TEXT")]
        text: String,

        /// Optional forum topic thread ID
        #[arg(long, value_name = "THREAD_ID")]
        message_thread_id: Option<i64>,

        /// Message parse mode
        #[arg(
            long,
            value_name = "MODE",
            default_value = "html",
            value_parser = ["markdown", "markdown-v2", "html", "plain"]
        )]
        parse_mode: String,

        /// Bot token override
        #[arg(long, value_name = "TOKEN")]
        bot_token: Option<String>,
    },
    /// Send a document (file) to a chat (optionally to a forum topic)
    SendDocument {
        /// Telegram chat ID
        #[arg(long, value_name = "CHAT_ID")]
        chat_id: i64,

        /// Path to the file to send
        #[arg(long, value_name = "PATH")]
        path: String,

        /// Optional caption
        #[arg(long, value_name = "CAPTION")]
        caption: Option<String>,

        /// Optional forum topic thread ID
        #[arg(long, value_name = "THREAD_ID")]
        message_thread_id: Option<i64>,

        /// Bot token override
        #[arg(long, value_name = "TOKEN")]
        bot_token: Option<String>,
    },
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

    if let Some(value) = cli.debug_trace.as_deref() {
        // set_var is unsafe in Rust 2024 (process-global mutation)
        unsafe {
            std::env::set_var("ZDX_DEBUG_TRACE", value);
        }
    }

    interrupt::init();

    // one tokio runtime for everything
    let rt = tokio::runtime::Runtime::new().context("create tokio runtime")?;

    rt.block_on(async move { dispatch(cli).await })
}

async fn dispatch(cli: Cli) -> Result<()> {
    let mut config = config::Config::load().context("load config")?;
    apply_system_prompt_override(&mut config, cli.system_prompt.as_deref());

    let Cli {
        command,
        root,
        system_prompt: _,
        model,
        thinking,
        thread_args,
        worktree,
        ..
    } = cli;

    let Some(command) = command else {
        return run_chat_command(
            &root,
            worktree.as_deref(),
            &thread_args,
            &config,
            model.as_deref(),
            thinking.as_deref(),
        )
        .await;
    };

    let context = DispatchContext {
        root: &root,
        worktree_id: worktree.as_deref(),
        thread_args: &thread_args,
        config: &config,
    };

    dispatch_command(command, &context).await
}

fn apply_system_prompt_override(config: &mut config::Config, system_prompt: Option<&str>) {
    let Some(sp) = system_prompt else {
        return;
    };

    let trimmed = sp.trim();
    if trimmed.is_empty() {
        config.system_prompt = None;
        config.system_prompt_file = None;
    } else {
        config.system_prompt = Some(trimmed.to_string());
        config.system_prompt_file = None;
    }
}

fn resolve_root(root: &str, worktree_id: Option<&str>) -> Result<PathBuf> {
    let root_path = PathBuf::from(root);
    if let Some(id) = worktree_id {
        worktree::ensure_worktree(&root_path, id)
            .with_context(|| format!("ensure worktree for '{id}'"))
    } else {
        Ok(root_path)
    }
}

async fn run_chat_command(
    root: &str,
    worktree_id: Option<&str>,
    thread_args: &ThreadArgs,
    config: &config::Config,
    model_override: Option<&str>,
    thinking_override: Option<&str>,
) -> Result<()> {
    let thread_opts: ThreadPersistenceOptions = thread_args.into();
    let root_path = resolve_root(root, worktree_id)?;
    let root_string = root_path.to_string_lossy().to_string();
    commands::chat::run(
        &root_string,
        &thread_opts,
        config,
        model_override,
        thinking_override,
    )
    .await
}

struct DispatchContext<'a> {
    root: &'a str,
    worktree_id: Option<&'a str>,
    thread_args: &'a ThreadArgs,
    config: &'a config::Config,
}

struct ExecCommandInput {
    prompt: String,
    model: Option<String>,
    thinking: Option<String>,
    tools: Option<String>,
    no_tools: bool,
}

async fn run_exec_command(context: &DispatchContext<'_>, input: ExecCommandInput) -> Result<()> {
    let thread_opts: ThreadPersistenceOptions = context.thread_args.into();
    let root_path = resolve_root(context.root, context.worktree_id)?;
    let root_string = root_path.to_string_lossy().to_string();
    commands::exec::run(commands::exec::ExecRunOptions {
        root: &root_string,
        thread_opts: &thread_opts,
        prompt: &input.prompt,
        config: context.config,
        model_override: input.model.as_deref(),
        tool_timeout_override: None,
        thinking_override: input.thinking.as_deref(),
        tools_override: input.tools.as_deref(),
        no_tools: input.no_tools,
    })
    .await
}

async fn dispatch_command(command: Commands, context: &DispatchContext<'_>) -> Result<()> {
    match command {
        Commands::Bot => dispatch_bot(context).await,
        Commands::Exec {
            prompt,
            model,
            thinking,
            tools,
            no_tools,
        } => {
            run_exec_command(
                context,
                ExecCommandInput {
                    prompt,
                    model,
                    thinking,
                    tools,
                    no_tools,
                },
            )
            .await
        }
        Commands::Threads { command } => dispatch_threads(command, context).await,
        Commands::Automations { command } => dispatch_automations(command, context).await,
        Commands::Config { command } => dispatch_config(&command),
        Commands::Login {
            anthropic,
            claude_cli,
            openai_codex,
            gemini_cli,
        } => dispatch_login((anthropic, claude_cli, openai_codex, gemini_cli)).await,
        Commands::Logout {
            anthropic,
            claude_cli,
            openai_codex,
            gemini_cli,
        } => dispatch_logout((anthropic, claude_cli, openai_codex, gemini_cli)),
        Commands::Models { command } => dispatch_models(command, context).await,
        Commands::Telegram { command } => dispatch_telegram(command, context).await,
        Commands::Worktree { command } => dispatch_worktree(command, context),
    }
}

async fn dispatch_bot(context: &DispatchContext<'_>) -> Result<()> {
    let root_path = resolve_root(context.root, context.worktree_id)?;
    zdx_bot::run_with_root(root_path).await
}

async fn dispatch_threads(command: ThreadCommands, context: &DispatchContext<'_>) -> Result<()> {
    match command {
        ThreadCommands::List => commands::threads::list(),
        ThreadCommands::Show { id } => commands::threads::show(&id),
        ThreadCommands::Resume { id } => commands::threads::resume(id, context.config).await,
        ThreadCommands::Rename { id, title } => commands::threads::rename(&id, &title),
        ThreadCommands::Append { id, role, text } => commands::threads::append(&id, &role, &text),
        ThreadCommands::Search {
            query,
            date,
            date_start,
            date_end,
            limit,
            json,
        } => commands::threads::search(commands::threads::SearchCommandOptions {
            query,
            date,
            date_start,
            date_end,
            limit,
            json,
        }),
    }
}

async fn dispatch_automations(
    command: AutomationCommands,
    context: &DispatchContext<'_>,
) -> Result<()> {
    let root_path = resolve_root(context.root, context.worktree_id)?;
    match command {
        AutomationCommands::List => commands::automations::list(&root_path),
        AutomationCommands::Validate => commands::automations::validate(&root_path),
        AutomationCommands::Daemon { poll_interval_secs } => {
            let thread_opts: ThreadPersistenceOptions = context.thread_args.into();
            commands::daemon::run(&root_path, &thread_opts, context.config, poll_interval_secs)
                .await
        }
        AutomationCommands::Runs {
            name,
            date,
            date_start,
            date_end,
            json,
        } => commands::automations::runs(commands::automations::RunsOptions {
            name,
            date,
            date_start,
            date_end,
            json,
        }),
        AutomationCommands::Run { name } => {
            let thread_opts: ThreadPersistenceOptions = context.thread_args.into();
            commands::automations::run(&root_path, &thread_opts, context.config, &name).await
        }
    }
}

fn dispatch_config(command: &ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Path => {
            commands::config::path();
            Ok(())
        }
        ConfigCommands::Init => commands::config::init(),
        ConfigCommands::Generate => commands::config::generate(),
    }
}

async fn dispatch_login(flags: (bool, bool, bool, bool)) -> Result<()> {
    let provider = select_auth_provider(flags)?;
    login_provider(provider).await
}

fn dispatch_logout(flags: (bool, bool, bool, bool)) -> Result<()> {
    let provider = select_auth_provider(flags)?;
    logout_provider(provider)
}

async fn dispatch_models(command: ModelsCommands, context: &DispatchContext<'_>) -> Result<()> {
    match command {
        ModelsCommands::Update => commands::models::update(context.config).await,
    }
}

async fn dispatch_telegram(command: TelegramCommands, context: &DispatchContext<'_>) -> Result<()> {
    match command {
        TelegramCommands::CreateTopic {
            chat_id,
            name,
            bot_token,
        } => commands::telegram::create_topic(context.config, bot_token, chat_id, &name).await,
        TelegramCommands::SendMessage {
            chat_id,
            text,
            message_thread_id,
            parse_mode,
            bot_token,
        } => {
            commands::telegram::send_message(
                context.config,
                bot_token,
                chat_id,
                message_thread_id,
                &text,
                &parse_mode,
            )
            .await
        }
        TelegramCommands::SendDocument {
            chat_id,
            path,
            caption,
            message_thread_id,
            bot_token,
        } => {
            commands::telegram::send_document(
                context.config,
                bot_token,
                chat_id,
                message_thread_id,
                &path,
                caption.as_deref(),
            )
            .await
        }
    }
}

fn dispatch_worktree(command: WorktreeCommands, context: &DispatchContext<'_>) -> Result<()> {
    match command {
        WorktreeCommands::Ensure { id } => commands::worktree::ensure(context.root, &id),
    }
}

#[derive(Clone, Copy)]
enum AuthProvider {
    Anthropic,
    ClaudeCli,
    OpenaiCodex,
    GeminiCli,
}

fn select_auth_provider(flags: (bool, bool, bool, bool)) -> Result<AuthProvider> {
    match flags {
        (true, false, false, false) => Ok(AuthProvider::Anthropic),
        (false, true, false, false) => Ok(AuthProvider::ClaudeCli),
        (false, false, true, false) => Ok(AuthProvider::OpenaiCodex),
        (false, false, false, true) => Ok(AuthProvider::GeminiCli),
        _ => anyhow::bail!(
            "Please specify a provider: --anthropic, --claude-cli, --openai-codex, or --gemini-cli"
        ),
    }
}

async fn login_provider(provider: AuthProvider) -> Result<()> {
    match provider {
        AuthProvider::Anthropic => commands::auth::login_anthropic().await,
        AuthProvider::ClaudeCli => commands::auth::login_claude_cli().await,
        AuthProvider::OpenaiCodex => commands::auth::login_openai_codex().await,
        AuthProvider::GeminiCli => commands::auth::login_gemini_cli().await,
    }
}

fn logout_provider(provider: AuthProvider) -> Result<()> {
    match provider {
        AuthProvider::Anthropic => {
            commands::auth::logout_anthropic();
            Ok(())
        }
        AuthProvider::ClaudeCli => commands::auth::logout_claude_cli(),
        AuthProvider::OpenaiCodex => commands::auth::logout_openai_codex(),
        AuthProvider::GeminiCli => commands::auth::logout_gemini_cli(),
    }
}
