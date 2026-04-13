//! Telegram bot command handlers.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use zdx_engine::config::{BotsConfig, Config, NamedBotConfig, ThinkingLevel};

pub struct BotInitOptions {
    pub name: Option<String>,
    pub bot_token: Option<String>,
    pub user_id: Option<i64>,
    pub chat_id: Option<i64>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub force: bool,
}

pub fn init(root: &Path, config: &Config, options: BotInitOptions) -> Result<()> {
    let name = match options.name {
        Some(name) => require_non_empty("bot name", &name)?,
        None => prompt_required("Bot name")?,
    };
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut bots = BotsConfig::load().context("load bot registry")?;
    if bots.get(&name).is_some() && !options.force {
        bail!("bot '{name}' already exists in the bot registry (use --force to overwrite)");
    }

    let bot_token = match options.bot_token {
        Some(token) => require_non_empty("bot token", &token)?,
        None => prompt_required("Bot token")?,
    };

    let default_user_id = config.telegram.allowlist_user_ids.first().copied();
    let user_id = match options.user_id {
        Some(id) => id,
        None => prompt_i64_with_default("Allowlisted user ID", default_user_id)?,
    };

    let chat_id = match options.chat_id {
        Some(id) => id,
        None => prompt_i64_required("Allowlisted group chat ID")?,
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

    let named_bot = NamedBotConfig {
        root: root.display().to_string(),
        bot_token,
        allowlist_user_ids: vec![user_id],
        allowlist_chat_ids: vec![chat_id],
        model,
        thinking_level,
    };

    bots.bots.insert(name.clone(), named_bot);
    bots.save().context("save bot registry")?;

    println!(
        "Saved bot '{}' to {}",
        name,
        zdx_engine::config::paths::bots_config_path().display()
    );
    println!("Run `zdx bot --bot {name}` to start it.");
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

fn prompt_i64_required(label: &str) -> Result<i64> {
    loop {
        let value = prompt_required(label)?;
        match value.parse::<i64>() {
            Ok(parsed) => return Ok(parsed),
            Err(_) => eprintln!("Enter a valid integer for {label}.",),
        }
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
            Err(_) => eprintln!("Enter a valid integer for {label}.",),
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
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        other => bail!(
            "invalid thinking level: {other} (expected off, minimal, low, medium, high, or xhigh)"
        ),
    }
}
