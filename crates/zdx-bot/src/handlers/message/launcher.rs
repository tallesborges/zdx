//! General-topic thread launcher.
//!
//! Exposes the shared `[[favorites]]` list as a tappable menu in General:
//! favorite presets + a Custom picker create a new thread pre-set to a model,
//! and the launcher is kept as the last message in General.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use zdx_engine::config::{ModelFavorite, ThinkingLevel};
use zdx_engine::core::thread_persistence;

use super::{escape_html, thread_id_for_chat};
use crate::bot::context::BotContext;
use crate::telegram::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, TelegramClient};

/// Per-chat launcher tracking, used to keep it as the last message in General.
/// Presence in the map means a launcher is active for the chat.
#[derive(Default)]
pub(crate) struct LauncherState {
    /// Message id of the currently-posted launcher.
    message_id: i64,
    /// Monotonic counter to coalesce rapid repost requests per chat.
    generation: u64,
}

pub(crate) type LauncherMap = Arc<Mutex<HashMap<i64, LauncherState>>>;

pub(crate) fn new_launcher_map() -> LauncherMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Favorites the launcher can render for this chat: the configured
/// `[[favorites]]` minus any whose model isn't available (provider disabled).
/// Skipped favorites are logged. An empty result means the launcher shows only
/// the `🎛 Custom` button.
pub(crate) fn bot_visible_favorites(context: &BotContext) -> Vec<ModelFavorite> {
    let config = context.config();
    filter_available_favorites(&config.favorites, &config.subagent_available_models())
}

/// Pure filter: keep favorites whose model resolves to an available model id.
/// Logs each skipped favorite so a misconfigured preset is visible in the logs.
fn filter_available_favorites(
    favorites: &[ModelFavorite],
    available: &[String],
) -> Vec<ModelFavorite> {
    favorites
        .iter()
        .filter(|fav| {
            let ok = fav.model_available(available);
            if !ok {
                tracing::warn!(
                    alias = %fav.alias,
                    model = %fav.model,
                    "launcher: skipping favorite with unavailable model"
                );
            }
            ok
        })
        .cloned()
        .collect()
}

/// Create a new forum topic pre-set to `model` (and optional `thinking`), mark
/// it for auto-titling on the first real message, and post a "ready" prompt
/// into it. Returns the new topic id.
///
/// Model/thinking overrides are best-effort: a failure to persist them is
/// logged but does not fail topic creation (the topic still works on defaults).
pub(crate) async fn create_topic_with_model(
    context: &BotContext,
    chat_id: i64,
    model: &str,
    thinking: Option<ThinkingLevel>,
) -> Result<i64> {
    let topic_name = format!("Chat {}", chrono::Utc::now().format("%Y-%m-%d %H:%M"));
    let topic_id = context
        .client()
        .create_forum_topic(chat_id, &topic_name)
        .await
        .context("create forum topic for launcher")?;

    let thread_id = thread_id_for_chat(chat_id, Some(topic_id));
    if let Err(err) =
        thread_persistence::Thread::with_id(thread_id.clone()).and_then(|mut thread| {
            thread.set_model_override(Some(model.to_string()))?;
            if let Some(level) = thinking {
                thread.set_thinking_override(Some(level))?;
            }
            thread.set_pending_topic_title(true)
        })
    {
        tracing::warn!(
            chat_id,
            topic_id,
            thread_id = %thread_id,
            model,
            %err,
            "launcher: created topic but failed to apply model/thinking override"
        );
    }

    context
        .client()
        .send_message(chat_id, &ready_message(model), None, Some(topic_id))
        .await
        .context("post launcher ready message")?;

    tracing::info!(
        chat_id,
        topic_id,
        model,
        "launcher: created topic pre-set to model"
    );
    Ok(topic_id)
}

/// Create a new forum topic that *resumes* an existing source thread: the topic
/// aliases to the source so messages append to the original conversation. Named
/// after the source's title; does not set a pending auto-title. Returns the new
/// topic id.
pub(crate) async fn create_topic_resuming(
    context: &BotContext,
    chat_id: i64,
    source_thread_id: &str,
) -> Result<i64> {
    if source_thread_id.starts_with("telegram-") {
        anyhow::bail!("cannot resume a telegram topic thread: {source_thread_id}");
    }
    if !thread_persistence::thread_exists(source_thread_id) {
        anyhow::bail!("source thread does not exist: {source_thread_id}");
    }
    // Reject aliasing onto an already-aliased thread (single-hop only).
    if thread_persistence::read_thread_alias(source_thread_id)
        .ok()
        .flatten()
        .is_some()
    {
        anyhow::bail!("source thread is itself an alias: {source_thread_id}");
    }

    let title = thread_persistence::read_thread_title(source_thread_id)
        .ok()
        .flatten();
    let topic_name = title.clone().unwrap_or_else(|| {
        let short: String = source_thread_id.chars().take(8).collect();
        format!("Resumed {short}")
    });

    let topic_id = context
        .client()
        .create_forum_topic(chat_id, &topic_name)
        .await
        .context("create forum topic for resume")?;

    let thread_id = thread_id_for_chat(chat_id, Some(topic_id));
    thread_persistence::Thread::with_id(thread_id)
        .and_then(|mut thread| thread.set_alias(Some(source_thread_id.to_string())))
        .context("set alias on resumed topic")?;

    let heading = title.as_deref().map_or_else(
        || "🔄 Resumed thread — continue here.".to_string(),
        |t| format!("🔄 Resumed <b>{}</b> — continue here.", escape_html(t)),
    );
    context
        .client()
        .send_message(chat_id, &heading, None, Some(topic_id))
        .await
        .context("post resume ready message")?;

    tracing::info!(
        chat_id,
        topic_id,
        source_thread_id,
        "launcher: created resuming topic"
    );
    Ok(topic_id)
}

/// The "ready" prompt posted into a freshly launched topic.
fn ready_message(model: &str) -> String {
    format!(
        "🆕 New thread — model <code>{}</code>.\nSend your message here.",
        escape_html(model)
    )
}

/// Header text shown above the launcher keyboard. Lists each preset and the
/// model (+ thinking) it maps to, so the buttons can stay short (alias only).
fn launcher_header(favorites: &[ModelFavorite]) -> String {
    if favorites.is_empty() {
        return "🚀 <b>Thread launcher</b>\nNo favorites configured yet — use 🎛 Custom, or add <code>[[favorites]]</code> to your config.".to_string();
    }
    let mut lines = vec!["🚀 <b>Thread launcher</b>".to_string(), String::new()];
    for fav in favorites {
        let model = fav.model.rsplit(':').next().unwrap_or(&fav.model);
        let thinking = if fav.thinking == ThinkingLevel::Off {
            String::new()
        } else {
            format!(" · {}", fav.thinking.display_name())
        };
        lines.push(format!(
            "<b>{}</b> → <code>{}</code>{thinking}",
            escape_html(&fav.alias),
            escape_html(model)
        ));
    }
    lines.push(String::new());
    lines.push("<i>Tap a preset, or use 🎛 Custom / 🔄 Continue.</i>".to_string());
    lines.join("\n")
}

/// Build the General launcher keyboard: one button per available favorite
/// (callback `nt:p:{alias}`) plus a `🎛 Custom` button (`nt:custom`).
fn build_launcher_keyboard(favorites: &[ModelFavorite]) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = favorites
        .chunks(3)
        .map(|chunk| {
            chunk
                .iter()
                .map(|fav| InlineKeyboardButton {
                    text: fav.alias.clone(),
                    callback_data: Some(format!("nt:p:{}", fav.alias)),
                    url: None,
                })
                .collect()
        })
        .collect();

    rows.push(vec![
        InlineKeyboardButton {
            text: "🎛 Custom".to_string(),
            callback_data: Some("nt:custom".to_string()),
            url: None,
        },
        InlineKeyboardButton {
            text: "🔄 Continue".to_string(),
            callback_data: Some("nt:resume".to_string()),
            url: None,
        },
    ]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

/// Newest-first threads that can be resumed in this chat's project: top-level
/// user threads (excludes `telegram-*` topic threads) whose `root_path` matches
/// the chat's resolved project root, capped to `cap`.
fn resumable_threads(
    all: Vec<zdx_engine::core::thread_persistence::ThreadSummary>,
    root: &std::path::Path,
    cap: usize,
) -> Vec<zdx_engine::core::thread_persistence::ThreadSummary> {
    let root_str = root.display().to_string();
    all.into_iter()
        .filter(|t| !t.id.starts_with("telegram-"))
        .filter(|t| t.root_path.as_deref() == Some(root_str.as_str()))
        .take(cap)
        .collect()
}

/// Compact relative-age label for a thread's last-modified time.
fn relative_time(modified: Option<std::time::SystemTime>) -> String {
    let Some(elapsed) = modified.and_then(|m| m.elapsed().ok()) else {
        return "?".to_string();
    };
    let secs = elapsed.as_secs();
    if secs < 60 {
        "now".to_string()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Build the resume-thread picker keyboard. Each button resumes a source thread
/// (`nt:r:{id}`); a trailing Cancel closes it.
fn build_resume_keyboard(
    threads: &[zdx_engine::core::thread_persistence::ThreadSummary],
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = threads
        .iter()
        .map(|t| {
            let title = t.display_title();
            let title: String = title.chars().take(40).collect();
            vec![InlineKeyboardButton {
                text: format!("{title} · {}", relative_time(t.modified)),
                callback_data: Some(format!("nt:r:{}", t.id)),
                url: None,
            }]
        })
        .collect();

    rows.push(vec![InlineKeyboardButton {
        text: "← Back".to_string(),
        callback_data: Some("nt:back".to_string()),
        url: None,
    }]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

/// Send the launcher keyboard and return the sent message id.
async fn send_launcher(
    context: &BotContext,
    chat_id: i64,
    reply_to_message_id: Option<i64>,
) -> Result<i64> {
    let favorites = bot_visible_favorites(context);
    let keyboard = build_launcher_keyboard(&favorites);
    let msg = context
        .client()
        .send_message_with_markup(
            chat_id,
            &launcher_header(&favorites),
            reply_to_message_id,
            None,
            &keyboard,
        )
        .await
        .context("post launcher keyboard")?;
    Ok(msg.id)
}

/// Re-render the launcher menu into an existing message (used to return from a
/// picker to the launcher). Keeps the same message id, so tracking stays valid.
pub(crate) async fn render_launcher(
    context: &BotContext,
    chat_id: i64,
    message_id: i64,
) -> Result<()> {
    let favorites = bot_visible_favorites(context);
    let keyboard = build_launcher_keyboard(&favorites);
    context
        .client()
        .edit_message_text(
            chat_id,
            message_id,
            &launcher_header(&favorites),
            Some(&keyboard),
        )
        .await
        .context("restore launcher")?;
    Ok(())
}

/// Post the launcher into General and track it so later messages can keep it at
/// the bottom. General-only: callers gate on `is_forum && message_thread_id.is_none()`.
pub(crate) async fn post_launcher(
    context: &BotContext,
    chat_id: i64,
    reply_to_message_id: Option<i64>,
) -> Result<()> {
    let message_id = send_launcher(context, chat_id, reply_to_message_id).await?;
    let mut map = context.launcher_map().lock().await;
    let state = map.entry(chat_id).or_default();
    state.generation += 1;
    state.message_id = message_id;
    Ok(())
}

/// Debounce window for coalescing rapid General messages into a single repost.
const LAUNCHER_REPOST_DEBOUNCE: Duration = Duration::from_millis(700);

/// If a launcher is tracked for this General chat, move it back to the bottom
/// (delete + repost). Coalesces rapid bursts via a per-chat generation counter
/// and no-ops when no launcher is active. Delete failures are non-fatal.
pub(crate) fn schedule_repost(context: &Arc<BotContext>, chat_id: i64) {
    let context = Arc::clone(context);
    tokio::spawn(async move {
        let my_gen = {
            let mut map = context.launcher_map().lock().await;
            let Some(state) = map.get_mut(&chat_id) else {
                return;
            };
            state.generation += 1;
            state.generation
        };

        tokio::time::sleep(LAUNCHER_REPOST_DEBOUNCE).await;

        // Only the most recently scheduled repost for this chat proceeds.
        let old_id = {
            let map = context.launcher_map().lock().await;
            match map.get(&chat_id) {
                Some(state) if state.generation == my_gen => state.message_id,
                _ => return,
            }
        };

        if let Err(err) = context.client().delete_message(chat_id, old_id).await {
            tracing::warn!(chat_id, old_id, %err, "launcher: failed to delete old launcher");
        }

        match send_launcher(context.as_ref(), chat_id, None).await {
            Ok(new_id) => {
                let superseded = {
                    let mut map = context.launcher_map().lock().await;
                    match map.get_mut(&chat_id) {
                        Some(state) if state.generation == my_gen => {
                            state.message_id = new_id;
                            false
                        }
                        _ => true,
                    }
                };
                if superseded {
                    // A newer repost won the race; drop the duplicate we just posted.
                    let _ = context.client().delete_message(chat_id, new_id).await;
                }
            }
            Err(err) => tracing::warn!(chat_id, %err, "launcher: failed to repost launcher"),
        }
    });
}

/// Handle `nt:` launcher callbacks. `rest` is the callback data after `nt:`.
pub(crate) async fn handle_callback(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
    rest: &str,
) {
    let Some(msg) = callback.message.as_ref() else {
        let _ = client
            .answer_callback_query(&callback.id, Some("No message context"))
            .await;
        return;
    };
    let chat_id = msg.chat.id;

    if let Some(alias) = rest.strip_prefix("p:") {
        // Resolve the preset from current config at click time so stale buttons
        // (e.g. after a restart or config edit) fail gracefully.
        let favorite = bot_visible_favorites(context)
            .into_iter()
            .find(|fav| fav.alias == alias);
        let Some(favorite) = favorite else {
            let _ = client
                .answer_callback_query(&callback.id, Some("Preset no longer configured"))
                .await;
            return;
        };

        match create_topic_with_model(context, chat_id, &favorite.model, Some(favorite.thinking))
            .await
        {
            Ok(_) => {
                let _ = client
                    .answer_callback_query(&callback.id, Some("New thread ready ✓"))
                    .await;
            }
            Err(err) => {
                tracing::error!(chat_id, alias, %err, "launcher: failed to create preset topic");
                let _ = client
                    .answer_callback_query(&callback.id, Some("Couldn't create the topic"))
                    .await;
            }
        }
    } else if rest == "custom" {
        // Open the provider → model picker targeting a brand-new thread.
        let keyboard = super::build_provider_keyboard(context, super::ModelPickerScope::NewThread);
        if let Err(err) = client
            .edit_message_text(
                chat_id,
                msg.id,
                "Pick a model for a new thread:",
                Some(&keyboard),
            )
            .await
        {
            tracing::warn!(chat_id, %err, "launcher: failed to open custom picker");
        }
        let _ = client.answer_callback_query(&callback.id, None).await;
    } else if rest == "resume" {
        let root = context.root_for_chat(chat_id).root;
        let all = zdx_engine::core::thread_persistence::list_threads().unwrap_or_default();
        let threads = resumable_threads(all, &root, 8);
        if threads.is_empty() {
            let _ = client
                .answer_callback_query(&callback.id, Some("No recent threads for this project yet"))
                .await;
            return;
        }
        let keyboard = build_resume_keyboard(&threads);
        if let Err(err) = client
            .edit_message_text(chat_id, msg.id, "Resume a thread:", Some(&keyboard))
            .await
        {
            tracing::warn!(chat_id, %err, "launcher: failed to open resume picker");
        }
        let _ = client.answer_callback_query(&callback.id, None).await;
    } else if let Some(source_id) = rest.strip_prefix("r:") {
        match create_topic_resuming(context, chat_id, source_id).await {
            Ok(_) => {
                let _ = client
                    .answer_callback_query(&callback.id, Some("Resumed ✓"))
                    .await;
            }
            Err(err) => {
                tracing::warn!(chat_id, source_id, %err, "launcher: failed to resume thread");
                let _ = client
                    .answer_callback_query(&callback.id, Some("That thread no longer exists"))
                    .await;
            }
        }
    } else if rest == "back" {
        if let Err(err) = render_launcher(context, chat_id, msg.id).await {
            tracing::warn!(chat_id, %err, "launcher: failed to restore launcher");
        }
        let _ = client.answer_callback_query(&callback.id, None).await;
    } else {
        let _ = client.answer_callback_query(&callback.id, None).await;
        tracing::warn!(?rest, "launcher: unknown nt callback");
    }
}

#[cfg(test)]
mod tests {
    use zdx_engine::config::ThinkingLevel;

    use super::*;

    fn fav(alias: &str, model: &str) -> ModelFavorite {
        ModelFavorite {
            alias: alias.to_string(),
            model: model.to_string(),
            thinking: ThinkingLevel::Off,
        }
    }

    #[test]
    fn filter_drops_unavailable_and_keeps_valid() {
        let available = vec![
            "anthropic:claude-opus-4-8".to_string(),
            "gemini:gemini-3.5-flash".to_string(),
        ];
        let favorites = vec![
            // Bare id, prefixed availability entry — must still match.
            fav("Smart", "claude-opus-4-8"),
            // Provider disabled — must be dropped.
            fav("Gone", "openai:gpt-5.5"),
            // Prefixed id matching exactly.
            fav("Fast", "gemini:gemini-3.5-flash"),
        ];

        let kept = filter_available_favorites(&favorites, &available);

        let aliases: Vec<&str> = kept.iter().map(|f| f.alias.as_str()).collect();
        assert_eq!(aliases, vec!["Smart", "Fast"]);
    }

    #[test]
    fn filter_empty_when_no_favorites_available() {
        let available = vec!["anthropic:claude-opus-4-8".to_string()];
        let favorites = vec![fav("Gone", "openai:gpt-5.5")];

        assert!(filter_available_favorites(&favorites, &available).is_empty());
    }

    #[test]
    fn ready_message_wraps_model_in_code_and_escapes() {
        let msg = ready_message("openai:gpt-5.5");
        assert!(msg.contains("<code>openai:gpt-5.5</code>"));
        assert!(msg.contains("Send your message here."));

        // Any HTML-significant characters in a model id must be escaped.
        let escaped = ready_message("a<b>&c");
        assert!(escaped.contains("a&lt;b&gt;&amp;c"));
    }

    #[test]
    fn launcher_keyboard_has_preset_and_custom_buttons() {
        let favorites = vec![
            fav("Fast", "gemini:gemini-3.5-flash"),
            fav("Smart", "claude-cli:claude-opus-4-8"),
        ];
        let keyboard = build_launcher_keyboard(&favorites);
        let buttons: Vec<&InlineKeyboardButton> =
            keyboard.inline_keyboard.iter().flatten().collect();

        let data: Vec<&str> = buttons
            .iter()
            .filter_map(|b| b.callback_data.as_deref())
            .collect();
        assert!(data.contains(&"nt:p:Fast"));
        assert!(data.contains(&"nt:p:Smart"));
        assert!(data.contains(&"nt:custom"));

        // Telegram caps callback data at 64 bytes.
        for d in &data {
            assert!(d.len() <= 64, "callback data too long: {d}");
        }
    }

    #[test]
    fn launcher_keyboard_shows_only_custom_when_no_favorites() {
        let keyboard = build_launcher_keyboard(&[]);
        let data: Vec<&str> = keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .filter_map(|b| b.callback_data.as_deref())
            .collect();
        assert_eq!(data, vec!["nt:custom", "nt:resume"]);
    }

    #[test]
    fn resumable_threads_filters_by_root_and_excludes_telegram() {
        use std::path::PathBuf;

        use zdx_engine::core::thread_persistence::ThreadSummary;

        let root = PathBuf::from("/proj/a");
        let summary = |id: &str, root_path: Option<&str>| ThreadSummary {
            id: id.to_string(),
            root_path: root_path.map(str::to_string),
            ..Default::default()
        };
        let all = vec![
            summary("uuid-keep-1", Some("/proj/a")),
            summary("telegram-1-topic-2", Some("/proj/a")), // excluded: telegram
            summary("uuid-other", Some("/proj/b")),         // excluded: wrong root
            summary("uuid-noroot", None),                   // excluded: no root
            summary("uuid-keep-2", Some("/proj/a")),
        ];

        let kept = resumable_threads(all, &root, 8);
        let ids: Vec<&str> = kept.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["uuid-keep-1", "uuid-keep-2"]);
    }

    #[test]
    fn resume_callback_data_within_telegram_limit() {
        let id = "123e4567-e89b-12d3-a456-426614174000"; // UUID
        let data = format!("nt:r:{id}");
        assert!(data.len() <= 64, "callback data too long: {data}");
    }
}
