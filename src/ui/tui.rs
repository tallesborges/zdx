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
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tui_textarea::TextArea;

use crate::config::Config;
use crate::core::engine::EngineOptions;
use crate::core::events::EngineEvent;
use crate::core::interrupt;
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

// ============================================================================
// Login Events (Reducer Pattern)
// ============================================================================

/// Events for the login flow (reducer pattern).
///
/// These events drive the login overlay state machine.
/// All state changes go through `TuiApp::update()`.
#[derive(Debug, Clone)]
enum LoginEvent {
    /// User requested login (e.g., via `/login` command).
    LoginRequested,
    /// User entered the auth code.
    AuthCodeEntered { code: String },
    /// Login succeeded.
    LoginSucceeded,
    /// Login failed with an error message.
    LoginFailed { message: String },
    /// User cancelled the login flow.
    LoginCancelled,
}

/// State for the login overlay.
#[derive(Debug, Clone)]
enum LoginState {
    /// Not in login flow.
    Idle,
    /// Showing auth URL, waiting for user to paste code.
    AwaitingCode {
        /// The auth URL to display.
        url: String,
        /// PKCE verifier for code exchange.
        pkce_verifier: String,
        /// User's input (the auth code).
        input: String,
        /// Error message from previous attempt (if any).
        error: Option<String>,
    },
    /// Exchanging code for tokens (async operation in progress).
    Exchanging {
        /// The auth code being exchanged.
        code: String,
        /// PKCE verifier for exchange.
        pkce_verifier: String,
    },
}

impl LoginState {
    /// Returns true if the login overlay should be displayed.
    fn is_active(&self) -> bool {
        !matches!(self, LoginState::Idle)
    }
}

// ============================================================================
// Slash Commands
// ============================================================================

/// Definition of a slash command.
#[derive(Debug, Clone)]
struct SlashCommand {
    /// Primary name (e.g., "clear") - without the leading slash.
    name: &'static str,
    /// Aliases (e.g., ["new"]) - without leading slashes.
    aliases: &'static [&'static str],
    /// Short description shown in palette.
    description: &'static str,
}

impl SlashCommand {
    /// Returns true if this command matches the given filter (case-insensitive).
    /// Matches against name and all aliases.
    fn matches(&self, filter: &str) -> bool {
        let filter_lower = filter.to_lowercase();
        self.name.to_lowercase().contains(&filter_lower)
            || self
                .aliases
                .iter()
                .any(|a| a.to_lowercase().contains(&filter_lower))
    }

    /// Returns the display name with aliases, e.g., "clear (new)".
    fn display_name(&self) -> String {
        if self.aliases.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} ({})", self.name, self.aliases.join(", "))
        }
    }
}

/// Available slash commands.
const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "login",
        aliases: &[],
        description: "Login with Anthropic OAuth",
    },
    SlashCommand {
        name: "logout",
        aliases: &[],
        description: "Logout from Anthropic OAuth",
    },
    SlashCommand {
        name: "model",
        aliases: &["m"],
        description: "Switch model",
    },
    SlashCommand {
        name: "new",
        aliases: &["clear"],
        description: "Start a new conversation",
    },
    SlashCommand {
        name: "quit",
        aliases: &["q", "exit"],
        description: "Exit ZDX",
    },
];

// ============================================================================
// Model Picker
// ============================================================================

/// Definition of an available model.
#[derive(Debug, Clone)]
struct ModelOption {
    /// Model ID (sent to API)
    id: &'static str,
    /// Display name for the picker
    display_name: &'static str,
}

/// Available models for the picker.
const AVAILABLE_MODELS: &[ModelOption] = &[
    ModelOption {
        id: "claude-sonnet-4-5-20250929",
        display_name: "Claude Sonnet 4.5",
    },
    ModelOption {
        id: "claude-opus-4-5-20251101",
        display_name: "Claude Opus 4.5",
    },
    ModelOption {
        id: "claude-haiku-4-5-20251001",
        display_name: "Claude Haiku 4.5",
    },
];

/// State for the model picker overlay.
#[derive(Debug, Clone)]
struct ModelPickerState {
    /// Currently selected index.
    selected: usize,
}

impl ModelPickerState {
    /// Creates a new picker state, selecting the current model if found.
    fn new(current_model: &str) -> Self {
        let selected = AVAILABLE_MODELS
            .iter()
            .position(|m| m.id == current_model)
            .unwrap_or(0);
        Self { selected }
    }
}

/// State for the slash command palette.
///
/// This is `Option<T>` in TuiApp, so it's trivially droppable.
/// Terminal restore (panic hook, Drop) doesn't need special handling.
#[derive(Debug, Clone)]
struct CommandPaletteState {
    /// Filter text (characters typed after `/`).
    filter: String,
    /// Currently selected command index (into filtered list).
    selected: usize,
    /// Whether to insert "/" on Escape (true if opened via "/", false if via Ctrl+P).
    insert_slash_on_escape: bool,
}

impl CommandPaletteState {
    /// Creates a new palette state with empty filter.
    fn new(insert_slash_on_escape: bool) -> Self {
        Self {
            filter: String::new(),
            selected: 0,
            insert_slash_on_escape,
        }
    }

    /// Returns commands matching the current filter.
    fn filtered_commands(&self) -> Vec<&'static SlashCommand> {
        if self.filter.is_empty() {
            SLASH_COMMANDS.iter().collect()
        } else {
            SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.matches(&self.filter))
                .collect()
        }
    }

    /// Clamps the selected index to valid range for current filter.
    fn clamp_selection(&mut self) {
        let count = self.filtered_commands().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(count - 1);
        }
    }
}

// ============================================================================
// Scroll Mode
// ============================================================================

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
    /// Command palette state (None = closed).
    /// Using Option<T> ensures trivial cleanup on drop/panic.
    command_palette: Option<CommandPaletteState>,
    /// Model picker state (None = closed).
    model_picker: Option<ModelPickerState>,
    /// Login overlay state.
    login_state: LoginState,
    /// Receiver for async login token exchange result.
    login_exchange_rx: Option<mpsc::Receiver<Result<(), String>>>,
    /// Current auth type indicator (cached, refreshed on login/logout).
    auth_type: AuthType,
}

/// Authentication type indicator for status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthType {
    /// Using OAuth token from ~/.zdx/oauth.json
    OAuth,
    /// Using API key from environment
    ApiKey,
    /// No authentication configured
    None,
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
            command_palette: None,
            model_picker: None,
            login_state: LoginState::Idle,
            login_exchange_rx: None,
            auth_type: Self::detect_auth_type(),
        })
    }

    /// Detects the current authentication type.
    fn detect_auth_type() -> AuthType {
        use crate::providers::oauth::anthropic;

        // Check for OAuth credentials first
        if let Ok(Some(_creds)) = anthropic::load_credentials() {
            return AuthType::OAuth;
        }

        // Check for API key in environment
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            return AuthType::ApiKey;
        }

        AuthType::None
    }

    /// Refreshes the cached auth type (call after login/logout).
    fn refresh_auth_type(&mut self) {
        self.auth_type = Self::detect_auth_type();
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

            // Poll for login exchange result
            self.poll_login_result();

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
        let palette_open = self.command_palette.is_some();
        let palette_state = self.command_palette.clone();
        let model_picker_state = self.model_picker.clone();
        let login_state = self.login_state.clone();
        let auth_type = self.auth_type;

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

            // Header line 2: Status line (model, auth, state, history indicator, palette indicator)
            let auth_indicator = match auth_type {
                AuthType::OAuth => ("●", Color::Green, "OAuth"),
                AuthType::ApiKey => ("●", Color::Blue, "API"),
                AuthType::None => ("○", Color::Red, "No Auth"),
            };
            let mut status_spans = vec![
                Span::styled(&model_name, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(auth_indicator.0, Style::default().fg(auth_indicator.1)),
                Span::styled(
                    format!(" {}", auth_indicator.2),
                    Style::default().fg(Color::DarkGray),
                ),
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
            // Show palette indicator (temporary - will be replaced by actual palette in Slice 2)
            if palette_open {
                status_spans.push(Span::raw(" │ "));
                status_spans.push(Span::styled(
                    "/ Commands (Esc to cancel)",
                    Style::default().fg(Color::Yellow),
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

            // Command palette overlay (rendered last to be on top)
            if let Some(palette) = &palette_state {
                Self::render_command_palette(frame, palette, area, chunks[2].y);
            }

            // Model picker overlay
            if let Some(picker) = &model_picker_state {
                Self::render_model_picker(frame, picker, area, chunks[2].y);
            }

            // Login overlay (rendered on top of everything)
            if login_state.is_active() {
                Self::render_login_overlay(frame, &login_state, area);
            }
        })?;

        Ok(())
    }

    /// Renders the command palette as an overlay.
    ///
    /// Layout (Amp-style with input at top):
    /// ```text
    /// ┌ Commands ──────────────────────────────┐
    /// │ > filter_text█                         │  ← Input at TOP
    /// ├────────────────────────────────────────┤
    /// │▶ /clear (new)     Clear conversation...│
    /// │  /quit (q, exit)  Exit ZDX             │
    /// ├────────────────────────────────────────┤
    /// │ ↑↓ navigate • Enter select • Esc cancel│  ← Keyboard hints
    /// └────────────────────────────────────────┘
    /// ```
    fn render_command_palette(
        frame: &mut ratatui::Frame,
        palette: &CommandPaletteState,
        area: Rect,
        input_top_y: u16,
    ) {
        let commands = palette.filtered_commands();

        // Calculate palette dimensions
        // Width: min(50, terminal_width - 4) to leave some margin
        let palette_width = 50.min(area.width.saturating_sub(4));
        // Height: commands + 6 (top border + filter line + separator + commands + separator + hints + bottom border)
        // Minimum 7 lines even if no commands match
        let palette_height = (commands.len() as u16 + 6).max(7).min(area.height / 2);

        // Available vertical space (between header and input)
        let available_top = HEADER_HEIGHT;
        let available_bottom = input_top_y;
        let available_height = available_bottom.saturating_sub(available_top);

        // Position: centered both horizontally and vertically
        let palette_x = (area.width.saturating_sub(palette_width)) / 2;
        let palette_y = available_top + (available_height.saturating_sub(palette_height)) / 2;

        let palette_area = Rect::new(palette_x, palette_y, palette_width, palette_height);

        // Clear the area behind the palette
        frame.render_widget(Clear, palette_area);

        // Render outer border
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Commands ")
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(outer_block, palette_area);

        // Inner area (inside border)
        let inner_area = Rect::new(
            palette_area.x + 1,
            palette_area.y + 1,
            palette_area.width.saturating_sub(2),
            palette_area.height.saturating_sub(2),
        );

        // Filter input line at TOP (row 0 of inner area)
        // Truncate long filter text to fit in available width (leave room for "> /" prefix and "█" cursor)
        let max_filter_len = inner_area.width.saturating_sub(4) as usize; // 4 = "> /" + "█"
        let filter_display = if palette.filter.is_empty() {
            "/".to_string()
        } else if palette.filter.len() > max_filter_len {
            // Truncate from the start to show the most recent characters
            let truncated = &palette.filter[palette.filter.len() - max_filter_len..];
            format!("/…{}", truncated)
        } else {
            format!("/{}", palette.filter)
        };
        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(&filter_display, Style::default().fg(Color::Yellow)),
            Span::styled("█", Style::default().fg(Color::Yellow)), // Cursor indicator
        ]);
        let filter_para = Paragraph::new(filter_line);
        let filter_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
        frame.render_widget(filter_para, filter_area);

        // Separator line (row 1 of inner area)
        let separator = "─".repeat(inner_area.width as usize);
        let separator_line = Paragraph::new(Line::from(Span::styled(
            &separator,
            Style::default().fg(Color::DarkGray),
        )));
        let separator_area = Rect::new(inner_area.x, inner_area.y + 1, inner_area.width, 1);
        frame.render_widget(separator_line, separator_area);

        // Command list area (rows 2 to height-3, leaving room for bottom separator + hints)
        let list_height = inner_area.height.saturating_sub(4); // -4 for filter, separator, bottom separator, hints
        let list_area = Rect::new(
            inner_area.x,
            inner_area.y + 2,
            inner_area.width,
            list_height,
        );

        // Build the list items
        let items: Vec<ListItem> = if commands.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "  No matching commands",
                Style::default().fg(Color::DarkGray),
            )))]
        } else {
            commands
                .iter()
                .map(|cmd| {
                    let name = cmd.display_name();
                    let desc = cmd.description;
                    // Format: "/clear (new)  Clear conversation..."
                    let line = Line::from(vec![
                        Span::styled(
                            format!("{:<16}", name),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(desc, Style::default().fg(Color::White)),
                    ]);
                    ListItem::new(line)
                })
                .collect()
        };

        // Create the list with selection (no block - we already have outer border)
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        // Render with selection state
        let mut list_state = ListState::default();
        if !commands.is_empty() {
            list_state.select(Some(palette.selected));
        }
        frame.render_stateful_widget(list, list_area, &mut list_state);

        // Bottom separator (above hints)
        let bottom_sep_y = inner_area.y + 2 + list_height;
        if bottom_sep_y < inner_area.y + inner_area.height {
            let bottom_separator_area = Rect::new(inner_area.x, bottom_sep_y, inner_area.width, 1);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    &separator,
                    Style::default().fg(Color::DarkGray),
                ))),
                bottom_separator_area,
            );
        }

        // Keyboard hints at the bottom
        let hints_y = inner_area.y + inner_area.height.saturating_sub(1);
        let hints_area = Rect::new(inner_area.x, hints_y, inner_area.width, 1);
        let hints_line = Line::from(vec![
            Span::styled("↑↓", Style::default().fg(Color::Yellow)),
            Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
            Span::styled("•", Style::default().fg(Color::DarkGray)),
            Span::styled(" Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" select ", Style::default().fg(Color::DarkGray)),
            Span::styled("•", Style::default().fg(Color::DarkGray)),
            Span::styled(" Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]);
        let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
        frame.render_widget(hints_para, hints_area);
    }

    /// Renders the model picker as an overlay.
    fn render_model_picker(
        frame: &mut ratatui::Frame,
        picker: &ModelPickerState,
        area: Rect,
        input_top_y: u16,
    ) {
        // Calculate picker dimensions
        let picker_width = 30.min(area.width.saturating_sub(4));
        // Height: models + 4 (top border + title space + models + separator + hints + bottom border)
        let picker_height = (AVAILABLE_MODELS.len() as u16 + 5).min(area.height / 2);

        // Available vertical space (between header and input)
        let available_top = HEADER_HEIGHT;
        let available_bottom = input_top_y;
        let available_height = available_bottom.saturating_sub(available_top);

        // Position: centered both horizontally and vertically
        let picker_x = (area.width.saturating_sub(picker_width)) / 2;
        let picker_y = available_top + (available_height.saturating_sub(picker_height)) / 2;

        let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

        // Clear the area behind the picker
        frame.render_widget(Clear, picker_area);

        // Render outer border
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .title(" Select Model ")
            .title_style(
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(outer_block, picker_area);

        // Inner area (inside border)
        let inner_area = Rect::new(
            picker_area.x + 1,
            picker_area.y + 1,
            picker_area.width.saturating_sub(2),
            picker_area.height.saturating_sub(2),
        );

        // Model list area
        let list_height = inner_area.height.saturating_sub(2); // -2 for separator + hints
        let list_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, list_height);

        // Build the list items
        let items: Vec<ListItem> = AVAILABLE_MODELS
            .iter()
            .map(|model| {
                let line = Line::from(Span::styled(
                    model.display_name,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                ListItem::new(line)
            })
            .collect();

        // Create the list with selection
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        // Render with selection state
        let mut list_state = ListState::default();
        list_state.select(Some(picker.selected));
        frame.render_stateful_widget(list, list_area, &mut list_state);

        // Separator line
        let separator = "─".repeat(inner_area.width as usize);
        let sep_y = inner_area.y + list_height;
        if sep_y < inner_area.y + inner_area.height {
            let separator_area = Rect::new(inner_area.x, sep_y, inner_area.width, 1);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    &separator,
                    Style::default().fg(Color::DarkGray),
                ))),
                separator_area,
            );
        }

        // Keyboard hints at the bottom
        let hints_y = inner_area.y + inner_area.height.saturating_sub(1);
        let hints_area = Rect::new(inner_area.x, hints_y, inner_area.width, 1);
        let hints_line = Line::from(vec![
            Span::styled("↑↓", Style::default().fg(Color::Magenta)),
            Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
            Span::styled("•", Style::default().fg(Color::DarkGray)),
            Span::styled(" Enter", Style::default().fg(Color::Magenta)),
            Span::styled(" select ", Style::default().fg(Color::DarkGray)),
            Span::styled("•", Style::default().fg(Color::DarkGray)),
            Span::styled(" Esc", Style::default().fg(Color::Magenta)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]);
        let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
        frame.render_widget(hints_para, hints_area);
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
                // Route paste to login input if overlay is active
                if let LoginState::AwaitingCode { ref mut input, .. } = self.login_state {
                    input.push_str(&text);
                } else {
                    self.textarea.insert_str(&text);
                }
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
        // If login overlay is active, route all keys to login handler
        if self.login_state.is_active() {
            return self.handle_login_key(key);
        }

        // If command palette is open, route all keys to palette handler
        if self.command_palette.is_some() {
            return self.handle_palette_key(key);
        }

        // If model picker is open, route all keys to picker handler
        if self.model_picker.is_some() {
            return self.handle_model_picker_key(key);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // "/" opens command palette only when input is empty
            // Otherwise, just type "/" normally
            KeyCode::Char('/') if !ctrl && !shift && !alt => {
                if self.get_input_text().is_empty() {
                    // Input is empty, so Escape shouldn't insert "/" (nothing to preserve)
                    self.open_command_palette(false);
                } else {
                    self.textarea.input(key);
                }
            }
            // Ctrl+P: open command palette (always works, Escape won't insert anything)
            KeyCode::Char('p') if ctrl && !shift && !alt => {
                self.open_command_palette(false);
            }
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

    // ========================================================================
    // Command Popup
    // ========================================================================

    /// Opens the command palette.
    ///
    /// `insert_slash_on_escape`: if true, Escape will insert "/" into input.
    /// Pass `true` when opened via "/" key, `false` when opened via Ctrl+P.
    fn open_command_palette(&mut self, insert_slash_on_escape: bool) {
        if self.command_palette.is_none() {
            self.command_palette = Some(CommandPaletteState::new(insert_slash_on_escape));
        }
    }

    /// Closes the command palette.
    ///
    /// If `insert_slash` is true, inserts "/" into the input at cursor position.
    /// This preserves user intent when they press Escape (they typed "/" expecting to type it).
    fn close_command_palette(&mut self, insert_slash: bool) {
        self.command_palette = None;
        if insert_slash {
            self.textarea.insert_char('/');
        }
    }

    /// Handles key events when the command palette is open.
    fn handle_palette_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            // Escape: close palette, insert "/" only if opened via "/" key
            KeyCode::Esc => {
                let insert_slash = self
                    .command_palette
                    .as_ref()
                    .is_some_and(|p| p.insert_slash_on_escape);
                self.close_command_palette(insert_slash);
            }
            // Ctrl+C: close palette without inserting "/" (user wants to cancel)
            KeyCode::Char('c') if ctrl => {
                self.close_command_palette(false);
            }
            // Up arrow: move selection up
            KeyCode::Up => {
                self.palette_select_prev();
            }
            // Down arrow: move selection down
            KeyCode::Down => {
                self.palette_select_next();
            }
            // Enter or Tab: execute selected command
            KeyCode::Enter | KeyCode::Tab => {
                self.execute_selected_command();
            }
            // Backspace: remove last filter character
            KeyCode::Backspace => {
                if let Some(palette) = &mut self.command_palette {
                    palette.filter.pop();
                    palette.clamp_selection();
                }
            }
            // Regular character: append to filter
            KeyCode::Char(c) if !ctrl => {
                if let Some(palette) = &mut self.command_palette {
                    palette.filter.push(c);
                    palette.clamp_selection();
                }
            }
            // Ignore other keys
            _ => {}
        }

        Ok(())
    }

    /// Moves palette selection to the previous item.
    fn palette_select_prev(&mut self) {
        if let Some(palette) = &mut self.command_palette {
            let count = palette.filtered_commands().len();
            if count > 0 && palette.selected > 0 {
                palette.selected -= 1;
            }
        }
    }

    /// Moves palette selection to the next item.
    fn palette_select_next(&mut self) {
        if let Some(palette) = &mut self.command_palette {
            let count = palette.filtered_commands().len();
            if count > 0 && palette.selected < count - 1 {
                palette.selected += 1;
            }
        }
    }

    /// Executes the currently selected command in the palette.
    fn execute_selected_command(&mut self) {
        let Some(palette) = &self.command_palette else {
            return;
        };

        let filtered = palette.filtered_commands();
        let Some(cmd) = filtered.get(palette.selected) else {
            // No command selected (empty filter result)
            self.close_command_palette(false);
            return;
        };

        // Match on command name and execute
        match cmd.name {
            "login" => {
                self.close_command_palette(false);
                self.update(LoginEvent::LoginRequested);
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

    /// Opens the model picker overlay.
    fn open_model_picker(&mut self) {
        if self.model_picker.is_none() {
            self.model_picker = Some(ModelPickerState::new(&self.config.model));
        }
    }

    /// Closes the model picker overlay.
    fn close_model_picker(&mut self) {
        self.model_picker = None;
    }

    /// Handles key events when the model picker is open.
    fn handle_model_picker_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            // Escape or Ctrl+C: close picker
            KeyCode::Esc => {
                self.close_model_picker();
            }
            KeyCode::Char('c') if ctrl => {
                self.close_model_picker();
            }
            // Up arrow: move selection up
            KeyCode::Up => {
                self.model_picker_select_prev();
            }
            // Down arrow: move selection down
            KeyCode::Down => {
                self.model_picker_select_next();
            }
            // Enter: select model
            KeyCode::Enter => {
                self.execute_model_selection();
            }
            // Ignore other keys
            _ => {}
        }

        Ok(())
    }

    /// Moves model picker selection to the previous item.
    fn model_picker_select_prev(&mut self) {
        if let Some(picker) = &mut self.model_picker
            && picker.selected > 0
        {
            picker.selected -= 1;
        }
    }

    /// Moves model picker selection to the next item.
    fn model_picker_select_next(&mut self) {
        if let Some(picker) = &mut self.model_picker
            && picker.selected < AVAILABLE_MODELS.len() - 1
        {
            picker.selected += 1;
        }
    }

    /// Executes the model selection.
    fn execute_model_selection(&mut self) {
        let Some(picker) = &self.model_picker else {
            return;
        };

        let Some(model) = AVAILABLE_MODELS.get(picker.selected) else {
            self.close_model_picker();
            return;
        };

        let model_id = model.id.to_string();
        let display_name = model.display_name;

        self.config.model = model_id;
        self.close_model_picker();

        self.transcript.push(HistoryCell::system(format!(
            "Switched to {}",
            display_name
        )));
    }

    /// Executes the /new (or /clear) command.
    ///
    /// Starts a fresh session: clears transcript, messages, and creates a new session file.
    fn execute_new(&mut self) {
        // Block if engine is running (safety - avoid race conditions)
        if self.is_engine_running() {
            self.transcript
                .push(HistoryCell::system("Cannot clear while streaming."));
            return;
        }

        // Clear transcript
        self.transcript.clear();
        // Clear message history (but system prompt is separate)
        self.messages.clear();
        // Clear command history for this session
        self.command_history.clear();
        // Reset scroll to follow latest
        self.scroll_mode = ScrollMode::FollowLatest;

        // Start a new session (if sessions are enabled)
        if self.session.is_some() {
            match session::Session::new() {
                Ok(new_session) => {
                    let new_id = new_session.id.clone();
                    self.session = Some(new_session);
                    self.transcript
                        .push(HistoryCell::system(format!("New session: {}", new_id)));
                }
                Err(e) => {
                    self.transcript.push(HistoryCell::system(format!(
                        "Warning: Failed to create new session: {}",
                        e
                    )));
                    self.transcript
                        .push(HistoryCell::system("Conversation cleared."));
                }
            }
        } else {
            // No session mode - just show confirmation
            self.transcript
                .push(HistoryCell::system("Conversation cleared."));
        }
    }

    /// Executes the /logout command.
    fn execute_logout(&mut self) {
        use crate::providers::oauth::anthropic;

        match anthropic::clear_credentials() {
            Ok(true) => {
                self.refresh_auth_type();
                self.transcript
                    .push(HistoryCell::system("Logged out from Anthropic OAuth."));
            }
            Ok(false) => {
                self.transcript
                    .push(HistoryCell::system("No OAuth credentials to clear."));
            }
            Err(e) => {
                self.transcript
                    .push(HistoryCell::system(format!("Logout failed: {}", e)));
            }
        }
    }

    /// Executes the /quit command.
    fn execute_quit(&mut self) {
        // Allow quit even during streaming - will interrupt first
        if self.is_engine_running() {
            self.interrupt_engine();
        }
        self.should_quit = true;
    }

    // ========================================================================
    // Login Flow (Reducer Pattern)
    // ========================================================================

    /// Updates login state based on an event (reducer pattern).
    ///
    /// This is the single point of mutation for login state.
    fn update(&mut self, event: LoginEvent) {
        use crate::providers::oauth::anthropic;

        match event {
            LoginEvent::LoginRequested => {
                let pkce = anthropic::generate_pkce();
                let url = anthropic::build_auth_url(&pkce);
                let _ = open::that(&url); // Try to open browser
                self.login_state = LoginState::AwaitingCode {
                    url,
                    pkce_verifier: pkce.verifier,
                    input: String::new(),
                    error: None,
                };
            }
            LoginEvent::AuthCodeEntered { code } => {
                if let LoginState::AwaitingCode { pkce_verifier, .. } = &self.login_state {
                    self.login_state = LoginState::Exchanging {
                        code,
                        pkce_verifier: pkce_verifier.clone(),
                    };
                }
            }
            LoginEvent::LoginSucceeded => {
                self.login_state = LoginState::Idle;
                self.refresh_auth_type();
                self.transcript
                    .push(HistoryCell::system("Logged in with Anthropic OAuth."));
            }
            LoginEvent::LoginFailed { message } => {
                let pkce = anthropic::generate_pkce();
                let url = anthropic::build_auth_url(&pkce);
                self.login_state = LoginState::AwaitingCode {
                    url,
                    pkce_verifier: pkce.verifier,
                    input: String::new(),
                    error: Some(message),
                };
            }
            LoginEvent::LoginCancelled => {
                self.login_state = LoginState::Idle;
                self.login_exchange_rx = None;
            }
        }
    }

    /// Handles key events when the login overlay is active.
    fn handle_login_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match &mut self.login_state {
            LoginState::Idle => {}
            LoginState::AwaitingCode { input, .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    self.update(LoginEvent::LoginCancelled);
                }
                KeyCode::Enter => {
                    let code = input.trim().to_string();
                    if !code.is_empty() {
                        self.update(LoginEvent::AuthCodeEntered { code });
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
                    self.update(LoginEvent::LoginCancelled);
                }
            }
        }
        Ok(())
    }

    /// Spawns async task to exchange auth code for tokens.
    fn spawn_token_exchange(&mut self) {
        use crate::providers::oauth::anthropic;

        let (code, pkce_verifier) = match &self.login_state {
            LoginState::Exchanging {
                code,
                pkce_verifier,
            } => (code.clone(), pkce_verifier.clone()),
            _ => return,
        };

        let (tx, rx) = mpsc::channel::<Result<(), String>>(1);
        self.login_exchange_rx = Some(rx);

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

    /// Polls for login exchange result (non-blocking).
    fn poll_login_result(&mut self) {
        let Some(rx) = &mut self.login_exchange_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(())) => {
                self.login_exchange_rx = None;
                self.update(LoginEvent::LoginSucceeded);
            }
            Ok(Err(msg)) => {
                self.login_exchange_rx = None;
                self.update(LoginEvent::LoginFailed { message: msg });
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.login_exchange_rx = None;
                self.update(LoginEvent::LoginFailed {
                    message: "Exchange task failed".to_string(),
                });
            }
        }
    }

    /// Renders the login overlay.
    fn render_login_overlay(frame: &mut ratatui::Frame, state: &LoginState, area: Rect) {
        let popup_width = 60.min(area.width.saturating_sub(4));
        let popup_height = 9.min(area.height.saturating_sub(4));
        let popup_x = (area.width.saturating_sub(popup_width)) / 2;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Anthropic Login ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(block, popup_area);

        let inner = Rect::new(
            popup_area.x + 2,
            popup_area.y + 1,
            popup_area.width.saturating_sub(4),
            popup_area.height.saturating_sub(2),
        );

        let lines: Vec<Line> = match state {
            LoginState::Idle => return,
            LoginState::AwaitingCode {
                url, input, error, ..
            } => {
                // Show truncated URL for display
                let display_url = truncate_middle(url, inner.width.saturating_sub(2) as usize);

                let mut l = vec![
                    Line::from(Span::styled(
                        "Browser opened for authentication.",
                        Style::default().fg(Color::Green),
                    )),
                    Line::from(Span::styled(
                        display_url,
                        Style::default().fg(Color::DarkGray),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Paste auth code:",
                        Style::default().fg(Color::White),
                    )),
                    Line::from(Span::styled(
                        format!("> {}█", input),
                        Style::default().fg(Color::Yellow),
                    )),
                ];
                if let Some(e) = error {
                    l.push(Line::from(""));
                    l.push(Line::from(Span::styled(
                        e.as_str(),
                        Style::default().fg(Color::Red),
                    )));
                }
                l.push(Line::from(""));
                l.push(Line::from(Span::styled(
                    "Esc to cancel",
                    Style::default().fg(Color::DarkGray),
                )));
                l
            }
            LoginState::Exchanging { .. } => vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Exchanging code...",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Esc to cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
        };

        let para = Paragraph::new(lines);
        frame.render_widget(para, inner);
    }
}

/// Truncates a string in the middle with "..." if too long.
fn truncate_middle(s: &str, max_len: usize) -> String {
    if s.len() <= max_len || max_len < 10 {
        return s.to_string();
    }
    let half = (max_len - 3) / 2;
    format!("{}...{}", &s[..half], &s[s.len() - half..])
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
    use super::*;

    // Note: Terminal tests are difficult to run in CI since they require a real TTY.
    // Integration tests for TUI2 should spawn the CLI and verify stdout/stderr behavior.
    //
    // Key guarantees to test manually or via integration tests:
    // - Terminal is restored on normal exit
    // - Terminal is restored on panic
    // - Terminal is restored on Ctrl+C
    // - Resize events don't break the UI

    // ========================================================================
    // Slash Command Tests
    // ========================================================================

    #[test]
    fn test_slash_command_matches_name() {
        let cmd = &SLASH_COMMANDS[3]; // new
        assert!(cmd.matches("new"));
        assert!(cmd.matches("ne"));
        assert!(cmd.matches("NEW")); // case-insensitive
        assert!(!cmd.matches("quit"));
    }

    #[test]
    fn test_slash_command_matches_alias() {
        let cmd = &SLASH_COMMANDS[3]; // new (alias: clear)
        assert!(cmd.matches("clear"));
        assert!(cmd.matches("cle"));
        assert!(cmd.matches("CLEAR")); // case-insensitive
    }

    #[test]
    fn test_slash_command_display_name() {
        let login_cmd = &SLASH_COMMANDS[0];
        assert_eq!(login_cmd.display_name(), "/login");

        let logout_cmd = &SLASH_COMMANDS[1];
        assert_eq!(logout_cmd.display_name(), "/logout");

        let model_cmd = &SLASH_COMMANDS[2];
        assert_eq!(model_cmd.display_name(), "/model (m)");

        let new_cmd = &SLASH_COMMANDS[3];
        assert_eq!(new_cmd.display_name(), "/new (clear)");

        let quit = &SLASH_COMMANDS[4];
        assert_eq!(quit.display_name(), "/quit (q, exit)");
    }

    #[test]
    fn test_palette_state_filtered_commands_empty_filter() {
        let state = CommandPaletteState::new(true);
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn test_palette_state_filtered_commands_with_filter() {
        let mut state = CommandPaletteState::new(true);
        state.filter = "ne".to_string();
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "new");
    }

    #[test]
    fn test_palette_state_filtered_commands_no_match() {
        let mut state = CommandPaletteState::new(true);
        state.filter = "xyz".to_string();
        let filtered = state.filtered_commands();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_palette_state_clamp_selection() {
        let mut state = CommandPaletteState::new(true);
        state.selected = 10; // way out of bounds
        state.clamp_selection();
        assert_eq!(state.selected, SLASH_COMMANDS.len() - 1);
    }

    #[test]
    fn test_palette_state_clamp_selection_empty_filter() {
        let mut state = CommandPaletteState::new(true);
        state.filter = "xyz".to_string(); // no matches
        state.selected = 5;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_palette_navigation_up_down() {
        let mut state = CommandPaletteState::new(true);
        assert_eq!(state.selected, 0);

        // Move down
        let count = state.filtered_commands().len();
        if count > 1 {
            state.selected = 1;
            assert_eq!(state.selected, 1);
        }

        // Move up
        state.selected = 0;
        assert_eq!(state.selected, 0);

        // Can't go below 0
        // (This is enforced by the palette_select_prev method, not by state itself)
    }

    #[test]
    fn test_palette_filter_clamps_selection() {
        let mut state = CommandPaletteState::new(true);
        // Select the second command
        state.selected = 1;
        assert_eq!(state.filtered_commands().len(), 5); // login, logout, model, new, quit

        // Filter to just one command
        state.filter = "new".to_string();
        state.clamp_selection();
        assert_eq!(state.filtered_commands().len(), 1);
        assert_eq!(state.selected, 0); // Clamped down
    }

    #[test]
    fn test_palette_filter_by_alias() {
        let mut state = CommandPaletteState::new(true);
        // Filter by "exit" which is an alias for "quit"
        state.filter = "exit".to_string();
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "quit");
    }

    #[test]
    fn test_palette_long_filter_still_filters_correctly() {
        // Very long filter text should still work for filtering
        // (truncation is only for display, not for matching)
        // The filter matches if command name/alias CONTAINS the filter,
        // so a very long filter won't match anything
        let mut state = CommandPaletteState::new(true);
        state.filter = "this_is_a_very_long_filter_that_wont_match_anything".to_string();
        let filtered = state.filtered_commands();
        assert!(filtered.is_empty());

        // Even a moderately long filter like "newclear" won't match
        // because neither "new" nor "clear" contains "newclear"
        let mut state2 = CommandPaletteState::new(true);
        state2.filter = "newclear".to_string();
        let filtered2 = state2.filtered_commands();
        assert!(filtered2.is_empty());

        // But a filter that's a substring of the command still works
        let mut state3 = CommandPaletteState::new(true);
        state3.filter = "ew".to_string(); // substring of "new"
        let filtered3 = state3.filtered_commands();
        assert_eq!(filtered3.len(), 1);
        assert_eq!(filtered3[0].name, "new");
    }

    #[test]
    fn test_palette_insert_slash_on_escape_flag() {
        // When opened via "/" (empty input) or Ctrl+P, insert_slash_on_escape should be false
        // (nothing to preserve when input was empty)
        let state_from_slash = CommandPaletteState::new(false);
        assert!(!state_from_slash.insert_slash_on_escape);

        let state_from_ctrl_p = CommandPaletteState::new(false);
        assert!(!state_from_ctrl_p.insert_slash_on_escape);

        // The flag can still be set to true (for future use cases)
        let state_with_flag = CommandPaletteState::new(true);
        assert!(state_with_flag.insert_slash_on_escape);
    }
}
