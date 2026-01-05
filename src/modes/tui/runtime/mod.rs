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
use crate::modes::tui::app::{AgentState, AppState};
use crate::modes::tui::events::{SessionUiEvent, UiEvent};
use crate::modes::tui::input::HandoffState;
use crate::modes::tui::overlays::{
    CommandPaletteState, FilePickerState, LoginState, ModelPickerState, Overlay,
    ThinkingPickerState,
};
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::{render, terminal, update};
use crate::providers::anthropic::ChatMessage;

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
    /// Application state (split: tui + overlay).
    pub state: AppState,
    /// Receiver for async file discovery (background service, not user workflow).
    file_discovery_rx: Option<mpsc::Receiver<Vec<PathBuf>>>,
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
        let state = AppState::with_history(config, root, system_prompt, session, history);

        Ok(Self {
            terminal,
            state,
            file_discovery_rx: None,
        })
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

        while !self.state.tui.should_quit {
            // Check for Ctrl+C signal (only quit if agent is idle)
            // If agent is running, the interrupt is meant to cancel it, not quit the app.
            // The agent will send an Interrupted event which resets the flag.
            if interrupt::is_interrupted() && !self.state.tui.agent_state.is_running() {
                self.state.tui.should_quit = true;
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
                        self.state.tui.agent_state.is_running()
                            || self.state.tui.transcript.selection.has_pending_clear()
                    }
                    UiEvent::Frame { .. } => false,
                    _ => true,
                };
                let effects = update::update(&mut self.state, event);
                if marks_dirty || !effects.is_empty() {
                    dirty = true;
                }
                self.execute_effects(effects);
            }

            // Only render if something changed
            if dirty {
                // Render - state is a separate field, no borrow conflict
                self.terminal.draw(|frame| {
                    render::render(&self.state, frame);
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

        // Poll for file discovery result
        self.collect_file_discovery_result(&mut events);

        // Poll for session async operation results
        self.collect_session_results(&mut events);

        // Determine poll timeout based on activity level.
        // Use fast polling (60fps) when:
        // - Agent is running (streaming content)
        // - Selection clear is pending (visual feedback timer)
        // - Any async operations are in progress
        // Otherwise use slow polling to save CPU.
        let needs_fast_poll = self.state.tui.agent_state.is_running()
            || self.state.tui.transcript.selection.has_pending_clear()
            || self.state.tui.auth.login_rx.is_some()
            || self.state.tui.input.handoff.is_generating()
            || self.file_discovery_rx.is_some()
            || self.state.tui.session_ops.is_loading();

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
            &mut self.state.tui.agent_state
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
        let Some(rx) = &mut self.state.tui.auth.login_rx else {
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
        let HandoffState::Generating { rx, .. } = &mut self.state.tui.input.handoff else {
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

    /// Collects file discovery result if available.
    fn collect_file_discovery_result(&mut self, events: &mut Vec<UiEvent>) {
        let Some(rx) = &mut self.file_discovery_rx else {
            return;
        };

        match rx.try_recv() {
            Ok(files) => {
                events.push(UiEvent::FilesDiscovered(files));
                // Clear the receiver after getting results
                self.file_discovery_rx = None;
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                // Task failed, close the file picker gracefully
                events.push(UiEvent::FilesDiscovered(Vec::new()));
                self.file_discovery_rx = None;
            }
        }
    }

    /// Collects session async operation results if available.
    fn collect_session_results(&mut self, events: &mut Vec<UiEvent>) {
        let ops = &mut self.state.tui.session_ops;

        // Session list loading
        if let Some(rx) = &mut ops.list_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                    ops.list_rx = None;
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Session(SessionUiEvent::ListFailed {
                        error: "Session list task failed".to_string(),
                    }));
                    ops.list_rx = None;
                }
            }
        }

        // Session loading (full switch)
        if let Some(rx) = &mut ops.load_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                    ops.load_rx = None;
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Session(SessionUiEvent::LoadFailed {
                        error: "Session load task failed".to_string(),
                    }));
                    ops.load_rx = None;
                }
            }
        }

        // Session preview loading
        if let Some(rx) = &mut ops.preview_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                    ops.preview_rx = None;
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Session(SessionUiEvent::PreviewFailed));
                    ops.preview_rx = None;
                }
            }
        }

        // Session creation
        if let Some(rx) = &mut ops.create_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                    ops.create_rx = None;
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Session(SessionUiEvent::CreateFailed {
                        error: "Session create task failed".to_string(),
                    }));
                    ops.create_rx = None;
                }
            }
        }

        // Session rename
        if let Some(rx) = &mut ops.rename_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                    ops.rename_rx = None;
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Session(SessionUiEvent::RenameFailed {
                        error: "Session rename task failed".to_string(),
                    }));
                    ops.rename_rx = None;
                }
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

    fn dispatch_event(&mut self, event: UiEvent) {
        let effects = update::update(&mut self.state, event);
        if !effects.is_empty() {
            self.execute_effects(effects);
        }
    }

    /// Executes a single effect by dispatching to the appropriate handler.
    fn execute_effect(&mut self, effect: UiEffect) {
        match effect {
            // Simple effects (inline)
            UiEffect::Quit => {
                self.state.tui.should_quit = true;
            }
            UiEffect::OpenBrowser { url } => {
                let _ = open::that(&url);
            }

            // Agent effects
            UiEffect::StartAgentTurn => {
                let event = handlers::spawn_agent_turn(&self.state.tui);
                self.dispatch_event(event);
            }
            UiEffect::InterruptAgent => {
                handlers::interrupt_agent(&self.state.tui);
            }

            // Auth effects
            UiEffect::SpawnTokenExchange { code, verifier } => {
                let event = handlers::spawn_token_exchange(&code, &verifier);
                self.dispatch_event(event);
            }

            // Config effects
            UiEffect::OpenConfig => {
                let config_path = crate::config::paths::config_path();
                if config_path.exists() {
                    let _ = open::that(&config_path);
                    // Note: errors are silently ignored for simplicity
                    // Could add an event for error reporting if needed
                }
            }
            UiEffect::PersistModel { model } => {
                let _ = crate::config::Config::save_model(&model);
                // Errors are silently ignored - model is already set in state
            }
            UiEffect::PersistThinking { level } => {
                let _ = crate::config::Config::save_thinking_level(level);
                // Errors are silently ignored - level is already set in state
            }

            // Session effects (async - spawn tasks and store receivers in state)
            UiEffect::SaveSession { event } => {
                if let Some(ref mut s) = self.state.tui.conversation.session {
                    let _ = s.append(&event);
                    // Errors are silently ignored for session persistence
                }
            }
            UiEffect::RenameSession { session_id, title } => {
                if self.state.tui.session_ops.rename_rx.is_none() {
                    self.state.tui.session_ops.rename_rx =
                        Some(handlers::spawn_session_rename(session_id, title));
                }
            }
            UiEffect::CreateNewSession => {
                // Only spawn if not already loading
                if self.state.tui.session_ops.create_rx.is_none() {
                    let config = self.state.tui.config.clone();
                    let root = self.state.tui.agent_opts.root.clone();
                    self.state.tui.session_ops.create_rx =
                        Some(handlers::spawn_session_create(config, root));
                }
            }
            UiEffect::OpenSessionPicker => {
                // Only spawn if not already loading and no overlay is open
                if self.state.tui.session_ops.list_rx.is_none() && self.state.overlay.is_none() {
                    let original_cells = self.state.tui.transcript.cells.clone();
                    self.state.tui.session_ops.list_rx =
                        Some(handlers::spawn_session_list_load(original_cells));
                }
            }
            UiEffect::LoadSession { session_id } => {
                // Only spawn if not already loading
                if self.state.tui.session_ops.load_rx.is_none() {
                    self.state.tui.session_ops.load_rx =
                        Some(handlers::spawn_session_load(session_id));
                }
            }
            UiEffect::PreviewSession { session_id } => {
                // Cancel any pending preview and start new one
                self.state.tui.session_ops.preview_rx =
                    Some(handlers::spawn_session_preview(session_id));
            }

            // Handoff effects
            UiEffect::StartHandoff { goal } => {
                if let Some(ref session) = self.state.tui.conversation.session {
                    let session_id = session.id.clone();
                    let root = self.state.tui.agent_opts.root.clone();
                    let event =
                        handoff::spawn_handoff_generation(&session_id, &goal, root.as_path());
                    self.dispatch_event(event);
                } else {
                    self.dispatch_event(UiEvent::HandoffResult(Err(
                        "Handoff requires an active session.".to_string(),
                    )));
                }
            }
            UiEffect::HandoffSubmit { prompt } => match handoff::execute_handoff_submit(&prompt) {
                Ok(session) => self.dispatch_event(UiEvent::HandoffSessionCreated { session }),
                Err(error) => {
                    self.dispatch_event(UiEvent::HandoffSessionCreateFailed { error });
                }
            },

            // File picker effects
            UiEffect::DiscoverFiles => {
                let root = self.state.tui.agent_opts.root.clone();
                self.file_discovery_rx = Some(handlers::spawn_file_discovery(&root));
            }

            // Overlay effects
            UiEffect::OpenCommandPalette { command_mode } => {
                if self.state.overlay.is_none() {
                    let (state, effects) = CommandPaletteState::open(command_mode);
                    self.set_overlay(Overlay::CommandPalette(state), effects);
                }
            }
            UiEffect::OpenFilePicker { trigger_pos } => {
                if self.state.overlay.is_none() {
                    let (state, effects) = FilePickerState::open(trigger_pos);
                    self.set_overlay(Overlay::FilePicker(state), effects);
                }
            }
            UiEffect::OpenModelPicker => {
                if self.state.overlay.is_none() {
                    let (state, effects) = ModelPickerState::open(&self.state.tui.config.model);
                    self.set_overlay(Overlay::ModelPicker(state), effects);
                }
            }
            UiEffect::OpenThinkingPicker => {
                if self.state.overlay.is_none() {
                    let (state, effects) =
                        ThinkingPickerState::open(self.state.tui.config.thinking_level);
                    self.set_overlay(Overlay::ThinkingPicker(state), effects);
                }
            }
            UiEffect::OpenLogin => {
                if self.state.overlay.is_none() {
                    let (state, effects) = LoginState::open();
                    self.set_overlay(Overlay::Login(state), effects);
                }
            }

            // Clipboard effects
            UiEffect::CopyToClipboard { text } => {
                use crate::modes::tui::shared::Clipboard;
                if Clipboard::copy(&text).is_ok() {
                    self.dispatch_event(UiEvent::ClipboardCopied);
                }
            }
        }
    }
}

impl TuiRuntime {
    fn set_overlay(&mut self, overlay: Overlay, effects: Vec<UiEffect>) {
        self.state.overlay = Some(overlay);
        if !effects.is_empty() {
            self.execute_effects(effects);
        }
    }
}

impl Drop for TuiRuntime {
    fn drop(&mut self) {
        let _ = terminal::restore_terminal();
    }
}
