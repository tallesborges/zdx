//! Telegram bot command handlers.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use zdx_engine::config::{Config, TelegramProfileConfig, paths};
use zdx_engine::service;

/// Names the gateway binary `zdx bot` starts instead of the installed
/// `~/.local/bin/zdx-gateway`. Must be an absolute path.
pub const GATEWAY_ENV: &str = "ZDX_GATEWAY";

/// `zdx bot [-- ARGS]`: replaces this process with the Go gateway, as
/// `<gateway> --root ROOT bot ARGS`, the argv the bot service runs. The
/// gateway gets this `ZDX_HOME`, and `ZDX_BIN` (this binary unless already
/// set), so it never looks `zdx` up on PATH. Being the same process, it
/// receives this process's signals and its exit status is `zdx bot`'s.
///
/// # Errors
/// Returns an error, before anything runs, when the Telegram settings are
/// not valid (no bot token, for one), or the gateway is missing, is not an
/// absolute path, or is this binary; or when the exec fails.
pub fn run_gateway(config: &Config, root: &Path, args: &[String]) -> Result<()> {
    config.resolve_telegram_runtime()?;
    let gateway = match std::env::var_os(GATEWAY_ENV) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => service::default_gateway_program(),
    };
    if !gateway.is_absolute() {
        bail!(
            "{GATEWAY_ENV} must be an absolute path to a zdx-gateway binary (got {})",
            gateway.display()
        );
    }
    if !gateway.is_file() {
        bail!(
            "zdx-gateway not found at {}: the Telegram bot is the Go gateway. Build and install it with `just gateway-install`, or set {GATEWAY_ENV} to a zdx-gateway binary",
            gateway.display()
        );
    }
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .context("resolve the zdx binary")?;
    let resolved = gateway
        .canonicalize()
        .with_context(|| format!("resolve {}", gateway.display()))?;
    if resolved == exe {
        bail!(
            "{} is this zdx binary, not the gateway; set {GATEWAY_ENV} to a zdx-gateway binary",
            gateway.display()
        );
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve root {}", root.display()))?;

    let mut command = Command::new(&gateway);
    command
        .arg("--root")
        .arg(&root)
        .arg("bot")
        .args(args)
        .env("ZDX_HOME", paths::zdx_home());
    if std::env::var_os("ZDX_BIN").is_none_or(|bin| bin.is_empty()) {
        command.env("ZDX_BIN", &exe);
    }
    exec(command, &gateway)
}

#[cfg(unix)]
fn exec(mut command: Command, gateway: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    let error = command.exec();
    Err(error).with_context(|| format!("start {}", gateway.display()))
}

#[cfg(not(unix))]
fn exec(_command: Command, _gateway: &Path) -> Result<()> {
    bail!("`zdx bot` starts the gateway in place of zdx, which needs a Unix system")
}

pub struct BotInitOptions {
    pub bot_token: Option<String>,
    pub user_id: Option<i64>,
}

pub fn init(config: &Config, options: BotInitOptions) -> Result<()> {
    let bot_token = match options.bot_token {
        Some(token) => require_non_empty("bot token", &token)?,
        None => prompt_required("Bot token")?,
    };

    let default_user_id = config.telegram.allowlist_user_ids.first().copied();
    let user_id = match options.user_id {
        Some(id) => id,
        None => prompt_i64_with_default("Allowlisted user ID", default_user_id)?,
    };

    Config::save_telegram_bot_settings(&bot_token, &[user_id])
        .context("save telegram bot settings")?;

    println!(
        "Saved Telegram bot settings to {}",
        zdx_engine::config::paths::config_path().display()
    );
    println!("Run `zdx bot` to start it.");
    Ok(())
}

pub fn add_profile(config: &Config, name: &str, chat_id: i64, cwd: &Path) -> Result<()> {
    let name = require_non_empty("profile name", name)?;
    if config.telegram.profiles.contains_key(&name) {
        bail!("telegram profile '{name}' already exists");
    }
    if let Some((existing_name, _)) = config.telegram_profile_for_chat(chat_id) {
        bail!("telegram chat ID {chat_id} is already used by profile '{existing_name}'");
    }
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("cwd does not exist: {}", cwd.display()))?;
    if !cwd.is_dir() {
        bail!("cwd is not a directory: {}", cwd.display());
    }

    let profile = TelegramProfileConfig {
        chat_id,
        cwd: cwd.display().to_string(),
        orchestrator: false,
        worker_root: None,
    };
    Config::save_telegram_profile(&name, &profile).context("save telegram profile")?;

    println!("Saved Telegram profile '{name}' to telegram.profiles.{name}");
    println!("Chat ID: {chat_id}");
    println!("CWD: {}", cwd.display());
    Ok(())
}

fn prompt_required(label: &str) -> Result<String> {
    loop {
        let value = prompt(label, None)?;
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
        eprintln!("{label} is required.");
    }
}

fn prompt_i64_with_default(label: &str, default: Option<i64>) -> Result<i64> {
    loop {
        let default_string = default.map(|v| v.to_string());
        let value = prompt(label, default_string.as_deref())?;
        let trimmed = value.trim();
        if trimmed.is_empty()
            && let Some(default) = default
        {
            return Ok(default);
        }
        match trimmed.parse::<i64>() {
            Ok(parsed) => return Ok(parsed),
            Err(_) => eprintln!("Enter a valid integer for {label}."),
        }
    }
}

fn prompt(label: &str, default: Option<&str>) -> Result<String> {
    let mut stdout = io::stdout();
    match default {
        Some(default) if !default.trim().is_empty() => write!(stdout, "{label} [{default}]: ")?,
        _ => write!(stdout, "{label}: ")?,
    }
    stdout.flush().context("flush prompt")?;

    let mut buffer = String::new();
    io::stdin()
        .read_line(&mut buffer)
        .context("read interactive input")?;
    Ok(buffer)
}

fn require_non_empty(label: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(trimmed.to_string())
}
