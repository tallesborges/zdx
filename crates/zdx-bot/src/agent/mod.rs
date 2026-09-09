use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use tokio_util::sync::CancellationToken;
use zdx_engine::config::{Config, TextVerbosity};
use zdx_engine::core::agent::{self, AgentEventRx, AgentOptions, ToolConfig, ToolSelection};
use zdx_engine::core::context::{
    PromptContextInclusion, RuntimeContext, build_prompt_with_context_and_layers,
    resolve_context_to_attach,
};
use zdx_engine::core::events::AgentEvent;
use zdx_engine::core::thread_persistence::{self, Thread, ThreadEvent};
use zdx_engine::providers::{ChatContentBlock, ChatMessage, MessageContent};

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

/// Appends the incoming user message to the thread and the in-memory messages,
/// attaching the advisory runtime-context block per the last-attached dedup
/// rule (attach only when the candidate differs from the last attached one).
///
/// # Errors
/// Returns an error if the operation fails.
pub(crate) fn record_user_message(
    thread: &mut Thread,
    messages: &mut Vec<ChatMessage>,
    incoming: &IncomingMessage,
    runtime_context: Option<&RuntimeContext>,
) -> Result<()> {
    let text = build_user_text(incoming);
    let last_key = thread_persistence::last_attached_context_key_from_messages(messages);
    let attach = resolve_context_to_attach(runtime_context, last_key.as_deref());
    let (block, key) = match attach {
        Some(attach) => (Some(attach.block), Some(attach.key)),
        None => (None, None),
    };
    thread
        .append(&ThreadEvent::user_message_with_context(
            text.clone(),
            block.clone(),
            key.clone(),
        ))
        .context("append user message")?;

    if incoming.images.is_empty() {
        messages.push(ChatMessage::user(text).with_runtime_context(block, key));
        return Ok(());
    }

    let mut blocks = Vec::with_capacity(1 + incoming.images.len());
    blocks.push(ChatContentBlock::text(text));
    for image in &incoming.images {
        blocks.push(ChatContentBlock::Image {
            mime_type: image.mime_type.clone(),
            data: image.data.clone(),
        });
    }

    messages.push(ChatMessage {
        role: "user".to_string(),
        phase: None,
        context: block,
        context_key: key,
        content: MessageContent::Blocks(blocks),
    });
    Ok(())
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
    /// Explicit tool allowlist for persistent-profile turns (orchestrator).
    pub(crate) tools_override: Option<Vec<String>>,
    /// Subagents `invoke_subagent` may reach on this turn; `None` is unrestricted.
    pub(crate) allowed_subagents: Option<Vec<String>>,
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
    persistent_profile: Option<&str>,
) -> Result<PreparedBotTurn> {
    if let Some(profile) = persistent_profile {
        return prepare_persistent_profile_turn(config, root, bot_instruction_layer, profile);
    }

    let bot_config = config.clone();
    let instruction_layers = collect_bot_instruction_layers(bot_instruction_layer);
    let effective = build_prompt_with_context_and_layers(
        &bot_config,
        root,
        &bot_config.model,
        &instruction_layers,
        true,
        bot_prompt_context(),
    )
    .context("build bot prompt")?;

    Ok(PreparedBotTurn {
        config: bot_config,
        root: root.to_path_buf(),
        system_prompt: effective.prompt,
        runtime_context: effective.runtime_context,
        tools_override: None,
        allowed_subagents: None,
    })
}

/// Builds the system prompt and tool allowlist for a persistent top-level
/// profile thread. Only the reserved built-in `orchestrator` profile exists;
/// unknown profile names fail the turn instead of silently loading the default
/// coding toolset.
fn prepare_persistent_profile_turn(
    config: &Config,
    root: &Path,
    bot_instruction_layer: Option<&str>,
    profile: &str,
) -> Result<PreparedBotTurn> {
    ensure!(
        profile == zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME,
        "Unknown persistent profile '{profile}' on this thread"
    );

    let definition = zdx_engine::subagents::load_builtin_orchestrator()
        .context("load built-in orchestrator profile")?;
    // The Telegram workspaces catalog is observational data: it moves into the
    // runtime-context snapshot (attached to the first user message) instead of
    // the system prompt, so it never churns the stable prefix.
    let workspaces = telegram_workspaces_block(config);
    let (mut prompt, runtime_context) =
        zdx_engine::subagents::render_prompt_with_discovered_skills_and_context(
            config,
            root,
            &definition,
            &config.model,
            bot_prompt_context(),
            workspaces.as_deref(),
        )
        .context("render orchestrator prompt")?;
    let overlay_path = zdx_engine::config::paths::zdx_home().join("orchestrator.md");
    if let Some(overlay) = load_orchestrator_overlay(&overlay_path) {
        prompt = format!("{prompt}\n\n# Personal Orchestrator Rules\n\n{overlay}");
    }
    if let Some(layer) = bot_instruction_layer {
        prompt = format!("{prompt}\n\n{layer}");
    }

    let tools = definition.tools.clone().unwrap_or_default();
    ensure!(
        !tools.is_empty(),
        "Orchestrator profile declares no tools; refusing to fall back to the default toolset"
    );

    Ok(PreparedBotTurn {
        config: config.clone(),
        root: root.to_path_buf(),
        system_prompt: Some(prompt),
        runtime_context,
        tools_override: Some(tools),
        allowed_subagents: definition.allowed_subagents.clone(),
    })
}

/// Telegram project groups bound to the bot, so the orchestrator can pick
/// worker roots deliberately and tell the user where a worker's mirror topic
/// will appear. Each workspace also lists the project skills its workers
/// discover there, since the orchestrator's own skill catalog only covers its
/// home root. `None` when no profiles are configured.
fn telegram_workspaces_block(config: &Config) -> Option<String> {
    if config.telegram.profiles.is_empty() {
        return None;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let skills = workspace_skills(config);

    let mut block = String::from(
        "# Telegram Workspaces\n\nProject groups bound to this bot (profile — root). A worker created inside one of these roots gets its mirror topic in that group (deepest matching root wins); workers in other roots get a topic in the chat you are in. Tell the user where to follow each worker.\n\nWorkers in a root automatically discover that project's skills (listed under it); you do not have them yourself. Use them to know what a workspace can do and to point a worker at the right one by name when the task matches.\n\n",
    );
    for (name, profile) in &config.telegram.profiles {
        let root = shorten_home(&profile.cwd_path().display().to_string(), &home);
        let flag = if profile.orchestrator {
            " (orchestrator home)"
        } else {
            ""
        };
        let _ = writeln!(
            block,
            "- {name} (chat {}) — `{root}`{flag}",
            profile.chat_id
        );
        for (skill_name, description) in skills.get(name).into_iter().flatten() {
            let _ = writeln!(block, "  - skill `{skill_name}`: {description}");
        }
    }
    Some(block)
}

/// Longest skill description shown per workspace skill line.
const WORKSPACE_SKILL_DESCRIPTION_CHARS: usize = 140;

/// Project-level skills each Telegram workspace's workers will discover, keyed
/// by profile name. Only project sources are scanned (user/global skills are
/// already in the orchestrator's own catalog). A skill reachable from several
/// nested workspaces is attributed once, to the deepest root that contains it.
fn workspace_skills(config: &Config) -> HashMap<String, Vec<(String, String)>> {
    use zdx_engine::config::SkillSourceToggles;
    use zdx_engine::skills::{LoadSkillsOptions, SkillSource, load_skills};

    // skill file → (owning profile, depth of that profile's root)
    let mut owner: HashMap<PathBuf, (String, usize, String, String)> = HashMap::new();
    for (name, profile) in &config.telegram.profiles {
        let root = profile.cwd_path();
        let root = root.canonicalize().unwrap_or(root);
        let depth = root.components().count();
        let options = LoadSkillsOptions {
            sources: SkillSourceToggles {
                zdx_project: true,
                claude_project: true,
                agents_project: true,
                ..SkillSourceToggles::default()
            },
            ..LoadSkillsOptions::new(root.clone())
        };
        // Bundled skills ignore the toggles; keep project sources only.
        let project_skills = load_skills(&options).skills.into_iter().filter(|skill| {
            matches!(
                skill.source,
                SkillSource::ZdxProject | SkillSource::ClaudeProject | SkillSource::AgentsProject
            )
        });
        for skill in project_skills {
            let contains = skill.base_dir.starts_with(&root);
            // Prefer the deepest root that actually contains the skill; a
            // skill inherited from an ancestor without a profile goes to
            // whichever workspace saw it first.
            let rank = if contains { depth } else { 0 };
            let replace = owner
                .get(&skill.file_path)
                .is_none_or(|(_, existing, _, _)| rank > *existing);
            if replace {
                owner.insert(
                    skill.file_path.clone(),
                    (
                        name.clone(),
                        rank,
                        skill.name.clone(),
                        truncate_description(&skill.description, WORKSPACE_SKILL_DESCRIPTION_CHARS),
                    ),
                );
            }
        }
    }

    let mut by_profile: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (profile, _, skill_name, description) in owner.into_values() {
        by_profile
            .entry(profile)
            .or_default()
            .push((skill_name, description));
    }
    for skills in by_profile.values_mut() {
        skills.sort();
    }
    by_profile
}

fn truncate_description(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= max_chars {
        return first_line.to_string();
    }
    let head: String = first_line.chars().take(max_chars).collect();
    format!("{}…", head.trim_end())
}

/// Loads the user's personal orchestrator overlay (`$ZDX_HOME/orchestrator.md`):
/// free-form manager rules appended only to orchestrator turns, editable
/// without rebuilding. A missing file is a no-op; other read failures are
/// logged and skipped so a broken overlay never blocks the home base.
fn load_orchestrator_overlay(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "Failed to read orchestrator overlay");
            None
        }
    }
}

fn shorten_home(path: &str, home: &str) -> String {
    if !home.is_empty() && path.starts_with(home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
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
) -> AgentTurnHandle {
    let PreparedBotTurn {
        config: bot_config,
        root,
        system_prompt,
        runtime_context: _,
        tools_override,
        allowed_subagents,
    } = prepared;

    // Persistent-profile turns pin the exact tool selection from the profile
    // definition; the shared registry (with bound orchestrator tools) is kept.
    let tool_config = match tools_override {
        Some(tools) => ToolConfig {
            registry: tool_config.registry.clone(),
            selection: ToolSelection::Explicit(tools),
            allowed_subagents,
        },
        None => tool_config.clone(),
    };

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

/// One-line "running tool" status shared by turn status messages and worker
/// mirror topics.
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

pub(crate) fn build_user_text(incoming: &IncomingMessage) -> String {
    let mut parts = Vec::new();
    if let Some(text) = incoming.text.as_ref()
        && !text.trim().is_empty()
    {
        parts.push(text.clone());
    }

    for audio in &incoming.audios {
        if let Some(transcript) = &audio.transcript {
            parts.push(zdx_engine::providers::wrap_voice_transcript(transcript));
            parts.push(format!(
                "(Audio file saved at {} — use the `ask_media` tool on it if you need more than the transcript.)",
                audio.local_path.display()
            ));
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
            "Document attachment '{}' saved at {} — use the `ask_media` tool on it to read its contents.",
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use zdx_engine::config::{Config, SkillSourceToggles};
    use zdx_engine::core::events::{AgentEvent, ToolOutput};

    use super::{
        STATUS_THINKING, STATUS_WAITING, STATUS_WRITING, event_to_status,
        load_orchestrator_overlay, prepare_bot_turn, telegram_workspaces_block, workspace_skills,
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

        let prepared = prepare_bot_turn(&config, &dir, None, None).unwrap();
        let prompt = prepared.system_prompt.unwrap_or_default();

        assert!(prompt.contains("Bot project note"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orchestrator_turn_renders_project_context_and_pins_tools() {
        // The orchestrator renders its profile prompt with an isolated home so
        // the test never runs against the developer's real ~/.zdx or rebuilds
        // their live thread index.
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = make_temp_dir();
        std::fs::write(dir.join("AGENTS.md"), "Orchestrator project note").unwrap();

        let mut config = Config::default();
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

        let prepared =
            prepare_bot_turn(&config, &dir, Some("TELEGRAM LAYER"), Some("orchestrator")).unwrap();
        let prompt = prepared.system_prompt.unwrap_or_default();

        // SPEC §18: profile prompt composes project context + Telegram layer.
        assert!(prompt.contains("ZDX Orchestrator"));
        assert!(prompt.contains("Orchestrator project note"));
        assert!(prompt.contains("TELEGRAM LAYER"));

        // Exact profile tool surface, no default-toolset fallback.
        let tools = prepared.tools_override.expect("orchestrator pins tools");
        assert!(tools.contains(&"create_thread".to_string()));
        assert!(!tools.contains(&"write".to_string()));
        assert!(!tools.contains(&"bash".to_string()));
        assert!(!tools.contains(&"invoke_subagent".to_string()));

        // SPEC §18: the home base delegates to visible workers, not subagents.
        assert_eq!(prepared.allowed_subagents, None);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_persistent_profile_fails_the_turn() {
        let dir = make_temp_dir();
        let err = prepare_bot_turn(&Config::default(), &dir, None, Some("mystery"))
            .err()
            .expect("unknown profile must fail");
        assert!(err.to_string().contains("Unknown persistent profile"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn telegram_workspaces_block_lists_profiles_with_routing_rule() {
        use std::collections::BTreeMap;

        use zdx_engine::config::{TelegramConfig, TelegramProfileConfig};

        // Rendering skills materializes bundled skills into $ZDX_HOME.
        let _home = zdx_engine::test_support::temp_zdx_home();

        assert!(telegram_workspaces_block(&Config::default()).is_none());

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([
                    (
                        "dub".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1001,
                            cwd: "/tmp/work/dub".to_string(),
                            orchestrator: false,
                        },
                    ),
                    (
                        "zdx".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1002,
                            cwd: "/tmp/personal/zdx".to_string(),
                            orchestrator: true,
                        },
                    ),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let block = telegram_workspaces_block(&config).unwrap();
        assert!(block.starts_with("# Telegram Workspaces"));
        assert!(block.contains("mirror topic in that group"));
        assert!(block.contains("- dub (chat -1001) — `/tmp/work/dub`"));
        assert!(block.contains("- zdx (chat -1002) — `/tmp/personal/zdx` (orchestrator home)"));
    }

    /// Workers discover a workspace's project skills, the orchestrator does
    /// not, so the workspace list names them. A skill living in an umbrella
    /// root is attributed to that umbrella, not repeated under nested roots.
    #[test]
    fn telegram_workspaces_block_lists_project_skills_once_under_their_root() {
        use std::collections::BTreeMap;

        use zdx_engine::config::{TelegramConfig, TelegramProfileConfig};

        // Rendering skills materializes bundled skills into $ZDX_HOME.
        let _home = zdx_engine::test_support::temp_zdx_home();

        let umbrella = make_temp_dir();
        let project = umbrella.join("dub");
        std::fs::create_dir_all(umbrella.join(".zdx/skills/parity-flow")).unwrap();
        std::fs::write(
            umbrella.join(".zdx/skills/parity-flow/SKILL.md"),
            "---\nname: parity-flow\ndescription: ExampleCo release flow.\n---\nbody\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.join(".zdx/skills/dub-attest")).unwrap();
        std::fs::write(
            project.join(".zdx/skills/dub-attest/SKILL.md"),
            "---\nname: dub-attest\ndescription: Run device attestation end to end.\n---\nbody\n",
        )
        .unwrap();

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([
                    (
                        "dub".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1001,
                            cwd: project.display().to_string(),
                            orchestrator: false,
                        },
                    ),
                    (
                        "parity".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1002,
                            cwd: umbrella.display().to_string(),
                            orchestrator: false,
                        },
                    ),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let skills = workspace_skills(&config);
        assert_eq!(
            skills.get("dub").unwrap(),
            &vec![(
                "dub-attest".to_string(),
                "Run device attestation end to end.".to_string()
            )]
        );
        assert_eq!(
            skills.get("parity").unwrap(),
            &vec![(
                "parity-flow".to_string(),
                "ExampleCo release flow.".to_string()
            )]
        );

        let block = telegram_workspaces_block(&config).unwrap();
        assert!(block.contains("  - skill `dub-attest`: Run device attestation end to end."));
        assert_eq!(block.matches("skill `parity-flow`").count(), 1);
    }

    #[test]
    fn orchestrator_overlay_loads_only_when_present_and_non_empty() {
        let dir = make_temp_dir();
        let path = dir.join("orchestrator.md");

        // Missing file is a silent no-op.
        assert_eq!(load_orchestrator_overlay(&path), None);

        // Whitespace-only content is treated as absent.
        std::fs::write(&path, "  \n\t\n").unwrap();
        assert_eq!(load_orchestrator_overlay(&path), None);

        std::fs::write(&path, "\n- Reply in English\n- dub first\n").unwrap();
        assert_eq!(
            load_orchestrator_overlay(&path).as_deref(),
            Some("- Reply in English\n- dub first")
        );

        std::fs::remove_dir_all(dir).unwrap();
    }
}
