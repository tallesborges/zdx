use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zdx-cli")]
#[command(version = "0.1")]
#[command(author = "Talles Borges <talles.borges92@gmail.com>")]
#[command(about = "ZDX Agentic CLI Tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
}

#[derive(Subcommand)]
enum SessionCommands {
    /// Lists saved sessions
    List,
    /// Shows a specific session
    Show {
        /// The ID of the session to show
        id: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Exec { prompt } => {
            println!("Executing with prompt: {}", prompt);
        }
        Commands::Chat => {
            println!("Starting chat...");
        }
        Commands::Sessions { command } => match command {
            SessionCommands::List => {
                println!("Listing saved sessions...");
            }
            SessionCommands::Show { id } => {
                println!("Showing session: {}", id);
            }
        },
    }
}
