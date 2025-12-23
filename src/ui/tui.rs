//! Full-screen alternate-screen TUI.
//!
//! This module provides a full-screen terminal UI using ratatui.
//! Uses the alternate screen buffer for a persistent, scrollable interface.
//!
//! Architecture (post-Slice 2):
//! - `TuiRuntime`: Owns terminal + state, runs event loop
//! - `TuiState` (in state.rs): All app state, no terminal
//! - `view()` (in view.rs): Pure render, no mutations

use std::io::{IsTerminal, Stdout, Write, stderr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::core::events::EngineEvent;
use crate::core::interrupt;
use crate::core::session::{self, Session, SessionEvent};
use crate::models::AVAILABLE_MODELS;
use crate::providers::anthropic::ChatMessage;
use crate::ui::state::{
    CommandPaletteState, EngineState, LoginEvent, LoginState, ModelPickerState, ScrollMode,
    TuiState,
};
use crate::ui::terminal;
use crate::ui::transcript::HistoryCell;
use crate::ui::view::{self, HEADER_HEIGHT, INPUT_HEIGHT};

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

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

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
        while !self.state.should_quit {
            // Check for Ctrl+C signal
            if interrupt::is_interrupted() {
                self.state.should_quit = true;
                break;
            }

            // Poll engine events (streaming deltas, completion, etc.)
            self.poll_engine_events();

            // Apply any pending deltas before render (coalescing)
            self.apply_pending_delta();

            // Check for engine task completion
            self.poll_engine_completion();

            // Poll for login exchange result
            self.poll_login_result();

            // Advance spinner animation frame
            self.state.spinner_frame = self.state.spinner_frame.wrapping_add(1);

            // Render - state is a separate field, no borrow conflict
            self.render()?;

            // Handle terminal events with short timeout for responsive streaming
            if event::poll(FRAME_DURATION)? {
                self.handle_event(event::read()?)?;
            }
        }

        Ok(())
    }

    /// Renders the UI.
    ///
    /// This is now clean: terminal and state are separate fields,
    /// so view() can borrow state while terminal.draw() borrows terminal.
    fn render(&mut self) -> Result<()> {
        // Update cached line count for scroll calculations
        let size = self.terminal.size()?;
        let transcript_width = size.width.saturating_sub(2) as usize;
        self.state.cached_line_count = view::calculate_line_count(&self.state, transcript_width);

        // Draw the UI - state is borrowed immutably, no clones needed
        self.terminal.draw(|frame| {
            view::view(&self.state, frame);
        })?;

        Ok(())
    }

    /// Polls the engine event channel for streaming events (non-blocking).
    fn poll_engine_events(&mut self) {
        while let EngineState::Waiting { rx, .. } | EngineState::Streaming { rx, .. } =
            &mut self.state.engine_state
        {
            let event = match rx.try_recv() {
                Ok(ev) => ev,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            };

            let event = (*event).clone();
            self.handle_engine_event(&event);
        }
    }

    /// Handles a single engine event, updating state as needed.
    fn handle_engine_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::AssistantDelta { text } => {
                match &mut self.state.engine_state {
                    EngineState::Waiting { .. } => {
                        // Create streaming cell and transition to Streaming state
                        let cell = HistoryCell::assistant_streaming("");
                        let cell_id = cell.id();
                        self.state.transcript.push(cell);

                        let old_state =
                            std::mem::replace(&mut self.state.engine_state, EngineState::Idle);
                        if let EngineState::Waiting { handle, rx } = old_state {
                            self.state.engine_state = EngineState::Streaming {
                                handle,
                                rx,
                                cell_id,
                                pending_delta: text.clone(),
                            };
                        }
                    }
                    EngineState::Streaming {
                        cell_id,
                        pending_delta,
                        ..
                    } => {
                        // Check if current cell was finalized
                        let needs_new_cell = self
                            .state
                            .transcript
                            .iter()
                            .find(|c| c.id() == *cell_id)
                            .map(|c| {
                                matches!(c, HistoryCell::Assistant { is_streaming, .. } if !*is_streaming)
                            })
                            .unwrap_or(false);

                        if needs_new_cell {
                            let new_cell = HistoryCell::assistant_streaming("");
                            let new_cell_id = new_cell.id();
                            self.state.transcript.push(new_cell);
                            *cell_id = new_cell_id;
                            pending_delta.clear();
                            pending_delta.push_str(text);
                        } else {
                            pending_delta.push_str(text);
                        }
                    }
                    EngineState::Idle => {}
                }
            }
            EngineEvent::AssistantFinal { .. } => {
                if let EngineState::Streaming { cell_id, .. } = &self.state.engine_state
                    && let Some(cell) = self
                        .state
                        .transcript
                        .iter_mut()
                        .find(|c| c.id() == *cell_id)
                {
                    cell.finalize_assistant();
                }
            }
            EngineEvent::Error { message, .. } => {
                self.state
                    .transcript
                    .push(HistoryCell::system(format!("Error: {}", message)));
            }
            EngineEvent::Interrupted => {
                self.state
                    .transcript
                    .push(HistoryCell::system("[Interrupted]"));
                interrupt::reset();
            }
            EngineEvent::ToolRequested { id, name, input } => {
                let tool_cell = HistoryCell::tool_running(id, name, input.clone());
                self.state.transcript.push(tool_cell);
            }
            EngineEvent::ToolStarted { .. } => {}
            EngineEvent::ToolFinished { id, result } => {
                if let Some(cell) = self.state.transcript.iter_mut().find(
                    |c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if tool_use_id == id),
                ) {
                    cell.set_tool_result(result.clone());
                }
            }
        }
    }

    /// Applies any pending delta to the streaming cell (coalescing).
    fn apply_pending_delta(&mut self) {
        if let EngineState::Streaming {
            cell_id,
            pending_delta,
            ..
        } = &mut self.state.engine_state
            && !pending_delta.is_empty()
        {
            if let Some(cell) = self
                .state
                .transcript
                .iter_mut()
                .find(|c| c.id() == *cell_id)
            {
                cell.append_assistant_delta(pending_delta);
            }
            pending_delta.clear();
        }
    }

    /// Polls the engine task for completion (non-blocking).
    fn poll_engine_completion(&mut self) {
        let is_finished = match &self.state.engine_state {
            EngineState::Waiting { handle, .. } | EngineState::Streaming { handle, .. } => {
                handle.is_finished()
            }
            EngineState::Idle => false,
        };

        if !is_finished {
            return;
        }

        let old_state = std::mem::replace(&mut self.state.engine_state, EngineState::Idle);

        let (handle, had_streaming_cell) = match old_state {
            EngineState::Waiting { handle, .. } => (handle, false),
            EngineState::Streaming { handle, .. } => (handle, true),
            EngineState::Idle => return,
        };

        match futures_util::FutureExt::now_or_never(handle) {
            Some(Ok(Ok((final_text, new_messages)))) => {
                self.state.messages = new_messages;

                if !final_text.is_empty()
                    && let Some(ref mut s) = self.state.session
                    && let Err(e) = s.append(&SessionEvent::assistant_message(&final_text))
                {
                    self.state.transcript.push(HistoryCell::system(format!(
                        "Warning: Failed to save session: {}",
                        e
                    )));
                }
            }
            Some(Ok(Err(e))) => {
                if e.downcast_ref::<crate::core::interrupt::InterruptedError>()
                    .is_some()
                {
                    // Already handled by Interrupted event
                } else if !had_streaming_cell {
                    self.state
                        .transcript
                        .push(HistoryCell::system(format!("Error: {}", e)));
                }
                self.state.messages.pop();
            }
            Some(Err(e)) => {
                self.state
                    .transcript
                    .push(HistoryCell::system(format!("Internal error: {}", e)));
                self.state.messages.pop();
            }
            None => {}
        }
    }

    /// Handles a terminal event.
    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => {
                self.handle_mouse(mouse);
                Ok(())
            }
            Event::Paste(text) => {
                if let LoginState::AwaitingCode { ref mut input, .. } = self.state.login_state {
                    input.push_str(&text);
                } else {
                    self.state.textarea.insert_str(&text);
                }
                Ok(())
            }
            Event::Resize(_, _) => Ok(()),
            _ => Ok(()),
        }
    }

    /// Handles a mouse event.
    fn handle_mouse(&mut self, mouse: event::MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_lines_up(MOUSE_SCROLL_LINES);
            }
            MouseEventKind::ScrollDown => {
                self.scroll_lines_down(MOUSE_SCROLL_LINES);
            }
            _ => {}
        }
    }

    /// Handles a key event.
    fn handle_key(&mut self, key: event::KeyEvent) -> Result<()> {
        // Route to overlay handlers first
        if self.state.login_state.is_active() {
            return self.handle_login_key(key);
        }
        if self.state.command_palette.is_some() {
            return self.handle_palette_key(key);
        }
        if self.state.model_picker.is_some() {
            return self.handle_model_picker_key(key);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            KeyCode::Char('/') if !ctrl && !shift && !alt => {
                if self.state.get_input_text().is_empty() {
                    self.open_command_palette(false);
                } else {
                    self.state.textarea.input(key);
                }
            }
            KeyCode::Char('p') if ctrl && !shift && !alt => {
                self.open_command_palette(false);
            }
            KeyCode::Char('q') if !ctrl && !shift && !alt => {
                if self.state.get_input_text().is_empty() {
                    self.state.should_quit = true;
                } else {
                    self.state.textarea.input(key);
                }
            }
            KeyCode::Char('c') if ctrl => {
                if self.state.engine_state.is_running() {
                    self.interrupt_engine();
                } else if !self.state.get_input_text().is_empty() {
                    self.state.clear_input();
                } else {
                    self.state.should_quit = true;
                }
            }
            KeyCode::Enter if !shift && !alt => {
                self.submit_input();
            }
            KeyCode::Char('j') if ctrl => {
                self.state.textarea.insert_newline();
            }
            KeyCode::Esc => {
                if self.state.engine_state.is_running() {
                    self.interrupt_engine();
                } else {
                    self.state.clear_input();
                }
            }
            KeyCode::PageUp => {
                self.scroll_page_up();
            }
            KeyCode::PageDown => {
                self.scroll_page_down();
            }
            KeyCode::Home if ctrl => {
                self.scroll_to_top();
            }
            KeyCode::End if ctrl => {
                self.scroll_to_bottom();
            }
            KeyCode::Up if !ctrl && !shift && !alt => {
                if self.should_navigate_history_up() {
                    self.navigate_history_up();
                } else {
                    self.state.textarea.input(key);
                }
            }
            KeyCode::Down if !ctrl && !shift && !alt => {
                if self.should_navigate_history_down() {
                    self.navigate_history_down();
                } else {
                    self.state.textarea.input(key);
                }
            }
            _ => {
                self.state.reset_history_navigation();
                self.state.textarea.input(key);
            }
        }

        Ok(())
    }

    // ========================================================================
    // Scroll Methods
    // ========================================================================

    fn transcript_height(&self) -> usize {
        self.terminal
            .size()
            .map(|s| s.height.saturating_sub(HEADER_HEIGHT + INPUT_HEIGHT) as usize)
            .unwrap_or(20)
    }

    fn scroll_page_up(&mut self) {
        self.scroll_lines_up(self.transcript_height().max(1));
    }

    fn scroll_page_down(&mut self) {
        self.scroll_lines_down(self.transcript_height().max(1));
    }

    fn scroll_to_top(&mut self) {
        self.state.scroll_mode = ScrollMode::Anchored { offset: 0 };
    }

    fn scroll_to_bottom(&mut self) {
        self.state.scroll_mode = ScrollMode::FollowLatest;
    }

    fn scroll_lines_up(&mut self, lines: usize) {
        let page_size = self.transcript_height().max(1);
        let current_offset = match &self.state.scroll_mode {
            ScrollMode::FollowLatest => self.state.cached_line_count.saturating_sub(page_size),
            ScrollMode::Anchored { offset } => *offset,
        };

        let new_offset = current_offset.saturating_sub(lines);
        self.state.scroll_mode = ScrollMode::Anchored { offset: new_offset };
    }

    fn scroll_lines_down(&mut self, lines: usize) {
        let page_size = self.transcript_height().max(1);
        let current_offset = match &self.state.scroll_mode {
            ScrollMode::FollowLatest => return,
            ScrollMode::Anchored { offset } => *offset,
        };

        let max_offset = self.state.cached_line_count.saturating_sub(page_size);
        let new_offset = (current_offset + lines).min(max_offset);

        if new_offset >= max_offset {
            self.state.scroll_mode = ScrollMode::FollowLatest;
        } else {
            self.state.scroll_mode = ScrollMode::Anchored { offset: new_offset };
        }
    }

    // ========================================================================
    // History Navigation
    // ========================================================================

    fn should_navigate_history_up(&self) -> bool {
        if self.state.command_history.is_empty() {
            return false;
        }
        if self.state.history_index.is_some() {
            return true;
        }
        if self.state.get_input_text().is_empty() {
            return true;
        }
        let (row, _col) = self.state.textarea.cursor();
        row == 0
    }

    fn should_navigate_history_down(&self) -> bool {
        if self.state.history_index.is_none() {
            return false;
        }
        let (row, _col) = self.state.textarea.cursor();
        let line_count = self.state.textarea.lines().len();
        row >= line_count.saturating_sub(1)
    }

    fn navigate_history_up(&mut self) {
        if self.state.command_history.is_empty() {
            return;
        }

        if self.state.history_index.is_none() {
            let current = self.state.get_input_text();
            self.state.input_draft = Some(current);
            self.state.history_index = Some(self.state.command_history.len() - 1);
        } else if let Some(idx) = self.state.history_index
            && idx > 0
        {
            self.state.history_index = Some(idx - 1);
        }

        if let Some(idx) = self.state.history_index
            && let Some(entry) = self.state.command_history.get(idx).cloned()
        {
            self.state.set_input_text(&entry);
        }
    }

    fn navigate_history_down(&mut self) {
        let Some(idx) = self.state.history_index else {
            return;
        };

        if idx + 1 < self.state.command_history.len() {
            self.state.history_index = Some(idx + 1);
            if let Some(entry) = self.state.command_history.get(idx + 1).cloned() {
                self.state.set_input_text(&entry);
            }
        } else {
            let draft = self.state.input_draft.take().unwrap_or_default();
            self.state.history_index = None;
            self.state.set_input_text(&draft);
        }
    }

    // ========================================================================
    // Engine / Submit
    // ========================================================================

    fn interrupt_engine(&mut self) {
        if self.state.engine_state.is_running() {
            interrupt::trigger_ctrl_c();
        }
    }

    fn submit_input(&mut self) {
        if !matches!(self.state.engine_state, EngineState::Idle) {
            return;
        }

        let text = self.state.get_input_text();
        if text.trim().is_empty() {
            return;
        }

        self.state.command_history.push(text.clone());
        self.state.reset_history_navigation();

        self.state.transcript.push(HistoryCell::user(&text));
        self.state.messages.push(ChatMessage::user(&text));

        if let Some(ref mut s) = self.state.session
            && let Err(e) = s.append(&SessionEvent::user_message(&text))
        {
            self.state.transcript.push(HistoryCell::system(format!(
                "Warning: Failed to save session: {}",
                e
            )));
        }

        self.state.clear_input();
        self.spawn_engine_turn();
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

        let handle = tokio::spawn(async move {
            crate::core::engine::run_turn(
                messages,
                &config,
                &engine_opts,
                system_prompt.as_deref(),
                engine_tx,
            )
            .await
        });

        self.state.engine_state = EngineState::Waiting { handle, rx: tui_rx };
    }

    // ========================================================================
    // Command Palette
    // ========================================================================

    fn open_command_palette(&mut self, insert_slash_on_escape: bool) {
        if self.state.command_palette.is_none() {
            self.state.command_palette = Some(CommandPaletteState::new(insert_slash_on_escape));
        }
    }

    fn close_command_palette(&mut self, insert_slash: bool) {
        self.state.command_palette = None;
        if insert_slash {
            self.state.textarea.insert_char('/');
        }
    }

    fn handle_palette_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => {
                let insert_slash = self
                    .state
                    .command_palette
                    .as_ref()
                    .is_some_and(|p| p.insert_slash_on_escape);
                self.close_command_palette(insert_slash);
            }
            KeyCode::Char('c') if ctrl => {
                self.close_command_palette(false);
            }
            KeyCode::Up => {
                self.palette_select_prev();
            }
            KeyCode::Down => {
                self.palette_select_next();
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.execute_selected_command();
            }
            KeyCode::Backspace => {
                if let Some(palette) = &mut self.state.command_palette {
                    palette.filter.pop();
                    palette.clamp_selection();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(palette) = &mut self.state.command_palette {
                    palette.filter.push(c);
                    palette.clamp_selection();
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn palette_select_prev(&mut self) {
        if let Some(palette) = &mut self.state.command_palette {
            let count = palette.filtered_commands().len();
            if count > 0 && palette.selected > 0 {
                palette.selected -= 1;
            }
        }
    }

    fn palette_select_next(&mut self) {
        if let Some(palette) = &mut self.state.command_palette {
            let count = palette.filtered_commands().len();
            if count > 0 && palette.selected < count - 1 {
                palette.selected += 1;
            }
        }
    }

    fn execute_selected_command(&mut self) {
        let Some(palette) = &self.state.command_palette else {
            return;
        };

        let filtered = palette.filtered_commands();
        let Some(cmd) = filtered.get(palette.selected) else {
            self.close_command_palette(false);
            return;
        };

        match cmd.name {
            "login" => {
                self.close_command_palette(false);
                self.update_login(LoginEvent::LoginRequested);
            }
            "logout" => {
                self.close_command_palette(false);
                self.execute_logout();
            }
            "model" => {
                self.close_command_palette(false);
                self.open_model_picker();
            }
            "new" => {
                self.close_command_palette(false);
                self.execute_new();
            }
            "quit" => {
                self.close_command_palette(false);
                self.execute_quit();
            }
            _ => {
                self.close_command_palette(false);
            }
        }
    }

    // ========================================================================
    // Model Picker
    // ========================================================================

    fn open_model_picker(&mut self) {
        if self.state.model_picker.is_none() {
            self.state.model_picker = Some(ModelPickerState::new(&self.state.config.model));
        }
    }

    fn close_model_picker(&mut self) {
        self.state.model_picker = None;
    }

    fn handle_model_picker_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => {
                self.close_model_picker();
            }
            KeyCode::Char('c') if ctrl => {
                self.close_model_picker();
            }
            KeyCode::Up => {
                self.model_picker_select_prev();
            }
            KeyCode::Down => {
                self.model_picker_select_next();
            }
            KeyCode::Enter => {
                self.execute_model_selection();
            }
            _ => {}
        }

        Ok(())
    }

    fn model_picker_select_prev(&mut self) {
        if let Some(picker) = &mut self.state.model_picker
            && picker.selected > 0
        {
            picker.selected -= 1;
        }
    }

    fn model_picker_select_next(&mut self) {
        if let Some(picker) = &mut self.state.model_picker
            && picker.selected < AVAILABLE_MODELS.len() - 1
        {
            picker.selected += 1;
        }
    }

    fn execute_model_selection(&mut self) {
        let Some(picker) = &self.state.model_picker else {
            return;
        };

        let Some(model) = AVAILABLE_MODELS.get(picker.selected) else {
            self.close_model_picker();
            return;
        };

        let model_id = model.id.to_string();
        let display_name = model.display_name;

        self.state.config.model = model_id.clone();
        self.close_model_picker();

        if let Err(e) = crate::config::Config::save_model(&model_id) {
            self.state.transcript.push(HistoryCell::system(format!(
                "Warning: Failed to save model preference: {}",
                e
            )));
        }

        self.state
            .transcript
            .push(HistoryCell::system(format!("Switched to {}", display_name)));
    }

    // ========================================================================
    // Slash Commands
    // ========================================================================

    fn execute_new(&mut self) {
        if self.state.engine_state.is_running() {
            self.state
                .transcript
                .push(HistoryCell::system("Cannot clear while streaming."));
            return;
        }

        self.state.transcript.clear();
        self.state.messages.clear();
        self.state.command_history.clear();
        self.state.scroll_mode = ScrollMode::FollowLatest;

        if self.state.session.is_some() {
            match session::Session::new() {
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
            }
        } else {
            self.state
                .transcript
                .push(HistoryCell::system("Conversation cleared."));
        }
    }

    fn execute_logout(&mut self) {
        use crate::providers::oauth::anthropic;

        match anthropic::clear_credentials() {
            Ok(true) => {
                self.state.refresh_auth_type();
                self.state
                    .transcript
                    .push(HistoryCell::system("Logged out from Anthropic OAuth."));
            }
            Ok(false) => {
                self.state
                    .transcript
                    .push(HistoryCell::system("No OAuth credentials to clear."));
            }
            Err(e) => {
                self.state
                    .transcript
                    .push(HistoryCell::system(format!("Logout failed: {}", e)));
            }
        }
    }

    fn execute_quit(&mut self) {
        if self.state.engine_state.is_running() {
            self.interrupt_engine();
        }
        self.state.should_quit = true;
    }

    // ========================================================================
    // Login Flow
    // ========================================================================

    fn update_login(&mut self, event: LoginEvent) {
        use crate::providers::oauth::anthropic;

        match event {
            LoginEvent::LoginRequested => {
                let pkce = anthropic::generate_pkce();
                let url = anthropic::build_auth_url(&pkce);
                let _ = open::that(&url);
                self.state.login_state = LoginState::AwaitingCode {
                    url,
                    pkce_verifier: pkce.verifier,
                    input: String::new(),
                    error: None,
                };
            }
            LoginEvent::AuthCodeEntered { code } => {
                if let LoginState::AwaitingCode { pkce_verifier, .. } = &self.state.login_state {
                    self.state.login_state = LoginState::Exchanging {
                        code,
                        pkce_verifier: pkce_verifier.clone(),
                    };
                }
            }
            LoginEvent::LoginSucceeded => {
                self.state.login_state = LoginState::Idle;
                self.state.refresh_auth_type();
                self.state
                    .transcript
                    .push(HistoryCell::system("Logged in with Anthropic OAuth."));
            }
            LoginEvent::LoginFailed { message } => {
                let pkce = anthropic::generate_pkce();
                let url = anthropic::build_auth_url(&pkce);
                self.state.login_state = LoginState::AwaitingCode {
                    url,
                    pkce_verifier: pkce.verifier,
                    input: String::new(),
                    error: Some(message),
                };
            }
            LoginEvent::LoginCancelled => {
                self.state.login_state = LoginState::Idle;
                self.state.login_exchange_rx = None;
            }
        }
    }

    fn handle_login_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match &mut self.state.login_state {
            LoginState::Idle => {}
            LoginState::AwaitingCode { input, .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    self.update_login(LoginEvent::LoginCancelled);
                }
                KeyCode::Enter => {
                    let code = input.trim().to_string();
                    if !code.is_empty() {
                        self.update_login(LoginEvent::AuthCodeEntered { code });
                        self.spawn_token_exchange();
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !ctrl => {
                    input.push(c);
                }
                _ => {}
            },
            LoginState::Exchanging { .. } => {
                if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                    self.update_login(LoginEvent::LoginCancelled);
                }
            }
        }
        Ok(())
    }

    fn spawn_token_exchange(&mut self) {
        use crate::providers::oauth::anthropic;

        let (code, pkce_verifier) = match &self.state.login_state {
            LoginState::Exchanging {
                code,
                pkce_verifier,
            } => (code.clone(), pkce_verifier.clone()),
            _ => return,
        };

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

    fn poll_login_result(&mut self) {
        let Some(rx) = &mut self.state.login_exchange_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(())) => {
                self.state.login_exchange_rx = None;
                self.update_login(LoginEvent::LoginSucceeded);
            }
            Ok(Err(msg)) => {
                self.state.login_exchange_rx = None;
                self.update_login(LoginEvent::LoginFailed { message: msg });
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.state.login_exchange_rx = None;
                self.update_login(LoginEvent::LoginFailed {
                    message: "Exchange task failed".to_string(),
                });
            }
        }
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
}
