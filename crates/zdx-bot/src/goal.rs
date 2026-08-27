//! Telegram-side goal state.
//!
//! Goals live in memory only, keyed by thread id. Process lifetime is the
//! goal's lifetime: a bot restart ends every run, and nothing resumes
//! autonomous work on its own. That is the safety property — it replaces the
//! explicit re-arm step a durable "active" flag would have needed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use uuid::Uuid;
use zdx_engine::core::events::NoticeKind;
use zdx_engine::core::goal::{Goal, GoalOutcome, verify_goal};
use zdx_engine::core::thread_persistence::{Thread, ThreadEvent};

use crate::bot::context::BotContext;
use crate::bot::queue::{ChatQueueMap, dispatch_message};
use crate::handlers::message::TurnOutcome;

/// Message ids for synthetic continuations. Counted down from `i64::MAX` so
/// they cannot collide with real Telegram ids, which the status and cancel maps
/// are keyed by.
static SYNTHETIC_MESSAGE_ID: AtomicI64 = AtomicI64::new(i64::MAX);

/// Where a finished turn happened, so the loop can answer in the same place.
pub(crate) struct TurnSite {
    pub chat: i64,
    pub topic: Option<i64>,
    pub user: i64,
    pub thread: String,
}

/// Active goals keyed by `thread_id`.
pub(crate) type GoalMap = Arc<Mutex<HashMap<String, Goal>>>;

pub(crate) fn new_goal_map() -> GoalMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Stores a new goal for `thread_id`, replacing any existing one.
pub(crate) fn set_goal(map: &GoalMap, thread_id: &str, goal: Goal) {
    map.lock()
        .expect("goal lock poisoned")
        .insert(thread_id.to_string(), goal);
}

/// Removes the goal for `thread_id`, returning it when one was active.
pub(crate) fn clear_goal(map: &GoalMap, thread_id: &str) -> Option<Goal> {
    map.lock().expect("goal lock poisoned").remove(thread_id)
}

/// Snapshot of the active goal, if any.
fn active_goal(map: &GoalMap, thread_id: &str) -> Option<Goal> {
    map.lock()
        .expect("goal lock poisoned")
        .get(thread_id)
        .cloned()
}

/// Claims the single verification slot, returning the `run_id` to fence the
/// result with. `None` when no goal is active or one is already in flight.
fn begin_verification(map: &GoalMap, thread_id: &str) -> Option<Uuid> {
    map.lock()
        .expect("goal lock poisoned")
        .get_mut(thread_id)?
        .begin_verification()
}

/// Releases the verification slot. `true` when `run_id` still refers to the
/// live goal, meaning the verdict may be acted on.
fn finish_verification(map: &GoalMap, thread_id: &str, run_id: Uuid) -> bool {
    map.lock()
        .expect("goal lock poisoned")
        .get_mut(thread_id)
        .is_some_and(|goal| goal.finish_verification(run_id))
}

/// Invalidates in-flight verification without ending the goal.
pub(crate) fn invalidate(map: &GoalMap, thread_id: &str) {
    if let Some(goal) = map.lock().expect("goal lock poisoned").get_mut(thread_id) {
        goal.invalidate();
    }
}

/// Records an accepted incomplete verdict, returning the continuation prompt.
/// `None` when the goal is gone or out of capacity.
fn record_continuation(
    map: &GoalMap,
    thread_id: &str,
    reason: String,
    next_action: &str,
) -> Option<String> {
    let mut guard = map.lock().expect("goal lock poisoned");
    let goal = guard.get_mut(thread_id)?;
    if !goal.has_capacity() {
        return None;
    }
    goal.record_continuation(reason);
    Some(goal.continuation_prompt(next_action))
}

/// Hook run after every agent turn in a chat.
///
/// Verification is deliberately spawned rather than awaited: it is another full
/// agent run, and holding the topic's queue slot for it would block every
/// inbound message until the verifier finished.
pub(crate) fn after_turn(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    site: TurnSite,
    outcome: &TurnOutcome,
) {
    let Some(goal) = active_goal(context.goal_map(), &site.thread) else {
        return;
    };

    // A cancelled or failed turn ends the run. Feeding an error back into
    // another autonomous round just burns the cap.
    let stop = match outcome {
        TurnOutcome::Completed => None,
        TurnOutcome::Cancelled => Some(GoalOutcome::Cancelled),
        TurnOutcome::Failed(error) => Some(GoalOutcome::TurnFailed {
            error: error.clone(),
        }),
    };
    if let Some(stop) = stop {
        let context = Arc::clone(context);
        tokio::spawn(async move {
            clear_goal(context.goal_map(), &site.thread);
            report(&context, &site, &stop).await;
        });
        return;
    }

    let Some(run_id) = begin_verification(context.goal_map(), &site.thread) else {
        return;
    };

    let context = Arc::clone(context);
    let queues = ChatQueueMap::clone(queues);
    tokio::spawn(async move {
        let root = context.root_for_chat(site.chat).root;
        let verdict = verify_goal(
            &root,
            &site.thread,
            goal.objective(),
            Some(&site.thread),
            None,
        )
        .await;

        // A verdict is only actionable while it still refers to the live goal.
        if !finish_verification(context.goal_map(), &site.thread, run_id) {
            tracing::info!(thread_id = %site.thread, "Discarded a superseded goal verdict");
            return;
        }

        let verdict = match verdict {
            Ok(verdict) => verdict,
            Err(err) => {
                clear_goal(context.goal_map(), &site.thread);
                report(
                    &context,
                    &site,
                    &GoalOutcome::VerifierFailed {
                        error: err.to_string(),
                    },
                )
                .await;
                return;
            }
        };

        if verdict.completed {
            clear_goal(context.goal_map(), &site.thread);
            report(
                &context,
                &site,
                &GoalOutcome::Completed {
                    reason: verdict.reason,
                },
            )
            .await;
            return;
        }

        let next_action = verdict.next_action.clone().unwrap_or_default();
        let Some(prompt) = record_continuation(
            context.goal_map(),
            &site.thread,
            verdict.reason.clone(),
            &next_action,
        ) else {
            clear_goal(context.goal_map(), &site.thread);
            report(
                &context,
                &site,
                &GoalOutcome::LimitReached {
                    reason: verdict.reason,
                },
            )
            .await;
            return;
        };

        dispatch_continuation(&context, &queues, &site, prompt).await;
    });
}

/// Posts a terminal outcome to the topic and records it in the thread, so a
/// reopened thread still shows why the run stopped.
async fn report(context: &Arc<BotContext>, site: &TurnSite, outcome: &GoalOutcome) {
    let message = outcome.message();

    if let Err(err) = Thread::with_id(site.thread.clone()).and_then(|mut thread| {
        thread.append(&ThreadEvent::Notice {
            kind: NoticeKind::Goal,
            message: message.clone(),
            ts: chrono::Utc::now().to_rfc3339(),
        })
    }) {
        tracing::warn!(%err, "Failed to persist goal notice");
    }

    if let Err(err) = context
        .client()
        .send_message(site.chat, &message, None, site.topic)
        .await
    {
        tracing::warn!(%err, "Failed to post goal outcome");
    }
}

/// Queues the next round as an ordinary turn in the same topic.
async fn dispatch_continuation(
    context: &Arc<BotContext>,
    queues: &ChatQueueMap,
    site: &TurnSite,
    prompt: String,
) {
    let message_id = SYNTHETIC_MESSAGE_ID.fetch_sub(1, Ordering::Relaxed);
    let mut value = json!({
        "message_id": message_id,
        "chat": {
            "id": site.chat,
            "type": if site.topic.is_some() { "supergroup" } else { "private" },
            "is_forum": site.topic.is_some(),
        },
        "from": { "id": site.user, "is_bot": false },
        "text": prompt,
    });
    if let Some(topic) = site.topic {
        value["message_thread_id"] = json!(topic);
    }

    match serde_json::from_value::<crate::telegram::Message>(value) {
        Ok(message) => dispatch_message(queues, context, message).await,
        Err(err) => {
            tracing::error!(%err, "Failed to build goal continuation message");
            clear_goal(context.goal_map(), &site.thread);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal() -> Goal {
        Goal::new("ship it", 2).unwrap()
    }

    #[test]
    fn set_and_clear_round_trip() {
        let map = new_goal_map();
        set_goal(&map, "t1", goal());

        let cleared = clear_goal(&map, "t1").expect("goal was active");
        assert_eq!(cleared.objective(), "ship it");

        // A restart drops the map entirely; clearing twice is the same shape as
        // clearing a goal that never survived one.
        assert!(clear_goal(&map, "t1").is_none());
    }
}
