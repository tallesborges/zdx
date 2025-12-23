//! Full-screen alternate-screen TUI.
//!
//! This module provides a full-screen terminal UI using ratatui.
//! Uses the alternate screen buffer for a persistent, scrollable interface.
//!
//! Architecture (post-Slice 3):
//! - `TuiRuntime`: Owns terminal + state, runs event loop, executes effects
//! - `TuiState` (in state.rs): All app state, no terminal
//! - `update()` (in update.rs): The reducer - all state mutations happen here
//! - `view()` (in view.rs): Pure render, no mutations

use std::io::{IsTerminal, Stdout, Write, stderr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::core::interrupt;
use crate::core::session::{self, Session};
use crate::providers::anthropic::ChatMessage;
use crate::ui::effects::UiEffect;
use crate::ui::events::UiEvent;
use crate::ui::state::{EngineState, TuiState};
use crate::ui::terminal;
use crate::ui::transcript::HistoryCell;
use crate::ui::update;
use crate::ui::view::{self, INPUT_HEIGHT, STATUS_HEIGHT};

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

    // Show loaded AGENTS.md files
    if !effective.loaded_agents_paths.is_empty() {
        writeln!(err, "Loaded AGENTS.md from:")?;
        for path in &effective.loaded_agents_paths {
            writeln!(err, "  - {}", path.display())?;
        }
    }

    // Small delay so user can see the info before TUI takes over
    err.flush()?;

    // Create and run the TUI
    let mut runtime = if history.is_empty() {
        TuiRuntime::new(config.clone(), root, effective.prompt, session)?
    } else {
        TuiRuntime::with_history(config.clone(), root, effective.prompt, session, history)?
    };
    runtime.run()?;

    // Print goodbye after TUI exits (terminal restored)
    writeln!(stderr(), "Goodbye!")?;

    Ok(())
}

/// Target frame rate for streaming updates (30fps = ~33ms per frame).
const FRAME_DURATION: std::time::Duration = std::time::Duration::from_millis(33);

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

    /// Returns the viewport height for the transcript area.
    fn transcript_height(&self) -> usize {
        self.terminal
            .size()
            .map(|s| s.height.saturating_sub(INPUT_HEIGHT + STATUS_HEIGHT) as usize)
            .unwrap_or(20)
    }

    fn event_loop(&mut self) -> Result<()> {
        while !self.state.should_quit {
            // Check for Ctrl+C signal (only quit if engine is idle)
            // If engine is running, the interrupt is meant to cancel it, not quit the app.
            // The engine will send an Interrupted event which resets the flag.
            if interrupt::is_interrupted() && !self.state.engine_state.is_running() {
                self.state.should_quit = true;
                break;
            }

            // Collect events from various sources
            let events = self.collect_events()?;

            // Process each event through the reducer
            let viewport_height = self.transcript_height();
            for event in events {
                let effects = update::update(&mut self.state, event, viewport_height);
                self.execute_effects(effects);
            }

            // Apply any pending deltas before render (coalescing)
            update::apply_pending_delta(&mut self.state);

            // Update cached line count for scroll calculations
            let size = self.terminal.size()?;
            let transcript_width = size.width.saturating_sub(2) as usize;
            let line_count = view::calculate_line_count(&self.state, transcript_width);
            self.state.scroll.update_line_count(line_count);

            // Render - state is a separate field, no borrow conflict
            self.terminal.draw(|frame| {
                view::view(&self.state, frame);
            })?;
        }

        Ok(())
    }

    /// Collects events from all sources (terminal, engine, async tasks).
    fn collect_events(&mut self) -> Result<Vec<UiEvent>> {
        let mut events = Vec::new();

        // Always emit a tick for animation/polling
        events.push(UiEvent::Tick);

        // Poll engine events (streaming deltas, tool events, completion, etc.)
        self.collect_engine_events(&mut events);

        // Poll for login exchange result
        self.collect_login_result(&mut events);

        // Poll terminal events with short timeout for responsive streaming
        if event::poll(FRAME_DURATION)? {
            events.push(UiEvent::Terminal(event::read()?));
        }

        Ok(events)
    }

    /// Collects engine events from the channel.
    fn collect_engine_events(&mut self, events: &mut Vec<UiEvent>) {
        while let EngineState::Waiting { rx, .. } | EngineState::Streaming { rx, .. } =
            &mut self.state.engine_state
        {
            let event = match rx.try_recv() {
                Ok(ev) => ev,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            };

            events.push(UiEvent::Engine((*event).clone()));
        }
    }

    /// Collects login exchange result if available.
    fn collect_login_result(&mut self, events: &mut Vec<UiEvent>) {
        let Some(rx) = &mut self.state.login_exchange_rx else {
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
            UiEffect::StartEngineTurn => {
                self.spawn_engine_turn();
            }
            UiEffect::InterruptEngine => {
                self.interrupt_engine();
            }
            UiEffect::SpawnTokenExchange { code, verifier } => {
                self.spawn_token_exchange(&code, &verifier);
            }
            UiEffect::OpenBrowser { url } => {
                let _ = open::that(&url);
            }
            UiEffect::SaveSession { event } => {
                if let Some(ref mut s) = self.state.session
                    && let Err(e) = s.append(&event)
                {
                    self.state.transcript.push(HistoryCell::system(format!(
                        "Warning: Failed to save session: {}",
                        e
                    )));
                }
            }
            UiEffect::PersistModel { model } => {
                if let Err(e) = crate::config::Config::save_model(&model) {
                    self.state.transcript.push(HistoryCell::system(format!(
                        "Warning: Failed to save model preference: {}",
                        e
                    )));
                }
            }
            UiEffect::CreateNewSession => match session::Session::new() {
                Ok(new_session) => {
                    let new_id = new_session.id.clone();
                    self.state.session = Some(new_session);
                    self.state
                        .transcript
                        .push(HistoryCell::system(format!("New session: {}", new_id)));
                }
                Err(e) => {
                    self.state.transcript.push(HistoryCell::system(format!(
                        "Warning: Failed to create new session: {}",
                        e
                    )));
                    self.state
                        .transcript
                        .push(HistoryCell::system("Conversation cleared."));
                }
            },
            UiEffect::ExecuteCommand { name } => {
                let effects = update::execute_command(&mut self.state, name);
                self.execute_effects(effects);
            }
        }
    }

    // ========================================================================
    // Effect Implementations (async spawning, etc.)
    // ========================================================================

    fn interrupt_engine(&mut self) {
        if self.state.engine_state.is_running() {
            interrupt::trigger_ctrl_c();
        }
    }

    fn spawn_engine_turn(&mut self) {
        let (engine_tx, engine_rx) = crate::core::engine::create_event_channel();

        let messages = self.state.messages.clone();
        let config = self.state.config.clone();
        let engine_opts = self.state.engine_opts.clone();
        let system_prompt = self.state.system_prompt.clone();

        let (tui_tx, tui_rx) = crate::core::engine::create_event_channel();

        if let Some(sess) = self.state.session.clone() {
            let (persist_tx, persist_rx) = crate::core::engine::create_event_channel();
            let _fanout =
                crate::core::engine::spawn_fanout_task(engine_rx, vec![tui_tx, persist_tx]);
            let _persist = session::spawn_persist_task(sess, persist_rx);
        } else {
            let _fanout = crate::core::engine::spawn_fanout_task(engine_rx, vec![tui_tx]);
        }

        // Spawn the engine task - it will send TurnComplete when done
        tokio::spawn(async move {
            let _ = crate::core::engine::run_turn(
                messages,
                &config,
                &engine_opts,
                system_prompt.as_deref(),
                engine_tx,
            )
            .await;
        });

        self.state.engine_state = EngineState::Waiting { rx: tui_rx };
    }

    fn spawn_token_exchange(&mut self, code: &str, verifier: &str) {
        use crate::providers::oauth::anthropic;

        let code = code.to_string();
        let pkce_verifier = verifier.to_string();

        let (tx, rx) = mpsc::channel::<Result<(), String>>(1);
        self.state.login_exchange_rx = Some(rx);

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
    // src/ui/commands.rs and src/ui/state.rs respectively.
    //
    // Unit tests for the reducer are in src/ui/update.rs.
}
