use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "ZDX maintainer tasks")]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandName>,
}

#[derive(Debug, Subcommand)]
enum CommandName {
    /// Update default_models.toml by running `zdx models update`.
    UpdateDefaultModels,
    /// Update default_config.toml by running `zdx config init`.
    UpdateDefaultConfig,
    /// Update both default_config.toml and default_models.toml.
    UpdateDefaults,
}

impl Default for CommandName {
    fn default() -> Self {
        CommandName::UpdateDefaults
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or_default();

    match command {
        CommandName::UpdateDefaultModels => update_default_models(),
        CommandName::UpdateDefaultConfig => update_default_config(),
        CommandName::UpdateDefaults => update_defaults(),
    }
}

fn update_default_models() -> Result<()> {
    let root = project_root()?;
    let temp_dir = tempfile::tempdir().context("create temp dir for ZDX_HOME")?;
    let models_path = temp_dir.path().join("models.toml");

    let status = Command::new("cargo")
        .current_dir(&root)
        .env("ZDX_HOME", temp_dir.path())
        .arg("run")
        .arg("-p")
        .arg("zdx")
        .arg("--")
        .arg("models")
        .arg("update")
        .status()
        .context("run `cargo run -p zdx -- models update`")?;

    if !status.success() {
        bail!("models update failed with status {status}");
    }

    if !models_path.exists() {
        bail!("models update did not produce {}", models_path.display());
    }

    let dest = root.join("default_models.toml");
    fs::copy(&models_path, &dest)
        .with_context(|| format!("copy {} to {}", models_path.display(), dest.display()))?;

    println!("Updated {}", dest.display());
    Ok(())
}

fn update_default_config() -> Result<()> {
    let root = project_root()?;
    let temp_dir = tempfile::tempdir().context("create temp dir for ZDX_HOME")?;
    let config_path = temp_dir.path().join("config.toml");

    let status = Command::new("cargo")
        .current_dir(&root)
        .env("ZDX_HOME", temp_dir.path())
        .arg("run")
        .arg("-p")
        .arg("zdx")
        .arg("--")
        .arg("config")
        .arg("init")
        .status()
        .context("run `cargo run -p zdx -- config init`")?;

    if !status.success() {
        bail!("config init failed with status {status}");
    }

    if !config_path.exists() {
        bail!("config init did not produce {}", config_path.display());
    }

    let dest = root.join("default_config.toml");
    fs::copy(&config_path, &dest)
        .with_context(|| format!("copy {} to {}", config_path.display(), dest.display()))?;

    println!("Updated {}", dest.display());
    Ok(())
}

fn update_defaults() -> Result<()> {
    update_default_config()?;
    update_default_models()?;
    Ok(())
}

fn project_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .context("locate workspace root from CARGO_MANIFEST_DIR")?;
    Ok(root.to_path_buf())
}
