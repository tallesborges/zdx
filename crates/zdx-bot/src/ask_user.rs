//! `ask_user_question` tool: lets the agent ask the user a question with
//! tappable inline-keyboard options mid-run and wait for the answer.
//!
//! The tool handler only registers a pending question and waits (async) on a
//! oneshot channel. The bot's event loop renders the question into the turn's
//! status message (options + cancel in one message) on `ToolInputCompleted`,
//! and freezes it as a Q&A record on `ToolCompleted`. Answers arrive either
//! via an `askq:` inline-keyboard callback or a typed reply in the same
//! chat/topic.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use zdx_engine::core::events::ToolOutput;
use zdx_engine::tools::{ToolContext, ToolDefinition, ToolHandler};

use crate::telegram::{InlineKeyboardButton, TelegramClient};

pub(crate) const TOOL_NAME: &str = "ask_user_question";

/// Key: (`chat_id`, `topic_id`); DMs use topic 0. One pending question per
/// chat/topic at a time.
type PendingKey = (i64, i64);

pub(crate) struct PendingQuestion {
    /// Tool-use id of the call, used in callback data to reject stale taps.
    tool_use_id: String,
    sender: oneshot::Sender<String>,
    option_labels: Vec<String>,
}

/// Shared map of questions currently waiting for a user answer.
///
/// Uses a std `Mutex` (never held across awaits) so the handler's drop guard
/// can clean up synchronously when the engine aborts the tool future.
pub(crate) type PendingQuestionMap = Arc<Mutex<HashMap<PendingKey, PendingQuestion>>>;

pub(crate) fn new_pending_map() -> PendingQuestionMap {
    Arc::new(Mutex::new(HashMap::new()))
}

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

/// Builds the tool handler closure capturing the pending map.
pub(crate) fn handler(pending: PendingQuestionMap) -> ToolHandler {
    Arc::new(move |input: &Value, ctx: &ToolContext| {
        let input = input.clone();
        let pending = Arc::clone(&pending);
        let thread_id = ctx.current_thread_id.clone();
        let tool_use_id = ctx.tool_use_id.clone();
        Box::pin(async move {
            execute(
                &input,
                thread_id.as_deref(),
                tool_use_id.as_deref(),
                &pending,
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
    let Some(tool_use_id) = tool_use_id else {
        return ToolOutput::failure(
            "internal",
            "Missing tool_use_id for ask_user_question",
            None,
        );
    };

    let key: PendingKey = (chat_id, topic_id.unwrap_or(0));
    let (tx, rx) = oneshot::channel::<String>();

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
                tool_use_id: tool_use_id.to_string(),
                sender: tx,
                option_labels: parsed.options.iter().map(|o| o.label.clone()).collect(),
            },
        );
    }

    // Removes the pending entry (id-checked) even if this future is aborted
    // by engine-side cancellation while awaiting the answer.
    let _guard = PendingGuard {
        pending: Arc::clone(pending),
        key,
        tool_use_id: tool_use_id.to_string(),
    };

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
    tool_use_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.pending.lock()
            && map
                .get(&self.key)
                .is_some_and(|entry| entry.tool_use_id == self.tool_use_id)
        {
            map.remove(&self.key);
        }
    }
}

/// Renders the question HTML base text (no answer hint) and option-button
/// rows for the bot's status message. Returns `None` when the tool input is
/// invalid.
pub(crate) fn render_question(
    input: &Value,
    tool_use_id: &str,
) -> Option<(String, Vec<Vec<InlineKeyboardButton>>)> {
    let parsed: QuestionInput = serde_json::from_value(input.clone()).ok()?;
    if parsed.options.len() < 2 || parsed.options.len() > 5 {
        return None;
    }

    let mut lines = vec![format!(
        "❓ <b>{}</b>",
        crate::handlers::message::escape_html(&parsed.question)
    )];
    let described: Vec<&OptionInput> = parsed
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

    let rows = parsed
        .options
        .iter()
        .enumerate()
        .map(|(idx, option)| {
            vec![InlineKeyboardButton {
                text: option.label.clone(),
                callback_data: Some(format!("askq:{tool_use_id}:{idx}")),
                url: None,
            }]
        })
        .collect();

    Some((lines.join("\n"), rows))
}

/// Extracts the user's answer from a completed `ask_user_question` output.
pub(crate) fn answer_from_output(output: &ToolOutput) -> Option<String> {
    match output {
        ToolOutput::Success { data, .. } => data
            .get("answer")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        _ => None,
    }
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

/// Removes and returns the pending question for `key` when `tool_use_id`
/// matches (or when no id check is requested).
fn take_pending(
    pending: &PendingQuestionMap,
    key: PendingKey,
    tool_use_id: Option<&str>,
) -> Option<PendingQuestion> {
    let mut map = pending.lock().expect("pending question lock poisoned");
    match map.get(&key) {
        Some(entry) if tool_use_id.is_none_or(|id| id == entry.tool_use_id) => map.remove(&key),
        _ => None,
    }
}

/// Intercepts a typed message as the answer to a pending question.
///
/// Returns `true` when the message was consumed as an answer (the caller
/// must not enqueue it as a new turn).
pub(crate) fn try_answer_with_text(
    pending: &PendingQuestionMap,
    chat_id: i64,
    topic_id: Option<i64>,
    text: &str,
) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return false;
    }
    let key: PendingKey = (chat_id, topic_id.unwrap_or(0));
    let Some(entry) = take_pending(pending, key, None) else {
        return false;
    };
    let _ = entry.sender.send(trimmed.to_string());
    true
}

/// Handles an `askq:{tool_use_id}:{option_idx}` inline-keyboard callback.
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
    let key: PendingKey = (message.chat.id, message.effective_thread_id().unwrap_or(0));

    let Some((tool_use_id, option_idx)) = parse_askq_callback(data) else {
        let _ = client.answer_callback_query(&callback.id, None).await;
        return;
    };

    let answer = {
        let map = pending.lock().expect("pending question lock poisoned");
        map.get(&key)
            .filter(|entry| entry.tool_use_id == tool_use_id)
            .and_then(|entry| entry.option_labels.get(option_idx).cloned())
    };
    let Some(answer) = answer else {
        let _ = client
            .answer_callback_query(&callback.id, Some("This question is no longer active"))
            .await;
        return;
    };

    if let Some(entry) = take_pending(pending, key, Some(tool_use_id)) {
        let _ = entry.sender.send(answer);
        let _ = client.answer_callback_query(&callback.id, None).await;
    } else {
        let _ = client
            .answer_callback_query(&callback.id, Some("This question is no longer active"))
            .await;
    }
}

fn parse_askq_callback(data: &str) -> Option<(&str, usize)> {
    let (tool_use_id, idx_str) = data.rsplit_once(':')?;
    if tool_use_id.is_empty() {
        return None;
    }
    Some((tool_use_id, idx_str.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_pending(pending: &PendingQuestionMap, key: PendingKey) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert(
            key,
            PendingQuestion {
                tool_use_id: "toolu_1".to_string(),
                sender: tx,
                option_labels: vec!["A".to_string(), "B".to_string()],
            },
        );
        rx
    }

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
        assert_eq!(parse_askq_callback("toolu_1:2"), Some(("toolu_1", 2)));
        assert_eq!(parse_askq_callback("bad"), None);
        assert_eq!(parse_askq_callback(":2"), None);
    }

    #[test]
    fn take_pending_rejects_stale_tool_use_id() {
        let pending = new_pending_map();
        let mut rx = insert_pending(&pending, (1, 0));

        assert!(take_pending(&pending, (1, 0), Some("toolu_2")).is_none());
        assert!(pending.lock().unwrap().contains_key(&(1, 0)));

        let entry = take_pending(&pending, (1, 0), Some("toolu_1")).expect("entry should resolve");
        assert!(pending.lock().unwrap().is_empty());
        let _ = entry.sender.send("A".to_string());
        assert_eq!(rx.try_recv().unwrap(), "A");
    }

    #[test]
    fn typed_text_answers_pending_question() {
        let pending = new_pending_map();
        let mut rx = insert_pending(&pending, (1, 7));

        assert!(!try_answer_with_text(&pending, 1, Some(7), "/command"));
        assert!(!try_answer_with_text(&pending, 1, None, "hello"));
        assert!(try_answer_with_text(
            &pending,
            1,
            Some(7),
            "  custom answer "
        ));
        assert_eq!(rx.try_recv().unwrap(), "custom answer");
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn guard_removes_only_matching_id() {
        let pending = new_pending_map();
        let _rx = insert_pending(&pending, (1, 0));

        drop(PendingGuard {
            pending: Arc::clone(&pending),
            key: (1, 0),
            tool_use_id: "other".to_string(),
        });
        assert!(pending.lock().unwrap().contains_key(&(1, 0)));

        drop(PendingGuard {
            pending: Arc::clone(&pending),
            key: (1, 0),
            tool_use_id: "toolu_1".to_string(),
        });
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn renders_question_text_and_rows() {
        let input = json!({
            "question": "Pick one?",
            "options": [
                {"label": "A", "description": "first"},
                {"label": "B"}
            ]
        });
        let (text, rows) = render_question(&input, "toolu_9").expect("should render");
        assert!(text.contains("Pick one?"));
        assert!(text.contains("first"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].callback_data.as_deref(), Some("askq:toolu_9:0"));
    }

    #[test]
    fn extracts_answer_from_output() {
        let output = ToolOutput::success(json!({"question": "Q", "answer": "A"}));
        assert_eq!(answer_from_output(&output).as_deref(), Some("A"));
        let dismissed = ToolOutput::success(json!({"question": "Q", "answer": null}));
        assert_eq!(answer_from_output(&dismissed), None);
    }
}
