mod cli;

use clap::Parser;
use cli::{Cli, Commands, SessionCommands};

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
        Commands::Resume { id } => {
            match id {
                Some(session_id) => println!("Resuming session: {}", session_id),
                None => println!("Resuming latest session..."),
            }
        }
    }
}
