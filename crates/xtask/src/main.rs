use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

mod codebase;

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
    /// Generate codebase.txt with all source files.
    Codebase,
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
        CommandName::Codebase => codebase::run(),
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

    let dest = root
        .join("crates")
        .join("zdx-core")
        .join("default_models.toml");
    fs::copy(&models_path, &dest)
        .with_context(|| format!("copy {} to {}", models_path.display(), dest.display()))?;

    println!("Updated {}", dest.display());
    Ok(())
}

fn update_default_config() -> Result<()> {
    let root = project_root()?;
    let dest = root
        .join("crates")
        .join("zdx-core")
        .join("default_config.toml");

    let output = Command::new("cargo")
        .current_dir(&root)
        .arg("run")
        .arg("-p")
        .arg("zdx")
        .arg("--")
        .arg("config")
        .arg("generate")
        .output()
        .context("run `cargo run -p zdx -- config generate`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("config generate failed: {}", stderr);
    }

    fs::write(&dest, &output.stdout)
        .with_context(|| format!("write config to {}", dest.display()))?;

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
