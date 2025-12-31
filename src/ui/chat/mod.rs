//! Full-screen alternate-screen TUI.
//!
//! This module provides a full-screen terminal UI using ratatui.
//! Uses the alternate screen buffer for a persistent, scrollable interface.
//!
//! Architecture (post-Slice 3):
//! - `TuiRuntime`: Owns terminal + state, runs event loop, executes effects
//! - `TuiState` (in state.rs): All app state, no terminal
//! - `update()` (in reducer.rs): The reducer - all state mutations happen here
//! - `view()` (in view.rs): Pure render, no mutations

pub mod commands;
pub mod effects;
pub mod events;
pub mod overlays;
pub mod reducer;
pub mod selection;
pub mod state;
pub mod terminal;
pub mod view;

use std::io::{IsTerminal, Stdout, Write, stderr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::core::interrupt;
use crate::core::session::{self, Session};
use crate::providers::anthropic::ChatMessage;
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::events::UiEvent;
use crate::ui::chat::state::{AgentState, HandoffState, TuiState};
use crate::ui::transcript::HistoryCell;

/// Runs the interactive chat loop.
pub async fn run_interactive_chat(
    config: &Config,
    session: Option<Session>,
    root: PathBuf,
) -> Result<()> {
    run_interactive_chat_with_history(config, session, Vec::new(), root).await
}

/// Runs the interactive chat loop with pre-loaded history.
pub async fn run_interactive_chat_with_history(
    config: &Config,
    session: Option<Session>,
    history: Vec<ChatMessage>,
    root: PathBuf,
) -> Result<()> {
    // Chat mode requires a terminal to render the TUI
    if !stderr().is_terminal() {
        anyhow::bail!(
            "Chat mode requires a terminal.\n\
             Use `zdx exec --prompt '...'` for non-interactive execution."
        );
    }

    let effective = crate::core::context::build_effective_system_prompt_with_paths(config, &root)?;

    // Print pre-TUI info to stderr (will be replaced by alternate screen)
    let mut err = stderr();
    writeln!(err, "ZDX Chat")?;
    writeln!(err, "Model: {}", config.model)?;
    if let Some(ref s) = session {
        writeln!(err, "Session: {}", s.id)?;
    }
    if !history.is_empty() {
        writeln!(err, "Loaded {} previous messages", history.len())?;
    }

    // Emit warnings from context loading (per SPEC §10)
    for warning in &effective.warnings {
        writeln!(err, "Warning: {}", warning.message)?;
    }

    // Small delay so user can see the info before TUI takes over
    err.flush()?;

    // Create and run the TUI
    let mut runtime = if history.is_empty() {
        TuiRuntime::new(config.clone(), root, effective.prompt, session)?
    } else {
        TuiRuntime::with_history(config.clone(), root, effective.prompt, session, history)?
    };

    // Add system message for session path
    if let Some(ref s) = runtime.state.conversation.session {
        let session_path_msg = format!("Session path: {}", s.path().display());
        runtime
            .state
            .transcript
            .cells
            .push(HistoryCell::system(session_path_msg));
    }

    // Add system message for loaded AGENTS.md files to transcript
    if !effective.loaded_agents_paths.is_empty() {
        let paths_list: Vec<String> = effective
            .loaded_agents_paths
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect();
        let message = format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n"));
        runtime
            .state
            .transcript
            .cells
            .push(HistoryCell::system(message));
    }

    runtime.run()?;

    // Print goodbye after TUI exits (terminal restored)
    writeln!(stderr(), "Goodbye!")?;

    Ok(())
}

/// Target frame rate for streaming updates (60fps = ~16ms per frame).
const FRAME_DURATION: std::time::Duration = std::time::Duration::from_millis(16);

/// Full-screen TUI runtime.
///
/// Owns the terminal and state. Runs the event loop and executes effects.
/// Terminal state is guaranteed to be restored on drop, panic, or Ctrl+C.
pub struct TuiRuntime {
    /// Terminal instance.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Application state (separate from terminal for borrow-checker friendly rendering).
    state: TuiState,
}

impl TuiRuntime {
    /// Creates a new TUI runtime.
    pub fn new(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
    ) -> Result<Self> {
        Self::with_history(config, root, system_prompt, session, Vec::new())
    }

    /// Creates a TUI runtime with pre-loaded message history.
    pub fn with_history(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
        history: Vec<ChatMessage>,
    ) -> Result<Self> {
        // Set up panic hook BEFORE entering alternate screen
        terminal::install_panic_hook();

        // Reset interrupt flag in case it was set from a previous run
        interrupt::reset();

        // Enter alternate screen and raw mode
        let terminal = terminal::setup_terminal().context("Failed to setup terminal")?;

        // Create state
        let state = TuiState::with_history(config, root, system_prompt, session, history);

        Ok(Self { terminal, state })
    }

    /// Runs the main event loop.
    pub fn run(&mut self) -> Result<()> {
        // Enable bracketed paste and mouse capture
        terminal::enable_input_features()?;

        let result = self.event_loop();

        // Disable mouse capture and bracketed paste
        let _ = terminal::disable_input_features();

        result
    }

    fn event_loop(&mut self) -> Result<()> {
        let mut dirty = true; // Start dirty to ensure initial render

        while !self.state.should_quit {
            // Check for Ctrl+C signal (only quit if agent is idle)
            // If agent is running, the interrupt is meant to cancel it, not quit the app.
            // The agent will send an Interrupted event which resets the flag.
            if interrupt::is_interrupted() && !self.state.agent_state.is_running() {
                self.state.should_quit = true;
                break;
            }

            // Collect events from various sources
            let mut events = self.collect_events()?;

            // Prepend Frame event with current terminal size
            // This ensures layout/delta updates happen before other events
            let size = self.terminal.size()?;
            events.insert(
                0,
                UiEvent::Frame {
                    width: size.width,
                    height: size.height,
                },
            );

            // Process each event through the reducer
            for event in events {
                // Tick marks dirty only if agent is running (spinner animation)
                // Other events always mark dirty
                let marks_dirty = match &event {
                    UiEvent::Tick => self.state.agent_state.is_running(),
                    _ => true,
                };
                let effects = reducer::update(&mut self.state, event);
                if marks_dirty || !effects.is_empty() {
                    dirty = true;
                }
                self.execute_effects(effects);
            }

            // Only render if something changed
            if dirty {
                // Render - state is a separate field, no borrow conflict
                self.terminal.draw(|frame| {
                    view::view(&self.state, frame);
                })?;

                dirty = false;
            }
        }

        Ok(())
    }

    /// Collects events from all sources (terminal, agent, async tasks).
    fn collect_events(&mut self) -> Result<Vec<UiEvent>> {
        let mut events = Vec::new();

        // Always emit a tick for animation/polling
        events.push(UiEvent::Tick);

        // Poll agent events (streaming deltas, tool events, completion, etc.)
        self.collect_agent_events(&mut events);

        // Poll for login exchange result
        self.collect_login_result(&mut events);

        // Poll for handoff generation result
        self.collect_handoff_result(&mut events);

        // Poll terminal events with short timeout for responsive streaming
        // Batch ALL available events to avoid one-event-per-frame lag on fast scroll
        if event::poll(FRAME_DURATION)? {
            events.push(UiEvent::Terminal(event::read()?));
            // Drain any remaining buffered events (non-blocking)
            while event::poll(std::time::Duration::ZERO)? {
                events.push(UiEvent::Terminal(event::read()?));
            }
        }

        Ok(events)
    }

    /// Collects agent events from the channel.
    fn collect_agent_events(&mut self, events: &mut Vec<UiEvent>) {
        while let AgentState::Waiting { rx, .. } | AgentState::Streaming { rx, .. } =
            &mut self.state.agent_state
        {
            let event = match rx.try_recv() {
                Ok(ev) => ev,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            };

            events.push(UiEvent::Agent((*event).clone()));
        }
    }

    /// Collects login exchange result if available.
    fn collect_login_result(&mut self, events: &mut Vec<UiEvent>) {
        let Some(rx) = &mut self.state.auth.login_rx else {
            return;
        };

        match rx.try_recv() {
            Ok(result) => {
                events.push(UiEvent::LoginResult(result));
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                events.push(UiEvent::LoginResult(
                    Err("Exchange task failed".to_string()),
                ));
            }
        }
    }

    /// Collects handoff generation result if available.
    fn collect_handoff_result(&mut self, events: &mut Vec<UiEvent>) {
        let HandoffState::Generating { rx, .. } = &mut self.state.input.handoff else {
            return;
        };

        match rx.try_recv() {
            Ok(result) => {
                events.push(UiEvent::HandoffResult(result));
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                events.push(UiEvent::HandoffResult(Err(
                    "Handoff generation task failed".to_string(),
                )));
            }
        }
    }

    /// Executes effects returned by the reducer.
    fn execute_effects(&mut self, effects: Vec<UiEffect>) {
        for effect in effects {
            self.execute_effect(effect);
        }
    }

    /// Executes a single effect.
    fn execute_effect(&mut self, effect: UiEffect) {
        match effect {
            UiEffect::Quit => {
                self.state.should_quit = true;
            }
            UiEffect::StartAgentTurn => {
                self.spawn_agent_turn();
            }
            UiEffect::InterruptAgent => {
                self.interrupt_agent();
            }
            UiEffect::SpawnTokenExchange { code, verifier } => {
                self.spawn_token_exchange(&code, &verifier);
            }
            UiEffect::OpenBrowser { url } => {
                let _ = open::that(&url);
            }
            UiEffect::OpenConfig => {
                let config_path = crate::config::paths::config_path();
                if config_path.exists() {
                    if let Err(e) = open::that(&config_path) {
                        self.state
                            .transcript
                            .cells
                            .push(HistoryCell::system(format!("Failed to open config: {}", e)));
                    }
                } else {
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system(format!(
                            "Config file not found: {}",
                            config_path.display()
                        )));
                }
            }
            UiEffect::StartHandoff { goal } => {
                // Check if we have an active session
                if let Some(ref session) = self.state.conversation.session {
                    self.spawn_handoff_generation(&session.id.clone(), &goal);
                } else {
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system("Handoff requires an active session."));
                }
            }
            UiEffect::HandoffSubmit { prompt } => {
                self.execute_handoff_submit(&prompt);
            }
            UiEffect::SaveSession { event } => {
                if let Some(ref mut s) = self.state.conversation.session
                    && let Err(e) = s.append(&event)
                {
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system(format!(
                            "Warning: Failed to save session: {}",
                            e
                        )));
                }
            }
            UiEffect::PersistModel { model } => {
                if let Err(e) = crate::config::Config::save_model(&model) {
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system(format!(
                            "Warning: Failed to save model preference: {}",
                            e
                        )));
                }
            }
            UiEffect::PersistThinking { level } => {
                if let Err(e) = crate::config::Config::save_thinking_level(level) {
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system(format!(
                            "Warning: Failed to save thinking level: {}",
                            e
                        )));
                }
            }
            UiEffect::CreateNewSession => {
                if self.create_session_and_show_context().is_err() {
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system("Conversation cleared."));
                }
            }
            UiEffect::OpenSessionPicker => {
                self.open_session_picker();
            }
            UiEffect::LoadSession { session_id } => {
                self.load_session(&session_id);
            }
        }
    }

    // ========================================================================
    // Effect Implementations (async spawning, etc.)
    // ========================================================================

    /// Opens the session picker overlay.
    ///
    /// Loads the session list (I/O) and opens the overlay if sessions exist.
    /// Shows an error message if no sessions are found or loading fails.
    fn open_session_picker(&mut self) {
        use crate::ui::chat::overlays::SessionPickerState;
        use crate::ui::chat::state::OverlayState;

        // Don't open if another overlay is active
        if !matches!(self.state.overlay, OverlayState::None) {
            return;
        }

        // Load sessions (I/O happens here in the effect handler, not reducer)
        match session::list_sessions() {
            Ok(sessions) if sessions.is_empty() => {
                self.state
                    .transcript
                    .cells
                    .push(HistoryCell::system("No sessions found."));
            }
            Ok(sessions) => {
                self.state.overlay = OverlayState::SessionPicker(SessionPickerState::new(sessions));
            }
            Err(e) => {
                self.state
                    .transcript
                    .cells
                    .push(HistoryCell::system(format!(
                        "Failed to load sessions: {}",
                        e
                    )));
            }
        }
    }

    /// Loads a session by ID and switches to it.
    ///
    /// This:
    /// 1. Loads events from the session file
    /// 2. Builds transcript cells from events
    /// 3. Builds API messages for conversation context
    /// 4. Resets all state facets with loaded data
    fn load_session(&mut self, session_id: &str) {
        // Load session events (I/O)
        let events = match session::load_session(session_id) {
            Ok(events) => events,
            Err(e) => {
                self.state
                    .transcript
                    .cells
                    .push(HistoryCell::system(format!(
                        "Failed to load session: {}",
                        e
                    )));
                return;
            }
        };

        // Build transcript cells from events
        let transcript_cells = build_transcript_from_events(&events);

        // Build API messages for conversation context
        let messages = session::events_to_messages(events);

        // Build input history from user messages in transcript
        let command_history: Vec<String> = transcript_cells
            .iter()
            .filter_map(|cell| {
                if let HistoryCell::User { content, .. } = cell {
                    Some(content.clone())
                } else {
                    None
                }
            })
            .collect();

        // Create or get the session handle for future appends
        let session_handle = match session::Session::with_id(session_id.to_string()) {
            Ok(s) => Some(s),
            Err(e) => {
                self.state
                    .transcript
                    .cells
                    .push(HistoryCell::system(format!(
                        "Warning: Failed to open session for writing: {}",
                        e
                    )));
                None
            }
        };

        // Reset state facets with loaded data
        self.state.transcript.cells = transcript_cells;
        self.state.conversation.messages = messages;
        self.state.conversation.session = session_handle;
        self.state.conversation.usage = crate::ui::chat::state::SessionUsage::new();
        self.state.input.history = command_history;
        self.state.transcript.scroll.reset();
        self.state.transcript.wrap_cache.clear();

        // Show confirmation message
        let short_id = if session_id.len() > 8 {
            format!("{}…", &session_id[..8])
        } else {
            session_id.to_string()
        };
        self.state
            .transcript
            .cells
            .push(HistoryCell::system(format!(
                "Switched to session {}",
                short_id
            )));
    }

    fn interrupt_agent(&mut self) {
        if self.state.agent_state.is_running() {
            interrupt::trigger_ctrl_c();
        }
    }

    /// Creates a new session and shows context info in transcript.
    ///
    /// Returns `Ok(())` if session was created successfully, `Err(())` if it failed.
    /// On failure, an error message is already added to the transcript.
    fn create_session_and_show_context(&mut self) -> Result<(), ()> {
        let new_session = match session::Session::new() {
            Ok(s) => s,
            Err(e) => {
                self.state
                    .transcript
                    .cells
                    .push(HistoryCell::system(format!(
                        "Warning: Failed to create new session: {}",
                        e
                    )));
                return Err(());
            }
        };

        let new_path = new_session.path().display().to_string();
        self.state.conversation.session = Some(new_session);

        // Show session path
        self.state
            .transcript
            .cells
            .push(HistoryCell::system(format!("Session path: {}", new_path)));

        // Show loaded AGENTS.md files
        let effective = match crate::core::context::build_effective_system_prompt_with_paths(
            &self.state.config,
            &self.state.agent_opts.root,
        ) {
            Ok(e) => e,
            Err(err) => {
                self.state
                    .transcript
                    .cells
                    .push(HistoryCell::system(format!(
                        "Warning: Failed to load context: {}",
                        err
                    )));
                return Ok(()); // Session created, just context loading failed
            }
        };

        if !effective.loaded_agents_paths.is_empty() {
            let paths_list: Vec<String> = effective
                .loaded_agents_paths
                .iter()
                .map(|p| format!("  - {}", p.display()))
                .collect();
            let message = format!("Loaded AGENTS.md from:\n{}", paths_list.join("\n"));
            self.state
                .transcript
                .cells
                .push(HistoryCell::system(message));
        }

        Ok(())
    }

    /// Executes a handoff submit: creates new session and sends prompt as first message.
    fn execute_handoff_submit(&mut self, prompt: &str) {
        use crate::core::session::SessionEvent;

        // 1. Clear state (like /new)
        self.state.transcript.cells.clear();
        self.state.conversation.messages.clear();
        self.state.input.history.clear();
        self.state.transcript.scroll.reset();
        self.state.conversation.usage = crate::ui::chat::state::SessionUsage::new();
        self.state.transcript.wrap_cache.clear();

        // 2. Create new session (continue even if it fails - user can still chat)
        let _ = self.create_session_and_show_context();

        // 3. Add user message to transcript and conversation
        self.state.input.history.push(prompt.to_string());
        self.state.transcript.cells.push(HistoryCell::user(prompt));
        self.state
            .conversation
            .messages
            .push(ChatMessage::user(prompt));

        // 4. Save user message to session
        if let Some(ref mut s) = self.state.conversation.session
            && let Err(e) = s.append(&SessionEvent::user_message(prompt))
        {
            self.state
                .transcript
                .cells
                .push(HistoryCell::system(format!(
                    "Warning: Failed to save session: {}",
                    e
                )));
        }

        // 5. Start agent turn
        self.spawn_agent_turn();
    }

    fn spawn_agent_turn(&mut self) {
        let (agent_tx, agent_rx) = crate::core::agent::create_event_channel();

        let messages = self.state.conversation.messages.clone();
        let config = self.state.config.clone();
        let agent_opts = self.state.agent_opts.clone();
        let system_prompt = self.state.system_prompt.clone();

        let (tui_tx, tui_rx) = crate::core::agent::create_event_channel();

        if let Some(sess) = self.state.conversation.session.clone() {
            let (persist_tx, persist_rx) = crate::core::agent::create_event_channel();
            let _fanout = crate::core::agent::spawn_fanout_task(agent_rx, vec![tui_tx, persist_tx]);
            let _persist = session::spawn_persist_task(sess, persist_rx);
        } else {
            let _fanout = crate::core::agent::spawn_fanout_task(agent_rx, vec![tui_tx]);
        }

        // Spawn the agent task - it will send TurnComplete when done
        tokio::spawn(async move {
            let _ = crate::core::agent::run_turn(
                messages,
                &config,
                &agent_opts,
                system_prompt.as_deref(),
                agent_tx,
            )
            .await;
        });

        self.state.agent_state = AgentState::Waiting { rx: tui_rx };
    }

    fn spawn_token_exchange(&mut self, code: &str, verifier: &str) {
        use crate::providers::oauth::anthropic;

        let code = code.to_string();
        let pkce_verifier = verifier.to_string();

        let (tx, rx) = mpsc::channel::<Result<(), String>>(1);
        self.state.auth.login_rx = Some(rx);

        tokio::spawn(async move {
            let pkce = anthropic::Pkce {
                verifier: pkce_verifier,
                challenge: String::new(),
            };
            let result = match anthropic::exchange_code(&code, &pkce).await {
                Ok(creds) => anthropic::save_credentials(&creds)
                    .map_err(|e| format!("Failed to save: {}", e)),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(result).await;
        });
    }

    /// Spawns an async task to generate a handoff prompt using a subagent.
    fn spawn_handoff_generation(&mut self, session_id: &str, goal: &str) {
        use tokio::process::Command;
        use tokio::sync::oneshot;

        const HANDOFF_TIMEOUT_SECS: u64 = 120; // 2 minute timeout for subagent

        let session_id = session_id.to_string();
        let goal_clone = goal.to_string();
        let root = self.state.agent_opts.root.clone();

        // Build the generation prompt
        let generation_prompt = format!(
            r#"Read session {session_id} using this command:
zdx sessions show {session_id}

Based on that session, generate a focused handoff prompt for the following goal:

<goal>
{goal_clone}
</goal>

Include:
- Relevant context and decisions made
- Key files or code discussed
- The specific goal/direction

Output ONLY the handoff prompt text, nothing else. The prompt should be
written as if the user is starting a fresh conversation with a new agent."#
        );

        let (tx, rx) = mpsc::channel::<Result<String, String>>(1);
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

        // Transition to Generating state with all necessary data
        self.state.input.handoff = HandoffState::Generating {
            goal: goal.to_string(),
            rx,
            cancel_tx,
        };

        // Show status in transcript
        self.state
            .transcript
            .cells
            .push(HistoryCell::system(format!(
                "Generating handoff for goal: \"{}\"...",
                goal
            )));

        tokio::spawn(async move {
            // Get the current executable path
            let exe = match std::env::current_exe() {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx
                        .send(Err(format!("Failed to get executable: {}", e)))
                        .await;
                    return;
                }
            };

            // Spawn the subagent process (async)
            let child = match Command::new(exe)
                .args(["--no-save", "exec", "-p", &generation_prompt])
                .current_dir(&root)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true) // Kill child if task is dropped/cancelled
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx
                        .send(Err(format!("Failed to spawn subagent: {}", e)))
                        .await;
                    return;
                }
            };

            // Wait for output with timeout and cancellation support
            let result = tokio::select! {
                // Cancellation signal (user pressed Esc)
                _ = cancel_rx => {
                    // kill_on_drop will handle cleanup when child is dropped
                    Err("Handoff cancelled".to_string())
                }
                // Timeout
                output_result = async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(HANDOFF_TIMEOUT_SECS),
                        child.wait_with_output()
                    ).await
                } => {
                    match output_result {
                        Ok(Ok(output)) => {
                            if output.status.success() {
                                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                                if stdout.is_empty() {
                                    Err("Handoff generation returned empty output".to_string())
                                } else {
                                    Ok(stdout)
                                }
                            } else {
                                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                                Err(format!("Handoff generation failed: {}", stderr.trim()))
                            }
                        }
                        Ok(Err(e)) => Err(format!("Failed to get subagent output: {}", e)),
                        Err(_) => {
                            // Timeout elapsed - child will be killed on drop
                            Err(format!("Handoff generation timed out after {} seconds", HANDOFF_TIMEOUT_SECS))
                        }
                    }
                }
            };

            let _ = tx.send(result).await;
        });
    }
}

/// Builds transcript cells from session events.
///
/// Maps session events to display cells:
/// - `Message` → `User` or `Assistant` cells
/// - `ToolUse` + `ToolResult` → `Tool` cells (paired by ID)
/// - `Thinking` → `Thinking` cells
/// - Skips `Meta` and `Interrupted` events
fn build_transcript_from_events(events: &[session::SessionEvent]) -> Vec<HistoryCell> {
    use std::collections::HashMap;

    use crate::core::events::ToolOutput;
    use crate::core::session::SessionEvent;

    let mut cells = Vec::new();
    // Track tool cells by ID for pairing with results
    let mut tool_cells: HashMap<String, usize> = HashMap::new();

    for event in events {
        match event {
            SessionEvent::Meta { .. } => {
                // Skip meta events
            }
            SessionEvent::Message { role, text, .. } => {
                let cell = match role.as_str() {
                    "user" => HistoryCell::user(text),
                    "assistant" => HistoryCell::assistant(text),
                    _ => continue,
                };
                cells.push(cell);
            }
            SessionEvent::Thinking {
                content, signature, ..
            } => {
                // Create a finalized thinking cell
                let mut cell = HistoryCell::thinking_streaming(content);
                if let Some(sig) = signature {
                    cell.finalize_thinking(sig.clone());
                }
                cells.push(cell);
            }
            SessionEvent::ToolUse {
                id, name, input, ..
            } => {
                // Create a running tool cell (will be updated by result)
                let cell = HistoryCell::tool_running(id, name, input.clone());
                let idx = cells.len();
                tool_cells.insert(id.clone(), idx);
                cells.push(cell);
            }
            SessionEvent::ToolResult {
                tool_use_id,
                output,
                ..
            } => {
                // Find and update the corresponding tool cell
                if let Some(&idx) = tool_cells.get(tool_use_id)
                    && let Some(cell) = cells.get_mut(idx)
                {
                    // Deserialize the stored JSON back to ToolOutput
                    // (it was serialized via serde_json::to_value in SessionEvent::from_agent)
                    let tool_output: ToolOutput = serde_json::from_value(output.clone())
                        .unwrap_or_else(|_| {
                            ToolOutput::failure("parse_error", "Failed to parse tool result")
                        });
                    cell.set_tool_result(tool_output);
                }
                // If no matching tool cell found, skip (incomplete pair)
            }
            SessionEvent::Interrupted { .. } => {
                // Skip interrupted events when loading
            }
        }
    }

    cells
}

impl Drop for TuiRuntime {
    fn drop(&mut self) {
        let _ = terminal::restore_terminal();
    }
}

#[cfg(test)]
mod tests {
    // Terminal tests are difficult to run in CI since they require a real TTY.
    // Integration tests for TUI should spawn the CLI and verify stdout/stderr behavior.
    //
    // Key guarantees to test manually or via integration tests:
    // - Terminal is restored on normal exit
    // - Terminal is restored on panic
    // - Terminal is restored on Ctrl+C
    // - Resize events don't break the UI
    //
    // Unit tests for slash commands and palette state have been moved to
    // src/ui/chat/commands.rs and src/ui/chat/state/ respectively.
    //
    // Unit tests for the reducer are in src/ui/chat/reducer.rs.

    use serde_json::json;

    use super::*;
    use crate::core::session::SessionEvent;
    use crate::ui::transcript::ToolState;

    #[test]
    fn test_build_transcript_from_events_empty() {
        let events: Vec<SessionEvent> = vec![];
        let cells = build_transcript_from_events(&events);
        assert!(cells.is_empty());
    }

    #[test]
    fn test_build_transcript_from_events_messages() {
        let events = vec![
            SessionEvent::Meta {
                schema_version: 1,
                ts: "2024-01-01T00:00:00Z".to_string(),
            },
            SessionEvent::Message {
                role: "user".to_string(),
                text: "Hello".to_string(),
                ts: "2024-01-01T00:00:01Z".to_string(),
            },
            SessionEvent::Message {
                role: "assistant".to_string(),
                text: "Hi there!".to_string(),
                ts: "2024-01-01T00:00:02Z".to_string(),
            },
        ];

        let cells = build_transcript_from_events(&events);
        assert_eq!(cells.len(), 2);

        // Verify user cell
        match &cells[0] {
            HistoryCell::User { content, .. } => {
                assert_eq!(content, "Hello");
            }
            _ => panic!("Expected User cell"),
        }

        // Verify assistant cell
        match &cells[1] {
            HistoryCell::Assistant { content, .. } => {
                assert_eq!(content, "Hi there!");
            }
            _ => panic!("Expected Assistant cell"),
        }
    }

    #[test]
    fn test_build_transcript_from_events_tool_use() {
        let events = vec![
            SessionEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "read".to_string(),
                input: json!({"path": "test.txt"}),
                ts: "2024-01-01T00:00:01Z".to_string(),
            },
            SessionEvent::ToolResult {
                tool_use_id: "tool-1".to_string(),
                // output is a serialized ToolOutput (from SessionEvent::from_agent)
                output: json!({"ok": true, "data": {"content": "file data"}}),
                ok: true,
                ts: "2024-01-01T00:00:02Z".to_string(),
            },
        ];

        let cells = build_transcript_from_events(&events);
        assert_eq!(cells.len(), 1);

        // Verify tool cell with result
        match &cells[0] {
            HistoryCell::Tool {
                name,
                state,
                result,
                ..
            } => {
                assert_eq!(name, "read");
                assert_eq!(*state, ToolState::Done);
                assert!(result.is_some());
            }
            _ => panic!("Expected Tool cell"),
        }
    }

    #[test]
    fn test_build_transcript_from_events_thinking() {
        let events = vec![SessionEvent::Thinking {
            content: "Let me analyze this...".to_string(),
            signature: Some("sig123".to_string()),
            ts: "2024-01-01T00:00:01Z".to_string(),
        }];

        let cells = build_transcript_from_events(&events);
        assert_eq!(cells.len(), 1);

        // Verify thinking cell
        match &cells[0] {
            HistoryCell::Thinking {
                content,
                signature,
                is_streaming,
                ..
            } => {
                assert_eq!(content, "Let me analyze this...");
                assert_eq!(signature.as_deref(), Some("sig123"));
                assert!(!*is_streaming);
            }
            _ => panic!("Expected Thinking cell"),
        }
    }

    #[test]
    fn test_build_transcript_from_events_mixed() {
        let events = vec![
            SessionEvent::Meta {
                schema_version: 1,
                ts: "2024-01-01T00:00:00Z".to_string(),
            },
            SessionEvent::Message {
                role: "user".to_string(),
                text: "Read the file".to_string(),
                ts: "2024-01-01T00:00:01Z".to_string(),
            },
            SessionEvent::Thinking {
                content: "Analyzing...".to_string(),
                signature: Some("sig".to_string()),
                ts: "2024-01-01T00:00:02Z".to_string(),
            },
            SessionEvent::ToolUse {
                id: "t1".to_string(),
                name: "read".to_string(),
                input: json!({"path": "file.txt"}),
                ts: "2024-01-01T00:00:03Z".to_string(),
            },
            SessionEvent::ToolResult {
                tool_use_id: "t1".to_string(),
                // output is a serialized ToolOutput (from SessionEvent::from_agent)
                output: json!({"ok": true, "data": {"content": "data"}}),
                ok: true,
                ts: "2024-01-01T00:00:04Z".to_string(),
            },
            SessionEvent::Message {
                role: "assistant".to_string(),
                text: "Done!".to_string(),
                ts: "2024-01-01T00:00:05Z".to_string(),
            },
            SessionEvent::Interrupted {
                role: "system".to_string(),
                text: "Interrupted".to_string(),
                ts: "2024-01-01T00:00:06Z".to_string(),
            },
        ];

        let cells = build_transcript_from_events(&events);
        // Meta and Interrupted are skipped: user + thinking + tool + assistant = 4
        assert_eq!(cells.len(), 4);

        assert!(matches!(&cells[0], HistoryCell::User { .. }));
        assert!(matches!(&cells[1], HistoryCell::Thinking { .. }));
        assert!(matches!(&cells[2], HistoryCell::Tool { .. }));
        assert!(matches!(&cells[3], HistoryCell::Assistant { .. }));
    }
}
