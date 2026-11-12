//! Steering: messages added to a running turn at a safe point.
//!
//! The agent loop takes pending steers only after a whole tool round and
//! before the next model request (SPEC §Steering). A steer never stops a
//! running tool and never changes a request already in flight.
//!
//! Two sources feed a run:
//! - [`SteerInbox`]: in memory, for runs in the owner's process (TUI, bot
//!   agent topics, orchestrator turns in the service).
//! - [`SteerFile`]: an append-only file a worker child reads; the worker
//!   manager is its only writer ([`append_steer_line`]).
//!
//! The engine closes the source before it reports the turn's end, so an owner
//! that reacts to `TurnFinished` already sees it closed. Items still pending
//! then are unread: the owner collects them with [`SteerInbox::drain_unread`]
//! and decides where they go (normal queue, given back, or dropped).

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// A message sent by the user of the surface.
pub const SOURCE_USER: &str = "user";
/// A `[worker update]` delivered into a running orchestrator turn.
pub const SOURCE_WORKER_UPDATE: &str = "worker_update";
/// A message the orchestrator sent to a running worker.
pub const SOURCE_ORCHESTRATOR: &str = "orchestrator";

/// One steer waiting for, or taken at, a safe point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerItem {
    /// Id the owner accepted it under. Reported back in
    /// `AgentEvent::UserMessageInjected`.
    pub id: String,
    /// Who sent it (`SOURCE_*`).
    pub source: String,
    /// The text added to the turn.
    pub text: String,
}

#[derive(Debug, Default)]
struct InboxState {
    closed: bool,
    items: VecDeque<SteerItem>,
}

/// In-memory steer inbox of one running turn. Clones share the inbox.
///
/// One mutex decides each item once: either it is pushed before the close
/// and the loop takes it or the owner drains it, or the push is refused and
/// the caller queues it.
#[derive(Debug, Clone, Default)]
pub struct SteerInbox {
    state: Arc<Mutex<InboxState>>,
}

impl SteerInbox {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InboxState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds `item` while the inbox is open, returning how many items are
    /// now pending. A closed inbox hands the item back.
    ///
    /// # Errors
    /// Returns the item when the inbox is closed.
    pub fn try_push(&self, item: SteerItem) -> Result<usize, SteerItem> {
        let mut state = self.lock();
        if state.closed {
            return Err(item);
        }
        state.items.push_back(item);
        Ok(state.items.len())
    }

    /// Takes every pending item. Nothing is taken once the inbox is closed:
    /// those items belong to the owner.
    #[must_use]
    pub fn take(&self) -> Vec<SteerItem> {
        let mut state = self.lock();
        if state.closed {
            return Vec::new();
        }
        state.items.drain(..).collect()
    }

    /// Closes the inbox. Idempotent; pending items stay for the owner.
    pub fn close(&self) {
        self.lock().closed = true;
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.lock().closed
    }

    #[must_use]
    pub fn pending(&self) -> usize {
        self.lock().items.len()
    }

    /// Closes the inbox and returns the items the turn never took.
    #[must_use]
    pub fn drain_unread(&self) -> Vec<SteerItem> {
        let mut state = self.lock();
        state.closed = true;
        state.items.drain(..).collect()
    }
}

/// One line of a worker's steer file.
#[derive(Debug, Serialize, Deserialize)]
struct SteerLine {
    seq: u64,
    text: String,
}

#[derive(Debug, Default)]
struct FileState {
    offset: u64,
    closed: bool,
}

/// Steers a worker child reads from the file its manager appends to. Only
/// complete lines are read; the manager never writes after a failed append,
/// so a partial line is never followed by another one.
#[derive(Debug)]
pub struct SteerFile {
    path: PathBuf,
    state: Mutex<FileState>,
}

impl SteerFile {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(FileState::default()),
        }
    }

    fn take(&self) -> Vec<SteerItem> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return Vec::new();
        }
        let (items, consumed) = match read_complete_lines(&self.path, state.offset) {
            Ok(read) => read,
            Err(err) => {
                tracing::warn!(
                    error = &err as &dyn std::error::Error,
                    "Failed to read steer file"
                );
                return Vec::new();
            }
        };
        state.offset += consumed;
        items
    }

    fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
    }
}

/// Reads complete lines after `offset`, returning their items and how many
/// bytes they took. A missing file has nothing to read.
fn read_complete_lines(path: &Path, offset: u64) -> std::io::Result<(Vec<SteerItem>, u64)> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(err) => return Err(err),
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let Some(end) = buf.iter().rposition(|byte| *byte == b'\n') else {
        return Ok((Vec::new(), 0));
    };
    let items = buf[..end]
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<SteerLine>(line).ok())
        .map(|line| SteerItem {
            id: line.seq.to_string(),
            source: SOURCE_ORCHESTRATOR.to_string(),
            text: line.text,
        })
        .collect();
    Ok((items, (end + 1) as u64))
}

/// Appends one steer line with a single write. On error the caller must stop
/// appending to this file: the write may have left a partial line.
///
/// # Errors
/// Returns an error when the file cannot be opened or written.
pub fn append_steer_line(path: &Path, seq: u64, text: &str) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(&SteerLine {
        seq,
        text: text.to_string(),
    })
    .map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;
    file.flush()
}

/// Where a run takes its steers from.
#[derive(Debug, Clone)]
pub enum SteerSource {
    Inbox(SteerInbox),
    File(Arc<SteerFile>),
}

impl SteerSource {
    /// Pending steers, taken at a safe point.
    #[must_use]
    pub fn take(&self) -> Vec<SteerItem> {
        match self {
            Self::Inbox(inbox) => inbox.take(),
            Self::File(file) => file.take(),
        }
    }

    /// Stops the run from taking more steers. Idempotent.
    pub fn close(&self) {
        match self {
            Self::Inbox(inbox) => inbox.close(),
            Self::File(file) => file.close(),
        }
    }
}

/// Closes a run's steer source when dropped, so every exit of the agent loop
/// closes it.
pub(crate) struct CloseOnDrop<'a>(pub(crate) Option<&'a SteerSource>);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(source) = self.0 {
            source.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> SteerItem {
        SteerItem {
            id: id.to_string(),
            source: SOURCE_USER.to_string(),
            text: format!("text {id}"),
        }
    }

    #[test]
    fn a_closed_inbox_refuses_pushes_and_keeps_unread_items_for_the_owner() {
        let inbox = SteerInbox::new();
        assert_eq!(inbox.try_push(item("1")), Ok(1));
        assert_eq!(inbox.take(), vec![item("1")]);
        assert_eq!(inbox.try_push(item("2")), Ok(1));
        inbox.close();
        inbox.close();
        assert!(
            inbox.take().is_empty(),
            "the loop takes nothing after close"
        );
        assert_eq!(inbox.try_push(item("3")), Err(item("3")));
        assert_eq!(inbox.drain_unread(), vec![item("2")]);
        assert!(inbox.drain_unread().is_empty());
    }

    #[test]
    fn the_steer_file_reads_only_complete_lines_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steer.jsonl");
        let file = SteerFile::new(path.clone());
        assert!(file.take().is_empty(), "missing file");

        append_steer_line(&path, 1, "first").unwrap();
        let mut partial = OpenOptions::new().append(true).open(&path).unwrap();
        partial.write_all(br#"{"seq":2,"te"#).unwrap();
        let taken = file.take();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].id, "1");
        assert_eq!(taken[0].text, "first");
        assert_eq!(taken[0].source, SOURCE_ORCHESTRATOR);
        assert!(file.take().is_empty(), "the partial line is never read");

        partial.write_all(b"xt\":\"second\"}\n").unwrap();
        let taken = file.take();
        assert_eq!(
            taken.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["2"]
        );
        file.close();
        append_steer_line(&path, 3, "third").unwrap();
        assert!(file.take().is_empty(), "closed");
    }
}
