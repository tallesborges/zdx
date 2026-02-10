//! Config command handlers.

use anyhow::{Context, Result};
use zdx_core::config;

pub fn path() {
    println!("{}", config::paths::config_path().display());
}

pub fn init() -> Result<()> {
    let config_path = config::paths::config_path();
    config::Config::init(&config_path)
        .with_context(|| format!("init config at {}", config_path.display()))?;
    println!("Created config at {}", config_path.display());
    Ok(())
}

pub fn generate() -> Result<()> {
    let toml = config::Config::generate()?;
    print!("{toml}");
    Ok(())
}
