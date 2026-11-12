//! Turn inputs: the `[worker update]` prompt a finished worker turn sends its
//! orchestrator, and the user message a turn appends to its thread.

use anyhow::{Context, Result};

use crate::core::context::{RuntimeContext, resolve_context_to_attach};
use crate::core::thread_persistence::{self, Thread, ThreadEvent};
use crate::core::workers::{CompletionEvent, WorkerStatus};
use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};

/// Longest worker final text or error, in Unicode characters, kept inline in
/// a `[worker update]` and in `get_thread_status`'s `latest_final_text`.
/// Longer text is spilled whole to a temp file that the orchestrator reads
/// with `Read`.
pub const MAX_WORKER_TEXT_CHARS: usize = 8000;

/// A worker's final text or error, cut to [`MAX_WORKER_TEXT_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedWorkerText {
    /// The first [`MAX_WORKER_TEXT_CHARS`] characters, or all of the text.
    pub head: String,
    /// Character count of the full text.
    pub total_chars: usize,
    pub truncated: bool,
    /// Temp file holding the exact full text; set only when `truncated` and
    /// the file was written.
    pub file: Option<String>,
    /// 1-indexed line of the full text that holds the cut point, for `Read`'s
    /// line-based `offset`; set only when `truncated`. Reading from it repeats
    /// the part of that line already shown inline, so nothing is skipped.
    pub resume_line: Option<usize>,
}

/// Cuts worker text to [`MAX_WORKER_TEXT_CHARS`] and, when it is longer,
/// writes the exact full text to a temp file.
#[must_use]
pub fn bound_worker_text(text: &str) -> BoundedWorkerText {
    let total_chars = text.chars().count();
    if total_chars <= MAX_WORKER_TEXT_CHARS {
        return BoundedWorkerText {
            head: text.to_string(),
            total_chars,
            truncated: false,
            file: None,
            resume_line: None,
        };
    }
    let head: String = text.chars().take(MAX_WORKER_TEXT_CHARS).collect();
    let resume_line = head.matches('\n').count() + 1;
    BoundedWorkerText {
        head,
        total_chars,
        truncated: true,
        file: zdx_tools::write_spill_file("worker-text", text.as_bytes()),
        resume_line: Some(resume_line),
    }
}

/// Inline worker text for the update prompt, with a notice that names the
/// spill file when the text was cut.
fn worker_text_for_prompt(text: &str) -> String {
    let bounded = bound_worker_text(text);
    if !bounded.truncated {
        return bounded.head;
    }
    let shown = format!(
        "showing the first {MAX_WORKER_TEXT_CHARS} of {} characters",
        bounded.total_chars
    );
    match bounded.file {
        Some(path) => format!(
            "{}…\n[truncated: {shown}. The exact full text is in `{path}`. The inline text ends in line {}; use Read on that file with offset {} for the rest.]",
            bounded.head,
            bounded.resume_line.unwrap_or(1),
            bounded.resume_line.unwrap_or(1),
        ),
        None => format!(
            "{}…\n[truncated: {shown}. The full text could not be saved to a file; Read_Thread gives a summary of the rest.]",
            bounded.head
        ),
    }
}

/// Compact prompt describing one finished worker turn.
///
/// `worker_context` is the [`crate::tools::orchestrator::thread_context_json`]
/// value for the worker: the latest recorded request's input tokens with its
/// limit and percentage. It is rendered as its own metadata section after the
/// final message, never merged into it, with no cutoff and no recommendation.
#[must_use]
pub fn worker_update_prompt(event: &CompletionEvent, worker_context: &serde_json::Value) -> String {
    let worker = &event.worker_thread_id;
    let status = event.status.as_str();

    let mut prompt = format!(
        "[worker update] Worker thread `{worker}` finished a turn with status: {status}.\n"
    );
    match event.status {
        WorkerStatus::Completed => {
            let text = event.final_text.as_deref().unwrap_or_default();
            prompt.push_str("\nFinal message:\n");
            prompt.push_str(&worker_text_for_prompt(text));
            prompt.push('\n');
        }
        _ => {
            if let Some(error) = event.error.as_deref() {
                prompt.push_str("\nError:\n");
                prompt.push_str(&worker_text_for_prompt(error));
                prompt.push('\n');
            }
        }
    }
    prompt.push_str(&steers_section(&event.steers));
    prompt.push_str(&worker_context_section(worker_context));
    prompt
}

/// One line per steer outcome of the turn; empty when it took no steers.
fn steers_section(steers: &[crate::core::workers::SteerReceipt]) -> String {
    use std::fmt::Write as _;

    use crate::core::workers::SteerState;

    let ids = |state: SteerState| {
        steers
            .iter()
            .filter(|steer| steer.state == state)
            .map(|steer| steer.steer_id.to_string())
            .collect::<Vec<_>>()
    };
    let mut lines = String::new();
    let mut line = |state: SteerState, text: &str| {
        let ids = ids(state);
        if !ids.is_empty() {
            let _ = writeln!(lines, "- {text}: {}", ids.join(", "));
        }
    };
    line(SteerState::Injected, "read in this turn");
    line(
        SteerState::Queued,
        "not read in time, queued as the next prompt",
    );
    line(SteerState::Discarded, "dropped by cancel");
    line(
        SteerState::Uncertain,
        "unknown whether read (the worker stopped reporting); not re-sent",
    );
    if lines.is_empty() {
        lines
    } else {
        format!("\nSteers (steer_id):\n{lines}")
    }
}

/// Runtime context-usage metadata for a worker update.
///
/// Same basis as `Get_Thread_Status`: the latest recorded request's input
/// tokens (including cache reads/writes), not cumulative spend, with output
/// tokens excluded. Unknown usage renders as `unknown`, never `0%`; known
/// tokens without a resolvable limit keep the tokens with the limit unknown.
/// Deliberately reports only: no cutoff, no fresh-thread recommendation.
fn worker_context_section(worker_context: &serde_json::Value) -> String {
    let input = worker_context
        .get("input_tokens")
        .and_then(serde_json::Value::as_u64);
    let limit = worker_context
        .get("context_limit")
        .and_then(serde_json::Value::as_u64)
        .filter(|limit| *limit > 0);
    let percent = worker_context
        .get("percent_used")
        .and_then(serde_json::Value::as_f64)
        .filter(|percent| percent.is_finite());
    match (input, limit, percent) {
        (Some(input), Some(limit), Some(percent)) => format!(
            "\nWorker context: ~{:.0}% used ({} / {} tokens)\nBasis: last recorded request input\n",
            percent,
            format_compact_tokens(input),
            format_compact_tokens(limit)
        ),
        (Some(input), _, _) => format!(
            "\nWorker context: {} tokens recorded (limit unknown)\nBasis: last recorded request input\n",
            format_compact_tokens(input)
        ),
        (None, _, _) => "\nWorker context: unknown (no recorded request input)\n".to_string(),
    }
}

/// Compact token count for prompts (`120k`, `1M`); an estimate, so whole
/// units are enough.
#[allow(clippy::cast_precision_loss)]
fn format_compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.0}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.0}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Cuts `text` to `max_chars` characters, pointing at `Read_Thread` for the
/// rest.
#[must_use]
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}… [truncated — use Read_Thread for the rest]")
}

/// One inline image sent with a user message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputImage {
    pub mime_type: String,
    /// Base64-encoded bytes.
    pub data: String,
}

/// Appends a user message to the thread and to the in-memory history.
///
/// The persisted event carries the text and the advisory runtime-context
/// block (attached only when it differs from the last attached one). Inline
/// images go to the live turn only. `runtime_note` (undelivered reply parts,
/// posts by other runs) becomes the first text block, which is exactly what
/// replay rebuilds once this message is on disk.
///
/// # Errors
/// Returns an error if the event cannot be appended.
pub fn record_user_input(
    thread: &mut Thread,
    messages: &mut Vec<ChatMessage>,
    text: String,
    images: &[InputImage],
    runtime_context: Option<&RuntimeContext>,
    runtime_note: Option<&str>,
) -> Result<()> {
    let last_key = thread_persistence::last_attached_context_key_from_messages(messages);
    let attach = resolve_context_to_attach(runtime_context, last_key.as_deref());
    let (block, key) = match attach {
        Some(attach) => (Some(attach.block), Some(attach.key)),
        None => (None, None),
    };
    thread
        .append(&ThreadEvent::user_message_with_context(
            text.clone(),
            block.clone(),
            key.clone(),
        ))
        .context("append user message")?;

    if images.is_empty() && runtime_note.is_none() {
        messages.push(ChatMessage::user(text).with_runtime_context(block, key));
        return Ok(());
    }

    let mut blocks = Vec::with_capacity(1 + images.len());
    blocks.push(ChatContentBlock::text(text));
    for image in images {
        blocks.push(ChatContentBlock::Image {
            mime_type: image.mime_type.clone(),
            data: image.data.clone(),
        });
    }
    let content = match runtime_note {
        Some(note) => thread_persistence::user_message_with_note(note, blocks),
        None => MessageContent::Blocks(blocks),
    };

    messages.push(ChatMessage {
        role: "user".to_string(),
        phase: None,
        context: block,
        context_key: key,
        content,
    });
    Ok(())
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
    fn completed_update_carries_final_text_without_topic_link() {
        let prompt = worker_update_prompt(
            &CompletionEvent {
                final_text: Some("all done".to_string()),
                mirror_url: Some("https://t.me/c/1/2".to_string()),
                ..completion(WorkerStatus::Completed)
            },
            &serde_json::Value::Null,
        );
        assert!(prompt.starts_with("[worker update]"));
        assert!(prompt.contains("`worker-1`"));
        assert!(prompt.contains("status: completed"));
        assert!(!prompt.contains("https://t.me/c/1/2"));
        assert!(prompt.contains("all done"));
        assert!(prompt.contains("Worker context: unknown"));
    }

    #[test]
    fn failed_update_carries_error_and_bounds_length() {
        let prompt = worker_update_prompt(
            &CompletionEvent {
                error: Some("x".repeat(10_000)),
                ..completion(WorkerStatus::Failed)
            },
            &serde_json::Value::Null,
        );
        assert!(prompt.contains("status: failed"));
        assert!(prompt.contains("[truncated: showing the first 8000 of 10000 characters"));
        assert!(prompt.contains("ends in line 1; use Read on that file with offset 1"));
        assert!(prompt.chars().count() < MAX_WORKER_TEXT_CHARS + 1100);
        let path = spill_path(&prompt);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x".repeat(10_000));
        let _ = std::fs::remove_file(path);
    }

    /// The path between the backticks of the truncation notice.
    fn spill_path(prompt: &str) -> String {
        let notice = prompt.find("full text is in `").expect("spill notice") + 17;
        let end = prompt[notice..].find('`').expect("closing backtick");
        prompt[notice..notice + end].to_string()
    }

    #[test]
    fn long_final_text_is_spilled_whole_to_a_readable_file() {
        let text = format!("{}é tail marker", "report line\n".repeat(800));
        let prompt = worker_update_prompt(
            &CompletionEvent {
                final_text: Some(text.clone()),
                ..completion(WorkerStatus::Completed)
            },
            &serde_json::Value::Null,
        );
        assert!(!prompt.contains("tail marker"));
        // 8000 chars of 12-char lines: 666 full lines, then the cut lands
        // inside line 667.
        assert!(
            prompt.contains(
                "The inline text ends in line 667; use Read on that file with offset 667"
            )
        );
        let path = spill_path(&prompt);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let _ = std::fs::remove_file(path);
    }

    /// Text a line-based `Read` returns from `offset` (1-indexed) to the end.
    fn read_from_line(text: &str, offset: usize) -> String {
        text.split('\n')
            .skip(offset - 1)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn resume_line_covers_everything_after_the_cut() {
        // Cut inside a line: the resume line repeats the shown part of it.
        let mid_line = format!("{}\n{}", "a".repeat(MAX_WORKER_TEXT_CHARS - 2), "bcd\nrest");
        // Cut right after a line break: the resume line starts at the cut.
        let at_break = format!("{}\nnext\nrest", "a".repeat(MAX_WORKER_TEXT_CHARS - 1));
        for text in [mid_line, at_break] {
            let bounded = bound_worker_text(&text);
            let rest = &text[bounded.head.len()..];
            let resumed = read_from_line(&text, bounded.resume_line.expect("truncated"));
            assert!(
                resumed.ends_with(rest),
                "{resumed:?} must end with {rest:?}"
            );
            if bounded.head.ends_with('\n') {
                assert_eq!(resumed, rest);
            }
            let _ = std::fs::remove_file(bounded.file.expect("spill file"));
        }
    }

    #[test]
    fn final_text_at_the_cap_stays_inline_without_a_file() {
        let text = "y".repeat(MAX_WORKER_TEXT_CHARS);
        let bounded = bound_worker_text(&text);
        assert!(!bounded.truncated);
        assert_eq!(bounded.head, text);
        assert!(bounded.file.is_none());

        let prompt = worker_update_prompt(
            &CompletionEvent {
                final_text: Some(text.clone()),
                ..completion(WorkerStatus::Completed)
            },
            &serde_json::Value::Null,
        );
        assert!(prompt.contains(&text));
        assert!(!prompt.contains("[truncated"));
    }

    #[test]
    fn worker_context_reports_percent_and_basis() {
        let prompt = worker_update_prompt(
            &CompletionEvent {
                final_text: Some("all done".to_string()),
                ..completion(WorkerStatus::Completed)
            },
            &serde_json::json!({
                "input_tokens": 120_000,
                "context_limit": 200_000,
                "percent_used": 60.0,
                "basis": "last_recorded_request_input",
            }),
        );
        assert!(prompt.contains("Worker context: ~60% used (120k / 200k tokens)"));
        assert!(prompt.contains("Basis: last recorded request input"));
    }

    #[test]
    fn worker_context_unknown_is_never_zero_percent() {
        let prompt = worker_update_prompt(
            &completion(WorkerStatus::Completed),
            &serde_json::Value::Null,
        );
        assert!(prompt.contains("Worker context: unknown"));
        assert!(!prompt.contains("0%"));
    }

    #[test]
    fn worker_context_keeps_tokens_when_limit_is_unknown() {
        let prompt = worker_update_prompt(
            &completion(WorkerStatus::Completed),
            &serde_json::json!({ "input_tokens": 120_000 }),
        );
        assert!(prompt.contains("120k tokens recorded (limit unknown)"));
        assert!(prompt.contains("Basis: last recorded request input"));
        assert!(!prompt.contains("used ("));
    }

    /// The metadata is its own section after the final answer: it must not be
    /// merged into the worker's text, and it reports only with no cutoff or
    /// fresh-thread recommendation.
    #[test]
    fn worker_context_is_separate_metadata_without_recommendation() {
        let prompt = worker_update_prompt(
            &CompletionEvent {
                final_text: Some("shipped it".to_string()),
                ..completion(WorkerStatus::Completed)
            },
            &serde_json::json!({
                "input_tokens": 120_000,
                "context_limit": 200_000,
                "percent_used": 60.0,
                "basis": "last_recorded_request_input",
            }),
        );
        let final_at = prompt.find("Final message:").expect("final answer section");
        let context_at = prompt
            .find("Worker context:")
            .expect("context metadata section");
        assert!(final_at < context_at);
        assert!(prompt.contains("shipped it"));
        assert!(!prompt.contains("fresh thread"));
        assert!(!prompt.contains("new thread"));
    }

    /// A failed delivery must reach the model on the next user message, and
    /// the live message must equal what replay rebuilds on the turn after.
    #[test]
    fn runtime_note_is_live_and_replayed_identically() {
        use crate::core::delivery::{DeliveryItem, DeliveryRecord};

        let _home = crate::test_support::temp_zdx_home();
        let id = format!("test-delivery-{}", uuid::Uuid::new_v4());
        {
            let mut thread = Thread::with_id(id.clone()).expect("thread");
            thread
                .append(&ThreadEvent::user_message("make it"))
                .expect("append");
            thread
                .append(&ThreadEvent::assistant_message(
                    "Ready <media>/tmp/x.html</media>",
                ))
                .expect("append");
            let mut file = DeliveryItem::file("/tmp/x.html");
            file.mark_failed("Telegram API error for sendDocument (429): Too Many Requests");
            thread
                .append(&ThreadEvent::delivery(DeliveryRecord::new(
                    "telegram",
                    Some(-100),
                    Some(7),
                    vec![file],
                )))
                .expect("append");
        }

        let replay = thread_persistence::load_thread_replay(&id).expect("load");
        let note = replay.trailing_note.expect("delivery note");
        assert!(note.contains("file /tmp/x.html: not delivered"));
        let mut thread = Thread::with_id(id.clone()).expect("thread");
        let mut messages = replay.messages;
        record_user_input(
            &mut thread,
            &mut messages,
            "where is it?".to_string(),
            &[],
            None,
            Some(&note),
        )
        .expect("record");

        let replayed = thread_persistence::load_thread_replay(&id).expect("replay");
        assert_eq!(replayed.trailing_note, None);
        assert_eq!(
            format!("{:?}", replayed.messages.last().map(|m| &m.content)),
            format!("{:?}", messages.last().map(|m| &m.content)),
        );
    }
}
