mod agent;
mod chat;
mod cli;
mod config;
mod paths;
mod providers;
mod session;
mod tools;

use clap::Parser;
use cli::{Cli, Commands, ConfigCommands, SessionCommands};
use session::SessionOptions;

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Exec {
            prompt,
            root,
            session_args,
        } => {
            let config = match config::Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {}", e);
                    std::process::exit(1);
                }
            };

            let session_opts: SessionOptions = (&session_args).into();
            let session = match session_opts.resolve() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            };

            let agent_opts = agent::AgentOptions {
                root: std::path::PathBuf::from(root),
            };

            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            match rt.block_on(agent::execute_prompt(
                &prompt,
                &config,
                session.as_ref(),
                &agent_opts,
            )) {
                Ok(response) => {
                    println!("{}", response);
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Chat { session_args } => {
            let config = match config::Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {}", e);
                    std::process::exit(1);
                }
            };

            let session_opts: SessionOptions = (&session_args).into();
            let session = match session_opts.resolve() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            };

            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            if let Err(e) = rt.block_on(chat::run_interactive_chat(&config, session)) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Sessions { command } => match command {
            SessionCommands::List => match session::list_sessions() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        for info in sessions {
                            let modified_str = info
                                .modified
                                .and_then(session::format_timestamp)
                                .unwrap_or_else(|| "unknown".to_string());

                            println!("{}  {}", info.id, modified_str);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error listing sessions: {}", e);
                    std::process::exit(1);
                }
            },
            SessionCommands::Show { id } => match session::load_session(&id) {
                Ok(events) => {
                    if events.is_empty() {
                        println!("Session '{}' is empty or not found.", id);
                    } else {
                        println!("{}", session::format_transcript(&events));
                    }
                }
                Err(e) => {
                    eprintln!("Error loading session '{}': {}", id, e);
                    std::process::exit(1);
                }
            },
        },
        // TODO: Add exec mode support for resume (e.g., `zdx resume -p "prompt"`)
        Commands::Resume { id } => {
            let config = match config::Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {}", e);
                    std::process::exit(1);
                }
            };

            // Determine session ID (provided or latest)
            let session_id = match id {
                Some(id) => id,
                None => match session::latest_session_id() {
                    Ok(Some(id)) => id,
                    Ok(None) => {
                        eprintln!("No sessions found to resume.");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Error finding latest session: {}", e);
                        std::process::exit(1);
                    }
                },
            };

            // Load existing messages as history
            let history = match session::load_session_as_messages(&session_id) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("Error loading session '{}': {}", session_id, e);
                    std::process::exit(1);
                }
            };

            // Open the session to continue appending
            let session = match session::Session::with_id(session_id) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error opening session: {}", e);
                    std::process::exit(1);
                }
            };

            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            if let Err(e) = rt.block_on(chat::run_interactive_chat_with_history(
                &config,
                Some(session),
                history,
            )) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
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
