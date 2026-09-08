use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use zdx_engine::agent_activity;
use zdx_engine::config::ThinkingLevel;
use zdx_engine::core::{thread_persistence, worktree};
use zdx_engine::service::{self, Service};

use super::status::current_status_message;
use super::{ReplyContext, escape_html, post_thread_header, thread_id_for_chat};
use crate::agent;
use crate::bot::context::BotContext;
use crate::commands::{
    BotCommand, ModelSubcommand, RestartMode, parse_command, parse_restart_command,
};
use crate::telegram::markdown::{to_telegram_html, truncate_telegram_html};
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
        || handle_tldr_command(
            context,
            incoming,
            thread_id,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?
        || handle_threadid_command(
            context,
            incoming,
            thread_id,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?
        || handle_goal_clear_command(
            context,
            incoming,
            thread_id,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await?
        || handle_threads_command(
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
        .await?
        || crate::command_picker::handle_commands_command(
            context,
            incoming,
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
            thread_id,
        )
        .await?)
}

/// `/new` in General: create an empty forum topic marked for auto-titling on
/// the first real message. Errors are surfaced to the user, not propagated.
async fn create_empty_topic_from_new(
    context: &BotContext,
    chat_id: i64,
    reply_to_message_id: Option<i64>,
) -> Result<()> {
    let topic_name = format!("Chat {}", chrono::Utc::now().format("%Y-%m-%d %H:%M"));
    match context
        .client()
        .create_forum_topic(chat_id, &topic_name)
        .await
    {
        Ok(topic_id) => {
            let thread_id = thread_id_for_chat(chat_id, Some(topic_id));
            if let Err(err) = thread_persistence::Thread::with_id(thread_id.clone())
                .and_then(|mut thread| thread.set_pending_topic_title(true))
            {
                tracing::warn!(
                    chat_id,
                    topic_id,
                    thread_id = %thread_id,
                    %err,
                    "Created empty topic but failed to mark pending auto-title"
                );
            }
            if let Err(err) = post_thread_header(context, chat_id, topic_id, &thread_id).await {
                tracing::warn!(
                    chat_id,
                    topic_id,
                    thread_id = %thread_id,
                    %err,
                    "Created empty topic but failed to post thread header"
                );
            }
            tracing::info!(
                chat_id,
                topic_id,
                topic_name = %topic_name,
                "Created empty topic from /new in General"
            );
        }
        Err(err) => {
            tracing::error!(chat_id, %err, "Failed to create empty topic from /new in General");
            context
                .client()
                .send_message(
                    chat_id,
                    "⚠️ I couldn't create a new topic. Please try again.",
                    reply_to_message_id,
                    None,
                )
                .await?;
        }
    }
    Ok(())
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
    if !matches!(
        command,
        BotCommand::New
            | BotCommand::WorktreeCreate
            | BotCommand::Handoff
            | BotCommand::Btw
            | BotCommand::Commands
            | BotCommand::PromptBuilder
            | BotCommand::Launcher
    ) {
        return Ok(false);
    }

    let message = match command {
        BotCommand::New => {
            create_empty_topic_from_new(context, incoming.chat_id, reply_to_message_id).await?;
            return Ok(true);
        }
        BotCommand::Launcher => {
            if let Err(err) =
                super::launcher::post_launcher(context, incoming.chat_id, reply_to_message_id).await
            {
                tracing::error!(chat_id = incoming.chat_id, %err, "Failed to post launcher");
                context
                    .client()
                    .send_message(
                        incoming.chat_id,
                        "⚠️ I couldn't post the launcher. Please try again.",
                        reply_to_message_id,
                        None,
                    )
                    .await?;
            }
            return Ok(true);
        }
        BotCommand::WorktreeCreate => "/worktree must be used inside a topic, not General.",
        BotCommand::Handoff => "/handoff must be used inside a topic, not General.",
        BotCommand::Btw => "/btw must be used inside a topic, not General.",
        BotCommand::Commands => "/commands must be used inside a topic, not General.",
        BotCommand::PromptBuilder => "/prompt_builder must be used inside a topic, not General.",
        BotCommand::Restart => unreachable!("restart is handled by handle_restart_command"),
        BotCommand::Status => unreachable!("status is handled by handle_status_command"),
        BotCommand::WhereAmI => unreachable!("whereami is handled by handle_whereami_command"),
        BotCommand::Tldr => unreachable!("tldr is handled by handle_tldr_command"),
        BotCommand::ThreadId => unreachable!("threadid is handled by handle_threadid_command"),
        BotCommand::Threads => unreachable!("threads is handled by handle_threads_command"),
        BotCommand::Goal => unreachable!("goal is staged by handle_staging_flow"),
        BotCommand::GoalClear => unreachable!("goal_clear is handled by handle_goal_clear_command"),
    };
    context
        .client()
        .send_message(incoming.chat_id, message, reply_to_message_id, None)
        .await?;
    Ok(true)
}

/// How often a queued restart re-checks for active agent runs.
const QUEUED_RESTART_POLL: std::time::Duration = std::time::Duration::from_secs(3);

pub(super) async fn handle_restart_command(
    context: &Arc<BotContext>,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
) -> Result<bool> {
    if !incoming.images.is_empty() || !incoming.audios.is_empty() {
        return Ok(false);
    }
    let Some(restart) = incoming.text.as_deref().and_then(parse_restart_command) else {
        return Ok(false);
    };
    let reply = |text: String| async move {
        context
            .client()
            .send_message(
                incoming.chat_id,
                &text,
                reply_to_message_id,
                incoming.message_thread_id,
            )
            .await
    };

    if !zdx_engine::pidfile::is_supervised("bot") {
        reply(
            "⚠️ No active supervisor — refusing to exit. Enable supervision in `zdx monitor` (Ctrl+R on `bot`) first."
                .to_string(),
        )
        .await?;
        return Ok(true);
    }

    match restart.mode {
        RestartMode::Queued => {
            if !context.try_claim_queued_restart() {
                reply(
                    "⏳ A restart is already queued; it fires as soon as no agent run is active."
                        .to_string(),
                )
                .await?;
                return Ok(true);
            }
            let active = tokio::task::spawn_blocking(|| agent_activity::list_active().len())
                .await
                .unwrap_or_default();
            reply(format!(
                "⏳ Restart queued. I'll restart as soon as no agent run is active ({active} active now).\n\n<code>/restart f</code> restarts immediately instead."
            ))
            .await?;
            spawn_queued_restart(
                Arc::clone(context),
                incoming.chat_id,
                incoming.message_thread_id,
            );
            Ok(true)
        }
        RestartMode::Gated | RestartMode::Force => {
            let force = restart.mode == RestartMode::Force;
            match attempt_restart(force).await? {
                RestartAttempt::Restarting(status) => {
                    reply(format!(
                        "🔄 {status}.\n👋 Exiting bot… launchd will restart it shortly."
                    ))
                    .await?;
                    context.request_exit();
                }
                RestartAttempt::Blocked(active_runs) => {
                    let suffix = if active_runs == 1 { "" } else { "s" };
                    reply(format!(
                        "⚠️ Restart blocked: <b>{active_runs}</b> active agent run{suffix}.\n\n<code>/restart q</code> — restart automatically once they finish\n<code>/restart f</code> — interrupt them and restart now"
                    ))
                    .await?;
                }
                RestartAttempt::Failed(error) => {
                    reply(format!(
                        "⚠️ Daemon restart failed, so I left the bot running.\n<code>{error}</code>"
                    ))
                    .await?;
                }
            }
            Ok(true)
        }
    }
}

enum RestartAttempt {
    /// Daemon restarted (status text, HTML-escaped); the bot should exit.
    Restarting(String),
    /// Refused because this many agent runs are active.
    Blocked(usize),
    /// Daemon restart failed (error text, HTML-escaped); the bot stays up.
    Failed(String),
}

/// Restarts the daemon (gated unless `force`) without exiting the bot.
async fn attempt_restart(force: bool) -> Result<RestartAttempt> {
    let outcome = tokio::task::spawn_blocking(move || service::restart(Service::Daemon, force))
        .await
        .context("join daemon restart task")?;
    Ok(match outcome {
        Ok(status) => RestartAttempt::Restarting(escape_html(&status)),
        Err(err) => match err.downcast_ref::<service::RestartBlocked>() {
            Some(blocked) => RestartAttempt::Blocked(blocked.active_runs()),
            None => RestartAttempt::Failed(escape_html(&format!("{err:#}"))),
        },
    })
}

/// Waits until no agent run is active, then performs the gated restart. A run
/// starting between the check and the restart is caught by the gate itself,
/// which simply puts the wait back to sleep.
fn spawn_queued_restart(context: Arc<BotContext>, chat_id: i64, topic_id: Option<i64>) {
    tokio::spawn(async move {
        loop {
            let idle = tokio::task::spawn_blocking(|| agent_activity::list_active().is_empty())
                .await
                .unwrap_or(false);
            if idle {
                match attempt_restart(false).await {
                    Ok(RestartAttempt::Restarting(status)) => {
                        let text = format!(
                            "🔄 {status}.\n👋 Queued restart: no agent run is active, exiting bot… launchd will restart it shortly."
                        );
                        if let Err(err) = context
                            .client()
                            .send_message(chat_id, &text, None, topic_id)
                            .await
                        {
                            tracing::warn!(%err, "Failed to announce queued restart");
                        }
                        context.request_exit();
                        return;
                    }
                    Ok(RestartAttempt::Blocked(_)) => {}
                    Ok(RestartAttempt::Failed(error)) => {
                        let text = format!(
                            "⚠️ Queued restart failed, so I left the bot running.\n<code>{error}</code>"
                        );
                        if let Err(err) = context
                            .client()
                            .send_message(chat_id, &text, None, topic_id)
                            .await
                        {
                            tracing::warn!(%err, "Failed to report queued restart failure");
                        }
                        context.release_queued_restart();
                        return;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "Queued restart task failed");
                        context.release_queued_restart();
                        return;
                    }
                }
            }
            tokio::time::sleep(QUEUED_RESTART_POLL).await;
        }
    });
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
    let bot_config = context.config_for_chat(incoming.chat_id);

    // General context = forum chat but NOT inside a topic thread
    let is_general = incoming.is_forum && incoming.message_thread_id.is_none();

    match subcmd {
        ModelSubcommand::Show | ModelSubcommand::List => {
            let (current, has_override) = if is_general {
                (bot_config.model.clone(), false)
            } else {
                let (resolved, overridden) =
                    resolve_topic_model_config(bot_config.clone(), thread_id)?;
                (resolved.model, overridden)
            };

            let header = if has_override {
                format!(
                    "Current model: <code>{current}</code> (topic override)\nDefault: <code>{}</code>",
                    bot_config.model
                )
            } else {
                format!("Current model: <code>{current}</code>")
            };

            let keyboard = build_provider_keyboard(
                context,
                incoming.chat_id,
                if is_general {
                    ModelPickerScope::General
                } else {
                    ModelPickerScope::Topic
                },
            );
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
            // Compare the bare spec so `@fast` / `@<thinking>` modifiers are accepted.
            let base = zdx_engine::models::ModelSpec::parse(&model_id)
                .base
                .to_string();
            let msg = if !available.iter().any(|m| m == &base) {
                format!(
                    "Unknown model: <code>{model_id}</code>\n\nUse /model list to see available models."
                )
            } else if is_general {
                context.set_chat_model(incoming.chat_id, &model_id)?;
                format!("✅ Default model set to <code>{model_id}</code>.")
            } else {
                set_topic_model_override(bot_config.clone(), thread_id, &model_id)?
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

fn resolve_topic_model_config(
    mut config: zdx_engine::config::Config,
    thread_id: &str,
) -> Result<(zdx_engine::config::Config, bool)> {
    if thread_persistence::read_persistent_profile(thread_id)?.as_deref()
        == Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME)
    {
        zdx_engine::subagents::apply_orchestrator_override(&mut config);
    }
    let model = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking = thread_persistence::read_thread_thinking_override(thread_id)?;
    let overridden = model.is_some() || thinking.is_some();
    config.apply_thread_model_override(model.as_deref(), thinking);
    Ok((config, overridden))
}

fn set_topic_model_override(
    config: zdx_engine::config::Config,
    thread_id: &str,
    model_id: &str,
) -> Result<String> {
    let (mut resolved, _) = resolve_topic_model_config(config, thread_id)?;
    resolved.apply_model_spec(model_id);
    let mut thread = zdx_engine::core::thread_persistence::Thread::with_id(thread_id.to_string())
        .context("open thread")?;
    thread.set_model_override(Some(resolved.model.clone()))?;
    Ok(format!(
        "✅ Model set to <code>{}</code> for this topic.",
        resolved.model
    ))
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

    let message = current_status_message(context, incoming.chat_id, thread_id).await?;

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

async fn handle_threadid_command(
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
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::ThreadId)))
    {
        return Ok(false);
    }

    let message = format!(
        "<b>Thread ID</b> <i>(tap to copy)</i>\n<code>{}</code>",
        escape_html(thread_id)
    );
    context
        .client()
        .send_message(incoming.chat_id, &message, reply_to_message_id, topic_id)
        .await?;

    Ok(true)
}

/// `/goal_clear` — stops the active goal for this thread.
///
/// Dropping the goal also invalidates any verification already in flight, so a
/// verdict that arrives afterwards cannot start another continuation.
async fn handle_goal_clear_command(
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
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::GoalClear)))
    {
        return Ok(false);
    }

    let message = match crate::goal::clear_goal(context.goal_map(), thread_id) {
        Some(goal) => format!(
            "🎯 Goal cleared after {} continuation(s): {}",
            goal.continuations(),
            escape_html(goal.objective())
        ),
        None => "No goal is active in this topic.".to_string(),
    };
    context
        .client()
        .send_message(incoming.chat_id, &message, reply_to_message_id, topic_id)
        .await?;

    Ok(true)
}

async fn handle_threads_command(
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
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::Threads)))
    {
        return Ok(false);
    }

    let config = context.config_for_chat(incoming.chat_id);
    let Some(mini_app_url) = config
        .telegram
        .server
        .as_ref()
        .filter(|server| server.enabled)
        .and_then(|server| server.mini_app_url.as_deref())
    else {
        context
            .client()
            .send_message(
                incoming.chat_id,
                "The Threads Mini App is not configured.",
                reply_to_message_id,
                topic_id,
            )
            .await?;
        return Ok(true);
    };

    let web_app_url = format!("{mini_app_url}?startapp={thread_id}");

    let keyboard = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![crate::telegram::InlineKeyboardButton::url(
            "💬 Open Thread in Mini App",
            web_app_url,
        )]],
    };

    context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            "📑 <b>Thread Viewer</b>\nTap below to open the interactive transcript:",
            reply_to_message_id,
            topic_id,
            &keyboard,
        )
        .await?;

    Ok(true)
}

async fn handle_tldr_command(
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
        .is_some_and(|text| matches!(parse_command(text), Some(BotCommand::Tldr)))
    {
        return Ok(false);
    }

    let placeholder = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            "⏳ Generating TLDR…",
            reply_to_message_id,
            topic_id,
            &InlineKeyboardMarkup {
                inline_keyboard: vec![],
            },
        )
        .await?;

    let config = context.config_for_chat(incoming.chat_id);
    let root = crate::command_picker::command_root(context, incoming.chat_id, thread_id)?;
    let text = match zdx_engine::core::tldr_generation::generate_tldr(
        thread_id,
        &config.tldr_model,
        &root,
    )
    .await
    {
        Ok(recap) => {
            let html = to_telegram_html(&recap);
            format!("📝 <b>TLDR</b>\n{}", truncate_telegram_html(&html, 3900))
        }
        Err(err) => format!(
            "⚠️ TLDR failed:\n<pre>{}</pre>",
            escape_html(&format!("{err:#}"))
        ),
    };
    context
        .client()
        .edit_message_text(incoming.chat_id, placeholder.id, &text, None)
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

/// Target of a model-picker interaction. Encoded in callback data so a tapped
/// model applies to the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelPickerScope {
    /// `/model` in General — edits the default Telegram model.
    General,
    /// `/model` in a topic — sets the per-topic override.
    Topic,
    /// Launcher `🎛 Custom` — creates a new topic pre-set to the picked model.
    NewThread,
}

impl ModelPickerScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Topic => "topic",
            Self::NewThread => "newthread",
        }
    }

    pub(crate) fn from_data(s: &str) -> Option<Self> {
        match s {
            "general" => Some(Self::General),
            "topic" => Some(Self::Topic),
            "newthread" => Some(Self::NewThread),
            _ => None,
        }
    }
}

/// Build an inline keyboard showing provider names as buttons.
/// Callback data format: `model_provider:{provider}:{scope}`.
pub(crate) fn build_provider_keyboard(
    context: &BotContext,
    chat_id: i64,
    scope: ModelPickerScope,
) -> InlineKeyboardMarkup {
    let models = context.config_for_chat(chat_id).subagent_available_models();
    // The launcher's Custom flow returns to the launcher menu, so label its
    // exit "← Back"; the standalone `/model` flow keeps "✖ Cancel".
    let exit_label = if scope == ModelPickerScope::NewThread {
        "← Back"
    } else {
        "✖ Cancel"
    };
    let scope = scope.as_str();

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
                .map(|p| {
                    InlineKeyboardButton::callback(
                        zdx_engine::providers::provider_key_label(p),
                        format!("model_provider:{p}:{scope}"),
                    )
                })
                .collect()
        })
        .collect();

    rows.push(vec![InlineKeyboardButton::callback(
        exit_label,
        format!("model_cancel:{scope}"),
    )]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

pub(crate) fn models_for_provider(
    context: &BotContext,
    chat_id: i64,
    provider: &str,
) -> Vec<String> {
    context
        .config_for_chat(chat_id)
        .subagent_available_models()
        .into_iter()
        .filter(|m| m.starts_with(&format!("{provider}:")))
        .collect()
}

/// Build an inline keyboard showing model names for a specific provider.
/// Callback data format: `model_pick:{provider}:{index}:{scope}`.
pub(crate) fn build_models_keyboard(
    context: &BotContext,
    chat_id: i64,
    provider: &str,
    scope: ModelPickerScope,
) -> InlineKeyboardMarkup {
    let scope = scope.as_str();
    let filtered = models_for_provider(context, chat_id, provider);

    let indexed: Vec<(usize, &String)> = filtered.iter().enumerate().collect();

    let mut rows: Vec<Vec<InlineKeyboardButton>> = indexed
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(index, m)| {
                    // Display just the model part (after provider:)
                    let display = m.split(':').nth(1).unwrap_or(m);
                    InlineKeyboardButton::callback(
                        display,
                        format!("model_pick:{provider}:{index}:{scope}"),
                    )
                })
                .collect()
        })
        .collect();

    // Add a "← Back" button
    rows.push(vec![InlineKeyboardButton::callback(
        "← Back",
        format!("model_back:{scope}"),
    )]);

    rows.push(vec![InlineKeyboardButton::callback(
        "✖ Cancel",
        format!("model_cancel:{scope}"),
    )]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

/// Build the second step of the model picker.
/// Callback data format: `model_thinking:{provider}:{index}:{scope}:{level}`.
pub(crate) fn build_model_thinking_keyboard(
    provider: &str,
    index: usize,
    scope: ModelPickerScope,
    current: ThinkingLevel,
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = ThinkingLevel::all()
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|level| {
                    let prefix = if *level == current { "✅ " } else { "" };
                    InlineKeyboardButton::callback(
                        format!("{prefix}{}", level.display_name()),
                        format!(
                            "model_thinking:{provider}:{index}:{}:{}",
                            scope.as_str(),
                            level.display_name()
                        ),
                    )
                })
                .collect()
        })
        .collect();

    rows.push(vec![InlineKeyboardButton::callback(
        "← Back",
        format!("model_provider:{provider}:{}", scope.as_str()),
    )]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

#[allow(clippy::too_many_lines)]
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
        // Handoff/Btw/PromptBuilder/Goal run via the staging flow; Commands via
        // the picker handler; Tldr via handle_tldr_command; GoalClear via
        // handle_goal_clear_command.
        BotCommand::Restart
        | BotCommand::Status
        | BotCommand::WhereAmI
        | BotCommand::Handoff
        | BotCommand::Btw
        | BotCommand::Commands
        | BotCommand::Tldr
        | BotCommand::ThreadId
        | BotCommand::Threads
        | BotCommand::Goal
        | BotCommand::GoalClear
        | BotCommand::PromptBuilder => {
            return Ok(false);
        }
        BotCommand::Launcher => {
            context
                .client()
                .send_message(
                    incoming.chat_id,
                    "/launcher is only available in General.",
                    reply_to_message_id,
                    topic_id,
                )
                .await?;
            return Ok(true);
        }
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
