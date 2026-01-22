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
//! - `inbox.rs`: Inbox channel types
//! - `handlers/`: Effect handler implementations (I/O, spawning, etc.)
//! - `handoff.rs`: Handoff generation handlers (subagent spawning)
//! - `thread_title.rs`: Auto-title generation handlers (subagent spawning)

mod handlers;
mod handoff;
mod inbox;
mod thread_title;

use std::future::Future;
use std::io::Stdout;
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event;
use inbox::{UiEventReceiver, UiEventSender};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zdx_core::config::Config;
use zdx_core::core::interrupt;
use zdx_core::core::thread_log::ThreadLog;
use zdx_core::providers::ChatMessage;

use crate::common::{TaskCompleted, TaskId, TaskKind, TaskMeta, TaskStarted};
use crate::effects::UiEffect;
use crate::events::UiEvent;
use crate::state::{AgentState, AppState};
use crate::{render, terminal, update};

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
    /// Last time a Tick event was emitted.
    last_tick: std::time::Instant,
    /// Last time a render occurred (for FPS calculation).
    last_render: std::time::Instant,
    /// Last time a terminal event was received (for fast tick during interaction).
    last_terminal_event: std::time::Instant,
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
        interrupt::set_restore_hook(|| {
            let _ = terminal::restore_terminal();
        });

        // Reset interrupt flag in case it was set from a previous run
        interrupt::reset();

        // Enter alternate screen and raw mode
        let terminal = terminal::setup_terminal().context("Failed to setup terminal")?;

        // Create state
        let state = AppState::with_history(config, root, system_prompt, thread_log, history);

        // Create inbox channel for async event collection
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();

        let now = std::time::Instant::now();
        Ok(Self {
            terminal,
            state,
            inbox_tx,
            inbox_rx,
            last_tick: now,
            last_render: now,
            last_terminal_event: now,
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
                // Track terminal activity for fast tick mode
                if matches!(&event, UiEvent::Terminal(_)) {
                    self.last_terminal_event = std::time::Instant::now();
                }

                // Only Tick triggers render - this caps frame rate at tick cadence
                // Terminal events update state but batch renders to next Tick
                let marks_dirty = matches!(&event, UiEvent::Tick);

                let effects = update::update(&mut self.state, event);
                if marks_dirty {
                    dirty = true;
                }
                self.execute_effects(effects);
            }

            // Only render if something changed
            if dirty {
                // Measure time since last render (actual frame interval for FPS)
                let frame_ms = self.last_render.elapsed().as_millis() as u16;
                self.last_render = std::time::Instant::now();

                // Render - state is a separate field, no borrow conflict
                self.terminal.draw(|frame| {
                    render::render(&self.state, frame);
                })?;

                dirty = false;

                // Update FPS based on actual render interval
                self.state.tui.status_line.on_frame(frame_ms);
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

        // Determine tick interval based on activity level.
        // Use fast polling (60fps) when:
        // - Agent is running (streaming content)
        // - Bash is running
        // - Selection clear is pending (visual feedback timer)
        // - Any async operations are in progress
        // - Recent terminal activity (scrolling, typing)
        // Otherwise use slow polling to save CPU.
        let recent_terminal_activity = self.last_terminal_event.elapsed() < IDLE_POLL_DURATION;
        let needs_fast_poll = self.state.tui.agent_state.is_running()
            || self.state.tui.bash_running.is_some()
            || self.state.tui.transcript.selection.has_pending_clear()
            || self.state.tui.auth.callback_in_progress
            || self.state.tui.input.handoff.is_generating()
            || self.state.tui.tasks.is_any_running()
            || recent_terminal_activity;

        let tick_interval = if needs_fast_poll {
            FRAME_DURATION
        } else {
            IDLE_POLL_DURATION
        };

        // Poll agent events (streaming deltas, tool events, completion, etc.)
        // Agent streaming is kept separate for now - could be unified later
        self.collect_agent_events(&mut events);

        // Drain inbox - all async results arrive here
        self.collect_inbox_events(&mut events);

        // Calculate time until next tick for poll duration.
        // This ensures we wake up exactly when Tick is due.
        let time_until_tick = tick_interval.saturating_sub(self.last_tick.elapsed());

        // Poll terminal events:
        // - If we already have events to process, do non-blocking poll (don't delay rendering)
        // - Otherwise, block until next tick is due (keeps input responsive while hitting tick cadence)
        let poll_duration = if events.is_empty() {
            time_until_tick
        } else {
            std::time::Duration::ZERO
        };

        if event::poll(poll_duration)? {
            events.push(UiEvent::Terminal(event::read()?));
            // Drain any remaining buffered events (non-blocking)
            while event::poll(std::time::Duration::ZERO)? {
                events.push(UiEvent::Terminal(event::read()?));
            }
        }

        // Emit Tick after poll - we've now waited until the tick interval elapsed
        // (or woke early due to terminal input, in which case we check again)
        if self.last_tick.elapsed() >= tick_interval {
            events.push(UiEvent::Tick);
            self.last_tick = std::time::Instant::now();
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

    /// Spawns an async task with a uniform TaskStarted/TaskCompleted lifecycle.
    fn spawn_task<F, Fut>(&self, kind: TaskKind, id: TaskId, meta: TaskMeta, cancelable: bool, f: F)
    where
        F: FnOnce(Option<CancellationToken>) -> Fut + Send + 'static,
        Fut: Future<Output = UiEvent> + Send + 'static,
    {
        let tx = self.inbox_tx.clone();
        let cancel = cancelable.then(CancellationToken::new);
        let started = TaskStarted {
            id,
            cancel: cancel.clone(),
            meta,
        };
        let _ = tx.send(UiEvent::TaskStarted { kind, started });
        tokio::spawn(async move {
            let inner = f(cancel).await;
            let completed = TaskCompleted {
                id,
                result: Box::new(inner),
            };
            let _ = tx.send(UiEvent::TaskCompleted { kind, completed });
        });
    }

    /// Executes a single effect by dispatching to the appropriate handler.
    ///
    /// Uses `spawn_effect` for pure async handlers (thread ops, auth) and
    /// `spawn_task` for async task lifecycles.
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
                // Unified cancellation: call cancel() on the token
                if let Some(cancel) = self.state.tui.tasks.bash.cancel.clone() {
                    cancel.cancel();
                }
            }

            // ================================================================
            // Cancellation Effects
            // ================================================================
            // These are emitted by the reducer (e.g., on Esc key) to cancel
            // in-progress operations. The runtime just calls cancel() on
            // the provided token.
            UiEffect::CancelTask { token, .. } => {
                if let Some(cancel) = token {
                    cancel.cancel();
                }
            }

            // Auth effects
            UiEffect::SpawnTokenExchange {
                task,
                provider,
                code,
                verifier,
                redirect_uri,
            } => {
                let Some(task) = task else {
                    return;
                };
                self.spawn_task(
                    TaskKind::LoginExchange,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| handlers::token_exchange(provider, code, verifier, redirect_uri),
                );
            }
            UiEffect::StartLocalAuthCallback {
                provider,
                state,
                port,
            } => {
                // Note: callback_in_progress is set by reducer via mutation when emitting this effect
                self.spawn_effect(None, move || {
                    handlers::local_auth_callback(provider, state, port)
                });
            }

            // Config effects
            UiEffect::OpenConfig => {
                let config_path = zdx_core::config::paths::config_path();
                if config_path.exists() {
                    let _ = open::that(&config_path);
                    // Note: errors are silently ignored for simplicity
                    // Could add an event for error reporting if needed
                }
            }
            UiEffect::OpenModelsConfig => {
                let models_path = self.state.tui.config.models_path();
                if !models_path.exists() {
                    if let Some(parent) = models_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&models_path, zdx_core::models::default_models_toml());
                }
                let _ = open::that(&models_path);
                // Note: errors are silently ignored for simplicity
                // Could add an event for error reporting if needed
            }
            UiEffect::PersistModel { model } => {
                let _ = zdx_core::config::Config::save_model(&model);
                // Errors are silently ignored - model is already set in state
            }
            UiEffect::PersistThinking { level } => {
                let _ = zdx_core::config::Config::save_thinking_level(level);
                // Errors are silently ignored - level is already set in state
            }

            // Thread effects (pure async handlers)
            UiEffect::SaveThread { event } => {
                if let Some(ref mut s) = self.state.tui.thread.thread_log {
                    let _ = s.append(&event);
                    // Errors are silently ignored for thread persistence
                }
            }
            UiEffect::RenameThread {
                task,
                thread_id,
                title,
            } => {
                let Some(task) = task else {
                    return;
                };
                self.spawn_task(
                    TaskKind::ThreadRename,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| handlers::thread_rename(thread_id, title),
                );
            }
            UiEffect::SuggestThreadTitle {
                task,
                thread_id,
                message,
            } => {
                let Some(task) = task else {
                    return;
                };
                let is_current = self
                    .state
                    .tui
                    .thread
                    .thread_log
                    .as_ref()
                    .is_some_and(|log| log.id == thread_id);
                if !is_current {
                    return;
                }
                let root = self.state.tui.agent_opts.root.clone();
                let title_model = self.state.tui.config.title_model.clone();
                self.spawn_task(
                    TaskKind::ThreadTitle,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| {
                        thread_title::suggest_thread_title(thread_id, message, title_model, root)
                    },
                );
            }
            UiEffect::CreateNewThread { task } => {
                let Some(task) = task else {
                    return;
                };
                let config = self.state.tui.config.clone();
                let root = self.state.tui.agent_opts.root.clone();
                self.spawn_task(
                    TaskKind::ThreadCreate,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| handlers::thread_create(config, root),
                );
            }
            UiEffect::ForkThread {
                task,
                events,
                user_input,
                turn_number,
            } => {
                let Some(task) = task else {
                    return;
                };
                let root = self.state.tui.agent_opts.root.clone();
                self.spawn_task(
                    TaskKind::ThreadFork,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| handlers::thread_fork(events, user_input, turn_number, root),
                );
            }
            UiEffect::OpenThreadPicker { task, mode } => {
                let Some(task) = task else {
                    return;
                };
                let original_cells = if mode.is_switch() {
                    self.state.tui.transcript.cells().to_vec()
                } else {
                    Vec::new()
                };
                self.spawn_task(
                    TaskKind::ThreadList,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| handlers::thread_list_load(original_cells, mode),
                );
            }
            UiEffect::LoadThread { task, thread_id } => {
                let Some(task) = task else {
                    return;
                };
                let root = self.state.tui.agent_opts.root.clone();
                self.spawn_task(
                    TaskKind::ThreadLoad,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| handlers::thread_load(thread_id, root),
                );
            }
            UiEffect::PreviewThread { task, thread_id } => {
                let Some(task) = task else {
                    return;
                };
                self.spawn_task(
                    TaskKind::ThreadPreview,
                    task,
                    TaskMeta::None,
                    false,
                    move |_| handlers::thread_preview(thread_id),
                );
            }

            // Handoff effects
            UiEffect::StartHandoff { task, goal } => {
                let Some(task) = task else {
                    return;
                };
                if let Some(ref thread_log) = self.state.tui.thread.thread_log {
                    let thread_id = thread_log.id.clone();
                    let root = self.state.tui.agent_opts.root.clone();
                    let handoff_model = self.state.tui.config.handoff_model.clone();
                    let meta = TaskMeta::Handoff { goal: goal.clone() };
                    self.spawn_task(TaskKind::Handoff, task, meta, true, move |cancel| {
                        handoff::handoff_generation(thread_id, goal, handoff_model, root, cancel)
                    });
                } else {
                    self.dispatch_event(UiEvent::HandoffResult(Err(
                        "Handoff requires an active thread.".to_string(),
                    )));
                }
            }
            UiEffect::HandoffSubmit {
                prompt,
                handoff_from,
            } => {
                let root = &self.state.tui.agent_opts.root;
                let config = self.state.tui.config.clone();
                match handoff::execute_handoff_submit(&config, root, handoff_from) {
                    Ok((thread_log, context_paths)) => {
                        self.dispatch_event(UiEvent::HandoffThreadCreated {
                            thread_log,
                            context_paths,
                            prompt,
                        });
                    }
                    Err(error) => {
                        self.dispatch_event(UiEvent::HandoffThreadCreateFailed { error });
                    }
                }
            }

            // File picker effects
            UiEffect::DiscoverFiles { task } => {
                let Some(task) = task else {
                    return;
                };
                let root = self.state.tui.agent_opts.root.clone();
                self.spawn_task(
                    TaskKind::FileDiscovery,
                    task,
                    TaskMeta::None,
                    true,
                    move |cancel| handlers::file_discovery(root, cancel),
                );
            }

            // Clipboard effects
            UiEffect::CopyToClipboard { text } => {
                use crate::common::Clipboard;
                if Clipboard::copy(&text).is_ok() {
                    self.dispatch_event(UiEvent::ClipboardCopied);
                }
            }

            // Direct bash execution effects
            UiEffect::ExecuteBash { task, command } => {
                let Some(task) = task else {
                    return;
                };
                // Only spawn if not already running a bash command
                if self.state.tui.bash_running.is_none() {
                    let id = format!("user-bash-{}", chrono::Utc::now().timestamp_millis());
                    let root = self.state.tui.agent_opts.root.clone();
                    let meta = TaskMeta::Bash {
                        id: id.clone(),
                        command: command.clone(),
                    };
                    self.spawn_task(TaskKind::Bash, task, meta, true, move |cancel| {
                        handlers::bash_execution(id, command, root, cancel)
                    });
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
