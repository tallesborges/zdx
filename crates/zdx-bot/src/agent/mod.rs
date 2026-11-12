use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use zdx_engine::config::{Config, TextVerbosity};
use zdx_engine::core::agent::{self, AgentEventRx, AgentOptions, ToolConfig};
use zdx_engine::core::context::{
    PromptContextInclusion, RuntimeContext, build_prompt_with_context_and_layers,
};
use zdx_engine::core::events::AgentEvent;
use zdx_engine::core::thread_persistence::{self, Thread};
use zdx_engine::providers::ChatMessage;

use crate::types::{ImageDelivery, IncomingImage, IncomingMessage, VideoStatus};

pub(crate) const STATUS_WAITING: &str = "⏳ Waiting for model...";
pub(crate) const STATUS_THINKING: &str = "🧠 Thinking...";
pub(crate) const STATUS_WRITING: &str = "✍️ Writing reply...";
pub(crate) const STATUS_TRANSCRIBING: &str = "🎤 Transcribing audio...";

///
/// # Errors
/// Returns an error if the operation fails.
/// Opens a thread and replays its history. The third value is the runtime
/// note (undelivered parts of the last reply, messages other runs posted
/// here); pass it to [`record_user_message`] so the new user message carries
/// it, exactly as replay will rebuild it.
pub(crate) fn load_thread_state(
    thread_id: &str,
) -> Result<(Thread, Vec<ChatMessage>, Option<String>)> {
    let thread = Thread::with_id(thread_id.to_string()).context("open thread log")?;
    let replay =
        thread_persistence::load_thread_replay(thread_id).context("load thread history")?;
    Ok((thread, replay.messages, replay.trailing_note))
}

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn clear_thread_history(thread_id: &str) -> Result<()> {
    thread_persistence::remove_thread_file(thread_id).context("clear thread history")
}

/// Appends the incoming user message to the thread and the in-memory messages
/// (see `zdx_engine::orchestration::updates::record_user_input`).
///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn record_user_message(
    thread: &mut Thread,
    messages: &mut Vec<ChatMessage>,
    incoming: &IncomingMessage,
    runtime_context: Option<&RuntimeContext>,
    runtime_note: Option<&str>,
) -> Result<()> {
    zdx_engine::orchestration::updates::record_user_input(
        thread,
        messages,
        build_user_text(incoming),
        &inline_images(incoming),
        runtime_context,
        runtime_note,
    )
}

/// The images of a message the model sees inline.
pub(crate) fn inline_images(
    incoming: &IncomingMessage,
) -> Vec<zdx_engine::orchestration::updates::InputImage> {
    incoming
        .images
        .iter()
        .filter_map(IncomingImage::inline)
        .map(
            |(mime_type, data)| zdx_engine::orchestration::updates::InputImage {
                mime_type: mime_type.to_string(),
                data: data.to_string(),
            },
        )
        .collect()
}

/// Handle to a running agent turn with streaming events.
///
/// The caller consumes events from `rx`. Thread persistence is handled
/// internally, but the caller must `await_persisted()` before anything reads
/// the thread back from disk.
pub(crate) struct AgentTurnHandle {
    /// Event stream for the caller to consume.
    pub rx: AgentEventRx,
    /// Cancellation token for this agent turn.
    pub cancel: CancellationToken,
    /// Task handle kept alive for the running agent turn.
    pub _task: tokio::task::JoinHandle<Result<(String, Vec<ChatMessage>)>>,
    /// Persistence task for this turn. Resolves once every event has been
    /// appended to the thread log.
    persist: Option<tokio::task::JoinHandle<()>>,
}

impl AgentTurnHandle {
    /// Waits until this turn is fully written to the thread log.
    ///
    /// `TurnFinished` only means the agent stopped producing events. Persistence
    /// runs as a separate task fed by the same broadcast, so the log can still
    /// be missing the tail of the turn when streaming ends. Every bot turn
    /// rebuilds its history with `load_thread_state`, so the next turn would
    /// read a truncated conversation without this barrier.
    pub(crate) async fn await_persisted(&mut self) {
        if let Some(persist) = self.persist.take()
            && let Err(err) = persist.await
        {
            tracing::warn!(%err, "Thread persistence task failed");
        }
    }
}

pub(crate) struct PreparedBotTurn {
    pub(crate) config: Config,
    /// Execution root the turn runs in.
    pub(crate) root: PathBuf,
    pub(crate) system_prompt: Option<String>,
    /// Advisory runtime-context snapshot to attach to the next user message
    /// (initial on first attach, update-eligible replacement on a meaningful
    /// change), or `None` when there is nothing to snapshot.
    pub(crate) runtime_context: Option<RuntimeContext>,
}

fn bot_prompt_context() -> PromptContextInclusion {
    PromptContextInclusion {
        project_context: true,
        memory_index: true,
        skills: true,
    }
}

fn collect_bot_instruction_layers(bot_instruction_layer: Option<&str>) -> Vec<&str> {
    bot_instruction_layer.into_iter().collect()
}

pub(crate) fn prepare_bot_turn(
    config: &Config,
    root: &Path,
    bot_instruction_layer: Option<&str>,
) -> Result<PreparedBotTurn> {
    let bot_config = config.clone();
    let instruction_layers = collect_bot_instruction_layers(bot_instruction_layer);
    let effective = build_prompt_with_context_and_layers(
        &bot_config,
        root,
        &bot_config.model,
        &instruction_layers,
        bot_prompt_context(),
    )
    .context("build bot prompt")?;

    Ok(PreparedBotTurn {
        config: bot_config,
        root: root.to_path_buf(),
        system_prompt: effective.prompt,
        runtime_context: effective.runtime_context,
    })
}

/// Spawns an agent turn and returns a handle with streaming events.
///
/// Thread persistence is wired internally via `spawn_broadcaster`.
/// The caller receives events through `AgentTurnHandle::rx` and should
/// look for `TurnFinished` to get the terminal result.
pub(crate) fn spawn_agent_turn(
    messages: Vec<ChatMessage>,
    prepared: PreparedBotTurn,
    thread_id: &str,
    thread: &Thread,
    tool_config: &ToolConfig,
    steer: zdx_engine::core::steer::SteerInbox,
) -> AgentTurnHandle {
    let PreparedBotTurn {
        config: bot_config,
        root,
        system_prompt,
        runtime_context: _,
    } = prepared;
    let tool_config = tool_config.clone();

    let agent_opts = AgentOptions {
        conversation_id: None,
        root,
        tool_config,
        surface: Some("telegram".to_string()),
        text_verbosity: Some(TextVerbosity::Low),
        service_tier: None,
        activity_kind: Some("telegram".to_string()),
        activity_parent_thread_id: None,
        activity_subagent_name: None,
        tool_timeout: None,
        steer: Some(zdx_engine::core::steer::SteerSource::Inbox(steer)),
    };

    // Create channels: agent -> broadcaster -> [bot, persist]
    let (agent_tx, agent_rx) = agent::create_event_channel();
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let (bot_tx, bot_rx) = agent::create_event_channel();
    let (persist_tx, persist_rx) = agent::create_event_channel();

    agent::spawn_broadcaster(agent_rx, vec![bot_tx, persist_tx]);
    let persist = thread_persistence::spawn_thread_persist_task(thread.clone(), persist_rx);

    // Spawn agent in background — owned values moved in
    let config = bot_config;
    let thread_id = thread_id.to_string();
    let task = tokio::spawn(async move {
        agent::run_turn_with_cancel(
            messages,
            &config,
            &agent_opts,
            system_prompt.as_deref(),
            Some(&thread_id),
            agent_tx,
            Some(run_cancel),
        )
        .await
    });

    AgentTurnHandle {
        rx: bot_rx,
        cancel,
        _task: task,
        persist: Some(persist),
    }
}

/// Maps an `AgentEvent` to a short status emoji + label for Telegram display.
pub(crate) fn event_to_status(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::TurnStarted
        | AgentEvent::ReasoningCompleted { .. }
        | AgentEvent::ToolCompleted { .. } => Some(STATUS_WAITING.to_string()),
        AgentEvent::ReasoningDelta { .. } => Some(STATUS_THINKING.to_string()),
        AgentEvent::AssistantDelta { .. } | AgentEvent::AssistantCompleted { .. } => {
            Some(STATUS_WRITING.to_string())
        }
        AgentEvent::ToolRequested { name, .. } => Some(format!("⚙️ Preparing `{name}`...")),
        AgentEvent::ToolStarted { name, .. } => Some(tool_running_status(name)),
        _ => None,
    }
}

/// One-line "running tool" status shared by turn status messages and legacy
/// worker mirror topics.
pub(crate) fn tool_running_status(name: &str) -> String {
    let emoji = match name {
        "bash" => "🔧",
        "read" => "📖",
        "write" | "edit" | "apply_patch" => "✏️",
        "web_search" => "🔍",
        "fetch_webpage" => "🌐",
        "read_thread" => "💬",
        _ => "⚙️",
    };
    format!("{emoji} Running `{name}`...")
}

/// The replied-to message, placed before the user's words.
fn reply_to_block(reply: &crate::types::RepliedTo) -> String {
    let sender = if reply.from_bot {
        "the bot (or another run posting through it)"
    } else {
        "a user"
    };
    let what = if reply.quoted {
        "quotes this part of"
    } else {
        "replies to"
    };
    format!(
        "<reply_to>\nThe user's message below {what} message {} in this chat, sent by {sender}:\n{}\n</reply_to>",
        reply.message_id, reply.text
    )
}

pub(crate) fn build_user_text(incoming: &IncomingMessage) -> String {
    let mut parts = Vec::new();
    if let Some(reply) = incoming.replied_to.as_ref() {
        parts.push(reply_to_block(reply));
    }
    if let Some(text) = incoming.text.as_ref()
        && !text.trim().is_empty()
    {
        parts.push(text.clone());
    }

    for audio in &incoming.audios {
        // A labelled clip (Mini App feedback) names the note it belongs to,
        // so the transcript stays attached to it.
        let prefix = audio
            .label
            .as_ref()
            .map_or_else(String::new, |label| format!("{label}: "));
        if let Some(transcript) = &audio.transcript {
            if !prefix.is_empty() {
                parts.push(format!("{prefix}transcript"));
            }
            parts.push(zdx_engine::providers::wrap_voice_transcript(transcript));
            parts.push(format!(
                "(Audio file saved at {} — use the `ask_media` tool on it if you need more than the transcript.)",
                audio.local_path.display()
            ));
        } else {
            parts.push(format!(
                "{prefix}Audio attachment saved at {} (transcription unavailable).",
                audio.local_path.display()
            ));
        }
    }

    for image in &incoming.images {
        let path = image.local_path.display();
        let (prefix, subject) = match &image.label {
            Some(label) => (format!("{label}: "), "image attachment"),
            None => (String::new(), "Image attachment"),
        };
        let text = match &image.delivery {
            ImageDelivery::Inline {
                downscaled: false, ..
            } => format!("{subject} saved at {path}."),
            ImageDelivery::Inline {
                downscaled: true, ..
            } => format!(
                "{subject} saved at {path}. You see a downscaled copy; the saved file is the full-resolution original — use `ask_media` on it if you need fine detail."
            ),
            ImageDelivery::SavedOnly {
                reason,
                ask_media: true,
            } => format!(
                "{subject} saved at {path}, but it was not attached for you to see: {reason}. Use the `ask_media` tool on the saved file to inspect it."
            ),
            ImageDelivery::SavedOnly {
                reason,
                ask_media: false,
            } => format!(
                "{subject} saved at {path}, but it was not attached for you to see: {reason}. `ask_media` cannot read this file either; ask the user to resend it as a PNG or JPEG."
            ),
        };
        parts.push(format!("{prefix}{text}"));
    }

    for doc in &incoming.documents {
        parts.push(format!(
            "Document attachment '{}' saved at {} — use the `ask_media` tool on it to read its contents.",
            doc.file_name,
            doc.local_path.display()
        ));
    }

    for video in &incoming.videos {
        parts.push(match &video.status {
            VideoStatus::Saved(path) => format!(
                "{} attachment saved at {} — use the `ask_media` tool on it to watch it.",
                video.kind,
                path.display()
            ),
            VideoStatus::TooLarge { size_bytes } => format!(
                "{} attachment was not downloaded: it is {:.1} MiB and the limit is {} MiB. Ask the user for a shorter or compressed clip.",
                video.kind,
                *size_bytes as f64 / (1024.0 * 1024.0),
                zdx_engine::media::MAX_INLINE_BYTES / (1024 * 1024)
            ),
        });
    }

    parts.extend(incoming.notices.iter().cloned());

    if parts.is_empty() {
        "User sent an attachment.".to_string()
    } else {
        parts.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use zdx_engine::config::{Config, SkillSourceToggles};
    use zdx_engine::core::events::{AgentEvent, ToolOutput};

    use super::{
        STATUS_THINKING, STATUS_WAITING, STATUS_WRITING, event_to_status, prepare_bot_turn,
    };

    fn make_temp_dir() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "zdx-bot-agent-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn maps_waiting_thinking_and_writing_states() {
        assert_eq!(
            event_to_status(&AgentEvent::TurnStarted),
            Some(STATUS_WAITING.to_string())
        );
        assert_eq!(
            event_to_status(&AgentEvent::ReasoningDelta {
                text: "hmm".to_string()
            }),
            Some(STATUS_THINKING.to_string())
        );
        assert_eq!(
            event_to_status(&AgentEvent::AssistantDelta {
                text: "hello".to_string()
            }),
            Some(STATUS_WRITING.to_string())
        );
    }

    #[test]
    fn maps_tool_lifecycle_states() {
        assert_eq!(
            event_to_status(&AgentEvent::ToolRequested {
                id: "1".to_string(),
                name: "read".to_string(),
                input: json!({}),
            }),
            Some("⚙️ Preparing `read`...".to_string())
        );
        assert_eq!(
            event_to_status(&AgentEvent::ToolStarted {
                id: "1".to_string(),
                name: "read".to_string(),
            }),
            Some("📖 Running `read`...".to_string())
        );
        assert_eq!(
            event_to_status(&AgentEvent::ToolCompleted {
                id: "1".to_string(),
                result: ToolOutput::success(json!({ "ok": true })),
                duration_ms: Some(1),
                started_at: None,
                completed_at: None,
            }),
            Some(STATUS_WAITING.to_string())
        );
    }

    #[test]
    fn prepare_bot_turn_includes_project_context() {
        // Rendering skills materializes bundled skills into $ZDX_HOME.
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = make_temp_dir();
        std::fs::write(dir.join("AGENTS.md"), "Bot project note").unwrap();

        let mut config = Config {
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let prepared = prepare_bot_turn(&config, &dir, None).unwrap();
        let prompt = prepared.system_prompt.unwrap_or_default();

        assert!(prompt.contains("Bot project note"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A failed delivery must reach the model on the next user message, and
    /// the live message must equal what replay rebuilds on the turn after.
    #[test]
    fn delivery_note_is_live_and_replayed_identically() {
        use zdx_engine::core::delivery::{DeliveryItem, DeliveryRecord};
        use zdx_engine::core::thread_persistence::{self, ThreadEvent};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let id = format!("test-delivery-{}", uuid::Uuid::new_v4());
        {
            let mut thread =
                zdx_engine::core::thread_persistence::Thread::with_id(id.clone()).expect("thread");
            thread
                .append(&ThreadEvent::user_message("make it"))
                .expect("append");
            thread
                .append(&ThreadEvent::assistant_message(
                    "Ready <media>/tmp/x.html</media>",
                ))
                .expect("append");
            let mut file = DeliveryItem::file("/tmp/x.html");
            file.mark_failed("Telegram API error for sendDocument (429): Too Many Requests");
            thread
                .append(&ThreadEvent::delivery(DeliveryRecord::new(
                    "telegram",
                    Some(-100),
                    Some(7),
                    vec![file],
                )))
                .expect("append");
        }

        let (mut thread, mut messages, note) = super::load_thread_state(&id).expect("load");
        let note = note.expect("delivery note");
        assert!(note.contains("file /tmp/x.html: not delivered"));
        let incoming = crate::types::IncomingMessage {
            chat_id: -100,
            message_id: 5,
            user_id: 1,
            text: Some("where is it?".to_string()),
            images: Vec::new(),
            audios: Vec::new(),
            documents: Vec::new(),
            videos: Vec::new(),
            notices: Vec::new(),
            message_thread_id: Some(7),
            is_forum: true,
            replied_to: None,
        };
        super::record_user_message(&mut thread, &mut messages, &incoming, None, Some(&note))
            .expect("record");

        let replayed = thread_persistence::load_thread_replay(&id).expect("replay");
        assert_eq!(replayed.trailing_note, None);
        assert_eq!(
            format!("{:?}", replayed.messages.last().map(|m| &m.content)),
            format!("{:?}", messages.last().map(|m| &m.content)),
        );
    }

    /// A job posts an alert into a topic the bot never ran in. The user's
    /// first ordinary message there runs with the alert, its source thread,
    /// and the sender's context, and replay rebuilds the same message.
    #[test]
    fn a_post_by_another_run_reaches_the_next_user_message() {
        use zdx_engine::core::thread_persistence;
        use zdx_engine::telegram::{PostDetails, SentMessage, record_post};

        let _home = zdx_engine::test_support::temp_zdx_home();
        let sent = SentMessage {
            chat_id: -1_001_234_567_890,
            message_thread_id: Some(69),
            message_id: 70,
            url: Some("https://t.me/c/1234567890/69/70".to_string()),
        };
        let recorded = record_post(
            &sent,
            PostDetails {
                text: Some("⏰ Pay the cleaner: due 30/10 at 15:00"),
                context: Some("Expected net amount: R$ 100,00, to be confirmed by the user."),
                source_thread_id: Some("job-example-20260101-080000"),
                ..PostDetails::default()
            },
        )
        .expect("record");
        let id = crate::handlers::message::thread_id_for_chat(-1_001_234_567_890, Some(69));
        assert_eq!(recorded.as_deref(), Some(id.as_str()));

        let (mut thread, mut messages, note) = super::load_thread_state(&id).expect("load");
        assert!(messages.is_empty(), "a post is not a conversation turn");
        let note = note.expect("posted note");
        let incoming = crate::types::IncomingMessage {
            chat_id: -1_001_234_567_890,
            message_id: 71,
            user_id: 1,
            text: Some("how much?".to_string()),
            images: Vec::new(),
            audios: Vec::new(),
            documents: Vec::new(),
            videos: Vec::new(),
            notices: Vec::new(),
            message_thread_id: Some(69),
            is_forum: true,
            replied_to: None,
        };
        super::record_user_message(&mut thread, &mut messages, &incoming, None, Some(&note))
            .expect("record");

        assert_eq!(messages.len(), 1);
        let live = format!("{:?}", messages[0].content);
        for expected in [
            "<posted_messages>",
            "Message 70 posted at",
            "by thread job-example-20260101-080000",
            "⏰ Pay the cleaner: due 30/10 at 15:00",
            "Expected net amount: R$ 100,00, to be confirmed by the user.",
            "how much?",
        ] {
            assert!(live.contains(expected), "missing {expected:?} in {live}");
        }
        let replayed = thread_persistence::load_thread_replay(&id).expect("replay");
        assert_eq!(replayed.trailing_note, None);
        assert_eq!(format!("{:?}", replayed.messages[0].content), live);
    }
}
