//! Session persistence for ZDX.
//!
//! Each session is stored as a JSONL file where each line is a JSON object
//! representing an event. Sessions use schema versioning (§8 of SPEC).
//!
//! ## Schema v1 Format
//!
//! ```jsonl
//! { "type": "meta", "schema_version": 1, "ts": "2025-12-17T03:21:09Z" }
//! { "type": "message", "role": "user", "text": "...", "ts": "..." }
//! { "type": "tool_use", "id": "...", "name": "read", "input": { "path": "..." }, "ts": "..." }
//! { "type": "tool_result", "tool_use_id": "...", "output": { ... }, "ok": true, "ts": "..." }
//! { "type": "message", "role": "assistant", "text": "...", "ts": "..." }
//! ```

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::engine::EventRx;
use crate::paths::sessions_dir;

/// Current schema version for new sessions.
pub const SCHEMA_VERSION: u32 = 1;

/// A session event (polymorphic, tag-based).
///
/// This enum represents all event types that can be persisted in a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Meta event: first line of a v1+ session file.
    Meta { schema_version: u32, ts: String },

    /// Message event: user or assistant text.
    Message {
        role: String,
        text: String,
        ts: String,
    },

    /// Tool use event: model requested a tool call.
    ToolUse {
        id: String,
        name: String,
        input: Value,
        ts: String,
    },

    /// Tool result event: output from tool execution.
    ToolResult {
        tool_use_id: String,
        output: Value,
        ok: bool,
        ts: String,
    },

    /// Interrupted event: session was interrupted by user.
    Interrupted {
        #[serde(default = "default_interrupted_role")]
        role: String,
        #[serde(default = "default_interrupted_text")]
        text: String,
        ts: String,
    },
}

impl SessionEvent {
    /// Creates a new meta event with the current schema version.
    pub fn meta() -> Self {
        Self::Meta {
            schema_version: SCHEMA_VERSION,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new user message event.
    pub fn user_message(text: impl Into<String>) -> Self {
        Self::Message {
            role: "user".to_string(),
            text: text.into(),
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new assistant message event.
    pub fn assistant_message(text: impl Into<String>) -> Self {
        Self::Message {
            role: "assistant".to_string(),
            text: text.into(),
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new tool use event.
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new tool result event.
    pub fn tool_result(tool_use_id: impl Into<String>, output: Value, ok: bool) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            output,
            ok,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new interrupted event.
    pub fn interrupted() -> Self {
        Self::Interrupted {
            role: default_interrupted_role(),
            text: default_interrupted_text(),
            ts: chrono_timestamp(),
        }
    }

    /// Converts an `EngineEvent` to a `SessionEvent` if applicable.
    ///
    /// Not all engine events are persisted. This returns `None` for events
    /// that don't need to be saved (e.g., `AssistantDelta`, `ToolStarted`).
    ///
    /// Note: `AssistantFinal` and user messages are handled separately by the
    /// chat/agent modules since they have additional context.
    pub fn from_engine(event: &crate::events::EngineEvent) -> Option<Self> {
        use crate::events::EngineEvent;

        match event {
            EngineEvent::ToolRequested { id, name, input } => {
                Some(Self::tool_use(id.clone(), name.clone(), input.clone()))
            }
            EngineEvent::ToolFinished { id, result } => {
                let output = serde_json::to_value(result).unwrap_or_default();
                Some(Self::tool_result(id.clone(), output, result.is_ok()))
            }
            EngineEvent::Interrupted => Some(Self::interrupted()),
            // These are not persisted via this path:
            // - AssistantDelta: streamed chunks, not final
            // - AssistantFinal: handled by caller with full context
            // - ToolStarted: UI-only, not persisted
            // - Error: not persisted (may be in future)
            _ => None,
        }
    }
}

fn default_interrupted_role() -> String {
    "system".to_string()
}

fn default_interrupted_text() -> String {
    "Interrupted".to_string()
}

/// Returns an RFC3339 UTC timestamp string.
fn chrono_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Manages a session file.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    path: PathBuf,
    /// Whether this is a new session (needs meta event written).
    is_new: bool,
}

impl Session {
    /// Creates a new session with a generated ID.
    pub fn new() -> Result<Self> {
        let id = generate_session_id();
        let dir = sessions_dir();
        fs::create_dir_all(&dir).context("Failed to create sessions directory")?;

        let path = dir.join(format!("{}.jsonl", id));
        let is_new = !path.exists();

        Ok(Self { id, path, is_new })
    }

    /// Creates or opens a session with a specific ID.
    pub fn with_id(id: String) -> Result<Self> {
        let dir = sessions_dir();
        fs::create_dir_all(&dir).context("Failed to create sessions directory")?;

        let path = dir.join(format!("{}.jsonl", id));
        let is_new = !path.exists();

        Ok(Self { id, path, is_new })
    }

    /// Ensures the meta event is written for new sessions.
    fn ensure_meta(&mut self) -> Result<()> {
        if self.is_new {
            self.append_raw(&SessionEvent::meta())?;
            self.is_new = false;
        }
        Ok(())
    }

    /// Appends an event to the session file (internal, no meta check).
    fn append_raw(&self, event: &SessionEvent) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .context("Failed to open session file")?;

        let json = serde_json::to_string(event).context("Failed to serialize event")?;
        writeln!(file, "{}", json).context("Failed to write to session file")?;

        Ok(())
    }

    /// Appends an event to the session file.
    ///
    /// For new sessions, automatically writes the meta event first.
    pub fn append(&mut self, event: &SessionEvent) -> Result<()> {
        // Don't write meta before another meta
        if !matches!(event, SessionEvent::Meta { .. }) {
            self.ensure_meta()?;
        }
        self.append_raw(event)
    }

    /// Reads all events from the session file.
    pub fn read_events(&self) -> Result<Vec<SessionEvent>> {
        read_session_events(&self.path)
    }

}

/// Reads session events from a file path, with backward compatibility.
fn read_session_events(path: &PathBuf) -> Result<Vec<SessionEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path).context("Failed to open session file")?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.context("Failed to read line")?;
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(event) = serde_json::from_str::<SessionEvent>(&line) {
            events.push(event);
        }
        // Skip unparseable lines (best-effort)
    }

    Ok(events)
}

/// Generates a unique session ID using UUID v4.
fn generate_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Information about a saved session.
/// Spawns a session persistence task that consumes events from a channel.
///
/// The task owns the `Session` and persists relevant events until the channel closes.
/// Returns a `JoinHandle` that resolves when all events have been persisted.
///
/// Only tool-related and interrupt events are persisted via this task.
/// User and assistant messages are handled separately by the chat/agent modules.
///
/// # Example
///
/// ```ignore
/// let session = Session::new()?;
/// let (tx, rx) = engine::create_event_channel();
/// let persist_handle = spawn_persist_task(session, rx);
///
/// // ... send events to tx ...
/// drop(tx); // Close channel
///
/// persist_handle.await.unwrap(); // Wait for persistence to finish
/// ```
pub fn spawn_persist_task(mut session: Session, mut rx: EventRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Some(session_event) = SessionEvent::from_engine(&event) {
                // Best-effort persistence - log errors but don't panic
                if let Err(e) = session.append(&session_event) {
                    eprintln!("Warning: Failed to persist session event: {}", e);
                }
            }
        }
    })
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub modified: Option<SystemTime>,
}

/// Lists all saved sessions.
///
/// Returns a vector of SessionInfo sorted by modification time (newest first).
pub fn list_sessions() -> Result<Vec<SessionInfo>> {
    let dir = sessions_dir();

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();

    for entry in fs::read_dir(&dir).context("Failed to read sessions directory")? {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        // Only process .jsonl files
        if path.extension().is_some_and(|ext| ext == "jsonl")
            && let Some(stem) = path.file_stem()
        {
            let id = stem.to_string_lossy().to_string();
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());

            sessions.push(SessionInfo { id, modified });
        }
    }

    // Sort by modification time (newest first)
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));

    Ok(sessions)
}

/// Loads and returns the events from a session by ID.
pub fn load_session(id: &str) -> Result<Vec<SessionEvent>> {
    let session = Session::with_id(id.to_string())?;
    session.read_events()
}

/// Returns the ID of the most recently modified session.
///
/// Returns None if no sessions exist.
pub fn latest_session_id() -> Result<Option<String>> {
    let sessions = list_sessions()?;
    Ok(sessions.into_iter().next().map(|s| s.id))
}

/// Loads session events and converts them to ChatMessages for API use.
///
/// Reconstructs the full conversation including tool use/result pairs.
pub fn load_session_as_messages(id: &str) -> Result<Vec<crate::providers::anthropic::ChatMessage>> {
    use crate::providers::anthropic::{ChatContentBlock, ChatMessage, MessageContent};

    let events = load_session(id)?;
    let mut messages: Vec<ChatMessage> = Vec::new();

    // Track pending tool uses to group with results
    let mut pending_tool_uses: Vec<(String, String, Value)> = Vec::new(); // (id, name, input)
    let mut pending_tool_results: Vec<crate::tools::ToolResult> = Vec::new();

    for event in events {
        match event {
            SessionEvent::Meta { .. } => {
                // Skip meta events
            }
            SessionEvent::Message { role, text, .. } => {
                // Flush any pending tool uses/results before adding a message
                if !pending_tool_uses.is_empty() {
                    // Add assistant message with tool_use blocks
                    let blocks: Vec<ChatContentBlock> = std::mem::take(&mut pending_tool_uses)
                        .into_iter()
                        .map(|(id, name, input)| ChatContentBlock::ToolUse { id, name, input })
                        .collect();
                    messages.push(ChatMessage::assistant_blocks(blocks));

                    // Add tool results
                    if !pending_tool_results.is_empty() {
                        messages.push(ChatMessage::tool_results(std::mem::take(
                            &mut pending_tool_results,
                        )));
                    }
                }

                messages.push(ChatMessage {
                    role,
                    content: MessageContent::Text(text),
                });
            }
            SessionEvent::ToolUse {
                id, name, input, ..
            } => {
                pending_tool_uses.push((id, name, input));
            }
            SessionEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                ..
            } => {
                pending_tool_results.push(crate::tools::ToolResult {
                    tool_use_id,
                    content: serde_json::to_string(&output).unwrap_or_default(),
                    is_error: !ok,
                });
            }
            SessionEvent::Interrupted { .. } => {
                // Skip interrupted events when loading for API
            }
        }
    }

    // Flush any remaining pending tool uses/results
    if !pending_tool_uses.is_empty() {
        let blocks: Vec<ChatContentBlock> = std::mem::take(&mut pending_tool_uses)
            .into_iter()
            .map(|(id, name, input)| ChatContentBlock::ToolUse { id, name, input })
            .collect();
        messages.push(ChatMessage::assistant_blocks(blocks));

        if !pending_tool_results.is_empty() {
            messages.push(ChatMessage::tool_results(std::mem::take(
                &mut pending_tool_results,
            )));
        }
    }

    Ok(messages)
}

/// Formats a SystemTime as a simple date/time string (YYYY-MM-DD HH:MM).
pub fn format_timestamp(time: SystemTime) -> Option<String> {
    let datetime: DateTime<Utc> = time.into();
    Some(datetime.format("%Y-%m-%d %H:%M").to_string())
}

/// Formats a session transcript in a human-readable format.
pub fn format_transcript(events: &[SessionEvent]) -> String {
    let mut output = String::new();

    for event in events {
        match event {
            SessionEvent::Meta { schema_version, .. } => {
                output.push_str(&format!("### Session (schema v{})\n\n", schema_version));
            }
            SessionEvent::Message { role, text, .. } => {
                let role_label = match role.as_str() {
                    "user" => "You",
                    "assistant" => "Assistant",
                    _ => role,
                };
                output.push_str(&format!("### {}\n", role_label));
                output.push_str(text);
                output.push_str("\n\n");
            }
            SessionEvent::ToolUse { name, input, .. } => {
                output.push_str(&format!("### Tool: {}\n", name));
                output.push_str(&format!(
                    "```json\n{}\n```\n\n",
                    serde_json::to_string_pretty(input).unwrap_or_default()
                ));
            }
            SessionEvent::ToolResult {
                ok, output: out, ..
            } => {
                let status = if *ok { "✓" } else { "✗" };
                output.push_str(&format!("### Result {}\n", status));
                // Truncate long outputs for display
                let out_str = serde_json::to_string_pretty(out).unwrap_or_default();
                if out_str.len() > 500 {
                    output.push_str(&format!("```json\n{}...\n```\n\n", &out_str[..500]));
                } else {
                    output.push_str(&format!("```json\n{}\n```\n\n", out_str));
                }
            }
            SessionEvent::Interrupted { .. } => {
                output.push_str("### Interrupted\n\n");
            }
        }
    }

    output.trim_end().to_string()
}

/// Session options for CLI commands.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    /// Append to an existing session by ID.
    pub session_id: Option<String>,
    /// Do not save the session.
    pub no_save: bool,
}

impl SessionOptions {
    /// Resolves session options into an optional Session.
    ///
    /// Returns None if no_save is true.
    /// Returns existing session if session_id is provided.
    /// Returns new session otherwise.
    pub fn resolve(&self) -> Result<Option<Session>> {
        if self.no_save {
            return Ok(None);
        }

        if let Some(ref id) = self.session_id {
            return Ok(Some(Session::with_id(id.clone())?));
        }

        Ok(Some(Session::new()?))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn setup_temp_zdx_home() -> TempDir {
        let temp = TempDir::new().unwrap();
        // SAFETY: Tests run serially, and we control the environment variable access
        unsafe {
            std::env::set_var("ZDX_HOME", temp.path());
        }
        temp
    }

    fn unique_session_id(prefix: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        format!("{}-{}", prefix, nanos)
    }

    #[test]
    fn test_session_creates_file_with_meta() {
        let _temp = setup_temp_zdx_home();

        let mut session = Session::with_id(unique_session_id("creates-meta")).unwrap();
        session
            .append(&SessionEvent::user_message("hello"))
            .unwrap();

        // Read raw file content to verify meta is first
        let content = fs::read_to_string(&session.path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.len() >= 2);
        assert!(lines[0].contains("\"type\":\"meta\""));
        assert!(lines[0].contains("\"schema_version\":1"));
    }

    #[test]
    fn test_session_appends_jsonl_with_tool_events() {
        let _temp = setup_temp_zdx_home();

        let mut session = Session::with_id(unique_session_id("tool-events")).unwrap();
        session
            .append(&SessionEvent::user_message("read main.rs"))
            .unwrap();
        session
            .append(&SessionEvent::tool_use(
                "tool-1",
                "read",
                json!({"path": "main.rs"}),
            ))
            .unwrap();
        session
            .append(&SessionEvent::tool_result(
                "tool-1",
                json!({"ok": true, "data": {"content": "fn main() {}"}}),
                true,
            ))
            .unwrap();
        session
            .append(&SessionEvent::assistant_message("Here's the file"))
            .unwrap();

        let events = session.read_events().unwrap();
        // meta + user + tool_use + tool_result + assistant = 5 events
        assert_eq!(events.len(), 5);
        assert!(matches!(events[0], SessionEvent::Meta { .. }));
        assert!(matches!(events[1], SessionEvent::Message { ref role, .. } if role == "user"));
        assert!(matches!(events[2], SessionEvent::ToolUse { ref name, .. } if name == "read"));
        assert!(matches!(
            events[3],
            SessionEvent::ToolResult { ok: true, .. }
        ));
        assert!(matches!(events[4], SessionEvent::Message { ref role, .. } if role == "assistant"));
    }

    #[test]
    fn test_session_event_serialization() {
        let meta = SessionEvent::meta();
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"type\":\"meta\""));
        assert!(json.contains("\"schema_version\":1"));

        let tool_use = SessionEvent::tool_use("t1", "bash", json!({"command": "ls"}));
        let json = serde_json::to_string(&tool_use).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"name\":\"bash\""));

        let tool_result = SessionEvent::tool_result("t1", json!({"stdout": "file.txt"}), true);
        let json = serde_json::to_string(&tool_result).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        assert!(json.contains("\"ok\":true"));

        let interrupted = SessionEvent::interrupted();
        let json = serde_json::to_string(&interrupted).unwrap();
        assert!(json.contains("\"type\":\"interrupted\""));
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"text\":\"Interrupted\""));
    }

    #[test]
    fn test_load_session_as_messages_with_tools() {
        let temp = setup_temp_zdx_home();

        let id = unique_session_id("messages-tools");
        let mut session = Session::with_id(id.clone()).unwrap();
        session
            .append(&SessionEvent::user_message("list files"))
            .unwrap();
        session
            .append(&SessionEvent::tool_use(
                "t1",
                "bash",
                json!({"command": "ls"}),
            ))
            .unwrap();
        session
            .append(&SessionEvent::tool_result(
                "t1",
                json!({"stdout": "file.txt\n"}),
                true,
            ))
            .unwrap();
        session
            .append(&SessionEvent::assistant_message("Found file.txt"))
            .unwrap();

        // Re-set ZDX_HOME to ensure path consistency (guards against parallel test interference)
        unsafe { std::env::set_var("ZDX_HOME", temp.path()) };

        let messages = load_session_as_messages(&id).unwrap();

        // user message + assistant with tool_use block + tool_results + assistant message = 4
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");
        // Second message should be assistant with tool_use blocks
        assert_eq!(messages[1].role, "assistant");
        // Third message should be tool results (role "user")
        assert_eq!(messages[2].role, "user");
        // Fourth is final assistant message
        assert_eq!(messages[3].role, "assistant");
    }

    #[test]
    fn test_session_options_no_save() {
        let opts = SessionOptions {
            no_save: true,
            ..Default::default()
        };
        assert!(opts.resolve().unwrap().is_none());
    }

    #[test]
    fn test_session_options_with_id() {
        let _temp = setup_temp_zdx_home();

        let id = unique_session_id("existing");
        let opts = SessionOptions {
            session_id: Some(id.clone()),
            ..Default::default()
        };
        let session = opts.resolve().unwrap().unwrap();
        assert_eq!(session.id, id);
    }

    #[test]
    fn test_format_transcript_with_tools() {
        let events = vec![
            SessionEvent::meta(),
            SessionEvent::user_message("read main.rs"),
            SessionEvent::tool_use("t1", "read", json!({"path": "main.rs"})),
            SessionEvent::tool_result(
                "t1",
                json!({"ok": true, "data": {"content": "fn main() {}"}}),
                true,
            ),
            SessionEvent::assistant_message("Here's the file content."),
        ];

        let transcript = format_transcript(&events);
        assert!(transcript.contains("Session (schema v1)"));
        assert!(transcript.contains("### You"));
        assert!(transcript.contains("### Tool: read"));
        assert!(transcript.contains("### Result ✓"));
        assert!(transcript.contains("### Assistant"));
    }
}
