//! TUI reducer (update function).
//!
//! All state mutations happen here. The runtime calls `update(state, event)`
//! and executes the returned effects.
//!
//! This is the single source of truth for how events modify state.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::core::interrupt;
use crate::core::session::SessionEvent;
use crate::models::AVAILABLE_MODELS;
use crate::ui::effects::UiEffect;
use crate::ui::events::{TurnResult, UiEvent};
use crate::ui::state::{
    CommandPaletteState, EngineState, LoginEvent, LoginState, ModelPickerState, ScrollMode,
    TuiState,
};
use crate::ui::transcript::HistoryCell;

/// Lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_LINES: usize = 3;

/// The main reducer function.
///
/// Takes the current state and an event, mutates state, and returns effects
/// for the runtime to execute.
pub fn update(state: &mut TuiState, event: UiEvent, viewport_height: usize) -> Vec<UiEffect> {
    match event {
        UiEvent::Tick => {
            // Advance spinner animation
            state.spinner_frame = state.spinner_frame.wrapping_add(1);
            vec![]
        }
        UiEvent::Terminal(term_event) => handle_terminal_event(state, term_event, viewport_height),
        UiEvent::Engine(engine_event) => {
            handle_engine_event(state, &engine_event);
            vec![]
        }
        UiEvent::TurnFinished(result) => handle_turn_finished(state, result),
        UiEvent::LoginResult(result) => {
            handle_login_result(state, result);
            vec![]
        }
    }
}

// ============================================================================
// Terminal Event Handlers
// ============================================================================

fn handle_terminal_event(
    state: &mut TuiState,
    event: Event,
    viewport_height: usize,
) -> Vec<UiEffect> {
    match event {
        Event::Key(key) => handle_key(state, key, viewport_height),
        Event::Mouse(mouse) => {
            handle_mouse(state, mouse, viewport_height);
            vec![]
        }
        Event::Paste(text) => {
            handle_paste(state, &text);
            vec![]
        }
        Event::Resize(_, _) => vec![],
        _ => vec![],
    }
}

fn handle_paste(state: &mut TuiState, text: &str) {
    if let LoginState::AwaitingCode { ref mut input, .. } = state.login_state {
        input.push_str(text);
    } else {
        state.textarea.insert_str(text);
    }
}

fn handle_mouse(state: &mut TuiState, mouse: crossterm::event::MouseEvent, viewport_height: usize) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            scroll_lines_up(state, MOUSE_SCROLL_LINES, viewport_height);
        }
        MouseEventKind::ScrollDown => {
            scroll_lines_down(state, MOUSE_SCROLL_LINES, viewport_height);
        }
        _ => {}
    }
}

fn handle_key(
    state: &mut TuiState,
    key: crossterm::event::KeyEvent,
    viewport_height: usize,
) -> Vec<UiEffect> {
    // Route to overlay handlers first
    if state.login_state.is_active() {
        return handle_login_key(state, key);
    }
    if state.command_palette.is_some() {
        return handle_palette_key(state, key);
    }
    if state.model_picker.is_some() {
        return handle_model_picker_key(state, key);
    }

    handle_main_key(state, key, viewport_height)
}

fn handle_main_key(
    state: &mut TuiState,
    key: crossterm::event::KeyEvent,
    viewport_height: usize,
) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char('/') if !ctrl && !shift && !alt => {
            if state.get_input_text().is_empty() {
                open_command_palette(state, false);
            } else {
                state.textarea.input(key);
            }
            vec![]
        }
        KeyCode::Char('p') if ctrl && !shift && !alt => {
            open_command_palette(state, false);
            vec![]
        }
        KeyCode::Char('q') if !ctrl && !shift && !alt => {
            if state.get_input_text().is_empty() {
                vec![UiEffect::Quit]
            } else {
                state.textarea.input(key);
                vec![]
            }
        }
        KeyCode::Char('c') if ctrl => {
            if state.engine_state.is_running() {
                vec![UiEffect::InterruptEngine]
            } else if !state.get_input_text().is_empty() {
                state.clear_input();
                vec![]
            } else {
                vec![UiEffect::Quit]
            }
        }
        KeyCode::Enter if !shift && !alt => submit_input(state),
        KeyCode::Char('j') if ctrl => {
            state.textarea.insert_newline();
            vec![]
        }
        KeyCode::Esc => {
            if state.engine_state.is_running() {
                vec![UiEffect::InterruptEngine]
            } else {
                state.clear_input();
                vec![]
            }
        }
        KeyCode::PageUp => {
            scroll_page_up(state, viewport_height);
            vec![]
        }
        KeyCode::PageDown => {
            scroll_page_down(state, viewport_height);
            vec![]
        }
        KeyCode::Home if ctrl => {
            scroll_to_top(state);
            vec![]
        }
        KeyCode::End if ctrl => {
            scroll_to_bottom(state);
            vec![]
        }
        KeyCode::Up if !ctrl && !shift && !alt => {
            if should_navigate_history_up(state) {
                navigate_history_up(state);
            } else {
                state.textarea.input(key);
            }
            vec![]
        }
        KeyCode::Down if !ctrl && !shift && !alt => {
            if should_navigate_history_down(state) {
                navigate_history_down(state);
            } else {
                state.textarea.input(key);
            }
            vec![]
        }
        _ => {
            state.reset_history_navigation();
            state.textarea.input(key);
            vec![]
        }
    }
}

// ============================================================================
// Submit / Engine
// ============================================================================

fn submit_input(state: &mut TuiState) -> Vec<UiEffect> {
    if !matches!(state.engine_state, EngineState::Idle) {
        return vec![];
    }

    let text = state.get_input_text();
    if text.trim().is_empty() {
        return vec![];
    }

    state.command_history.push(text.clone());
    state.reset_history_navigation();

    state.transcript.push(HistoryCell::user(&text));
    state
        .messages
        .push(crate::providers::anthropic::ChatMessage::user(&text));

    let effects = if state.session.is_some() {
        vec![
            UiEffect::SaveSession {
                event: SessionEvent::user_message(&text),
            },
            UiEffect::StartEngineTurn,
        ]
    } else {
        vec![UiEffect::StartEngineTurn]
    };

    state.clear_input();
    effects
}

// ============================================================================
// Scroll Methods
// ============================================================================

fn scroll_page_up(state: &mut TuiState, viewport_height: usize) {
    scroll_lines_up(state, viewport_height.max(1), viewport_height);
}

fn scroll_page_down(state: &mut TuiState, viewport_height: usize) {
    scroll_lines_down(state, viewport_height.max(1), viewport_height);
}

fn scroll_to_top(state: &mut TuiState) {
    state.scroll_mode = ScrollMode::Anchored { offset: 0 };
}

fn scroll_to_bottom(state: &mut TuiState) {
    state.scroll_mode = ScrollMode::FollowLatest;
}

fn scroll_lines_up(state: &mut TuiState, lines: usize, viewport_height: usize) {
    let page_size = viewport_height.max(1);
    let current_offset = match &state.scroll_mode {
        ScrollMode::FollowLatest => state.cached_line_count.saturating_sub(page_size),
        ScrollMode::Anchored { offset } => *offset,
    };

    let new_offset = current_offset.saturating_sub(lines);
    state.scroll_mode = ScrollMode::Anchored { offset: new_offset };
}

fn scroll_lines_down(state: &mut TuiState, lines: usize, viewport_height: usize) {
    let page_size = viewport_height.max(1);
    let current_offset = match &state.scroll_mode {
        ScrollMode::FollowLatest => return,
        ScrollMode::Anchored { offset } => *offset,
    };

    let max_offset = state.cached_line_count.saturating_sub(page_size);
    let new_offset = (current_offset + lines).min(max_offset);

    if new_offset >= max_offset {
        state.scroll_mode = ScrollMode::FollowLatest;
    } else {
        state.scroll_mode = ScrollMode::Anchored { offset: new_offset };
    }
}

// ============================================================================
// History Navigation
// ============================================================================

fn should_navigate_history_up(state: &TuiState) -> bool {
    if state.command_history.is_empty() {
        return false;
    }
    if state.history_index.is_some() {
        return true;
    }
    if state.get_input_text().is_empty() {
        return true;
    }
    let (row, _col) = state.textarea.cursor();
    row == 0
}

fn should_navigate_history_down(state: &TuiState) -> bool {
    if state.history_index.is_none() {
        return false;
    }
    let (row, _col) = state.textarea.cursor();
    let line_count = state.textarea.lines().len();
    row >= line_count.saturating_sub(1)
}

fn navigate_history_up(state: &mut TuiState) {
    if state.command_history.is_empty() {
        return;
    }

    if state.history_index.is_none() {
        let current = state.get_input_text();
        state.input_draft = Some(current);
        state.history_index = Some(state.command_history.len() - 1);
    } else if let Some(idx) = state.history_index
        && idx > 0
    {
        state.history_index = Some(idx - 1);
    }

    if let Some(idx) = state.history_index
        && let Some(entry) = state.command_history.get(idx).cloned()
    {
        state.set_input_text(&entry);
    }
}

fn navigate_history_down(state: &mut TuiState) {
    let Some(idx) = state.history_index else {
        return;
    };

    if idx + 1 < state.command_history.len() {
        state.history_index = Some(idx + 1);
        if let Some(entry) = state.command_history.get(idx + 1).cloned() {
            state.set_input_text(&entry);
        }
    } else {
        let draft = state.input_draft.take().unwrap_or_default();
        state.history_index = None;
        state.set_input_text(&draft);
    }
}

// ============================================================================
// Command Palette
// ============================================================================

fn open_command_palette(state: &mut TuiState, insert_slash_on_escape: bool) {
    if state.command_palette.is_none() {
        state.command_palette = Some(CommandPaletteState::new(insert_slash_on_escape));
    }
}

fn close_command_palette(state: &mut TuiState, insert_slash: bool) {
    state.command_palette = None;
    if insert_slash {
        state.textarea.insert_char('/');
    }
}

fn handle_palette_key(state: &mut TuiState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Esc => {
            let insert_slash = state
                .command_palette
                .as_ref()
                .is_some_and(|p| p.insert_slash_on_escape);
            close_command_palette(state, insert_slash);
            vec![]
        }
        KeyCode::Char('c') if ctrl => {
            close_command_palette(state, false);
            vec![]
        }
        KeyCode::Up => {
            palette_select_prev(state);
            vec![]
        }
        KeyCode::Down => {
            palette_select_next(state);
            vec![]
        }
        KeyCode::Enter | KeyCode::Tab => execute_selected_command(state),
        KeyCode::Backspace => {
            if let Some(palette) = &mut state.command_palette {
                palette.filter.pop();
                palette.clamp_selection();
            }
            vec![]
        }
        KeyCode::Char(c) if !ctrl => {
            if let Some(palette) = &mut state.command_palette {
                palette.filter.push(c);
                palette.clamp_selection();
            }
            vec![]
        }
        _ => vec![],
    }
}

fn palette_select_prev(state: &mut TuiState) {
    if let Some(palette) = &mut state.command_palette {
        let count = palette.filtered_commands().len();
        if count > 0 && palette.selected > 0 {
            palette.selected -= 1;
        }
    }
}

fn palette_select_next(state: &mut TuiState) {
    if let Some(palette) = &mut state.command_palette {
        let count = palette.filtered_commands().len();
        if count > 0 && palette.selected < count - 1 {
            palette.selected += 1;
        }
    }
}

fn execute_selected_command(state: &mut TuiState) -> Vec<UiEffect> {
    let Some(palette) = &state.command_palette else {
        return vec![];
    };

    let filtered = palette.filtered_commands();
    let Some(cmd) = filtered.get(palette.selected) else {
        close_command_palette(state, false);
        return vec![];
    };

    let cmd_name = cmd.name;
    close_command_palette(state, false);

    match cmd_name {
        "login" => update_login(state, LoginEvent::LoginRequested),
        "logout" => {
            execute_logout(state);
            vec![]
        }
        "model" => {
            open_model_picker(state);
            vec![]
        }
        "new" => execute_new(state),
        "quit" => execute_quit(state),
        _ => vec![],
    }
}

// ============================================================================
// Model Picker
// ============================================================================

fn open_model_picker(state: &mut TuiState) {
    if state.model_picker.is_none() {
        state.model_picker = Some(ModelPickerState::new(&state.config.model));
    }
}

fn close_model_picker(state: &mut TuiState) {
    state.model_picker = None;
}

fn handle_model_picker_key(state: &mut TuiState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Esc => {
            close_model_picker(state);
            vec![]
        }
        KeyCode::Char('c') if ctrl => {
            close_model_picker(state);
            vec![]
        }
        KeyCode::Up => {
            model_picker_select_prev(state);
            vec![]
        }
        KeyCode::Down => {
            model_picker_select_next(state);
            vec![]
        }
        KeyCode::Enter => execute_model_selection(state),
        _ => vec![],
    }
}

fn model_picker_select_prev(state: &mut TuiState) {
    if let Some(picker) = &mut state.model_picker
        && picker.selected > 0
    {
        picker.selected -= 1;
    }
}

fn model_picker_select_next(state: &mut TuiState) {
    if let Some(picker) = &mut state.model_picker
        && picker.selected < AVAILABLE_MODELS.len() - 1
    {
        picker.selected += 1;
    }
}

fn execute_model_selection(state: &mut TuiState) -> Vec<UiEffect> {
    let Some(picker) = &state.model_picker else {
        return vec![];
    };

    let Some(model) = AVAILABLE_MODELS.get(picker.selected) else {
        close_model_picker(state);
        return vec![];
    };

    let model_id = model.id.to_string();
    let display_name = model.display_name;

    state.config.model = model_id.clone();
    close_model_picker(state);

    state
        .transcript
        .push(HistoryCell::system(format!("Switched to {}", display_name)));

    vec![UiEffect::PersistModel { model: model_id }]
}

// ============================================================================
// Slash Commands
// ============================================================================

fn execute_new(state: &mut TuiState) -> Vec<UiEffect> {
    if state.engine_state.is_running() {
        state
            .transcript
            .push(HistoryCell::system("Cannot clear while streaming."));
        return vec![];
    }

    state.transcript.clear();
    state.messages.clear();
    state.command_history.clear();
    state.scroll_mode = ScrollMode::FollowLatest;

    if state.session.is_some() {
        vec![UiEffect::CreateNewSession]
    } else {
        state
            .transcript
            .push(HistoryCell::system("Conversation cleared."));
        vec![]
    }
}

fn execute_logout(state: &mut TuiState) {
    use crate::providers::oauth::anthropic;

    match anthropic::clear_credentials() {
        Ok(true) => {
            state.refresh_auth_type();
            state
                .transcript
                .push(HistoryCell::system("Logged out from Anthropic OAuth."));
        }
        Ok(false) => {
            state
                .transcript
                .push(HistoryCell::system("No OAuth credentials to clear."));
        }
        Err(e) => {
            state
                .transcript
                .push(HistoryCell::system(format!("Logout failed: {}", e)));
        }
    }
}

fn execute_quit(state: &mut TuiState) -> Vec<UiEffect> {
    if state.engine_state.is_running() {
        vec![UiEffect::InterruptEngine, UiEffect::Quit]
    } else {
        vec![UiEffect::Quit]
    }
}

// ============================================================================
// Engine Event Handlers
// ============================================================================

pub fn handle_engine_event(state: &mut TuiState, event: &crate::core::events::EngineEvent) {
    use crate::core::events::EngineEvent;

    match event {
        EngineEvent::AssistantDelta { text } => {
            match &mut state.engine_state {
                EngineState::Waiting { .. } => {
                    // Create streaming cell and transition to Streaming state
                    let cell = HistoryCell::assistant_streaming("");
                    let cell_id = cell.id();
                    state.transcript.push(cell);

                    let old_state = std::mem::replace(&mut state.engine_state, EngineState::Idle);
                    if let EngineState::Waiting { handle, rx } = old_state {
                        state.engine_state = EngineState::Streaming {
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
                    let needs_new_cell = state
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
                        state.transcript.push(new_cell);
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
            if let EngineState::Streaming { cell_id, .. } = &state.engine_state
                && let Some(cell) = state.transcript.iter_mut().find(|c| c.id() == *cell_id)
            {
                cell.finalize_assistant();
            }
        }
        EngineEvent::Error { message, .. } => {
            state
                .transcript
                .push(HistoryCell::system(format!("Error: {}", message)));
        }
        EngineEvent::Interrupted => {
            state.transcript.push(HistoryCell::system("[Interrupted]"));
            interrupt::reset();
        }
        EngineEvent::ToolRequested { id, name, input } => {
            let tool_cell = HistoryCell::tool_running(id, name, input.clone());
            state.transcript.push(tool_cell);
        }
        EngineEvent::ToolStarted { .. } => {}
        EngineEvent::ToolFinished { id, result } => {
            if let Some(cell) = state
                .transcript
                .iter_mut()
                .find(|c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if tool_use_id == id))
            {
                cell.set_tool_result(result.clone());
            }
        }
    }
}

/// Applies any pending delta to the streaming cell (coalescing).
pub fn apply_pending_delta(state: &mut TuiState) {
    if let EngineState::Streaming {
        cell_id,
        pending_delta,
        ..
    } = &mut state.engine_state
        && !pending_delta.is_empty()
    {
        if let Some(cell) = state.transcript.iter_mut().find(|c| c.id() == *cell_id) {
            cell.append_assistant_delta(pending_delta);
        }
        pending_delta.clear();
    }
}

fn handle_turn_finished(state: &mut TuiState, result: TurnResult) -> Vec<UiEffect> {
    let old_state = std::mem::replace(&mut state.engine_state, EngineState::Idle);

    let had_streaming_cell = matches!(old_state, EngineState::Streaming { .. });

    match result {
        TurnResult::Success {
            final_text,
            messages,
        } => {
            state.messages = messages;

            if !final_text.is_empty() && state.session.is_some() {
                return vec![UiEffect::SaveSession {
                    event: SessionEvent::assistant_message(&final_text),
                }];
            }
        }
        TurnResult::Error(msg) => {
            if !had_streaming_cell {
                state
                    .transcript
                    .push(HistoryCell::system(format!("Error: {}", msg)));
            }
            state.messages.pop();
        }
        TurnResult::Interrupted => {
            // Already handled by EngineEvent::Interrupted
        }
    }

    vec![]
}

// ============================================================================
// Login Flow
// ============================================================================

fn update_login(state: &mut TuiState, event: LoginEvent) -> Vec<UiEffect> {
    use crate::providers::oauth::anthropic;

    match event {
        LoginEvent::LoginRequested => {
            let pkce = anthropic::generate_pkce();
            let url = anthropic::build_auth_url(&pkce);
            state.login_state = LoginState::AwaitingCode {
                url: url.clone(),
                pkce_verifier: pkce.verifier,
                input: String::new(),
                error: None,
            };
            vec![UiEffect::OpenBrowser { url }]
        }
        LoginEvent::AuthCodeEntered { code } => {
            if let LoginState::AwaitingCode { pkce_verifier, .. } = &state.login_state {
                let verifier = pkce_verifier.clone();
                state.login_state = LoginState::Exchanging;
                vec![UiEffect::SpawnTokenExchange { code, verifier }]
            } else {
                vec![]
            }
        }
        LoginEvent::LoginSucceeded => {
            state.login_state = LoginState::Idle;
            state.refresh_auth_type();
            state
                .transcript
                .push(HistoryCell::system("Logged in with Anthropic OAuth."));
            vec![]
        }
        LoginEvent::LoginFailed { message } => {
            let pkce = anthropic::generate_pkce();
            let url = anthropic::build_auth_url(&pkce);
            state.login_state = LoginState::AwaitingCode {
                url,
                pkce_verifier: pkce.verifier,
                input: String::new(),
                error: Some(message),
            };
            vec![]
        }
        LoginEvent::LoginCancelled => {
            state.login_state = LoginState::Idle;
            state.login_exchange_rx = None;
            vec![]
        }
    }
}

fn handle_login_key(state: &mut TuiState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match &mut state.login_state {
        LoginState::Idle => vec![],
        LoginState::AwaitingCode { input, .. } => match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                update_login(state, LoginEvent::LoginCancelled)
            }
            KeyCode::Enter => {
                let code = input.trim().to_string();
                if !code.is_empty() {
                    update_login(state, LoginEvent::AuthCodeEntered { code })
                } else {
                    vec![]
                }
            }
            KeyCode::Backspace => {
                input.pop();
                vec![]
            }
            KeyCode::Char(c) if !ctrl => {
                input.push(c);
                vec![]
            }
            _ => vec![],
        },
        LoginState::Exchanging => {
            if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                update_login(state, LoginEvent::LoginCancelled)
            } else {
                vec![]
            }
        }
    }
}

fn handle_login_result(state: &mut TuiState, result: Result<(), String>) {
    state.login_exchange_rx = None;
    match result {
        Ok(()) => {
            let _ = update_login(state, LoginEvent::LoginSucceeded);
        }
        Err(msg) => {
            let _ = update_login(state, LoginEvent::LoginFailed { message: msg });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scroll_to_top() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);
        state.scroll_mode = ScrollMode::FollowLatest;

        scroll_to_top(&mut state);

        assert!(matches!(
            state.scroll_mode,
            ScrollMode::Anchored { offset: 0 }
        ));
    }

    #[test]
    fn test_scroll_to_bottom() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);
        state.scroll_mode = ScrollMode::Anchored { offset: 10 };

        scroll_to_bottom(&mut state);

        assert!(matches!(state.scroll_mode, ScrollMode::FollowLatest));
    }

    #[test]
    fn test_execute_quit_when_idle() {
        let config = crate::config::Config::default();
        let mut state = TuiState::new(config, std::path::PathBuf::new(), None, None);

        let effects = execute_quit(&mut state);

        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], UiEffect::Quit));
    }
}
