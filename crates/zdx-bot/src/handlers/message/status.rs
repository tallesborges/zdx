use std::fmt::Write as _;

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

pub(super) async fn setup_turn_status(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_to_message_id: Option<i64>,
    topic_id: Option<i64>,
    existing: Option<TurnStatus>,
) -> TurnStatus {
    if let Some(status) = existing {
        update_turn_status_text(context, incoming.chat_id, &status, agent::STATUS_WAITING).await;
        return status;
    }

    let key = (incoming.chat_id, incoming.message_id);
    let cancel_markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "⏹ Cancel".to_string(),
            callback_data: Some(format!("cancel:{}:{}", key.0, key.1)),
            url: None,
        }]],
    };

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
    let cancel_markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "⏹ Cancel".to_string(),
            callback_data: Some(format!("cancel:{}:{}", key.0, key.1)),
            url: None,
        }]],
    };
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
    update_turn_status_text(context, chat_id, status, "Cancelled ✓").await;
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

pub(super) fn format_status_message(snapshot: &StatusSnapshot<'_>) -> String {
    let model_meta = ModelOption::find_by_id(snapshot.model_id);
    let provider = provider_for_model(snapshot.model_id);
    let mut lines = vec!["<b>Status</b>".to_string()];

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
    lines.push(format!(
        "Root: <code>{}</code>",
        escape_html(&snapshot.root_path.display().to_string())
    ));
    lines.push(format!(
        "Branch: <code>{}</code>",
        escape_html(snapshot.branch.unwrap_or("n/a"))
    ));

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

    lines.join("\n")
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
