//! Session persistence for ZDX.
//!
//! Each session is stored as a JSONL file where each line is a JSON object
//! representing a message event.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::paths::sessions_dir;

/// A session event representing a message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub role: String,
    pub text: String,
    pub ts: String,
}

impl SessionEvent {
    /// Creates a new user message event.
    pub fn user_message(text: impl Into<String>) -> Self {
        Self {
            event_type: "message".to_string(),
            role: "user".to_string(),
            text: text.into(),
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new assistant message event.
    pub fn assistant_message(text: impl Into<String>) -> Self {
        Self {
            event_type: "message".to_string(),
            role: "assistant".to_string(),
            text: text.into(),
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new interrupted event.
    pub fn interrupted() -> Self {
        Self {
            event_type: "interrupted".to_string(),
            role: "system".to_string(),
            text: "Interrupted".to_string(),
            ts: chrono_timestamp(),
        }
    }
}

/// Returns an ISO 8601 timestamp string.
fn chrono_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Manages a session file.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    path: PathBuf,
}

impl Session {
    /// Creates a new session with a generated ID.
    pub fn new() -> Result<Self> {
        let id = generate_session_id();
        Self::with_id(id)
    }

    /// Creates or opens a session with a specific ID.
    pub fn with_id(id: String) -> Result<Self> {
        let dir = sessions_dir();
        fs::create_dir_all(&dir).context("Failed to create sessions directory")?;

        let path = dir.join(format!("{}.jsonl", id));

        Ok(Self { id, path })
    }

    /// Appends an event to the session file.
    pub fn append(&self, event: &SessionEvent) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .context("Failed to open session file")?;

        let json = serde_json::to_string(event).context("Failed to serialize event")?;
        writeln!(file, "{}", json).context("Failed to write to session file")?;

        Ok(())
    }

    /// Reads all events from the session file.
    #[allow(dead_code)] // Used in tests and will be used for resume feature
    pub fn read_events(&self) -> Result<Vec<SessionEvent>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&self.path).context("Failed to open session file")?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for line in reader.lines() {
            let line = line.context("Failed to read line")?;
            if line.trim().is_empty() {
                continue;
            }
            let event: SessionEvent =
                serde_json::from_str(&line).context("Failed to parse session event")?;
            events.push(event);
        }

        Ok(events)
    }

    /// Returns the path to the session file.
    #[allow(dead_code)] // Used in tests
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

/// Generates a unique session ID using UUID v4.
fn generate_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Information about a saved session.
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
pub fn load_session_as_messages(id: &str) -> Result<Vec<crate::providers::anthropic::ChatMessage>> {
    use crate::providers::anthropic::MessageContent;
    let events = load_session(id)?;
    Ok(events
        .into_iter()
        .filter(|e| e.event_type == "message")
        .map(|e| crate::providers::anthropic::ChatMessage {
            role: e.role,
            content: MessageContent::Text(e.text),
        })
        .collect())
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
        let role_label = match event.role.as_str() {
            "user" => "You",
            "assistant" => "Assistant",
            _ => &event.role,
        };

        output.push_str(&format!("### {}\n", role_label));
        output.push_str(&event.text);
        output.push_str("\n\n");
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
    fn test_session_creates_file() {
        let _temp = setup_temp_zdx_home();

        let session = Session::with_id(unique_session_id("creates")).unwrap();
        let event = SessionEvent::user_message("hello");
        session.append(&event).unwrap();

        assert!(session.path().exists());
    }

    #[test]
    fn test_session_appends_jsonl() {
        let _temp = setup_temp_zdx_home();

        let session = Session::with_id(unique_session_id("appends")).unwrap();
        session
            .append(&SessionEvent::user_message("hello"))
            .unwrap();
        session
            .append(&SessionEvent::assistant_message("hi there"))
            .unwrap();

        let events = session.read_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].role, "user");
        assert_eq!(events[0].text, "hello");
        assert_eq!(events[1].role, "assistant");
        assert_eq!(events[1].text, "hi there");
    }

    #[test]
    fn test_session_event_interrupted_serializes() {
        let event = SessionEvent::interrupted();
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"interrupted\""));
        assert!(json.contains("\"role\":\"system\""));
    }

    #[test]
    fn test_session_with_id() {
        let _temp = setup_temp_zdx_home();

        let id = unique_session_id("my-session");
        let session = Session::with_id(id.clone()).unwrap();
        session.append(&SessionEvent::user_message("test")).unwrap();

        assert!(
            session
                .path()
                .to_string_lossy()
                .contains(&format!("{}.jsonl", id))
        );
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
}
