use anyhow::Result;
use zdx_engine::core::events::{AgentEvent, TurnStatus as AgentTurnStatus};
use zdx_engine::core::thread_persistence;

use super::response::send_final_response;
use super::status::{STATUS_DEBOUNCE, cleanup_turn_status, setup_turn_status, update_status};
use super::{
    ReplyContext, SpawnRequest, TurnOutcome, TurnResult, TurnStatus, format_user_error_message,
};
use crate::agent;
use crate::bot::context::BotContext;
use crate::telegram::InlineKeyboardMarkup;

pub(super) async fn run_agent_turn(
    context: &BotContext,
    incoming: crate::types::IncomingMessage,
    reply_ctx: ReplyContext,
    thread_id: &str,
    synthetic_topic_routed_from_general: bool,
    provisional_status: Option<TurnStatus>,
    record_user: bool,
) -> Result<TurnOutcome> {
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let stored_root = thread_persistence::read_thread_root_path(thread_id)?;
    let worktree_root = stored_root
        .clone()
        .map_or_else(|| resolved_root.root.clone(), std::path::PathBuf::from);
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let config = if model_override.is_some() || thinking_override.is_some() {
        let mut cfg = context.config_for_chat(incoming.chat_id);
        if let Some(ref model_id) = model_override {
            cfg.model.clone_from(model_id);
        }
        if let Some(level) = thinking_override {
            cfg.thinking_level = level;
        }
        cfg
    } else {
        context.config_for_chat(incoming.chat_id)
    };
    let (mut thread, mut messages) = agent::load_thread_state(thread_id)?;
    // Bot threads are opened by id, so their meta has no root until now. Record
    // the chat's project root so they group by project like CLI/TUI threads.
    if stored_root.is_none() {
        thread.set_root_path(&worktree_root)?;
    }
    // Persistent top-level profile (reserved orchestrator home base). Recorded
    // routes let worker completion callbacks reach this topic later.
    let persistent_profile = thread_persistence::read_persistent_profile(thread_id)?;
    let is_orchestrator =
        persistent_profile.as_deref() == Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME);
    if is_orchestrator {
        context.record_orchestrator_route(
            thread_id,
            crate::bot::context::OrchestratorRoute {
                chat: incoming.chat_id,
                topic: reply_ctx.topic_id,
                user: incoming.user_id,
            },
        );
    }
    let pending_topic_title = thread_persistence::read_thread_pending_topic_title(thread_id)?;
    if record_user {
        agent::record_user_message(&mut thread, &mut messages, &incoming)?;
    }

    // Async topic title: spawn LLM-based title generation + rename for new topics.
    // This runs only after the user message is persisted, so the thread file exists.
    // Orchestrator homes get titles too: Threaded Mode DM threads are otherwise
    // left as "New Thread" (voice) or a truncated first line (text).
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
        thread_id,
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
        persistent_profile: persistent_profile.as_deref(),
    };
    let mut handle = spawn_or_fail(context, &incoming, &status, spawn).await?;
    let result = stream_turn_events(context, &incoming, &mut handle, &mut status).await;
    // Barrier: the next queued message rebuilds its history from the thread log,
    // so the turn must be fully written before this one releases the queue slot.
    handle.await_persisted().await;
    drop(typing);
    cleanup_turn_status(context, &status).await;
    finalize_turn(
        context,
        &incoming,
        &reply_ctx,
        thread_id,
        &mut thread,
        &status,
        result,
        is_orchestrator,
    )
    .await
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
        spawn.persistent_profile,
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
    let mut touched_workers: Vec<String> = Vec::new();
    // Tool call id → name, so a completed Create_Thread / Send_Thread_Message
    // can be recognized (ToolCompleted carries only the id).
    let mut tool_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

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
                        match other {
                            AgentEvent::ToolStarted { id, name } => {
                                tool_names.insert(id.clone(), name.clone());
                            }
                            AgentEvent::ToolCompleted { id, result, .. } => {
                                if let Some(name) = tool_names.remove(id)
                                    && let Some(worker) = touched_worker_id(&name, result)
                                    && !touched_workers.contains(&worker)
                                {
                                    touched_workers.push(worker);
                                }
                            }
                            _ => {}
                        }
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
        touched_workers,
    }
}

/// Worker thread id from a successful `Create_Thread` / `Send_Thread_Message`
/// result, so the reply can link the worker's mirror topic.
fn touched_worker_id(
    tool_name: &str,
    result: &zdx_engine::core::events::ToolOutput,
) -> Option<String> {
    if !matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "create_thread" | "send_thread_message"
    ) {
        return None;
    }
    let zdx_engine::core::events::ToolOutput::Success { data, .. } = result else {
        return None;
    };
    data.get("thread_id")
        .or_else(|| {
            data.get("worker")
                .and_then(|worker| worker.get("thread_id"))
        })
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
async fn finalize_turn(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    thread_id: &str,
    _thread: &mut zdx_engine::core::thread_persistence::Thread,
    status: &TurnStatus,
    result: TurnResult,
    is_orchestrator: bool,
) -> Result<TurnOutcome> {
    if status.token.is_cancelled() {
        tracing::info!(
            chat_id = incoming.chat_id,
            topic_id = ?reply_ctx.topic_id,
            "Agent turn cancelled",
        );
        if let Some(msg_id) = status.message_id {
            let _ = context
                .client()
                .edit_message_text(
                    incoming.chat_id,
                    msg_id,
                    "Cancelled ✓",
                    Some(&InlineKeyboardMarkup::empty()),
                )
                .await;
        }
        return Ok(TurnOutcome::Cancelled);
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
        // No retry buttons on orchestrator topics: a retry can be tapped long
        // after the process-lifetime routes/queues it depends on are gone.
        if !is_orchestrator {
            crate::retry::send_retry_buttons(
                context,
                incoming.chat_id,
                crate::retry::RetryRequest {
                    thread_id: thread_id.to_string(),
                    topic_id: reply_ctx.topic_id,
                    reply_to_message_id: reply_ctx.reply_to_message_id,
                    user_message_id: incoming.message_id,
                },
            )
            .await;
        }
        return Ok(TurnOutcome::Failed(
            result
                .error_message
                .unwrap_or_else(|| "the turn failed".to_string()),
        ));
    }

    send_final_response(
        context,
        incoming,
        reply_ctx,
        status.message_id,
        &result.final_text,
        thread_id,
        &result.touched_workers,
    )
    .await?;

    Ok(TurnOutcome::Completed)
}
