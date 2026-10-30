use anyhow::Result;
use zdx_engine::core::events::{AgentEvent, TurnStatus as AgentTurnStatus};
use zdx_engine::core::thread_persistence;

use super::response::send_final_response;
use super::status::{StatusDebounce, cleanup_turn_status, flush_status, setup_turn_status};
use super::{ReplyContext, SpawnRequest, TurnResult, TurnStatus, format_user_error_message};
use crate::agent;
use crate::bot::context::BotContext;
use crate::telegram::InlineKeyboardMarkup;

#[allow(clippy::too_many_lines)]
pub(super) async fn run_agent_turn(
    context: &BotContext,
    incoming: crate::types::IncomingMessage,
    reply_ctx: ReplyContext,
    thread_id: &str,
    synthetic_topic_routed_from_general: bool,
    provisional_status: Option<TurnStatus>,
    record_user: bool,
) -> Result<()> {
    // Queued turns can start long after the message arrived, so re-check the
    // config layers here too: this is what feeds model, thinking level, and
    // subagent overrides into the run.
    context.reload_config_if_changed();
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let stored_root = thread_persistence::read_thread_root_path(thread_id)?;
    let worktree_root = stored_root
        .clone()
        .map_or_else(|| resolved_root.root.clone(), std::path::PathBuf::from);
    // Persistent top-level profile (reserved orchestrator home base). Recorded
    // routes let worker completion callbacks reach this topic later.
    let persistent_profile = thread_persistence::read_persistent_profile(thread_id)?;
    let is_orchestrator =
        persistent_profile.as_deref() == Some(zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME);
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let config = turn_config(
        context.config_for_chat(incoming.chat_id),
        is_orchestrator,
        model_override.as_deref(),
        thinking_override,
    );
    let (mut thread, mut messages, runtime_note) = agent::load_thread_state(thread_id)?;
    // Bot threads are opened by id, so their meta has no root until now. Record
    // the chat's project root so they group by project like CLI/TUI threads.
    if stored_root.is_none() {
        thread.set_root_path(&worktree_root)?;
    }
    // Prepare the turn once so the advisory runtime-context snapshot is known
    // before the user message is persisted (it is attached there and replayed
    // identically on the next turn).
    zdx_engine::core::context::set_runtime_env(&config, Some(thread_id));
    let prepared = match agent::prepare_bot_turn(
        &config,
        &worktree_root,
        context.bot_instruction_layer(),
        persistent_profile.as_deref(),
        Some(incoming.chat_id),
    ) {
        Ok(prepared) => prepared,
        Err(err) => {
            // The turn cannot run, but the incoming message must not be
            // silently dropped (the queue caller only logs). Persist it
            // without a runtime-context snapshot (preparation failed) and
            // surface a visible error. Only a newly recorded message is
            // written — a retry has nothing new to persist, and the success
            // path below never runs, so nothing is double-written.
            tracing::error!(err = %format!("{err:#}"), "Failed to prepare bot turn");
            if record_user
                && let Err(persist_err) = agent::record_user_message(
                    &mut thread,
                    &mut messages,
                    &incoming,
                    None,
                    runtime_note.as_deref(),
                )
            {
                tracing::error!(
                    %persist_err,
                    "Failed to persist user message after prompt-preparation failure"
                );
            }
            let _ = context
                .client()
                .send_message_without_preview(
                    incoming.chat_id,
                    &format_user_error_message(&err.to_string()),
                    reply_ctx.reply_to_message_id,
                    reply_ctx.topic_id,
                )
                .await;
            return Err(err);
        }
    };
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
        agent::record_user_message(
            &mut thread,
            &mut messages,
            &incoming,
            prepared.runtime_context.as_ref(),
            runtime_note.as_deref(),
        )?;
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
        thread_id,
        thread: &thread,
        messages,
        prepared,
    };
    let mut handle = agent::spawn_agent_turn(
        spawn.messages,
        spawn.prepared,
        spawn.thread_id,
        spawn.thread,
        context.tool_config(),
    );
    let result = stream_turn_events(
        context,
        &incoming,
        &mut handle.rx,
        &handle.cancel,
        &mut status,
    )
    .await;
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

/// Layers the model/thinking a turn runs with: chat config →
/// `[subagents.overrides.orchestrator]` (orchestrator topics only) → the
/// topic's own unified `/model` override, plus historical thinking metadata.
fn turn_config(
    mut config: zdx_engine::config::Config,
    is_orchestrator: bool,
    model_override: Option<&str>,
    thinking_override: Option<zdx_engine::config::ThinkingLevel>,
) -> zdx_engine::config::Config {
    if is_orchestrator {
        zdx_engine::subagents::apply_orchestrator_override(&mut config);
    }
    config.apply_thread_model_override(model_override, thinking_override);
    config
}

/// Consumes one turn's agent events until it ends, keeping the status
/// message's progress line current. A progress change that arrives inside the
/// debounce interval is sent when the interval ends, even with no further
/// event. Ready events are handled before a due flush, and the pending status
/// is dropped when the loop exits (turn finished or cancelled), so no
/// progress edit is sent after that.
async fn stream_turn_events(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    rx: &mut zdx_engine::core::agent::AgentEventRx,
    agent_cancel: &tokio_util::sync::CancellationToken,
    status: &mut TurnStatus,
) -> TurnResult {
    let mut progress = StatusDebounce::new(agent::STATUS_WAITING, std::time::Instant::now());
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
        let flush_at = progress.next_flush_at();
        let flush_deadline =
            tokio::time::Instant::from_std(flush_at.unwrap_or_else(std::time::Instant::now));
        tokio::select! {
            biased;
            () = status.token.cancelled() => {
                agent_cancel.cancel();
                break;
            }
            event = rx.recv() => {
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
                        progress.observe(agent::event_to_status(other));
                    }
                }
                // Every non-terminal event can send a due status, so a steady
                // stream of events cannot starve the flush below.
                flush_status(context, incoming.chat_id, status, &mut progress, std::time::Instant::now()).await;
            }
            () = tokio::time::sleep_until(flush_deadline), if flush_at.is_some() => {
                // The timer never fires early; `max` keeps a clock rounding
                // difference from turning into an immediate re-wake.
                let now = std::time::Instant::now().max(flush_at.unwrap_or_else(std::time::Instant::now));
                flush_status(context, incoming.chat_id, status, &mut progress, now).await;
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
/// result, so the reply can link the worker.
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
    thread: &mut zdx_engine::core::thread_persistence::Thread,
    status: &TurnStatus,
    result: TurnResult,
    is_orchestrator: bool,
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
                .edit_message_text(
                    incoming.chat_id,
                    msg_id,
                    "Cancelled ✓",
                    Some(&InlineKeyboardMarkup::empty()),
                )
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
        return Ok(());
    }

    send_final_response(
        context,
        incoming,
        reply_ctx,
        status.message_id,
        &result.final_text,
        thread_id,
        &result.touched_workers,
        thread,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use zdx_engine::core::events::{AgentEvent, TurnStatus as AgentTurnStatus};

    use super::stream_turn_events;
    use crate::bot::context::BotContext;
    use crate::handlers::message::TurnStatus;
    use crate::telegram::fake::{FakeTelegram, TOKEN};
    use crate::telegram::{InlineKeyboardMarkup, TelegramClient};

    const CHAT: i64 = -1001;

    fn incoming() -> crate::types::IncomingMessage {
        crate::types::IncomingMessage {
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
        }
    }

    fn turn_status() -> TurnStatus {
        TurnStatus {
            key: (CHAT, 42),
            token: CancellationToken::new(),
            markup: InlineKeyboardMarkup::empty(),
            message_id: Some(55),
        }
    }

    fn tool_started(name: &str) -> Arc<AgentEvent> {
        Arc::new(AgentEvent::ToolStarted {
            id: format!("id-{name}"),
            name: name.to_string(),
        })
    }

    fn thinking() -> Arc<AgentEvent> {
        Arc::new(AgentEvent::ReasoningDelta {
            text: "…".to_string(),
        })
    }

    fn finished() -> Arc<AgentEvent> {
        Arc::new(AgentEvent::TurnFinished {
            status: AgentTurnStatus::Completed,
            final_text: "done".to_string(),
            messages: Vec::new(),
            prior_message_count: 0,
        })
    }

    /// Bodies of the status edits the fake received, in order.
    fn edits(fake: &FakeTelegram) -> Vec<String> {
        fake.seen()
            .into_iter()
            .filter(|seen| seen.method == "editMessageText")
            .map(|seen| seen.body)
            .collect()
    }

    fn context(fake: &FakeTelegram) -> Arc<BotContext> {
        Arc::new(BotContext::for_test(
            TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone()),
            zdx_engine::config::Config::default(),
            std::env::temp_dir(),
        ))
    }

    /// The reported bug: the newest progress arrived inside the 3 s interval,
    /// no further event came, and the message kept the old text.
    #[tokio::test]
    async fn the_newest_skipped_status_is_sent_when_the_interval_ends() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = Arc::new(FakeTelegram::start().await);
        let context = context(&fake);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let mut status = turn_status();

        tx.send(tool_started("bash")).expect("send");
        tx.send(tool_started("read")).expect("send");
        tx.send(thinking()).expect("send");
        // No further progress event. Finish only after the second edit landed
        // (bounded wait), so the check does not depend on scheduling slack.
        let watched = Arc::clone(&fake);
        let driver = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while edits(&watched).len() < 2 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tx.send(finished()).expect("send");
        });
        let started = std::time::Instant::now();
        let result = stream_turn_events(&context, &incoming(), &mut rx, &cancel, &mut status).await;
        driver.await.expect("driver");

        assert!(result.got_result);
        let edits = edits(&fake);
        assert_eq!(edits.len(), 2, "{edits:?}");
        assert!(edits[0].contains("bash"));
        // `read` was overtaken by `thinking`: only the newest is sent, and
        // only after the 3 s interval.
        assert!(edits[1].contains("Thinking"), "{}", edits[1]);
        assert!(started.elapsed() >= Duration::from_millis(2900));
    }

    /// A turn that ends (or is cancelled) before the interval passes sends no
    /// progress edit afterwards: the pending status ends with the loop.
    #[tokio::test]
    async fn completion_or_cancel_before_the_flush_drops_the_pending_status() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        let context = context(&fake);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let mut status = turn_status();
        tx.send(tool_started("bash")).expect("send");
        tx.send(thinking()).expect("send");
        tx.send(finished()).expect("send");
        let result = stream_turn_events(&context, &incoming(), &mut rx, &cancel, &mut status).await;
        assert!(result.got_result);

        let (tx2, mut rx2) = mpsc::unbounded_channel();
        let agent_cancel = CancellationToken::new();
        let mut cancelled = turn_status();
        tx2.send(tool_started("edit")).expect("send");
        tx2.send(thinking()).expect("send");
        let token = cancelled.token.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            token.cancel();
        });
        let _ = stream_turn_events(
            &context,
            &incoming(),
            &mut rx2,
            &agent_cancel,
            &mut cancelled,
        )
        .await;
        cancel_task.await.expect("cancel task");
        assert!(agent_cancel.is_cancelled());

        tokio::time::sleep(STATUS_DEBOUNCE_PLUS).await;
        let edits = edits(&fake);
        assert_eq!(
            edits.len(),
            2,
            "one immediate edit per turn only: {edits:?}"
        );
        assert!(edits.iter().all(|body| !body.contains("Thinking")));
        drop(tx);
        drop(tx2);
    }

    const STATUS_DEBOUNCE_PLUS: Duration = Duration::from_millis(3300);
}
