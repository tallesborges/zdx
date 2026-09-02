//! Orchestrator worker bridge.
//!
//! Consumes [`WorkerEvent`]s from the engine's `WorkerManager`:
//!
//! - `Created` opens a **mirror topic** in the owner's forum chat, aliased to
//!   the worker thread and marked `worker_topic`, so the user can follow the
//!   worker in Telegram. Messages typed there are queued into the worker FIFO
//!   by the message handler (never run as in-process turns). The topic link
//!   is registered on the manager so `create_thread` can hand it back.
//! - `Activity` keeps one live status line per running turn (`🔧 Running
//!   `bash`...`, same shape as a normal turn's status) up to date with
//!   debounced edits; it is deleted when the result posts.
//! - `Completed` posts the worker's final text into its mirror topic and
//!   wakes the owning orchestrator topic with a synthetic queued turn,
//!   reusing the same dispatch path as `/goal` continuations.
//!
//! Mirror header and result messages carry `⏹ Cancel worker` (`wk:c`) and
//! `💬 Open Thread` buttons; the cancel callback resolves the worker from the
//! topic's persisted alias, so it works after a restart too.
//!
//! Routes are process-lifetime; the worker→topic map is rebuilt at startup
//! from the persisted `worker_topic` + `alias_to` metadata, so results keep
//! landing in their mirrors across restarts without anyone poking the topic.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;
use zdx_engine::core::thread_persistence::{self, Thread};
use zdx_engine::core::workers::{CompletionEvent, WorkerActivity, WorkerEvent, WorkerStatus};

use crate::agent::{STATUS_WAITING, tool_running_status};
use crate::bot::context::BotContext;
use crate::bot::queue::ChatQueueMap;
use crate::bot::synthetic::dispatch_synthetic_prompt;
use crate::handlers::message::{
    escape_html, mini_app_base_url, parse_topic_thread_id, post_thread_header,
    resolve_effective_thread_id, thread_id_for_chat,
};
use crate::telegram::markdown::{to_telegram_html, truncate_telegram_html};
use crate::telegram::{
    CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient, topic_link,
};

/// Longest worker final-text excerpt embedded in the callback prompt; the
/// orchestrator is told to use `Read_Thread` for anything longer.
const MAX_CALLBACK_TEXT_CHARS: usize = 2000;
/// Longest final-text excerpt posted into a mirror topic message.
const MAX_MIRROR_TEXT_CHARS: usize = 3500;
/// Minimum spacing between edits of one live status message.
const LIVE_EDIT_INTERVAL: Duration = Duration::from_secs(3);

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
    queues: ChatQueueMap,
    mirrors: HashMap<String, MirrorTopic>,
    live: HashMap<String, LiveTurn>,
}

/// Spawns the process-lifetime bridge task.
pub(crate) fn spawn_completion_bridge(
    context: Arc<BotContext>,
    queues: ChatQueueMap,
    mut events_rx: UnboundedReceiver<WorkerEvent>,
) {
    tokio::spawn(async move {
        let mut bridge = Bridge {
            context,
            queues,
            mirrors: HashMap::new(),
            live: HashMap::new(),
        };
        bridge.recover_mirrors().await;

        loop {
            let next_flush = bridge.next_flush_at();
            tokio::select! {
                event = events_rx.recv() => {
                    let Some(event) = event else { break };
                    bridge.handle(event).await;
                }
                () = async {
                    match next_flush {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    bridge.flush_due().await;
                }
            }
        }
    });
}

impl Bridge {
    /// Rebuilds the worker→mirror map from persisted mirror-topic metadata so
    /// feeds resume after a restart, and registers each link on the manager.
    async fn recover_mirrors(&mut self) {
        let pairs = tokio::task::spawn_blocking(thread_persistence::list_worker_topics).await;
        let pairs = match pairs {
            Ok(Ok(pairs)) => pairs,
            Ok(Err(err)) => {
                tracing::warn!(%err, "Failed to scan persisted worker mirror topics");
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
            let mirror = MirrorTopic { chat, topic };
            self.context
                .worker_manager()
                .set_mirror_url(&worker_thread_id, topic_link(chat, topic));
            self.mirrors.insert(worker_thread_id, mirror);
        }
        if !self.mirrors.is_empty() {
            tracing::info!(count = self.mirrors.len(), "Recovered worker mirror topics");
        }
    }

    async fn handle(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Created {
                owner_thread_id,
                worker_thread_id,
                root,
                title,
                prompt,
            } => {
                let mirror = create_mirror_topic(
                    &self.context,
                    &owner_thread_id,
                    &worker_thread_id,
                    &root,
                    title.as_deref(),
                    &prompt,
                )
                .await;
                // Always resolve the link, even to `None`, so a `create_thread`
                // waiting on it returns immediately instead of timing out.
                self.context.worker_manager().set_mirror_url(
                    &worker_thread_id,
                    mirror.and_then(|mirror| topic_link(mirror.chat, mirror.topic)),
                );
                if let Some(mirror) = mirror {
                    self.mirrors.insert(worker_thread_id, mirror);
                }
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
            }
            WorkerEvent::Activity {
                worker_thread_id,
                activity,
            } => {
                self.handle_activity(&worker_thread_id, activity).await;
            }
            WorkerEvent::Completed(event) => {
                self.finish_live_turn(&event).await;
                post_mirror_update(
                    &self.context,
                    self.mirrors.get(&event.worker_thread_id),
                    &event,
                )
                .await;
                dispatch_owner_callback(&self.context, &self.queues, &event).await;
            }
        }
    }

    async fn handle_activity(&mut self, worker_thread_id: &str, activity: WorkerActivity) {
        if let Some(turn) = self.live.get_mut(worker_thread_id) {
            if turn.apply(&activity) && turn.last_edit.elapsed() >= LIVE_EDIT_INTERVAL {
                self.flush(worker_thread_id).await;
            }
            return;
        }

        let Some(mirror) = self.mirrors.get(worker_thread_id).copied() else {
            return;
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
                tracing::warn!(worker = %worker_thread_id, %err, "Failed to post live status message");
            }
        }
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
            tracing::warn!(message_id = turn.message_id, %err, "Failed to edit live status message");
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
            tracing::warn!(message_id = turn.message_id, %err, "Failed to delete live status message");
        }
    }
}

/// Buttons under mirror header/result messages: cancel the worker from its
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
            tracing::warn!(%err, "Failed to answer worker callback");
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
    match context.worker_manager().cancel(&worker_thread_id) {
        Ok(_) => {
            tracing::info!(worker = %worker_thread_id, chat_id, topic_id, "Worker cancelled from mirror topic");
            answer("Cancelling worker…").await;
        }
        Err(_) => answer("Worker is idle; nothing to cancel").await,
    }
}

/// Opens the worker's mirror topic: preferred host is the group whose profile
/// `cwd` contains the worker's project root (so project workers surface in
/// their project's group, even when orchestrated from a DM home), falling back
/// to the orchestrator's own group. Returns `None` (with a log) when no group
/// can host it or every attempt fails — the worker still runs, just without a
/// Telegram window.
async fn create_mirror_topic(
    context: &Arc<BotContext>,
    owner_thread_id: &str,
    worker_thread_id: &str,
    root: &Path,
    title: Option<&str>,
    prompt: &str,
) -> Option<MirrorTopic> {
    let project_chat = context.mirror_chat_for_root(root);
    // Bots may create topics in Threaded Mode private chats too, so a DM home
    // is a valid fallback host for workers whose root has no group profile.
    let owner_chat = context
        .orchestrator_route(owner_thread_id)
        .map(|route| route.chat);
    let mut candidates: Vec<i64> = Vec::new();
    candidates.extend(project_chat);
    if let Some(chat) = owner_chat
        && !candidates.contains(&chat)
    {
        candidates.push(chat);
    }
    if candidates.is_empty() {
        tracing::info!(worker = %worker_thread_id, "No group can host a mirror topic; skipping");
        return None;
    }

    let short_id: String = worker_thread_id.chars().take(8).collect();
    let name = match title {
        Some(title) => format!("🛠 {title}"),
        None => format!("🛠 Worker {short_id}"),
    };

    let mut created: Option<(i64, i64)> = None;
    for chat in candidates {
        match context.client().create_forum_topic(chat, &name).await {
            Ok(topic_id) => {
                created = Some((chat, topic_id));
                break;
            }
            Err(err) => {
                tracing::warn!(worker = %worker_thread_id, chat, %err, "Failed to create mirror topic");
            }
        }
    }
    let (chat, topic_id) = created?;

    let topic_thread_id = thread_id_for_chat(chat, Some(topic_id));
    let marked = Thread::with_id(topic_thread_id.clone()).and_then(|mut thread| {
        thread.set_worker_topic();
        thread.set_alias(Some(worker_thread_id.to_string()))
    });
    if let Err(err) = marked {
        tracing::error!(
            thread_id = %topic_thread_id,
            %err,
            "Failed to bind mirror topic to worker; leaving topic unbound"
        );
        return None;
    }

    // Same pinned status card as every other topic (model, thinking, root,
    // usage, Open Thread + Refresh), computed for the worker thread itself.
    if let Err(err) = post_thread_header(context, chat, topic_id, worker_thread_id).await {
        tracing::warn!(worker = %worker_thread_id, %err, "Failed to post mirror topic header");
    }
    let mirror = MirrorTopic {
        chat,
        topic: topic_id,
    };
    post_prompt_message(
        context,
        &mirror,
        worker_thread_id,
        "📤 First prompt",
        prompt,
    )
    .await;
    Some(mirror)
}

/// Posts an orchestrator-sent follow-up prompt into the worker's mirror topic.
async fn post_mirror_prompt(
    context: &Arc<BotContext>,
    mirror: Option<&MirrorTopic>,
    worker_thread_id: &str,
    prompt: &str,
) {
    let Some(mirror) = mirror else {
        return;
    };
    post_prompt_message(
        context,
        mirror,
        worker_thread_id,
        "📤 Prompt from the orchestrator",
        prompt,
    )
    .await;
}

/// Renders a prompt (Markdown → Telegram HTML, entity-safe truncation) under
/// `label`, with the worker action buttons.
async fn post_prompt_message(
    context: &Arc<BotContext>,
    mirror: &MirrorTopic,
    worker_thread_id: &str,
    label: &str,
    prompt: &str,
) {
    let body = truncate_telegram_html(&to_telegram_html(prompt), MAX_MIRROR_TEXT_CHARS);
    let text = format!("<b>{label}</b>\n\n{body}");
    let keyboard = mirror_keyboard(context, mirror.chat, worker_thread_id);
    if let Err(err) = context
        .client()
        .send_message_with_markup(mirror.chat, &text, None, Some(mirror.topic), &keyboard)
        .await
    {
        tracing::warn!(worker = %worker_thread_id, %err, "Failed to post mirror prompt");
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
        _ => format!(
            "❌ Turn {}:\n<pre>{}</pre>",
            event.status.as_str(),
            escape_html(&truncate_chars(
                event.error.as_deref().unwrap_or("unknown error"),
                1000
            ))
        ),
    };

    let keyboard = mirror_keyboard(context, mirror.chat, &event.worker_thread_id);
    if let Err(err) = context
        .client()
        .send_message_with_markup(mirror.chat, &text, None, Some(mirror.topic), &keyboard)
        .await
    {
        tracing::warn!(worker = %event.worker_thread_id, %err, "Failed to post mirror update");
    }
}

/// Wakes the owning orchestrator topic with a synthetic `[worker update]` turn.
async fn dispatch_owner_callback(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    event: &CompletionEvent,
) {
    // Self-owned workers (re-attached from a mirror topic) have no
    // orchestrator to wake; the mirror post is the whole delivery.
    if event.owner_thread_id == event.worker_thread_id {
        return;
    }
    let Some(route) = context.orchestrator_route(&event.owner_thread_id) else {
        tracing::warn!(
            worker = %event.worker_thread_id,
            owner = %event.owner_thread_id,
            "No orchestrator route for worker completion; dropping callback"
        );
        return;
    };

    let prompt = build_worker_update_prompt(event);
    let dispatched =
        dispatch_synthetic_prompt(context, queues, route.chat, route.topic, route.user, prompt)
            .await;
    if dispatched {
        tracing::info!(
            worker = %event.worker_thread_id,
            status = event.status.as_str(),
            "Dispatched worker completion callback"
        );
    } else {
        tracing::warn!(
            worker = %event.worker_thread_id,
            "Failed to dispatch worker completion callback"
        );
    }
}

/// Compact synthetic prompt describing one finished worker turn.
fn build_worker_update_prompt(event: &CompletionEvent) -> String {
    let worker = &event.worker_thread_id;
    let status = event.status.as_str();

    let mut prompt = format!(
        "[worker update] Worker thread `{worker}` finished a turn with status: {status}.\n"
    );
    if let Some(url) = event.mirror_url.as_deref() {
        let _ = writeln!(prompt, "Mirror topic: {url}");
    }
    match event.status {
        WorkerStatus::Completed => {
            let text = event.final_text.as_deref().unwrap_or_default();
            prompt.push_str("\nFinal message:\n");
            prompt.push_str(&truncate_chars(text, MAX_CALLBACK_TEXT_CHARS));
            prompt.push('\n');
        }
        _ => {
            if let Some(error) = event.error.as_deref() {
                prompt.push_str("\nError:\n");
                prompt.push_str(&truncate_chars(error, MAX_CALLBACK_TEXT_CHARS));
                prompt.push('\n');
            }
        }
    }
    prompt.push_str(
        "\nHandle this update: reconcile your todos, follow up on the worker (Send_Thread_Message) or start dependent work if needed, and report the outcome to the user. Use Read_Thread on the worker for full context.",
    );
    prompt
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}… [truncated — use Read_Thread for the rest]")
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
        }
    }

    #[test]
    fn completed_prompt_carries_final_text_and_link() {
        let prompt = build_worker_update_prompt(&CompletionEvent {
            final_text: Some("all done".to_string()),
            mirror_url: Some("https://t.me/c/1/2".to_string()),
            ..completion(WorkerStatus::Completed)
        });
        assert!(prompt.starts_with("[worker update]"));
        assert!(prompt.contains("`worker-1`"));
        assert!(prompt.contains("status: completed"));
        assert!(prompt.contains("Mirror topic: https://t.me/c/1/2"));
        assert!(prompt.contains("all done"));
    }

    #[test]
    fn failed_prompt_carries_error_and_bounds_length() {
        let prompt = build_worker_update_prompt(&CompletionEvent {
            error: Some("x".repeat(10_000)),
            ..completion(WorkerStatus::Failed)
        });
        assert!(prompt.contains("status: failed"));
        assert!(!prompt.contains("Mirror topic:"));
        assert!(prompt.contains("[truncated"));
        assert!(prompt.chars().count() < 3000);
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
