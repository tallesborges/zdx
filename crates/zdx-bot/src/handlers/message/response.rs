use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use zdx_engine::core::artifacts;
use zdx_engine::core::delivery::{self, DeliveryItem, DeliveryRecord, DeliveryStatus};
use zdx_engine::core::thread_persistence::{self, Thread, ThreadEvent};
use zdx_engine::telegram::flood::{self, FailureClass};

use super::ReplyContext;
use super::media::{
    is_audio_path, is_image_path, is_valid_media_path, is_voice_note_path, parse_final_response,
    send_media_file,
};
use crate::bot::context::BotContext;
use crate::file_card;
use crate::telegram::markdown::{split_telegram_html, visible_utf16_len};
use crate::telegram::{
    InlineKeyboardButton, InlineKeyboardMarkup, MediaSend, Message, ReplyParameters,
    TelegramClient, TextSend, is_invalid_reply_error,
};

/// Telegram's caption limit, in characters after entity parsing.
const MAX_CAPTION_LEN: usize = 1024;
/// Telegram's `sendMessage` text limit. Longer reply text is split into
/// several messages (`markdown::split_telegram_html`).
const MAX_MESSAGE_LEN: usize = 4096;

/// Delivers a turn's final reply and records what reached the chat.
///
/// The reply is always a fresh message, so its Telegram timestamp marks when
/// the turn ended and completion raises a notification (edits do not). Reply
/// text and files go through the reliable client (paced, explicit flood
/// refusals retried within policy); every outcome is persisted as a
/// `ThreadEvent::Delivery` before and after the sends, and a partial delivery
/// posts one summary.
#[allow(clippy::too_many_arguments)]
pub(super) async fn send_final_response(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    status_message_id: Option<i64>,
    final_text: &str,
    thread_id: &str,
    touched_workers: &[String],
    thread: &mut Thread,
) -> Result<()> {
    let parsed = parse_final_response(final_text);
    let has_text = !parsed.text.trim().is_empty();
    let text_parts = if has_text {
        let text = with_worker_links(context, incoming.chat_id, touched_workers, &parsed.text);
        let text = with_thread_link(context, incoming.chat_id, thread_id, &text);
        split_telegram_html(&text, MAX_MESSAGE_LEN)
    } else {
        Vec::new()
    };

    // Mark the delivery as running and write its pending record before any
    // await, so a Mini App client polling the thread keeps polling from the
    // first moment until the outcome lands (see `delivery_tracker`).
    let delivery = (has_text || !parsed.media_paths.is_empty()).then(|| {
        let running = context.active_deliveries().start(thread_id);
        let mut items = text_items(text_parts.len());
        items.extend(
            parsed
                .media_paths
                .iter()
                .map(|path| DeliveryItem::file(path.display().to_string())),
        );
        let record = DeliveryRecord::new(
            "telegram",
            Some(incoming.chat_id),
            reply_ctx.topic_id,
            items,
        );
        append_delivery(thread, &record);
        (running, record)
    });

    // Paced, so a running flood wait delays the delete instead of leaving a
    // stale status message with a live Cancel button.
    if let Some(msg_id) = status_message_id
        && let Err(err) = context
            .client()
            .paced()
            .delete_message(incoming.chat_id, msg_id)
            .await
    {
        tracing::warn!(msg_id, err = %format!("{err:#}"), "Failed to delete status message");
    }

    let Some((running, mut record)) = delivery else {
        crate::followups::send_followups(
            context,
            incoming.chat_id,
            reply_ctx.topic_id,
            parsed.followups,
        )
        .await;
        return Ok(());
    };

    // Follow-ups ride on the reply text (or the captioned file); a reply
    // without text keeps the separate suggestions message.
    let keyboard = if has_text {
        crate::followups::keyboard(&parsed.followups)
    } else {
        None
    };
    let files = reply_files(context, incoming.chat_id, reply_ctx, &parsed.media_paths);
    let target = ReplyTarget::from_context(incoming.chat_id, reply_ctx);
    let keyboard_message = deliver_reply(
        &context.client().reliable(),
        &target,
        &text_parts,
        &files,
        keyboard.as_ref(),
        &mut record,
    )
    .await;
    append_delivery(thread, &record);
    log_delivery(thread_id, &record);
    // The outcome is persisted; clients may stop polling.
    drop(running);

    match keyboard_message {
        Some(message_id) => {
            crate::followups::register(context, incoming.chat_id, message_id, parsed.followups);
        }
        None if !has_text => {
            crate::followups::send_followups(
                context,
                incoming.chat_id,
                reply_ctx.topic_id,
                parsed.followups,
            )
            .await;
        }
        None => {}
    }

    send_delivery_summary(context, incoming.chat_id, reply_ctx.topic_id, &record).await;
    Ok(())
}

/// One delivery item per message of reply text: plain `text` when it fits one
/// message, numbered parts when it was split.
fn text_items(parts: usize) -> Vec<DeliveryItem> {
    match parts {
        0 => Vec::new(),
        1 => vec![DeliveryItem::text()],
        total => (1..=total)
            .map(|index| DeliveryItem::text_part(index, total))
            .collect(),
    }
}

/// Reply files with their `Open artifact` links. The links name the topic
/// that receives the reply (its session id, before any `alias_to` hop), not
/// the thread that made the file: the topic's artifact list includes every
/// file its replies sent, a worker's included, and feedback sent from the
/// preview is routed back to that topic. A file whose listed (lexical) path
/// is not the file sent gets no link rather than one to another file.
fn reply_files(
    context: &BotContext,
    chat_id: i64,
    reply_ctx: &ReplyContext,
    paths: &[PathBuf],
) -> Vec<ReplyFile> {
    let base = super::mini_app_base_url(context, chat_id);
    let topic_thread = super::thread_id_for_chat(chat_id, reply_ctx.topic_id);
    paths
        .iter()
        .map(|path| ReplyFile {
            path: path.clone(),
            open_url: base
                .as_deref()
                .filter(|_| artifacts::lexical_path_is_physical(path))
                .and_then(|base| artifact_link(base, &topic_thread, path)),
        })
        .collect()
}

/// Posts one summary after a partial delivery, through the same reliable
/// path. Its own failure is only logged, so a flood cannot cascade into more
/// notices.
async fn send_delivery_summary(
    context: &BotContext,
    chat_id: i64,
    topic_id: Option<i64>,
    record: &DeliveryRecord,
) {
    let Some(summary) = delivery::user_summary(record) else {
        return;
    };
    if let Err(err) = context
        .client()
        .reliable()
        .send_message(chat_id, &super::escape_html(&summary), None, topic_id)
        .await
    {
        tracing::warn!(chat_id, err = %format!("{err:#}"), "Failed to send delivery summary");
    }
}

fn append_delivery(thread: &mut Thread, record: &DeliveryRecord) {
    if let Err(err) = thread.append(&ThreadEvent::delivery(record.clone())) {
        tracing::warn!(err = %format!("{err:#}"), "Failed to record delivery outcome");
    }
}

/// One line per finished delivery: the record id, the thread, and each item's
/// outcome and message id, so the per-call `Telegram call` lines (which carry
/// the same message ids, chat, and topic) can be tied to this record. File
/// names only; no paths or bodies.
fn log_delivery(thread_id: &str, record: &DeliveryRecord) {
    let items = record
        .items
        .iter()
        .map(|item| {
            let name = item
                .path
                .as_deref()
                .and_then(|path| std::path::Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("text");
            let status = match item.status {
                DeliveryStatus::Pending => "pending",
                DeliveryStatus::Sent => "sent",
                DeliveryStatus::Failed => "failed",
                DeliveryStatus::Unknown => "unknown",
            };
            match item.message_id {
                Some(id) => format!("{name}={status}#{id}"),
                None => format!("{name}={status}"),
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let problems = record.has_problems();
    if problems {
        tracing::warn!(record = %record.id, thread = thread_id, chat = record.chat_id, topic = record.topic_id, items, "Telegram delivery incomplete");
    } else {
        tracing::info!(record = %record.id, thread = thread_id, chat = record.chat_id, topic = record.topic_id, items, "Telegram delivery");
    }
}

/// Where a reply goes in the chat.
#[derive(Debug, Clone)]
pub(super) struct ReplyTarget {
    chat_id: i64,
    topic_id: Option<i64>,
    reply_to_message_id: Option<i64>,
    /// Set for replies quoting a message in another topic; replaces
    /// `reply_to_message_id`.
    cross_topic: Option<ReplyParameters>,
}

impl ReplyTarget {
    fn from_context(chat_id: i64, reply_ctx: &ReplyContext) -> Self {
        Self {
            chat_id,
            topic_id: reply_ctx.topic_id,
            reply_to_message_id: reply_ctx.reply_to_message_id,
            cross_topic: reply_ctx.cross_topic_reply_parameters.clone(),
        }
    }

    fn media_options<'a>(
        &self,
        caption: Option<&'a str>,
        reply_markup: Option<&'a InlineKeyboardMarkup>,
    ) -> MediaSend<'a> {
        MediaSend {
            caption,
            reply_markup,
            reply_to_message_id: if self.cross_topic.is_some() {
                None
            } else {
                self.reply_to_message_id
            },
            message_thread_id: self.topic_id,
            reply_parameters: self.cross_topic.clone(),
        }
    }
}

/// One file of a reply, with the Mini App link that opens this exact file
/// (`None` when the Mini App is not configured for the chat, or no link fits).
#[derive(Debug, Clone)]
pub(super) struct ReplyFile {
    pub(super) path: PathBuf,
    pub(super) open_url: Option<String>,
}

/// Sends the reply text and files, filling in each item's status in
/// `record` (items: one per text part first, then one per file, in order).
/// Returns the id of the message that carries `followups`, if it was sent.
///
/// `text` is the reply text already split into messages that fit
/// (`split_telegram_html`); empty when there is none. The parts go out in
/// order and the follow-ups ride on the last one, so they appear once, after
/// the whole explanation. A refused or unknown part does not stop the parts
/// and files after it (each keeps its own outcome).
///
/// Follow-ups (what the agent can do next) ride on the reply text; every file
/// is its own message, carrying its own `Open artifact` button (file
/// actions). An HTML file with that link is not uploaded: it goes out as a
/// file card (`file_card`), a text message with `Open artifact` and
/// `Send file`. Every delivery posts new messages; earlier cards and files
/// are never edited or deleted. Documents are never merged into one message
/// and never get the
/// reply as a caption: a single photo, voice note, or audio file whose
/// complete caption fits Telegram's limit is sent with the text as its caption
/// (one message instead of two), and that message carries both the file's
/// button and the follow-ups. Buttons always go out with the message they
/// belong to; no request is spent only to add them. After a flood refusal that
/// the retry policy could not absorb, the remaining items are not attempted.
/// An ambiguous failure is recorded as unknown and never resent.
pub(super) async fn deliver_reply(
    client: &TelegramClient,
    target: &ReplyTarget,
    text: &[String],
    files: &[ReplyFile],
    followups: Option<&InlineKeyboardMarkup>,
    record: &mut DeliveryRecord,
) -> Option<i64> {
    let file_offset = text.len();
    for (idx, file) in files.iter().enumerate() {
        if !is_valid_media_path(&file.path) {
            record.items[file_offset + idx].mark_failed("file not found or not a regular file");
        }
    }

    if let [text] = text
        && let [file] = files
        && record.items[1].status == DeliveryStatus::Pending
        && takes_reply_caption(&file.path)
        && visible_utf16_len(text) <= MAX_CAPTION_LEN
    {
        let keyboard = captioned_keyboard(file.open_url.as_deref(), followups);
        let options = target.media_options(Some(text), keyboard.as_ref());
        match send_media_file(client, target.chat_id, &file.path, &options).await {
            Ok(message) => {
                record.items[0].mark_sent(Some(message.id));
                record.items[1].mark_sent(Some(message.id));
                return Some(message.id);
            }
            Err(err) => {
                let class = flood::classify(&err);
                let error = format!("{err:#}");
                match class {
                    FailureClass::Unknown => {
                        record.items[0].mark_unknown(&error);
                        record.items[1].mark_unknown(&error);
                        return None;
                    }
                    FailureClass::FloodWait(_) => {
                        record.items[0].mark_failed(&error);
                        record.items[1].mark_failed(&error);
                        return None;
                    }
                    // The file itself was refused; the text can still go out.
                    FailureClass::Rejected | FailureClass::NotSent => {
                        record.items[1].mark_failed(&error);
                        let sent = send_reply_text(client, target, text, followups).await;
                        return settle(&mut record.items[0], sent).ok().flatten();
                    }
                }
            }
        }
    }

    let mut keyboard_message = None;
    let mut flood_stopped: Option<String> = None;
    for (idx, part) in text.iter().enumerate() {
        let item = &mut record.items[idx];
        if let Some(reason) = &flood_stopped {
            item.mark_failed(&format!("not attempted after a flood refusal: {reason}"));
            continue;
        }
        let last = idx + 1 == text.len();
        let keyboard = if last { followups } else { None };
        let sent = send_reply_text(client, target, part, keyboard).await;
        match settle(item, sent) {
            Ok(id) if last => keyboard_message = id,
            Err(FailureClass::FloodWait(_)) => flood_stopped = item.error.clone(),
            Ok(_) | Err(_) => {}
        }
    }
    for (idx, file) in files.iter().enumerate() {
        let item = &mut record.items[file_offset + idx];
        if item.status != DeliveryStatus::Pending {
            continue;
        }
        if let Some(reason) = &flood_stopped {
            item.mark_failed(&format!("not attempted after a flood refusal: {reason}"));
            continue;
        }
        let sent = match file.open_url.as_deref() {
            Some(url) if file_card::is_card_file(&file.path, Some(url)) => {
                item.link_only = true;
                let card = file_card::card_keyboard(url, &file.path);
                send_reply_text(
                    client,
                    target,
                    &file_card::card_text(&file.path),
                    Some(&card),
                )
                .await
            }
            url => {
                let keyboard = url.map(open_artifact_keyboard);
                let options = target.media_options(None, keyboard.as_ref());
                send_media_file(client, target.chat_id, &file.path, &options)
                    .await
                    .map(|message| Some(message.id))
            }
        };
        if let Err(FailureClass::FloodWait(_)) = settle(item, sent) {
            flood_stopped = item.error.clone();
        }
    }
    keyboard_message
}

/// Whether a lone file may carry the reply text as its caption: photos, voice
/// notes, and audio. A document always goes out on its own, after the text.
fn takes_reply_caption(path: &Path) -> bool {
    is_image_path(path) || is_voice_note_path(path) || is_audio_path(path)
}

const OPEN_ARTIFACT_LABEL: &str = file_card::OPEN_ARTIFACT_LABEL;

/// The file action on one file message: open this exact file in the Mini App.
fn open_artifact_keyboard(url: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton::url(OPEN_ARTIFACT_LABEL, url)]],
    }
}

/// Keyboard of a captioned file: its file action first, then the follow-ups.
/// Follow-up taps and Dismiss keep the file action (see `followups`).
fn captioned_keyboard(
    open_url: Option<&str>,
    followups: Option<&InlineKeyboardMarkup>,
) -> Option<InlineKeyboardMarkup> {
    let mut rows =
        open_url.map_or_else(Vec::new, |url| open_artifact_keyboard(url).inline_keyboard);
    if let Some(followups) = followups {
        rows.extend(followups.inline_keyboard.iter().cloned());
    }
    (!rows.is_empty()).then_some(InlineKeyboardMarkup {
        inline_keyboard: rows,
    })
}

/// Longest `startapp` value used: a conservative local limit, not an
/// established Mini App limit. Telegram documents 64 characters only for bot
/// `start` parameters and states no length for `startapp`; staying within 64
/// keeps a margin. Characters are limited to `A-Z`, `a-z`, `0-9`, `_`, `-`
/// (Telegram's documented start-parameter alphabet).
const MAX_START_PARAM_LEN: usize = 64;

/// Mini App deep link that opens one artifact: `startapp=a_<key>_<thread>`,
/// where `key` is the engine's [`artifacts::artifact_key`] (fixed-length hex)
/// and `thread` is the id of the topic that receives the reply, so feedback
/// sent from the preview returns to this topic. The key does not embed the raw
/// path; it is an opaque lookup identifier, not an authorization: the Mini App
/// resolves it in that thread's artifact list with its own authenticated
/// session. `None` when the value would not fit the limits above.
pub(crate) fn artifact_link(base: &str, thread_id: &str, path: &Path) -> Option<String> {
    let param = format!("a_{}_{thread_id}", artifacts::artifact_key(path));
    let fits = param.len() <= MAX_START_PARAM_LEN
        && param
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    fits.then(|| format!("{base}?startapp={param}"))
}

/// Records a send result on `item`. Returns the message id on success, or the
/// failure class.
fn settle(item: &mut DeliveryItem, sent: Result<Option<i64>>) -> Result<Option<i64>, FailureClass> {
    match sent {
        Ok(id) => {
            item.mark_sent(id);
            Ok(id)
        }
        Err(err) => {
            let class = flood::classify(&err);
            let error = format!("{err:#}");
            if class.is_definitely_undelivered() {
                item.mark_failed(&error);
            } else {
                item.mark_unknown(&error);
            }
            Err(class)
        }
    }
}

/// Sends the reply text. A reply target that no longer exists is dropped and
/// the text resent once, which is safe because the first request was refused.
async fn send_reply_text(
    client: &TelegramClient,
    target: &ReplyTarget,
    text: &str,
    keyboard: Option<&InlineKeyboardMarkup>,
) -> Result<Option<i64>> {
    tracing::info!(chat_id = target.chat_id, "Sending reply");
    let mut request = TextSend {
        chat_id: target.chat_id,
        text,
        reply_to_message_id: None,
        message_thread_id: target.topic_id,
        reply_parameters: target.cross_topic.clone(),
        reply_markup: keyboard,
        disable_preview: true,
    };
    if target.cross_topic.is_some() {
        return client.send_text(&request).await.map(|m| Some(m.id));
    }
    request.reply_to_message_id = target.reply_to_message_id;
    let result: Result<Message> = match client.send_text(&request).await {
        Err(err) if request.reply_to_message_id.is_some() && is_invalid_reply_error(&err) => {
            request.reply_to_message_id = None;
            client.send_text(&request).await
        }
        other => other,
    };
    result.map(|m| Some(m.id))
}

/// Appends a subtle Mini App deep link for this thread to an answer.
///
/// Saves the trip through the pinned thread header just to reach the Mini App.
/// The link is skipped when the Mini App is not configured for this chat, and
/// the answer is sent with link previews disabled so it stays a plain line of
/// text instead of a preview card.
fn with_thread_link(context: &BotContext, chat_id: i64, thread_id: &str, text: &str) -> String {
    append_thread_link(
        super::mini_app_base_url(context, chat_id).as_deref(),
        thread_id,
        text,
    )
}

/// Pure half of [`with_thread_link`], so the rendered suffix is testable
/// without constructing a whole bot context.
pub(crate) fn append_thread_link(base: Option<&str>, thread_id: &str, text: &str) -> String {
    match base {
        Some(base) => {
            format!("{text}\n\n<a href=\"{base}?startapp={thread_id}\">↗ Open thread</a>")
        }
        None => text.to_string(),
    }
}

/// Where a worker link points: its legacy mirror topic when it still has one,
/// otherwise the worker thread in the Mini App. New workers are silent and
/// never get a topic, so for them this is always the Mini App deep link.
/// `None` only when neither exists.
pub(crate) fn worker_link(
    mirror_url: Option<String>,
    mini_app_base: Option<&str>,
    worker_thread_id: &str,
) -> Option<String> {
    mirror_url.or_else(|| mini_app_base.map(|base| format!("{base}?startapp={worker_thread_id}")))
}

/// Appends one `🛠 <title>` link per worker this turn created or messaged, so
/// the user can follow the work without relying on the model to mention it.
/// See [`worker_link`] for where each link points.
fn with_worker_links(
    context: &BotContext,
    chat_id: i64,
    touched_workers: &[String],
    text: &str,
) -> String {
    let mini_app = super::mini_app_base_url(context, chat_id);
    let links: Vec<(String, String)> = touched_workers
        .iter()
        .filter_map(|worker| {
            let url = worker_link(
                context.worker_manager().mirror_url(worker),
                mini_app.as_deref(),
                worker,
            )?;
            let title = thread_persistence::read_thread_title(worker)
                .ok()
                .flatten()
                .unwrap_or_else(|| worker.chars().take(8).collect());
            Some((title, url))
        })
        .collect();
    append_worker_links(&links, text)
}

fn append_worker_links(links: &[(String, String)], text: &str) -> String {
    if links.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    out.push('\n');
    for (title, url) in links {
        let _ = write!(
            out,
            "\n🛠 <a href=\"{url}\">{}</a>",
            super::escape_html(title)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use zdx_engine::core::delivery::{DeliveryItem, DeliveryRecord, DeliveryStatus};

    use super::{
        ReplyFile, ReplyTarget, append_thread_link, append_worker_links, artifact_link,
        deliver_reply,
    };
    use crate::telegram::fake::{FakeTelegram, Reply, TOKEN};
    use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};

    const CHAT: i64 = -1001;

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(ext: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("zdx-test-{}.{ext}", uuid::Uuid::new_v4()));
            std::fs::write(&path, b"synthetic file").expect("write temp file");
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn target() -> ReplyTarget {
        ReplyTarget {
            chat_id: CHAT,
            topic_id: Some(7),
            reply_to_message_id: Some(42),
            cross_topic: None,
        }
    }

    fn keyboard() -> InlineKeyboardMarkup {
        InlineKeyboardMarkup {
            inline_keyboard: vec![vec![InlineKeyboardButton::callback("Next step", "fu:0")]],
        }
    }

    fn record(text_parts: usize, files: &[PathBuf]) -> DeliveryRecord {
        let mut items = super::text_items(text_parts);
        items.extend(
            files
                .iter()
                .map(|p| DeliveryItem::file(p.display().to_string())),
        );
        DeliveryRecord::new("telegram", Some(CHAT), Some(7), items)
    }

    const MINI_APP: &str = "https://t.me/zdx_test_bot/app";
    const TOPIC_THREAD: &str = "telegram--1001-topic-7";

    fn linked(files: &[PathBuf]) -> Vec<ReplyFile> {
        files
            .iter()
            .map(|path| ReplyFile {
                path: path.clone(),
                open_url: artifact_link(MINI_APP, TOPIC_THREAD, path),
            })
            .collect()
    }

    async fn deliver(
        fake: &FakeTelegram,
        text: Option<&str>,
        files: &[PathBuf],
    ) -> (Option<i64>, DeliveryRecord) {
        deliver_files(fake, text, &linked(files)).await
    }

    async fn deliver_files(
        fake: &FakeTelegram,
        text: Option<&str>,
        files: &[ReplyFile],
    ) -> (Option<i64>, DeliveryRecord) {
        let client =
            TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone()).reliable();
        let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
        let parts = text.map_or_else(Vec::new, |text| {
            crate::telegram::markdown::split_telegram_html(text, super::MAX_MESSAGE_LEN)
        });
        let mut record = record(parts.len(), &paths);
        let keyboard = keyboard();
        let id = deliver_reply(
            &client,
            &target(),
            &parts,
            files,
            Some(&keyboard),
            &mut record,
        )
        .await;
        (id, record)
    }

    /// The `Open artifact` URL a file message carries, if any.
    fn open_url(body: &str) -> Option<String> {
        let field = "name=\"reply_markup\"\r\n\r\n";
        let start = body.find(field)? + field.len();
        let end = body[start..]
            .find("\r\n")
            .map_or(body.len(), |at| start + at);
        let markup: serde_json::Value = serde_json::from_str(&body[start..end]).ok()?;
        markup["inline_keyboard"]
            .as_array()?
            .iter()
            .flat_map(|row| row.as_array().into_iter().flatten())
            .find(|button| button["text"] == super::OPEN_ARTIFACT_LABEL)
            .and_then(|button| button["url"].as_str().map(str::to_string))
    }

    fn statuses(record: &DeliveryRecord) -> Vec<DeliveryStatus> {
        record.items.iter().map(|item| item.status).collect()
    }

    /// The pending record and the running mark exist before the first await
    /// (the paced status delete), so a polling client never misses the
    /// delivery; both clear only once the outcome is persisted.
    #[tokio::test]
    async fn delivery_is_recorded_and_running_before_the_status_delete_waits() {
        use zdx_engine::core::thread_persistence::{Thread, ThreadEvent, load_thread_events};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        // A flood refusal puts the chat in a 1 s wait, so the paced status
        // delete has to wait.
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 1",
                Some(1),
            )],
        );
        let client = TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone());
        assert!(client.send_message(CHAT, "x", None, Some(7)).await.is_err());
        let root = std::env::temp_dir();
        let context = std::sync::Arc::new(crate::bot::context::BotContext::for_test(
            client,
            zdx_engine::config::Config::default(),
            root,
        ));

        let thread_id = format!("telegram-delivery-order-{}", uuid::Uuid::new_v4());
        let mut thread = Thread::with_id(thread_id.clone()).expect("thread");
        thread
            .append(&ThreadEvent::user_message("go"))
            .expect("append");
        let incoming = crate::types::IncomingMessage {
            chat_id: CHAT,
            message_id: 42,
            user_id: 1,
            text: Some("go".to_string()),
            images: Vec::new(),
            audios: Vec::new(),
            documents: Vec::new(),
            videos: Vec::new(),
            notices: Vec::new(),
            message_thread_id: Some(7),
            is_forum: true,
            replied_to: None,
        };
        let reply_ctx = super::ReplyContext {
            reply_to_message_id: Some(42),
            topic_id: Some(7),
            cross_topic_reply_parameters: None,
        };

        let task_context = std::sync::Arc::clone(&context);
        let task_thread_id = thread_id.clone();
        let task = tokio::spawn(async move {
            super::send_final_response(
                &task_context,
                &incoming,
                &reply_ctx,
                Some(55),
                "Ready",
                &task_thread_id,
                &[],
                &mut thread,
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(context.active_deliveries().is_active(&thread_id));
        let pending = load_thread_events(&thread_id).expect("events");
        assert!(matches!(pending.last(), Some(ThreadEvent::Delivery { .. })));
        assert_eq!(fake.methods(), ["sendMessage"], "delete still waiting");

        task.await.expect("join").expect("send final response");
        assert!(!context.active_deliveries().is_active(&thread_id));
        assert_eq!(
            fake.methods(),
            ["sendMessage", "deleteMessage", "sendMessage"]
        );
        let events = load_thread_events(&thread_id).expect("events");
        let Some(ThreadEvent::Delivery { record, .. }) = events.last() else {
            panic!("outcome recorded last");
        };
        assert_eq!(record.items[0].status, DeliveryStatus::Sent);
    }

    /// One document: the explanation and follow-ups first, then the file on
    /// its own with its own `Open artifact` button. The document never takes
    /// the reply as its caption, and no request is spent only on buttons.
    #[tokio::test]
    async fn one_document_follows_the_text_with_its_own_file_action() {
        let fake = FakeTelegram::start().await;
        let file = TempFile::new("pdf");
        let (id, record) =
            deliver(&fake, Some("<b>Ready</b>"), std::slice::from_ref(&file.0)).await;

        assert_eq!(fake.methods(), ["sendMessage", "sendDocument"]);
        let seen = fake.seen();
        assert!(seen[0].body.contains("fu:0"), "follow-ups on the text");
        assert!(!seen[0].body.contains("Open artifact"));
        assert!(!seen[1].body.contains("fu:0"), "no follow-ups on the file");
        assert!(!seen[1].body.contains("name=\"caption\""));
        assert_eq!(
            open_url(&seen[1].body),
            artifact_link(MINI_APP, TOPIC_THREAD, &file.0)
        );
        assert!(
            seen[1]
                .body
                .contains("name=\"reply_to_message_id\"\r\n\r\n42")
        );
        assert!(seen[1].body.contains("name=\"message_thread_id\"\r\n\r\n7"));
        assert_eq!(
            statuses(&record),
            [DeliveryStatus::Sent, DeliveryStatus::Sent]
        );
        assert_eq!(id, record.items[0].message_id);
        assert_ne!(record.items[0].message_id, record.items[1].message_id);
    }

    /// An HTML file with a link is a file card, whatever its size: one text
    /// message with `Open artifact` and `Send file`, no upload. A second
    /// delivery of the same file posts a second card; the first is never
    /// edited, deleted, or marked.
    #[tokio::test]
    async fn html_goes_out_as_a_new_file_card_every_time() {
        let fake = FakeTelegram::start().await;
        let file = TempFile::new("html");
        std::fs::File::options()
            .write(true)
            .open(&file.0)
            .and_then(|handle| handle.set_len(50 * 1024 * 1024 + 1))
            .expect("grow file");
        let (_, first) = deliver(&fake, Some("v1"), std::slice::from_ref(&file.0)).await;
        let (_, second) = deliver(&fake, Some("v2"), std::slice::from_ref(&file.0)).await;

        assert_eq!(fake.methods(), ["sendMessage"; 4], "no upload, no edit");
        let seen = fake.seen();
        for card in [&seen[1], &seen[3]] {
            let body: serde_json::Value = serde_json::from_str(&card.body).expect("json");
            let text = body["text"].as_str().expect("text");
            assert!(
                text.contains("not uploaded") && text.contains("as it is now"),
                "{text}"
            );
            let row = &body["reply_markup"]["inline_keyboard"][0];
            assert_eq!(
                row[0]["url"].as_str().map(str::to_string),
                artifact_link(MINI_APP, TOPIC_THREAD, &file.0)
            );
            assert_eq!(
                row[1]["callback_data"].as_str(),
                Some(crate::file_card::callback_data(&file.0).as_str())
            );
            assert_eq!(body["reply_to_message_id"], 42);
            assert_eq!(body["message_thread_id"], 7);
            assert!(!card.body.contains("fu:0"), "follow-ups stay on the text");
        }
        for record in [&first, &second] {
            assert_eq!(statuses(record), [DeliveryStatus::Sent; 2]);
            assert!(record.items[1].link_only);
            assert!(!record.items[0].link_only);
        }
        assert_ne!(first.items[1].message_id, second.items[1].message_id);
    }

    /// Images, voice notes, audio, and other files keep uploading next to an
    /// HTML card.
    #[tokio::test]
    async fn only_html_becomes_a_card() {
        let fake = FakeTelegram::start().await;
        let files: Vec<TempFile> = ["png", "ogg", "mp3", "pdf", "zip", "csv", "html"]
            .into_iter()
            .map(TempFile::new)
            .collect();
        let paths: Vec<PathBuf> = files.iter().map(|file| file.0.clone()).collect();
        let (_, record) = deliver(&fake, Some("**done**"), &paths).await;

        assert_eq!(
            fake.methods(),
            [
                "sendMessage",
                "sendPhoto",
                "sendVoice",
                "sendAudio",
                "sendDocument",
                "sendDocument",
                "sendDocument",
                "sendMessage"
            ]
        );
        let link_only: Vec<bool> = record.items.iter().map(|item| item.link_only).collect();
        assert_eq!(
            link_only,
            [false, false, false, false, false, false, false, true]
        );
    }

    /// The reported defect: several documents must each open their own file.
    /// Each is a separate message whose button names that file, in order.
    #[tokio::test]
    async fn each_document_is_its_own_message_with_its_own_link() {
        let fake = FakeTelegram::start().await;
        let files: Vec<TempFile> = ["txt", "pdf", "md"]
            .into_iter()
            .map(|ext| {
                let file = TempFile::new(ext);
                std::fs::write(&file.0, format!("synthetic {ext} body")).expect("write");
                file
            })
            .collect();
        let paths: Vec<PathBuf> = files.iter().map(|file| file.0.clone()).collect();
        let (id, record) = deliver(&fake, Some("**done**"), &paths).await;

        assert_eq!(
            fake.methods(),
            [
                "sendMessage",
                "sendDocument",
                "sendDocument",
                "sendDocument"
            ]
        );
        let seen = fake.seen();
        let mut urls = Vec::new();
        for (index, (path, ext)) in paths.iter().zip(["txt", "pdf", "md"]).enumerate() {
            let body = &seen[index + 1].body;
            assert!(
                body.contains(&format!("synthetic {ext} body")),
                "file {index} uploaded alone"
            );
            let url = open_url(body).expect("own button");
            assert_eq!(
                Some(url.clone()),
                artifact_link(MINI_APP, TOPIC_THREAD, path)
            );
            urls.push(url);
        }
        urls.dedup();
        assert_eq!(urls.len(), 3, "every button opens a different file");
        assert_eq!(statuses(&record), [DeliveryStatus::Sent; 4]);
        assert_eq!(id, record.items[0].message_id);
        let mut ids: Vec<_> = record.items.iter().map(|item| item.message_id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 4, "one message per item");
    }

    #[tokio::test]
    async fn documents_without_text_carry_only_their_file_actions() {
        let fake = FakeTelegram::start().await;
        let a = TempFile::new("pdf");
        let b = TempFile::new("csv");
        let (id, record) = deliver(&fake, None, &[a.0.clone(), b.0.clone()]).await;

        assert_eq!(id, None, "no message carries follow-ups");
        assert_eq!(fake.methods(), ["sendDocument", "sendDocument"]);
        for seen in fake.seen() {
            assert!(open_url(&seen.body).is_some());
            assert!(!seen.body.contains("fu:0"));
        }
        assert_eq!(statuses(&record), [DeliveryStatus::Sent; 2]);
    }

    /// Without a configured Mini App the files still go out separately, with
    /// no button.
    #[tokio::test]
    async fn files_go_out_without_buttons_when_there_is_no_link() {
        let fake = FakeTelegram::start().await;
        let doc = TempFile::new("html");
        let photo = TempFile::new("png");
        let files = [ReplyFile {
            path: doc.0.clone(),
            open_url: None,
        }];
        let (_, record) = deliver_files(&fake, Some("done"), &files).await;
        assert_eq!(fake.methods(), ["sendMessage", "sendDocument"]);
        assert!(!fake.seen()[1].body.contains("reply_markup"));
        assert_eq!(statuses(&record), [DeliveryStatus::Sent; 2]);

        let fake = FakeTelegram::start().await;
        let files = [ReplyFile {
            path: photo.0.clone(),
            open_url: None,
        }];
        let (_, _) = deliver_files(&fake, Some("done"), &files).await;
        assert_eq!(fake.methods(), ["sendPhoto"]);
        let body = &fake.seen()[0].body;
        assert!(body.contains("fu:0") && open_url(body).is_none());
    }

    /// A partial failure is recorded per file: a refused document fails, the
    /// next one still goes out, and a flood refusal stops the rest.
    #[tokio::test]
    async fn document_failures_are_recorded_per_file() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendDocument",
            &[
                Reply::Error(400, "Bad Request: file is empty", None),
                Reply::Ok,
                Reply::Error(429, "Too Many Requests: retry after 500", Some(500)),
            ],
        );
        let files: Vec<TempFile> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|_| TempFile::new("pdf"))
            .collect();
        let paths: Vec<PathBuf> = files.iter().map(|file| file.0.clone()).collect();
        let (id, record) = deliver(&fake, Some("done"), &paths).await;

        assert_eq!(
            fake.methods(),
            [
                "sendMessage",
                "sendDocument",
                "sendDocument",
                "sendDocument"
            ]
        );
        assert!(id.is_some());
        assert_eq!(
            statuses(&record),
            [
                DeliveryStatus::Sent,
                DeliveryStatus::Failed,
                DeliveryStatus::Sent,
                DeliveryStatus::Failed,
                DeliveryStatus::Failed,
            ]
        );
        assert!(
            record.items[4]
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("not attempted after a flood refusal"))
        );
    }

    #[tokio::test]
    async fn an_ambiguous_document_send_is_unknown_and_not_resent() {
        let fake = FakeTelegram::start().await;
        fake.script("sendDocument", &[Reply::Garbage]);
        let a = TempFile::new("pdf");
        let b = TempFile::new("pdf");
        let (_, record) = deliver(&fake, Some("done"), &[a.0.clone(), b.0.clone()]).await;

        assert_eq!(
            fake.methods(),
            ["sendMessage", "sendDocument", "sendDocument"]
        );
        assert_eq!(
            statuses(&record),
            [
                DeliveryStatus::Sent,
                DeliveryStatus::Unknown,
                DeliveryStatus::Sent
            ]
        );
    }

    #[tokio::test]
    async fn photos_and_mixed_media_keep_their_own_methods() {
        let fake = FakeTelegram::start().await;
        let photo = TempFile::new("png");
        let doc = TempFile::new("pdf");
        let files = [photo.0.clone(), doc.0.clone()];
        let (_, record) = deliver(&fake, Some("done"), &files).await;
        assert_eq!(fake.methods(), ["sendMessage", "sendPhoto", "sendDocument"]);
        let seen = fake.seen();
        assert_eq!(
            open_url(&seen[1].body),
            artifact_link(MINI_APP, TOPIC_THREAD, &photo.0)
        );
        assert_eq!(
            open_url(&seen[2].body),
            artifact_link(MINI_APP, TOPIC_THREAD, &doc.0)
        );
        assert_eq!(statuses(&record), [DeliveryStatus::Sent; 3]);
    }

    #[tokio::test]
    async fn an_oversized_document_fails_and_the_text_still_goes_out() {
        let fake = FakeTelegram::start().await;
        let big = TempFile::new("pdf");
        std::fs::File::options()
            .write(true)
            .open(&big.0)
            .and_then(|file| file.set_len(50 * 1024 * 1024 + 1))
            .expect("grow file");
        let (_, record) = deliver(&fake, Some("done"), std::slice::from_ref(&big.0)).await;

        assert_eq!(fake.methods(), ["sendMessage"], "no document request");
        assert_eq!(
            statuses(&record),
            [DeliveryStatus::Sent, DeliveryStatus::Failed]
        );
    }

    /// The link names where the reply goes, before any alias hop: a forum
    /// topic, a supergroup's General (no topic), or a private chat, so the
    /// Mini App reads that chat's artifacts and sends feedback back to it.
    /// Images get the same link as documents.
    #[test]
    fn links_name_the_receiving_chat_or_topic() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let mut config = zdx_engine::config::Config::default();
        config.telegram.server = Some(zdx_engine::config::TelegramServerConfig {
            enabled: true,
            port: 4141,
            mini_app_url: Some(MINI_APP.to_string()),
        });
        let context = crate::bot::context::BotContext::for_test(
            TelegramClient::with_base_url(TOKEN.to_string(), "http://127.0.0.1:9".to_string()),
            config,
            std::env::temp_dir(),
        );
        let doc = TempFile::new("html");
        let image = TempFile::new("png");
        let paths = [doc.0.clone(), image.0.clone()];
        let ctx = |topic_id| super::ReplyContext {
            reply_to_message_id: None,
            topic_id,
            cross_topic_reply_parameters: None,
        };
        // A resumed topic: its session aliases another thread.
        zdx_engine::core::thread_persistence::Thread::with_id("telegram--1001-topic-9".to_string())
            .expect("thread")
            .set_alias(Some("resumed-source".to_string()))
            .expect("alias");
        for (chat, topic, thread) in [
            (-1001, Some(7), "telegram--1001-topic-7"),
            (-1001, Some(9), "telegram--1001-topic-9"),
            (-1001, None, "telegram--1001"),
            (424_242, None, "telegram-424242"),
        ] {
            let files = super::reply_files(&context, chat, &ctx(topic), &paths);
            for (file, path) in files.iter().zip(&paths) {
                assert_eq!(
                    file.open_url,
                    artifact_link(MINI_APP, thread, path),
                    "{thread}"
                );
            }
        }

        let unconfigured = crate::bot::context::BotContext::for_test(
            TelegramClient::with_base_url(TOKEN.to_string(), "http://127.0.0.1:9".to_string()),
            zdx_engine::config::Config::default(),
            std::env::temp_dir(),
        );
        let files = super::reply_files(&unconfigured, -1001, &ctx(Some(7)), &paths);
        assert!(files.iter().all(|file| file.open_url.is_none()));
    }

    /// The link names the receiving topic and the file's key, within
    /// Telegram's start-parameter rules, and never the path.
    #[test]
    fn artifact_links_are_short_opaque_and_valid_start_parameters() {
        let path = std::path::Path::new("/Users/someone/.zdx/artifacts/threads/w/report.html");
        let url =
            artifact_link(MINI_APP, "telegram--1001234567890-topic-1234567", path).expect("link");
        let param = url
            .strip_prefix(&format!("{MINI_APP}?startapp="))
            .expect("startapp link");
        assert!(param.len() <= 64, "{param}");
        assert!(
            param
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        );
        let key = zdx_engine::core::artifacts::artifact_key(path);
        assert_eq!(
            param,
            format!("a_{key}_telegram--1001234567890-topic-1234567")
        );
        assert!(!url.contains("report") && !url.contains("someone"));

        // Same name, different place: a different link.
        let other = std::path::Path::new("/Users/someone/.zdx/artifacts/threads/x/report.html");
        assert_ne!(
            artifact_link(MINI_APP, TOPIC_THREAD, path),
            artifact_link(MINI_APP, TOPIC_THREAD, other)
        );
        // Out of Telegram's rules: no link rather than a broken one.
        assert!(artifact_link(MINI_APP, &"9".repeat(46), path).is_none());
        assert!(artifact_link(MINI_APP, "thread with space", path).is_none());
    }

    fn sent_texts(fake: &FakeTelegram) -> Vec<String> {
        fake.seen()
            .iter()
            .filter(|seen| seen.method == "sendMessage")
            .map(|seen| {
                let body: serde_json::Value = serde_json::from_str(&seen.body).expect("json");
                body["text"].as_str().expect("text").to_string()
            })
            .collect()
    }

    /// Text without tags: what splitting must keep exactly.
    fn visible(html: &str) -> String {
        let mut out = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    }

    /// An explanation of about 5000 characters with markup and multi-byte
    /// text, as the model writes when it describes several documents.
    fn long_explanation() -> String {
        use std::fmt::Write as _;

        let mut text = String::from("<b>Reports</b> &amp; notes 😀\n\n");
        for n in 0..42 {
            let _ = writeln!(
                text,
                "<b>Section {n}</b>: the <i>comparison</i> of <code>a&lt;b</code> 中文 sizes, with a <a href=\"https://example.com/{n}?x=1&amp;y=2\">source</a>."
            );
        }
        let len = text.encode_utf16().count();
        assert!((5000..8000).contains(&len), "{len}");
        text
    }

    /// The old failure, reproduced: the whole explanation in one request is
    /// refused (the fake measures raw HTML, stricter than Telegram).
    #[tokio::test]
    async fn the_fake_refuses_text_over_the_limit_like_telegram() {
        let fake = FakeTelegram::start().await;
        let client = TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone());
        let text = long_explanation();
        let err = client
            .send_text(&crate::telegram::TextSend {
                chat_id: CHAT,
                text: &text,
                reply_to_message_id: None,
                message_thread_id: Some(7),
                reply_parameters: None,
                reply_markup: None,
                disable_preview: true,
            })
            .await
            .expect_err("too long");
        assert_eq!(
            crate::telegram::api_error_description(&err),
            Some("Bad Request: message is too long")
        );
    }

    #[tokio::test]
    async fn a_long_explanation_is_split_before_the_document() {
        let fake = FakeTelegram::start().await;
        let file = TempFile::new("pdf");
        let text = long_explanation();
        let (id, record) = deliver(&fake, Some(&text), std::slice::from_ref(&file.0)).await;

        assert_eq!(
            fake.methods(),
            ["sendMessage", "sendMessage", "sendDocument"]
        );
        let seen = fake.seen();
        assert!(!seen[0].body.contains("fu:0"), "follow-ups once");
        assert!(seen[1].body.contains("fu:0"), "on the last part");
        assert!(open_url(&seen[2].body).is_some());
        let texts = sent_texts(&fake);
        assert!(texts.iter().all(|t| t.encode_utf16().count() <= 4096));
        assert_eq!(visible(&texts.concat()), visible(&text), "no text lost");
        assert_eq!(statuses(&record), [DeliveryStatus::Sent; 3]);
        assert_eq!(id, record.items[1].message_id);
        assert_ne!(record.items[0].message_id, record.items[1].message_id);
        assert_eq!(
            record.items[1].part,
            Some(zdx_engine::core::delivery::TextPart { index: 2, total: 2 })
        );
    }

    /// The reported case in plain words: 5000 visible characters and a
    /// document. All of it arrives, text first.
    #[tokio::test]
    async fn a_5000_character_plain_explanation_and_a_document() {
        let fake = FakeTelegram::start().await;
        let file = TempFile::new("pdf");
        let text = "Plain words, nothing else. ".repeat(186);
        assert!(text.chars().count() > 5000);
        let (id, record) = deliver(&fake, Some(&text), std::slice::from_ref(&file.0)).await;

        assert_eq!(
            fake.methods(),
            ["sendMessage", "sendMessage", "sendDocument"]
        );
        assert_eq!(sent_texts(&fake).concat(), text, "exact text, in order");
        assert_eq!(statuses(&record), [DeliveryStatus::Sent; 3]);
        assert_eq!(id, record.items[1].message_id);
    }

    /// A refused middle part fails alone: the rest of the text and the file
    /// still go out, the follow-ups stay on the last part, and each part keeps
    /// its real outcome. A flood refusal stops everything after it.
    #[tokio::test]
    async fn a_failed_middle_part_is_recorded_and_the_rest_still_goes_out() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[
                Reply::Ok,
                Reply::Error(400, "Bad Request: chat not found", None),
            ],
        );
        let file = TempFile::new("pdf");
        let text = format!("{}\n{}", long_explanation(), long_explanation());
        let (id, record) = deliver(&fake, Some(&text), std::slice::from_ref(&file.0)).await;

        let methods = fake.methods();
        let parts = methods
            .iter()
            .filter(|m| m.as_str() == "sendMessage")
            .count();
        assert!(parts >= 3, "{methods:?}");
        assert_eq!(methods.last().map(String::as_str), Some("sendDocument"));
        let mut expected = vec![DeliveryStatus::Sent, DeliveryStatus::Failed];
        expected.extend(std::iter::repeat_n(DeliveryStatus::Sent, parts - 1));
        assert_eq!(statuses(&record), expected);
        assert_eq!(
            id,
            record.items[parts - 1].message_id,
            "follow-ups on the last part"
        );

        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[
                Reply::Ok,
                Reply::Error(429, "Too Many Requests: retry after 500", Some(500)),
            ],
        );
        let (id, record) = deliver(&fake, Some(&text), std::slice::from_ref(&file.0)).await;
        assert_eq!(fake.methods(), ["sendMessage", "sendMessage"]);
        assert_eq!(id, None);
        assert_eq!(record.items[0].status, DeliveryStatus::Sent);
        assert!(
            record.items[1..]
                .iter()
                .all(|item| item.status == DeliveryStatus::Failed)
        );
    }

    /// Through the whole reply path: the record has one item per part, and a
    /// partial delivery names the missing part in the summary.
    #[tokio::test]
    async fn a_partial_long_reply_names_the_missing_part() {
        use zdx_engine::core::thread_persistence::{Thread, ThreadEvent, load_thread_events};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[
                Reply::Ok,
                Reply::Error(400, "Bad Request: chat not found", None),
            ],
        );
        let context = crate::bot::context::BotContext::for_test(
            TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone()),
            zdx_engine::config::Config::default(),
            std::env::temp_dir(),
        );
        let file = TempFile::new("pdf");
        let thread_id = format!("telegram-long-reply-{}", uuid::Uuid::new_v4());
        let mut thread = Thread::with_id(thread_id.clone()).expect("thread");
        let incoming = crate::types::IncomingMessage {
            chat_id: CHAT,
            message_id: 42,
            user_id: 1,
            text: Some("go".to_string()),
            images: Vec::new(),
            audios: Vec::new(),
            documents: Vec::new(),
            videos: Vec::new(),
            notices: Vec::new(),
            message_thread_id: Some(7),
            is_forum: true,
            replied_to: None,
        };
        let reply_ctx = super::ReplyContext {
            reply_to_message_id: Some(42),
            topic_id: Some(7),
            cross_topic_reply_parameters: None,
        };
        let final_text = format!(
            "{}\n<media>{}</media>",
            long_explanation(),
            file.0.display()
        );
        super::send_final_response(
            &context,
            &incoming,
            &reply_ctx,
            None,
            &final_text,
            &thread_id,
            &[],
            &mut thread,
        )
        .await
        .expect("send final response");

        assert_eq!(
            fake.methods(),
            ["sendMessage", "sendMessage", "sendDocument", "sendMessage"]
        );
        let summary = sent_texts(&fake).pop().expect("summary");
        assert!(
            summary.contains("reply text part 2 of 2: not delivered"),
            "{summary}"
        );
        let events = load_thread_events(&thread_id).expect("events");
        let Some(ThreadEvent::Delivery { record, .. }) = events.last() else {
            panic!("outcome recorded last");
        };
        assert_eq!(
            statuses(record),
            [
                DeliveryStatus::Sent,
                DeliveryStatus::Failed,
                DeliveryStatus::Sent
            ]
        );
    }

    #[tokio::test]
    async fn short_text_and_one_file_go_out_as_one_captioned_message() {
        let fake = FakeTelegram::start().await;
        let file = TempFile::new("png");
        let (id, record) =
            deliver(&fake, Some("<b>Ready</b>"), std::slice::from_ref(&file.0)).await;

        assert_eq!(
            fake.methods(),
            ["sendPhoto"],
            "no extra request for buttons"
        );
        let body = &fake.seen()[0].body;
        assert!(body.contains("<b>Ready</b>"));
        assert!(
            body.contains("fu:0"),
            "follow-ups ride on the captioned file"
        );
        assert_eq!(
            open_url(body),
            artifact_link(MINI_APP, TOPIC_THREAD, &file.0),
            "and so does its file action"
        );
        assert!(
            body.find("Open artifact") < body.find("fu:0"),
            "file action first"
        );
        assert_eq!(
            statuses(&record),
            [DeliveryStatus::Sent, DeliveryStatus::Sent]
        );
        assert_eq!(id, record.items[0].message_id);
        assert_eq!(record.items[0].message_id, record.items[1].message_id);
    }

    #[tokio::test]
    async fn long_text_goes_first_with_the_buttons_then_the_file() {
        let fake = FakeTelegram::start().await;
        let file = TempFile::new("png");
        let text = "x".repeat(1100);
        let (id, record) = deliver(&fake, Some(&text), std::slice::from_ref(&file.0)).await;

        assert_eq!(fake.methods(), ["sendMessage", "sendPhoto"]);
        let seen = fake.seen();
        assert!(seen[0].body.contains("fu:0"));
        assert!(!seen[1].body.contains("fu:0"));
        assert!(open_url(&seen[1].body).is_some());
        assert_eq!(
            statuses(&record),
            [DeliveryStatus::Sent, DeliveryStatus::Sent]
        );
        assert_eq!(id, record.items[0].message_id);
    }

    #[tokio::test]
    async fn an_unabsorbed_flood_refusal_stops_the_remaining_items() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                429,
                "Too Many Requests: retry after 500",
                Some(500),
            )],
        );
        let a = TempFile::new("png");
        let b = TempFile::new("jpg");
        let (id, record) = deliver(&fake, Some("done"), &[a.0.clone(), b.0.clone()]).await;

        assert_eq!(id, None);
        assert_eq!(fake.methods(), ["sendMessage"]);
        assert_eq!(
            statuses(&record),
            [
                DeliveryStatus::Failed,
                DeliveryStatus::Failed,
                DeliveryStatus::Failed
            ]
        );
        assert!(
            record.items[1]
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("not attempted after a flood refusal"))
        );
    }

    #[tokio::test]
    async fn a_refused_captioned_file_still_delivers_the_text() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendPhoto",
            &[Reply::Error(400, "Bad Request: wrong file type", None)],
        );
        let file = TempFile::new("png");
        let (id, record) = deliver(&fake, Some("done"), std::slice::from_ref(&file.0)).await;

        assert_eq!(fake.methods(), ["sendPhoto", "sendMessage"]);
        assert_eq!(
            statuses(&record),
            [DeliveryStatus::Sent, DeliveryStatus::Failed]
        );
        assert_eq!(id, record.items[0].message_id);
    }

    #[tokio::test]
    async fn an_ambiguous_captioned_send_is_unknown_and_not_resent() {
        let fake = FakeTelegram::start().await;
        fake.script("sendPhoto", &[Reply::Garbage]);
        let file = TempFile::new("png");
        let (id, record) = deliver(&fake, Some("done"), std::slice::from_ref(&file.0)).await;

        assert_eq!(id, None);
        assert_eq!(fake.methods(), ["sendPhoto"]);
        assert_eq!(
            statuses(&record),
            [DeliveryStatus::Unknown, DeliveryStatus::Unknown]
        );
    }

    #[tokio::test]
    async fn a_missing_file_is_recorded_and_the_text_still_goes_out() {
        let fake = FakeTelegram::start().await;
        let missing = std::env::temp_dir().join(format!("zdx-missing-{}", uuid::Uuid::new_v4()));
        let (_, record) = deliver(&fake, Some("done"), &[missing]).await;

        assert_eq!(fake.methods(), ["sendMessage"]);
        assert_eq!(
            statuses(&record),
            [DeliveryStatus::Sent, DeliveryStatus::Failed]
        );
    }

    #[tokio::test]
    async fn only_an_invalid_reply_target_is_dropped_and_resent() {
        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(
                400,
                "Bad Request: message to be replied not found",
                None,
            )],
        );
        let (_, record) = deliver(&fake, Some(&"y".repeat(1100)), &[]).await;
        assert_eq!(fake.methods(), ["sendMessage", "sendMessage"]);
        assert!(fake.seen()[0].body.contains("\"reply_to_message_id\":42"));
        assert!(!fake.seen()[1].body.contains("reply_to_message_id"));
        assert_eq!(statuses(&record), [DeliveryStatus::Sent]);

        let fake = FakeTelegram::start().await;
        fake.script(
            "sendMessage",
            &[Reply::Error(400, "Bad Request: chat not found", None)],
        );
        let (_, record) = deliver(&fake, Some("z"), &[]).await;
        assert_eq!(fake.methods(), ["sendMessage"]);
        assert_eq!(statuses(&record), [DeliveryStatus::Failed]);
    }

    #[test]
    fn appends_a_deep_link_only_when_the_mini_app_is_configured() {
        assert_eq!(
            append_thread_link(
                Some("https://t.me/zdx_2026_bot/app"),
                "telegram--1001234567890-topic-17771",
                "done",
            ),
            "done\n\n<a href=\"https://t.me/zdx_2026_bot/app?startapp=telegram--1001234567890-topic-17771\">↗ Open thread</a>"
        );

        // Mini App disabled or unset: the answer must go out untouched.
        assert_eq!(
            append_thread_link(None, "telegram--100-topic-1", "done"),
            "done"
        );
    }

    #[test]
    fn appends_one_escaped_link_per_touched_worker() {
        let links = vec![
            (
                "dub · fix <PR> comments".to_string(),
                "https://t.me/c/1/2".to_string(),
            ),
            ("zdx".to_string(), "https://t.me/c/1/3".to_string()),
        ];
        assert_eq!(
            append_worker_links(&links, "sent"),
            "sent\n\n🛠 <a href=\"https://t.me/c/1/2\">dub · fix &lt;PR&gt; comments</a>\n🛠 <a href=\"https://t.me/c/1/3\">zdx</a>"
        );
        assert_eq!(append_worker_links(&[], "sent"), "sent");
    }
}
