use clap::{Args, Parser, Subcommand};

use crate::session::SessionOptions;

#[derive(Parser)]
#[command(name = "zdx")]
#[command(version = "0.1")]
#[command(author = "Talles Borges <talles.borges92@gmail.com>")]
#[command(about = "ZDX Agentic CLI Tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Root directory for file operations (default: current directory)
    #[arg(long, default_value = ".", global = true)]
    pub root: String,

    #[command(flatten)]
    pub session_args: SessionArgs,
}

/// Common session arguments for commands that support session persistence.
#[derive(Args, Debug, Clone, Default)]
pub struct SessionArgs {
    /// Append to an existing session by ID
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,

    /// Do not save the session
    #[arg(long)]
    pub no_save: bool,
}

impl From<&SessionArgs> for SessionOptions {
    fn from(args: &SessionArgs) -> Self {
        SessionOptions {
            session_id: args.session.clone(),
            no_save: args.no_save,
        }
    }
}

#[derive(Subcommand)]
pub enum Commands {
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
    /// Resume a previous session
    Resume {
        /// The ID of the session to resume (uses latest if not provided)
        #[arg(value_name = "SESSION_ID")]
        id: Option<String>,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Subcommand)]
pub enum SessionCommands {
    /// Lists saved sessions
    List,
    /// Shows a specific session
    Show {
        /// The ID of the session to show
        #[arg(value_name = "SESSION_ID")]
        id: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show the path to the config file
    Path,
    /// Initialize a default config file (if not present)
    Init,
}
