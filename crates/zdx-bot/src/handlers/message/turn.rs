use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zdx_engine::core::agent::AgentEventRx;
use zdx_engine::core::events::{AgentEvent, ErrorKind, TurnStatus as AgentTurnStatus};
use zdx_engine::core::thread_persistence;
use zdx_engine::orchestration::OrchestratorClient;
use zdx_engine::orchestration::client::ok;
use zdx_engine::orchestration::protocol::{Event, Follow, Reply, Request, Route, Submit};

use super::response::{TouchedWorker, send_final_response};
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
    // A launcher orchestrator topic whose setup failed has no profile, so a
    // turn would run the default coding agent. Refuse it instead.
    if super::launcher::orchestrator_setup_failed(context, thread_id) {
        super::cleanup_provisional_status(context, Some(incoming.chat_id), provisional_status)
            .await;
        if let Err(err) = context
            .client()
            .send_message(
                incoming.chat_id,
                super::launcher::UNSETUP_TOPIC_NOTICE,
                reply_ctx.reply_to_message_id,
                reply_ctx.topic_id,
            )
            .await
        {
            tracing::warn!(thread_id, err = %format!("{err:#}"), "Failed to refuse a turn in an orchestrator topic whose setup failed");
        }
        return Ok(());
    }
    // Persistent-profile threads (orchestrator home bases) run in the
    // orchestrator service, their only writer; this process renders them.
    if thread_persistence::read_persistent_profile(thread_id)?.is_some() {
        return run_service_turn(
            context,
            incoming,
            reply_ctx,
            thread_id,
            synthetic_topic_routed_from_general,
            provisional_status,
            record_user,
        )
        .await;
    }
    let resolved_root = context.root_for_chat(incoming.chat_id);
    let stored_root = thread_persistence::read_thread_root_path(thread_id)?;
    let worktree_root = stored_root
        .clone()
        .map_or_else(|| resolved_root.root.clone(), std::path::PathBuf::from);
    let model_override = thread_persistence::read_thread_model_override(thread_id)?;
    let thinking_override = thread_persistence::read_thread_thinking_override(thread_id)?;
    let mut config = context.config_for_chat(incoming.chat_id);
    config.apply_thread_model_override(model_override.as_deref(), thinking_override);
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
    let prepared =
        match agent::prepare_bot_turn(&config, &worktree_root, context.bot_instruction_layer()) {
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
    // Runs only after the user message is persisted, so the thread file exists.
    spawn_topic_title(
        context,
        &incoming,
        &reply_ctx,
        &mut thread,
        synthetic_topic_routed_from_general,
        pending_topic_title,
    )?;

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
        false,
    )
    .await
}

/// Async LLM topic title for a new topic (and Threaded Mode DM threads, which
/// are otherwise left as "New Thread" or a truncated first line).
fn spawn_topic_title(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    thread: &mut thread_persistence::Thread,
    synthetic_topic_routed_from_general: bool,
    pending_topic_title: bool,
) -> Result<()> {
    if !(synthetic_topic_routed_from_general || pending_topic_title) {
        return Ok(());
    }
    let Some(topic_id) = reply_ctx.topic_id else {
        return Ok(());
    };
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
    Ok(())
}

/// Sends a message to a persistent-profile thread in the orchestrator service
/// and renders the turn that runs it, exactly like a local turn: status line,
/// cancel, final reply, delivery records.
async fn run_service_turn(
    context: &BotContext,
    incoming: crate::types::IncomingMessage,
    reply_ctx: ReplyContext,
    thread_id: &str,
    synthetic_topic_routed_from_general: bool,
    provisional_status: Option<TurnStatus>,
    record_user: bool,
) -> Result<()> {
    if !record_user {
        // A retry re-runs persisted state in this process, which would make
        // the bot a second writer of the thread.
        let _ = context
            .client()
            .send_message(
                incoming.chat_id,
                "Retry is not available here: this thread runs in the orchestrator service. Send the message again.",
                reply_ctx.reply_to_message_id,
                reply_ctx.topic_id,
            )
            .await;
        return Ok(());
    }
    // The service has not run anything on a new topic yet, so this meta
    // rewrite cannot race one of its turns.
    let pending_topic_title = thread_persistence::read_thread_pending_topic_title(thread_id)?;
    if synthetic_topic_routed_from_general || pending_topic_title {
        let mut thread = thread_persistence::Thread::with_id(thread_id.to_string())?;
        spawn_topic_title(
            context,
            &incoming,
            &reply_ctx,
            &mut thread,
            synthetic_topic_routed_from_general,
            pending_topic_title,
        )?;
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
    let submit = Submit {
        thread_id: Some(thread_id.to_string()),
        root: context.root_for_chat(incoming.chat_id).root,
        profile: None,
        model: None,
        route: Some(Route {
            chat: incoming.chat_id,
            topic: reply_ctx.topic_id,
        }),
        text: agent::build_user_text(&incoming),
        images: agent::inline_images(&incoming),
        follow: Follow::Turn,
    };
    let turn = match ServiceTurn::submit(context.orchestrator(), thread_id, submit).await {
        Ok(turn) => turn,
        Err(err) => {
            drop(typing);
            tracing::warn!(thread = %thread_id, err = %format!("{err:#}"), "Orchestrator service refused the message");
            cleanup_turn_status(context, &status).await;
            if let Some(message_id) = status.message_id {
                let _ = context
                    .client()
                    .edit_message_text(
                        incoming.chat_id,
                        message_id,
                        &format_user_error_message(&format!("{err:#}")),
                        Some(&InlineKeyboardMarkup::empty()),
                    )
                    .await;
            }
            return Ok(());
        }
    };
    let result = render_service_turn(context, &incoming, &reply_ctx, thread_id, status, turn).await;
    drop(typing);
    result
}

/// Renders an orchestrator turn the service started on its own (a worker
/// update) in its Telegram route, like a turn the bot submitted.
pub(crate) async fn render_started_turn(
    context: Arc<BotContext>,
    thread_id: String,
    turn_id: u64,
    route: Route,
    events: mpsc::UnboundedReceiver<Event>,
) {
    let incoming = crate::types::IncomingMessage {
        chat_id: route.chat,
        message_id: crate::bot::synthetic::next_synthetic_id(),
        user_id: 0,
        text: None,
        images: Vec::new(),
        audios: Vec::new(),
        documents: Vec::new(),
        videos: Vec::new(),
        notices: Vec::new(),
        message_thread_id: route.topic,
        is_forum: route.topic.is_some(),
        replied_to: None,
    };
    // Nothing to quote: no user message started this turn.
    let reply_ctx = ReplyContext {
        reply_to_message_id: None,
        topic_id: route.topic,
        cross_topic_reply_parameters: None,
    };
    let typing = context.client().start_typing(route.chat, route.topic);
    let status = setup_turn_status(&context, &incoming, None, route.topic, &thread_id, None).await;
    let turn = ServiceTurn::pump(
        context.orchestrator().clone(),
        thread_id.clone(),
        CancelBy::Turn(turn_id),
        events,
    );
    if let Err(err) =
        render_service_turn(&context, &incoming, &reply_ctx, &thread_id, status, turn).await
    {
        tracing::warn!(thread = %thread_id, turn_id, err = %format!("{err:#}"), "Failed to render orchestrator turn");
    }
    drop(typing);
}

async fn render_service_turn(
    context: &BotContext,
    incoming: &crate::types::IncomingMessage,
    reply_ctx: &ReplyContext,
    thread_id: &str,
    mut status: TurnStatus,
    mut turn: ServiceTurn,
) -> Result<()> {
    let result =
        stream_turn_events(context, incoming, &mut turn.rx, &turn.cancel, &mut status).await;
    // The reply's delivery records go after the turn's own events.
    let ended = turn.ended.await.ok().flatten();
    cleanup_turn_status(context, &status).await;
    let outcome = match thread_persistence::Thread::with_id(thread_id.to_string()) {
        Ok(mut thread) => {
            finalize_turn(
                context,
                incoming,
                reply_ctx,
                thread_id,
                &mut thread,
                &status,
                result,
                true,
            )
            .await
        }
        Err(err) => Err(err),
    };
    // Released whatever happened above: the thread must not wait for a
    // delivery that will not come.
    if let Some((turn_id, true)) = ended
        && let Err(err) = context
            .orchestrator()
            .call(&Request::Release {
                thread_id: thread_id.to_string(),
                turn_id,
            })
            .await
    {
        tracing::warn!(thread = %thread_id, turn_id, err = %format!("{err:#}"), "Failed to release orchestrator turn");
    }
    outcome
}

/// Which work a cancel from the turn's status message stops.
#[derive(Clone, Copy)]
enum CancelBy {
    /// The bot's own message: its turn, or the message while still queued.
    Input(u64),
    Turn(u64),
}

/// One orchestrator turn seen through the service's events, shaped like a
/// local turn: agent events on `rx`, `cancel` to stop it, and `ended` once the
/// turn is persisted (its id and whether the service waits for a release;
/// `None` when no turn ran).
struct ServiceTurn {
    rx: AgentEventRx,
    cancel: CancellationToken,
    ended: oneshot::Receiver<Option<(u64, bool)>>,
}

impl ServiceTurn {
    /// Submits a message and follows the turn that runs it.
    async fn submit(client: &OrchestratorClient, thread_id: &str, submit: Submit) -> Result<Self> {
        let mut stream = client.open(&Request::Submit(submit)).await?;
        let first = stream
            .next()
            .await?
            .context("the orchestrator service closed the connection")?;
        let Reply::Submitted { input_id, .. } = ok(first)? else {
            bail!("unexpected reply from the orchestrator service");
        };
        let (tx, events) = mpsc::unbounded_channel();
        // Read until the service closes the connection: it holds a routed
        // turn for this connection until the reply is delivered and released,
        // so closing early would let the next turn start first.
        tokio::spawn(async move {
            while let Ok(Some(reply)) = stream.next().await {
                if let Reply::Event { event } = reply {
                    let _ = tx.send(event);
                }
            }
        });
        Ok(Self::pump(
            client.clone(),
            thread_id.to_string(),
            CancelBy::Input(input_id),
            events,
        ))
    }

    /// Turns service events of one turn into agent events. A turn that never
    /// reports a result (the message was dropped, or the service went away)
    /// ends with a failure, so the user sees why.
    fn pump(
        client: OrchestratorClient,
        thread_id: String,
        cancel_by: CancelBy,
        mut events: mpsc::UnboundedReceiver<Event>,
    ) -> Self {
        let (tx, rx) = zdx_engine::core::agent::create_event_channel();
        let (ended_tx, ended) = oneshot::channel();
        let cancel = CancellationToken::new();
        let watched = cancel.clone();
        tokio::spawn(async move {
            let mut finished = false;
            let mut cancel_sent = false;
            let mut ran = None;
            let failure = loop {
                let event = tokio::select! {
                    event = events.recv() => event,
                    () = watched.cancelled(), if !cancel_sent => {
                        cancel_sent = true;
                        let (input_id, turn_id) = match cancel_by {
                            CancelBy::Input(id) => (Some(id), None),
                            CancelBy::Turn(id) => (None, Some(id)),
                        };
                        let client = client.clone();
                        let request = Request::Cancel {
                            thread_id: thread_id.clone(),
                            input_id,
                            turn_id,
                            clear_queue: false,
                        };
                        tokio::spawn(async move {
                            if let Err(err) = client.call(&request).await {
                                tracing::warn!(err = %format!("{err:#}"), "Failed to cancel orchestrator turn");
                            }
                        });
                        continue;
                    }
                };
                match event {
                    Some(Event::Agent { event, .. }) => {
                        finished |= matches!(event, AgentEvent::TurnFinished { .. });
                        let _ = tx.send(Arc::new(event));
                    }
                    Some(Event::TurnEnded { turn_id, held, .. }) => {
                        ran = Some((turn_id, held));
                        break None;
                    }
                    Some(Event::InputsDropped { .. }) => {
                        break Some("The orchestrator service dropped this message before it ran.");
                    }
                    Some(_) => {}
                    None => break Some("Lost the connection to the orchestrator service."),
                }
            };
            if !finished && let Some(message) = failure {
                let status = if cancel_sent {
                    AgentTurnStatus::Interrupted
                } else {
                    AgentTurnStatus::Failed {
                        kind: ErrorKind::Internal,
                        message: message.to_string(),
                        details: None,
                    }
                };
                let _ = tx.send(Arc::new(AgentEvent::TurnFinished {
                    status,
                    final_text: String::new(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            }
            let _ = ended_tx.send(ran);
        });
        Self { rx, cancel, ended }
    }
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

    let mut touched_workers = Vec::with_capacity(result.touched_workers.len());
    for id in result.touched_workers {
        let mirror_url = context
            .orchestrator()
            .worker(&id)
            .await
            .and_then(|worker| worker.mirror_url);
        touched_workers.push(TouchedWorker { id, mirror_url });
    }
    send_final_response(
        context,
        incoming,
        reply_ctx,
        status.message_id,
        &result.final_text,
        thread_id,
        &touched_workers,
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

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// End to end through a real orchestrator service on a socket: a message
    /// in an orchestrator topic runs in the service and the bot renders it;
    /// the worker it starts finishes, the service starts the review turn on
    /// its own, and the bot's renderer posts the notice and that reply too.
    /// Both turns are released, so the service settles.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)]
    async fn an_orchestrator_topic_runs_in_the_service_and_the_bot_renders_both_turns() {
        use zdx_engine::core::thread_persistence::{Thread, ThreadEvent, load_thread_events};
        use zdx_engine::core::workers::{WorkerManager, WorkerRunRequest};
        use zdx_engine::orchestration::protocol::Request;
        use zdx_engine::orchestration::runtime::{TurnRequest, TurnRunner};
        use zdx_engine::orchestration::{Orchestrator, OrchestratorClient, server};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let fake = FakeTelegram::start().await;
        // Short path: Unix socket paths are limited to about 100 bytes.
        let dir_path = std::env::temp_dir().join(format!(
            "zdx-o-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&dir_path).expect("dir");
        let dir = TempDir(dir_path);
        let socket = dir.path().join("o.sock");
        let (workers, events) = WorkerManager::with_runner(Arc::new(|_: WorkerRunRequest| {
            Box::pin(async { Ok("worker finished the task".to_string()) })
        }));
        let delegate = Arc::clone(&workers);
        let root = dir.path().to_path_buf();
        let runner: TurnRunner = Arc::new(move |request: TurnRequest| {
            let workers = Arc::clone(&delegate);
            let root = root.clone();
            Box::pin(async move {
                let reply = if request.text.starts_with("[worker update]") {
                    "Review: the worker is done.".to_string()
                } else {
                    workers
                        .create_worker(&request.thread_id, &root, "do it", None, None, None)
                        .expect("worker");
                    "Started a worker.".to_string()
                };
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: AgentTurnStatus::Completed,
                    final_text: reply,
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        });
        let orchestrator = Orchestrator::start_with(workers, events, runner);
        tokio::spawn(server::serve(orchestrator, socket.clone()));
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let client = OrchestratorClient::at(&socket);
        let context = Arc::new(
            BotContext::for_test(
                TelegramClient::with_base_url(TOKEN.to_string(), fake.base_url.clone()),
                zdx_engine::config::Config::default(),
                dir.path().to_path_buf(),
            )
            .with_orchestrator(client.clone()),
        );
        crate::orchestrator::spawn_service_bridge(Arc::clone(&context));
        let thread_id = crate::handlers::message::thread_id_for_chat(CHAT, Some(7));
        Thread::with_id(thread_id.clone())
            .expect("thread")
            .set_persistent_profile("orchestrator")
            .expect("profile");
        // Let the bridge subscribe as the renderer first.
        tokio::time::sleep(Duration::from_millis(300)).await;

        super::run_agent_turn(
            &context,
            incoming(),
            super::ReplyContext {
                reply_to_message_id: Some(42),
                topic_id: Some(7),
                cross_topic_reply_parameters: None,
            },
            &thread_id,
            false,
            None,
            true,
        )
        .await
        .expect("turn");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let sent: Vec<String> = fake
                .seen()
                .into_iter()
                .filter(|seen| seen.method == "sendMessage")
                .map(|seen| seen.body)
                .collect();
            let settled = matches!(
                client.call(&Request::Status { thread_id: None }).await,
                Ok(zdx_engine::orchestration::protocol::Reply::Status { report }) if report.settled
            );
            if settled
                && sent
                    .iter()
                    .any(|body| body.contains("Review: the worker is done."))
            {
                assert!(sent.iter().any(|body| body.contains("Started a worker.")));
                assert!(
                    sent.iter()
                        .any(|body| body.contains("finished · reviewing"))
                );
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never rendered and settled: {sent:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // The bot recorded both deliveries in the thread the service wrote.
        let deliveries = load_thread_events(&thread_id)
            .expect("events")
            .into_iter()
            .filter(|event| matches!(event, ThreadEvent::Delivery { .. }))
            .count();
        assert!(deliveries >= 2, "{deliveries} delivery records");
    }
}
