use std::path::Path;

use anyhow::Result;
use commands::{handle_exit_command, handle_general_forum_commands, handle_thread_setup_commands};
use status::{discard_turn_status, finalize_preprocessing_cancelled, setup_preprocessing_status};
use tokio_util::sync::CancellationToken;
use turn::run_agent_turn;
use zdx_engine::config::ThinkingLevel;
use zdx_engine::core::thread_persistence;

use crate::bot::context::BotContext;
use crate::ingest::{self, AllowlistConfig};
use crate::telegram::{InlineKeyboardMarkup, Message, ReplyParameters};

mod commands;
mod media;
mod response;
mod status;
mod turn;

pub(crate) use commands::{build_models_keyboard, build_provider_keyboard, models_for_provider};

/// Groups the reply-targeting fields that travel together through the turn pipeline.
struct ReplyContext {
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    cross_topic_reply_parameters: Option<ReplyParameters>,
}

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) async fn handle_message(context: &BotContext, message: Message) -> Result<()> {
    let bot_config = context.config();
    let synthetic_topic_routed_from_general = message.synthetic_topic_routed_from_general;
    let provisional_status = if message_has_audio(&message) {
        Some(
            setup_preprocessing_status(context, &message, synthetic_topic_routed_from_general)
                .await,
        )
    } else {
        None
    };
    let allowlist = AllowlistConfig {
        user_ids: context.allowlist_user_ids(),
        chat_ids: context.allowlist_chat_ids(),
    };
    let Some(incoming) = parse_message_with_status(
        context,
        allowlist,
        &bot_config,
        message,
        provisional_status.as_ref(),
    )
    .await?
    else {
        cleanup_provisional_status(context, None, provisional_status).await;
        return Ok(());
    };

    let reply_ctx = build_reply_context(&incoming, synthetic_topic_routed_from_general);

    if handle_pre_agent_commands(context, &incoming, &reply_ctx).await? {
        cleanup_provisional_status(context, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    tracing::info!(
        user_id = incoming.user_id,
        chat_id = incoming.chat_id,
        topic_id = ?reply_ctx.topic_id,
        "Accepted message",
    );

    let thread_id = thread_id_for_chat(incoming.chat_id, reply_ctx.topic_id);
    if handle_thread_setup_commands(context, &incoming, &reply_ctx, &thread_id).await? {
        cleanup_provisional_status(context, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    if crate::staging::handle_staging_flow(
        context,
        &incoming,
        reply_ctx.reply_to_message_id,
        reply_ctx.topic_id,
        &thread_id,
    )
    .await?
    {
        cleanup_provisional_status(context, Some(incoming.chat_id), provisional_status).await;
        return Ok(());
    }

    run_agent_turn(
        context,
        incoming,
        reply_ctx,
        &thread_id,
        synthetic_topic_routed_from_general,
        provisional_status,
    )
    .await
}

async fn parse_message_with_status(
    context: &BotContext,
    allowlist: AllowlistConfig<'_>,
    bot_config: &zdx_engine::config::Config,
    message: Message,
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

fn build_reply_context(
    incoming: &crate::types::IncomingMessage,
    synthetic_topic_routed_from_general: bool,
) -> ReplyContext {
    let reply_to_message_id = if synthetic_topic_routed_from_general
        || incoming.message_thread_id == Some(incoming.message_id)
    {
        None
    } else {
        Some(incoming.message_id)
    };
    let topic_id = incoming.message_thread_id;
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
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
) -> Result<bool> {
    Ok(
        handle_general_forum_commands(context, incoming, reply_ctx.reply_to_message_id).await?
            || handle_exit_command(context, incoming, reply_ctx.reply_to_message_id).await?,
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
}

struct SpawnRequest<'a> {
    worktree_root: &'a std::path::Path,
    thread_id: &'a str,
    thread: &'a zdx_engine::core::thread_persistence::Thread,
    messages: Vec<zdx_engine::providers::ChatMessage>,
    config: &'a zdx_engine::config::Config,
}

struct StatusSnapshot<'a> {
    model_id: &'a str,
    model_override: Option<&'a str>,
    thinking: ThinkingLevel,
    thinking_override: Option<ThinkingLevel>,
    profile_name: Option<&'a str>,
    thread_id: &'a str,
    root_path: &'a Path,
    branch: Option<&'a str>,
    cumulative_usage: thread_persistence::Usage,
    latest_usage: thread_persistence::Usage,
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
    format!(
        "❌ Request failed.\n\n<blockquote><code>{}</code></blockquote>",
        escape_html(&compact)
    )
}

pub(crate) fn thread_id_for_chat(chat_id: i64, message_thread_id: Option<i64>) -> String {
    match message_thread_id {
        Some(topic_id) => format!("telegram-{chat_id}-topic-{topic_id}"),
        None => format!("telegram-{chat_id}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::commands::format_whereami_message;
    use super::media::{is_audio_path, is_image_path, is_voice_note_path, parse_final_response};

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
}
