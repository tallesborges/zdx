use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::process::Command;
use zdx_engine::config::ThinkingLevel;
use zdx_engine::core::{thread_persistence, worktree};

use super::status::format_status_message;
use super::{ReplyContext, StatusSnapshot, escape_html, thread_id_for_chat};
use crate::agent;
use crate::bot::context::BotContext;
use crate::commands::{BotCommand, ModelSubcommand, ThinkingSubcommand, parse_command};
use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup};

pub(super) async fn handle_thread_setup_commands(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    thread_id: &str,
) -> Result<bool> {
    Ok(handle_model_command(
        context,
        incoming,
        thread_id,
        reply_ctx.reply_to_message_id,
        reply_ctx.topic_id,
    )
    .await?
        || handle_thinking_command(
            context,
            incoming,
            thread_id,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?
        || handle_status_command(
            context,
            incoming,
            thread_id,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?
        || handle_whereami_command(
            context,
            incoming,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?
        || handle_thread_commands(
            context,
            incoming,
            thread_id,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?)
}

pub(super) async fn handle_general_forum_commands(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
) -> Result<bool> {
    if !incoming.is_forum
        || incoming.message_thread_id.is_some()
        || !incoming.images.is_empty()
        || !incoming.audios.is_empty()
    {
        return Ok(false);
    }

    let Some(text) = incoming.text.as_deref() else {
        return Ok(false);
    };
    let Some(command) = parse_command(text) else {
        return Ok(false);
    };
    if !matches!(command, BotCommand::New | BotCommand::WorktreeCreate) {
        return Ok(false);
    }

    let message = match command {
        BotCommand::New => {
            let topic_name = format!("Chat {}", chrono::Utc::now().format("%Y-%m-%d %H:%M"));
            match context
                .client()
                .create_forum_topic(incoming.chat_id, &topic_name)
                .await
            {
                Ok(topic_id) => {
                    let thread_id = thread_id_for_chat(incoming.chat_id, Some(topic_id));
                    if let Err(err) = thread_persistence::Thread::with_id(thread_id.clone())
                        .and_then(|mut thread| thread.set_pending_topic_title(true))
                    {
                        tracing::warn!(
                            chat_id = incoming.chat_id,
                            topic_id,
                            thread_id = %thread_id,
                            %err,
                            "Created empty topic but failed to mark pending auto-title"
                        );
                    }
                    tracing::info!(
                        chat_id = incoming.chat_id,
                        topic_id,
                        topic_name = %topic_name,
                        "Created empty topic from /new in General"
                    );
                }
                Err(err) => {
                    tracing::error!(
                        chat_id = incoming.chat_id,
                        %err,
                        "Failed to create empty topic from /new in General"
                    );
                    context
                        .client()
                        .send_message(
                            incoming.chat_id,
                            "⚠️ I couldn't create a new topic. Please try again.",
                            reply_to_message_id,
                            None,
                        )
                        .await?;
                }
            }
            return Ok(true);
        }
        BotCommand::WorktreeCreate => "/worktree must be used inside a topic, not General.",
        BotCommand::Exit => unreachable!("exit is handled by handle_exit_command"),
        BotCommand::Status => unreachable!("status is handled by handle_status_command"),
        BotCommand::WhereAmI => unreachable!("whereami is handled by handle_whereami_command"),
    };
    context
        .client()
        .send_message(incoming.chat_id, message, reply_to_message_id, None)
        .await?;
    Ok(true)
}

pub(super) async fn handle_exit_command(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    if !incoming
        .text
        .as_deref()
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::Exit)))
    {
        return Ok(false);
    }

    if !zdx_engine::pidfile::is_supervised("bot") {
        context
            .client()
            .send_message(
                incoming.chat_id,
                "⚠️ No active supervisor — refusing to exit. Enable supervision in `zdx monitor` (Ctrl+R on `bot`) first.",
                reply_to_message_id,
                incoming.message_thread_id,
            )
            .await?;
        return Ok(true);
    }

    context
        .client()
        .send_message(
            incoming.chat_id,
            "👋 Exiting… supervisor will restart shortly.",
            reply_to_message_id,
            incoming.message_thread_id,
        )
        .await?;
    context.request_exit();
    Ok(true)
}

async fn handle_model_command(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    thread_id: &str,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    let Some(text) = incoming.text.as_deref() else {
        return Ok(false);
    };
    let Some(subcmd) = crate::commands::parse_model_command(text) else {
        return Ok(false);
    };
    let bot_config = context.config();

    // General context = forum chat but NOT inside a topic thread
    let is_general = incoming.is_forum && incoming.message_thread_id.is_none();

    match subcmd {
        ModelSubcommand::Show | ModelSubcommand::List => {
            let override_model = if is_general {
                None
            } else {
                zdx_engine::core::thread_persistence::read_thread_model_override(thread_id)?
            };
            let current = override_model.as_deref().unwrap_or(&bot_config.model);

            let header = if override_model.is_some() {
                format!(
                    "Current model: <code>{current}</code> (topic override)\nDefault: <code>{}</code>",
                    bot_config.model
                )
            } else {
                format!("Current model: <code>{current}</code>")
            };

            let keyboard = build_provider_keyboard(context, is_general);
            context
                .client()
                .send_message_with_markup(
                    incoming.chat_id,
                    &header,
                    reply_to_message_id,
                    topic_id,
                    &keyboard,
                )
                .await?;
        }
        ModelSubcommand::Set(model_id) => {
            let available = bot_config.subagent_available_models();
            let msg = if !available.iter().any(|m| m == &model_id) {
                format!(
                    "Unknown model: <code>{model_id}</code>\n\nUse /model list to see available models."
                )
            } else if is_general {
                zdx_engine::config::Config::save_telegram_model(&model_id)?;
                context.update_config(|cfg| {
                    cfg.telegram.model.clone_from(&model_id);
                    cfg.model.clone_from(&model_id);
                });
                format!("✅ Default model set to <code>{model_id}</code>.")
            } else {
                let mut thread =
                    zdx_engine::core::thread_persistence::Thread::with_id(thread_id.to_string())
                        .context("open thread")?;
                thread.set_model_override(Some(model_id.clone()))?;
                format!("✅ Model set to <code>{model_id}</code> for this topic.")
            };
            context
                .client()
                .send_message(incoming.chat_id, &msg, reply_to_message_id, topic_id)
                .await?;
        }
        ModelSubcommand::Reset => {
            let msg = if is_general {
                format!(
                    "Default model: <code>{}</code>\n\nUse /model set &lt;id&gt; to change.",
                    bot_config.model
                )
            } else {
                let mut thread =
                    zdx_engine::core::thread_persistence::Thread::with_id(thread_id.to_string())
                        .context("open thread")?;
                thread.set_model_override(None)?;
                format!(
                    "✅ Model reset to default: <code>{}</code>",
                    bot_config.model
                )
            };
            context
                .client()
                .send_message(incoming.chat_id, &msg, reply_to_message_id, topic_id)
                .await?;
        }
    }

    Ok(true)
}

async fn handle_thinking_command(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    thread_id: &str,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    let Some(text) = incoming.text.as_deref() else {
        return Ok(false);
    };
    let Some(subcmd) = crate::commands::parse_thinking_command(text) else {
        return Ok(false);
    };

    let is_general = incoming.is_forum && incoming.message_thread_id.is_none();
    let default_level = context.config().thinking_level;

    let msg = match subcmd {
        ThinkingSubcommand::Show | ThinkingSubcommand::List => {
            let override_level = if is_general {
                None
            } else {
                thread_persistence::read_thread_thinking_override(thread_id)?
            };
            let current = override_level.unwrap_or(default_level);
            let mut msg = if override_level.is_some() {
                format!(
                    "Current thinking: <code>{}</code> (topic override)\nDefault: <code>{}</code>",
                    current.display_name(),
                    default_level.display_name()
                )
            } else {
                format!("Current thinking: <code>{}</code>", current.display_name())
            };
            if matches!(subcmd, ThinkingSubcommand::List | ThinkingSubcommand::Show) {
                if is_general {
                    msg.push_str(
                        "\n\nPick a level below or use <code>/thinking set &lt;level&gt;</code>.",
                    );
                } else {
                    msg.push_str(
                        "\n\nPick a level below, use <code>/thinking set &lt;level&gt;</code>, or <code>/thinking reset</code>.",
                    );
                }
            }

            let keyboard = build_thinking_keyboard(current, is_general);
            context
                .client()
                .send_message_with_markup(
                    incoming.chat_id,
                    &msg,
                    reply_to_message_id,
                    topic_id,
                    &keyboard,
                )
                .await?;
            return Ok(true);
        }
        ThinkingSubcommand::Set(level) => {
            if is_general {
                zdx_engine::config::Config::save_telegram_thinking_level(level)?;
                context.update_config(|cfg| {
                    cfg.telegram.thinking_level = level;
                    cfg.thinking_level = level;
                });
                format!(
                    "✅ Default thinking set to <code>{}</code>.",
                    level.display_name()
                )
            } else {
                let mut thread =
                    zdx_engine::core::thread_persistence::Thread::with_id(thread_id.to_string())
                        .context("open thread")?;
                thread.set_thinking_override(Some(level))?;
                format!(
                    "✅ Thinking set to <code>{}</code> for this topic.",
                    level.display_name()
                )
            }
        }
        ThinkingSubcommand::Reset => {
            if is_general {
                format!(
                    "Default thinking: <code>{}</code>\n\nUse <code>/thinking set &lt;level&gt;</code> to change.",
                    default_level.display_name()
                )
            } else {
                let mut thread =
                    zdx_engine::core::thread_persistence::Thread::with_id(thread_id.to_string())
                        .context("open thread")?;
                thread.set_thinking_override(None)?;
                format!(
                    "✅ Thinking reset to default: <code>{}</code>",
                    default_level.display_name()
                )
            }
        }
    };

    context
        .client()
        .send_message(incoming.chat_id, &msg, reply_to_message_id, topic_id)
        .await?;

    Ok(true)
}

async fn handle_status_command(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    thread_id: &str,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    if !incoming
        .text
        .as_deref()
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::Status)))
    {
        return Ok(false);
    }

    let config = context.config();
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let root_path = thread_persistence::read_thread_root_path(thread_id)?
        .map_or_else(|| resolved_root.root.clone(), PathBuf::from);
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let effective_model = model_override.as_deref().unwrap_or(&config.model);
    let effective_thinking = thinking_override.unwrap_or(config.thinking_level);
    let branch = git_branch_name(&root_path).await;
    let events = thread_persistence::load_thread_events(thread_id)?;
    let (cumulative_usage, latest_usage) =
        thread_persistence::extract_usage_from_thread_events(&events);
    let message = format_status_message(&StatusSnapshot {
        model_id: effective_model,
        model_override: model_override.as_deref(),
        thinking: effective_thinking,
        thinking_override,
        profile_name: resolved_root.profile_name.as_deref(),
        thread_id,
        root_path: &root_path,
        branch: branch.as_deref(),
        cumulative_usage,
        latest_usage,
    });

    context
        .client()
        .send_message(incoming.chat_id, &message, reply_to_message_id, topic_id)
        .await?;

    Ok(true)
}

async fn handle_whereami_command(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    if !incoming
        .text
        .as_deref()
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::WhereAmI)))
    {
        return Ok(false);
    }

    let resolved_root = context.root_for_chat(incoming.chat_id);
    // Private DMs to the bot use chat_id == user_id and are trusted; group
    // chats need to be on the allowlist for full info (CWD).
    let is_private = incoming.chat_id == incoming.user_id;
    let chat_allowlisted = context.allowlist_chat_ids().contains(&incoming.chat_id);
    let discovery_mode = !is_private && !chat_allowlisted;

    let message = format_whereami_message(
        incoming.chat_id,
        topic_id,
        resolved_root.profile_name.as_deref(),
        &resolved_root.root,
        discovery_mode,
    );

    context
        .client()
        .send_message(incoming.chat_id, &message, reply_to_message_id, topic_id)
        .await?;

    Ok(true)
}

pub(super) fn format_whereami_message(
    chat_id: i64,
    topic_id: Option<i64>,
    profile_name: Option<&str>,
    root_path: &Path,
    discovery_mode: bool,
) -> String {
    let mut lines = vec!["<b>Where am I</b>".to_string()];
    lines.push(format!("Chat ID: <code>{chat_id}</code>"));
    if let Some(topic_id) = topic_id {
        lines.push(format!("Topic ID: <code>{topic_id}</code>"));
    }

    if discovery_mode {
        // Chat is not on the bot allowlist. Keep the reply minimal: do not
        // disclose the bot's filesystem (cwd) or profile state to a chat the
        // operator has not yet opted in.
        lines.push("Status: <code>chat not on bot allowlist</code>".to_string());
        lines.push(format!(
            "Allow this chat by adding <code>{chat_id}</code> to <code>telegram.allowlist_chat_ids</code> in <code>config.toml</code>, then restart the bot."
        ));
        lines.push(format!(
            "Bind workspace: <code>zdx bot profile add &lt;name&gt; {chat_id} &lt;cwd&gt;</code>"
        ));
        return lines.join("\n");
    }

    let root_display = root_path.display().to_string();
    if let Some(name) = profile_name {
        lines.push(format!("Profile: <code>{}</code>", escape_html(name)));
        lines.push(format!("CWD: <code>{}</code>", escape_html(&root_display)));
    } else {
        lines.push("Profile: <code>none</code> (fallback root)".to_string());
        lines.push(format!("CWD: <code>{}</code>", escape_html(&root_display)));
        lines.push(format!(
            "Bind this chat with: <code>zdx bot profile add &lt;name&gt; {chat_id} &lt;cwd&gt;</code>"
        ));
    }
    lines.join("\n")
}

/// Build an inline keyboard showing provider names as buttons.
/// Callback data format: `model_provider:{provider}:{scope}` where scope is `general` or `topic`.
pub(crate) fn build_provider_keyboard(
    context: &BotContext,
    is_general: bool,
) -> InlineKeyboardMarkup {
    let models = context.config().subagent_available_models();
    let scope = if is_general { "general" } else { "topic" };

    // Extract unique providers (part before ':')
    let mut providers: Vec<String> = Vec::new();
    for m in &models {
        let provider = m.split(':').next().unwrap_or(m).to_string();
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }

    let mut rows: Vec<Vec<InlineKeyboardButton>> = providers
        .chunks(3)
        .map(|chunk| {
            chunk
                .iter()
                .map(|p| InlineKeyboardButton {
                    text: p.clone(),
                    callback_data: Some(format!("model_provider:{p}:{scope}")),
                    url: None,
                })
                .collect()
        })
        .collect();

    rows.push(vec![InlineKeyboardButton {
        text: "✖ Cancel".to_string(),
        callback_data: Some(format!("model_cancel:{scope}")),
        url: None,
    }]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

pub(crate) fn models_for_provider(context: &BotContext, provider: &str) -> Vec<String> {
    context
        .config()
        .subagent_available_models()
        .into_iter()
        .filter(|m| m.starts_with(&format!("{provider}:")))
        .collect()
}

/// Build an inline keyboard showing model names for a specific provider.
/// Callback data format: `model_pick:{provider}:{index}:{scope}`.
pub(crate) fn build_models_keyboard(
    context: &BotContext,
    provider: &str,
    is_general: bool,
) -> InlineKeyboardMarkup {
    let scope = if is_general { "general" } else { "topic" };
    let filtered = models_for_provider(context, provider);

    let indexed: Vec<(usize, &String)> = filtered.iter().enumerate().collect();

    let mut rows: Vec<Vec<InlineKeyboardButton>> = indexed
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(index, m)| {
                    // Display just the model part (after provider:)
                    let display = m.split(':').nth(1).unwrap_or(m);
                    InlineKeyboardButton {
                        text: display.to_string(),
                        callback_data: Some(format!("model_pick:{provider}:{index}:{scope}")),
                        url: None,
                    }
                })
                .collect()
        })
        .collect();

    // Add a "← Back" button
    rows.push(vec![InlineKeyboardButton {
        text: "← Back".to_string(),
        callback_data: Some(format!("model_back:{scope}")),
        url: None,
    }]);

    rows.push(vec![InlineKeyboardButton {
        text: "✖ Cancel".to_string(),
        callback_data: Some(format!("model_cancel:{scope}")),
        url: None,
    }]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

/// Build an inline keyboard showing thinking levels.
/// Callback data format: `thinking_set:{level}:{scope}`.
pub(crate) fn build_thinking_keyboard(
    current: ThinkingLevel,
    is_general: bool,
) -> InlineKeyboardMarkup {
    let scope = if is_general { "general" } else { "topic" };

    let mut rows: Vec<Vec<InlineKeyboardButton>> = ThinkingLevel::all()
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|level| {
                    let prefix = if *level == current { "✅ " } else { "" };
                    InlineKeyboardButton {
                        text: format!("{prefix}{}", level.display_name()),
                        callback_data: Some(format!(
                            "thinking_set:{}:{scope}",
                            level.display_name()
                        )),
                        url: None,
                    }
                })
                .collect()
        })
        .collect();

    if !is_general {
        rows.push(vec![InlineKeyboardButton {
            text: "↺ Use default".to_string(),
            callback_data: Some("thinking_reset:topic".to_string()),
            url: None,
        }]);
    }

    rows.push(vec![InlineKeyboardButton {
        text: "✖ Cancel".to_string(),
        callback_data: Some(format!("thinking_cancel:{scope}")),
        url: None,
    }]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

async fn handle_thread_commands(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    thread_id: &str,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    let Some(text) = incoming.text.as_deref() else {
        return Ok(false);
    };
    let Some(command) = parse_command(text) else {
        return Ok(false);
    };

    match command {
        BotCommand::New => {
            if incoming.is_forum && topic_id.is_some() {
                context
                    .client()
                    .send_message(
                        incoming.chat_id,
                        "/new is not allowed in topics.",
                        reply_to_message_id,
                        topic_id,
                    )
                    .await?;
                return Ok(true);
            }
            agent::clear_thread_history(thread_id)?;
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    "History cleared. Start a new conversation anytime.",
                    reply_to_message_id,
                    topic_id,
                )
                .await?;
            return Ok(true);
        }
        BotCommand::Exit | BotCommand::Status | BotCommand::WhereAmI => return Ok(false),
        BotCommand::WorktreeCreate => {}
    }

    let resolved_root = context.root_for_chat(incoming.chat_id);
    let worktree_root = match worktree::ensure_worktree(&resolved_root.root, thread_id) {
        Ok(path) => path,
        Err(err) => {
            let msg = format!(
                "Failed to enable worktree: {err}\n\nTip: start the bot from inside a git repo (or a subdirectory of one)."
            );
            context
                .client()
                .send_message(incoming.chat_id, &msg, reply_to_message_id, topic_id)
                .await?;
            return Ok(true);
        }
    };

    let mut thread = zdx_engine::core::thread_persistence::Thread::with_id(thread_id.to_string())
        .context("open thread log")?;
    if let Err(err) = thread.set_root_path(&worktree_root) {
        context
            .client()
            .send_message(
                incoming.chat_id,
                &format!("Failed to persist worktree root: {err}"),
                reply_to_message_id,
                topic_id,
            )
            .await?;
        return Ok(true);
    }

    context
        .client()
        .send_message(
            incoming.chat_id,
            &format!("Worktree enabled: {}", worktree_root.display()),
            reply_to_message_id,
            topic_id,
        )
        .await?;
    Ok(true)
}

async fn git_branch_name(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("branch")
        .arg("--show-current")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}
