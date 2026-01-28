use std::path::Path;

use anyhow::{Result, anyhow};
use zdx_core::config::Config;
use zdx_core::core::agent::{self, AgentOptions, ToolConfig};
use zdx_core::core::context::build_effective_system_prompt_with_paths;
use zdx_core::core::thread_log::{self, ThreadEvent, ThreadLog};
use zdx_core::providers::{ChatContentBlock, ChatMessage, MessageContent};

use crate::types::IncomingMessage;

pub(crate) fn load_thread_state(thread_id: &str) -> Result<(ThreadLog, Vec<ChatMessage>)> {
    let thread = ThreadLog::with_id(thread_id.to_string())
        .map_err(|_| anyhow!("Failed to open thread log"))?;
    let messages = thread_log::load_thread_as_messages(thread_id)
        .map_err(|_| anyhow!("Failed to load thread history"))?;
    Ok((thread, messages))
}

pub(crate) fn clear_thread_history(thread_id: &str) -> Result<()> {
    let thread = ThreadLog::with_id(thread_id.to_string())
        .map_err(|_| anyhow!("Failed to resolve thread log"))?;
    let path = thread.path();
    if path.exists() {
        std::fs::remove_file(path).map_err(|_| anyhow!("Failed to clear thread history"))?;
    }
    Ok(())
}

pub(crate) fn record_user_message(
    thread: &mut ThreadLog,
    messages: &mut Vec<ChatMessage>,
    incoming: &IncomingMessage,
) -> Result<()> {
    let text = build_user_text(incoming);
    thread
        .append(&ThreadEvent::user_message(text.clone()))
        .map_err(|_| anyhow!("Failed to append user message"))?;

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
        content: MessageContent::Blocks(blocks),
    });
    Ok(())
}

pub(crate) async fn run_agent_turn_with_persist(
    messages: Vec<ChatMessage>,
    config: &Config,
    root: &Path,
    bot_system_prompt: Option<&str>,
    thread_id: &str,
    thread: &ThreadLog,
    tool_config: &ToolConfig,
) -> Result<(String, Vec<ChatMessage>)> {
    // Build effective system prompt from config + AGENTS.md + skills
    let effective = build_effective_system_prompt_with_paths(config, root)
        .map_err(|_| anyhow!("Failed to build system prompt"))?;

    // Append bot-specific prompt if provided
    let system_prompt = match (effective.prompt, bot_system_prompt) {
        (Some(base), Some(bot)) => Some(format!("{}\n\n{}", base, bot)),
        (Some(base), None) => Some(base),
        (None, Some(bot)) => Some(bot.to_string()),
        (None, None) => None,
    };

    let agent_opts = AgentOptions {
        root: root.to_path_buf(),
        tool_config: tool_config.clone(),
    };

    let (agent_tx, agent_rx) = agent::create_event_channel();
    let persist_handle = thread_log::spawn_thread_persist_task(thread.clone(), agent_rx);

    let result = agent::run_turn(
        messages,
        config,
        &agent_opts,
        system_prompt.as_deref(),
        Some(thread_id),
        agent_tx,
    )
    .await;

    let _ = persist_handle.await;

    result
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
            parts.push(format!("Audio transcript:\n{}", transcript));
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

    if parts.is_empty() {
        "User sent an attachment.".to_string()
    } else {
        parts.join("\n\n")
    }
}
