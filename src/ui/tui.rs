//! Full-screen alternate-screen TUI.
//!
//! This module provides a full-screen terminal UI using ratatui.
//! Uses the alternate screen buffer for a persistent, scrollable interface.
//!
//! See docs/plans/plan_ratatui_full_screen_tui.md for the implementation plan.

use std::io::{self, IsTerminal, Stdout, Write, stderr};
use std::panic;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tui_textarea::TextArea;

use crate::config::Config;
use crate::core::events::EngineEvent;
use crate::core::interrupt;
use crate::core::engine::EngineOptions;
use crate::core::session::{self, Session, SessionEvent};
use crate::providers::anthropic::ChatMessage;
use crate::ui::transcript::{CellId, HistoryCell, Style as TranscriptStyle, StyledLine};

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
    // (alternate screen will hide all this output)
    err.flush()?;

    // Create and run the TUI
    let mut app = if history.is_empty() {
        TuiApp::new(config.clone(), root, effective.prompt, session)?
    } else {
        TuiApp::with_history(config.clone(), root, effective.prompt, session, history)?
    };
    app.run()?;

    // Print goodbye after TUI exits (terminal restored)
    writeln!(stderr(), "Goodbye!")?;

    Ok(())
}

/// Height of the input area (lines).
const INPUT_HEIGHT: u16 = 5;

/// Height of header area (lines: title + status + border).
const HEADER_HEIGHT: u16 = 3;

/// Target frame rate for streaming updates (30fps = ~33ms per frame).
const FRAME_DURATION: std::time::Duration = std::time::Duration::from_millis(33);

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

/// Spinner speed divisor (render frames per spinner frame).
/// At 30fps render rate, 3 gives ~10fps spinner animation.
const SPINNER_SPEED_DIVISOR: usize = 3;

/// Scroll mode for the transcript pane.
#[derive(Debug, Clone)]
enum ScrollMode {
    /// Auto-scroll to show latest content (bottom of transcript).
    FollowLatest,
    /// User scrolled manually; offset is line index from top.
    Anchored { offset: usize },
}

/// Engine execution state.
#[derive(Debug)]
enum EngineState {
    /// No engine task running, ready for input.
    Idle,
    /// Streaming response in progress.
    Streaming {
        /// Handle to the spawned engine task.
        handle: JoinHandle<Result<(String, Vec<ChatMessage>)>>,
        /// Receiver for engine events.
        rx: mpsc::Receiver<Arc<EngineEvent>>,
        /// ID of the streaming assistant cell in transcript.
        cell_id: CellId,
        /// Buffered delta text to apply on next tick (coalescing).
        pending_delta: String,
    },
    /// Waiting for first response (shows "thinking...").
    Waiting {
        /// Handle to the spawned engine task.
        handle: JoinHandle<Result<(String, Vec<ChatMessage>)>>,
        /// Receiver for engine events.
        rx: mpsc::Receiver<Arc<EngineEvent>>,
    },
}

/// Full-screen TUI application.
///
/// Uses the alternate screen buffer for a persistent interface.
/// Terminal state is guaranteed to be restored on drop, panic, or Ctrl+C.
pub struct TuiApp {
    /// Terminal instance.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Flag indicating the app should quit.
    should_quit: bool,
    /// Text area for input.
    textarea: TextArea<'static>,
    /// Transcript cells (in-memory display).
    transcript: Vec<HistoryCell>,
    /// Engine configuration.
    config: Config,
    /// Engine options (root path, etc).
    engine_opts: EngineOptions,
    /// System prompt for the engine.
    system_prompt: Option<String>,
    /// Message history for the engine.
    messages: Vec<ChatMessage>,
    /// Current engine state.
    engine_state: EngineState,
    /// Scroll mode for transcript.
    scroll_mode: ScrollMode,
    /// Cached total line count from last render (for scroll calculations).
    cached_line_count: usize,
    /// Session for persistence (if enabled).
    session: Option<Session>,
    /// Command history for ↑/↓ navigation.
    command_history: Vec<String>,
    /// Current position in command history (None = not navigating).
    history_index: Option<usize>,
    /// Draft text saved when navigating history.
    input_draft: Option<String>,
    /// Spinner animation frame counter (for running tools).
    spinner_frame: usize,
}

impl TuiApp {
    /// Creates a new TUI2 application.
    ///
    /// This enters the alternate screen and enables raw mode.
    /// Terminal state will be restored when the app is dropped.
    pub fn new(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
    ) -> Result<Self> {
        Self::with_history(config, root, system_prompt, session, Vec::new())
    }

    /// Creates a TUI2 application with pre-loaded message history.
    ///
    /// Used for resuming previous sessions.
    pub fn with_history(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
        history: Vec<ChatMessage>,
    ) -> Result<Self> {
        // Set up panic hook BEFORE entering alternate screen
        install_panic_hook();

        // Reset interrupt flag in case it was set from a previous run
        interrupt::reset();

        // Enter alternate screen and raw mode
        let terminal = setup_terminal().context("Failed to setup terminal")?;

        // Set up textarea with styling
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Input (Enter=send, Shift+Enter=newline, Ctrl+J=newline) "),
        );

        let engine_opts = EngineOptions { root };

        // Build transcript from history
        let transcript = Self::build_transcript_from_history(&history);

        // Build command history from previous user messages
        let command_history: Vec<String> = transcript
            .iter()
            .filter_map(|cell| {
                if let HistoryCell::User { content, .. } = cell {
                    Some(content.clone())
                } else {
                    None
                }
            })
            .collect();

        Ok(Self {
            terminal,
            should_quit: false,
            textarea,
            transcript,
            config,
            engine_opts,
            system_prompt,
            messages: history,
            engine_state: EngineState::Idle,
            scroll_mode: ScrollMode::FollowLatest,
            cached_line_count: 0,
            session,
            command_history,
            history_index: None,
            input_draft: None,
            spinner_frame: 0,
        })
    }

    /// Builds transcript cells from message history.
    fn build_transcript_from_history(messages: &[ChatMessage]) -> Vec<HistoryCell> {
        use crate::providers::anthropic::MessageContent;

        let mut transcript = Vec::new();

        for msg in messages {
            let text = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => {
                    // Extract text blocks, ignore tool use/result for display
                    blocks
                        .iter()
                        .filter_map(|b| {
                            if let crate::providers::anthropic::ChatContentBlock::Text(t) = b {
                                Some(t.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            };

            if text.is_empty() {
                continue;
            }

            let cell = match msg.role.as_str() {
                "user" => HistoryCell::user(&text),
                "assistant" => HistoryCell::assistant(&text),
                _ => continue,
            };
            transcript.push(cell);
        }

        transcript
    }

    /// Runs the main event loop.
    ///
    /// This blocks until the user quits (q or Ctrl+C).
    pub fn run(&mut self) -> Result<()> {
        // Enable bracketed paste and mouse capture
        execute!(
            io::stdout(),
            event::EnableBracketedPaste,
            EnableMouseCapture
        )?;

        let result = self.event_loop();

        // Disable mouse capture and bracketed paste
        execute!(
            io::stdout(),
            DisableMouseCapture,
            event::DisableBracketedPaste
        )?;

        result
    }

    fn event_loop(&mut self) -> Result<()> {
        while !self.should_quit {
            // Check for Ctrl+C signal (uses global interrupt flag)
            if interrupt::is_interrupted() {
                self.should_quit = true;
                break;
            }

            // Poll engine events (streaming deltas, completion, etc.)
            self.poll_engine_events();

            // Apply any pending deltas before render (coalescing)
            self.apply_pending_delta();

            // Check for engine task completion
            self.poll_engine_completion();

            // Advance spinner animation frame
            self.spinner_frame = self.spinner_frame.wrapping_add(1);

            // Render
            self.render()?;

            // Handle terminal events with short timeout for responsive streaming
            if event::poll(FRAME_DURATION)? {
                self.handle_event(event::read()?)?;
            }
        }

        Ok(())
    }

    /// Polls the engine event channel for streaming events (non-blocking).
    fn poll_engine_events(&mut self) {
        // Drain all available events from the channel
        while let EngineState::Waiting { rx, .. } | EngineState::Streaming { rx, .. } =
            &mut self.engine_state
        {
            let event = match rx.try_recv() {
                Ok(ev) => ev,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            };

            // Must clone event before calling handle_engine_event to avoid borrow conflict
            let event = event.clone();
            self.handle_engine_event(&event);
        }
    }

    /// Handles a single engine event, updating state as needed.
    fn handle_engine_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::AssistantDelta { text } => {
                // Transition from Waiting to Streaming on first delta
                match &mut self.engine_state {
                    EngineState::Waiting { .. } => {
                        // Create streaming cell and transition to Streaming state
                        let cell = HistoryCell::assistant_streaming("");
                        let cell_id = cell.id();
                        self.transcript.push(cell);

                        // Take ownership and transition state
                        let old_state =
                            std::mem::replace(&mut self.engine_state, EngineState::Idle);
                        if let EngineState::Waiting { handle, rx } = old_state {
                            self.engine_state = EngineState::Streaming {
                                handle,
                                rx,
                                cell_id,
                                pending_delta: text.clone(),
                            };
                        }
                    }
                    EngineState::Streaming { pending_delta, .. } => {
                        // Buffer the delta for coalescing
                        pending_delta.push_str(text);
                    }
                    EngineState::Idle => {}
                }
            }
            EngineEvent::AssistantFinal { .. } => {
                // Finalize the streaming cell
                if let EngineState::Streaming { cell_id, .. } = &self.engine_state {
                    // Find and finalize the streaming cell
                    if let Some(cell) = self.transcript.iter_mut().find(|c| c.id() == *cell_id) {
                        cell.finalize_assistant();
                    }
                }
                // Note: completion handled by poll_engine_completion
            }
            EngineEvent::Error { message, .. } => {
                // Show error in transcript
                self.transcript
                    .push(HistoryCell::system(format!("Error: {}", message)));
            }
            EngineEvent::Interrupted => {
                self.transcript.push(HistoryCell::system("[Interrupted]"));
                interrupt::reset();
            }
            EngineEvent::ToolRequested { id, name, input } => {
                // Create a tool cell in running state
                // Tools are appended after assistant text since Claude streams text before tool_use blocks
                let tool_cell = HistoryCell::tool_running(id, name, input.clone());
                self.transcript.push(tool_cell);
            }
            EngineEvent::ToolStarted { .. } => {
                // Already showing running state from ToolRequested
            }
            EngineEvent::ToolFinished { id, result } => {
                // Find the tool cell and update its state
                if let Some(cell) = self.transcript.iter_mut().find(
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
        } = &mut self.engine_state
            && !pending_delta.is_empty()
        {
            // Find the streaming cell and append the delta
            if let Some(cell) = self.transcript.iter_mut().find(|c| c.id() == *cell_id) {
                cell.append_assistant_delta(pending_delta);
            }
            pending_delta.clear();
        }
    }

    /// Polls the engine task for completion (non-blocking).
    fn poll_engine_completion(&mut self) {
        // Check if we have a finished engine task
        let is_finished = match &self.engine_state {
            EngineState::Waiting { handle, .. } | EngineState::Streaming { handle, .. } => {
                handle.is_finished()
            }
            EngineState::Idle => false,
        };

        if !is_finished {
            return;
        }

        // Take ownership of the state to handle completion
        let old_state = std::mem::replace(&mut self.engine_state, EngineState::Idle);

        let (handle, had_streaming_cell) = match old_state {
            EngineState::Waiting { handle, .. } => (handle, false),
            EngineState::Streaming { handle, .. } => (handle, true),
            EngineState::Idle => return,
        };

        // Get the result
        match futures_util::FutureExt::now_or_never(handle) {
            Some(Ok(Ok((final_text, new_messages)))) => {
                // Success - update messages
                // Note: streaming cell was already finalized via AssistantFinal event
                // If we never got a streaming cell (empty response), don't add anything
                self.messages = new_messages;

                // Log assistant response to session
                if !final_text.is_empty()
                    && let Some(ref mut s) = self.session
                    && let Err(e) = s.append(&SessionEvent::assistant_message(&final_text))
                {
                    self.transcript.push(HistoryCell::system(format!(
                        "Warning: Failed to save session: {}",
                        e
                    )));
                }
            }
            Some(Ok(Err(e))) => {
                // Engine error
                if e.downcast_ref::<crate::core::interrupt::InterruptedError>()
                    .is_some()
                {
                    // Already handled by Interrupted event
                } else if !had_streaming_cell {
                    // Only show error if we haven't already via event
                    self.transcript
                        .push(HistoryCell::system(format!("Error: {}", e)));
                }
                // Remove the failed user message from history
                self.messages.pop();
            }
            Some(Err(e)) => {
                // Join error (panic in task)
                self.transcript
                    .push(HistoryCell::system(format!("Internal error: {}", e)));
                self.messages.pop();
            }
            None => {
                // Shouldn't happen since we checked is_finished()
            }
        }
    }

    /// Renders the UI.
    fn render(&mut self) -> Result<()> {
        // Get terminal size for transcript rendering
        let size = self.terminal.size()?;
        let transcript_width = size.width.saturating_sub(2) as usize;

        // Calculate transcript pane height
        let transcript_height = size.height.saturating_sub(HEADER_HEIGHT + INPUT_HEIGHT) as usize;

        // Pre-render transcript lines (avoids borrow issues in closure)
        let all_lines = self.render_transcript(transcript_width);
        let total_lines = all_lines.len();
        self.cached_line_count = total_lines;

        // Calculate scroll offset based on mode
        let scroll_offset = match &self.scroll_mode {
            ScrollMode::FollowLatest => {
                // Show bottom of transcript
                total_lines.saturating_sub(transcript_height)
            }
            ScrollMode::Anchored { offset } => {
                // Clamp to valid range
                let max_offset = total_lines.saturating_sub(transcript_height);
                (*offset).min(max_offset)
            }
        };

        // Check if there's content below the viewport (for indicator)
        let has_content_below = scroll_offset + transcript_height < total_lines;

        // Slice visible lines
        let visible_end = (scroll_offset + transcript_height).min(total_lines);
        let visible_lines: Vec<Line<'static>> = all_lines
            .into_iter()
            .skip(scroll_offset)
            .take(visible_end - scroll_offset)
            .collect();

        // Clone textarea for rendering (tui-textarea doesn't impl Copy)
        let textarea = &self.textarea;

        // Prepare status line info (before closure to avoid borrow issues)
        let status_state = match &self.engine_state {
            EngineState::Idle => ("Ready", Color::Green),
            EngineState::Waiting { .. } => ("Thinking...", Color::Yellow),
            EngineState::Streaming { .. } => ("Streaming...", Color::Yellow),
        };
        let model_name = self.config.model.clone();
        let history_indicator = if self.history_index.is_some() {
            let idx = self.history_index.unwrap();
            let total = self.command_history.len();
            Some(format!("history {}/{}", idx + 1, total))
        } else {
            None
        };

        self.terminal.draw(|frame| {
            let area = frame.area();

            // Create layout: header, transcript, input
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(HEADER_HEIGHT), // Header
                    Constraint::Min(1),                // Transcript
                    Constraint::Length(INPUT_HEIGHT),  // Input
                ])
                .split(area);

            // Header line 1: Title and scroll indicator
            let mut title_spans = vec![
                Span::styled(
                    "ZDX",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" — "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" to quit"),
            ];
            if has_content_below {
                title_spans.push(Span::raw("  "));
                title_spans.push(Span::styled("▼ more", Style::default().fg(Color::DarkGray)));
            }

            // Header line 2: Status line (model, state, history indicator)
            let mut status_spans = vec![
                Span::styled(&model_name, Style::default().fg(Color::DarkGray)),
                Span::raw(" │ "),
                Span::styled(status_state.0, Style::default().fg(status_state.1)),
            ];
            if let Some(hist) = &history_indicator {
                status_spans.push(Span::raw(" │ "));
                status_spans.push(Span::styled(
                    hist.as_str(),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            let header = Paragraph::new(vec![Line::from(title_spans), Line::from(status_spans)])
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
            frame.render_widget(header, chunks[0]);

            // Transcript area (already sliced to visible)
            let transcript = Paragraph::new(visible_lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::NONE));
            frame.render_widget(transcript, chunks[1]);

            // Input area
            frame.render_widget(textarea, chunks[2]);
        })?;

        Ok(())
    }

    /// Renders the transcript into ratatui Lines.
    fn render_transcript(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for cell in &self.transcript {
            let styled_lines =
                cell.display_lines(width, self.spinner_frame / SPINNER_SPEED_DIVISOR);
            for styled_line in styled_lines {
                lines.push(self.convert_styled_line(styled_line));
            }
            // Add blank line between cells
            lines.push(Line::default());
        }

        // Remove trailing blank line if not waiting or streaming
        let is_active = matches!(
            self.engine_state,
            EngineState::Waiting { .. } | EngineState::Streaming { .. }
        );
        if !is_active && lines.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
            lines.pop();
        }

        lines
    }

    /// Converts a transcript StyledLine to a ratatui Line.
    fn convert_styled_line(&self, styled_line: StyledLine) -> Line<'static> {
        let spans: Vec<Span<'static>> = styled_line
            .spans
            .into_iter()
            .map(|s| {
                let style = self.convert_style(s.style);
                Span::styled(s.text, style)
            })
            .collect();
        Line::from(spans)
    }

    /// Converts a transcript Style to a ratatui Style.
    fn convert_style(&self, style: TranscriptStyle) -> Style {
        match style {
            TranscriptStyle::Plain => Style::default(),
            TranscriptStyle::UserPrefix => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::User => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::ITALIC),
            TranscriptStyle::AssistantPrefix => Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::Assistant => Style::default().fg(Color::White),
            TranscriptStyle::StreamingCursor => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::SLOW_BLINK),
            TranscriptStyle::SystemPrefix => Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::System => Style::default().fg(Color::DarkGray),
            TranscriptStyle::ToolBracket => Style::default().fg(Color::DarkGray),
            TranscriptStyle::ToolStatus => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::ToolError => Style::default().fg(Color::Red),
            TranscriptStyle::ToolRunning => Style::default().fg(Color::Cyan),
            TranscriptStyle::ToolSuccess => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::ToolCancelled => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::CROSSED_OUT),
            TranscriptStyle::ToolOutput => Style::default().fg(Color::Gray),
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
                self.textarea.insert_str(&text);
                Ok(())
            }
            Event::Resize(_, _) => {
                // Ratatui handles resize automatically
                Ok(())
            }
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
            // Ignore other mouse events (clicks, drags, etc.)
            _ => {}
        }
    }

    /// Handles a key event.
    fn handle_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // q without modifiers: quit (only when input is empty)
            KeyCode::Char('q') if !ctrl && !shift && !alt => {
                if self.get_input_text().is_empty() {
                    self.should_quit = true;
                } else {
                    self.textarea.input(key);
                }
            }
            // Ctrl+C: progressive behavior (interrupt → clear → quit)
            KeyCode::Char('c') if ctrl => {
                if self.is_engine_running() {
                    self.interrupt_engine();
                } else if !self.get_input_text().is_empty() {
                    self.clear_input();
                } else {
                    self.should_quit = true;
                }
            }
            // Enter: submit (unless Shift+Enter or Alt+Enter for newline)
            KeyCode::Enter if !shift && !alt => {
                self.submit_input();
            }
            // Ctrl+J: insert newline (terminal-reliable alternative)
            KeyCode::Char('j') if ctrl => {
                self.textarea.insert_newline();
            }
            // Escape: interrupt if running, else clear input
            KeyCode::Esc => {
                if self.is_engine_running() {
                    self.interrupt_engine();
                } else {
                    self.clear_input();
                }
            }
            // Scroll: PageUp/PageDown
            KeyCode::PageUp => {
                self.scroll_page_up();
            }
            KeyCode::PageDown => {
                self.scroll_page_down();
            }
            // Scroll: Home (top) / End (bottom, re-enables follow-latest)
            KeyCode::Home if ctrl => {
                self.scroll_to_top();
            }
            KeyCode::End if ctrl => {
                self.scroll_to_bottom();
            }
            // History navigation: Up arrow
            KeyCode::Up if !ctrl && !shift && !alt => {
                if self.should_navigate_history_up() {
                    self.navigate_history_up();
                } else {
                    self.textarea.input(key);
                }
            }
            // History navigation: Down arrow
            KeyCode::Down if !ctrl && !shift && !alt => {
                if self.should_navigate_history_down() {
                    self.navigate_history_down();
                } else {
                    self.textarea.input(key);
                }
            }
            // Pass everything else to textarea
            _ => {
                // Any other key resets history navigation state
                self.reset_history_navigation();
                self.textarea.input(key);
            }
        }

        Ok(())
    }

    /// Returns the visible height of the transcript pane.
    fn transcript_height(&self) -> usize {
        self.terminal
            .size()
            .map(|s| s.height.saturating_sub(HEADER_HEIGHT + INPUT_HEIGHT) as usize)
            .unwrap_or(20)
    }

    /// Scrolls the transcript up by one page.
    fn scroll_page_up(&mut self) {
        self.scroll_lines_up(self.transcript_height().max(1));
    }

    /// Scrolls the transcript down by one page.
    fn scroll_page_down(&mut self) {
        self.scroll_lines_down(self.transcript_height().max(1));
    }

    /// Scrolls to the top of the transcript.
    fn scroll_to_top(&mut self) {
        self.scroll_mode = ScrollMode::Anchored { offset: 0 };
    }

    /// Scrolls to the bottom and re-enables follow-latest.
    fn scroll_to_bottom(&mut self) {
        self.scroll_mode = ScrollMode::FollowLatest;
    }

    /// Scrolls the transcript up by a number of lines.
    fn scroll_lines_up(&mut self, lines: usize) {
        let page_size = self.transcript_height().max(1);
        let current_offset = match &self.scroll_mode {
            ScrollMode::FollowLatest => self.cached_line_count.saturating_sub(page_size),
            ScrollMode::Anchored { offset } => *offset,
        };

        let new_offset = current_offset.saturating_sub(lines);
        self.scroll_mode = ScrollMode::Anchored { offset: new_offset };
    }

    /// Scrolls the transcript down by a number of lines.
    fn scroll_lines_down(&mut self, lines: usize) {
        let page_size = self.transcript_height().max(1);
        let current_offset = match &self.scroll_mode {
            ScrollMode::FollowLatest => {
                // Already at bottom, nothing to do
                return;
            }
            ScrollMode::Anchored { offset } => *offset,
        };

        let max_offset = self.cached_line_count.saturating_sub(page_size);
        let new_offset = (current_offset + lines).min(max_offset);

        // If we've scrolled to the bottom, switch back to FollowLatest
        if new_offset >= max_offset {
            self.scroll_mode = ScrollMode::FollowLatest;
        } else {
            self.scroll_mode = ScrollMode::Anchored { offset: new_offset };
        }
    }

    /// Gets the current input text.
    fn get_input_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Clears the input textarea.
    fn clear_input(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
        self.reset_history_navigation();
    }

    /// Returns true if the engine is currently running (waiting or streaming).
    fn is_engine_running(&self) -> bool {
        !matches!(self.engine_state, EngineState::Idle)
    }

    /// Interrupts the running engine task.
    fn interrupt_engine(&mut self) {
        if self.is_engine_running() {
            interrupt::trigger_ctrl_c();
        }
    }

    /// Returns true if we should navigate to the previous history entry.
    ///
    /// History navigation activates when:
    /// - Input is empty, OR
    /// - Already navigating history, OR
    /// - Cursor is at the first line of the input
    fn should_navigate_history_up(&self) -> bool {
        if self.command_history.is_empty() {
            return false;
        }

        // Already navigating history
        if self.history_index.is_some() {
            return true;
        }

        // Input is empty - start navigation
        if self.get_input_text().is_empty() {
            return true;
        }

        // Cursor at first line - start navigation
        let (row, _col) = self.textarea.cursor();
        row == 0
    }

    /// Returns true if we should navigate to the next history entry.
    ///
    /// Only active when already navigating history and cursor at last line.
    fn should_navigate_history_down(&self) -> bool {
        // Only navigate if we're already in history navigation mode
        if self.history_index.is_none() {
            return false;
        }

        // Check if cursor is at last line
        let (row, _col) = self.textarea.cursor();
        let line_count = self.textarea.lines().len();
        row >= line_count.saturating_sub(1)
    }

    /// Navigates to the previous entry in command history.
    fn navigate_history_up(&mut self) {
        if self.command_history.is_empty() {
            return;
        }

        // Save current input as draft on first navigation
        if self.history_index.is_none() {
            let current = self.get_input_text();
            self.input_draft = Some(current);
            // Start at the most recent entry
            self.history_index = Some(self.command_history.len() - 1);
        } else if let Some(idx) = self.history_index {
            // Navigate backwards (older entries)
            if idx > 0 {
                self.history_index = Some(idx - 1);
            }
        }

        // Load history entry into textarea
        if let Some(idx) = self.history_index
            && let Some(entry) = self.command_history.get(idx).cloned()
        {
            self.set_input_text(&entry);
        }
    }

    /// Navigates to the next entry in command history.
    fn navigate_history_down(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };

        if idx + 1 < self.command_history.len() {
            // Move to more recent entry
            self.history_index = Some(idx + 1);
            if let Some(entry) = self.command_history.get(idx + 1).cloned() {
                self.set_input_text(&entry);
            }
        } else {
            // Past the end - restore draft and exit history mode
            let draft = self.input_draft.take().unwrap_or_default();
            self.history_index = None;
            self.set_input_text(&draft);
        }
    }

    /// Resets history navigation state.
    fn reset_history_navigation(&mut self) {
        self.history_index = None;
        self.input_draft = None;
    }

    /// Sets the input textarea to the given text.
    fn set_input_text(&mut self, text: &str) {
        // Clear current content
        self.textarea.select_all();
        self.textarea.cut();
        // Insert new text
        self.textarea.insert_str(text);
    }

    /// Submits the current input.
    fn submit_input(&mut self) {
        // Don't submit if engine is already running
        if !matches!(self.engine_state, EngineState::Idle) {
            return;
        }

        let text = self.get_input_text();
        if text.trim().is_empty() {
            return;
        }

        // Add to command history for ↑/↓ navigation
        self.command_history.push(text.clone());
        self.reset_history_navigation();

        // Add user cell to transcript
        self.transcript.push(HistoryCell::user(&text));

        // Add user message to engine history
        self.messages.push(ChatMessage::user(&text));

        // Log user message to session
        if let Some(ref mut s) = self.session
            && let Err(e) = s.append(&SessionEvent::user_message(&text))
        {
            // Best-effort: show warning in transcript
            self.transcript.push(HistoryCell::system(format!(
                "Warning: Failed to save session: {}",
                e
            )));
        }

        // Clear input
        self.clear_input();

        // Spawn engine task
        self.spawn_engine_turn();
    }

    /// Spawns an engine turn in the background.
    fn spawn_engine_turn(&mut self) {
        let (engine_tx, engine_rx) = crate::core::engine::create_event_channel();

        // Clone what we need for the async task
        let messages = self.messages.clone();
        let config = self.config.clone();
        let engine_opts = self.engine_opts.clone();
        let system_prompt = self.system_prompt.clone();

        // Set up event receivers: one for TUI updates, optionally one for session persistence
        let (tui_tx, tui_rx) = crate::core::engine::create_event_channel();

        // If session exists, spawn persist task
        if let Some(sess) = self.session.clone() {
            let (persist_tx, persist_rx) = crate::core::engine::create_event_channel();
            let _fanout =
                crate::core::engine::spawn_fanout_task(engine_rx, vec![tui_tx, persist_tx]);
            let _persist = session::spawn_persist_task(sess, persist_rx);
        } else {
            // No session - just fan out to TUI
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

        self.engine_state = EngineState::Waiting { handle, rx: tui_rx };
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        // Restore terminal state
        let _ = restore_terminal();
    }
}

/// Sets up the terminal for the TUI.
///
/// - Enables raw mode
/// - Enters alternate screen
/// - Creates the terminal instance
fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("Failed to create terminal")?;
    Ok(terminal)
}

/// Restores terminal state.
///
/// - Leaves alternate screen
/// - Disables raw mode
fn restore_terminal() -> Result<()> {
    // Leave alternate screen first (while still in raw mode)
    execute!(io::stdout(), LeaveAlternateScreen).context("Failed to leave alternate screen")?;
    disable_raw_mode().context("Failed to disable raw mode")?;
    Ok(())
}

/// Installs a panic hook that restores the terminal before printing the panic.
fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal first
        let _ = restore_terminal();
        // Then call the original panic hook
        original_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    // Note: Terminal tests are difficult to run in CI since they require a real TTY.
    // Integration tests for TUI2 should spawn the CLI and verify stdout/stderr behavior.
    //
    // Key guarantees to test manually or via integration tests:
    // - Terminal is restored on normal exit
    // - Terminal is restored on panic
    // - Terminal is restored on Ctrl+C
    // - Resize events don't break the UI
}
