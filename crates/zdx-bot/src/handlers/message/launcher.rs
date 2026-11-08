//! General-topic thread launcher.
//!
//! Exposes the shared `[[model_modes]]` list as a tappable menu in General:
//! model modes + a Custom picker create a new thread pre-set to a model, as an
//! agent or an orchestrator (the `New as:` switch), `✖ Dismiss` closes the
//! launcher, and an open launcher is kept as the last message in General.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use zdx_engine::config::{Config, ModelMode, ThinkingLevel};
use zdx_engine::core::thread_persistence;

use super::{escape_html, orchestrator_topic_name, post_thread_header, thread_id_for_chat};
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

/// Modes the launcher can render for this chat: the configured
/// `[[model_modes]]` minus any whose primary isn't available (provider
/// disabled). Skipped modes are logged. An empty result means the launcher
/// shows only the `🎛 Custom` button.
pub(crate) fn bot_visible_modes(context: &BotContext, chat_id: i64) -> Vec<ModelMode> {
    let config = context.config_for_chat(chat_id);
    filter_available_modes(&config.model_modes, &config.subagent_available_models())
}

/// Pure filter: keep modes whose primary resolves to an available model id.
/// Logs each skipped mode so a misconfigured tier is visible in the logs.
fn filter_available_modes(modes: &[ModelMode], available: &[String]) -> Vec<ModelMode> {
    modes
        .iter()
        .filter(|mode| {
            let ok = mode.model_available(available);
            if !ok {
                tracing::warn!(
                    mode = %mode.name,
                    primary = %mode.primary,
                    "launcher: skipping mode with unavailable primary model"
                );
            }
            ok
        })
        .cloned()
        .collect()
}

/// What a launcher-created topic runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchRole {
    /// An ordinary coding-agent topic.
    Agent,
    /// An orchestrator home base (persistent `orchestrator` profile).
    Orchestrator,
}

impl LaunchRole {
    fn label(self) -> &'static str {
        match self {
            Self::Agent => "🤖 Agent",
            Self::Orchestrator => "🧭 Orchestrator",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Agent => Self::Orchestrator,
            Self::Orchestrator => Self::Agent,
        }
    }

    /// Callback code for `nt:as:{code}`.
    fn code(self) -> &'static str {
        match self {
            Self::Agent => "a",
            Self::Orchestrator => "o",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "a" => Some(Self::Agent),
            "o" => Some(Self::Orchestrator),
            _ => None,
        }
    }

    /// Callback data for a mode button: `nt:p:{name}` (agent) or
    /// `nt:op:{name}` (orchestrator).
    fn mode_data(self, name: &str) -> String {
        match self {
            Self::Agent => format!("nt:p:{name}"),
            Self::Orchestrator => format!("nt:op:{name}"),
        }
    }

    fn custom_data(self) -> &'static str {
        match self {
            Self::Agent => "nt:custom",
            Self::Orchestrator => "nt:ocustom",
        }
    }

    pub(crate) fn picker_scope(self) -> super::ModelPickerScope {
        match self {
            Self::Agent => super::ModelPickerScope::NewThread,
            Self::Orchestrator => super::ModelPickerScope::NewOrchestrator,
        }
    }
}

/// The role a launcher starts on: `Orchestrator` in a chat whose General
/// messages create orchestrators (`telegram.profiles.<name>.orchestrator`),
/// `Agent` otherwise. The launcher's switch changes it per message.
pub(crate) fn default_role(context: &BotContext, chat_id: i64) -> LaunchRole {
    if context.orchestrator_enabled_for_chat(chat_id) {
        LaunchRole::Orchestrator
    } else {
        LaunchRole::Agent
    }
}

/// The config a new topic of `role` starts with before any model is pinned:
/// the chat's config, plus `[subagents.overrides.orchestrator]` for an
/// orchestrator. Shown as the launcher's and the picker's default.
pub(crate) fn launch_config(context: &BotContext, chat_id: i64, role: LaunchRole) -> Config {
    let mut config = context.config_for_chat(chat_id);
    if role == LaunchRole::Orchestrator {
        zdx_engine::subagents::apply_orchestrator_override(&mut config);
    }
    config
}

/// Create a new forum topic of `role` pre-set to a unified `model` spec, mark
/// it for auto-titling on the first real message, and post its pinned thread
/// header. Returns the new topic id.
///
/// The model is pinned as the thread's own override, the last layer of model
/// resolution, so it beats the chat default and the orchestrator override.
/// Nothing outside the new thread changes.
///
/// For an agent, the model override is best-effort: a failure is logged and
/// the topic works on defaults. An orchestrator fails closed: the topic is
/// deleted and an error is returned, since a message there would otherwise run
/// the default coding agent, or the orchestrator on another model than the one
/// picked.
pub(crate) async fn create_topic_with_model(
    context: &BotContext,
    chat_id: i64,
    model: &str,
    role: LaunchRole,
) -> Result<i64> {
    let topic_name = format!("Chat {}", chrono::Utc::now().format("%Y-%m-%d %H:%M"));
    let topic_name = if role == LaunchRole::Orchestrator {
        orchestrator_topic_name(&topic_name)
    } else {
        topic_name
    };
    let topic_id = context
        .client()
        .create_forum_topic(chat_id, &topic_name)
        .await
        .context("create forum topic for launcher")?;

    let thread_id = thread_id_for_chat(chat_id, Some(topic_id));
    let init = thread_persistence::Thread::with_id(thread_id.clone()).and_then(|mut thread| {
        if role == LaunchRole::Orchestrator {
            thread.set_persistent_profile(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME)?;
        }
        thread.set_model_override(Some(model.to_string()))?;
        thread.set_pending_topic_title(true)
    });
    if let Err(err) = init {
        if role == LaunchRole::Orchestrator {
            abandon_orchestrator_topic(context, chat_id, topic_id).await;
            return Err(err.context("set up launcher orchestrator topic"));
        }
        tracing::warn!(
            chat_id,
            topic_id,
            thread_id = %thread_id,
            model,
            err = %format!("{err:#}"),
            "launcher: created topic but failed to apply model override"
        );
    }

    if let Err(err) = post_thread_header(context, chat_id, topic_id, &thread_id).await {
        tracing::warn!(
            chat_id,
            topic_id,
            thread_id = %thread_id,
            err = %format!("{err:#}"),
            "launcher: created topic but failed to post thread header"
        );
    }

    tracing::info!(
        chat_id,
        topic_id,
        model,
        ?role,
        "launcher: created topic pre-set to model"
    );
    Ok(topic_id)
}

/// Remove a launcher orchestrator topic whose profile or model could not be
/// saved, so no message can run there on the wrong agent or model. When
/// Telegram refuses the delete (the bot lacks `can_delete_messages`), the topic
/// gets a warning not to use it.
async fn abandon_orchestrator_topic(context: &BotContext, chat_id: i64, topic_id: i64) {
    let Err(err) = context.client().delete_forum_topic(chat_id, topic_id).await else {
        return;
    };
    tracing::warn!(chat_id, topic_id, err = %format!("{err:#}"), "launcher: failed to delete orchestrator topic after init failure");
    if let Err(send_err) = context
        .client()
        .send_message(
            chat_id,
            "⚠️ I couldn't set up this topic as an orchestrator on the picked model. Don't use it; open a new one from /launcher.",
            None,
            Some(topic_id),
        )
        .await
    {
        tracing::warn!(chat_id, topic_id, err = %format!("{send_err:#}"), "launcher: failed to warn about orchestrator init failure");
    }
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

    if let Err(err) = post_thread_header(context, chat_id, topic_id, source_thread_id).await {
        tracing::warn!(
            chat_id,
            topic_id,
            source_thread_id,
            err = %format!("{err:#}"),
            "launcher: created resumed topic but failed to post thread header"
        );
    }

    tracing::info!(
        chat_id,
        topic_id,
        source_thread_id,
        "launcher: created resuming topic"
    );
    Ok(topic_id)
}

/// Header text shown above the launcher keyboard. Leads with the role new
/// topics start as and that role's default model, then lists each mode, what
/// it is for, and the model (+ thinking) it maps to, so the buttons can stay
/// short (name only).
fn launcher_header(
    modes: &[ModelMode],
    role: LaunchRole,
    default_model: &str,
    default_thinking: ThinkingLevel,
) -> String {
    let default_id = default_model.rsplit(':').next().unwrap_or(default_model);
    let thinking = if default_thinking == ThinkingLevel::Off {
        String::new()
    } else {
        format!(" · {}", default_thinking.display_name())
    };
    let role_line = format!("New topics start as: <b>{}</b>", role.label());
    let default_line = format!(
        "Default: <code>{}</code>{thinking}",
        escape_html(default_id)
    );

    if modes.is_empty() {
        return format!(
            "🚀 <b>Thread launcher</b>\n{role_line}\n{default_line}\nNo model modes configured yet — use 🎛 Custom, or add <code>[[model_modes]]</code> to your config."
        );
    }
    let mut lines = vec![
        "🚀 <b>Thread launcher</b>".to_string(),
        role_line,
        default_line,
        String::new(),
    ];
    for mode in modes {
        let primary_id = mode.primary.rsplit(':').next().unwrap_or(&mode.primary);
        let thinking = if mode.thinking == ThinkingLevel::Off {
            String::new()
        } else {
            format!(" · {}", mode.thinking.display_name())
        };
        let description = if mode.description.trim().is_empty() {
            String::new()
        } else {
            format!(" — {}", escape_html(mode.description.trim()))
        };
        lines.push(format!(
            "<b>{}</b> → <code>{}</code>{thinking}{description}",
            escape_html(&mode.name),
            escape_html(primary_id)
        ));
    }
    lines.push(String::new());
    lines.push(
        "<i>Tap a mode, or use 🎛 Custom / 🔄 Continue. The first button switches agent / orchestrator.</i>"
            .to_string(),
    );
    lines.join("\n")
}

/// Build the launcher keyboard for `role`: the role switch (`nt:as:{a|o}`,
/// labelled with the current role), one button per available mode
/// (`nt:p:{name}` or `nt:op:{name}`), `🎛 Custom` (`nt:custom` or
/// `nt:ocustom`), `🔄 Continue` (`nt:resume`), and `✖ Dismiss` (`nt:x`).
fn build_launcher_keyboard(modes: &[ModelMode], role: LaunchRole) -> InlineKeyboardMarkup {
    let mut rows = vec![vec![InlineKeyboardButton::callback(
        format!("New as: {} ⇄", role.label()),
        format!("nt:as:{}", role.other().code()),
    )]];
    rows.extend(modes.chunks(3).map(|chunk| {
        chunk
            .iter()
            .map(|mode| {
                InlineKeyboardButton::callback(mode.name.clone(), role.mode_data(&mode.name))
            })
            .collect::<Vec<_>>()
    }));
    rows.push(vec![
        InlineKeyboardButton::callback("🎛 Custom", role.custom_data()),
        InlineKeyboardButton::callback("🔄 Continue", "nt:resume"),
    ]);
    rows.push(vec![InlineKeyboardButton::callback("✖ Dismiss", "nt:x")]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

/// Header and keyboard of the launcher menu for `role`.
fn launcher_view(
    context: &BotContext,
    chat_id: i64,
    role: LaunchRole,
) -> (String, InlineKeyboardMarkup) {
    let modes = bot_visible_modes(context, chat_id);
    let config = launch_config(context, chat_id, role);
    let header = launcher_header(&modes, role, &config.model, config.thinking_level);
    (header, build_launcher_keyboard(&modes, role))
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
            vec![InlineKeyboardButton::callback(
                format!("{title} · {}", relative_time(t.modified)),
                format!("nt:r:{}", t.id),
            )]
        })
        .collect();

    rows.push(vec![InlineKeyboardButton::callback("← Back", "nt:back")]);

    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

/// Send the launcher keyboard into General and return the sent message id.
async fn send_launcher(
    context: &BotContext,
    chat_id: i64,
    reply_to_message_id: Option<i64>,
) -> Result<i64> {
    let (header, keyboard) = launcher_view(context, chat_id, default_role(context, chat_id));
    let msg = context
        .client()
        .send_message_with_markup(chat_id, &header, reply_to_message_id, None, &keyboard)
        .await
        .context("post launcher keyboard")?;
    Ok(msg.id)
}

/// Re-render the launcher menu for `role` into an existing message (returning
/// from a picker, or switching the role). Keeps the same message id, so
/// tracking stays valid.
pub(crate) async fn render_launcher(
    context: &BotContext,
    chat_id: i64,
    message_id: i64,
    role: LaunchRole,
) -> Result<()> {
    let (header, keyboard) = launcher_view(context, chat_id, role);
    context
        .client()
        .edit_message_text(chat_id, message_id, &header, Some(&keyboard))
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
            tracing::warn!(chat_id, old_id, err = %format!("{err:#}"), "launcher: failed to delete old launcher");
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
            Err(err) => {
                tracing::warn!(chat_id, err = %format!("{err:#}"), "launcher: failed to repost launcher");
            }
        }
    });
}

/// Close a launcher message. Stops tracking it when it is the chat's current
/// launcher, so no repost brings it back (an in-flight repost sees the missing
/// entry and drops what it posted); `/launcher` opens a new one. The message is
/// deleted; when Telegram refuses the delete (a bot cannot delete its messages
/// after 48 hours), only its buttons are removed.
async fn dismiss_launcher(context: &BotContext, chat_id: i64, message_id: i64) {
    {
        let mut map = context.launcher_map().lock().await;
        if map
            .get(&chat_id)
            .is_some_and(|state| state.message_id == message_id)
        {
            map.remove(&chat_id);
        }
    }

    let Err(err) = context.client().delete_message(chat_id, message_id).await else {
        return;
    };
    tracing::warn!(chat_id, message_id, err = %format!("{err:#}"), "launcher: failed to delete dismissed launcher; removing its buttons");
    let empty = InlineKeyboardMarkup {
        inline_keyboard: Vec::new(),
    };
    if let Err(err) = context
        .client()
        .edit_message_reply_markup(chat_id, message_id, &empty)
        .await
    {
        tracing::warn!(chat_id, message_id, err = %format!("{err:#}"), "launcher: failed to remove dismissed launcher buttons");
    }
}

/// Threads the orchestrator service runs as workers right now.
async fn managed_worker_ids(context: &BotContext) -> std::collections::HashSet<String> {
    context
        .orchestrator()
        .workers(None)
        .await
        .into_iter()
        .map(|worker| worker.thread_id)
        .collect()
}

/// `nt:p:{name}` / `nt:op:{name}`: open a new topic of `role` pre-set to the
/// mode's primary model (which carries its thinking level).
async fn open_mode_topic(
    context: &BotContext,
    client: &TelegramClient,
    callback: &CallbackQuery,
    chat_id: i64,
    name: &str,
    role: LaunchRole,
) {
    // Resolve the mode from current config at click time so stale buttons
    // (e.g. after a restart or config edit) fail gracefully.
    let mode = bot_visible_modes(context, chat_id)
        .into_iter()
        .find(|mode| mode.name == name);
    let Some(mode) = mode else {
        let _ = client
            .answer_callback_query(&callback.id, Some("Mode no longer configured"))
            .await;
        return;
    };

    answer_topic_created(
        client,
        callback,
        create_topic_with_model(context, chat_id, &mode_model(&mode), role).await,
        role,
    )
    .await;
}

/// The thread override a mode button pins. A suffixless mode primary means
/// thinking `off`, but a suffixless thread override inherits the chat's or the
/// orchestrator override's level, so the level is written out.
fn mode_model(mode: &ModelMode) -> String {
    zdx_engine::models::format_model_thinking(&mode.primary, mode.thinking)
}

/// Answer a launcher tap that created (or failed to create) a topic of `role`.
pub(crate) async fn answer_topic_created(
    client: &TelegramClient,
    callback: &CallbackQuery,
    result: Result<i64>,
    role: LaunchRole,
) {
    let text = match (&result, role) {
        (Ok(_), LaunchRole::Agent) => "New thread ready ✓",
        (Ok(_), LaunchRole::Orchestrator) => "New orchestrator ready ✓",
        (Err(_), LaunchRole::Agent) => "Couldn't create the topic",
        (Err(_), LaunchRole::Orchestrator) => "Couldn't create the orchestrator",
    };
    if let Err(err) = &result {
        tracing::error!(?role, err = %format!("{err:#}"), "launcher: failed to create topic");
    }
    let _ = client.answer_callback_query(&callback.id, Some(text)).await;
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

    if let Some(name) = rest.strip_prefix("p:") {
        open_mode_topic(context, client, callback, chat_id, name, LaunchRole::Agent).await;
    } else if let Some(name) = rest.strip_prefix("op:") {
        open_mode_topic(
            context,
            client,
            callback,
            chat_id,
            name,
            LaunchRole::Orchestrator,
        )
        .await;
    } else if let Some(role) = rest.strip_prefix("as:").and_then(LaunchRole::from_code) {
        if let Err(err) = render_launcher(context, chat_id, msg.id, role).await {
            tracing::warn!(chat_id, err = %format!("{err:#}"), "launcher: failed to switch role");
        }
        let _ = client.answer_callback_query(&callback.id, None).await;
    } else if rest == "x" {
        let _ = client.answer_callback_query(&callback.id, None).await;
        dismiss_launcher(context, chat_id, msg.id).await;
    } else if rest == "custom" || rest == "ocustom" {
        let role = if rest == "custom" {
            LaunchRole::Agent
        } else {
            LaunchRole::Orchestrator
        };
        // Open the provider → model picker targeting a brand-new topic.
        let scope = role.picker_scope();
        let keyboard = super::build_provider_keyboard(context, chat_id, scope);
        let header = crate::model_picker_header(context, chat_id, None, scope);
        if let Err(err) = client
            .edit_message_text(chat_id, msg.id, &header, Some(&keyboard))
            .await
        {
            tracing::warn!(chat_id, err = %format!("{err:#}"), "launcher: failed to open custom picker");
        }
        let _ = client.answer_callback_query(&callback.id, None).await;
    } else if rest == "resume" {
        let root = context.root_for_chat(chat_id).root;
        let all = zdx_engine::core::thread_persistence::list_threads().unwrap_or_default();
        // Managed workers must not be offered for resume: an in-process turn
        // on a worker thread would race its child process writing the same
        // JSONL. Steering happens in the worker's mirror topic instead.
        let managed = managed_worker_ids(context).await;
        let all: Vec<_> = all
            .into_iter()
            .filter(|t| !managed.contains(&t.id))
            .collect();
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
            tracing::warn!(chat_id, err = %format!("{err:#}"), "launcher: failed to open resume picker");
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
                tracing::warn!(chat_id, source_id, err = %format!("{err:#}"), "launcher: failed to resume thread");
                let _ = client
                    .answer_callback_query(&callback.id, Some("That thread no longer exists"))
                    .await;
            }
        }
        let role = default_role(context, chat_id);
        if let Err(err) = render_launcher(context, chat_id, msg.id, role).await {
            tracing::warn!(chat_id, err = %format!("{err:#}"), "launcher: failed to restore launcher after resume");
        }
    } else if rest == "back" {
        let role = default_role(context, chat_id);
        if let Err(err) = render_launcher(context, chat_id, msg.id, role).await {
            tracing::warn!(chat_id, err = %format!("{err:#}"), "launcher: failed to restore launcher");
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

    fn mode(name: &str, primary: &str) -> ModelMode {
        ModelMode {
            name: name.to_string(),
            description: String::new(),
            primary: primary.to_string(),
            alternatives: Vec::new(),
            thinking: ThinkingLevel::Off,
        }
    }

    #[test]
    fn filter_drops_unavailable_and_keeps_valid() {
        let available = vec![
            "anthropic:claude-opus-4-8".to_string(),
            "gemini:gemini-3.5-flash".to_string(),
        ];
        let modes = vec![
            // Bare id, prefixed availability entry — must still match.
            mode("smart", "claude-opus-4-8"),
            // Provider disabled — must be dropped.
            mode("gone", "openai:gpt-5.5"),
            // Prefixed id matching exactly.
            mode("fast", "gemini:gemini-3.5-flash"),
        ];

        let kept = filter_available_modes(&modes, &available);

        let names: Vec<&str> = kept.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["smart", "fast"]);
    }

    #[test]
    fn filter_empty_when_no_modes_available() {
        let available = vec!["anthropic:claude-opus-4-8".to_string()];
        let modes = vec![mode("gone", "openai:gpt-5.5")];

        assert!(filter_available_modes(&modes, &available).is_empty());
    }

    fn callback_data(keyboard: &InlineKeyboardMarkup) -> Vec<&str> {
        keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .filter_map(|b| b.callback_data.as_deref())
            .collect()
    }

    #[test]
    fn agent_launcher_keyboard_creates_agents_and_offers_the_switch() {
        let modes = vec![
            mode("fast", "gemini:gemini-3.5-flash"),
            mode("smart", "claude-cli:claude-opus-4-8"),
        ];
        let keyboard = build_launcher_keyboard(&modes, LaunchRole::Agent);
        let data = callback_data(&keyboard);
        assert_eq!(
            data,
            vec![
                "nt:as:o",
                "nt:p:fast",
                "nt:p:smart",
                "nt:custom",
                "nt:resume",
                "nt:x"
            ]
        );
        assert_eq!(keyboard.inline_keyboard[0][0].text, "New as: 🤖 Agent ⇄");

        // Telegram caps callback data at 64 bytes.
        for d in &data {
            assert!(d.len() <= 64, "callback data too long: {d}");
        }
    }

    #[test]
    fn orchestrator_launcher_keyboard_creates_orchestrators() {
        let keyboard = build_launcher_keyboard(
            &[mode("fast", "gemini:gemini-3.5-flash")],
            LaunchRole::Orchestrator,
        );
        assert_eq!(
            callback_data(&keyboard),
            vec!["nt:as:a", "nt:op:fast", "nt:ocustom", "nt:resume", "nt:x"]
        );
        assert_eq!(
            keyboard.inline_keyboard[0][0].text,
            "New as: 🧭 Orchestrator ⇄"
        );
        assert_eq!(
            callback_data(&build_launcher_keyboard(&[], LaunchRole::Orchestrator)),
            vec!["nt:as:a", "nt:ocustom", "nt:resume", "nt:x"]
        );
    }

    #[test]
    fn launcher_header_names_the_role_and_its_default() {
        let header = launcher_header(
            &[mode("fast", "gemini:gemini-3.5-flash")],
            LaunchRole::Orchestrator,
            "anthropic:claude-opus-4-8",
            ThinkingLevel::High,
        );
        assert!(
            header.contains("New topics start as: <b>🧭 Orchestrator</b>"),
            "{header}"
        );
        assert!(
            header.contains("Default: <code>claude-opus-4-8</code> · high"),
            "{header}"
        );
        assert!(
            launcher_header(
                &[],
                LaunchRole::Agent,
                "anthropic:claude-opus-4-8",
                ThinkingLevel::Off
            )
            .contains("<b>🤖 Agent</b>")
        );
    }

    fn launcher_callback(data: &str, message_id: i64) -> CallbackQuery {
        serde_json::from_value(serde_json::json!({
            "id": "cb1",
            "from": { "id": 7, "is_bot": false },
            "data": data,
            "message": {
                "message_id": message_id,
                "chat": { "id": -1001, "type": "supergroup", "is_forum": true },
            },
        }))
        .expect("callback")
    }

    fn fake_context(fake: &crate::telegram::fake::FakeTelegram) -> (BotContext, TelegramClient) {
        let client = TelegramClient::with_base_url(
            crate::telegram::fake::TOKEN.to_string(),
            fake.base_url.clone(),
        );
        let context = BotContext::for_test(
            client.clone(),
            zdx_engine::config::Config::default(),
            std::env::temp_dir(),
        );
        (context, client)
    }

    /// Dismiss deletes the launcher and stops tracking it, so a later General
    /// message does not repost it.
    #[tokio::test]
    async fn dismiss_deletes_the_launcher_and_stops_tracking_it() {
        use crate::telegram::fake::FakeTelegram;

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let (context, client) = fake_context(&fake);
        context.launcher_map().lock().await.insert(
            -1001,
            LauncherState {
                message_id: 900,
                generation: 1,
            },
        );

        handle_callback(&context, &client, &launcher_callback("nt:x", 900), "x").await;

        assert!(!context.launcher_map().lock().await.contains_key(&-1001));
        let methods = fake.methods();
        assert!(methods.iter().any(|m| m == "deleteMessage"), "{methods:?}");
        assert!(
            !methods.iter().any(|m| m == "editMessageReplyMarkup"),
            "{methods:?}"
        );
    }

    /// An older launcher that Telegram no longer lets the bot delete loses its
    /// buttons, and the chat's current launcher stays tracked.
    #[tokio::test]
    async fn dismissing_an_old_launcher_removes_its_buttons_and_keeps_the_current_one() {
        use crate::telegram::fake::{FakeTelegram, Reply};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        fake.script(
            "deleteMessage",
            &[Reply::Error(
                400,
                "Bad Request: message can't be deleted",
                None,
            )],
        );
        let (context, client) = fake_context(&fake);
        context.launcher_map().lock().await.insert(
            -1001,
            LauncherState {
                message_id: 950,
                generation: 1,
            },
        );

        handle_callback(&context, &client, &launcher_callback("nt:x", 900), "x").await;

        assert_eq!(
            context
                .launcher_map()
                .lock()
                .await
                .get(&-1001)
                .map(|state| state.message_id),
            Some(950)
        );
        let edit = fake
            .seen()
            .into_iter()
            .find(|seen| seen.method == "editMessageReplyMarkup")
            .expect("buttons removed");
        let body: serde_json::Value = serde_json::from_str(&edit.body).expect("json");
        assert_eq!(body["message_id"], 900);
        assert_eq!(
            body["reply_markup"]["inline_keyboard"],
            serde_json::json!([])
        );
    }

    fn created_topic_name(fake: &crate::telegram::fake::FakeTelegram) -> String {
        let create = fake
            .seen()
            .into_iter()
            .find(|seen| seen.method == "createForumTopic")
            .expect("topic created");
        let body: serde_json::Value = serde_json::from_str(&create.body).expect("json");
        body["name"].as_str().expect("name").to_string()
    }

    /// A launcher orchestrator is marked before its first message and pinned
    /// to the picked model as its own override, which wins over the shared
    /// `[subagents.overrides.orchestrator]` when the service resolves the turn.
    #[tokio::test]
    async fn new_orchestrator_runs_on_the_picked_model() {
        use zdx_engine::config::SubagentOverride;

        use crate::telegram::fake::FakeTelegram;

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let (context, _client) = fake_context(&fake);

        let topic_id = create_topic_with_model(
            &context,
            -1001,
            "anthropic:claude-opus-4-8@high",
            LaunchRole::Orchestrator,
        )
        .await
        .expect("orchestrator topic");

        assert!(
            created_topic_name(&fake).starts_with(super::super::ORCHESTRATOR_TOPIC_PREFIX),
            "{:?}",
            fake.methods()
        );
        let thread_id = thread_id_for_chat(-1001, Some(topic_id));
        assert_eq!(
            thread_persistence::read_persistent_profile(&thread_id)
                .expect("read profile")
                .as_deref(),
            Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME)
        );
        assert!(
            thread_persistence::read_thread_pending_topic_title(&thread_id).expect("read title")
        );
        let model_override =
            thread_persistence::read_thread_model_override(&thread_id).expect("read model");
        assert_eq!(
            model_override.as_deref(),
            Some("anthropic:claude-opus-4-8@high")
        );

        // Same resolution the orchestrator service runs per turn.
        let mut shared = Config::default();
        shared.subagents.overrides.insert(
            zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME.to_string(),
            SubagentOverride {
                model: Some("openai:gpt-5.5@low".to_string()),
                thinking_level: None,
            },
        );
        let definition = zdx_engine::subagents::load_persistent_profile(
            &std::env::temp_dir(),
            zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME,
        )
        .expect("orchestrator profile");
        let resolved = zdx_engine::orchestration::prompt::turn_config(
            shared,
            &definition,
            model_override.as_deref(),
            None,
        );
        assert!(
            resolved.model.contains("claude-opus-4-8"),
            "{}",
            resolved.model
        );
        assert_eq!(resolved.thinking_level, ThinkingLevel::High);
    }

    #[tokio::test]
    async fn new_agent_topic_keeps_the_default_profile() {
        use crate::telegram::fake::FakeTelegram;

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let (context, _client) = fake_context(&fake);

        let topic_id = create_topic_with_model(
            &context,
            -1001,
            "anthropic:claude-opus-4-8",
            LaunchRole::Agent,
        )
        .await
        .expect("agent topic");

        assert!(created_topic_name(&fake).starts_with("Chat "));
        let thread_id = thread_id_for_chat(-1001, Some(topic_id));
        assert_eq!(
            thread_persistence::read_persistent_profile(&thread_id).expect("read profile"),
            None
        );
    }

    /// The General launcher starts on the agent role unless the chat opted in
    /// to orchestrator General messages, and is tracked for reposting.
    #[tokio::test]
    async fn general_launcher_starts_on_the_chat_default_role() {
        use crate::telegram::fake::FakeTelegram;

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let (context, _client) = fake_context(&fake);
        assert_eq!(default_role(&context, -1001), LaunchRole::Agent);

        post_launcher(&context, -1001, Some(7))
            .await
            .expect("post launcher");

        assert!(context.launcher_map().lock().await.contains_key(&-1001));
        let send = fake
            .seen()
            .into_iter()
            .find(|seen| seen.method == "sendMessage")
            .expect("launcher sent");
        let body: serde_json::Value = serde_json::from_str(&send.body).expect("json");
        assert!(
            body.get("message_thread_id")
                .is_none_or(serde_json::Value::is_null),
            "{body}"
        );
        assert_eq!(
            body["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
            "nt:as:o"
        );
    }

    /// The role switch re-renders the same message with the other role's
    /// buttons.
    #[tokio::test]
    async fn role_switch_rerenders_the_launcher() {
        use crate::telegram::fake::FakeTelegram;

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let (context, client) = fake_context(&fake);

        handle_callback(
            &context,
            &client,
            &launcher_callback("nt:as:o", 900),
            "as:o",
        )
        .await;

        let edit = fake
            .seen()
            .into_iter()
            .find(|seen| seen.method == "editMessageText")
            .expect("launcher edited");
        let body: serde_json::Value = serde_json::from_str(&edit.body).expect("json");
        assert_eq!(body["message_id"], 900);
        let data: Vec<&str> = body["reply_markup"]["inline_keyboard"]
            .as_array()
            .expect("rows")
            .iter()
            .flat_map(|row| row.as_array().expect("row"))
            .filter_map(|button| button["callback_data"].as_str())
            .collect();
        assert_eq!(data, vec!["nt:as:a", "nt:ocustom", "nt:resume", "nt:x"]);
        assert!(!fake.methods().iter().any(|m| m == "createForumTopic"));
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

    /// A mode without a thinking suffix means `off`; the pinned override says
    /// so, or the new topic would inherit the orchestrator override's level.
    #[test]
    fn mode_model_writes_out_the_mode_thinking_level() {
        let mode = |primary: &str, thinking| ModelMode {
            name: "fast".to_string(),
            description: String::new(),
            primary: primary.to_string(),
            alternatives: Vec::new(),
            thinking,
        };
        assert_eq!(
            mode_model(&mode("anthropic:claude-haiku", ThinkingLevel::Off)),
            "anthropic:claude-haiku@off"
        );
        assert_eq!(
            mode_model(&mode("anthropic:claude-opus@high", ThinkingLevel::High)),
            "anthropic:claude-opus@high"
        );
        let (_, inherited) = zdx_engine::models::resolve_model_spec(
            &mode_model(&mode("anthropic:claude-haiku", ThinkingLevel::Off)),
            ThinkingLevel::High,
        );
        assert_eq!(inherited, ThinkingLevel::Off);
    }

    /// An orchestrator topic that could not be set up is deleted, so nothing
    /// runs there.
    #[tokio::test]
    async fn abandoned_orchestrator_topic_is_deleted() {
        use crate::telegram::fake::FakeTelegram;

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let (context, _client) = fake_context(&fake);

        abandon_orchestrator_topic(&context, -1001, 77).await;

        let delete = fake
            .seen()
            .into_iter()
            .find(|seen| seen.method == "deleteForumTopic")
            .expect("topic deleted");
        let body: serde_json::Value = serde_json::from_str(&delete.body).expect("json");
        assert_eq!(body["message_thread_id"], 77);
        assert!(!fake.methods().iter().any(|m| m == "sendMessage"));
    }

    /// When Telegram refuses to delete it, the topic gets a warning instead.
    #[tokio::test]
    async fn abandoned_orchestrator_topic_is_warned_when_it_cannot_be_deleted() {
        use crate::telegram::fake::{FakeTelegram, Reply};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        fake.script(
            "deleteForumTopic",
            &[Reply::Error(
                400,
                "Bad Request: not enough rights to delete the topic",
                None,
            )],
        );
        let (context, _client) = fake_context(&fake);

        abandon_orchestrator_topic(&context, -1001, 77).await;

        let warning = fake
            .seen()
            .into_iter()
            .find(|seen| seen.method == "sendMessage")
            .expect("warning sent");
        let body: serde_json::Value = serde_json::from_str(&warning.body).expect("json");
        assert_eq!(body["message_thread_id"], 77);
    }

    #[test]
    fn resume_callback_data_within_telegram_limit() {
        let id = "123e4567-e89b-12d3-a456-426614174000"; // UUID
        let data = format!("nt:r:{id}");
        assert!(data.len() <= 64, "callback data too long: {data}");
    }
}
