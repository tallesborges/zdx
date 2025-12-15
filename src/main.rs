mod agent;
mod chat;
mod cli;
mod config;
mod paths;
mod providers;

use clap::Parser;
use cli::{Cli, Commands, ConfigCommands, SessionCommands};

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Exec { prompt } => {
            let config = match config::Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {}", e);
                    std::process::exit(1);
                }
            };

            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            match rt.block_on(agent::execute_prompt(&prompt, &config)) {
                Ok(response) => {
                    println!("{}", response);
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Chat => {
            let config = match config::Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {}", e);
                    std::process::exit(1);
                }
            };

            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            if let Err(e) = rt.block_on(chat::run_interactive_chat(&config)) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
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
