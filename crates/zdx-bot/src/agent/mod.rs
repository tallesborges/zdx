use std::path::Path;

use anyhow::{Context, Result};
use zdx_core::config::Config;
use zdx_core::core::agent::{self, AgentEventRx, AgentOptions, ToolConfig};
use zdx_core::core::context::build_effective_system_prompt_with_paths_and_surface_rules;
use zdx_core::core::events::AgentEvent;
use zdx_core::core::thread_persistence::{self, Thread, ThreadEvent};
use zdx_core::providers::{ChatContentBlock, ChatMessage, MessageContent};

use crate::types::IncomingMessage;

pub(crate) const STATUS_WAITING: &str = "⏳ Waiting for model...";
pub(crate) const STATUS_THINKING: &str = "🧠 Thinking...";
pub(crate) const STATUS_WRITING: &str = "✍️ Writing reply...";
pub(crate) const STATUS_TRANSCRIBING: &str = "🎤 Transcribing audio...";

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn load_thread_state(thread_id: &str) -> Result<(Thread, Vec<ChatMessage>)> {
    let thread = Thread::with_id(thread_id.to_string()).context("open thread log")?;
    let messages =
        thread_persistence::load_thread_as_messages(thread_id).context("load thread history")?;
    Ok((thread, messages))
}

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn clear_thread_history(thread_id: &str) -> Result<()> {
    let thread = Thread::with_id(thread_id.to_string()).context("resolve thread log")?;
    let path = thread.path();
    if path.exists() {
        std::fs::remove_file(path).context("clear thread history")?;
    }
    Ok(())
}

///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn record_user_message(
    thread: &mut Thread,
    messages: &mut Vec<ChatMessage>,
    incoming: &IncomingMessage,
) -> Result<()> {
    let text = build_user_text(incoming);
    thread
        .append(&ThreadEvent::user_message(text.clone()))
        .context("append user message")?;

    if incoming.images.is_empty() {
        messages.push(ChatMessage::user(text));
        return Ok(());
    }

    let mut blocks = Vec::with_capacity(1 + incoming.images.len());
    blocks.push(ChatContentBlock::Text(text));
    for image in &incoming.images {
        blocks.push(ChatContentBlock::Image {
            mime_type: image.mime_type.clone(),
            data: image.data.clone(),
        });
    }

    messages.push(ChatMessage {
        role: "user".to_string(),
        phase: None,
        content: MessageContent::Blocks(blocks),
    });
    Ok(())
}

/// Handle to a running agent turn with streaming events.
///
/// The caller consumes events from `rx`. Thread persistence is handled
/// internally — the caller doesn't need to manage it.
pub(crate) struct AgentTurnHandle {
    /// Event stream for the caller to consume.
    pub rx: AgentEventRx,
    /// Task handle for the agent. Abort this on cancellation.
    pub task: tokio::task::JoinHandle<Result<(String, Vec<ChatMessage>)>>,
}

/// Spawns an agent turn and returns a handle with streaming events.
///
/// Thread persistence is wired internally via `spawn_broadcaster`.
/// The caller receives events through `AgentTurnHandle::rx` and should
/// look for `TurnFinished` to get the terminal result.
///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn spawn_agent_turn(
    messages: Vec<ChatMessage>,
    config: &Config,
    root: &Path,
    bot_surface_rules: Option<&str>,
    thread_id: &str,
    thread: &Thread,
    tool_config: &ToolConfig,
) -> Result<AgentTurnHandle> {
    // Set runtime env vars before building prompt (Slice 1: env-vars-runtime-context)
    zdx_core::core::context::set_runtime_env(config, Some(thread_id));

    // Build effective system prompt from config + AGENTS.md + memory + skills + optional surface rules.
    let effective = build_effective_system_prompt_with_paths_and_surface_rules(
        config,
        root,
        bot_surface_rules,
        true,
    )
    .context("build system prompt")?;
    let system_prompt = effective.prompt;

    let agent_opts = AgentOptions {
        root: root.to_path_buf(),
        tool_config: tool_config.clone(),
        surface: Some("telegram".to_string()),
    };

    // Create channels: agent -> broadcaster -> [bot, persist]
    let (agent_tx, agent_rx) = agent::create_event_channel();
    let (bot_tx, bot_rx) = agent::create_event_channel();
    let (persist_tx, persist_rx) = agent::create_event_channel();

    agent::spawn_broadcaster(agent_rx, vec![bot_tx, persist_tx]);
    thread_persistence::spawn_thread_persist_task(thread.clone(), persist_rx);

    // Spawn agent in background — owned values moved in
    let config = config.clone();
    let thread_id = thread_id.to_string();
    let task = tokio::spawn(async move {
        agent::run_turn(
            messages,
            &config,
            &agent_opts,
            system_prompt.as_deref(),
            Some(&thread_id),
            agent_tx,
        )
        .await
    });

    Ok(AgentTurnHandle { rx: bot_rx, task })
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
        AgentEvent::ToolStarted { name, .. } => {
            let emoji = match name.as_str() {
                "bash" => "🔧",
                "read" => "📖",
                "write" | "edit" | "apply_patch" => "✏️",
                "web_search" => "🔍",
                "fetch_webpage" => "🌐",
                "read_thread" => "💬",
                _ => "⚙️",
            };
            Some(format!("{emoji} Running `{name}`..."))
        }
        _ => None,
    }
}

fn build_user_text(incoming: &IncomingMessage) -> String {
    let mut parts = Vec::new();
    if let Some(text) = incoming.text.as_ref()
        && !text.trim().is_empty()
    {
        parts.push(text.clone());
    }

    for audio in &incoming.audios {
        if let Some(transcript) = &audio.transcript {
            parts.push(format!("Audio transcript:\n{transcript}"));
        } else {
            parts.push(format!(
                "Audio attachment saved at {} (transcription unavailable).",
                audio.local_path.display()
            ));
        }
    }

    for image in &incoming.images {
        parts.push(format!(
            "Image attachment saved at {}.",
            image.local_path.display()
        ));
    }

    for doc in &incoming.documents {
        parts.push(format!(
            "Document attachment '{}' saved at {}.",
            doc.file_name,
            doc.local_path.display()
        ));
    }

    if parts.is_empty() {
        "User sent an attachment.".to_string()
    } else {
        parts.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use zdx_core::core::events::{AgentEvent, ToolOutput};

    use super::{STATUS_THINKING, STATUS_WAITING, STATUS_WRITING, event_to_status};

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
            }),
            Some(STATUS_WAITING.to_string())
        );
    }
}
