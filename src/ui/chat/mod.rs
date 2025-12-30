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
use crate::ui::chat::state::{AgentState, TuiState};
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
            let events = self.collect_events()?;

            // Update transcript layout before processing events
            let size = self.terminal.size()?;
            let viewport_height =
                view::calculate_transcript_height_with_state(&self.state, size.height);
            self.state
                .transcript
                .update_layout((size.width, size.height), viewport_height);

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
                // Apply any pending deltas before render (coalescing)
                reducer::apply_pending_delta(&mut self.state);

                // Apply accumulated scroll delta from mouse events (coalescing)
                reducer::apply_scroll_delta(&mut self.state);

                // Update cached line count for scroll calculations
                let line_count = view::calculate_line_count(&self.state, size.width as usize);
                self.state.transcript.scroll.update_line_count(line_count);

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
            UiEffect::CreateNewSession => match session::Session::new() {
                Ok(new_session) => {
                    let new_path = new_session.path().display().to_string();
                    self.state.conversation.session = Some(new_session);

                    // Show session path
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system(format!("Session path: {}", new_path)));

                    // Show loaded AGENTS.md files (same as on startup)
                    let effective =
                        match crate::core::context::build_effective_system_prompt_with_paths(
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
                                return;
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
                }
                Err(e) => {
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system(format!(
                            "Warning: Failed to create new session: {}",
                            e
                        )));
                    self.state
                        .transcript
                        .cells
                        .push(HistoryCell::system("Conversation cleared."));
                }
            },
            UiEffect::ExecuteCommand { name } => {
                let effects = reducer::execute_command(&mut self.state, name);
                self.execute_effects(effects);
            }
        }
    }

    // ========================================================================
    // Effect Implementations (async spawning, etc.)
    // ========================================================================

    fn interrupt_agent(&mut self) {
        if self.state.agent_state.is_running() {
            interrupt::trigger_ctrl_c();
        }
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
}
