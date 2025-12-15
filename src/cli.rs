use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zdx")]
#[command(version = "0.1")]
#[command(author = "Talles Borges <talles.borges92@gmail.com>")]
#[command(about = "ZDX Agentic CLI Tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Executes a command with a prompt
    Exec {
        /// The prompt to send to the agent
        prompt: String,
    },
    /// Starts an interactive chat with the agent
    Chat,
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
