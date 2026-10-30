//! Outbound Telegram Bot API calls: create a forum topic, send a message, send
//! a document.
//!
//! This is the shared core behind the `zdx telegram` CLI subcommands and the
//! native `telegram` tool, so both resolve tokens and talk to Telegram the same
//! way. It is deliberately narrow: only the three outbound operations those
//! surfaces expose.
//!
//! The Telegram bot runtime (`zdx-bot`) keeps its own richer client for the
//! long-poll loop, message editing, callbacks, and media types; that client
//! serves a different job and is not used here. Both share [`flood`] for
//! error classification, flood-wait retries, and per-call logging. This client
//! has no pacing gate: it runs in other processes (and beside the bot), so it
//! cannot see the bot's per-chat state.

pub mod flood;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use flood::{CallRecord, FailureClass, RetryPolicy, TelegramTransportError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;

const CONNECT_TIMEOUT_SECS: u64 = 10;
const HTTP_TIMEOUT_SECS: u64 = 60;

/// Telegram's hard limit on a message body.
pub const MAX_MESSAGE_CHARS: usize = 4096;
/// Telegram's hard limit on a document caption.
pub const MAX_CAPTION_CHARS: usize = 1024;
/// Telegram's upload limit for a document sent by a bot through
/// `api.telegram.org` (the only Bot API server this client talks to).
pub const MAX_DOCUMENT_BYTES: u64 = 50 * 1024 * 1024;

/// Message formatting mode accepted by the CLI and the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    Html,
    Markdown,
    MarkdownV2,
    Plain,
}

impl ParseMode {
    /// Parses a user-facing parse-mode name.
    ///
    /// # Errors
    /// Returns an error if the name is not recognized.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "html" => Ok(Self::Html),
            "markdown" => Ok(Self::Markdown),
            "markdown-v2" | "markdownv2" => Ok(Self::MarkdownV2),
            "plain" | "none" => Ok(Self::Plain),
            other => bail!(
                "invalid parse mode: {other} (expected html, markdown, markdown-v2, or plain)"
            ),
        }
    }

    #[must_use]
    fn api_value(self) -> Option<&'static str> {
        match self {
            Self::Html => Some("HTML"),
            Self::Markdown => Some("Markdown"),
            Self::MarkdownV2 => Some("MarkdownV2"),
            Self::Plain => None,
        }
    }
}

/// Receipt for one message Telegram accepted: the ids from its answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SentMessage {
    pub chat_id: i64,
    /// Forum topic the message landed in; `None` for the General topic or a
    /// chat without topics.
    pub message_thread_id: Option<i64>,
    pub message_id: i64,
    /// `t.me/c` link to the message; `None` where Telegram has no such link
    /// (private chats and basic groups).
    pub url: Option<String>,
}

impl SentMessage {
    fn from_result(chat_id: i64, sent: &SentResult) -> Self {
        let message_thread_id = if sent.is_topic_message {
            sent.message_thread_id
        } else {
            None
        };
        Self {
            chat_id,
            message_thread_id,
            message_id: sent.message_id,
            url: message_link(chat_id, message_thread_id, sent.message_id),
        }
    }
}

/// The fields of Telegram's `Message` answer that a send receipt needs.
/// `message_thread_id` names a forum topic only when `is_topic_message` is
/// set; on other messages it is a reply thread.
#[derive(Deserialize)]
struct SentResult {
    message_id: i64,
    message_thread_id: Option<i64>,
    #[serde(default)]
    is_topic_message: bool,
}

/// Thread id of the bot conversation for a chat or forum topic:
/// `telegram-{chat_id}-topic-{topic_id}` in a topic, `telegram-{chat_id}`
/// otherwise. The bot runs every message there through this id.
#[must_use]
pub fn conversation_thread_id(chat_id: i64, topic_id: Option<i64>) -> String {
    match topic_id {
        Some(topic_id) => format!("telegram-{chat_id}-topic-{topic_id}"),
        None => format!("telegram-{chat_id}"),
    }
}

/// What one send carried, for [`record_post`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PostDetails<'a> {
    /// Message text, or a document's caption.
    pub text: Option<&'a str>,
    /// Path of a sent document.
    pub file: Option<&'a Path>,
    /// Background for later replies, not shown in the chat.
    pub context: Option<&'a str>,
    /// Thread of the run that sent it.
    pub source_thread_id: Option<&'a str>,
}

/// Records a delivered message in the destination conversation's thread, so
/// a later message there (a reply or an ordinary one) runs with it as
/// context, and links it to the sender's thread (`core::posted::PostLink`)
/// so the sender's later runs find that conversation. Follows one `alias_to`
/// hop, like the bot. Returns the thread it
/// was written to, or `None` when the destination is the sender's own thread,
/// which already holds the send as a tool call.
///
/// # Errors
/// Returns an error if the thread cannot be written. The message is already
/// delivered then; callers report the error and must not resend.
pub fn record_post(sent: &SentMessage, details: PostDetails<'_>) -> Result<Option<String>> {
    use crate::core::posted::PostedMessage;
    use crate::core::thread_persistence::{Thread, ThreadEvent, read_thread_alias};

    let effective = |id: &str| match read_thread_alias(id) {
        Ok(Some(source)) => source,
        _ => id.to_string(),
    };
    let target = effective(&conversation_thread_id(
        sent.chat_id,
        sent.message_thread_id,
    ));
    let source_thread_id = details
        .source_thread_id
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if source_thread_id.is_some_and(|source| effective(source) == target) {
        return Ok(None);
    }

    let post = PostedMessage {
        surface: "telegram".to_string(),
        chat_id: sent.chat_id,
        topic_id: sent.message_thread_id,
        message_id: sent.message_id,
        url: sent.url.clone(),
        text: details.text.map(str::to_string),
        file: details.file.map(|path| path.display().to_string()),
        context: details.context.map(str::to_string),
        source_thread_id: source_thread_id.map(str::to_string),
    }
    .normalized();
    let event = ThreadEvent::posted(post);
    Thread::with_id(target.clone())
        .and_then(|mut thread| thread.append(&event))
        .with_context(|| format!("record posted message in thread {target}"))?;
    if let (Some(source), ThreadEvent::Posted { post, ts }) = (source_thread_id, &event) {
        let link = crate::core::posted::PostLink::new(ts, &target, post);
        if let Err(err) =
            crate::core::posted::append_link(&crate::config::paths::zdx_home(), source, &link)
        {
            tracing::warn!(
                error = %format!("{err:#}"),
                "Linking posted Telegram message to its sender failed"
            );
        }
    }
    Ok(Some(target))
}

/// Builds the `t.me/c` link to one message in a supergroup:
/// `https://t.me/c/<internal_id>/<topic_id>/<message_id>` inside a forum
/// topic, `https://t.me/c/<internal_id>/<message_id>` otherwise.
///
/// Supergroup chat ids carry the `-100` prefix that `t.me/c` links omit.
/// Private chats (positive ids) and basic groups have no such link.
#[must_use]
pub fn message_link(
    chat_id: i64,
    message_thread_id: Option<i64>,
    message_id: i64,
) -> Option<String> {
    let chat = chat_id.to_string();
    let internal = chat.strip_prefix("-100").filter(|id| !id.is_empty())?;
    if message_id <= 0 {
        return None;
    }
    Some(match message_thread_id.filter(|topic| *topic > 0) {
        Some(topic) => format!("https://t.me/c/{internal}/{topic}/{message_id}"),
        None => format!("https://t.me/c/{internal}/{message_id}"),
    })
}

/// Minimal outbound Telegram Bot API client.
#[derive(Clone)]
pub struct TelegramOutbound {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl TelegramOutbound {
    #[must_use]
    pub fn new(token: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: "https://api.telegram.org".to_string(),
            token,
        }
    }

    /// Creates a forum topic and returns its `message_thread_id`.
    ///
    /// # Errors
    /// Returns an error if the name is empty or the request fails.
    pub async fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("topic name must not be empty");
        }

        let value: Value = self
            .post(
                "createForumTopic",
                chat_id,
                None,
                &serde_json::json!({
                    "chat_id": chat_id,
                    "name": name,
                }),
            )
            .await?;

        value
            .get("message_thread_id")
            .and_then(Value::as_i64)
            .context("Telegram createForumTopic response had no message_thread_id")
    }

    /// Sends a text message and returns Telegram's receipt for it.
    ///
    /// # Errors
    /// Returns an error if the text is empty, exceeds Telegram's limit, or the
    /// request fails.
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        message_thread_id: Option<i64>,
        parse_mode: ParseMode,
    ) -> Result<SentMessage> {
        let text = text.trim();
        if text.is_empty() {
            bail!("text must not be empty");
        }
        let chars = text.chars().count();
        if chars > MAX_MESSAGE_CHARS {
            bail!(
                "message is {chars} characters; Telegram's limit is {MAX_MESSAGE_CHARS}. Split it into several messages by section, or send a file instead"
            );
        }

        let mut body = serde_json::json!({ "chat_id": chat_id, "text": text });
        if let Some(thread) = message_thread_id {
            body["message_thread_id"] = thread.into();
        }
        if let Some(mode) = parse_mode.api_value() {
            body["parse_mode"] = mode.into();
        }

        let sent: SentResult = self
            .post("sendMessage", chat_id, message_thread_id, &body)
            .await?;
        Ok(SentMessage::from_result(chat_id, &sent))
    }

    /// Sends a local file as a document and returns Telegram's receipt for it.
    ///
    /// # Errors
    /// Returns an error if the file is missing, the caption is too long, the
    /// file is over [`MAX_DOCUMENT_BYTES`], or the request fails. The size is
    /// checked from the file's metadata before it is read, and again on the
    /// bytes read, in case the file grew in between; either refusal means
    /// nothing was sent.
    pub async fn send_document(
        &self,
        chat_id: i64,
        path: &Path,
        caption: Option<&str>,
        message_thread_id: Option<i64>,
    ) -> Result<SentMessage> {
        if !path.is_file() {
            bail!("file not found: {}", path.display());
        }
        if let Some(caption) = caption {
            let chars = caption.chars().count();
            if chars > MAX_CAPTION_CHARS {
                bail!(
                    "caption is {chars} characters; Telegram's limit is {MAX_CAPTION_CHARS}. Put the detail in a separate message instead"
                );
            }
        }

        let len = std::fs::metadata(path)
            .with_context(|| format!("read document {}", path.display()))?
            .len();
        check_document_size(len)?;
        let bytes =
            std::fs::read(path).with_context(|| format!("read document {}", path.display()))?;
        check_document_size(bytes.len() as u64)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.bin")
            .to_string();
        let mime = infer::get(&bytes).map_or_else(
            || "application/octet-stream".to_string(),
            |kind| kind.mime_type().to_string(),
        );

        let caption = caption.map(str::trim).filter(|c| !c.is_empty());
        let sent: SentResult = self
            .send_with_retry("sendDocument", chat_id, message_thread_id, || {
                let part = reqwest::multipart::Part::bytes(bytes.clone())
                    .file_name(file_name.clone())
                    .mime_str(&mime)
                    .context("build Telegram document part")?;
                let mut form = reqwest::multipart::Form::new()
                    .text("chat_id", chat_id.to_string())
                    .part("document", part);
                if let Some(caption) = caption {
                    form = form.text("caption", caption.to_string());
                }
                if let Some(thread) = message_thread_id {
                    form = form.text("message_thread_id", thread.to_string());
                }
                Ok(self.http.post(self.url("sendDocument")).multipart(form))
            })
            .await?;
        Ok(SentMessage::from_result(chat_id, &sent))
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.token, method)
    }

    async fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        method: &str,
        chat_id: i64,
        message_thread_id: Option<i64>,
        body: &B,
    ) -> Result<T> {
        self.send_with_retry(method, chat_id, message_thread_id, || {
            Ok(self.http.post(self.url(method)).json(body))
        })
        .await
    }

    /// Sends a request, retrying only explicit flood refusals within
    /// [`RetryPolicy::IMPORTANT`]. Ambiguous failures (timeouts, unreadable
    /// answers) are returned as-is: the message may already be in the chat.
    async fn send_with_retry<T: DeserializeOwned>(
        &self,
        method: &str,
        chat_id: i64,
        message_thread_id: Option<i64>,
        build: impl Fn() -> Result<reqwest::RequestBuilder>,
    ) -> Result<T> {
        let policy = RetryPolicy::IMPORTANT;
        let first_attempt = Instant::now();
        let mut waited = Duration::ZERO;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let started = Instant::now();
            let result = self
                .send_once(method, build())
                .await
                .and_then(|value| flood::decode_result::<T>(method, value));
            let record = CallRecord {
                client: "outbound",
                bot: flood::bot_identity(&self.token),
                method,
                chat: Some(chat_id),
                topic: message_thread_id,
                new_message: method != "createForumTopic",
                attempt,
                waited,
                duration: started.elapsed(),
            };
            flood::log_call(&record, result.as_ref().map(|(message_id, _)| *message_id));
            let err = match result {
                Ok((_, decoded)) => return Ok(decoded),
                Err(err) => err,
            };
            let FailureClass::FloodWait(retry_after) = flood::classify(&err) else {
                return Err(err);
            };
            let Some(delay) =
                policy.delay_before_retry(attempt, first_attempt.elapsed(), retry_after)
            else {
                return Err(err);
            };
            tokio::time::sleep(delay).await;
            waited = delay;
        }
    }

    async fn send_once(
        &self,
        method: &str,
        request: Result<reqwest::RequestBuilder>,
    ) -> Result<Value> {
        let response = request?
            .send()
            .await
            .map_err(|err| TelegramTransportError::from_reqwest(method, err))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|err| TelegramTransportError::from_reqwest(method, err))?;
        flood::parse_response(method, &bytes)
    }
}

/// Resolves the bot token from an explicit override, config, then environment.
///
/// # Errors
/// Returns an error when no token is configured.
pub fn resolve_bot_token(config: &Config, override_token: Option<&str>) -> Result<String> {
    let candidates = [
        override_token.map(str::to_string),
        config.telegram.bot_token.clone(),
        std::env::var("ZDX_TELEGRAM_BOT_TOKEN").ok(),
        std::env::var("TELEGRAM_BOT_TOKEN").ok(),
    ];

    for candidate in candidates.into_iter().flatten() {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    bail!(
        "Telegram bot token is required (set telegram.bot_token, ZDX_TELEGRAM_BOT_TOKEN, or TELEGRAM_BOT_TOKEN)"
    )
}

fn check_document_size(len: u64) -> Result<()> {
    if len > MAX_DOCUMENT_BYTES {
        bail!(
            "document is {len} bytes; Telegram's limit for bots is {MAX_DOCUMENT_BYTES} bytes (50 MB). Send a smaller file or share it another way"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_parse_modes() {
        assert_eq!(ParseMode::parse("html").unwrap(), ParseMode::Html);
        assert_eq!(ParseMode::parse("HTML").unwrap(), ParseMode::Html);
        assert_eq!(
            ParseMode::parse("markdown-v2").unwrap(),
            ParseMode::MarkdownV2
        );
        assert_eq!(ParseMode::parse("plain").unwrap(), ParseMode::Plain);
        assert!(ParseMode::parse("rst").is_err());
    }

    #[test]
    fn plain_sends_no_parse_mode() {
        assert_eq!(ParseMode::Plain.api_value(), None);
        assert_eq!(ParseMode::Html.api_value(), Some("HTML"));
    }

    #[test]
    fn surfaces_telegram_api_errors() {
        let body = br#"{"ok":false,"description":"chat not found"}"#;
        let err = flood::parse_response("sendMessage", body).unwrap_err();
        assert!(format!("{err:#}").contains("chat not found"));
    }

    #[test]
    fn decodes_ok_results() {
        let body = br#"{"ok":true,"result":{"message_thread_id":42}}"#;
        let value: Value = flood::parse_response("createForumTopic", body).unwrap();
        assert_eq!(value["message_thread_id"], 42);
    }

    #[test]
    fn message_links_follow_t_me_c_forms() {
        assert_eq!(
            message_link(-1_001_234_567_890, Some(17), 4242).as_deref(),
            Some("https://t.me/c/1234567890/17/4242")
        );
        assert_eq!(
            message_link(-1_001_234_567_890, None, 4242).as_deref(),
            Some("https://t.me/c/1234567890/4242")
        );
        assert_eq!(message_link(123_456, Some(17), 4242), None);
        assert_eq!(message_link(-4_242, None, 7), None);
    }

    /// Serves one canned Telegram answer on a local port and returns the raw
    /// request it received.
    pub(super) fn one_shot_server(
        response_body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 8192];
            let header_end = loop {
                let read = stream.read(&mut buf).expect("read");
                assert!(read > 0, "client closed before headers ended");
                request.extend_from_slice(&buf[..read]);
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let content_length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map_or(0, |value| value.trim().parse().expect("content-length"));
            while request.len() < header_end + content_length {
                let read = stream.read(&mut buf).expect("read body");
                assert!(read > 0, "client closed before body ended");
                request.extend_from_slice(&buf[..read]);
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            )
            .expect("write");
            String::from_utf8_lossy(&request).into_owned()
        });
        (base_url, handle)
    }

    fn client_for(base_url: String) -> TelegramOutbound {
        TelegramOutbound {
            base_url,
            ..TelegramOutbound::new("123:test-token".to_string())
        }
    }

    #[tokio::test]
    async fn send_message_returns_telegram_message_id_and_topic_link() {
        let (base_url, server) = one_shot_server(
            r#"{"ok":true,"result":{"message_id":4242,"message_thread_id":17,"is_topic_message":true,"chat":{"id":-1001234567890}}}"#,
        );
        let sent = client_for(base_url)
            .send_message(-1_001_234_567_890, "hello", Some(17), ParseMode::Html)
            .await
            .expect("send");
        assert_eq!(
            sent,
            SentMessage {
                chat_id: -1_001_234_567_890,
                message_thread_id: Some(17),
                message_id: 4242,
                url: Some("https://t.me/c/1234567890/17/4242".to_string()),
            }
        );
        let request = server.join().expect("server");
        assert!(request.starts_with("POST /bot123:test-token/sendMessage "));
        assert!(request.contains(r#""message_thread_id":17"#));
        assert!(request.contains(r#""parse_mode":"HTML""#));
    }

    #[tokio::test]
    async fn send_document_returns_telegram_message_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("report.txt");
        std::fs::write(&path, "report body").expect("write file");
        let (base_url, server) = one_shot_server(
            r#"{"ok":true,"result":{"message_id":99,"chat":{"id":-1001234567890},"document":{"file_id":"x"}}}"#,
        );
        let sent = client_for(base_url)
            .send_document(-1_001_234_567_890, &path, Some("caption"), None)
            .await
            .expect("send");
        assert_eq!(sent.message_id, 99);
        assert_eq!(sent.message_thread_id, None);
        assert_eq!(sent.url.as_deref(), Some("https://t.me/c/1234567890/99"));
        let request = server.join().expect("server");
        assert!(request.starts_with("POST /bot123:test-token/sendDocument "));
        assert!(request.contains("report body"));
    }

    /// Over the limit: refused from the file size alone, before the file is
    /// read (it is unreadable, mode 000) and before any request, as not sent.
    /// The same check runs on the bytes read; exactly the limit is allowed.
    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_documents_are_refused_before_reading_or_sending() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.bin");
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(MAX_DOCUMENT_BYTES + 1).expect("sparse size");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // Nothing listens here: any request would fail as a transport error.
        let err = client_for("http://127.0.0.1:9".to_string())
            .send_document(-1_001_234_567_890, &path, None, None)
            .await
            .expect_err("over the limit");
        assert_eq!(
            format!("{err:#}"),
            format!(
                "document is {} bytes; Telegram's limit for bots is {MAX_DOCUMENT_BYTES} bytes (50 MB). Send a smaller file or share it another way",
                MAX_DOCUMENT_BYTES + 1
            )
        );
        assert!(matches!(flood::classify(&err), FailureClass::NotSent));

        assert!(check_document_size(MAX_DOCUMENT_BYTES).is_ok());
        assert!(check_document_size(MAX_DOCUMENT_BYTES + 1).is_err());
    }

    #[tokio::test]
    async fn reply_thread_is_not_reported_as_topic() {
        let (base_url, server) = one_shot_server(
            r#"{"ok":true,"result":{"message_id":8,"message_thread_id":3,"chat":{"id":-1001234567890}}}"#,
        );
        let sent = client_for(base_url)
            .send_message(-1_001_234_567_890, "hello", None, ParseMode::Plain)
            .await
            .expect("send");
        assert_eq!(sent.message_thread_id, None);
        assert_eq!(sent.url.as_deref(), Some("https://t.me/c/1234567890/8"));
        server.join().expect("server");
    }

    #[tokio::test]
    async fn answer_without_message_id_is_an_error_not_an_invented_id() {
        let (base_url, server) = one_shot_server(r#"{"ok":true,"result":true}"#);
        let err = client_for(base_url)
            .send_message(-1_001_234_567_890, "hello", None, ParseMode::Plain)
            .await
            .expect_err("missing message_id must fail");
        assert!(matches!(flood::classify(&err), FailureClass::Unknown));
        server.join().expect("server");
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;
    use crate::core::posted::PostedMessage;
    use crate::core::thread_persistence::{
        Thread, ThreadEvent, load_thread_events, load_thread_replay,
    };

    fn posts(thread_id: &str) -> Vec<(String, PostedMessage)> {
        load_thread_events(thread_id)
            .expect("events")
            .into_iter()
            .filter_map(|event| match event {
                ThreadEvent::Posted { post, ts } => Some((ts, post)),
                _ => None,
            })
            .collect()
    }

    fn sent(topic: Option<i64>, message_id: i64) -> SentMessage {
        SentMessage {
            chat_id: -1_001_234_567_890,
            message_thread_id: topic,
            message_id,
            url: message_link(-1_001_234_567_890, topic, message_id),
        }
    }

    /// The shared path behind the CLI and the tool: a delivered message lands
    /// in the destination conversation's thread with its context and sender,
    /// and replay turns it into the note for the next user message.
    #[tokio::test]
    async fn a_delivered_message_is_recorded_in_the_destination_thread() {
        let _home = crate::test_support::temp_zdx_home();
        let (base_url, server) = super::tests::one_shot_server(
            r#"{"ok":true,"result":{"message_id":70,"message_thread_id":69,"is_topic_message":true,"chat":{"id":-1001234567890}}}"#,
        );
        let client = TelegramOutbound {
            base_url,
            ..TelegramOutbound::new("123:test-token".to_string())
        };
        let text = "⏰ Pay the cleaner: due 30/10";
        let sent = client
            .send_message(-1_001_234_567_890, text, Some(69), ParseMode::Html)
            .await
            .expect("send");
        server.join().expect("server");

        let recorded = record_post(
            &sent,
            PostDetails {
                text: Some(text),
                context: Some("  Expected: R$ 100,00 (to be confirmed)  "),
                source_thread_id: Some("job-example-1"),
                ..PostDetails::default()
            },
        )
        .expect("record");
        let thread_id = "telegram--1001234567890-topic-69";
        assert_eq!(recorded.as_deref(), Some(thread_id));

        let posts = posts(thread_id);
        assert_eq!(posts.len(), 1);
        let post = &posts[0].1;
        assert_eq!(post.message_id, 70);
        assert_eq!(post.topic_id, Some(69));
        assert_eq!(post.text.as_deref(), Some(text));
        assert_eq!(
            post.context.as_deref(),
            Some("Expected: R$ 100,00 (to be confirmed)")
        );
        assert_eq!(post.source_thread_id.as_deref(), Some("job-example-1"));
        assert_eq!(post.url.as_deref(), Some("https://t.me/c/1234567890/69/70"));

        let links =
            crate::core::posted::links_from(&crate::config::paths::zdx_home(), "job-example-1")
                .expect("links");
        assert_eq!(links.len(), 1, "the sender's later runs can find the post");
        assert_eq!(links[0].target_thread_id, thread_id);
        assert_eq!(links[0].message_id, 70);
        assert_eq!(links[0].ts, posts[0].0);

        let replay = load_thread_replay(thread_id).expect("replay");
        assert!(replay.messages.is_empty());
        let note = replay.trailing_note.expect("note");
        assert!(note.contains("by thread job-example-1"), "{note}");
        assert!(
            note.contains("Expected: R$ 100,00 (to be confirmed)"),
            "{note}"
        );
    }

    #[test]
    fn a_send_into_the_senders_own_thread_is_not_recorded() {
        let _home = crate::test_support::temp_zdx_home();
        let own = conversation_thread_id(-1_001_234_567_890, Some(5));
        let recorded = record_post(
            &sent(Some(5), 9),
            PostDetails {
                text: Some("hi"),
                source_thread_id: Some(&own),
                ..PostDetails::default()
            },
        )
        .expect("record");
        assert_eq!(recorded, None);
        assert!(posts(&own).is_empty());
    }

    /// A resumed topic reads its history from the source thread, so the post
    /// goes there; a sender running in that source thread is the same thread.
    #[test]
    fn records_follow_one_alias_hop() {
        let _home = crate::test_support::temp_zdx_home();
        let topic = conversation_thread_id(-1_001_234_567_890, Some(8));
        Thread::with_id(topic.clone())
            .expect("thread")
            .set_alias(Some("source-thread".to_string()))
            .expect("alias");

        let recorded = record_post(
            &sent(Some(8), 3),
            PostDetails {
                text: Some("hi"),
                ..PostDetails::default()
            },
        )
        .expect("record");
        assert_eq!(recorded.as_deref(), Some("source-thread"));
        assert_eq!(posts("source-thread").len(), 1);

        let skipped = record_post(
            &sent(Some(8), 4),
            PostDetails {
                text: Some("again"),
                source_thread_id: Some("source-thread"),
                ..PostDetails::default()
            },
        )
        .expect("record");
        assert_eq!(skipped, None);
    }

    #[test]
    fn general_topic_posts_use_the_chat_thread() {
        assert_eq!(
            conversation_thread_id(-1_001_234_567_890, None),
            "telegram--1001234567890"
        );
        assert_eq!(
            conversation_thread_id(-1_001_234_567_890, Some(7)),
            "telegram--1001234567890-topic-7"
        );
    }
}
