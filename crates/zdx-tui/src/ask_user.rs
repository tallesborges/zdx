//! TUI-side `ask_user_question` tool: lets the agent ask the user a mid-run
//! question and wait for the answer.
//!
//! The tool handler only registers a pending question (keyed by the run's
//! thread id) and waits on a oneshot channel. Slice 1: the user answers by
//! typing — the input submit path resolves the pending entry instead of
//! queueing a prompt. A picker overlay arrives in a later slice, gated on the
//! handler's `REGISTERED_MARKER` event.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::oneshot;
use zdx_engine::core::agent::EventSender;
use zdx_engine::core::events::{AgentEvent, ToolOutput};
pub(crate) use zdx_engine::tools::ask_user_question::TOOL_NAME;
use zdx_engine::tools::ask_user_question::parse_input;
use zdx_engine::tools::{ToolContext, ToolHandler};

pub(crate) struct PendingQuestion {
    /// Tool-use id of the call, used to reject stale resolutions.
    tool_use_id: String,
    sender: oneshot::Sender<String>,
    question: String,
    options: Vec<QuestionOption>,
}

#[derive(Clone)]
pub(crate) struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// Snapshot of a pending question for the picker overlay.
#[derive(Clone)]
pub(crate) struct QuestionView {
    pub tool_use_id: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
}

/// Questions currently waiting for a user answer, keyed by thread id.
///
/// Uses a std `Mutex` (never held across awaits) so the handler's drop guard
/// can clean up synchronously when the engine aborts the tool future.
pub(crate) type PendingQuestionMap = Arc<Mutex<HashMap<String, PendingQuestion>>>;

pub(crate) fn new_pending_map() -> PendingQuestionMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Builds the tool handler closure capturing the pending map.
pub(crate) fn handler(pending: PendingQuestionMap) -> ToolHandler {
    Arc::new(move |input: &Value, ctx: &ToolContext| {
        let input = input.clone();
        let pending = Arc::clone(&pending);
        let thread_id = ctx.current_thread_id.clone();
        let tool_use_id = ctx.tool_use_id.clone();
        let event_sender = ctx.event_sender.clone();
        Box::pin(async move {
            execute(
                &input,
                thread_id.as_deref(),
                tool_use_id.as_deref(),
                &pending,
                event_sender.as_ref(),
            )
            .await
        })
    })
}

async fn execute(
    input: &Value,
    thread_id: Option<&str>,
    tool_use_id: Option<&str>,
    pending: &PendingQuestionMap,
    event_sender: Option<&EventSender>,
) -> ToolOutput {
    let parsed = match parse_input(input) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let Some(thread_id) = thread_id else {
        return ToolOutput::failure(
            "unsupported_surface",
            "ask_user_question requires a persisted thread",
            None,
        );
    };
    let Some(tool_use_id) = tool_use_id else {
        return ToolOutput::failure(
            "internal",
            "Missing tool_use_id for ask_user_question",
            None,
        );
    };

    let (tx, rx) = oneshot::channel::<String>();
    {
        let mut map = pending.lock().expect("pending question lock poisoned");
        if map.contains_key(thread_id) {
            return ToolOutput::failure(
                "question_pending",
                "Another question is already waiting for the user's answer. Ask one question at \
                 a time.",
                None,
            );
        }
        map.insert(
            thread_id.to_string(),
            PendingQuestion {
                tool_use_id: tool_use_id.to_string(),
                sender: tx,
                question: parsed.question.clone(),
                options: parsed
                    .options
                    .iter()
                    .map(|o| QuestionOption {
                        label: o.label.clone(),
                        description: o.description.clone(),
                    })
                    .collect(),
            },
        );
    }

    // Removes the pending entry (id-checked) even if this future is aborted
    // by engine-side cancellation while awaiting the answer.
    let _guard = PendingGuard {
        pending: Arc::clone(pending),
        thread_id: thread_id.to_string(),
        tool_use_id: tool_use_id.to_string(),
    };

    // Tell the UI the question is registered and answerable. Rendering waits
    // for this marker so answer affordances never appear before the pending
    // entry exists.
    if let Some(sender) = event_sender {
        sender.send(AgentEvent::ToolOutputDelta {
            id: tool_use_id.to_string(),
            chunk: zdx_engine::tools::ask_user_question::REGISTERED_MARKER.to_string(),
        });
    }

    match rx.await {
        Ok(answer) => ToolOutput::success(json!({
            "question": parsed.question,
            "answer": answer,
        })),
        Err(_) => ToolOutput::success(json!({
            "question": parsed.question,
            "answer": Value::Null,
            "note": "The question was dismissed without an answer. Proceed with your best judgment.",
        })),
    }
}

struct PendingGuard {
    pending: PendingQuestionMap,
    thread_id: String,
    tool_use_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.pending.lock()
            && map
                .get(&self.thread_id)
                .is_some_and(|entry| entry.tool_use_id == self.tool_use_id)
        {
            map.remove(&self.thread_id);
        }
    }
}

/// Returns whether a question is currently pending for the given thread.
pub(crate) fn has_pending(pending: &PendingQuestionMap, thread_id: &str) -> bool {
    pending
        .lock()
        .expect("pending question lock poisoned")
        .contains_key(thread_id)
}

/// Snapshots the pending question for `thread_id` (for the picker overlay).
pub(crate) fn pending_view(pending: &PendingQuestionMap, thread_id: &str) -> Option<QuestionView> {
    let map = pending.lock().expect("pending question lock poisoned");
    let entry = map.get(thread_id)?;
    Some(QuestionView {
        tool_use_id: entry.tool_use_id.clone(),
        question: entry.question.clone(),
        options: entry.options.clone(),
    })
}

/// Resolves the pending question for `thread_id` with the user's typed
/// answer. Returns `true` when a question was resolved.
pub(crate) fn answer_with_text(pending: &PendingQuestionMap, thread_id: &str, text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let entry = {
        let mut map = pending.lock().expect("pending question lock poisoned");
        map.remove(thread_id)
    };
    let Some(entry) = entry else {
        return false;
    };
    let _ = entry.sender.send(trimmed.to_string());
    true
}

#[cfg(test)]
mod tests {
    use zdx_engine::core::agent::create_event_channel;

    use super::*;

    #[tokio::test]
    async fn execute_registers_emits_marker_and_resolves_with_typed_answer() {
        let pending = new_pending_map();
        let (tx, mut event_rx) = create_event_channel();
        let input = json!({
            "question": "Q?",
            "options": [{"label": "A"}, {"label": "B"}]
        });

        let pending_clone = Arc::clone(&pending);
        let task = tokio::spawn(async move {
            let sender = EventSender::new(tx);
            execute(
                &input,
                Some("thread-uuid-1"),
                Some("toolu_7"),
                &pending_clone,
                Some(&sender),
            )
            .await
        });

        let event = event_rx.recv().await.expect("marker event");
        match &*event {
            AgentEvent::ToolOutputDelta { id, chunk } => {
                assert_eq!(id, "toolu_7");
                assert_eq!(
                    chunk,
                    zdx_engine::tools::ask_user_question::REGISTERED_MARKER
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }

        assert!(has_pending(&pending, "thread-uuid-1"));
        assert!(!answer_with_text(&pending, "other-thread", "nope"));
        assert!(has_pending(&pending, "thread-uuid-1"));
        assert!(answer_with_text(&pending, "thread-uuid-1", " Beta "));

        let output = task.await.expect("task join");
        match output {
            ToolOutput::Success { data, .. } => assert_eq!(data["answer"], "Beta"),
            other => panic!("unexpected output: {other:?}"),
        }
        assert!(!has_pending(&pending, "thread-uuid-1"));
    }

    #[test]
    fn rejects_second_question_for_same_thread() {
        let pending = new_pending_map();
        let (tx, _rx) = oneshot::channel();
        pending.lock().unwrap().insert(
            "t1".to_string(),
            PendingQuestion {
                tool_use_id: "toolu_1".to_string(),
                sender: tx,
                question: "Q?".to_string(),
                options: vec![],
            },
        );

        assert!(!answer_with_text(&pending, "t1", "  "));
        assert!(has_pending(&pending, "t1"));
        assert!(!has_pending(&pending, "t2"));
    }

    #[test]
    fn tui_tool_config_advertises_ask_user_question() {
        let (_map, tool_config) = crate::state::build_ask_user_tooling();
        assert!(
            tool_config
                .registry
                .definitions()
                .iter()
                .any(|d| d.name == TOOL_NAME)
        );
        match &tool_config.selection {
            zdx_engine::core::agent::ToolSelection::Auto { include, .. } => {
                assert!(include.contains(&TOOL_NAME.to_string()));
            }
            other => panic!("unexpected selection: {other:?}"),
        }
    }
}
