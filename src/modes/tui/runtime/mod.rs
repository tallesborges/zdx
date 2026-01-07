//! TUI runtime - owns terminal, runs event loop, executes effects.
//!
//! This is the "Elm runtime" boundary: all side effects happen here.
//! The reducer stays pure and produces effects; this module executes them.
//!
//! ## Inbox Pattern
//!
//! The runtime uses an "inbox" pattern for async event collection:
//! - Handlers send `UiEvent`s directly to `inbox_tx`
//! - Runtime drains `inbox_rx` each frame to collect results
//! - This eliminates per-operation receivers and simplifies event collection
//!
//! Structure:
//! - `mod.rs`: Core runtime (TuiRuntime, event loop, effect dispatch)
//! - `handlers.rs`: Effect handler implementations (I/O, spawning, etc.)
//! - `handoff.rs`: Handoff generation handlers (subagent spawning)

mod handlers;
mod handoff;

use std::future::Future;
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

use handlers::{UiEventReceiver, UiEventSender};

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
    /// Inbox sender - handlers send events here.
    inbox_tx: UiEventSender,
    /// Inbox receiver - runtime drains this each frame.
    inbox_rx: UiEventReceiver,
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

        // Create inbox channel for async event collection
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();

        Ok(Self {
            terminal,
            state,
            inbox_tx,
            inbox_rx,
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
            if interrupt::is_interrupted() {
                if self.state.tui.agent_state.is_running() {
                    // Let the agent handle the interrupt.
                } else if self.state.tui.bash_running.is_some() {
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
                            || self.state.tui.bash_running.is_some()
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

    /// Collects events from all sources (terminal, agent, inbox).
    ///
    /// With the inbox pattern, most async results arrive via `inbox_rx`.
    /// Agent events still use their dedicated channel for streaming.
    fn collect_events(&mut self) -> Result<Vec<UiEvent>> {
        let mut events = Vec::new();

        // Always emit a tick for animation/polling
        events.push(UiEvent::Tick);

        // Poll agent events (streaming deltas, tool events, completion, etc.)
        // Agent streaming is kept separate for now - could be unified later
        self.collect_agent_events(&mut events);

        // Poll for handoff generation result (still uses oneshot)
        self.collect_handoff_result(&mut events);

        // Drain inbox - all other async results arrive here
        self.collect_inbox_events(&mut events);

        // Determine poll timeout based on activity level.
        // Use fast polling (60fps) when:
        // - Agent is running (streaming content)
        // - Bash is running
        // - Selection clear is pending (visual feedback timer)
        // - Any async operations are in progress
        // Otherwise use slow polling to save CPU.
        let file_discovery_pending = matches!(
            &self.state.overlay,
            Some(Overlay::FilePicker(picker)) if picker.discovery_cancel.is_some()
        );
        let needs_fast_poll = self.state.tui.agent_state.is_running()
            || self.state.tui.bash_running.is_some()
            || self.state.tui.transcript.selection.has_pending_clear()
            || self.state.tui.auth.login_in_progress
            || self.state.tui.auth.callback_in_progress
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

    /// Collects handoff generation result if available.
    ///
    /// Handoff still uses oneshot because it has a cancel mechanism that
    /// needs to be stored in state.
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

    /// Drains all events from the inbox channel.
    ///
    /// This is the main event collection point for the inbox pattern.
    /// All async handlers (thread ops, auth, file discovery, bash) send
    /// their results here.
    fn collect_inbox_events(&mut self, events: &mut Vec<UiEvent>) {
        while let Ok(ev) = self.inbox_rx.try_recv() {
            events.push(ev);
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

    /// Spawns an async effect, sending an optional "started" event immediately
    /// and the result event when complete.
    ///
    /// This centralizes the spawn-and-send pattern: handlers become pure async
    /// functions that return `UiEvent`, while the runtime handles spawning.
    fn spawn_effect<F, Fut>(&self, started: Option<UiEvent>, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = UiEvent> + Send + 'static,
    {
        let tx = self.inbox_tx.clone();
        if let Some(ev) = started {
            let _ = tx.send(ev);
        }
        tokio::spawn(async move {
            let _ = tx.send(f().await);
        });
    }

    /// Spawns an effect that returns both a started event and a future.
    ///
    /// Used for handlers with cancel tokens where the started event contains
    /// shared state (the cancel token) that the future also needs.
    fn spawn_effect_pair<Fut>(&self, started: UiEvent, fut: Fut)
    where
        Fut: Future<Output = UiEvent> + Send + 'static,
    {
        let tx = self.inbox_tx.clone();
        let _ = tx.send(started);
        tokio::spawn(async move {
            let _ = tx.send(fut.await);
        });
    }

    /// Executes a single effect by dispatching to the appropriate handler.
    ///
    /// Uses `spawn_effect` for pure async handlers (thread ops, auth) and
    /// `spawn_effect_pair` for handlers with cancel tokens (file discovery, bash).
    fn execute_effect(&mut self, effect: UiEffect) {
        match effect {
            // Simple effects (inline)
            UiEffect::Quit => {
                self.state.tui.should_quit = true;
            }
            UiEffect::OpenBrowser { url } => {
                let _ = open::that(&url);
            }

            // Agent effects (still returns event for now - streaming is special)
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

            // Auth effects (pure async handlers)
            UiEffect::SpawnTokenExchange {
                provider,
                code,
                verifier,
            } => {
                self.state.tui.auth.login_in_progress = true;
                self.spawn_effect(None, move || {
                    handlers::token_exchange(provider, code, verifier)
                });
            }
            UiEffect::StartLocalAuthCallback { provider, state } => {
                self.state.tui.auth.callback_in_progress = true;
                self.spawn_effect(None, move || handlers::local_auth_callback(provider, state));
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

            // Thread effects (pure async handlers)
            UiEffect::SaveThread { event } => {
                if let Some(ref mut s) = self.state.tui.thread.thread_log {
                    let _ = s.append(&event);
                    // Errors are silently ignored for thread persistence
                }
            }
            UiEffect::RenameThread { thread_id, title } => {
                if !self.state.tui.thread_ops.rename_loading {
                    self.spawn_effect(
                        Some(UiEvent::Thread(ThreadUiEvent::RenameStarted)),
                        move || handlers::thread_rename(thread_id, title),
                    );
                }
            }
            UiEffect::CreateNewThread => {
                // Only spawn if not already loading
                if !self.state.tui.thread_ops.create_loading {
                    let config = self.state.tui.config.clone();
                    let root = self.state.tui.agent_opts.root.clone();
                    self.spawn_effect(
                        Some(UiEvent::Thread(ThreadUiEvent::CreateStarted)),
                        move || handlers::thread_create(config, root),
                    );
                }
            }
            UiEffect::ForkThread {
                events,
                user_input,
                turn_number,
            } => {
                if !self.state.tui.thread_ops.fork_loading {
                    let root = self.state.tui.agent_opts.root.clone();
                    self.spawn_effect(
                        Some(UiEvent::Thread(ThreadUiEvent::ForkStarted)),
                        move || handlers::thread_fork(events, user_input, turn_number, root),
                    );
                }
            }
            UiEffect::OpenThreadPicker => {
                // Only spawn if not already loading and no overlay is open
                if !self.state.tui.thread_ops.list_loading && self.state.overlay.is_none() {
                    let original_cells = self.state.tui.transcript.cells.clone();
                    self.spawn_effect(
                        Some(UiEvent::Thread(ThreadUiEvent::ListStarted)),
                        move || handlers::thread_list_load(original_cells),
                    );
                }
            }
            UiEffect::LoadThread { thread_id } => {
                // Only spawn if not already loading
                if !self.state.tui.thread_ops.load_loading {
                    let root = self.state.tui.agent_opts.root.clone();
                    self.spawn_effect(
                        Some(UiEvent::Thread(ThreadUiEvent::LoadStarted)),
                        move || handlers::thread_load(thread_id, root),
                    );
                }
            }
            UiEffect::PreviewThread { thread_id } => {
                self.spawn_effect(
                    Some(UiEvent::Thread(ThreadUiEvent::PreviewStarted)),
                    move || handlers::thread_preview(thread_id),
                );
            }

            // Handoff effects (still uses oneshot for cancel mechanism)
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

            // File picker effects (returns started event + future for cancel support)
            UiEffect::DiscoverFiles => {
                let root = self.state.tui.agent_opts.root.clone();
                let (started, fut) = handlers::file_discovery(root);
                self.spawn_effect_pair(started, fut);
            }

            // Clipboard effects
            UiEffect::CopyToClipboard { text } => {
                use crate::modes::tui::shared::Clipboard;
                if Clipboard::copy(&text).is_ok() {
                    self.dispatch_event(UiEvent::ClipboardCopied);
                }
            }

            // Direct bash execution effects (returns started event + future for cancel support)
            UiEffect::ExecuteBash { command } => {
                // Only spawn if not already running a bash command
                if self.state.tui.bash_running.is_none() {
                    let id = format!("user-bash-{}", chrono::Utc::now().timestamp_millis());
                    let root = self.state.tui.agent_opts.root.clone();
                    let (started, fut) = handlers::bash_execution(id, command, root);
                    self.spawn_effect_pair(started, fut);
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
