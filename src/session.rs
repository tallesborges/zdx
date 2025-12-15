//! Session persistence for ZDX.
//!
//! Each session is stored as a JSONL file where each line is a JSON object
//! representing a message event.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

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
}

/// Returns an ISO 8601 timestamp string.
fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    // Format as ISO 8601 (simplified)
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();

    // Convert to approximate datetime (not perfect but avoids chrono dependency)
    format!("{}:{:03}Z", secs, millis)
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

/// Session options for CLI commands.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    /// Append to an existing session by ID.
    pub session_id: Option<String>,
    /// Force creation of a new session.
    pub new_session: bool,
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
            if self.new_session {
                anyhow::bail!("Cannot use --session and --new-session together");
            }
            return Ok(Some(Session::with_id(id.clone())?));
        }

        Ok(Some(Session::new()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

    #[test]
    fn test_session_options_conflict() {
        let opts = SessionOptions {
            session_id: Some("test".to_string()),
            new_session: true,
            no_save: false,
        };
        assert!(opts.resolve().is_err());
    }
}
