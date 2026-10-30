use anyhow::Result;
use commands::{
    handle_general_forum_commands, handle_restart_command, handle_thread_setup_commands,
};
use status::{discard_turn_status, finalize_preprocessing_cancelled, setup_preprocessing_status};
use tokio_util::sync::CancellationToken;
use turn::run_agent_turn;
use zdx_engine::core::thread_persistence;

use crate::bot::context::BotContext;
use crate::ingest::{self, AllowlistConfig};
use crate::telegram::{InlineKeyboardMarkup, Message, ReplyParameters};

mod commands;
mod launcher;
mod media;
mod response;
mod status;
mod thread_header;
mod turn;

pub(crate) use commands::{
    ModelPickerScope, build_model_thinking_keyboard, build_models_keyboard,
    build_provider_keyboard, models_for_provider,
};
pub(crate) use launcher::{
    LauncherMap, create_topic_with_model, handle_callback as handle_launcher_callback,
    new_launcher_map, render_launcher, schedule_repost as schedule_launcher_repost,
};
pub(crate) use response::{append_thread_link, worker_link};
pub(crate) use thread_header::{
    handle_callback as handle_thread_header_callback, post_thread_header, refresh_thread_header,
};

/// Groups the reply-targeting fields that travel together through the turn pipeline.
struct ReplyContext {
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    cross_topic_reply_parameters: Option<ReplyParameters>,
}

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) async fn handle_message(
    context: &std::sync::Arc<BotContext>,
    queues: &crate::bot::queue::ChatQueueMap,
    message: Message,
) -> Result<()> {
    let ctx = context.as_ref();
    let bot_config = context.config_for_chat(message.chat.id);
    let synthetic_topic_routed_from_general = message.synthetic_topic_routed_from_general;
    let routed_as_btw_input = message.routed_as_btw_input;
    let synthetic_prompt = message.synthetic.is_some();
    let provisional_status = if message_has_audio(&message) {
        Some(setup_preprocessing_status(ctx, &message, synthetic_topic_routed_from_general).await)
    } else {
        None
    };
    let allowlist = AllowlistConfig {
        user_ids: ctx.allowlist_user_ids(),
        chat_ids: ctx.allowlist_chat_ids(),
    };
    let parsed = parse_message_with_status(
        ctx,
        allowlist,
        &bot_config,
        &message,
        provisional_status.as_ref(),
    )
    .await;
    let incoming = match parsed {
        Ok(Some(incoming)) => incoming,
        Ok(None) => {
            cleanup_provisional_status(ctx, None, provisional_status).await;
            return Ok(());
        }
        Err(err) => {
            let unavailable = err.downcast::<ingest::DownloadUnavailable>()?;
            cleanup_provisional_status(ctx, Some(message.chat.id), provisional_status).await;
            crate::retry::offer_download_retry(ctx, message, &unavailable.label).await;
            return Ok(());
        }
    };

    let reply_ctx = build_reply_context(
        &incoming,
        synthetic_topic_routed_from_general,
        synthetic_prompt,
    );

    // Worker mirror topics are checked before ANY local interpretation:
    // slash commands, staging, and setup flows would otherwise act on (or
    // rewrite) the aliased worker JSONL while the worker's child process owns
    // it. Every mirror-topic message is queued to the worker verbatim.
    if route_worker_topic_message(ctx, &incoming, &reply_ctx).await? {
        cleanup_provisional_status(ctx, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    if handle_pre_agent_commands(context, &incoming, &reply_ctx).await? {
        cleanup_provisional_status(ctx, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    tracing::info!(
        user_id = incoming.user_id,
        chat_id = incoming.chat_id,
        topic_id = ?reply_ctx.topic_id,
        message_id = incoming.message_id,
        "Accepted message",
    );

    let thread_id = thread_id_for_chat(incoming.chat_id, reply_ctx.topic_id);
    // Resumed topics alias to a source thread; resolve so history load + writes
    // (and model/thinking/root overrides) target the source thread.
    let thread_id = resolve_effective_thread_id(&thread_id);
    if handle_thread_setup_commands(ctx, &incoming, &reply_ctx, &thread_id).await? {
        cleanup_provisional_status(ctx, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    if crate::staging::handle_staging_flow(
        context,
        queues,
        &incoming,
        reply_ctx.reply_to_message_id,
        reply_ctx.topic_id,
        &thread_id,
        synthetic_prompt,
    )
    .await?
    {
        cleanup_provisional_status(ctx, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    // A `/btw` input skipped this topic's queue, so it must never fall through
    // into a normal turn — that would run concurrently with the turn holding
    // the queue slot and write to the same thread. The session can be gone by
    // now (cancelled, or already used by an earlier input).
    if routed_as_btw_input {
        cleanup_provisional_status(ctx, Some(incoming.chat_id), provisional_status).await;
        notify_stale_btw_input(ctx, &incoming, &reply_ctx).await;
        return Ok(());
    }

    run_agent_turn(
        ctx,
        incoming,
        reply_ctx,
        &thread_id,
        synthetic_topic_routed_from_general,
        provisional_status,
        true,
    )
    .await?;
    Ok(())
}

/// Re-runs a failed turn from the already-persisted thread state.
///
/// No new user message is recorded — the last persisted user message (whose
/// turn failed) is retried verbatim. Builds a minimal synthetic incoming so
/// the existing turn pipeline (status message, streaming, final send) can run.
///
/// # Errors
/// Returns an error if the turn cannot be spawned or persisted.
pub(crate) async fn retry_agent_turn(
    context: &BotContext,
    chat_id: i64,
    user_message_id: i64,
    thread_id: &str,
    topic_id: Option<i64>,
    reply_to_message_id: Option<i64>,
) -> Result<()> {
    let incoming = crate::types::IncomingMessage {
        chat_id,
        message_id: user_message_id,
        user_id: 0,
        text: None,
        images: Vec::new(),
        audios: Vec::new(),
        documents: Vec::new(),
        videos: Vec::new(),
        notices: Vec::new(),
        message_thread_id: topic_id,
        is_forum: topic_id.is_some(),
        replied_to: None,
    };
    let reply_ctx = ReplyContext {
        reply_to_message_id,
        topic_id,
        cross_topic_reply_parameters: None,
    };

    run_agent_turn(context, incoming, reply_ctx, thread_id, false, None, false).await
}

async fn parse_message_with_status(
    context: &BotContext,
    allowlist: AllowlistConfig<'_>,
    bot_config: &zdx_engine::config::Config,
    message: &Message,
    provisional_status: Option<&TurnStatus>,
) -> Result<Option<crate::types::IncomingMessage>> {
    match ingest::parse_incoming_message(
        context.client(),
        allowlist,
        bot_config,
        message,
        provisional_status.map(|status| &status.token),
    )
    .await
    {
        Ok(incoming) => Ok(incoming),
        Err(err) if crate::transcribe::is_operation_cancelled(&err) => {
            if let Some(status) = provisional_status {
                finalize_preprocessing_cancelled(context, status.key.0, status).await;
            }
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Tells the user their `/btw` input arrived after the session ended.
async fn notify_stale_btw_input(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
) {
    if let Err(err) = context
        .client()
        .send_message(
            incoming.chat_id,
            "💬 No side question is pending anymore — send /btw again to ask one.",
            Some(incoming.message_id),
            reply_ctx.topic_id,
        )
        .await
    {
        tracing::warn!(err = %format!("{err:#}"), "Failed to notify stale btw input");
    }
}

/// Routes a mirror-topic message into its worker's FIFO, returning `true` when
/// the message was consumed.
///
/// The aliased thread's JSONL is written by the worker's child process, so an
/// in-process turn in a mirror topic would mean two writers on one file. The
/// queued prompt uses the same text a normal turn would record (voice
/// transcripts and attachment notes included), so steering by voice works.
async fn route_worker_topic_message(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
) -> Result<bool> {
    let topic_thread_id = thread_id_for_chat(incoming.chat_id, reply_ctx.topic_id);
    if !thread_persistence::read_thread_worker_topic(&topic_thread_id)? {
        return Ok(false);
    }
    let worker_thread_id = resolve_effective_thread_id(&topic_thread_id);

    let prompt = crate::agent::build_user_text(incoming);
    let reply = match context
        .worker_manager()
        .enqueue_from_topic(&worker_thread_id, &prompt)
    {
        Ok(snapshot) => {
            if snapshot.status == zdx_engine::core::workers::WorkerStatus::Running
                || snapshot.queue_depth > 1
            {
                format!(
                    "📨 Queued to the worker ({} waiting).",
                    snapshot.queue_depth
                )
            } else {
                "📨 Queued to the worker.".to_string()
            }
        }
        Err(err) => format!("❌ Couldn't queue to the worker: {err:#}"),
    };
    if let Err(err) = context
        .client()
        .send_message(
            incoming.chat_id,
            &reply,
            // A synthetic prompt (Mini App feedback) has no message to quote.
            reply_ctx.reply_to_message_id,
            reply_ctx.topic_id,
        )
        .await
    {
        tracing::warn!(err = %format!("{err:#}"), "Failed to acknowledge worker topic message");
    }
    Ok(true)
}

/// Where a reply goes. `synthetic_prompt` marks a bot-generated prompt
/// (`Message::synthetic`, e.g. a `[worker update]`): its message id is an
/// internal counter, not a Telegram message, so the reply quotes nothing.
/// `synthetic_topic_routed_from_general` is different: a real user message
/// from General, answered in its new topic with a cross-topic quote.
fn build_reply_context(
    incoming: &crate::types::IncomingMessage,
    synthetic_topic_routed_from_general: bool,
    synthetic_prompt: bool,
) -> ReplyContext {
    let topic_id = incoming.message_thread_id;
    if synthetic_prompt {
        return ReplyContext {
            reply_to_message_id: None,
            topic_id,
            cross_topic_reply_parameters: None,
        };
    }
    let reply_to_message_id = if synthetic_topic_routed_from_general
        || incoming.message_thread_id == Some(incoming.message_id)
    {
        None
    } else {
        Some(incoming.message_id)
    };
    let cross_topic_reply_parameters = if synthetic_topic_routed_from_general && topic_id.is_some()
    {
        Some(ReplyParameters {
            message_id: incoming.message_id,
            chat_id: Some(incoming.chat_id),
            allow_sending_without_reply: Some(true),
        })
    } else {
        None
    };

    ReplyContext {
        reply_to_message_id,
        topic_id,
        cross_topic_reply_parameters,
    }
}

async fn handle_pre_agent_commands(
    context: &std::sync::Arc<BotContext>,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
) -> Result<bool> {
    Ok(
        handle_general_forum_commands(context, incoming, reply_ctx.reply_to_message_id).await?
            || handle_restart_command(context, incoming, reply_ctx.reply_to_message_id).await?,
    )
}

async fn cleanup_provisional_status(
    context: &BotContext,
    chat_id: Option<i64>,
    provisional_status: Option<TurnStatus>,
) {
    if let Some(status) = provisional_status {
        discard_turn_status(context, chat_id, &status).await;
    }
}

struct TurnStatus {
    key: (i64, i64),
    token: CancellationToken,
    markup: InlineKeyboardMarkup,
    message_id: Option<i64>,
}

fn message_has_audio(message: &Message) -> bool {
    message.voice.is_some()
        || message.audio.is_some()
        || message
            .local_attachments
            .iter()
            .any(|attachment| matches!(attachment, crate::types::LocalAttachment::Audio { .. }))
        || message
            .document
            .as_ref()
            .and_then(|doc| doc.mime_type.as_deref())
            .is_some_and(|mime| mime.starts_with("audio/"))
}

struct TurnResult {
    final_text: String,
    got_result: bool,
    had_error: bool,
    error_message: Option<String>,
    /// Worker thread ids this turn created or messaged (orchestrator turns),
    /// in first-touch order, so the reply can link them.
    touched_workers: Vec<String>,
}

struct SpawnRequest<'a> {
    thread_id: &'a str,
    thread: &'a zdx_engine::core::thread_persistence::Thread,
    messages: Vec<zdx_engine::providers::ChatMessage>,
    /// Fully prepared turn (config, system prompt, tool overrides).
    prepared: crate::agent::PreparedBotTurn,
}

pub(crate) fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn format_user_error_message(message: &str) -> String {
    let trimmed = message.trim();
    let compact = if trimmed.len() > 700 {
        format!("{}…", trimmed.chars().take(700).collect::<String>())
    } else {
        trimmed.to_string()
    };
    format!("❌ Request failed.\n\n<pre>{}</pre>", escape_html(&compact))
}

/// Prefix marking a forum topic as an orchestrator home base, like the `🛠`
/// prefix legacy worker mirror topics carry. Applied to the Telegram topic
/// name only; the persisted thread title stays clean.
pub(crate) const ORCHESTRATOR_TOPIC_PREFIX: &str = "🎛️";

/// Decorates a forum topic name for an orchestrator home base.
///
/// Idempotent, and tolerant of the prefix without its variation selector, so a
/// topic renamed by hand or re-titled by the auto-title pass never collects two
/// icons.
pub(crate) fn orchestrator_topic_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.starts_with('🎛') {
        return trimmed.to_string();
    }
    format!("{ORCHESTRATOR_TOPIC_PREFIX} {trimmed}")
}

pub(crate) fn thread_id_for_chat(chat_id: i64, message_thread_id: Option<i64>) -> String {
    zdx_engine::telegram::conversation_thread_id(chat_id, message_thread_id)
}

/// Inverse of [`thread_id_for_chat`] for topic threads: `(chat_id, topic_id)`
/// from a `telegram-{chat_id}-topic-{topic_id}` id, `None` for anything else.
pub(crate) fn parse_topic_thread_id(thread_id: &str) -> Option<(i64, i64)> {
    let rest = thread_id.strip_prefix("telegram-")?;
    let (chat, topic) = rest.rsplit_once("-topic-")?;
    Some((chat.parse().ok()?, topic.parse().ok()?))
}

/// Follow a single `alias_to` hop so a resumed topic reads history from and
/// persists new events to its source thread. One hop only (no chains/loops);
/// a no-op when the thread has no alias.
pub(crate) fn resolve_effective_thread_id(id: &str) -> String {
    match zdx_engine::core::thread_persistence::read_thread_alias(id) {
        Ok(Some(source)) => source,
        _ => id.to_string(),
    }
}

/// Mini App base URL for a chat, when the embedded server is enabled and
/// configured. `None` means Mini App buttons are omitted.
pub(crate) fn mini_app_base_url(context: &BotContext, chat_id: i64) -> Option<String> {
    context
        .config_for_chat(chat_id)
        .telegram
        .server
        .as_ref()
        .filter(|server| server.enabled)
        .and_then(|server| server.mini_app_url.clone())
}

#[cfg(test)]
mod reply_target_tests {
    use zdx_engine::core::delivery::DeliveryStatus;
    use zdx_engine::core::thread_persistence::{Thread, ThreadEvent, load_thread_events};

    use super::build_reply_context;
    use super::response::send_final_response;
    use crate::bot::context::BotContext;
    use crate::telegram::TelegramClient;
    use crate::telegram::fake::{FakeTelegram, TOKEN};

    const CHAT: i64 = -1_001_234;

    fn incoming(message_id: i64, topic: Option<i64>) -> crate::types::IncomingMessage {
        crate::types::IncomingMessage {
            chat_id: CHAT,
            message_id,
            user_id: 1,
            text: Some("go".to_string()),
            images: Vec::new(),
            audios: Vec::new(),
            documents: Vec::new(),
            videos: Vec::new(),
            notices: Vec::new(),
            message_thread_id: topic,
            is_forum: true,
            replied_to: None,
        }
    }

    struct TempFile(std::path::PathBuf);

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Sends a final reply with text, one document, and follow-ups through
    /// the real reply path to a fake Bot API. Returns the requests seen and
    /// the last delivery record's statuses and message ids.
    async fn send_document_reply(
        reply_ctx: &super::ReplyContext,
        incoming: &crate::types::IncomingMessage,
    ) -> (
        Vec<crate::telegram::fake::Seen>,
        Vec<(DeliveryStatus, Option<i64>)>,
    ) {
        let fake = FakeTelegram::start().await;
        let context = BotContext::for_test(
            TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone()),
            zdx_engine::config::Config::default(),
            std::env::temp_dir(),
        );
        let file =
            TempFile(std::env::temp_dir().join(format!("zdx-reply-{}.html", uuid::Uuid::new_v4())));
        std::fs::write(&file.0, "<p>synthetic</p>").expect("write file");
        let thread_id = format!("telegram-reply-target-{}", uuid::Uuid::new_v4());
        let mut thread = Thread::with_id(thread_id.clone()).expect("thread");
        thread
            .append(&ThreadEvent::user_message("go"))
            .expect("append");
        let final_text = format!(
            "**Done**\n\n<followups><followup>Next step</followup></followups>\n<media>{}</media>",
            file.0.display()
        );
        send_final_response(
            &context,
            incoming,
            reply_ctx,
            None,
            &final_text,
            &thread_id,
            &[],
            &mut thread,
        )
        .await
        .expect("send final response");
        let events = load_thread_events(&thread_id).expect("events");
        let Some(ThreadEvent::Delivery { record, .. }) = events.last() else {
            panic!("delivery outcome recorded last");
        };
        let items = record
            .items
            .iter()
            .map(|item| (item.status, item.message_id))
            .collect();
        (fake.seen(), items)
    }

    /// The production failure: a `[worker update]` carries an internal id
    /// near `i64::MAX`. Its reply must quote nothing, so the text and the
    /// document are accepted in the right topic.
    #[tokio::test]
    async fn a_synthetic_update_reply_quotes_nothing() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let update = incoming(i64::MAX - 3, Some(7));
        let reply_ctx = build_reply_context(&update, false, true);
        assert_eq!(reply_ctx.reply_to_message_id, None);
        assert!(reply_ctx.cross_topic_reply_parameters.is_none());
        assert_eq!(reply_ctx.topic_id, Some(7));

        let (seen, items) = send_document_reply(&reply_ctx, &update).await;
        let methods: Vec<&str> = seen.iter().map(|s| s.method.as_str()).collect();
        assert_eq!(methods, ["sendMessage", "sendDocument"]);
        for request in &seen {
            assert!(!request.body.contains("reply_parameters"));
            assert!(!request.body.contains("reply_to_message_id"));
            assert!(!request.body.contains(&(i64::MAX - 3).to_string()));
        }
        assert!(seen[0].body.contains("\"message_thread_id\":7"));
        assert!(seen[1].body.contains("name=\"message_thread_id\"\r\n\r\n7"));
        assert!(seen[0].body.contains("fu:0"), "follow-ups on the text");
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|(status, _)| *status == DeliveryStatus::Sent)
        );
    }

    /// The fake refuses what Telegram refused, so the test above would catch
    /// a regression: the old context quoted the internal id.
    #[tokio::test]
    async fn the_old_synthetic_reply_target_is_refused() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let update = incoming(i64::MAX - 3, Some(7));
        let old = build_reply_context(&update, false, false);
        assert_eq!(old.reply_to_message_id, Some(i64::MAX - 3));

        let (seen, items) = send_document_reply(&old, &update).await;
        let methods: Vec<&str> = seen.iter().map(|s| s.method.as_str()).collect();
        // Both refused, then the partial-delivery summary.
        assert_eq!(methods, ["sendMessage", "sendDocument", "sendMessage"]);
        assert!(
            items
                .iter()
                .all(|(status, _)| *status == DeliveryStatus::Failed)
        );
    }

    #[tokio::test]
    async fn a_real_user_reply_keeps_its_target() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let message = incoming(42, Some(7));
        let reply_ctx = build_reply_context(&message, false, false);
        assert_eq!(reply_ctx.reply_to_message_id, Some(42));

        let (seen, items) = send_document_reply(&reply_ctx, &message).await;
        assert_eq!(seen.len(), 2);
        assert!(seen[0].body.contains("\"reply_to_message_id\":42"));
        assert!(
            seen[1]
                .body
                .contains("name=\"reply_to_message_id\"\r\n\r\n42")
        );
        assert!(
            items
                .iter()
                .all(|(status, _)| *status == DeliveryStatus::Sent)
        );
    }

    /// End to end through the real reply path: a resumed topic (aliased to
    /// its source thread) replies with its own file and a worker's file of the
    /// same name. Each document gets a link naming the *topic*, so feedback
    /// returns there, and the file's key, which the topic's artifact list (read
    /// through the alias, as the Mini App does) resolves to that exact file.
    #[tokio::test]
    async fn artifact_links_name_the_topic_and_resolve_to_the_exact_file() {
        use zdx_engine::core::artifacts::{artifact_dir_for_thread, list_thread_artifacts};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let mut config = zdx_engine::config::Config::default();
        config.telegram.server = Some(zdx_engine::config::TelegramServerConfig {
            enabled: true,
            port: 4141,
            mini_app_url: Some("https://t.me/zdx_test_bot/app".to_string()),
        });
        let context = BotContext::for_test(
            TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone()),
            config,
            std::env::temp_dir(),
        );

        let source = format!("resumed-source-{}", uuid::Uuid::new_v4());
        let worker = format!("worker-{}", uuid::Uuid::new_v4());
        let topic_thread = super::thread_id_for_chat(CHAT, Some(7));
        Thread::with_id(topic_thread.clone())
            .expect("topic thread")
            .set_alias(Some(source.clone()))
            .expect("alias");
        let own = artifact_dir_for_thread(Some(&source)).join("report.html");
        let theirs = artifact_dir_for_thread(Some(&worker)).join("report.html");
        for (path, body) in [(&own, "own"), (&theirs, "worker")] {
            std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
            std::fs::write(path, body).expect("write");
        }
        let final_text = format!(
            "Both reports.\n<media>{}</media>\n<media>{}</media>",
            own.display(),
            theirs.display()
        );
        let mut thread = Thread::with_id(source.clone()).expect("thread");
        thread
            .append(&ThreadEvent::assistant_message(final_text.clone()))
            .expect("append");
        let update = incoming(i64::MAX - 9, Some(7));
        let reply_ctx = build_reply_context(&update, false, true);
        send_final_response(
            &context,
            &update,
            &reply_ctx,
            None,
            &final_text,
            &source,
            &[],
            &mut thread,
        )
        .await
        .expect("send final response");

        // HTML with a configured Mini App goes out as file cards, not uploads.
        let seen = fake.seen();
        let methods: Vec<&str> = seen.iter().map(|s| s.method.as_str()).collect();
        assert_eq!(methods, ["sendMessage", "sendMessage", "sendMessage"]);
        let listed = list_thread_artifacts(&super::resolve_effective_thread_id(&topic_thread))
            .expect("artifacts");
        for (request, expected) in seen[1..].iter().zip([&own, &theirs]) {
            let body: serde_json::Value = serde_json::from_str(&request.body).expect("json");
            let row = &body["reply_markup"]["inline_keyboard"][0];
            let url = row[0]["url"].as_str().expect("open artifact link");
            let (_, param) = url.split_once("startapp=a_").expect("artifact link");
            let (key, thread) = param.split_once('_').expect("key and thread");
            assert_eq!(thread, topic_thread, "the receiving topic, not the source");
            let listed_row = listed.iter().find(|row| row.key == key).expect("listed");
            assert_eq!(listed_row.path, expected.to_string_lossy());
            assert!(!param.contains('/'), "no path in the link");
            assert_eq!(
                row[1]["callback_data"].as_str(),
                Some(format!("sf:{key}").as_str()),
                "Send file names the same listed file"
            );
        }
        // The link's thread maps back to the receiving topic, which is where
        // Mini App feedback for it is routed.
        assert_eq!(super::parse_topic_thread_id(&topic_thread), Some((CHAT, 7)));
    }

    #[test]
    fn general_routing_and_topic_starters_keep_their_targets() {
        // A real General message moved into a new topic: quoted across topics.
        let routed = build_reply_context(&incoming(50, Some(9)), true, false);
        assert_eq!(routed.reply_to_message_id, None);
        let cross = routed
            .cross_topic_reply_parameters
            .expect("cross-topic quote");
        assert_eq!((cross.message_id, cross.chat_id), (50, Some(CHAT)));
        assert_eq!(routed.topic_id, Some(9));

        // The topic's own opening message is not quoted.
        let starter = build_reply_context(&incoming(9, Some(9)), false, false);
        assert_eq!(starter.reply_to_message_id, None);
        assert!(starter.cross_topic_reply_parameters.is_none());

        // A synthetic prompt never quotes, even if marked as routed.
        let synthetic = build_reply_context(&incoming(i64::MAX, Some(9)), true, true);
        assert_eq!(synthetic.reply_to_message_id, None);
        assert!(synthetic.cross_topic_reply_parameters.is_none());
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::commands::format_whereami_message;
    use super::media::{is_audio_path, is_image_path, is_voice_note_path, parse_final_response};
    use super::orchestrator_topic_name;

    #[test]
    fn media_path_routing_classifies_by_extension() {
        assert!(is_image_path(Path::new("/tmp/a.png")));
        assert!(is_voice_note_path(Path::new("/tmp/a.ogg")));
        assert!(is_voice_note_path(Path::new("/tmp/A.OPUS")));
        assert!(is_audio_path(Path::new("/tmp/a.mp3")));
        assert!(is_audio_path(Path::new("/tmp/a.wav")));

        // Voice-note and generic-audio buckets stay disjoint.
        assert!(!is_audio_path(Path::new("/tmp/a.ogg")));
        assert!(!is_voice_note_path(Path::new("/tmp/a.mp3")));

        // Non-audio files fall through to the document sender.
        assert!(!is_voice_note_path(Path::new("/tmp/a.pdf")));
        assert!(!is_audio_path(Path::new("/tmp/a.pdf")));
    }

    #[test]
    fn parse_final_response_extracts_media_wrapper_format() {
        let parsed = parse_final_response("Done.\n<medias><media>/tmp/out.png</media></medias>");
        assert_eq!(parsed.text, "Done.");
        assert_eq!(parsed.media_paths, vec![PathBuf::from("/tmp/out.png")]);
    }

    #[test]
    fn parse_final_response_converts_markdown_text_to_telegram_html() {
        let parsed = parse_final_response(
            "**Answer:** use `git rebase`.\n\n- one\n- two\n\n<media>/tmp/out.png</media>",
        );
        assert_eq!(
            parsed.text,
            "<b>Answer:</b> use <code>git rebase</code>.\n\n- one\n- two"
        );
        assert_eq!(parsed.media_paths, vec![PathBuf::from("/tmp/out.png")]);
    }

    #[test]
    fn parse_final_response_extracts_text_followups_and_media() {
        let parsed = parse_final_response(
            "Done.\n<followups><followup>Open the report</followup></followups>\n\
             <medias><media>/tmp/out.png</media></medias>",
        );
        assert_eq!(parsed.text, "Done.");
        assert_eq!(parsed.followups, vec!["Open the report"]);
        assert_eq!(parsed.media_paths, vec![PathBuf::from("/tmp/out.png")]);
    }

    #[test]
    fn parse_final_response_extracts_multiple_media_entries() {
        let parsed = parse_final_response(
            "<medias><media>/tmp/one.png</media><media>/tmp/two.pdf</media></medias>",
        );
        assert!(parsed.text.is_empty());
        assert_eq!(
            parsed.media_paths,
            vec![PathBuf::from("/tmp/one.png"), PathBuf::from("/tmp/two.pdf")]
        );
    }

    #[test]
    fn parse_final_response_extracts_bare_media_entries_without_wrapper() {
        let parsed =
            parse_final_response("Done.\n<media>/tmp/one.png</media>\n<media>/tmp/two.pdf</media>");
        assert_eq!(parsed.text, "Done.");
        assert_eq!(
            parsed.media_paths,
            vec![PathBuf::from("/tmp/one.png"), PathBuf::from("/tmp/two.pdf")]
        );
    }

    /// Spellings of one file are sent once, in first-seen order; a `..` out
    /// of a symlinked directory names another file and is kept.
    #[cfg(unix)]
    #[test]
    fn parse_final_response_sends_equivalent_paths_once() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("zdx-media-{}", uuid::Uuid::new_v4()));
        let r = root.display();
        for dir in ["real/sub", "deep/target"] {
            std::fs::create_dir_all(root.join(dir)).expect("dirs");
        }
        for file in ["real/x.png", "x.png", "deep/x.png"] {
            std::fs::write(root.join(file), file).expect("write");
        }
        // `link/..` reads as the root but opens `deep`.
        symlink(root.join("deep/target"), root.join("link")).expect("symlink");
        let parsed = parse_final_response(&format!(
            "<media>{r}/real/x.png</media><media>{r}/real/./x.png</media>\
             <media>{r}/real//x.png</media><media>{r}/real/sub/../x.png</media>\
             <media>{r}/x.png</media><media>{r}/link/../x.png</media>"
        ));
        assert_eq!(
            parsed.media_paths,
            vec![
                PathBuf::from(format!("{r}/real/x.png")),
                PathBuf::from(format!("{r}/x.png")),
                PathBuf::from(format!("{r}/link/../x.png")),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_final_response_ignores_media_path_attribute_format() {
        let parsed = parse_final_response("<media path=\"/tmp/out.png\"/>");
        assert!(parsed.text.is_empty());
        assert!(parsed.media_paths.is_empty());
    }

    #[test]
    fn parse_final_response_ignores_plain_absolute_paths_without_media_xml() {
        let parsed = parse_final_response("/tmp/report.pdf");
        assert_eq!(parsed.text, "/tmp/report.pdf");
        assert!(parsed.media_paths.is_empty());
    }

    #[test]
    fn image_extension_routing_is_detected() {
        assert!(is_image_path(Path::new("/tmp/screenshot.webp")));
        assert!(!is_image_path(Path::new("/tmp/report.pdf")));
    }

    #[test]
    fn whereami_discovery_mode_hides_cwd_and_profile() {
        let msg = format_whereami_message(
            -1_001_234_567_890,
            Some(42),
            None,
            Path::new("/Users/secret/projects/private-thing"),
            true,
        );
        assert!(msg.contains("Chat ID: <code>-1001234567890</code>"));
        assert!(msg.contains("Topic ID: <code>42</code>"));
        assert!(msg.contains("chat not on bot allowlist"));
        assert!(msg.contains("allowlist_chat_ids"));
        assert!(msg.contains("zdx bot profile add"));
        // Discovery mode MUST NOT leak filesystem paths or profile info.
        assert!(!msg.contains("/Users/secret"));
        assert!(!msg.contains("CWD"));
        assert!(!msg.contains("Profile"));
    }

    #[test]
    fn whereami_allowlisted_unbound_shows_cwd_and_bind_hint() {
        let msg = format_whereami_message(
            -1_001_234_567_890,
            None,
            None,
            Path::new("/work/fallback-root"),
            false,
        );
        assert!(msg.contains("Profile: <code>none</code> (fallback root)"));
        assert!(msg.contains("CWD: <code>/work/fallback-root</code>"));
        assert!(msg.contains("Bind this chat with"));
        assert!(!msg.contains("Topic ID"));
    }

    #[test]
    fn whereami_allowlisted_bound_shows_profile_and_cwd() {
        let msg = format_whereami_message(
            -1_001_234_567_890,
            None,
            Some("zdx"),
            Path::new("/work/zdx"),
            false,
        );
        assert!(msg.contains("Profile: <code>zdx</code>"));
        assert!(msg.contains("CWD: <code>/work/zdx</code>"));
        assert!(!msg.contains("Bind this chat"));
    }

    /// Orchestrator topics carry `🎛️` the way worker mirrors carry `🛠`, and
    /// decorating an already-marked name is a no-op so the auto-title rename
    /// (and a hand-renamed topic) never stacks icons.
    #[test]
    fn orchestrator_topic_names_are_marked_once() {
        assert_eq!(
            orchestrator_topic_name("Add status icon"),
            "🎛️ Add status icon"
        );
        assert_eq!(
            orchestrator_topic_name("  Handoff 2026-09-11 00:12  "),
            "🎛️ Handoff 2026-09-11 00:12"
        );
        // Already marked, with and without the variation selector.
        assert_eq!(
            orchestrator_topic_name("🎛️ Add status icon"),
            "🎛️ Add status icon"
        );
        assert_eq!(
            orchestrator_topic_name("🎛 Add status icon"),
            "🎛 Add status icon"
        );
        // A worker mirror name is left alone by this helper.
        assert_eq!(orchestrator_topic_name("🛠 Worker abc"), "🎛️ 🛠 Worker abc");
    }
}
