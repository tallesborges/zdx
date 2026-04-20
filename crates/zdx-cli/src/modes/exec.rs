//! Structured streaming rendering and exec wrapper.
//!
//! This module provides:
//! - `ExecRenderer` + `spawn_exec_renderer_task` for JSONL agent events
//! - `run_exec` for single-shot exec mode

use std::io::{Stdout, Write, stdout};
use std::path::PathBuf;

use anyhow::Result;
use tokio::task::JoinHandle;
use tracing::{info, warn};
use zdx_engine::config::Config;
use zdx_engine::core::agent::{AgentOptions, ToolConfig};
use zdx_engine::core::events::{AgentEvent, TurnStatus};
use zdx_engine::core::thread_persistence::{self, Thread, ThreadEvent};
use zdx_engine::providers::ChatMessage;

const EXEC_INSTRUCTION_LAYER: &str = zdx_engine::prompts::EXEC_INSTRUCTION_LAYER;

fn exec_instruction_layers() -> Vec<&'static str> {
    let trimmed_instruction_layer = EXEC_INSTRUCTION_LAYER.trim();
    (!trimmed_instruction_layer.is_empty())
        .then_some(trimmed_instruction_layer)
        .into_iter()
        .collect()
}

/// Options for exec execution.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
    /// Tool configuration.
    pub tool_config: ToolConfig,
    /// Optional event type filters to emit.
    pub event_filter: Vec<String>,
    /// Optional fully-rendered system prompt override.
    pub effective_system_prompt: Option<String>,
    /// Disable all system prompt/context composition.
    pub no_system_prompt: bool,
}

impl From<&ExecOptions> for AgentOptions {
    fn from(opts: &ExecOptions) -> Self {
        AgentOptions {
            root: opts.root.clone(),
            tool_config: opts.tool_config.clone(),
            surface: Some("exec".to_string()),
            text_verbosity: None,
            service_tier: None,
        }
    }
}

/// Sends a prompt to the LLM and streams JSONL events to stdout.
///
/// If a thread is provided, logs the user prompt and final assistant response,
/// plus `tool_use` and `tool_result` events for full history.
/// Implements tool loop - if the model requests tools, executes them and continues.
/// Returns the complete response text.
///
/// Logs effective context info (project context files, skills) at startup.
fn log_effective_context(effective: &zdx_engine::core::context::EffectivePrompt) {
    if !effective.loaded_agents_paths.is_empty() {
        let paths_str: Vec<String> = effective
            .loaded_agents_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        info!(paths = %paths_str.join(", "), "exec inlined project context files");
    }
    if !effective.scoped_context_paths.is_empty() {
        let paths_str: Vec<String> = effective
            .scoped_context_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        info!(paths = %paths_str.join(", "), "exec scoped project context files");
    }
    if !effective.loaded_skills.is_empty() {
        let names: Vec<String> = effective
            .loaded_skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        info!(skills = %names.join(", "), "exec loaded skills");
    }
}

/// This is a backward-compatible wrapper that uses the agent internally.
pub async fn run_exec(
    prompt: &str,
    config: &Config,
    mut thread: Option<Thread>,
    options: &ExecOptions,
) -> Result<String> {
    let thread_id_ref = thread.as_ref().map(|t| t.id.as_str());

    // Set runtime env vars before building prompt (Slice 1: env-vars-runtime-context)
    zdx_engine::core::context::set_runtime_env(config, thread_id_ref);

    let effective = if options.no_system_prompt {
        None
    } else if let Some(prompt) = options.effective_system_prompt.as_ref() {
        Some(zdx_engine::core::context::EffectivePrompt {
            prompt: Some(prompt.clone()),
            loaded_agents_paths: Vec::new(),
            scoped_context_paths: Vec::new(),
            warnings: Vec::new(),
            loaded_skills: Vec::new(),
        })
    } else {
        let instruction_layers = exec_instruction_layers();
        Some(
            zdx_engine::core::context::build_effective_system_prompt_with_paths_and_instruction_layers(
                config,
                &options.root,
                &instruction_layers,
                false,
            )?,
        )
    };

    if let Some(effective) = &effective {
        for warning in &effective.warnings {
            warn!(message = %warning.message, "exec context warning");
        }
    }

    // Emit config path info (only if config exists on disk).
    let config_path = zdx_engine::config::paths::config_path();
    if config_path.exists() {
        info!(path = %config_path.display(), "exec config file");
    }

    // Emit context info (project context files, skills)
    if let Some(effective) = &effective {
        log_effective_context(effective);
    }

    // Load thread history if continuing an existing thread
    let messages = if let Some(ref existing_thread) = thread {
        let mut history = thread_persistence::load_thread_as_messages(&existing_thread.id)?;
        history.push(ChatMessage::user(prompt));
        history
    } else {
        vec![ChatMessage::user(prompt)]
    };

    // Log user message to thread (ensures meta is written for new threads)
    if let Some(ref mut s) = thread {
        s.append(&ThreadEvent::user_message(prompt))?;
    }
    let agent_opts = AgentOptions::from(options);

    // Create channels for broadcast
    let (agent_tx, agent_rx) = zdx_engine::core::agent::create_event_channel();
    let (render_tx, render_rx) = zdx_engine::core::agent::create_event_channel();

    // Spawn renderer task
    let renderer_handle =
        spawn_exec_renderer_task_with_filter(render_rx, options.event_filter.clone());

    // Spawn persist task if thread exists
    let thread_id = thread.as_ref().map(|t| t.id.clone());
    let persist_handle = if let Some(thread_handle) = thread.clone() {
        let (persist_tx, persist_rx) = zdx_engine::core::agent::create_event_channel();
        let broadcaster =
            zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![render_tx, persist_tx]);
        let persist = thread_persistence::spawn_thread_persist_task(thread_handle, persist_rx);
        Some((broadcaster, persist))
    } else {
        // No thread - just broadcast to renderer
        let broadcaster = zdx_engine::core::agent::spawn_broadcaster(agent_rx, vec![render_tx]);
        Some((broadcaster, tokio::spawn(async {}))) // Dummy persist task
    };

    // Run the agent turn
    let result = zdx_engine::core::agent::run_turn(
        messages,
        config,
        &agent_opts,
        effective.as_ref().and_then(|e| e.prompt.as_deref()),
        thread_id.as_deref(),
        agent_tx,
    )
    .await;

    // Wait for all tasks to complete (even on error, to flush error events)
    if let Some((broadcaster, persist)) = persist_handle {
        let _ = broadcaster.await;
        let _ = persist.await;
    }
    let _ = renderer_handle.await;

    // Propagate error after tasks complete
    let (final_text, _messages) = result?;

    emit_final_turn_finished(&final_text, &options.event_filter);

    // Log assistant response to thread
    if let Some(ref mut s) = thread {
        s.append(&ThreadEvent::assistant_message_with_phase(
            &final_text,
            Some("final_answer".to_string()),
        ))?;
    }

    Ok(final_text)
}

/// CLI renderer that writes agent events as compact JSONL to stdout.
pub struct ExecRenderer {
    stdout: Stdout,
    event_filter: Vec<String>,
}

impl Default for ExecRenderer {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ExecRenderer {
    /// Creates a new CLI renderer.
    pub fn new(event_filter: Vec<String>) -> Self {
        Self {
            stdout: stdout(),
            event_filter,
        }
    }

    /// Handles a single agent event by writing a compact JSON object per line.
    pub fn handle_event(&mut self, event: &AgentEvent) {
        let Some(event) = sanitize_exec_event(event) else {
            return;
        };

        if !self.event_filter.is_empty()
            && !self
                .event_filter
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(event_type_name(&event)))
        {
            return;
        }

        if let Ok(line) = serde_json::to_string(&event) {
            let _ = writeln!(self.stdout, "{line}");
            let _ = self.stdout.flush();
        }
    }

    pub fn finish() {}
}

/// Spawns a renderer task that consumes events from a channel.
///
/// The task owns the `ExecRenderer` and processes events until the channel closes.
/// Returns a `JoinHandle` that resolves when all events have been rendered.
pub fn spawn_exec_renderer_task_with_filter(
    mut rx: zdx_engine::core::agent::AgentEventRx,
    event_filter: Vec<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut renderer = ExecRenderer::new(event_filter);

        while let Some(event) = rx.recv().await {
            renderer.handle_event(&event);
        }

        ExecRenderer::finish();
    })
}

fn event_type_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::TurnStarted => "turn_started",
        AgentEvent::ReasoningDelta { .. } => "reasoning_delta",
        AgentEvent::ReasoningCompleted { .. } => "reasoning_completed",
        AgentEvent::AssistantDelta { .. } => "assistant_delta",
        AgentEvent::AssistantCompleted { .. } => "assistant_completed",
        AgentEvent::ToolRequested { .. } => "tool_requested",
        AgentEvent::ToolInputCompleted { .. } => "tool_input_completed",
        AgentEvent::ToolInputDelta { .. } => "tool_input_delta",
        AgentEvent::ToolStarted { .. } => "tool_started",
        AgentEvent::ToolOutputDelta { .. } => "tool_output_delta",
        AgentEvent::ToolCompleted { .. } => "tool_completed",
        AgentEvent::Error { .. } => "error",
        AgentEvent::Notice { .. } => "notice",
        AgentEvent::ProviderRetry { .. } => "provider_retry",
        AgentEvent::TurnFinished { .. } => "turn_finished",
        AgentEvent::UsageUpdate { .. } => "usage_update",
    }
}

fn sanitize_exec_event(event: &AgentEvent) -> Option<AgentEvent> {
    match event {
        AgentEvent::AssistantDelta { .. }
        | AgentEvent::ReasoningDelta { .. }
        | AgentEvent::ToolOutputDelta { .. }
        | AgentEvent::ToolInputDelta { .. }
        | AgentEvent::TurnFinished { .. } => None,
        AgentEvent::ReasoningCompleted { block } => {
            let sanitized = zdx_engine::providers::ReasoningBlock {
                text: block.text.clone(),
                replay: None,
            };
            if sanitized.text.is_some() {
                Some(AgentEvent::ReasoningCompleted { block: sanitized })
            } else {
                None
            }
        }
        _ => Some(event.clone()),
    }
}

fn emit_final_turn_finished(final_text: &str, event_filter: &[String]) {
    if !event_filter.is_empty()
        && !event_filter
            .iter()
            .any(|wanted| wanted.eq_ignore_ascii_case("turn_finished"))
    {
        return;
    }

    let event = AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: final_text.to_string(),
        messages: Vec::new(),
    };
    if let Ok(line) = serde_json::to_string(&event) {
        let mut out = stdout();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use zdx_engine::core::events::AgentEvent;
    use zdx_engine::providers::{ReasoningBlock, ReplayToken};

    use super::sanitize_exec_event;

    #[test]
    fn sanitize_exec_event_drops_empty_reasoning() {
        let event = AgentEvent::ReasoningCompleted {
            block: ReasoningBlock {
                text: None,
                replay: None,
            },
        };

        assert!(sanitize_exec_event(&event).is_none());
    }

    #[test]
    fn sanitize_exec_event_strips_reasoning_replay() {
        let event = AgentEvent::ReasoningCompleted {
            block: ReasoningBlock {
                text: Some("thinking".to_string()),
                replay: Some(ReplayToken::OpenAI {
                    id: "r1".to_string(),
                    encrypted_content: "secret".to_string(),
                }),
            },
        };

        let sanitized = sanitize_exec_event(&event).expect("event should remain");
        match sanitized {
            AgentEvent::ReasoningCompleted { block } => {
                assert_eq!(block.text.as_deref(), Some("thinking"));
                assert!(block.replay.is_none());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
