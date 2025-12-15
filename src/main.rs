mod cli;
mod config;
mod paths;

use clap::Parser;
use cli::{Cli, Commands, ConfigCommands, SessionCommands};

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
        Commands::Resume { id } => match id {
            Some(session_id) => println!("Resuming session: {}", session_id),
            None => println!("Resuming latest session..."),
        },
        Commands::Config { command } => match command {
            ConfigCommands::Path => {
                println!("{}", paths::config_path().display());
            }
            ConfigCommands::Init => {
                let config_path = paths::config_path();
                match config::Config::init(&config_path) {
                    Ok(()) => println!("Created config at {}", config_path.display()),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        },
    }
}
