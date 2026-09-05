use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use zdx_engine::core::events::AgentEvent;
use zdx_engine::core::thread_persistence;
use zdx_engine::models::{ModelOption, ModelPricing};
use zdx_engine::providers::{ProviderAuthMode, provider_for_model};

use super::{StatusSnapshot, TurnStatus, escape_html};
use crate::agent;
use crate::bot::context::BotContext;
use crate::telegram::{InlineKeyboardButton, InlineKeyboardMarkup, Message};

/// Minimum interval between Telegram status message edits (avoid rate limiting).
pub(super) const STATUS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(3);

fn turn_status_markup(
    context: &BotContext,
    chat_id: i64,
    key: (i64, i64),
    thread_id: Option<&str>,
) -> InlineKeyboardMarkup {
    let mini_app_url = thread_id.and(super::mini_app_base_url(context, chat_id));
    turn_status_markup_for_url(key, mini_app_url.as_deref(), thread_id)
}

fn turn_status_markup_for_url(
    key: (i64, i64),
    mini_app_url: Option<&str>,
    thread_id: Option<&str>,
) -> InlineKeyboardMarkup {
    let mut row = vec![InlineKeyboardButton::callback(
        "⏹ Cancel",
        format!("cancel:{}:{}", key.0, key.1),
    )];
    if let (Some(mini_app_url), Some(thread_id)) = (mini_app_url, thread_id) {
        row.push(InlineKeyboardButton::url(
            "💬 Open Thread",
            format!("{mini_app_url}?startapp={thread_id}"),
        ));
    }
    InlineKeyboardMarkup {
        inline_keyboard: vec![row],
    }
}

pub(super) async fn setup_turn_status(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    thread_id: &str,
    existing: Option<TurnStatus>,
) -> TurnStatus {
    if let Some(mut status) = existing {
        // The provisional (transcription) status was created before the thread
        // was known, so it only had Cancel. Give it the thread link now.
        status.markup = turn_status_markup(context, incoming.chat_id, status.key, Some(thread_id));
        update_turn_status_text(context, incoming.chat_id, &status, agent::STATUS_WAITING).await;
        return status;
    }

    let key = (incoming.chat_id, incoming.message_id);
    let cancel_markup = turn_status_markup(context, incoming.chat_id, key, Some(thread_id));

    let token = CancellationToken::new();
    {
        let mut map = context.cancel_map().lock().await;
        map.insert(key, token.clone());
    }

    let mut message_id = context
        .client()
        .send_message_with_markup(
            incoming.chat_id,
            agent::STATUS_WAITING,
            reply_to_message_id,
            topic_id,
            &cancel_markup,
        )
        .await
        .ok()
        .map(|m| m.id);

    // Retry without reply_to on REPLY_MESSAGE_ID_INVALID
    if message_id.is_none() && reply_to_message_id.is_some() {
        message_id = context
            .client()
            .send_message_with_markup(
                incoming.chat_id,
                agent::STATUS_WAITING,
                None,
                topic_id,
                &cancel_markup,
            )
            .await
            .ok()
            .map(|m| m.id);
    }

    TurnStatus {
        key,
        token,
        markup: cancel_markup,
        message_id,
    }
}

pub(super) async fn setup_preprocessing_status(
    context: &BotContext,
    message: &Message,
    synthetic_topic_routed_from_general: bool,
) -> TurnStatus {
    let key = (message.chat.id, message.id);
    let cancel_markup = turn_status_markup(context, message.chat.id, key, None);
    let token = CancellationToken::new();
    {
        let mut map = context.cancel_map().lock().await;
        map.insert(key, token.clone());
    }

    let reply_to_message_id = if synthetic_topic_routed_from_general
        || message.effective_thread_id() == Some(message.id)
    {
        None
    } else {
        Some(message.id)
    };

    let mut message_id = context
        .client()
        .send_message_with_markup(
            message.chat.id,
            agent::STATUS_TRANSCRIBING,
            reply_to_message_id,
            message.effective_thread_id(),
            &cancel_markup,
        )
        .await
        .ok()
        .map(|m| m.id);

    if message_id.is_none() && reply_to_message_id.is_some() {
        message_id = context
            .client()
            .send_message_with_markup(
                message.chat.id,
                agent::STATUS_TRANSCRIBING,
                None,
                message.effective_thread_id(),
                &cancel_markup,
            )
            .await
            .ok()
            .map(|m| m.id);
    }

    TurnStatus {
        key,
        token,
        markup: cancel_markup,
        message_id,
    }
}

async fn update_turn_status_text(
    context: &BotContext,
    chat_id: i64,
    status: &TurnStatus,
    text: &str,
) {
    let Some(msg_id) = status.message_id else {
        return;
    };
    let _ = context
        .client()
        .edit_message_text(chat_id, msg_id, text, Some(&status.markup))
        .await;
}

pub(super) async fn discard_turn_status(
    context: &BotContext,
    chat_id: Option<i64>,
    status: &TurnStatus,
) {
    if let (Some(chat_id), Some(msg_id)) = (chat_id, status.message_id) {
        let _ = context.client().delete_message(chat_id, msg_id).await;
    }
    cleanup_turn_status(context, status).await;
}

pub(super) async fn finalize_preprocessing_cancelled(
    context: &BotContext,
    chat_id: i64,
    status: &TurnStatus,
) {
    if let Some(msg_id) = status.message_id {
        let _ = context
            .client()
            .edit_message_text(
                chat_id,
                msg_id,
                "Cancelled ✓",
                Some(&InlineKeyboardMarkup::empty()),
            )
            .await;
    }
    cleanup_turn_status(context, status).await;
}

pub(super) async fn update_status(
    context: &BotContext,
    chat_id: i64,
    status: &TurnStatus,
    event: &AgentEvent,
    current_status: &mut String,
    last_edit: &mut std::time::Instant,
) {
    let Some(new_status) = agent::event_to_status(event) else {
        return;
    };
    if new_status == *current_status {
        return;
    }

    *current_status = new_status;
    if status.message_id.is_none() {
        return;
    }
    let now = std::time::Instant::now();
    if now.duration_since(*last_edit) < STATUS_DEBOUNCE {
        return;
    }
    *last_edit = now;
    update_turn_status_text(context, chat_id, status, current_status).await;
}

pub(super) async fn cleanup_turn_status(context: &BotContext, status: &TurnStatus) {
    let mut map = context.cancel_map().lock().await;
    map.remove(&status.key);
}

pub(super) async fn current_status_message(
    context: &BotContext,
    chat_id: i64,
    thread_id: &str,
) -> Result<String> {
    current_status_message_with_heading(context, chat_id, thread_id, "<b>Status</b>").await
}

pub(super) async fn current_thread_header_message(
    context: &BotContext,
    chat_id: i64,
    thread_id: &str,
) -> Result<String> {
    current_status_message_with_heading(context, chat_id, thread_id, "🧵 <b>Thread</b>").await
}

async fn current_status_message_with_heading(
    context: &BotContext,
    chat_id: i64,
    thread_id: &str,
    heading: &str,
) -> Result<String> {
    let config = context.config_for_chat(chat_id);
    let resolved_root = context.root_for_chat(chat_id);
    let root_path = thread_persistence::read_thread_root_path(thread_id)?
        .map_or_else(|| resolved_root.root.clone(), PathBuf::from);
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let effective_model = model_override.as_deref().unwrap_or(&config.model);
    let effective_thinking = thinking_override.unwrap_or(config.thinking_level);
    let branch = git_branch_name(&root_path).await;
    let events = thread_persistence::load_thread_events(thread_id)?;
    let (cumulative_usage, latest_usage) =
        thread_persistence::extract_usage_from_thread_events(&events);
    // Orchestrator home bases get their own card: the folder/branch are fixed
    // noise there, while live worker state is the interesting part.
    let is_orchestrator = thread_persistence::read_persistent_profile(thread_id)?.as_deref()
        == Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME);
    let workers = is_orchestrator.then(|| context.worker_manager().list_for_owner(thread_id));
    let mini_app_url = super::mini_app_base_url(context, chat_id);
    Ok(format_status_message_with_heading(
        &StatusSnapshot {
            model_id: effective_model,
            model_override: model_override.as_deref(),
            thinking: effective_thinking,
            thinking_override,
            profile_name: resolved_root.profile_name.as_deref(),
            thread_id,
            root_path: &root_path,
            branch: branch.as_deref(),
            cumulative_usage,
            latest_usage,
        },
        heading,
        workers.as_deref(),
        mini_app_url.as_deref(),
    ))
}

fn format_status_message_with_heading(
    snapshot: &StatusSnapshot<'_>,
    heading: &str,
    orchestrator_workers: Option<&[zdx_engine::core::workers::WorkerSnapshot]>,
    mini_app_url: Option<&str>,
) -> String {
    let model_meta = ModelOption::find_by_id(snapshot.model_id);
    let provider = provider_for_model(snapshot.model_id);
    let heading = if orchestrator_workers.is_some() {
        "🎛 <b>Orchestrator</b>"
    } else {
        heading
    };
    let mut lines = vec![heading.to_string()];

    lines.push(format!(
        "Model: <code>{}</code> ({})",
        escape_html(snapshot.model_id),
        if snapshot.model_override.is_some() {
            "override"
        } else {
            "default"
        }
    ));
    lines.push(format!(
        "Thinking: <code>{}</code> ({})",
        snapshot.thinking.display_name(),
        if snapshot.thinking_override.is_some() {
            "override"
        } else {
            "default"
        }
    ));
    lines.push(format!(
        "Thread: <code>{}</code>",
        escape_html(snapshot.thread_id)
    ));
    lines.push(format!(
        "Profile: <code>{}</code>",
        escape_html(snapshot.profile_name.unwrap_or("fallback"))
    ));
    if orchestrator_workers.is_none() {
        lines.push(format!(
            "Root: <code>{}</code>",
            escape_html(&snapshot.root_path.display().to_string())
        ));
        lines.push(format!(
            "Branch: <code>{}</code>",
            escape_html(snapshot.branch.unwrap_or("n/a"))
        ));
    }

    lines.push(format_context_usage_line(model_meta, snapshot.latest_usage));
    lines.push(format!(
        "Usage totals: <code>↑{} ↓{} R{} W{}</code>",
        format_token_count(snapshot.cumulative_usage.input),
        format_token_count(snapshot.cumulative_usage.output),
        format_token_count(snapshot.cumulative_usage.cache_read),
        format_token_count(snapshot.cumulative_usage.cache_write)
    ));
    lines.push(format_pricing_line(
        model_meta,
        provider.auth_mode(),
        snapshot.cumulative_usage,
    ));

    if let Some(workers) = orchestrator_workers {
        lines.extend(format_worker_lines(workers, mini_app_url));
    }

    lines.join("\n")
}

/// Formats the live worker section of the orchestrator card. Each worker
/// line links to its mirror topic, or to the worker thread in the Mini App
/// when the mirror has no topic link (`mini_app_url` set), so the card is a
/// jump table as well as a status.
fn format_worker_lines(
    workers: &[zdx_engine::core::workers::WorkerSnapshot],
    mini_app_url: Option<&str>,
) -> Vec<String> {
    use zdx_engine::core::workers::WorkerStatus;

    if workers.is_empty() {
        return vec!["Workers: <i>none yet</i>".to_string()];
    }

    let count = |status: WorkerStatus| workers.iter().filter(|w| w.status == status).count();
    let running = count(WorkerStatus::Running);
    let queued = workers
        .iter()
        .filter(|w| w.status == WorkerStatus::Queued)
        .count();
    let done = workers.len() - running - queued;

    let mut lines = vec![format!(
        "Workers: <code>{running} running · {queued} queued · {done} settled</code>"
    )];
    for worker in workers.iter().take(5) {
        let glyph = match worker.status {
            WorkerStatus::Running => "⚙️",
            WorkerStatus::Queued => "⏳",
            WorkerStatus::Completed => "✅",
            WorkerStatus::Failed => "❌",
            WorkerStatus::Cancelled => "🚫",
        };
        let title = thread_persistence::read_thread_title(&worker.thread_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| worker.thread_id.chars().take(8).collect());
        let title = escape_html(&title);
        let url = worker
            .mirror_url
            .clone()
            .or_else(|| mini_app_url.map(|base| format!("{base}?startapp={}", worker.thread_id)));
        let label = match url {
            Some(url) => format!("<a href=\"{url}\">{title}</a>"),
            None => title,
        };
        lines.push(format!("• {glyph} {label} — {}", worker.status.as_str()));
    }
    if workers.len() > 5 {
        lines.push(format!("• … and {} more", workers.len() - 5));
    }
    lines
}

async fn git_branch_name(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("branch")
        .arg("--show-current")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

fn format_context_usage_line(
    model_meta: Option<&ModelOption>,
    latest_usage: thread_persistence::Usage,
) -> String {
    let context_tokens = latest_usage.context_input() + latest_usage.output;
    match model_meta {
        Some(model) if model.context_limit > 0 => {
            let pct = (context_tokens as f64 / model.context_limit as f64) * 100.0;
            format!(
                "Context usage: <code>{pct:.0}% of {} ({})</code>",
                format_context_limit(model.context_limit),
                format_token_count(context_tokens)
            )
        }
        _ => format!(
            "Context usage: <code>{}</code> (limit unknown)",
            format_token_count(context_tokens)
        ),
    }
}

fn format_pricing_line(
    model_meta: Option<&ModelOption>,
    auth_mode: ProviderAuthMode,
    usage: thread_persistence::Usage,
) -> String {
    let Some(model) = model_meta else {
        return "Pricing: <code>unknown</code> (model registry metadata not found)".to_string();
    };

    if auth_mode == ProviderAuthMode::OAuth {
        return "Pricing: <code>subscription</code> (OAuth provider)".to_string();
    }

    let total_cost = calculate_usage_cost(usage, &model.pricing);
    let cache_savings = calculate_cache_savings(usage, &model.pricing);
    let mut line = format!(
        "Pricing: <code>{}</code> total · rates <code>${}/${}/${}/${}</code>/1M",
        format_cost(total_cost),
        trim_price(model.pricing.input),
        trim_price(model.pricing.output),
        trim_price(model.pricing.cache_read),
        trim_price(model.pricing.cache_write)
    );

    if cache_savings > 0.001 {
        let _ = write!(line, " · saved <code>{}</code>", format_cost(cache_savings));
    } else if usage.cache_read > 0 {
        line.push_str(" · cached");
    }

    line
}

fn calculate_usage_cost(usage: thread_persistence::Usage, pricing: &ModelPricing) -> f64 {
    pricing.cost(
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
    )
}

fn calculate_cache_savings(usage: thread_persistence::Usage, pricing: &ModelPricing) -> f64 {
    pricing.cache_savings(usage.cache_read)
}

fn format_token_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn format_context_limit(limit: u64) -> String {
    if limit >= 1_000_000 {
        format!("{:.0}M", limit as f64 / 1_000_000.0)
    } else if limit >= 1_000 {
        format!("{:.0}k", limit as f64 / 1_000.0)
    } else {
        limit.to_string()
    }
}

fn format_cost(cost: f64) -> String {
    if cost < 0.001 {
        format!("${cost:.4}")
    } else if cost < 0.01 {
        format!("${cost:.3}")
    } else {
        format!("${cost:.2}")
    }
}

fn trim_price(value: f64) -> String {
    let mut text = format!("{value:.4}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{format_worker_lines, turn_status_markup_for_url};

    #[test]
    fn worker_lines_summarize_counts_and_cap_listing() {
        use std::path::PathBuf;

        use zdx_engine::core::workers::{WorkerSnapshot, WorkerStatus};

        let snapshot = |id: &str, status: WorkerStatus| WorkerSnapshot {
            thread_id: format!("nonexistent-worker-{id}"),
            owner_thread_id: "owner".to_string(),
            root: PathBuf::from("/tmp"),
            status,
            queue_depth: 0,
            latest_final_text: None,
            last_error: None,
            mirror_url: None,
        };

        assert_eq!(
            format_worker_lines(&[], None),
            vec!["Workers: <i>none yet</i>"]
        );

        let workers: Vec<_> = [
            WorkerStatus::Running,
            WorkerStatus::Queued,
            WorkerStatus::Completed,
            WorkerStatus::Failed,
            WorkerStatus::Cancelled,
            WorkerStatus::Completed,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, status)| snapshot(&i.to_string(), status))
        .collect();

        let lines = format_worker_lines(&workers, None);
        assert_eq!(
            lines[0],
            "Workers: <code>1 running · 1 queued · 4 settled</code>"
        );
        // 5 listed + overflow line.
        assert_eq!(lines.len(), 7);
        assert!(lines[6].contains("and 1 more"));
        assert!(lines[1].starts_with("• ⚙️"));
        // No mirror and no Mini App: plain title.
        assert_eq!(lines[1], "• ⚙️ nonexist — running");

        // Each worker line is a jump link: mirror topic first, Mini App
        // fallback when the mirror has no topic link.
        let mut linked = snapshot("m", WorkerStatus::Running);
        linked.mirror_url = Some("https://t.me/c/1/2".to_string());
        let lines = format_worker_lines(
            &[linked, snapshot("n", WorkerStatus::Completed)],
            Some("https://t.me/zdx_bot/app"),
        );
        assert_eq!(
            lines[1],
            "• ⚙️ <a href=\"https://t.me/c/1/2\">nonexist</a> — running"
        );
        assert_eq!(
            lines[2],
            "• ✅ <a href=\"https://t.me/zdx_bot/app?startapp=nonexistent-worker-n\">nonexist</a> — completed"
        );
    }

    #[test]
    fn configured_status_cancels_and_opens_the_effective_thread() {
        let markup = turn_status_markup_for_url(
            (-100, 42),
            Some("https://t.me/zdx_bot/threads"),
            Some("source-thread-id"),
        );
        let row = &markup.inline_keyboard[0];
        assert_eq!(markup.inline_keyboard.len(), 1);
        assert_eq!(row.len(), 2);
        assert_eq!(row[0].callback_data.as_deref(), Some("cancel:-100:42"));
        assert_eq!(
            row[1].url.as_deref(),
            Some("https://t.me/zdx_bot/threads?startapp=source-thread-id")
        );
    }

    #[test]
    fn status_without_mini_app_or_thread_only_cancels() {
        for markup in [
            turn_status_markup_for_url((-100, 42), None, Some("thread-id")),
            turn_status_markup_for_url((-100, 42), Some("https://t.me/zdx_bot/threads"), None),
        ] {
            assert_eq!(markup.inline_keyboard[0].len(), 1);
            assert_eq!(
                markup.inline_keyboard[0][0].callback_data.as_deref(),
                Some("cancel:-100:42")
            );
        }
    }
}
