//! Orchestrator worker bridge.
//!
//! Consumes [`WorkerEvent`]s from the engine's `WorkerManager`:
//!
//! - `Created` opens a **mirror topic** in the owner's forum chat, aliased to
//!   the worker thread and marked `worker_topic`, so the user can follow the
//!   worker in Telegram. Messages typed there are queued into the worker FIFO
//!   by the message handler (never run as in-process turns).
//! - `Completed` posts the worker's final text into its mirror topic and wakes
//!   the owning orchestrator topic with a synthetic queued turn, reusing the
//!   same dispatch path as `/goal` continuations.
//!
//! Best-effort by design: routes and the worker→topic map are process-lifetime.
//! After a restart the mirror topics and transcripts survive, and a message in
//! a mirror topic re-attaches its worker.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;
use zdx_engine::core::thread_persistence::Thread;
use zdx_engine::core::workers::{CompletionEvent, WorkerEvent, WorkerStatus};

use crate::bot::context::BotContext;
use crate::bot::queue::ChatQueueMap;
use crate::bot::synthetic::dispatch_synthetic_prompt;
use crate::handlers::message::thread_id_for_chat;

/// Longest worker final-text excerpt embedded in the callback prompt; the
/// orchestrator is told to use `Read_Thread` for anything longer.
const MAX_CALLBACK_TEXT_CHARS: usize = 2000;
/// Longest final-text excerpt posted into a mirror topic message.
const MAX_MIRROR_TEXT_CHARS: usize = 3500;

/// Mirror topic destination for one worker (process-lifetime).
#[derive(Clone, Copy)]
struct MirrorTopic {
    chat: i64,
    topic: i64,
}

/// Spawns the process-lifetime bridge task.
pub(crate) fn spawn_completion_bridge(
    context: Arc<BotContext>,
    queues: ChatQueueMap,
    mut events_rx: UnboundedReceiver<WorkerEvent>,
) {
    tokio::spawn(async move {
        let mut mirrors: HashMap<String, MirrorTopic> = HashMap::new();
        while let Some(event) = events_rx.recv().await {
            match event {
                WorkerEvent::Created {
                    owner_thread_id,
                    worker_thread_id,
                    root,
                    title,
                    prompt,
                } => {
                    if let Some(mirror) = create_mirror_topic(
                        &context,
                        &owner_thread_id,
                        &worker_thread_id,
                        &root,
                        title.as_deref(),
                        &prompt,
                    )
                    .await
                    {
                        mirrors.insert(worker_thread_id, mirror);
                    }
                }
                WorkerEvent::Prompted {
                    worker_thread_id,
                    prompt,
                } => {
                    post_mirror_prompt(&context, mirrors.get(&worker_thread_id), &prompt).await;
                }
                WorkerEvent::Completed(event) => {
                    post_mirror_update(&context, mirrors.get(&event.worker_thread_id), &event)
                        .await;
                    dispatch_owner_callback(&context, &queues, &event).await;
                }
            }
        }
    });
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

    let header = format!(
        "🛠 Worker `{worker_thread_id}`\nProject: {}\n\nResults are posted here as the worker finishes each turn. Messages you send in this topic are queued straight to the worker.\n\n📤 First prompt:\n{}",
        root.display(),
        truncate_chars(prompt, MAX_MIRROR_TEXT_CHARS),
    );
    if let Err(err) = context
        .client()
        .send_message(chat, &header, None, Some(topic_id))
        .await
    {
        tracing::warn!(worker = %worker_thread_id, %err, "Failed to post mirror topic header");
    }

    Some(MirrorTopic {
        chat,
        topic: topic_id,
    })
}

/// Posts an orchestrator-sent follow-up prompt into the worker's mirror topic.
async fn post_mirror_prompt(context: &Arc<BotContext>, mirror: Option<&MirrorTopic>, prompt: &str) {
    let Some(mirror) = mirror else {
        return;
    };
    let text = format!(
        "📤 Prompt from the orchestrator:\n{}",
        truncate_chars(prompt, MAX_MIRROR_TEXT_CHARS)
    );
    if let Err(err) = context
        .client()
        .send_message(mirror.chat, &text, None, Some(mirror.topic))
        .await
    {
        tracing::warn!(%err, "Failed to post mirror prompt");
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
            truncate_chars(body, MAX_MIRROR_TEXT_CHARS)
        }
        WorkerStatus::Cancelled => "🚫 Turn cancelled.".to_string(),
        _ => format!(
            "❌ Turn {}:\n{}",
            event.status.as_str(),
            truncate_chars(event.error.as_deref().unwrap_or("unknown error"), 1000)
        ),
    };

    if let Err(err) = context
        .client()
        .send_message(mirror.chat, &text, None, Some(mirror.topic))
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

    #[test]
    fn completed_prompt_carries_final_text() {
        let prompt = build_worker_update_prompt(&CompletionEvent {
            owner_thread_id: "owner".to_string(),
            worker_thread_id: "worker-1".to_string(),
            status: WorkerStatus::Completed,
            final_text: Some("all done".to_string()),
            error: None,
        });
        assert!(prompt.starts_with("[worker update]"));
        assert!(prompt.contains("`worker-1`"));
        assert!(prompt.contains("status: completed"));
        assert!(prompt.contains("all done"));
    }

    #[test]
    fn failed_prompt_carries_error_and_bounds_length() {
        let prompt = build_worker_update_prompt(&CompletionEvent {
            owner_thread_id: "owner".to_string(),
            worker_thread_id: "worker-1".to_string(),
            status: WorkerStatus::Failed,
            final_text: None,
            error: Some("x".repeat(10_000)),
        });
        assert!(prompt.contains("status: failed"));
        assert!(prompt.contains("[truncated"));
        assert!(prompt.chars().count() < 3000);
    }
}
