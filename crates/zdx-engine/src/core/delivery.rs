//! Delivery records: what actually reached an external chat surface after a
//! turn produced its final reply.
//!
//! A turn's final text says what the model *meant* to send (`<media>` tags,
//! reply text). It is not evidence of delivery: a send can fail, be refused by
//! flood control, or time out with no answer. Surfaces that deliver replies
//! (the Telegram bot) persist a [`DeliveryRecord`] as a
//! `ThreadEvent::Delivery`, and every reader derives its view from here:
//!
//! - replay prepends [`model_note`] to the next user message, so the model
//!   does not claim a failed file was attached;
//! - transcripts and the Mini App show [`notice_text`];
//! - the bot posts [`user_summary`] once after a partial delivery.
//!
//! `Sent` means the surface accepted the message, not that the user read it.
//! `Unknown` means the request may or may not have arrived (for example a
//! timeout after the request was written); it is never resent automatically.
//! `Pending` on a stored record means the outcome is not recorded yet: the
//! delivery is still running, or the process stopped mid-send. Either way it
//! is unconfirmed.

use serde::{Deserialize, Serialize};

/// Longest error excerpt kept per item, in characters.
const MAX_ERROR_CHARS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Sent,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryItemKind {
    /// The reply text (possibly carried as a file caption).
    Text,
    /// A file referenced by a `<media>` tag.
    File,
}

/// One deliverable part of a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryItem {
    pub kind: DeliveryItemKind,
    /// Local path for `File` items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub status: DeliveryStatus,
    /// Id the surface assigned to the delivered message, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// For reply text sent as several messages (too long for one): which
    /// part this item is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<TextPart>,
    /// For `File` items: the surface posted a message that links to the file
    /// (a Telegram file card) instead of uploading it. `Sent` then means the
    /// card arrived, not the file's bytes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub link_only: bool,
}

/// Position of one message of reply text that was split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPart {
    /// 1-based.
    pub index: usize,
    pub total: usize,
}

impl DeliveryItem {
    #[must_use]
    pub fn text() -> Self {
        Self {
            kind: DeliveryItemKind::Text,
            path: None,
            status: DeliveryStatus::Pending,
            message_id: None,
            error: None,
            part: None,
            link_only: false,
        }
    }

    /// Part `index` (1-based) of reply text sent as `total` messages.
    #[must_use]
    pub fn text_part(index: usize, total: usize) -> Self {
        Self {
            part: Some(TextPart { index, total }),
            ..Self::text()
        }
    }

    #[must_use]
    pub fn file(path: impl Into<String>) -> Self {
        Self {
            kind: DeliveryItemKind::File,
            path: Some(path.into()),
            status: DeliveryStatus::Pending,
            message_id: None,
            error: None,
            part: None,
            link_only: false,
        }
    }

    pub fn mark_sent(&mut self, message_id: Option<i64>) {
        self.status = DeliveryStatus::Sent;
        self.message_id = message_id;
        self.error = None;
    }

    pub fn mark_failed(&mut self, error: &str) {
        self.status = DeliveryStatus::Failed;
        self.error = Some(truncate(error));
    }

    pub fn mark_unknown(&mut self, error: &str) {
        self.status = DeliveryStatus::Unknown;
        self.error = Some(truncate(error));
    }

    fn is_problem(&self) -> bool {
        self.status != DeliveryStatus::Sent
    }

    fn label(&self, full_path: bool) -> String {
        match self.kind {
            DeliveryItemKind::Text => match self.part {
                Some(TextPart { index, total }) => format!("reply text part {index} of {total}"),
                None => "reply text".to_string(),
            },
            DeliveryItemKind::File => {
                let path = self.path.as_deref().unwrap_or("(unknown file)");
                let shown = if full_path {
                    path
                } else {
                    std::path::Path::new(path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(path)
                };
                if self.link_only {
                    format!("file card for {shown}")
                } else {
                    format!("file {shown}")
                }
            }
        }
    }

    fn outcome(&self) -> &'static str {
        match self.status {
            DeliveryStatus::Sent => "delivered",
            DeliveryStatus::Failed => "not delivered",
            DeliveryStatus::Unknown => "outcome unknown, it may or may not have arrived",
            DeliveryStatus::Pending => {
                "not confirmed, delivery is still running or was interrupted"
            }
        }
    }

    fn line(&self, full_path: bool) -> String {
        let mut line = format!("{}: {}", self.label(full_path), self.outcome());
        if let Some(error) = self.error.as_deref() {
            line.push_str(" (");
            line.push_str(error);
            line.push(')');
        }
        line
    }
}

/// Delivery of one final reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRecord {
    /// Stable id: the pending record and its outcome share it, and the later
    /// event supersedes the earlier one.
    pub id: String,
    /// Delivering surface, e.g. `"telegram"`.
    pub surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<i64>,
    pub items: Vec<DeliveryItem>,
}

impl DeliveryRecord {
    #[must_use]
    pub fn new(
        surface: impl Into<String>,
        chat_id: Option<i64>,
        topic_id: Option<i64>,
        items: Vec<DeliveryItem>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            surface: surface.into(),
            chat_id,
            topic_id,
            items,
        }
    }

    /// Whether any item is not confirmed as delivered.
    #[must_use]
    pub fn has_problems(&self) -> bool {
        self.items.iter().any(DeliveryItem::is_problem)
    }

    /// Whether any item is still pending.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.status == DeliveryStatus::Pending)
    }

    /// This record (same id) with every pending item `unknown`: the
    /// delivering client went away mid-delivery, so nothing is resent.
    #[must_use]
    pub fn interrupted(&self) -> Self {
        let mut record = self.clone();
        for item in &mut record.items {
            if item.status == DeliveryStatus::Pending {
                item.mark_unknown(INTERRUPTED);
            }
        }
        record
    }
}

/// The error of an item whose delivery was cut off.
pub const INTERRUPTED: &str = "delivery interrupted; outcome unknown";

/// Folds records by id (a later record with the same id replaces the earlier
/// one) and keeps first-seen order.
pub fn fold_latest(records: &mut Vec<DeliveryRecord>, record: DeliveryRecord) {
    if let Some(existing) = records.iter_mut().find(|r| r.id == record.id) {
        *existing = record;
    } else {
        records.push(record);
    }
}

fn problem_lines<'a>(
    records: impl IntoIterator<Item = &'a DeliveryRecord>,
    full_path: bool,
) -> Vec<String> {
    records
        .into_iter()
        .flat_map(|record| record.items.iter())
        .filter(|item| item.is_problem())
        .map(|item| item.line(full_path))
        .collect()
}

/// Model-facing note for records that did not fully deliver. Replay prepends
/// it to the next user message; `None` when everything was delivered.
#[must_use]
pub fn model_note<'a>(records: impl IntoIterator<Item = &'a DeliveryRecord>) -> Option<String> {
    let lines = problem_lines(records, true);
    if lines.is_empty() {
        return None;
    }
    let mut note = String::from(
        "<delivery_status>\nRuntime record, not written by the user: part of your previous reply was not confirmed as delivered to the chat.\n",
    );
    for line in lines {
        note.push_str("- ");
        note.push_str(&line);
        note.push('\n');
    }
    note.push_str(
        "Do not say these items were sent or attached. Send them again only if the user still needs them.\n</delivery_status>",
    );
    Some(note)
}

/// Plain-text summary for the user, posted once after a partial delivery.
#[must_use]
pub fn user_summary(record: &DeliveryRecord) -> Option<String> {
    let lines = problem_lines([record], false);
    if lines.is_empty() {
        return None;
    }
    let mut text = String::from("⚠️ Part of this reply was not delivered:");
    for line in lines {
        text.push_str("\n• ");
        text.push_str(&line);
    }
    Some(text)
}

/// One-line notice for transcripts and the Mini App.
#[must_use]
pub fn notice_text(record: &DeliveryRecord) -> Option<String> {
    let lines = problem_lines([record], false);
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "{} delivery: {}",
        surface_label(&record.surface),
        lines.join("; ")
    ))
}

fn surface_label(surface: &str) -> String {
    let mut chars = surface.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect::<String>()
    })
}

fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_ERROR_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_ERROR_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(items: Vec<DeliveryItem>) -> DeliveryRecord {
        DeliveryRecord::new("telegram", Some(-100), Some(7), items)
    }

    #[test]
    fn fully_delivered_records_produce_no_note() {
        let mut text = DeliveryItem::text();
        text.mark_sent(Some(1));
        let mut file = DeliveryItem::file("/tmp/report.html");
        file.mark_sent(Some(2));
        let record = record(vec![text, file]);

        assert!(!record.has_problems());
        assert_eq!(model_note([&record]), None);
        assert_eq!(user_summary(&record), None);
        assert_eq!(notice_text(&record), None);
    }

    #[test]
    fn partial_delivery_names_each_problem() {
        let mut text = DeliveryItem::text();
        text.mark_sent(Some(1));
        let mut file = DeliveryItem::file("/tmp/out/report.html");
        file.mark_failed("Telegram API error for sendDocument (429): Too Many Requests");
        let mut other = DeliveryItem::file("/tmp/out/data.csv");
        other.mark_unknown("request timed out");
        let record = record(vec![text, file, other]);

        let note = model_note([&record]).expect("note");
        assert!(note.contains("file /tmp/out/report.html: not delivered"));
        assert!(note.contains("file /tmp/out/data.csv: outcome unknown"));
        assert!(!note.contains("reply text"));

        let summary = user_summary(&record).expect("summary");
        assert!(summary.contains("• file report.html: not delivered"));
        assert!(!summary.contains("/tmp/out"));

        let notice = notice_text(&record).expect("notice");
        assert!(notice.starts_with("Telegram delivery: file report.html"));
    }

    #[test]
    fn a_missing_text_part_is_named_by_its_position() {
        let mut first = DeliveryItem::text_part(1, 2);
        first.mark_sent(Some(1));
        let mut second = DeliveryItem::text_part(2, 2);
        second.mark_failed("Bad Request: message is too long");
        let record = record(vec![first, second]);

        let summary = user_summary(&record).expect("summary");
        assert!(summary.contains("• reply text part 2 of 2: not delivered"));
        assert!(!summary.contains("part 1 of 2"));
        let json = serde_json::to_string(&record).expect("json");
        assert!(json.contains("\"part\":{\"index\":2,\"total\":2}"));
        let single = serde_json::to_string(&DeliveryItem::text()).expect("json");
        assert!(!single.contains("part"), "unchanged for one-message text");
    }

    /// A file card is recorded as link-only, and a missing card says so.
    /// Older records without the field read as uploads.
    #[test]
    fn link_only_items_are_marked_and_named() {
        let mut card = DeliveryItem::file("/tmp/out/report.html");
        card.link_only = true;
        card.mark_failed("Bad Request: chat not found");
        let record = record(vec![card]);
        let summary = user_summary(&record).expect("summary");
        assert!(
            summary.contains("• file card for report.html: not delivered"),
            "{summary}"
        );
        let json = serde_json::to_string(&record).expect("json");
        assert!(json.contains("\"link_only\":true"));
        let upload = serde_json::to_string(&DeliveryItem::file("/tmp/a.pdf")).expect("json");
        assert!(!upload.contains("link_only"));
        let old: DeliveryItem =
            serde_json::from_str(r#"{"kind":"file","path":"/tmp/a.html","status":"sent"}"#)
                .expect("old record");
        assert!(!old.link_only);
    }

    #[test]
    fn pending_items_read_as_interrupted() {
        let record = record(vec![DeliveryItem::text()]);
        let note = model_note([&record]).expect("note");
        assert!(
            note.contains(
                "reply text: not confirmed, delivery is still running or was interrupted"
            )
        );
    }

    #[test]
    fn later_record_with_same_id_supersedes() {
        let pending = record(vec![DeliveryItem::text()]);
        let mut done = pending.clone();
        done.items[0].mark_sent(Some(9));

        let mut records = Vec::new();
        fold_latest(&mut records, pending);
        fold_latest(&mut records, done);
        assert_eq!(records.len(), 1);
        assert!(!records[0].has_problems());
    }

    #[test]
    fn long_errors_are_truncated() {
        let mut item = DeliveryItem::file("/tmp/a");
        item.mark_failed(&"x".repeat(1000));
        assert_eq!(item.error.as_deref().map(|e| e.chars().count()), Some(300));
    }
}
