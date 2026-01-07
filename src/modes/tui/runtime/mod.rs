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
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::core::interrupt;
use crate::core::thread_log::ThreadLog;
use crate::modes::tui::app::{AgentState, AppState};
use crate::modes::tui::events::{ThreadUiEvent, UiEvent};
use crate::modes::tui::input::HandoffState;
use crate::modes::tui::overlays::Overlay;
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
}

impl TuiRuntime {
    /// Creates a new TUI runtime.
    pub fn new(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        thread_log: Option<ThreadLog>,
    ) -> Result<Self> {
        Self::with_history(config, root, system_prompt, thread_log, Vec::new())
    }

    /// Creates a TUI runtime with pre-loaded message history.
    pub fn with_history(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        thread_log: Option<ThreadLog>,
        history: Vec<ChatMessage>,
    ) -> Result<Self> {
        // Set up panic hook BEFORE entering alternate screen
        terminal::install_panic_hook();

        // Reset interrupt flag in case it was set from a previous run
        interrupt::reset();

        // Enter alternate screen and raw mode
        let terminal = terminal::setup_terminal().context("Failed to setup terminal")?;

        // Create state
        let state = AppState::with_history(config, root, system_prompt, thread_log, history);

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

        while !self.state.tui.should_quit {
            // Check for Ctrl+C signal (only quit if agent is idle)
            // If agent is running, the interrupt is meant to cancel it, not quit the app.
            // The agent will send an Interrupted event which resets the flag.
            if interrupt::is_interrupted() {
                if self.state.tui.agent_state.is_running() {
                    // Let the agent handle the interrupt.
                } else if self.state.tui.bash_rx.is_some() {
                    self.execute_effect(UiEffect::InterruptBash);
                    interrupt::reset();
                } else {
                    self.state.tui.should_quit = true;
                    break;
                }
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
                            || self.state.tui.bash_rx.is_some()
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
        self.collect_login_callback_result(&mut events);

        // Poll for handoff generation result
        self.collect_handoff_result(&mut events);

        // Poll for file discovery result
        self.collect_file_discovery_result(&mut events);

        // Poll for bash execution result
        self.collect_bash_result(&mut events);

        // Poll for thread async operation results
        self.collect_thread_results(&mut events);

        // Determine poll timeout based on activity level.
        // Use fast polling (60fps) when:
        // - Agent is running (streaming content)
        // - Selection clear is pending (visual feedback timer)
        // - Any async operations are in progress
        // Otherwise use slow polling to save CPU.
        let file_discovery_pending = matches!(
            &self.state.overlay,
            Some(Overlay::FilePicker(picker)) if picker.discovery_rx.is_some()
        );
        let needs_fast_poll = self.state.tui.agent_state.is_running()
            || self.state.tui.bash_rx.is_some()
            || self.state.tui.transcript.selection.has_pending_clear()
            || self.state.tui.auth.login_rx.is_some()
            || self.state.tui.input.handoff.is_generating()
            || file_discovery_pending
            || self.state.tui.thread_ops.is_loading();

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

    /// Collects local OAuth callback result if available.
    fn collect_login_callback_result(&mut self, events: &mut Vec<UiEvent>) {
        let Some(rx) = &mut self.state.tui.auth.login_callback_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(code) => {
                self.state.tui.auth.login_callback_rx = None;
                events.push(UiEvent::LoginCallbackResult(code));
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.state.tui.auth.login_callback_rx = None;
                events.push(UiEvent::LoginCallbackResult(None));
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
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                events.push(UiEvent::HandoffResult(Err(
                    "Handoff generation task failed".to_string(),
                )));
            }
        }
    }

    /// Collects file discovery result if available.
    fn collect_file_discovery_result(&mut self, events: &mut Vec<UiEvent>) {
        use tokio::sync::oneshot;

        use crate::modes::tui::overlays::Overlay;

        let Some(Overlay::FilePicker(picker)) = &mut self.state.overlay else {
            return;
        };

        let Some(rx) = &mut picker.discovery_rx else {
            return;
        };

        match rx.try_recv() {
            Ok(files) => {
                events.push(UiEvent::FilesDiscovered(files));
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                // Emit empty list so picker shows "No files found"
                events.push(UiEvent::FilesDiscovered(Vec::new()));
            }
        }
    }

    /// Collects bash execution result if available.
    fn collect_bash_result(&mut self, events: &mut Vec<UiEvent>) {
        use crate::core::events::ToolOutput;

        let Some((id, _command, rx)) = &mut self.state.tui.bash_rx else {
            return;
        };

        match rx.try_recv() {
            Ok(result) => {
                let id = id.clone();
                events.push(UiEvent::BashExecuted { id, result });
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                // Channel closed unexpectedly (task panic/abort) - emit failure to update cell
                let id = id.clone();
                let result =
                    ToolOutput::failure("task_failed", "Bash task terminated unexpectedly");
                events.push(UiEvent::BashExecuted { id, result });
            }
        }
    }

    /// Collects thread async operation results if available.
    fn collect_thread_results(&mut self, events: &mut Vec<UiEvent>) {
        let ops = &mut self.state.tui.thread_ops;

        // Thread list loading
        if let Some(rx) = &mut ops.list_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Thread(ThreadUiEvent::ListFailed {
                        error: "Thread list task failed".to_string(),
                    }));
                }
            }
        }

        // Thread loading (full switch)
        if let Some(rx) = &mut ops.load_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Thread(ThreadUiEvent::LoadFailed {
                        error: "Thread load task failed".to_string(),
                    }));
                }
            }
        }

        // Thread preview loading
        if let Some(rx) = &mut ops.preview_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Thread(ThreadUiEvent::PreviewFailed));
                }
            }
        }

        // Thread creation
        if let Some(rx) = &mut ops.create_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Thread(ThreadUiEvent::CreateFailed {
                        error: "Thread create task failed".to_string(),
                    }));
                }
            }
        }

        // Thread fork
        if let Some(rx) = &mut ops.fork_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Thread(ThreadUiEvent::ForkFailed {
                        error: "Thread fork task failed".to_string(),
                    }));
                }
            }
        }

        // Thread rename
        if let Some(rx) = &mut ops.rename_rx {
            match rx.try_recv() {
                Ok(event) => {
                    events.push(event);
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    events.push(UiEvent::Thread(ThreadUiEvent::RenameFailed {
                        error: "Thread rename task failed".to_string(),
                    }));
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
            UiEffect::InterruptBash => {
                if let Some(cancel) = self.state.tui.bash_cancel.take() {
                    let _ = cancel.send(());
                }
            }

            // Auth effects
            UiEffect::SpawnTokenExchange {
                provider,
                code,
                verifier,
            } => {
                let event = handlers::spawn_token_exchange(provider, &code, &verifier);
                self.dispatch_event(event);
            }
            UiEffect::StartLocalAuthCallback { provider, state } => {
                let event = handlers::spawn_local_auth_callback(provider, state);
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

            // Thread effects (async - spawn tasks and store receivers in state)
            UiEffect::SaveThread { event } => {
                if let Some(ref mut s) = self.state.tui.thread.thread_log {
                    let _ = s.append(&event);
                    // Errors are silently ignored for thread persistence
                }
            }
            UiEffect::RenameThread { thread_id, title } => {
                if self.state.tui.thread_ops.rename_rx.is_none() {
                    let event = handlers::spawn_thread_rename(thread_id, title);
                    self.dispatch_event(event);
                }
            }
            UiEffect::CreateNewThread => {
                // Only spawn if not already loading
                if self.state.tui.thread_ops.create_rx.is_none() {
                    let config = self.state.tui.config.clone();
                    let root = self.state.tui.agent_opts.root.clone();
                    let event = handlers::spawn_thread_create(config, root);
                    self.dispatch_event(event);
                }
            }
            UiEffect::ForkThread {
                events,
                user_input,
                turn_number,
            } => {
                if self.state.tui.thread_ops.fork_rx.is_none() {
                    let event = handlers::spawn_forked_thread(
                        events,
                        user_input,
                        turn_number,
                        self.state.tui.agent_opts.root.clone(),
                    );
                    self.dispatch_event(event);
                }
            }
            UiEffect::OpenThreadPicker => {
                // Only spawn if not already loading and no overlay is open
                if self.state.tui.thread_ops.list_rx.is_none() && self.state.overlay.is_none() {
                    let original_cells = self.state.tui.transcript.cells.clone();
                    let event = handlers::spawn_thread_list_load(original_cells);
                    self.dispatch_event(event);
                }
            }
            UiEffect::LoadThread { thread_id } => {
                // Only spawn if not already loading
                if self.state.tui.thread_ops.load_rx.is_none() {
                    let event = handlers::spawn_thread_load(
                        thread_id,
                        self.state.tui.agent_opts.root.clone(),
                    );
                    self.dispatch_event(event);
                }
            }
            UiEffect::PreviewThread { thread_id } => {
                // Cancel any pending preview and start new one
                let event = handlers::spawn_thread_preview(thread_id);
                self.dispatch_event(event);
            }

            // Handoff effects
            UiEffect::StartHandoff { goal } => {
                if let Some(ref thread_log) = self.state.tui.thread.thread_log {
                    let thread_id = thread_log.id.clone();
                    let root = self.state.tui.agent_opts.root.clone();
                    let event =
                        handoff::spawn_handoff_generation(&thread_id, &goal, root.as_path());
                    self.dispatch_event(event);
                } else {
                    self.dispatch_event(UiEvent::HandoffResult(Err(
                        "Handoff requires an active thread.".to_string(),
                    )));
                }
            }
            UiEffect::HandoffSubmit { prompt } => {
                let root = &self.state.tui.agent_opts.root;
                match handoff::execute_handoff_submit(&prompt, root) {
                    Ok(thread_log) => {
                        self.dispatch_event(UiEvent::HandoffThreadCreated { thread_log });
                    }
                    Err(error) => {
                        self.dispatch_event(UiEvent::HandoffThreadCreateFailed { error });
                    }
                }
            }

            // File picker effects
            UiEffect::DiscoverFiles => {
                let root = self.state.tui.agent_opts.root.clone();
                let event = handlers::spawn_file_discovery(&root);
                self.dispatch_event(event);
            }

            // Clipboard effects
            UiEffect::CopyToClipboard { text } => {
                use crate::modes::tui::shared::Clipboard;
                if Clipboard::copy(&text).is_ok() {
                    self.dispatch_event(UiEvent::ClipboardCopied);
                }
            }

            // Direct bash execution effects
            UiEffect::ExecuteBash { command } => {
                // Only spawn if not already running a bash command
                if self.state.tui.bash_rx.is_none() {
                    let id = format!("user-bash-{}", chrono::Utc::now().timestamp_millis());
                    let root = self.state.tui.agent_opts.root.clone();
                    let event = handlers::spawn_bash_execution(id, command, root);
                    self.dispatch_event(event);
                }
            }
        }
    }
}

impl Drop for TuiRuntime {
    fn drop(&mut self) {
        let _ = terminal::restore_terminal();
    }
}
