//! `ask_user_question` tool: lets the agent ask the user a question with
//! tappable inline-keyboard options mid-run and wait for the answer.
//!
//! The tool handler blocks (async) on a oneshot channel until either an
//! inline button is tapped or the user types a reply in the same chat/topic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use zdx_engine::core::events::ToolOutput;
use zdx_engine::tools::{ToolContext, ToolDefinition, ToolHandler};

use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};

pub(crate) const TOOL_NAME: &str = "ask_user_question";

/// Key: (`chat_id`, `topic_id`); DMs use topic 0. One pending question per
/// chat/topic at a time.
type PendingKey = (i64, i64);

pub(crate) struct PendingQuestion {
    /// Unique id used in callback data to reject stale button taps.
    qid: u64,
    sender: oneshot::Sender<String>,
    /// `message_id` of the question message (to strip the keyboard on answer).
    message_id: Option<i64>,
    option_labels: Vec<String>,
    /// Rendered HTML of the question message, for answered-state edits.
    rendered_text: String,
}

/// Shared map of questions currently waiting for a user answer.
///
/// Uses a std `Mutex` (never held across awaits) so the handler's drop guard
/// can clean up synchronously when the engine aborts the tool future.
pub(crate) type PendingQuestionMap = Arc<Mutex<HashMap<PendingKey, PendingQuestion>>>;

pub(crate) fn new_pending_map() -> PendingQuestionMap {
    Arc::new(Mutex::new(HashMap::new()))
}

static NEXT_QID: AtomicU64 = AtomicU64::new(1);

#[derive(Deserialize)]
struct QuestionInput {
    question: String,
    options: Vec<OptionInput>,
}

#[derive(Deserialize)]
struct OptionInput {
    label: String,
    #[serde(default)]
    description: String,
}

pub(crate) fn definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_NAME.to_string(),
        description: "Ask the user one question with tappable answer options, then wait for \
                      their reply. Use this when you are blocked on a decision that is genuinely \
                      the user's to make — clarifying ambiguous instructions, choosing between \
                      approaches, or offering concrete follow-up directions. Do NOT use it for \
                      decisions you can resolve from context or sensible defaults; overusing it \
                      interrupts the user. The user can always type a free-form reply instead of \
                      tapping an option — treat whatever answer comes back as authoritative. If \
                      you recommend an option, put it first and append ' (Recommended)' to its \
                      label. Do not add an 'Other' or 'Something else' option. Ask one question \
                      per call; call again for follow-up questions."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "A clear, specific question ending with a question mark."
                },
                "options": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 5,
                    "description": "2-5 distinct, meaningful choices.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {
                                "type": "string",
                                "description": "Concise button text (1-5 words)."
                            },
                            "description": {
                                "type": "string",
                                "description": "Optional one-line explanation of trade-offs or implications."
                            }
                        },
                        "required": ["label"]
                    }
                }
            },
            "required": ["question", "options"]
        }),
    }
}

/// Builds the tool handler closure capturing the pending map and the
/// Telegram client.
pub(crate) fn handler(pending: PendingQuestionMap, client: TelegramClient) -> ToolHandler {
    Arc::new(move |input: &Value, ctx: &ToolContext| {
        let input = input.clone();
        let pending = Arc::clone(&pending);
        let client = client.clone();
        let thread_id = ctx.current_thread_id.clone();
        Box::pin(async move { execute(&input, thread_id.as_deref(), &pending, &client).await })
    })
}

async fn execute(
    input: &Value,
    thread_id: Option<&str>,
    pending: &PendingQuestionMap,
    client: &TelegramClient,
) -> ToolOutput {
    let parsed: QuestionInput = match serde_json::from_value(input.clone()) {
        Ok(parsed) => parsed,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Invalid ask_user_question input: {err}"),
                None,
            );
        }
    };
    if parsed.options.len() < 2 || parsed.options.len() > 5 {
        return ToolOutput::failure(
            "invalid_input",
            "ask_user_question requires 2-5 options",
            None,
        );
    }

    let Some((chat_id, topic_id)) = thread_id.and_then(parse_telegram_thread_id) else {
        return ToolOutput::failure(
            "unsupported_surface",
            "ask_user_question is only available on the Telegram bot surface",
            None,
        );
    };

    let key: PendingKey = (chat_id, topic_id.unwrap_or(0));
    let qid = NEXT_QID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel::<String>();
    let text = build_question_text(&parsed);

    {
        let mut map = pending.lock().expect("pending question lock poisoned");
        if map.contains_key(&key) {
            return ToolOutput::failure(
                "question_pending",
                "Another question is already waiting for the user's answer. Ask one question at \
                 a time.",
                None,
            );
        }
        map.insert(
            key,
            PendingQuestion {
                qid,
                sender: tx,
                message_id: None,
                option_labels: parsed.options.iter().map(|o| o.label.clone()).collect(),
                rendered_text: text.clone(),
            },
        );
    }

    // Removes the pending entry (qid-checked) even if this future is aborted
    // by engine-side cancellation while awaiting the answer.
    let _guard = PendingGuard {
        pending: Arc::clone(pending),
        key,
        qid,
    };

    let markup = build_keyboard(qid, &parsed.options);
    let message_id = match client
        .send_message_with_markup(chat_id, &text, None, topic_id, &markup)
        .await
    {
        Ok(message) => message.id,
        Err(err) => {
            return ToolOutput::failure(
                "send_failed",
                format!("Failed to send question to Telegram: {err}"),
                None,
            );
        }
    };

    {
        let mut map = pending.lock().expect("pending question lock poisoned");
        if let Some(entry) = map.get_mut(&key)
            && entry.qid == qid
        {
            entry.message_id = Some(message_id);
        }
    }

    // Wait indefinitely: no LLM connection is open while waiting, the user
    // can cancel the turn, and a bot restart clears the run anyway.
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
    key: PendingKey,
    qid: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.pending.lock()
            && map
                .get(&self.key)
                .is_some_and(|entry| entry.qid == self.qid)
        {
            map.remove(&self.key);
        }
    }
}

fn build_keyboard(qid: u64, options: &[OptionInput]) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: options
            .iter()
            .enumerate()
            .map(|(idx, option)| {
                vec![InlineKeyboardButton {
                    text: option.label.clone(),
                    callback_data: Some(format!("askq:{qid}:{idx}")),
                    url: None,
                }]
            })
            .collect(),
    }
}

fn build_question_text(input: &QuestionInput) -> String {
    let mut lines = vec![format!(
        "❓ <b>{}</b>",
        crate::handlers::message::escape_html(&input.question)
    )];
    let described: Vec<&OptionInput> = input
        .options
        .iter()
        .filter(|o| !o.description.trim().is_empty())
        .collect();
    if !described.is_empty() {
        lines.push(String::new());
        for option in described {
            lines.push(format!(
                "• <b>{}</b> — {}",
                crate::handlers::message::escape_html(&option.label),
                crate::handlers::message::escape_html(option.description.trim())
            ));
        }
    }
    lines.push(String::new());
    lines.push("<i>Tap an option or reply with your answer.</i>".to_string());
    lines.join("\n")
}

/// Parses a bot thread id (`telegram-{chat_id}` or
/// `telegram-{chat_id}-topic-{topic_id}`) into (`chat_id`, `topic_id`).
fn parse_telegram_thread_id(thread_id: &str) -> Option<(i64, Option<i64>)> {
    let rest = thread_id.strip_prefix("telegram-")?;
    if let Some((chat_part, topic_part)) = rest.rsplit_once("-topic-") {
        Some((chat_part.parse().ok()?, Some(topic_part.parse().ok()?)))
    } else {
        Some((rest.parse().ok()?, None))
    }
}

/// Resolves a pending question with the user's answer.
///
/// Returns `(message_id, rendered_text)` of the answered question (for
/// keyboard cleanup) when a pending question existed for the key.
fn resolve_pending(
    pending: &PendingQuestionMap,
    key: PendingKey,
    qid: Option<u64>,
    answer: &str,
) -> Option<(Option<i64>, String)> {
    let entry = {
        let mut map = pending.lock().expect("pending question lock poisoned");
        match map.get(&key) {
            Some(entry) if qid.is_none_or(|qid| qid == entry.qid) => map.remove(&key),
            _ => None,
        }
    }?;
    let resolved = (entry.message_id, entry.rendered_text);
    let _ = entry.sender.send(answer.to_string());
    Some(resolved)
}

/// Intercepts a typed message as the answer to a pending question.
///
/// Returns `true` when the message was consumed as an answer (the caller
/// must not enqueue it as a new turn).
pub(crate) async fn try_answer_with_text(
    pending: &PendingQuestionMap,
    client: &TelegramClient,
    chat_id: i64,
    topic_id: Option<i64>,
    text: &str,
) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return false;
    }
    let key: PendingKey = (chat_id, topic_id.unwrap_or(0));
    let Some((message_id, rendered_text)) = resolve_pending(pending, key, None, trimmed) else {
        return false;
    };
    if let Some(message_id) = message_id {
        let updated = format!("{rendered_text}\n\n✅ <i>Answered by reply.</i>");
        let _ = client
            .edit_message_text(chat_id, message_id, &updated, None)
            .await;
    }
    true
}

/// Handles an `askq:{qid}:{option_idx}` inline-keyboard callback.
pub(crate) async fn handle_callback(
    pending: &PendingQuestionMap,
    client: &TelegramClient,
    callback: &crate::telegram::CallbackQuery,
    data: &str,
) {
    let Some(message) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };
    let chat_id = message.chat.id;
    let key: PendingKey = (chat_id, message.effective_thread_id().unwrap_or(0));

    let parsed = parse_askq_callback(data);
    let Some((qid, option_idx)) = parsed else {
        let _ = client.answer_callback_query(&callback.id, None).await;
        return;
    };

    let label = {
        let map = pending.lock().expect("pending question lock poisoned");
        map.get(&key)
            .filter(|entry| entry.qid == qid)
            .and_then(|entry| entry.option_labels.get(option_idx).cloned())
    };
    let Some(label) = label else {
        let _ = client
            .answer_callback_query(&callback.id, Some("This question is no longer active"))
            .await;
        return;
    };

    if let Some((message_id, rendered_text)) = resolve_pending(pending, key, Some(qid), &label) {
        let updated = format!(
            "{rendered_text}\n\n✅ <b>{}</b>",
            crate::handlers::message::escape_html(&label)
        );
        let target_message_id = message_id.unwrap_or(message.id);
        let _ = client
            .edit_message_text(chat_id, target_message_id, &updated, None)
            .await;
        let _ = client.answer_callback_query(&callback.id, None).await;
    } else {
        let _ = client
            .answer_callback_query(&callback.id, Some("This question is no longer active"))
            .await;
    }
}

fn parse_askq_callback(data: &str) -> Option<(u64, usize)> {
    let (qid_str, idx_str) = data.split_once(':')?;
    Some((qid_str.parse().ok()?, idx_str.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dm_thread_id() {
        assert_eq!(
            parse_telegram_thread_id("telegram-12345"),
            Some((12345, None))
        );
    }

    #[test]
    fn parses_negative_chat_with_topic() {
        assert_eq!(
            parse_telegram_thread_id("telegram--1003841286314-topic-7194"),
            Some((-1_003_841_286_314, Some(7194)))
        );
    }

    #[test]
    fn rejects_non_telegram_thread_ids() {
        assert_eq!(parse_telegram_thread_id("abc123"), None);
        assert_eq!(parse_telegram_thread_id("telegram-notanumber"), None);
    }

    #[test]
    fn parses_askq_callback_data() {
        assert_eq!(parse_askq_callback("7:2"), Some((7, 2)));
        assert_eq!(parse_askq_callback("bad"), None);
    }

    #[test]
    fn resolve_pending_rejects_stale_qid() {
        let pending = new_pending_map();
        let (tx, mut rx) = oneshot::channel();
        pending.lock().unwrap().insert(
            (1, 0),
            PendingQuestion {
                qid: 5,
                sender: tx,
                message_id: Some(99),
                option_labels: vec!["A".to_string()],
                rendered_text: "q".to_string(),
            },
        );

        assert!(resolve_pending(&pending, (1, 0), Some(4), "A").is_none());
        assert!(pending.lock().unwrap().contains_key(&(1, 0)));

        assert_eq!(
            resolve_pending(&pending, (1, 0), Some(5), "A"),
            Some((Some(99), "q".to_string()))
        );
        assert!(pending.lock().unwrap().is_empty());
        assert_eq!(rx.try_recv().unwrap(), "A");
    }

    #[test]
    fn guard_removes_only_matching_qid() {
        let pending = new_pending_map();
        let (tx, _rx) = oneshot::channel();
        pending.lock().unwrap().insert(
            (1, 0),
            PendingQuestion {
                qid: 5,
                sender: tx,
                message_id: None,
                option_labels: vec![],
                rendered_text: String::new(),
            },
        );

        drop(PendingGuard {
            pending: Arc::clone(&pending),
            key: (1, 0),
            qid: 4,
        });
        assert!(pending.lock().unwrap().contains_key(&(1, 0)));

        drop(PendingGuard {
            pending: Arc::clone(&pending),
            key: (1, 0),
            qid: 5,
        });
        assert!(pending.lock().unwrap().is_empty());
    }
}
