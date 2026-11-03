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
use zdx_engine::agent_activity;
use zdx_engine::orchestration::protocol::{Reply, Request};
use zdx_engine::service::{self, QueuedRestart, Service};

use crate::bot::context::BotContext;
use crate::bot::queue::ChatQueueMap;
use crate::handlers::message::{escape_html, parse_topic_thread_id};

/// How often the poller checks for a queued restart.
const POLL: Duration = Duration::from_secs(3);

pub(crate) enum RestartAttempt {
    /// Daemon (and orchestrator service, when installed) restarted (status
    /// text, HTML-escaped); the bot should exit.
    Restarting(String),
    /// Refused because this many agent runs are active.
    Blocked(usize),
    /// A restart failed (error text, HTML-escaped); the bot stays up.
    Failed(String),
}

/// Restarts the daemon and the orchestrator service when it is installed
/// (gated unless `force`), without exiting the bot.
pub(crate) async fn attempt_restart(force: bool) -> Result<RestartAttempt> {
    let mut services = vec![Service::Daemon];
    if service::state(Service::Orchestrator).installed {
        services.push(Service::Orchestrator);
    }
    restart_services(services, force).await
}

async fn restart_services(services: Vec<Service>, force: bool) -> Result<RestartAttempt> {
    let outcome = tokio::task::spawn_blocking(move || service::restart_many(&services, force))
        .await
        .context("join service restart task")?;
    Ok(match outcome {
        Ok(status) => RestartAttempt::Restarting(escape_html(&status.join(", "))),
        Err(err) => match err.downcast_ref::<service::RestartBlocked>() {
            Some(blocked) => RestartAttempt::Blocked(blocked.active_runs()),
            None => RestartAttempt::Failed(escape_html(&format!("{err:#}"))),
        },
    })
}

/// Checks for a queued restart every few seconds, acting only when nothing
/// could be lost: the orchestrator service is settled (or not running) and no
/// chat queue holds a message or a running turn.
pub(crate) fn spawn_poller(context: Arc<BotContext>, queues: ChatQueueMap) {
    tokio::spawn(async move {
        let mut poll = tokio::time::interval(POLL);
        loop {
            poll.tick().await;
            if orchestrator_settled(&context).await && queues.lock().await.is_empty() {
                tick(&context).await;
            }
        }
    });
}

/// Whether the orchestrator service has nothing in flight. A service that is
/// not running has nothing to lose.
async fn orchestrator_settled(context: &BotContext) -> bool {
    if !context.orchestrator().socket().exists() {
        return true;
    }
    match context
        .orchestrator()
        .call(&Request::Status { thread_id: None })
        .await
    {
        Ok(Reply::Status { report }) => report.settled,
        Ok(_) => false,
        Err(err) => {
            tracing::debug!(err = %format!("{err:#}"), "Orchestrator service status unavailable");
            !matches!(
                zdx_engine::pidfile::status(Service::Orchestrator.name()),
                zdx_engine::pidfile::ServiceStatus::Running { .. }
            )
        }
    }
}

enum Pending {
    None,
    Waiting,
    Ready(QueuedRestart),
}

/// Reads the queue and decides whether to act. Blocking: file and process checks.
fn check_pending() -> Pending {
    let request = match service::queued_restart() {
        Ok(Some(request)) => request,
        Ok(None) => return Pending::None,
        Err(err) => {
            tracing::warn!(err = %format!("{err:#}"), "Dropping unreadable queued restart");
            service::clear_queued_restart();
            return Pending::None;
        }
    };
    if request.is_fulfilled() {
        tracing::info!("Queued restart already fulfilled by a newer process; dropping it");
        service::clear_queued_restart();
        return Pending::None;
    }
    if agent_activity::list_active().is_empty() {
        Pending::Ready(request)
    } else {
        Pending::Waiting
    }
}

/// Performs a pending queued restart when no agent run is active.
///
/// Called only by the poller once the orchestrator service is settled and no
/// bot turn is queued or running; this adds the cross-process
/// `agent_activity` check.
async fn tick(context: &Arc<BotContext>) {
    if let Err(err) = try_tick(context).await {
        tracing::warn!(err = %format!("{err:#}"), "Queued restart check failed");
    }
}

async fn try_tick(context: &Arc<BotContext>) -> Result<()> {
    let Pending::Ready(request) = tokio::task::spawn_blocking(check_pending)
        .await
        .context("join queued restart check")?
    else {
        return Ok(());
    };
    let target = announce_target(
        request.thread_id.as_deref(),
        context.allowlist_chat_ids(),
        context.allowlist_user_ids(),
    );

    if request.includes(Service::Bot) && !zdx_engine::pidfile::is_supervised("bot") {
        service::clear_queued_restart();
        announce(
            context,
            target,
            "⚠️ Queued restart dropped: the bot has no active supervisor, so it cannot exit and come back.",
        )
        .await;
        return Ok(());
    }

    let services: Vec<Service> = [Service::Daemon, Service::Orchestrator]
        .into_iter()
        .filter(|svc| request.includes(*svc))
        // `all` includes the orchestrator; skip it where it is not installed.
        .filter(|svc| *svc != Service::Orchestrator || service::state(*svc).installed)
        .collect();
    let mut status = None;
    if !services.is_empty() {
        match restart_services(services, false).await? {
            RestartAttempt::Restarting(text) => status = Some(text),
            // A run started after the idle check; the gate caught it, so wait again.
            RestartAttempt::Blocked(_) => return Ok(()),
            RestartAttempt::Failed(error) => {
                service::clear_queued_restart();
                announce(
                    context,
                    target,
                    &format!(
                        "⚠️ Queued restart failed, so I left the bot running.\n<code>{error}</code>"
                    ),
                )
                .await;
                return Ok(());
            }
        }
    }

    service::clear_queued_restart();
    let prefix = status.map(|s| format!("🔄 {s}.\n")).unwrap_or_default();
    if request.includes(Service::Bot) {
        announce(
            context,
            target,
            &format!(
                "{prefix}👋 Queued restart: no agent run is active, exiting bot… launchd will restart it shortly."
            ),
        )
        .await;
        context.request_exit();
    } else {
        announce(
            context,
            target,
            &format!("{prefix}✅ Queued restart done: no agent run was active."),
        )
        .await;
    }
    Ok(())
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
