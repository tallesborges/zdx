//! Performs the restart queued in `zdx_engine::service` (by Telegram
//! `/restart q` or `zdx service restart --queue`) once the machine is idle.
//!
//! The bot owns this because a queued restart must outlive the agent run that
//! asked for it, and the bot is the process that restarts by exiting. It acts
//! only once the orchestrator service reports itself settled (no worker
//! running or queued, no worker result waiting, no orchestrator turn queued,
//! running, or being delivered) and the bot's own chat queues are empty.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use zdx_engine::service;

use crate::bot::context::BotContext;
use crate::bot::queue::ChatQueueMap;
use crate::handlers::message::{escape_html, parse_topic_thread_id};

/// How often the poller checks for a queued restart.
const POLL: Duration = Duration::from_secs(3);

pub(crate) enum RestartAttempt {
    /// Daemon (and orchestrator service, when installed) restarted (status
    /// text, HTML-escaped); the bot should exit.
    Restarting(String),
    /// Refused because work would be lost (the guard's explanation).
    Blocked(String),
    /// A restart failed (error text, HTML-escaped); the bot stays up.
    Failed(String),
}

/// Restarts the daemon and the orchestrator service when it is installed
/// (gated unless `force`), without exiting the bot.
pub(crate) async fn attempt_restart(force: bool) -> Result<RestartAttempt> {
    let outcome = tokio::task::spawn_blocking(move || service::restart_bot_peers(force))
        .await
        .context("join service restart task")?;
    Ok(match outcome {
        Ok(status) => RestartAttempt::Restarting(escape_html(&status.join(", "))),
        Err(err) => match err.downcast_ref::<service::RestartBlocked>() {
            Some(blocked) => RestartAttempt::Blocked(escape_html(&blocked.to_string())),
            None => RestartAttempt::Failed(escape_html(&format!("{err:#}"))),
        },
    })
}

/// Checks for a queued restart every few seconds, acting only when nothing
/// could be lost: no chat queue holds a message or a running turn here, and
/// `service::run_queued_restart` checks agent runs and the orchestrator
/// service.
pub(crate) fn spawn_poller(context: Arc<BotContext>, queues: ChatQueueMap) {
    tokio::spawn(async move {
        let mut poll = tokio::time::interval(POLL);
        loop {
            poll.tick().await;
            if queues.lock().await.is_empty() {
                tick(&context).await;
            }
        }
    });
}

async fn tick(context: &Arc<BotContext>) {
    let run = match tokio::task::spawn_blocking(service::run_queued_restart).await {
        Ok(run) => run,
        Err(err) => {
            tracing::warn!(err = %err, "Queued restart check failed");
            return;
        }
    };
    if let Some(message) = &run.message {
        let target = announce_target(
            run.thread_id.as_deref(),
            context.allowlist_chat_ids(),
            context.allowlist_user_ids(),
        );
        announce(context, target, &escape_html(message)).await;
    }
    if run.restart_bot {
        context.request_exit();
    }
}

/// Chat and topic to announce in, from the requesting thread id. Only
/// Telegram threads of allowlisted chats qualify; anything else stays silent.
fn announce_target(
    thread_id: Option<&str>,
    allowed_chats: &HashSet<i64>,
    allowed_users: &HashSet<i64>,
) -> Option<(i64, Option<i64>)> {
    let thread_id = thread_id?;
    let (chat_id, topic_id) = match parse_topic_thread_id(thread_id) {
        Some((chat_id, topic_id)) => (chat_id, Some(topic_id)),
        None => (thread_id.strip_prefix("telegram-")?.parse().ok()?, None),
    };
    let allowed = allowed_chats.contains(&chat_id) || allowed_users.contains(&chat_id);
    allowed.then_some((chat_id, topic_id))
}

async fn announce(context: &BotContext, target: Option<(i64, Option<i64>)>, text: &str) {
    let Some((chat_id, topic_id)) = target else {
        tracing::info!(message = text, "Queued restart (no thread to announce in)");
        return;
    };
    if let Err(err) = context
        .client()
        .send_message(chat_id, text, None, topic_id)
        .await
    {
        tracing::warn!(err = %format!("{err:#}"), "Failed to announce queued restart");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announces_only_in_allowlisted_telegram_threads() {
        let chats = HashSet::from([-100]);
        let users = HashSet::from([42]);
        let target = |id| announce_target(Some(id), &chats, &users);

        assert_eq!(target("telegram--100-topic-7"), Some((-100, Some(7))));
        assert_eq!(target("telegram-42"), Some((42, None)));
        assert_eq!(target("telegram--200-topic-7"), None);
        assert_eq!(target("a1b2c3-worker"), None);
        assert_eq!(announce_target(None, &chats, &users), None);
    }
}
