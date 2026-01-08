use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

use anyhow::{Context, Result, bail};

const DEFAULT_CMD: &str = "update-default-models";

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| DEFAULT_CMD.to_string());

    match cmd.as_str() {
        "update-default-models" => update_default_models(),
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        _ => bail!("Unknown command: {cmd}. Run with --help for usage."),
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

fn project_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .context("locate workspace root from CARGO_MANIFEST_DIR")?;
    Ok(root.to_path_buf())
}

fn print_help() {
    println!(
        "Usage:\n  cargo xtask update-default-models\n\n\
Updates default_models.toml by running `zdx models update` with a temporary ZDX_HOME."
    );
}
