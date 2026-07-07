use anyhow::Result;
use zdx_engine::core::events::{AgentEvent, TurnStatus as AgentTurnStatus};
use zdx_engine::core::thread_persistence;

use super::response::send_final_response;
use super::status::{STATUS_DEBOUNCE, cleanup_turn_status, setup_turn_status, update_status};
use super::{ReplyContext, SpawnRequest, TurnResult, TurnStatus, format_user_error_message};
use crate::agent;
use crate::bot::context::BotContext;

pub(super) async fn run_agent_turn(
    context: &BotContext,
    incoming: crate::types::IncomingMessage,
    reply_ctx: ReplyContext,
    thread_id: &str,
    synthetic_topic_routed_from_general: bool,
    provisional_status: Option<TurnStatus>,
) -> Result<()> {
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let worktree_root = thread_persistence::read_thread_root_path(thread_id)?
        .map_or_else(|| resolved_root.root.clone(), std::path::PathBuf::from);
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let config = if model_override.is_some() || thinking_override.is_some() {
        let mut cfg = context.config();
        if let Some(ref model_id) = model_override {
            cfg.model.clone_from(model_id);
        }
        if let Some(level) = thinking_override {
            cfg.thinking_level = level;
        }
        cfg
    } else {
        context.config()
    };
    let (mut thread, mut messages) = agent::load_thread_state(thread_id)?;
    let pending_topic_title = thread_persistence::read_thread_pending_topic_title(thread_id)?;
    agent::record_user_message(&mut thread, &mut messages, &incoming)?;

    // Async topic title: spawn LLM-based title generation + rename for new topics.
    // This runs only after the user message is persisted, so the thread file exists.
    if (synthetic_topic_routed_from_general || pending_topic_title)
        && let Some(topic_id) = reply_ctx.topic_id
    {
        let effective_text = incoming
            .text
            .as_deref()
            .or_else(|| incoming.audios.iter().find_map(|a| a.transcript.as_deref()))
            .filter(|t| !t.trim().is_empty());

        if let Some(text) = effective_text {
            if pending_topic_title {
                thread.set_pending_topic_title(false)?;
            }
            crate::topic_title::spawn_topic_title_update(
                context,
                incoming.chat_id,
                topic_id,
                text.to_string(),
            );
        }
    }

    let typing = context
        .client()
        .start_typing(incoming.chat_id, reply_ctx.topic_id);

    let status = setup_turn_status(
        context,
        &incoming,
        reply_ctx.reply_to_message_id,
        reply_ctx.topic_id,
        provisional_status,
    )
    .await;
    let mut status = status;
    let spawn = SpawnRequest {
        worktree_root: &worktree_root,
        thread_id,
        thread: &thread,
        messages,
        config: &config,
    };
    let mut handle = spawn_or_fail(context, &incoming, &status, spawn).await?;
    let result = stream_turn_events(context, &incoming, &mut handle, &mut status).await;
    drop(typing);
    cleanup_turn_status(context, &status).await;
    finalize_turn(context, &incoming, &reply_ctx, &mut thread, &status, result).await
}

async fn spawn_or_fail(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    status: &TurnStatus,
    spawn: SpawnRequest<'_>,
) -> Result<agent::AgentTurnHandle> {
    let handle = agent::spawn_agent_turn(
        spawn.messages,
        spawn.config,
        spawn.worktree_root,
        context.bot_instruction_layer(),
        spawn.thread_id,
        spawn.thread,
        context.tool_config(),
    );

    match handle {
        Ok(handle) => Ok(handle),
        Err(err) => {
            tracing::error!(%err, "Failed to spawn agent turn");
            if let Some(msg_id) = status.message_id {
                let _ = context
                    .client()
                    .edit_message_text(
                        incoming.chat_id,
                        msg_id,
                        &format_user_error_message(&err.to_string()),
                        None,
                    )
                    .await;
            }
            cleanup_turn_status(context, status).await;
            Err(err)
        }
    }
}

async fn stream_turn_events(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    handle: &mut agent::AgentTurnHandle,
    status: &mut TurnStatus,
) -> TurnResult {
    let mut current_status = agent::STATUS_WAITING.to_string();
    let mut last_edit = std::time::Instant::now()
        .checked_sub(STATUS_DEBOUNCE)
        .expect("debounce subtraction should always succeed");
    let mut final_text = String::new();
    let mut got_result = false;
    let mut had_error = false;
    let mut error_message = None;

    loop {
        tokio::select! {
            biased;
            () = status.token.cancelled() => {
                handle.cancel.cancel();
                break;
            }
            event = handle.rx.recv() => {
                let Some(event) = event else { break; };
                match &*event {
                    AgentEvent::TurnFinished {
                        status,
                        final_text: text,
                        ..
                    } => {
                        final_text.clone_from(text);
                        match status {
                            AgentTurnStatus::Completed => {
                                got_result = true;
                            }
                            AgentTurnStatus::Interrupted => {}
                            AgentTurnStatus::Failed { message, .. } => {
                                had_error = true;
                                error_message = Some(message.clone());
                            }
                        }
                        break;
                    }
                    AgentEvent::Error { message, .. } => {
                        tracing::error!(message, "Agent error event");
                        // Diagnostic only; terminal outcome is carried by TurnFinished.
                    }
                    AgentEvent::Notice { kind, message, .. } => {
                        tracing::info!(?kind, message, "Agent notice event");
                    }
                    other => {
                        update_status(context, incoming.chat_id, status, other, &mut current_status, &mut last_edit).await;
                    }
                }
            }
        }
    }

    TurnResult {
        final_text,
        got_result,
        had_error,
        error_message,
    }
}

async fn finalize_turn(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    _thread: &mut zdx_engine::core::thread_persistence::Thread,
    status: &TurnStatus,
    result: TurnResult,
) -> Result<()> {
    if status.token.is_cancelled() {
        tracing::info!(
            chat_id = incoming.chat_id,
            topic_id = ?reply_ctx.topic_id,
            "Agent turn cancelled",
        );
        if let Some(msg_id) = status.message_id {
            let _ = context
                .client()
                .edit_message_text(incoming.chat_id, msg_id, "Cancelled ✓", None)
                .await;
        }
        return Ok(());
    }

    if result.had_error && !result.got_result {
        if let Some(msg_id) = status.message_id {
            let error_text = result.error_message.as_deref().map_or_else(
                || "Sorry, something went wrong.".to_string(),
                format_user_error_message,
            );
            let _ = context
                .client()
                .edit_message_text(incoming.chat_id, msg_id, &error_text, None)
                .await;
        }
        return Ok(());
    }

    send_final_response(
        context,
        incoming,
        reply_ctx,
        status.message_id,
        &result.final_text,
    )
    .await
}
