//! Full-screen alternate-screen TUI (TUI2).
//!
//! This module provides a full-screen terminal UI using ratatui.
//! Unlike the inline-viewport TUI in `app.rs`, this uses the alternate
//! screen buffer for a persistent, scrollable interface.
//!
//! See docs/plans/plan_ratatui_full_screen_tui2.md for the implementation plan.

use std::io::{self, Stdout};
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
use crate::core::transcript::{CellId, HistoryCell, Style as TranscriptStyle, StyledLine};
use crate::engine::{self, EngineOptions};
use crate::providers::anthropic::ChatMessage;

/// Height of the input area (lines).
const INPUT_HEIGHT: u16 = 5;

/// Height of header area (lines).
const HEADER_HEIGHT: u16 = 2;

/// Target frame rate for streaming updates (30fps = ~33ms per frame).
const FRAME_DURATION: std::time::Duration = std::time::Duration::from_millis(33);

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

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
pub struct Tui2App {
    /// Terminal instance.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Flag indicating the app should quit.
    should_quit: bool,
    /// Text area for input.
    textarea: TextArea<'static>,
    /// Transcript cells (in-memory, no persistence yet).
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
}

impl Tui2App {
    /// Creates a new TUI2 application.
    ///
    /// This enters the alternate screen and enables raw mode.
    /// Terminal state will be restored when the app is dropped.
    pub fn new(config: Config, root: PathBuf, system_prompt: Option<String>) -> Result<Self> {
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

        Ok(Self {
            terminal,
            should_quit: false,
            textarea,
            transcript: Vec::new(),
            config,
            engine_opts,
            system_prompt,
            messages: Vec::new(),
            engine_state: EngineState::Idle,
            scroll_mode: ScrollMode::FollowLatest,
            cached_line_count: 0,
        })
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
                // Insert BEFORE the streaming assistant cell (if any) so tool appears above its output
                let tool_cell = HistoryCell::tool_running(id, name, input.clone());
                if let Some(pos) = self.transcript.iter().position(|c| {
                    matches!(
                        c,
                        HistoryCell::Assistant {
                            is_streaming: true,
                            ..
                        }
                    )
                }) {
                    self.transcript.insert(pos, tool_cell);
                } else {
                    self.transcript.push(tool_cell);
                }
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
            Some(Ok(Ok((_final_text, new_messages)))) => {
                // Success - update messages
                // Note: streaming cell was already finalized via AssistantFinal event
                // If we never got a streaming cell (empty response), don't add anything
                self.messages = new_messages;
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

            // Header with scroll indicator
            let header_text = if has_content_below {
                vec![
                    Span::styled(
                        "ZDX",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" — "),
                    Span::styled("q", Style::default().fg(Color::Yellow)),
                    Span::raw(" to quit"),
                    Span::raw("  "),
                    Span::styled("▼ more", Style::default().fg(Color::DarkGray)),
                ]
            } else {
                vec![
                    Span::styled(
                        "ZDX",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" — "),
                    Span::styled("q", Style::default().fg(Color::Yellow)),
                    Span::raw(" to quit"),
                ]
            };

            let header = Paragraph::new(Line::from(header_text))
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
            let styled_lines = cell.display_lines(width);
            for styled_line in styled_lines {
                lines.push(self.convert_styled_line(styled_line));
            }
            // Add blank line between cells
            lines.push(Line::default());
        }

        // Show "thinking..." indicator when engine is waiting (before first delta)
        if matches!(self.engine_state, EngineState::Waiting { .. }) {
            lines.push(Line::from(vec![
                Span::styled(
                    "Assistant: ",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "thinking...",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
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
            TranscriptStyle::User => Style::default().fg(Color::White),
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
            TranscriptStyle::ToolStatus => Style::default().fg(Color::White),
            TranscriptStyle::ToolError => Style::default().fg(Color::Red),
            TranscriptStyle::ToolRunning => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::ToolSuccess => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::ToolCancelled => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::CROSSED_OUT),
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
            // Pass everything else to textarea
            _ => {
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

        // Add user cell to transcript
        self.transcript.push(HistoryCell::user(&text));

        // Add user message to engine history
        self.messages.push(ChatMessage::user(&text));

        // Clear input
        self.clear_input();

        // Spawn engine task
        self.spawn_engine_turn();
    }

    /// Spawns an engine turn in the background.
    fn spawn_engine_turn(&mut self) {
        let (tx, rx) = engine::create_event_channel();

        // Clone what we need for the async task
        let messages = self.messages.clone();
        let config = self.config.clone();
        let engine_opts = self.engine_opts.clone();
        let system_prompt = self.system_prompt.clone();

        let handle = tokio::spawn(async move {
            engine::run_turn(
                messages,
                &config,
                &engine_opts,
                system_prompt.as_deref(),
                tx,
            )
            .await
        });

        self.engine_state = EngineState::Waiting { handle, rx };
    }
}

impl Drop for Tui2App {
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
