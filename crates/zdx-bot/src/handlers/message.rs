use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use zdx_core::config::ThinkingLevel;
use zdx_core::core::events::{AgentEvent, TurnStatus as AgentTurnStatus};
use zdx_core::core::thread_persistence::{self, ThreadEvent};
use zdx_core::core::worktree;

use crate::bot::context::BotContext;
use crate::commands::{BotCommand, ModelSubcommand, ThinkingSubcommand, parse_command};
use crate::ingest::AllowlistConfig;
use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, Message, ReplyParameters};
use crate::{agent, ingest};

/// Groups the reply-targeting fields that travel together through the turn pipeline.
struct ReplyContext {
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    cross_topic_reply_parameters: Option<ReplyParameters>,
}

/// Minimum interval between Telegram status message edits (avoid rate limiting).
const STATUS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(3);
const MEDIA_BLOCK_OPEN: &str = "<medias>";
const MEDIA_BLOCK_CLOSE: &str = "</medias>";
const MEDIA_TAG_OPEN: &str = "<media";
const MEDIA_TAG_CLOSE: &str = "</media>";

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) async fn handle_message(context: &BotContext, message: Message) -> Result<()> {
    let bot_config = context.config();
    let synthetic_topic_routed_from_general = message.synthetic_topic_routed_from_general;
    let provisional_status = if message_has_audio(&message) {
        Some(
            setup_preprocessing_status(context, &message, synthetic_topic_routed_from_general)
                .await,
        )
    } else {
        None
    };
    let allowlist = AllowlistConfig {
        user_ids: context.allowlist_user_ids(),
        chat_ids: context.allowlist_chat_ids(),
    };
    let Some(incoming) = parse_message_with_status(
        context,
        allowlist,
        &bot_config,
        message,
        provisional_status.as_ref(),
    )
    .await?
    else {
        cleanup_provisional_status(context, None, provisional_status).await;
        return Ok(());
    };

    let reply_ctx = build_reply_context(&incoming, synthetic_topic_routed_from_general);

    if handle_pre_agent_commands(context, &incoming, &reply_ctx).await? {
        cleanup_provisional_status(context, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    tracing::info!(
        user_id = incoming.user_id,
        chat_id = incoming.chat_id,
        topic_id = ?reply_ctx.topic_id,
        "Accepted message",
    );

    let thread_id = thread_id_for_chat(incoming.chat_id, reply_ctx.topic_id);
    if handle_thread_setup_commands(context, &incoming, &reply_ctx, &thread_id).await? {
        cleanup_provisional_status(context, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    run_agent_turn(
        context,
        incoming,
        reply_ctx,
        &thread_id,
        synthetic_topic_routed_from_general,
        provisional_status,
    )
    .await
}

async fn parse_message_with_status(
    context: &BotContext,
    allowlist: AllowlistConfig<'_>,
    bot_config: &zdx_core::config::Config,
    message: Message,
    provisional_status: Option<&TurnStatus>,
) -> Result<Option<crate::types::IncomingMessage>> {
    match ingest::parse_incoming_message(
        context.client(),
        allowlist,
        bot_config,
        message,
        provisional_status.map(|status| &status.token),
    )
    .await
    {
        Ok(incoming) => Ok(incoming),
        Err(err) if crate::transcribe::is_operation_cancelled(&err) => {
            if let Some(status) = provisional_status {
                finalize_preprocessing_cancelled(context, status.key.0, status).await;
            }
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn build_reply_context(
    incoming: &crate::types::IncomingMessage,
    synthetic_topic_routed_from_general: bool,
) -> ReplyContext {
    let reply_to_message_id = if synthetic_topic_routed_from_general
        || incoming.message_thread_id == Some(incoming.message_id)
    {
        None
    } else {
        Some(incoming.message_id)
    };
    let topic_id = incoming.message_thread_id;
    let cross_topic_reply_parameters = if synthetic_topic_routed_from_general && topic_id.is_some()
    {
        Some(ReplyParameters {
            message_id: incoming.message_id,
            chat_id: Some(incoming.chat_id),
            allow_sending_without_reply: Some(true),
        })
    } else {
        None
    };

    ReplyContext {
        reply_to_message_id,
        topic_id,
        cross_topic_reply_parameters,
    }
}

async fn handle_pre_agent_commands(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
) -> Result<bool> {
    Ok(
        handle_general_forum_commands(context, incoming, reply_ctx.reply_to_message_id).await?
            || handle_rebuild_command(context, incoming, reply_ctx.reply_to_message_id).await?,
    )
}

async fn handle_thread_setup_commands(
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
        || handle_thread_commands(
            context,
            incoming,
            thread_id,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?)
}

async fn cleanup_provisional_status(
    context: &BotContext,
    chat_id: Option<i64>,
    provisional_status: Option<TurnStatus>,
) {
    if let Some(status) = provisional_status {
        discard_turn_status(context, chat_id, &status).await;
    }
}

struct TurnStatus {
    key: (i64, i64),
    token: CancellationToken,
    markup: InlineKeyboardMarkup,
    message_id: Option<i64>,
}

fn message_has_audio(message: &Message) -> bool {
    message.voice.is_some()
        || message.audio.is_some()
        || message
            .document
            .as_ref()
            .and_then(|doc| doc.mime_type.as_deref())
            .is_some_and(|mime| mime.starts_with("audio/"))
}

struct TurnResult {
    final_text: String,
    got_result: bool,
    had_error: bool,
    error_message: Option<String>,
}

struct SpawnRequest<'a> {
    worktree_root: &'a std::path::Path,
    thread_id: &'a str,
    thread: &'a zdx_core::core::thread_persistence::Thread,
    messages: Vec<zdx_core::providers::ChatMessage>,
    config: &'a zdx_core::config::Config,
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn format_user_error_message(message: &str) -> String {
    let trimmed = message.trim();
    let compact = if trimmed.len() > 700 {
        format!("{}…", trimmed.chars().take(700).collect::<String>())
    } else {
        trimmed.to_string()
    };
    format!(
        "❌ Request failed.\n\n<blockquote><code>{}</code></blockquote>",
        escape_html(&compact)
    )
}

async fn handle_general_forum_commands(
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
        BotCommand::Rebuild => unreachable!("rebuild is handled by handle_rebuild_command"),
    };
    context
        .client()
        .send_message(incoming.chat_id, message, reply_to_message_id, None)
        .await?;
    Ok(true)
}

async fn handle_rebuild_command(
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
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::Rebuild)))
    {
        return Ok(false);
    }

    context
        .client()
        .send_message(
            incoming.chat_id,
            "♻️ Rebuilding bot… coming back shortly.",
            reply_to_message_id,
            incoming.message_thread_id,
        )
        .await?;
    context.request_rebuild();
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
                zdx_core::core::thread_persistence::read_thread_model_override(thread_id)?
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
                zdx_core::config::Config::save_telegram_model(&model_id)?;
                context.update_config(|cfg| {
                    cfg.telegram.model.clone_from(&model_id);
                    cfg.model.clone_from(&model_id);
                });
                format!("✅ Default model set to <code>{model_id}</code>.")
            } else {
                let mut thread =
                    zdx_core::core::thread_persistence::Thread::with_id(thread_id.to_string())
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
                    zdx_core::core::thread_persistence::Thread::with_id(thread_id.to_string())
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
                zdx_core::config::Config::save_telegram_thinking_level(level)?;
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
                    zdx_core::core::thread_persistence::Thread::with_id(thread_id.to_string())
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
                    zdx_core::core::thread_persistence::Thread::with_id(thread_id.to_string())
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
        BotCommand::Rebuild => return Ok(false),
        BotCommand::WorktreeCreate => {}
    }

    let worktree_root = match worktree::ensure_worktree(context.root(), thread_id) {
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

    let mut thread = zdx_core::core::thread_persistence::Thread::with_id(thread_id.to_string())
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

async fn run_agent_turn(
    context: &BotContext,
    incoming: crate::types::IncomingMessage,
    reply_ctx: ReplyContext,
    thread_id: &str,
    synthetic_topic_routed_from_general: bool,
    provisional_status: Option<TurnStatus>,
) -> Result<()> {
    let worktree_root = thread_persistence::read_thread_root_path(thread_id)?
        .map_or_else(|| context.root().to_path_buf(), std::path::PathBuf::from);
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let config = if model_override.is_some() || thinking_override.is_some() {
        let mut cfg = context.config();
        if let Some(ref model_id) = model_override {
            cfg.model.clone_from(model_id);
        }
        if let Some(level) = thinking_override {
            cfg.thinking_level = level;
        }
        cfg
    } else {
        context.config()
    };
    let (mut thread, mut messages) = agent::load_thread_state(thread_id)?;
    let pending_topic_title = thread_persistence::read_thread_pending_topic_title(thread_id)?;
    agent::record_user_message(&mut thread, &mut messages, &incoming)?;

    // Async topic title: spawn LLM-based title generation + rename for new topics.
    // This runs only after the user message is persisted, so the thread file exists.
    if (synthetic_topic_routed_from_general || pending_topic_title)
        && let Some(topic_id) = reply_ctx.topic_id
    {
        let effective_text = incoming
            .text
            .as_deref()
            .or_else(|| incoming.audios.iter().find_map(|a| a.transcript.as_deref()))
            .filter(|t| !t.trim().is_empty());

        if let Some(text) = effective_text {
            if pending_topic_title {
                thread.set_pending_topic_title(false)?;
            }
            crate::topic_title::spawn_topic_title_update(
                context,
                incoming.chat_id,
                topic_id,
                text.to_string(),
            );
        }
    }

    let typing = context
        .client()
        .start_typing(incoming.chat_id, reply_ctx.topic_id);

    let status = setup_turn_status(
        context,
        &incoming,
        reply_ctx.reply_to_message_id,
        reply_ctx.topic_id,
        provisional_status,
    )
    .await;
    let spawn = SpawnRequest {
        worktree_root: &worktree_root,
        thread_id,
        thread: &thread,
        messages,
        config: &config,
    };
    let mut handle = spawn_or_fail(context, &incoming, &status, spawn).await?;
    let result = stream_turn_events(context, &incoming, &mut handle, &status).await;
    drop(typing);
    cleanup_turn_status(context, &status).await;
    finalize_turn(context, &incoming, &reply_ctx, &mut thread, &status, result).await
}

async fn setup_turn_status(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    existing: Option<TurnStatus>,
) -> TurnStatus {
    if let Some(status) = existing {
        update_turn_status_text(context, incoming.chat_id, &status, agent::STATUS_WAITING).await;
        return status;
    }

    let key = (incoming.chat_id, incoming.message_id);
    let cancel_markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "⏹ Cancel".to_string(),
            callback_data: Some(format!("cancel:{}:{}", key.0, key.1)),
            url: None,
        }]],
    };

    let token = CancellationToken::new();
    {
        let mut map = context.cancel_map().lock().await;
        map.insert(key, token.clone());
    }

    let mut message_id = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            agent::STATUS_WAITING,
            reply_to_message_id,
            topic_id,
            &cancel_markup,
        )
        .await
        .ok()
        .map(|m| m.id);

    // Retry without reply_to on REPLY_MESSAGE_ID_INVALID
    if message_id.is_none() && reply_to_message_id.is_some() {
        message_id = context
            .client()
            .send_message_with_markup(
                incoming.chat_id,
                agent::STATUS_WAITING,
                None,
                topic_id,
                &cancel_markup,
            )
            .await
            .ok()
            .map(|m| m.id);
    }

    TurnStatus {
        key,
        token,
        markup: cancel_markup,
        message_id,
    }
}

async fn setup_preprocessing_status(
    context: &BotContext,
    message: &Message,
    synthetic_topic_routed_from_general: bool,
) -> TurnStatus {
    let key = (message.chat.id, message.id);
    let cancel_markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "⏹ Cancel".to_string(),
            callback_data: Some(format!("cancel:{}:{}", key.0, key.1)),
            url: None,
        }]],
    };
    let token = CancellationToken::new();
    {
        let mut map = context.cancel_map().lock().await;
        map.insert(key, token.clone());
    }

    let reply_to_message_id = if synthetic_topic_routed_from_general
        || message.effective_thread_id() == Some(message.id)
    {
        None
    } else {
        Some(message.id)
    };

    let mut message_id = context
        .client()
        .send_message_with_markup(
            message.chat.id,
            agent::STATUS_TRANSCRIBING,
            reply_to_message_id,
            message.effective_thread_id(),
            &cancel_markup,
        )
        .await
        .ok()
        .map(|m| m.id);

    if message_id.is_none() && reply_to_message_id.is_some() {
        message_id = context
            .client()
            .send_message_with_markup(
                message.chat.id,
                agent::STATUS_TRANSCRIBING,
                None,
                message.effective_thread_id(),
                &cancel_markup,
            )
            .await
            .ok()
            .map(|m| m.id);
    }

    TurnStatus {
        key,
        token,
        markup: cancel_markup,
        message_id,
    }
}

async fn update_turn_status_text(
    context: &BotContext,
    chat_id: i64,
    status: &TurnStatus,
    text: &str,
) {
    let Some(msg_id) = status.message_id else {
        return;
    };
    let _ = context
        .client()
        .edit_message_text(chat_id, msg_id, text, Some(&status.markup))
        .await;
}

async fn discard_turn_status(context: &BotContext, chat_id: Option<i64>, status: &TurnStatus) {
    if let (Some(chat_id), Some(msg_id)) = (chat_id, status.message_id) {
        let _ = context.client().delete_message(chat_id, msg_id).await;
    }
    cleanup_turn_status(context, status).await;
}

async fn finalize_preprocessing_cancelled(context: &BotContext, chat_id: i64, status: &TurnStatus) {
    update_turn_status_text(context, chat_id, status, "Cancelled ✓").await;
    cleanup_turn_status(context, status).await;
}

async fn spawn_or_fail(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    status: &TurnStatus,
    spawn: SpawnRequest<'_>,
) -> Result<agent::AgentTurnHandle> {
    let handle = agent::spawn_agent_turn(
        spawn.messages,
        spawn.config,
        spawn.worktree_root,
        context.bot_surface_rules(),
        spawn.thread_id,
        spawn.thread,
        context.tool_config(),
    );

    match handle {
        Ok(handle) => Ok(handle),
        Err(err) => {
            tracing::error!(%err, "Failed to spawn agent turn");
            if let Some(msg_id) = status.message_id {
                let _ = context
                    .client()
                    .edit_message_text(
                        incoming.chat_id,
                        msg_id,
                        &format_user_error_message(&err.to_string()),
                        None,
                    )
                    .await;
            }
            cleanup_turn_status(context, status).await;
            Err(err)
        }
    }
}

async fn stream_turn_events(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    handle: &mut agent::AgentTurnHandle,
    status: &TurnStatus,
) -> TurnResult {
    let mut current_status = agent::STATUS_WAITING.to_string();
    let mut last_edit = std::time::Instant::now()
        .checked_sub(STATUS_DEBOUNCE)
        .expect("debounce subtraction should always succeed");
    let mut final_text = String::new();
    let mut got_result = false;
    let mut had_error = false;
    let mut error_message = None;

    loop {
        tokio::select! {
            biased;
            () = status.token.cancelled() => {
                handle.task.abort();
                break;
            }
            event = handle.rx.recv() => {
                let Some(event) = event else { break; };
                match &*event {
                    AgentEvent::TurnFinished {
                        status,
                        final_text: text,
                        ..
                    } => {
                        final_text.clone_from(text);
                        match status {
                            AgentTurnStatus::Completed => {
                                got_result = true;
                            }
                            AgentTurnStatus::Interrupted => {}
                            AgentTurnStatus::Failed { message, .. } => {
                                had_error = true;
                                error_message = Some(message.clone());
                            }
                        }
                        break;
                    }
                    AgentEvent::Error { message, .. } => {
                        tracing::error!(message, "Agent error event");
                        // Diagnostic only; terminal outcome is carried by TurnFinished.
                    }
                    other => update_status(context, incoming.chat_id, status, other, &mut current_status, &mut last_edit).await,
                }
            }
        }
    }

    TurnResult {
        final_text,
        got_result,
        had_error,
        error_message,
    }
}

async fn update_status(
    context: &BotContext,
    chat_id: i64,
    status: &TurnStatus,
    event: &AgentEvent,
    current_status: &mut String,
    last_edit: &mut std::time::Instant,
) {
    let Some(new_status) = agent::event_to_status(event) else {
        return;
    };
    if new_status == *current_status {
        return;
    }

    *current_status = new_status;
    if status.message_id.is_none() {
        return;
    }
    let now = std::time::Instant::now();
    if now.duration_since(*last_edit) < STATUS_DEBOUNCE {
        return;
    }
    *last_edit = now;
    update_turn_status_text(context, chat_id, status, current_status).await;
}

async fn cleanup_turn_status(context: &BotContext, status: &TurnStatus) {
    let mut map = context.cancel_map().lock().await;
    map.remove(&status.key);
}

async fn finalize_turn(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    thread: &mut zdx_core::core::thread_persistence::Thread,
    status: &TurnStatus,
    result: TurnResult,
) -> Result<()> {
    if status.token.is_cancelled() {
        tracing::info!(
            chat_id = incoming.chat_id,
            topic_id = ?reply_ctx.topic_id,
            "Agent turn cancelled",
        );
        if let Some(msg_id) = status.message_id {
            let _ = context
                .client()
                .edit_message_text(incoming.chat_id, msg_id, "Cancelled ✓", None)
                .await;
        }
        return Ok(());
    }

    if result.had_error && !result.got_result {
        if let Some(msg_id) = status.message_id {
            let error_text = result.error_message.as_deref().map_or_else(
                || "Sorry, something went wrong.".to_string(),
                format_user_error_message,
            );
            let _ = context
                .client()
                .edit_message_text(incoming.chat_id, msg_id, &error_text, None)
                .await;
        }
        return Ok(());
    }

    if result.got_result {
        thread
            .append(&ThreadEvent::assistant_message_with_phase(
                &result.final_text,
                Some("final_answer".to_string()),
            ))
            .context("append assistant message")?;
    }

    send_final_response(
        context,
        incoming,
        reply_ctx,
        status.message_id,
        &result.final_text,
    )
    .await
}

async fn send_final_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    status_message_id: Option<i64>,
    final_text: &str,
) -> Result<()> {
    let parsed = parse_final_response(final_text);
    let has_text = !parsed.text.trim().is_empty();

    if !has_text && parsed.media_paths.is_empty() {
        if let Some(msg_id) = status_message_id
            && let Err(err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
        {
            tracing::warn!(msg_id, %err, "Failed to delete empty status message");
        }
        return Ok(());
    }

    if has_text {
        send_text_response(
            context,
            incoming,
            reply_ctx,
            status_message_id,
            parsed.text.as_str(),
        )
        .await?;
    } else if let Some(msg_id) = status_message_id
        && let Err(err) = context
            .client()
            .delete_message(incoming.chat_id, msg_id)
            .await
    {
        tracing::warn!(msg_id, %err, "Failed to delete empty status message");
    }

    send_media_responses(context, incoming, reply_ctx, &parsed.media_paths, has_text).await
}

async fn send_text_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    status_message_id: Option<i64>,
    text: &str,
) -> Result<()> {
    tracing::info!(chat_id = incoming.chat_id, "Sending reply");

    if let Some(ref reply_parameters) = reply_ctx.cross_topic_reply_parameters {
        if let Some(msg_id) = status_message_id
            && let Err(err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
        {
            tracing::warn!(msg_id, %err, "Failed to delete status message");
        }

        context
            .client()
            .send_message_with_reply_params(
                incoming.chat_id,
                text,
                reply_ctx.topic_id,
                Some(reply_parameters.clone()),
            )
            .await?;
        return Ok(());
    }

    if let Some(msg_id) = status_message_id {
        let edit_result = context
            .client()
            .edit_message_text(incoming.chat_id, msg_id, text, None)
            .await;
        if let Err(ref err) = edit_result {
            tracing::warn!(msg_id, chat_id = incoming.chat_id, %err, "Failed to edit status message");
            if let Err(del_err) = context
                .client()
                .delete_message(incoming.chat_id, msg_id)
                .await
            {
                tracing::warn!(msg_id, err = %del_err, "Failed to delete stale status message");
            }
            let send_result = context
                .client()
                .send_message(
                    incoming.chat_id,
                    text,
                    reply_ctx.reply_to_message_id,
                    reply_ctx.topic_id,
                )
                .await;
            if let Err(ref e) = send_result {
                if e.to_string().contains("REPLY_MESSAGE_ID_INVALID") {
                    context
                        .client()
                        .send_message(incoming.chat_id, text, None, reply_ctx.topic_id)
                        .await?;
                } else {
                    send_result?;
                }
            }
        }
    } else {
        let send_result = context
            .client()
            .send_message(
                incoming.chat_id,
                text,
                reply_ctx.reply_to_message_id,
                reply_ctx.topic_id,
            )
            .await;
        if let Err(ref e) = send_result {
            if e.to_string().contains("REPLY_MESSAGE_ID_INVALID") {
                context
                    .client()
                    .send_message(incoming.chat_id, text, None, reply_ctx.topic_id)
                    .await?;
            } else {
                send_result?;
            }
        }
    }

    Ok(())
}

async fn send_media_responses(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    media_paths: &[PathBuf],
    sent_text: bool,
) -> Result<()> {
    if media_paths.is_empty() {
        return Ok(());
    }

    let valid_media_paths: Vec<PathBuf> = media_paths
        .iter()
        .filter(|path| is_valid_media_path(path))
        .cloned()
        .collect();

    if valid_media_paths.is_empty() {
        if !sent_text {
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    "I couldn't find a valid local media file to send.",
                    None,
                    reply_ctx.topic_id,
                )
                .await?;
        }
        return Ok(());
    }

    for media_path in valid_media_paths {
        let reply_parameters = reply_ctx.cross_topic_reply_parameters.clone();
        let reply_to_message_id = if reply_parameters.is_some() {
            None
        } else {
            reply_ctx.reply_to_message_id
        };

        let send_result = if is_image_path(&media_path) {
            context
                .client()
                .send_photo_from_path(
                    incoming.chat_id,
                    &media_path,
                    None,
                    reply_to_message_id,
                    reply_ctx.topic_id,
                    reply_parameters,
                )
                .await
        } else {
            context
                .client()
                .send_document_from_path(
                    incoming.chat_id,
                    &media_path,
                    None,
                    reply_to_message_id,
                    reply_ctx.topic_id,
                    reply_parameters,
                )
                .await
        };

        if let Err(err) = send_result {
            tracing::error!(path = %media_path.display(), %err, "Failed to send media file");
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    &format!("Failed to send media file {}: {err}", media_path.display()),
                    None,
                    reply_ctx.topic_id,
                )
                .await?;
        }
    }

    Ok(())
}

#[derive(Default)]
struct ParsedFinalResponse {
    text: String,
    media_paths: Vec<PathBuf>,
}

fn parse_final_response(final_text: &str) -> ParsedFinalResponse {
    let text_without_wrappers = strip_media_wrappers(final_text);
    let (text_without_media_tags, raw_media_values) = extract_media_tags(&text_without_wrappers);
    let mut media_paths = Vec::new();
    let mut seen = HashSet::new();

    for raw in raw_media_values {
        if let Some(path) = parse_media_path(&raw)
            && seen.insert(path.clone())
        {
            media_paths.push(path);
        }
    }

    ParsedFinalResponse {
        text: normalize_reply_text(&text_without_media_tags),
        media_paths,
    }
}

fn strip_media_wrappers(input: &str) -> String {
    input
        .replace(MEDIA_BLOCK_OPEN, "")
        .replace(MEDIA_BLOCK_CLOSE, "")
}

fn extract_media_tags(input: &str) -> (String, Vec<String>) {
    let mut cleaned = String::new();
    let mut media_values = Vec::new();
    let mut cursor = 0;

    while let Some(start_rel) = input[cursor..].find(MEDIA_TAG_OPEN) {
        let start = cursor + start_rel;
        let Some(after_tag_name) = input.as_bytes().get(start + MEDIA_TAG_OPEN.len()) else {
            break;
        };
        // Skip accidental matches such as "<medias>".
        if *after_tag_name == b's' {
            let skip_to = start + MEDIA_TAG_OPEN.len();
            cleaned.push_str(&input[cursor..skip_to]);
            cursor = skip_to;
            continue;
        }

        cleaned.push_str(&input[cursor..start]);

        let Some(open_end_rel) = input[start..].find('>') else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let open_end = start + open_end_rel;
        let open_tag = &input[start..=open_end];

        if open_tag.ends_with("/>") {
            cursor = open_end + 1;
            continue;
        }

        let content_start = open_end + 1;
        let Some(close_rel) = input[content_start..].find(MEDIA_TAG_CLOSE) else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let content_end = content_start + close_rel;
        let inner = input[content_start..content_end].trim();
        if !inner.is_empty() {
            media_values.push(inner.to_string());
        }

        cursor = content_end + MEDIA_TAG_CLOSE.len();
    }

    if cursor < input.len() {
        cleaned.push_str(&input[cursor..]);
    }

    (cleaned, media_values)
}

fn normalize_reply_text(text: &str) -> String {
    let mut out = String::new();
    let mut prev_blank = false;

    for line in text.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }

        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        prev_blank = is_blank;
    }

    out.trim().to_string()
}

fn parse_media_path(raw: &str) -> Option<PathBuf> {
    let candidate = raw
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim_end_matches([',', ';']);

    if candidate.starts_with('/') {
        Some(PathBuf::from(candidate))
    } else {
        None
    }
}

fn is_valid_media_path(path: &Path) -> bool {
    path.is_absolute() && std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp"
            )
        })
}

fn thread_id_for_chat(chat_id: i64, message_thread_id: Option<i64>) -> String {
    match message_thread_id {
        Some(topic_id) => format!("telegram-{chat_id}-topic-{topic_id}"),
        None => format!("telegram-{chat_id}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{is_image_path, parse_final_response};

    #[test]
    fn parse_final_response_extracts_media_wrapper_format() {
        let parsed = parse_final_response("Done.\n<medias><media>/tmp/out.png</media></medias>");
        assert_eq!(parsed.text, "Done.");
        assert_eq!(parsed.media_paths, vec![PathBuf::from("/tmp/out.png")]);
    }

    #[test]
    fn parse_final_response_extracts_multiple_media_entries() {
        let parsed = parse_final_response(
            "<medias><media>/tmp/one.png</media><media>/tmp/two.pdf</media></medias>",
        );
        assert!(parsed.text.is_empty());
        assert_eq!(
            parsed.media_paths,
            vec![PathBuf::from("/tmp/one.png"), PathBuf::from("/tmp/two.pdf")]
        );
    }

    #[test]
    fn parse_final_response_extracts_bare_media_entries_without_wrapper() {
        let parsed =
            parse_final_response("Done.\n<media>/tmp/one.png</media>\n<media>/tmp/two.pdf</media>");
        assert_eq!(parsed.text, "Done.");
        assert_eq!(
            parsed.media_paths,
            vec![PathBuf::from("/tmp/one.png"), PathBuf::from("/tmp/two.pdf")]
        );
    }

    #[test]
    fn parse_final_response_ignores_media_path_attribute_format() {
        let parsed = parse_final_response("<media path=\"/tmp/out.png\"/>");
        assert!(parsed.text.is_empty());
        assert!(parsed.media_paths.is_empty());
    }

    #[test]
    fn parse_final_response_ignores_plain_absolute_paths_without_media_xml() {
        let parsed = parse_final_response("/tmp/report.pdf");
        assert_eq!(parsed.text, "/tmp/report.pdf");
        assert!(parsed.media_paths.is_empty());
    }

    #[test]
    fn image_extension_routing_is_detected() {
        assert!(is_image_path(Path::new("/tmp/screenshot.webp")));
        assert!(!is_image_path(Path::new("/tmp/report.pdf")));
    }
}
