//! Telegram bot command handlers.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use zdx_engine::config::{Config, TelegramProfileConfig, ThinkingLevel};

pub struct BotInitOptions {
    pub bot_token: Option<String>,
    pub user_id: Option<i64>,
    pub model: Option<String>,
    pub thinking: Option<String>,
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

    let default_model = config.telegram.model.trim();
    let model = match options.model {
        Some(model) => require_non_empty("model", &model)?,
        None => prompt_with_default("Model", default_model)?,
    };

    let default_thinking = config.telegram.thinking_level.display_name();
    let thinking_level = match options.thinking {
        Some(level) => parse_thinking_level(&level)?,
        None => parse_thinking_level(&prompt_with_default("Thinking level", default_thinking)?)?,
    };

    Config::save_telegram_bot_settings(&bot_token, &[user_id], &model, thinking_level)
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

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    let value = prompt(label, Some(default))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
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

fn parse_thinking_level(value: &str) -> Result<ThinkingLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" | "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        "max" => Ok(ThinkingLevel::Max),
        other => bail!(
            "invalid thinking level: {other} (expected off, low, medium, high, xhigh, or max)"
        ),
    }
}
