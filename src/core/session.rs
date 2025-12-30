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

use super::agent::AgentEventRx;
use crate::config::paths::sessions_dir;

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

    /// Thinking event: extended thinking block from the assistant.
    Thinking {
        content: String,
        /// Cryptographic signature from the API.
        /// None/missing if thinking was aborted (will be converted to text block on replay).
        #[serde(default)]
        signature: Option<String>,
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

    /// Creates a new thinking event.
    pub fn thinking(content: impl Into<String>, signature: Option<String>) -> Self {
        Self::Thinking {
            content: content.into(),
            signature,
            ts: chrono_timestamp(),
        }
    }

    /// Converts an `EngineEvent` to a `SessionEvent` if applicable.
    ///
    /// Not all agent events are persisted. This returns `None` for events
    /// that don't need to be saved (e.g., `AssistantDelta`, `ToolStarted`).
    ///
    /// Note: `AssistantComplete` and user messages are handled separately by the
    /// chat/agent modules since they have additional context.
    pub fn from_agent(event: &crate::core::events::AgentEvent) -> Option<Self> {
        use crate::core::events::AgentEvent;

        match event {
            // ToolInputReady has the complete input (ToolRequested is emitted early with empty input)
            AgentEvent::ToolInputReady { id, name, input } => {
                Some(Self::tool_use(id.clone(), name.clone(), input.clone()))
            }
            AgentEvent::ToolFinished { id, result } => {
                let output = serde_json::to_value(result).unwrap_or_default();
                Some(Self::tool_result(id.clone(), output, result.is_ok()))
            }
            AgentEvent::Interrupted => Some(Self::interrupted()),
            AgentEvent::ThinkingComplete { text, signature } => {
                Some(Self::thinking(text.clone(), Some(signature.clone())))
            }
            // These are not persisted via this path:
            // - AssistantDelta: streamed chunks, not final
            // - AssistantComplete: handled by caller with full context
            // - ThinkingDelta: streamed chunks, not final
            // - ToolRequested: early notification with empty input (ToolInputReady has full input)
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
    /// Returns the path to the session file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Guard to prevent session creation in tests without proper isolation.
    ///
    /// # Panics
    /// - In unit tests (`#[cfg(test)]`): panics if `ZDX_HOME` is not set
    /// - At runtime: panics if `ZDX_BLOCK_SESSION_WRITES=1` is set
    ///
    /// This ensures tests don't pollute the user's home directory with session files.
    fn guard_session_creation() {
        // Compile-time guard for unit tests
        #[cfg(test)]
        if std::env::var("ZDX_HOME").is_err() {
            panic!(
                "Tests must set ZDX_HOME to a temp directory!\n\
                 Session would be created in user's home directory.\n\
                 Use `setup_temp_zdx_home()` or set ZDX_HOME env var."
            );
        }

        // Runtime guard for integration tests
        #[cfg(not(test))]
        if std::env::var("ZDX_BLOCK_SESSION_WRITES").is_ok_and(|v| v == "1") {
            panic!(
                "ZDX_BLOCK_SESSION_WRITES=1 but trying to create a session!\n\
                 Use --no-save flag or set ZDX_HOME to a temp directory."
            );
        }
    }

    /// Creates a new session with a generated ID.
    ///
    /// # Panics
    /// In tests, panics if `ZDX_HOME` is not set (to prevent polluting user's home).
    pub fn new() -> Result<Self> {
        Self::guard_session_creation();

        let id = generate_session_id();
        let dir = sessions_dir();
        fs::create_dir_all(&dir).context("Failed to create sessions directory")?;

        let path = dir.join(format!("{}.jsonl", id));
        let is_new = !path.exists();

        Ok(Self { id, path, is_new })
    }

    /// Creates or opens a session with a specific ID.
    ///
    /// # Panics
    /// In tests, panics if `ZDX_HOME` is not set (to prevent polluting user's home).
    pub fn with_id(id: String) -> Result<Self> {
        Self::guard_session_creation();

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
/// let (tx, rx) = agent::create_event_channel();
/// let persist_handle = spawn_persist_task(session, rx);
///
/// // ... send events to tx ...
/// drop(tx); // Close channel
///
/// persist_handle.await.unwrap(); // Wait for persistence to finish
/// ```
pub fn spawn_persist_task(mut session: Session, mut rx: AgentEventRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Some(session_event) = SessionEvent::from_agent(&event) {
                // Best-effort persistence - log errors but don't panic
                if let Err(e) = session.append(&session_event) {
                    eprintln!("Warning: Failed to persist session event: {}", e);
                }
            }
        }
    })
}

/// Summary information about a saved session.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub modified: Option<SystemTime>,
}

/// Lists all saved sessions.
///
/// Returns a vector of SessionSummary sorted by modification time (newest first).
pub fn list_sessions() -> Result<Vec<SessionSummary>> {
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

            sessions.push(SessionSummary { id, modified });
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
    let events = load_session(id)?;
    Ok(events_to_messages(events))
}

/// Converts session events to chat messages for API replay.
pub fn events_to_messages(
    events: Vec<SessionEvent>,
) -> Vec<crate::providers::anthropic::ChatMessage> {
    use crate::providers::anthropic::{ChatContentBlock, ChatMessage, MessageContent};

    let mut messages: Vec<ChatMessage> = Vec::new();

    // Track pending assistant content to group into single messages
    // (thinking blocks + tool uses belong to the same assistant turn)
    let mut pending_thinking: Vec<(String, Option<String>)> = Vec::new(); // (content, signature)
    let mut pending_tool_uses: Vec<(String, String, Value)> = Vec::new(); // (id, name, input)
    let mut pending_tool_results: Vec<crate::tools::ToolResult> = Vec::new();

    /// Flushes pending assistant content (thinking + tool_use) into messages.
    fn flush_pending_assistant(
        messages: &mut Vec<ChatMessage>,
        pending_thinking: &mut Vec<(String, Option<String>)>,
        pending_tool_uses: &mut Vec<(String, String, Value)>,
        pending_tool_results: &mut Vec<crate::tools::ToolResult>,
    ) {
        if pending_thinking.is_empty() && pending_tool_uses.is_empty() {
            return;
        }

        let mut blocks: Vec<ChatContentBlock> = Vec::new();

        // Add thinking blocks first
        for (content, signature) in std::mem::take(pending_thinking) {
            blocks.push(ChatContentBlock::Thinking {
                thinking: content,
                // Use empty string if signature is missing (aborted thinking)
                // The API serialization will convert this to a text block
                signature: signature.unwrap_or_default(),
            });
        }

        // Add tool_use blocks
        for (id, name, input) in std::mem::take(pending_tool_uses) {
            blocks.push(ChatContentBlock::ToolUse { id, name, input });
        }

        if !blocks.is_empty() {
            messages.push(ChatMessage::assistant_blocks(blocks));
        }

        // Add tool results as a separate user message
        if !pending_tool_results.is_empty() {
            messages.push(ChatMessage::tool_results(std::mem::take(
                pending_tool_results,
            )));
        }
    }

    for event in events {
        match event {
            SessionEvent::Meta { .. } => {
                // Skip meta events
            }
            SessionEvent::Message { role, text, .. } => {
                // Flush any pending assistant content before adding a new message
                flush_pending_assistant(
                    &mut messages,
                    &mut pending_thinking,
                    &mut pending_tool_uses,
                    &mut pending_tool_results,
                );

                // If this is an assistant message and we have it as a text message,
                // convert it to blocks format to allow appending thinking
                messages.push(ChatMessage {
                    role,
                    content: MessageContent::Text(text),
                });
            }
            SessionEvent::Thinking {
                content, signature, ..
            } => {
                pending_thinking.push((content, signature));
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
                    content: crate::tools::ToolResultContent::Text(
                        serde_json::to_string(&output).unwrap_or_default(),
                    ),
                    is_error: !ok,
                });
            }
            SessionEvent::Interrupted { .. } => {
                // Skip interrupted events when loading for API
            }
        }
    }

    // Flush any remaining pending assistant content
    flush_pending_assistant(
        &mut messages,
        &mut pending_thinking,
        &mut pending_tool_uses,
        &mut pending_tool_results,
    );

    messages
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
            SessionEvent::Thinking { content, .. } => {
                output.push_str("### Thinking\n");
                // Truncate long thinking content for display
                if content.len() > 500 {
                    output.push_str(&content[..500]);
                    output.push_str("...");
                } else {
                    output.push_str(content);
                }
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
pub struct SessionPersistenceOptions {
    /// Append to an existing session by ID.
    pub session_id: Option<String>,
    /// Do not save the session.
    pub no_save: bool,
}

impl SessionPersistenceOptions {
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
    fn test_events_to_messages_with_tools() {
        // Test the conversion logic directly without env var dependency
        let events = vec![
            SessionEvent::user_message("list files"),
            SessionEvent::tool_use("t1", "bash", json!({"command": "ls"})),
            SessionEvent::tool_result("t1", json!({"stdout": "file.txt\n"}), true),
            SessionEvent::assistant_message("Found file.txt"),
        ];

        let messages = events_to_messages(events);

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
    fn test_session_persistence_options_no_save() {
        let opts = SessionPersistenceOptions {
            no_save: true,
            ..Default::default()
        };
        assert!(opts.resolve().unwrap().is_none());
    }

    #[test]
    fn test_session_persistence_options_with_id() {
        let _temp = setup_temp_zdx_home();

        let id = unique_session_id("existing");
        let opts = SessionPersistenceOptions {
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

    #[test]
    fn test_thinking_event_serialization() {
        // Test thinking with signature
        let thinking = SessionEvent::thinking("Let me analyze this...", Some("sig123".to_string()));
        let json = serde_json::to_string(&thinking).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        assert!(json.contains("\"content\":\"Let me analyze this...\""));
        assert!(json.contains("\"signature\":\"sig123\""));

        // Test thinking without signature (aborted)
        let aborted = SessionEvent::thinking("Partial thought...", None);
        let json = serde_json::to_string(&aborted).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        assert!(json.contains("\"signature\":null"));
    }

    #[test]
    fn test_thinking_event_deserialization() {
        // Test deserialization with signature
        let json = r#"{"type":"thinking","content":"Deep analysis","signature":"abc123","ts":"2024-01-01T00:00:00Z"}"#;
        let event: SessionEvent = serde_json::from_str(json).unwrap();
        match event {
            SessionEvent::Thinking {
                content, signature, ..
            } => {
                assert_eq!(content, "Deep analysis");
                assert_eq!(signature, Some("abc123".to_string()));
            }
            _ => panic!("Expected Thinking event"),
        }

        // Test deserialization without signature (backward compat)
        let json_no_sig = r#"{"type":"thinking","content":"Partial","ts":"2024-01-01T00:00:00Z"}"#;
        let event: SessionEvent = serde_json::from_str(json_no_sig).unwrap();
        match event {
            SessionEvent::Thinking { signature, .. } => {
                assert_eq!(signature, None);
            }
            _ => panic!("Expected Thinking event"),
        }
    }

    #[test]
    fn test_events_to_messages_with_thinking() {
        use crate::providers::anthropic::{ChatContentBlock, MessageContent};

        let events = vec![
            SessionEvent::user_message("solve this problem"),
            SessionEvent::thinking(
                "Let me think about this...".to_string(),
                Some("sig123".to_string()),
            ),
            SessionEvent::tool_use("t1", "bash", json!({"command": "echo test"})),
            SessionEvent::tool_result("t1", json!({"stdout": "test\n"}), true),
            SessionEvent::assistant_message("Done!"),
        ];

        let messages = events_to_messages(events);

        // user + assistant(thinking + tool_use) + tool_results + assistant = 4
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");

        // Second message should be assistant with thinking + tool_use blocks
        assert_eq!(messages[1].role, "assistant");
        if let MessageContent::Blocks(blocks) = &messages[1].content {
            assert_eq!(blocks.len(), 2);
            assert!(
                matches!(&blocks[0], ChatContentBlock::Thinking { thinking, signature }
                    if thinking == "Let me think about this..." && signature == "sig123"
                )
            );
            assert!(matches!(&blocks[1], ChatContentBlock::ToolUse { .. }));
        } else {
            panic!("Expected Blocks content");
        }
    }

    #[test]
    fn test_session_thinking_roundtrip() {
        let _temp = setup_temp_zdx_home();

        let mut session = Session::with_id(unique_session_id("thinking-roundtrip")).unwrap();
        session
            .append(&SessionEvent::user_message("explain"))
            .unwrap();
        session
            .append(&SessionEvent::thinking(
                "Deep analysis here...",
                Some("signature456".to_string()),
            ))
            .unwrap();
        session
            .append(&SessionEvent::assistant_message("Here's my answer"))
            .unwrap();

        let events = session.read_events().unwrap();
        // meta + user + thinking + assistant = 4 events
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], SessionEvent::Meta { .. }));
        assert!(matches!(events[1], SessionEvent::Message { ref role, .. } if role == "user"));
        assert!(
            matches!(events[2], SessionEvent::Thinking { ref content, ref signature, .. }
                if content == "Deep analysis here..." && signature == &Some("signature456".to_string())
            )
        );
        assert!(matches!(events[3], SessionEvent::Message { ref role, .. } if role == "assistant"));
    }

    #[test]
    fn test_format_transcript_with_thinking() {
        let events = vec![
            SessionEvent::meta(),
            SessionEvent::user_message("explain this"),
            SessionEvent::thinking(
                "Analyzing the request...".to_string(),
                Some("sig".to_string()),
            ),
            SessionEvent::assistant_message("Here's my explanation."),
        ];

        let transcript = format_transcript(&events);
        assert!(transcript.contains("### Thinking"));
        assert!(transcript.contains("Analyzing the request..."));
    }
}
