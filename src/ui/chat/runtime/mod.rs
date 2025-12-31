//! TUI runtime - owns terminal, runs event loop, executes effects.
//!
//! This is the "Elm runtime" boundary: all side effects happen here.
//! The reducer stays pure and produces effects; this module executes them.
//!
//! Structure:
//! - `mod.rs`: Core runtime (TuiRuntime, event loop, effect dispatch)
//! - `handlers.rs`: Effect handler implementations (I/O, spawning, etc.)
//! - `handoff.rs`: Handoff generation handlers (subagent spawning)

mod handlers;
mod handoff;

use std::io::Stdout;
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::core::interrupt;
use crate::core::session::Session;
use crate::providers::anthropic::ChatMessage;
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::events::UiEvent;
use crate::ui::chat::state::{AgentState, HandoffState, TuiState};
use crate::ui::chat::{reducer, terminal, view};
use crate::ui::transcript::HistoryCell;

/// Target frame rate for streaming updates (60fps = ~16ms per frame).
pub const FRAME_DURATION: std::time::Duration = std::time::Duration::from_millis(16);

/// Poll duration when idle (no agent running, no pending timers).
/// Longer timeout reduces CPU usage when nothing is happening.
pub const IDLE_POLL_DURATION: std::time::Duration = std::time::Duration::from_millis(100);

/// Full-screen TUI runtime.
///
/// Owns the terminal and state. Runs the event loop and executes effects.
/// Terminal state is guaranteed to be restored on drop, panic, or Ctrl+C.
pub struct TuiRuntime {
    /// Terminal instance.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Application state (separate from terminal for borrow-checker friendly rendering).
    pub state: TuiState,
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
                // Determine if this event should mark the view as dirty.
                // We're conservative here to avoid unnecessary renders:
                // - Tick marks dirty if agent is running (spinner) or selection clear pending
                // - Frame never marks dirty on its own (it's just housekeeping)
                // - Other events (input, agent events) always mark dirty
                let marks_dirty = match &event {
                    UiEvent::Tick => {
                        self.state.agent_state.is_running()
                            || self.state.transcript.selection.has_pending_clear()
                    }
                    UiEvent::Frame { .. } => false,
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

    // ========================================================================
    // Event Collection
    // ========================================================================

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

        // Determine poll timeout based on activity level.
        // Use fast polling (60fps) when:
        // - Agent is running (streaming content)
        // - Selection clear is pending (visual feedback timer)
        // - Login or handoff async operations are in progress
        // Otherwise use slow polling to save CPU.
        let needs_fast_poll = self.state.agent_state.is_running()
            || self.state.transcript.selection.has_pending_clear()
            || self.state.auth.login_rx.is_some()
            || self.state.input.handoff.is_generating();

        let poll_duration = if needs_fast_poll {
            FRAME_DURATION
        } else {
            IDLE_POLL_DURATION
        };

        // Poll terminal events with appropriate timeout
        // Batch ALL available events to avoid one-event-per-frame lag on fast scroll
        if event::poll(poll_duration)? {
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

    // ========================================================================
    // Effect Dispatch
    // ========================================================================

    /// Executes effects returned by the reducer.
    fn execute_effects(&mut self, effects: Vec<UiEffect>) {
        for effect in effects {
            self.execute_effect(effect);
        }
    }

    /// Executes a single effect by dispatching to the appropriate handler.
    fn execute_effect(&mut self, effect: UiEffect) {
        match effect {
            // Simple effects (inline)
            UiEffect::Quit => {
                self.state.should_quit = true;
            }
            UiEffect::OpenBrowser { url } => {
                let _ = open::that(&url);
            }

            // Agent effects
            UiEffect::StartAgentTurn => {
                handlers::spawn_agent_turn(&mut self.state);
            }
            UiEffect::InterruptAgent => {
                handlers::interrupt_agent(&mut self.state);
            }

            // Auth effects
            UiEffect::SpawnTokenExchange { code, verifier } => {
                handlers::spawn_token_exchange(&mut self.state, &code, &verifier);
            }

            // Config effects
            UiEffect::OpenConfig => {
                self.handle_open_config();
            }
            UiEffect::PersistModel { model } => {
                if let Err(e) = crate::config::Config::save_model(&model) {
                    handlers::push_warning(
                        &mut self.state,
                        "Warning: Failed to save model preference",
                        e,
                    );
                }
            }
            UiEffect::PersistThinking { level } => {
                if let Err(e) = crate::config::Config::save_thinking_level(level) {
                    handlers::push_warning(
                        &mut self.state,
                        "Warning: Failed to save thinking level",
                        e,
                    );
                }
            }

            // Session effects
            UiEffect::SaveSession { event } => {
                if let Some(ref mut s) = self.state.conversation.session
                    && let Err(e) = s.append(&event)
                {
                    handlers::push_warning(&mut self.state, "Warning: Failed to save session", e);
                }
            }
            UiEffect::CreateNewSession => {
                if handlers::create_session_and_show_context(&mut self.state).is_err() {
                    handlers::push_system(&mut self.state, "Conversation cleared.");
                }
            }
            UiEffect::OpenSessionPicker => {
                handlers::open_session_picker(&mut self.state);
            }
            UiEffect::LoadSession { session_id } => {
                handlers::load_session(&mut self.state, &session_id);
            }
            UiEffect::PreviewSession { session_id } => {
                handlers::preview_session(&mut self.state, &session_id);
            }

            // Handoff effects
            UiEffect::StartHandoff { goal } => {
                if let Some(ref session) = self.state.conversation.session {
                    let session_id = session.id.clone();
                    handoff::spawn_handoff_generation(&mut self.state, &session_id, &goal);
                } else {
                    handlers::push_system(&mut self.state, "Handoff requires an active session.");
                }
            }
            UiEffect::HandoffSubmit { prompt } => {
                handoff::execute_handoff_submit(&mut self.state, &prompt);
            }
        }
    }

    /// Handles opening the config file.
    fn handle_open_config(&mut self) {
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
}

impl Drop for TuiRuntime {
    fn drop(&mut self) {
        let _ = terminal::restore_terminal();
    }
}
