//! Orchestrator service bridge.
//!
//! Orchestrator topics run in the orchestrator service (`zdx orchestrator
//! serve`), which owns their workers and turns. This task subscribes to the
//! service as its renderer and turns its events into Telegram output:
//!
//! - A turn the service starts on its own (a `[worker update]`, several
//!   results batched) is rendered in the thread's route like any other turn:
//!   status line, cancel, reply. The service holds the thread until the reply
//!   is delivered (`Release`). Turns the bot submits itself are followed and
//!   rendered by the message handler instead.
//! - Workers are **silent**: a new worker gets no Telegram topic. `Created`,
//!   `Prompted`, and `Completed` refresh the owner's pinned card; `Completed`
//!   also posts a one-line notice in the owner's topic, linked to the worker.
//!
//! **Legacy mirror topics.** Workers created before silent workers may still
//! have a mirror topic (aliased to the worker and marked `worker_topic`). The
//! worker→topic map is rebuilt at startup from that persisted metadata, and
//! for those workers only the bridge keeps the old feed: orchestrator prompts
//! (`Prompted`), one debounced live status line per running turn (`Activity`,
//! carrying `⏹ Cancel worker` = `wk:c` and `💬 Open Thread`), and the final
//! result. Messages typed in such a topic are queued into the worker FIFO by
//! the message handler. No new mirror topics are ever opened.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use zdx_engine::core::events::AgentEvent;
use zdx_engine::core::thread_persistence;
use zdx_engine::core::workers::{CompletionEvent, WorkerActivity, WorkerEvent, WorkerStatus};
use zdx_engine::orchestration::client::ReplyStream;
use zdx_engine::orchestration::protocol::{Event, Reply, Request, Route};
use zdx_engine::orchestration::updates::truncate_chars;

use crate::agent::{STATUS_WAITING, tool_running_status};
use crate::bot::context::BotContext;
use crate::handlers::message::{
    append_thread_link, escape_html, mini_app_base_url, parse_topic_thread_id,
    refresh_thread_header, render_started_turn, resolve_effective_thread_id, thread_id_for_chat,
    worker_link,
};
use crate::telegram::markdown::{to_telegram_html, truncate_telegram_html};
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};

/// Longest final-text excerpt posted into a mirror topic message.
const MAX_MIRROR_TEXT_CHARS: usize = 3500;
/// Minimum spacing between edits of one live status message.
const LIVE_EDIT_INTERVAL: Duration = Duration::from_secs(3);
/// Wait before reconnecting to the orchestrator service.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

const CANCEL_CALLBACK: &str = "wk:c";

/// Mirror topic destination for one worker.
#[derive(Clone, Copy)]
struct MirrorTopic {
    chat: i64,
    topic: i64,
}

/// One running worker turn's live status message: a single line naming the
/// tool currently running (same shape as a normal turn's status), deleted
/// when the result posts.
struct LiveTurn {
    mirror: MirrorTopic,
    message_id: i64,
    status: String,
    dirty: bool,
    last_edit: Instant,
}

impl LiveTurn {
    /// Returns whether the visible status changed.
    fn apply(&mut self, activity: &WorkerActivity) -> bool {
        let next = match activity {
            WorkerActivity::ToolStarted { name, .. } => tool_running_status(name),
            WorkerActivity::ToolFinished { .. } => STATUS_WAITING.to_string(),
            WorkerActivity::ToolInput { .. } => return false,
        };
        if next == self.status {
            return false;
        }
        self.status = next;
        self.dirty = true;
        true
    }

    fn next_flush_at(&self) -> Option<Instant> {
        self.dirty.then(|| self.last_edit + LIVE_EDIT_INTERVAL)
    }
}

struct Bridge {
    context: Arc<BotContext>,
    mirrors: HashMap<String, MirrorTopic>,
    live: HashMap<String, LiveTurn>,
    /// Turns the service started on its own and this process renders, by
    /// turn id: where their events go.
    turns: HashMap<u64, mpsc::UnboundedSender<Event>>,
}

/// Spawns the process-lifetime bridge task. It reconnects whenever the
/// service restarts or is not running yet.
pub(crate) fn spawn_service_bridge(context: Arc<BotContext>) {
    tokio::spawn(async move {
        let mut bridge = Bridge {
            context,
            mirrors: HashMap::new(),
            live: HashMap::new(),
            turns: HashMap::new(),
        };
        bridge.recover_mirrors().await;
        let mut reported_down = false;
        loop {
            let subscribe = Request::Subscribe { renderer: true };
            match bridge.context.orchestrator().open(&subscribe).await {
                Ok(stream) => {
                    tracing::info!("Connected to the orchestrator service");
                    reported_down = false;
                    bridge.run(stream).await;
                    // Turns in flight cannot finish without the service.
                    bridge.turns.clear();
                    tracing::warn!("Lost the connection to the orchestrator service");
                }
                Err(err) if !reported_down => {
                    reported_down = true;
                    tracing::warn!(err = %format!("{err:#}"), "Orchestrator service unavailable; orchestrator topics will not answer until it runs");
                }
                Err(_) => {}
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    });
}

impl Bridge {
    /// Rebuilds the worker→mirror map from persisted mirror-topic metadata so
    /// feeds resume after a restart. (The service registers the links on its
    /// workers itself.)
    async fn recover_mirrors(&mut self) {
        let pairs = tokio::task::spawn_blocking(thread_persistence::list_worker_topics).await;
        let pairs = match pairs {
            Ok(Ok(pairs)) => pairs,
            Ok(Err(err)) => {
                tracing::warn!(err = %format!("{err:#}"), "Failed to scan persisted worker mirror topics");
                return;
            }
            Err(err) => {
                tracing::warn!(%err, "Mirror topic scan task failed");
                return;
            }
        };
        for (topic_thread_id, worker_thread_id) in pairs {
            let Some((chat, topic)) = parse_topic_thread_id(&topic_thread_id) else {
                continue;
            };
            self.mirrors
                .insert(worker_thread_id, MirrorTopic { chat, topic });
        }
        if !self.mirrors.is_empty() {
            tracing::info!(count = self.mirrors.len(), "Recovered worker mirror topics");
        }
    }

    async fn run(&mut self, mut stream: ReplyStream) {
        loop {
            let next_flush = self.next_flush_at();
            tokio::select! {
                reply = stream.next() => {
                    match reply {
                        Ok(Some(Reply::Event { event })) => self.handle(event).await,
                        Ok(Some(_)) => {}
                        Ok(None) => return,
                        Err(err) => {
                            tracing::warn!(err = %format!("{err:#}"), "Bad message from the orchestrator service");
                            return;
                        }
                    }
                }
                () = async {
                    match next_flush {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.flush_due().await;
                }
            }
        }
    }

    async fn handle(&mut self, event: Event) {
        self.update_steer_acks(&event).await;
        match event {
            Event::TurnStarted {
                thread_id,
                turn_id,
                route: Some(route),
                followed: false,
                ..
            } => {
                let (tx, rx) = mpsc::unbounded_channel();
                self.turns.insert(turn_id, tx);
                tokio::spawn(render_started_turn(
                    Arc::clone(&self.context),
                    thread_id,
                    turn_id,
                    route,
                    rx,
                ));
            }
            Event::Agent { turn_id, .. } => {
                if let Some(tx) = self.turns.get(&turn_id) {
                    let _ = tx.send(event);
                }
            }
            Event::TurnEnded { turn_id, .. } => {
                if let Some(tx) = self.turns.remove(&turn_id) {
                    let _ = tx.send(event);
                }
            }
            Event::Worker {
                owner_thread_id,
                route,
                event,
            } => {
                self.handle_worker(owner_thread_id.as_deref(), route, event)
                    .await;
            }
            Event::TurnStarted { .. } | Event::InputsDropped { .. } | Event::Settled { .. } => {}
        }
    }

    /// Keeps the acknowledgement of each message steered into a service turn
    /// current: added when the turn reads it, a new turn when it missed the
    /// turn, not added when a cancel gave it back.
    async fn update_steer_acks(&self, event: &Event) {
        let steer = self.context.steer();
        let updates: Vec<(crate::steer::Ack, &str)> = match event {
            Event::Agent {
                event: AgentEvent::UserMessageInjected { id, .. },
                ..
            } if !crate::steer::BotSteer::is_local(id) => steer
                .settle_service_injection(id)
                .map(|ack| (ack, crate::steer::ACK_ADDED))
                .into_iter()
                .collect(),
            Event::TurnStarted { inputs, .. } => inputs
                .iter()
                .filter_map(|input| {
                    steer.settle(&input.input_id.to_string(), crate::steer::ACK_NEW_TURN)
                })
                .map(|ack| (ack, crate::steer::ACK_NEW_TURN))
                .collect(),
            Event::InputsDropped { input_ids, .. } => input_ids
                .iter()
                .filter_map(|id| steer.settle(&id.to_string(), crate::steer::ACK_NOT_ADDED))
                .map(|ack| (ack, crate::steer::ACK_NOT_ADDED))
                .collect(),
            _ => Vec::new(),
        };
        for (ack, text) in updates {
            crate::handlers::message::edit_steer_ack(&self.context, ack, text).await;
        }
    }

    async fn handle_worker(
        &mut self,
        owner: Option<&str>,
        route: Option<Route>,
        event: WorkerEvent,
    ) {
        match event {
            WorkerEvent::Created { .. } | WorkerEvent::QueueChanged { .. } => {
                // Workers are silent: no Telegram topic is opened for them.
                // Only the owner's pinned card changes.
                self.refresh_owner_header(owner).await;
            }
            WorkerEvent::Prompted {
                worker_thread_id,
                prompt,
            } => {
                post_mirror_prompt(
                    &self.context,
                    self.mirrors.get(&worker_thread_id),
                    &worker_thread_id,
                    &prompt,
                )
                .await;
                self.refresh_owner_header(owner).await;
            }
            WorkerEvent::Activity {
                worker_thread_id,
                activity,
            } => {
                if self.handle_activity(&worker_thread_id, activity).await {
                    // First activity of a turn: the worker just went queued →
                    // running.
                    self.refresh_owner_header(owner).await;
                }
            }
            WorkerEvent::Completed(event) => {
                self.finish_live_turn(&event).await;
                post_mirror_update(
                    &self.context,
                    self.mirrors.get(&event.worker_thread_id),
                    &event,
                )
                .await;
                // Self-owned workers (re-attached from a mirror topic) have no
                // orchestrator to wake; the mirror post is the whole delivery.
                if event.owner_thread_id != event.worker_thread_id
                    && let Some(route) = route
                {
                    post_update_notice(&self.context, route, &event).await;
                }
                refresh_thread_header(&self.context, &event.owner_thread_id).await;
            }
        }
    }

    /// Refreshes the pinned card of the orchestrator that owns a worker (the
    /// card carries the live worker summary).
    async fn refresh_owner_header(&self, owner: Option<&str>) {
        if let Some(owner) = owner {
            refresh_thread_header(&self.context, owner).await;
        }
    }

    /// Feeds a legacy mirror topic's live status line. Returns whether this
    /// is the first activity seen for the worker's current turn.
    async fn handle_activity(&mut self, worker_thread_id: &str, activity: WorkerActivity) -> bool {
        if let Some(turn) = self.live.get_mut(worker_thread_id) {
            if turn.apply(&activity) && turn.last_edit.elapsed() >= LIVE_EDIT_INTERVAL {
                self.flush(worker_thread_id).await;
            }
            return false;
        }

        let Some(mirror) = self.mirrors.get(worker_thread_id).copied() else {
            return false;
        };
        let mut turn = LiveTurn {
            mirror,
            message_id: 0,
            status: STATUS_WAITING.to_string(),
            dirty: false,
            last_edit: Instant::now(),
        };
        turn.apply(&activity);
        turn.dirty = false;
        let keyboard = mirror_keyboard(&self.context, mirror.chat, worker_thread_id);
        let sent = self
            .context
            .client()
            .send_message_with_markup(
                mirror.chat,
                &turn.status,
                None,
                Some(mirror.topic),
                &keyboard,
            )
            .await;
        match sent {
            Ok(message) => {
                turn.message_id = message.id;
                self.live.insert(worker_thread_id.to_string(), turn);
            }
            Err(err) => {
                tracing::warn!(worker = %worker_thread_id, err = %format!("{err:#}"), "Failed to post live status message");
            }
        }
        true
    }

    fn next_flush_at(&self) -> Option<Instant> {
        self.live.values().filter_map(LiveTurn::next_flush_at).min()
    }

    async fn flush_due(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .live
            .iter()
            .filter(|(_, turn)| turn.next_flush_at().is_some_and(|at| at <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for worker_thread_id in due {
            self.flush(&worker_thread_id).await;
        }
    }

    async fn flush(&mut self, worker_thread_id: &str) {
        let Some(turn) = self.live.get_mut(worker_thread_id) else {
            return;
        };
        let keyboard = mirror_keyboard(&self.context, turn.mirror.chat, worker_thread_id);
        turn.dirty = false;
        turn.last_edit = Instant::now();
        if let Err(err) = self
            .context
            .client()
            .edit_message_text(
                turn.mirror.chat,
                turn.message_id,
                &turn.status,
                Some(&keyboard),
            )
            .await
            && !err.to_string().contains("message is not modified")
        {
            tracing::warn!(message_id = turn.message_id, err = %format!("{err:#}"), "Failed to edit live status message");
        }
    }

    /// Removes the live status message; the result posted right after it is
    /// the record of the turn, exactly like a normal turn's status.
    async fn finish_live_turn(&mut self, event: &CompletionEvent) {
        let Some(turn) = self.live.remove(&event.worker_thread_id) else {
            return;
        };
        if let Err(err) = self
            .context
            .client()
            .delete_message(turn.mirror.chat, turn.message_id)
            .await
        {
            tracing::warn!(message_id = turn.message_id, err = %format!("{err:#}"), "Failed to delete live status message");
        }
    }
}

/// Buttons under the live status message: cancel the worker from its
/// topic, and open the worker thread in the Mini App when configured.
fn mirror_keyboard(
    context: &BotContext,
    chat_id: i64,
    worker_thread_id: &str,
) -> InlineKeyboardMarkup {
    mirror_keyboard_for_url(
        mini_app_base_url(context, chat_id).as_deref(),
        worker_thread_id,
    )
}

fn mirror_keyboard_for_url(
    mini_app_url: Option<&str>,
    worker_thread_id: &str,
) -> InlineKeyboardMarkup {
    let mut row = vec![InlineKeyboardButton::callback(
        "⏹ Cancel worker",
        CANCEL_CALLBACK,
    )];
    if let Some(mini_app_url) = mini_app_url {
        row.push(InlineKeyboardButton::url(
            "💬 Open Thread",
            format!("{mini_app_url}?startapp={worker_thread_id}"),
        ));
    }
    InlineKeyboardMarkup {
        inline_keyboard: vec![row],
    }
}

/// Handles `wk:*` callbacks from mirror-topic keyboards. The worker is
/// resolved from the topic's persisted alias, never from callback data.
pub(crate) async fn handle_callback(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
    rest: &str,
) {
    let answer = |text: &'static str| async move {
        if let Err(err) = client.answer_callback_query(&callback.id, Some(text)).await {
            tracing::warn!(err = %format!("{err:#}"), "Failed to answer worker callback");
        }
    };
    if rest != "c" {
        answer("Unknown action").await;
        return;
    }
    let Some((chat_id, topic_id)) = callback
        .message
        .as_ref()
        .and_then(|message| Some((message.chat.id, message.effective_thread_id()?)))
    else {
        answer("Not inside a worker topic").await;
        return;
    };
    let topic_thread_id = thread_id_for_chat(chat_id, Some(topic_id));
    if !thread_persistence::read_thread_worker_topic(&topic_thread_id).unwrap_or(false) {
        answer("Not a worker topic").await;
        return;
    }
    let worker_thread_id = resolve_effective_thread_id(&topic_thread_id);
    let request = Request::CancelWorker {
        thread_id: worker_thread_id.clone(),
    };
    match context.orchestrator().call(&request).await {
        Ok(Reply::Worker { .. }) => {
            tracing::info!(worker = %worker_thread_id, chat_id, topic_id, "Worker cancelled from mirror topic");
            answer("Cancelling worker…").await;
        }
        _ => answer("Worker is idle; nothing to cancel").await,
    }
}

/// Posts an orchestrator-sent follow-up prompt into the worker's legacy mirror
/// topic (Markdown → Telegram HTML, entity-safe truncation). New workers have
/// no mirror, so this is a no-op for them.
async fn post_mirror_prompt(
    context: &Arc<BotContext>,
    mirror: Option<&MirrorTopic>,
    worker_thread_id: &str,
    prompt: &str,
) {
    let Some(mirror) = mirror else {
        return;
    };
    let body = truncate_telegram_html(&to_telegram_html(prompt), MAX_MIRROR_TEXT_CHARS);
    let text = format!("<b>📤 Prompt from the orchestrator</b>\n\n{body}");
    if let Err(err) = context
        .client()
        .send_message(mirror.chat, &text, None, Some(mirror.topic))
        .await
    {
        tracing::warn!(worker = %worker_thread_id, err = %format!("{err:#}"), "Failed to post mirror prompt");
    }
}

/// Posts one finished worker turn into its mirror topic, when known.
async fn post_mirror_update(
    context: &Arc<BotContext>,
    mirror: Option<&MirrorTopic>,
    event: &CompletionEvent,
) {
    let Some(mirror) = mirror else {
        return;
    };

    let text = match event.status {
        WorkerStatus::Completed => {
            let body = event.final_text.as_deref().unwrap_or_default();
            truncate_telegram_html(&to_telegram_html(body), MAX_MIRROR_TEXT_CHARS)
        }
        WorkerStatus::Cancelled => "🚫 Turn cancelled.".to_string(),
        _ => {
            let err_raw = event.error.as_deref().unwrap_or("unknown error");
            let summary = summarize_worker_error(err_raw);
            format!("❌ Turn {}: {summary}", event.status.as_str())
        }
    };
    // Same trailing Mini App link as a normal answer: results here are
    // bounded, and the full transcript is one tap away.
    let text = append_thread_link(
        mini_app_base_url(context, mirror.chat).as_deref(),
        &event.worker_thread_id,
        &text,
    );

    if let Err(err) = context
        .client()
        .send_message_without_preview(mirror.chat, &text, None, Some(mirror.topic))
        .await
    {
        tracing::warn!(worker = %event.worker_thread_id, err = %format!("{err:#}"), "Failed to post mirror update");
    }
}

/// Posts the one-line notice for a finished worker in its orchestrator's
/// topic. The review turn that follows is started by the service and
/// rendered from its events, so the user sees which worker woke it.
async fn post_update_notice(context: &BotContext, route: Route, event: &CompletionEvent) {
    let link = worker_link(
        event.mirror_url.clone(),
        mini_app_base_url(context, route.chat).as_deref(),
        &event.worker_thread_id,
    );
    let notice = worker_update_notice(event, link.as_deref());
    if let Err(err) = context
        .client()
        .send_message_without_preview(route.chat, &notice, None, route.topic)
        .await
    {
        tracing::warn!(worker = %event.worker_thread_id, err = %format!("{err:#}"), "Failed to post worker update notice");
    }
}

/// One-line notice posted in the orchestrator topic when a worker turn ends:
/// status glyph, worker title linked per [`worker_link`] when a link exists.
fn worker_update_notice(event: &CompletionEvent, link: Option<&str>) -> String {
    let title = thread_persistence::read_thread_title(&event.worker_thread_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| event.worker_thread_id.chars().take(8).collect());
    let title = escape_html(&title);
    let worker = match link {
        Some(url) => format!("<a href=\"{url}\">{title}</a>"),
        None => title,
    };
    let (glyph, verb) = match event.status {
        WorkerStatus::Completed => ("✅", "finished"),
        WorkerStatus::Cancelled => ("🚫", "was cancelled"),
        _ => ("❌", "failed"),
    };
    format!("{glyph} Worker 🛠 {worker} {verb} · reviewing…")
}

fn summarize_worker_error(error: &str) -> String {
    // Strip ANSI escape sequences.
    let stripped = strip_ansi_escapes(error);

    // If the error indicates a provider overload or rate limit on terminal failure,
    // summarize cleanly without misleading "retrying" claims.
    let lower = stripped.to_lowercase();
    if lower.contains("overloaded_error") || lower.contains(": overloaded") {
        return "provider overloaded".to_string();
    }
    if lower.contains("rate_limit") || lower.contains("rate limit") {
        return "provider rate limited".to_string();
    }

    // Otherwise, pick the most informative line (ignoring boilerplate log lines).
    for line in stripped.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(msg) = trimmed.strip_prefix("execute prompt: ") {
            return escape_html(&truncate_chars(msg, 200));
        }
        if let Some(msg) = trimmed.strip_prefix("provider: ") {
            return escape_html(&truncate_chars(msg, 200));
        }
        if let Some(msg) = trimmed.strip_prefix("Subagent failed: ") {
            // Avoid returning raw timestamped tracing lines.
            if !msg.contains("WARN") && !msg.contains("INFO") && !msg.contains("ERROR") {
                return escape_html(&truncate_chars(msg, 200));
            }
        }
    }

    format!(
        "<pre>{}</pre>",
        escape_html(&truncate_chars(&stripped, 500))
    )
}

fn strip_ansi_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(status: WorkerStatus) -> CompletionEvent {
        CompletionEvent {
            owner_thread_id: "owner".to_string(),
            worker_thread_id: "worker-1".to_string(),
            status,
            final_text: None,
            error: None,
            mirror_url: None,
            steers: Vec::new(),
        }
    }

    #[test]
    fn update_notice_links_the_worker_when_a_link_exists() {
        let linked = worker_update_notice(
            &completion(WorkerStatus::Completed),
            Some("https://t.me/zdx_bot/app?startapp=worker-1"),
        );
        assert_eq!(
            linked,
            "✅ Worker 🛠 <a href=\"https://t.me/zdx_bot/app?startapp=worker-1\">worker-1</a> finished · reviewing…"
        );
        // No thread on disk → short id; no link → plain text.
        let plain = worker_update_notice(&completion(WorkerStatus::Failed), None);
        assert_eq!(plain, "❌ Worker 🛠 worker-1 failed · reviewing…");
    }

    #[test]
    fn summarizes_worker_error_cleanly() {
        let overload_log = "Subagent failed: \u{1b}[2m2026-09-03T13:28:05.481367Z\u{1b}[0m \u{1b}[33m WARN\u{1b}[0m \u{1b}[2mzdx_engine::core::agent\u{1b}[0m:\u{1b}[0m Transient provider error, retrying attempt=2 max=3 delay_ms=4000 error=overloaded_error: Overloaded\nprovider: overloaded_error: Overloaded";
        assert_eq!(summarize_worker_error(overload_log), "provider overloaded");

        let simple_err = "execute prompt: something broke";
        assert_eq!(summarize_worker_error(simple_err), "something broke");
    }

    #[test]
    fn mirror_keyboard_has_cancel_and_optional_open_thread() {
        assert!(CANCEL_CALLBACK.len() <= 64);

        let with_app = mirror_keyboard_for_url(Some("https://t.me/zdx_bot/threads"), "w-1");
        let row = &with_app.inline_keyboard[0];
        assert_eq!(row.len(), 2);
        assert_eq!(row[0].callback_data.as_deref(), Some(CANCEL_CALLBACK));
        assert_eq!(
            row[1].url.as_deref(),
            Some("https://t.me/zdx_bot/threads?startapp=w-1")
        );

        let without_app = mirror_keyboard_for_url(None, "w-1");
        assert_eq!(without_app.inline_keyboard[0].len(), 1);
    }

    fn live_turn() -> LiveTurn {
        LiveTurn {
            mirror: MirrorTopic { chat: -1, topic: 1 },
            message_id: 1,
            status: STATUS_WAITING.to_string(),
            dirty: false,
            last_edit: Instant::now(),
        }
    }

    #[test]
    fn live_turn_tracks_only_the_current_tool() {
        let mut turn = live_turn();
        assert!(turn.apply(&WorkerActivity::ToolStarted {
            id: "a".to_string(),
            name: "bash".to_string(),
        }));
        assert_eq!(turn.status, "🔧 Running `bash`...");
        // Arguments are noise here; the thread has the detail.
        assert!(!turn.apply(&WorkerActivity::ToolInput {
            id: "a".to_string(),
            arg: "cat <file>".to_string(),
        }));
        assert!(turn.apply(&WorkerActivity::ToolFinished {
            id: "a".to_string(),
            ok: true,
        }));
        assert_eq!(turn.status, STATUS_WAITING);
        // Same visible status again is not a change.
        assert!(!turn.apply(&WorkerActivity::ToolFinished {
            id: "a".to_string(),
            ok: false,
        }));
    }

    #[test]
    fn flush_is_due_only_while_dirty() {
        let mut turn = live_turn();
        assert_eq!(turn.next_flush_at(), None);
        turn.apply(&WorkerActivity::ToolStarted {
            id: "a".to_string(),
            name: "glob".to_string(),
        });
        assert_eq!(
            turn.next_flush_at(),
            Some(turn.last_edit + LIVE_EDIT_INTERVAL)
        );
    }
}
