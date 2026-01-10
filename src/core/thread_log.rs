//! ThreadLog persistence for ZDX.
//!
//! Each thread is stored as a JSONL file where each line is a JSON object
//! representing an event. Threads use schema versioning (§8 of SPEC).
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
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinHandle;

use super::agent::AgentEventRx;
use crate::config::paths::threads_dir;

/// Token usage data for a single API request.
///
/// Used for both persistence (in thread files) and runtime tracking.
/// Supports event-sourcing: each request saves its own Usage, and cumulative
/// totals are derived by summing all events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens (non-cached) for this request
    pub input: u64,
    /// Output tokens for this request
    pub output: u64,
    /// Tokens read from cache for this request
    pub cache_read: u64,
    /// Tokens written to cache for this request
    pub cache_write: u64,
}

impl Usage {
    /// Creates a new Usage with all fields set.
    pub fn new(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    /// Returns true if all fields are zero.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_write == 0
    }

    /// Total tokens for this request (for context window calculation).
    #[allow(dead_code)]
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// Context tokens (input side) for context window percentage.
    pub fn context_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    /// Adds another Usage to this one (for accumulation).
    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    /// Returns a new Usage that is the sum of self and other.
    pub fn plus(&self, other: &Usage) -> Usage {
        Usage {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_read: self.cache_read + other.cache_read,
            cache_write: self.cache_write + other.cache_write,
        }
    }
}

impl std::ops::Add for Usage {
    type Output = Usage;

    fn add(self, other: Usage) -> Usage {
        self.plus(&other)
    }
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, other: Usage) {
        self.add(&other);
    }
}

/// Current schema version for new threads.
pub const SCHEMA_VERSION: u32 = 1;

/// A thread event (polymorphic, tag-based).
///
/// This enum represents all event types that can be persisted in a thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadEvent {
    /// Meta event: first line of a v1+ thread file.
    Meta {
        schema_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_path: Option<String>,
        ts: String,
    },

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

    /// Interrupted event: thread was interrupted by user.
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

    /// Usage event: token usage snapshot after a turn.
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        ts: String,
    },
}

impl ThreadEvent {
    /// Creates a new meta event with an optional root path.
    pub fn meta_with_root(root_path: Option<String>) -> Self {
        Self::Meta {
            schema_version: SCHEMA_VERSION,
            title: None,
            root_path,
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

    /// Creates a new usage event.
    pub fn usage(usage: Usage) -> Self {
        Self::Usage {
            input_tokens: usage.input,
            output_tokens: usage.output,
            cache_read_tokens: usage.cache_read,
            cache_write_tokens: usage.cache_write,
            ts: chrono_timestamp(),
        }
    }

    /// Converts an `EngineEvent` to a `ThreadEvent` if applicable.
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

fn normalize_title(title: impl Into<String>) -> Option<String> {
    let trimmed = title.into().trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Returns a shortened thread ID for display.
pub fn short_thread_id(id: &str) -> String {
    if id.len() > 8 {
        format!("{}…", &id[..8])
    } else {
        id.to_string()
    }
}

/// Truncates a string to at most `max_bytes`, ensuring we don't split a UTF-8 character.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the last char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Manages a thread file.
#[derive(Debug, Clone)]
pub struct ThreadLog {
    pub id: String,
    path: PathBuf,
    /// Whether this is a new thread (needs meta event written).
    is_new: bool,
    /// Root path for the thread (workspace association).
    root_path: Option<String>,
}

impl ThreadLog {
    /// Returns the path to the thread file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Guard to prevent thread creation in tests without proper isolation.
    ///
    /// # Panics
    /// - In unit tests (`#[cfg(test)]`): panics if `ZDX_HOME` is not set
    /// - At runtime: panics if `ZDX_BLOCK_THREAD_WRITES=1` is set
    ///
    /// This ensures tests don't pollute the user's home directory with thread files.
    fn guard_thread_creation() {
        // Compile-time guard for unit tests
        #[cfg(test)]
        if std::env::var("ZDX_HOME").is_err() {
            panic!(
                "Tests must set ZDX_HOME to a temp directory!\n\
                 ThreadLog would be created in user's home directory.\n\
                 Use `setup_temp_zdx_home()` or set ZDX_HOME env var."
            );
        }

        // Runtime guard for integration tests
        #[cfg(not(test))]
        if std::env::var("ZDX_BLOCK_THREAD_WRITES").is_ok_and(|v| v == "1") {
            panic!(
                "ZDX_BLOCK_THREAD_WRITES=1 but trying to create a thread!\n\
                 Use --no-thread flag or set ZDX_HOME to a temp directory."
            );
        }
    }

    /// Creates a new thread and associates it with a root path.
    pub fn new_with_root(root: &Path) -> Result<Self> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root_path = Some(root.display().to_string());
        Self::new_with_root_path(root_path)
    }

    fn new_with_root_path(root_path: Option<String>) -> Result<Self> {
        Self::guard_thread_creation();

        let id = generate_thread_id();
        let dir = threads_dir();
        fs::create_dir_all(&dir).context("Failed to create threads directory")?;

        let path = dir.join(format!("{}.jsonl", id));
        let is_new = !path.exists();

        Ok(Self {
            id,
            path,
            is_new,
            root_path,
        })
    }

    /// Creates or opens a thread with a specific ID.
    ///
    /// # Panics
    /// In tests, panics if `ZDX_HOME` is not set (to prevent polluting user's home).
    pub fn with_id(id: String) -> Result<Self> {
        Self::guard_thread_creation();

        let dir = threads_dir();
        fs::create_dir_all(&dir).context("Failed to create threads directory")?;

        let path = dir.join(format!("{}.jsonl", id));
        let is_new = !path.exists();

        Ok(Self {
            id,
            path,
            is_new,
            root_path: None,
        })
    }

    /// Ensures the meta event is written for new threads.
    fn ensure_meta(&mut self) -> Result<()> {
        if self.is_new {
            self.append_raw(&ThreadEvent::meta_with_root(self.root_path.clone()))?;
            self.is_new = false;
        }
        Ok(())
    }

    /// Appends an event to the thread file (internal, no meta check).
    fn append_raw(&self, event: &ThreadEvent) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .context("Failed to open thread file")?;

        let json = serde_json::to_string(event).context("Failed to serialize event")?;
        writeln!(file, "{}", json).context("Failed to write to thread file")?;

        Ok(())
    }

    /// Appends an event to the thread file.
    ///
    /// For new threads, automatically writes the meta event first.
    pub fn append(&mut self, event: &ThreadEvent) -> Result<()> {
        // Don't write meta before another meta
        if !matches!(event, ThreadEvent::Meta { .. }) {
            self.ensure_meta()?;
        }
        self.append_raw(event)
    }

    /// Reads all events from the thread file.
    pub fn read_events(&self) -> Result<Vec<ThreadEvent>> {
        read_thread_events(&self.path)
    }

    /// Updates the thread title stored in the meta event.
    ///
    /// Writes the meta line with the provided title (or clears it if None/empty),
    /// preserving all subsequent events. The update is performed atomically via
    /// write-to-temp-then-rename.
    pub fn set_title(&mut self, title: Option<String>) -> Result<Option<String>> {
        self.ensure_meta()?;
        let normalized = title.and_then(normalize_title);
        rewrite_meta_with_title(&self.path, normalized.clone())?;
        Ok(normalized)
    }

    /// Updates the thread root path stored in the meta event.
    pub fn set_root_path(&mut self, root: &Path) -> Result<()> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root_path = Some(root.display().to_string());
        self.ensure_meta()?;
        rewrite_meta_with_root(&self.path, root_path)?;
        Ok(())
    }
}

/// Reads thread events from a file path, with backward compatibility.
fn read_thread_events(path: &PathBuf) -> Result<Vec<ThreadEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path).context("Failed to open thread file")?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.context("Failed to read line")?;
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(event) = serde_json::from_str::<ThreadEvent>(&line) {
            events.push(event);
        }
        // Skip unparseable lines (best-effort)
    }

    Ok(events)
}

/// Rewrites the meta event with an updated title, preserving the rest of the file.
fn rewrite_meta_with_title(path: &PathBuf, title: Option<String>) -> Result<()> {
    let file = fs::File::open(path).context("Failed to open thread file")?;
    let reader = BufReader::new(file);

    let temp_path = path.with_extension("jsonl.tmp");
    let mut temp = fs::File::create(&temp_path).context("Failed to create temp thread file")?;

    let mut lines = reader.lines();
    let first_line = lines
        .next()
        .transpose()
        .context("Failed to read meta line")?
        .ok_or_else(|| anyhow!("Thread file is empty"))?;

    let mut meta_event: ThreadEvent =
        serde_json::from_str(&first_line).context("Failed to parse meta event")?;
    match meta_event {
        ThreadEvent::Meta {
            title: ref mut meta_title,
            ..
        } => {
            *meta_title = title;
        }
        _ => bail!("First thread event is not a meta event"),
    }

    let new_meta =
        serde_json::to_string(&meta_event).context("Failed to serialize updated meta event")?;
    writeln!(temp, "{}", new_meta).context("Failed to write updated meta")?;

    for line in lines {
        let line = line.context("Failed to read thread line")?;
        writeln!(temp, "{}", line).context("Failed to write thread line")?;
    }

    temp.sync_all().context("Failed to sync temp thread file")?;
    fs::rename(&temp_path, path).context("Failed to replace thread file")?;
    Ok(())
}

/// Rewrites the meta event with an updated root path, preserving the rest of the file.
fn rewrite_meta_with_root(path: &PathBuf, root_path: Option<String>) -> Result<()> {
    let file = fs::File::open(path).context("Failed to open thread file")?;
    let reader = BufReader::new(file);

    let temp_path = path.with_extension("jsonl.tmp");
    let mut temp = fs::File::create(&temp_path).context("Failed to create temp thread file")?;

    let mut lines = reader.lines();
    let first_line = lines
        .next()
        .transpose()
        .context("Failed to read meta line")?
        .ok_or_else(|| anyhow!("Thread file is empty"))?;

    let mut meta_event: ThreadEvent =
        serde_json::from_str(&first_line).context("Failed to parse meta event")?;
    match meta_event {
        ThreadEvent::Meta {
            root_path: ref mut meta_root,
            ..
        } => {
            *meta_root = root_path;
        }
        _ => bail!("First thread event is not a meta event"),
    }

    let new_meta =
        serde_json::to_string(&meta_event).context("Failed to serialize updated meta event")?;
    writeln!(temp, "{}", new_meta).context("Failed to write updated meta")?;

    for line in lines {
        let line = line.context("Failed to read thread line")?;
        writeln!(temp, "{}", line).context("Failed to write thread line")?;
    }

    temp.sync_all().context("Failed to sync temp thread file")?;
    fs::rename(&temp_path, path).context("Failed to replace thread file")?;
    Ok(())
}

/// Reads only the meta line to extract title (backward compatible).
fn read_meta_title(path: &PathBuf) -> Result<Option<Option<String>>> {
    if !path.exists() {
        return Ok(None);
    }

    let file = fs::File::open(path).context("Failed to open thread file")?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();

    // Read first non-empty line
    loop {
        first_line.clear();
        let bytes = reader.read_line(&mut first_line)?;
        if bytes == 0 {
            return Ok(None); // Empty file
        }
        if !first_line.trim().is_empty() {
            break;
        }
    }

    // Parse meta event, defaulting title to None if missing
    let parsed: ThreadEvent = match serde_json::from_str(&first_line) {
        Ok(event) => event,
        Err(_) => return Ok(None), // Unparseable meta, fallback to None
    };

    if let ThreadEvent::Meta { title, .. } = parsed {
        Ok(Some(title))
    } else {
        Ok(None) // First event wasn't meta
    }
}

/// Reads only the meta line to extract root path (backward compatible).
fn read_meta_root_path(path: &PathBuf) -> Result<Option<Option<String>>> {
    if !path.exists() {
        return Ok(None);
    }

    let file = fs::File::open(path).context("Failed to open thread file")?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();

    // Read first non-empty line
    loop {
        first_line.clear();
        let bytes = reader.read_line(&mut first_line)?;
        if bytes == 0 {
            return Ok(None); // Empty file
        }
        if !first_line.trim().is_empty() {
            break;
        }
    }

    // Parse meta event, defaulting root_path to None if missing
    let parsed: ThreadEvent = match serde_json::from_str(&first_line) {
        Ok(event) => event,
        Err(_) => return Ok(None), // Unparseable meta, fallback to None
    };

    if let ThreadEvent::Meta { root_path, .. } = parsed {
        Ok(Some(root_path))
    } else {
        Ok(None) // First event wasn't meta
    }
}

/// Generates a unique thread ID using UUID v4.
fn generate_thread_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Spawns a thread persistence task that consumes events from a channel.
///
/// The task owns the `ThreadLog` and persists relevant events until the channel closes.
/// Returns a `JoinHandle` that resolves when all events have been persisted.
///
/// Only tool-related and interrupt events are persisted via this task.
/// User and assistant messages are handled separately by the chat/agent modules.
///
/// # Example
///
/// ```ignore
/// let thread = ThreadLog::new_with_root(Path::new("."))?;
/// let (tx, rx) = agent::create_event_channel();
/// let persist_handle = spawn_thread_persist_task(thread, rx);
///
/// // ... send events to tx ...
/// drop(tx); // Close channel
///
/// persist_handle.await.unwrap(); // Wait for persistence to finish
/// ```
pub fn spawn_thread_persist_task(mut thread: ThreadLog, mut rx: AgentEventRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Some(thread_event) = ThreadEvent::from_agent(&event) {
                // Best-effort persistence - log errors but don't panic
                if let Err(e) = thread.append(&thread_event) {
                    eprintln!("Warning: Failed to persist thread event: {}", e);
                }
            }
        }
    })
}

/// Summary information about a saved thread.
#[derive(Debug, Clone)]
pub struct ThreadSummary {
    pub id: String,
    pub title: Option<String>,
    pub root_path: Option<String>,
    pub modified: Option<SystemTime>,
}

impl ThreadSummary {
    /// Returns a display-friendly title (or short ID fallback).
    pub fn display_title(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| short_thread_id(&self.id))
    }
}

/// Lists all saved threads.
///
/// Returns a vector of ThreadSummary sorted by modification time (newest first).
pub fn list_threads() -> Result<Vec<ThreadSummary>> {
    let dir = threads_dir();

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut threads = Vec::new();

    for entry in fs::read_dir(&dir).context("Failed to read threads directory")? {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        // Only process .jsonl files
        if path.extension().is_some_and(|ext| ext == "jsonl")
            && let Some(stem) = path.file_stem()
        {
            let id = stem.to_string_lossy().to_string();
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
            let title = read_meta_title(&path).unwrap_or(None).flatten();
            let root_path = read_meta_root_path(&path).unwrap_or(None).flatten();

            threads.push(ThreadSummary {
                id,
                title,
                root_path,
                modified,
            });
        }
    }

    // Sort by modification time (newest first)
    threads.sort_by(|a, b| b.modified.cmp(&a.modified));

    Ok(threads)
}

/// Loads and returns the events from a thread by ID.
pub fn load_thread_events(id: &str) -> Result<Vec<ThreadEvent>> {
    let thread = ThreadLog::with_id(id.to_string())?;
    thread.read_events()
}

/// Extracts the root path from thread events (if present).
pub fn extract_root_path_from_events(events: &[ThreadEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        ThreadEvent::Meta { root_path, .. } => root_path.clone(),
        _ => None,
    })
}

/// Returns the ID of the most recently modified thread.
///
/// Returns None if no threads exist.
pub fn latest_thread_id() -> Result<Option<String>> {
    let threads = list_threads()?;
    Ok(threads.into_iter().next().map(|s| s.id))
}

/// Loads thread events and converts them to ChatMessages for API use.
///
/// Reconstructs the full thread including tool use/result pairs.
pub fn load_thread_as_messages(id: &str) -> Result<Vec<crate::providers::ChatMessage>> {
    let events = load_thread_events(id)?;
    Ok(thread_events_to_messages(events))
}

/// Updates a thread's title by ID.
pub fn set_thread_title(id: &str, title: Option<String>) -> Result<Option<String>> {
    let path = threads_dir().join(format!("{}.jsonl", id));
    if !path.exists() {
        bail!("Thread '{}' not found", id);
    }

    let mut thread = ThreadLog::with_id(id.to_string())?;
    thread.set_title(title)
}

/// Converts thread events to chat messages for API replay.
pub fn thread_events_to_messages(events: Vec<ThreadEvent>) -> Vec<crate::providers::ChatMessage> {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};

    let mut messages: Vec<ChatMessage> = Vec::new();

    // Track pending assistant content to group into single messages
    // (thinking blocks + tool uses belong to the same assistant turn)
    let mut pending_thinking: Vec<(String, Option<String>)> = Vec::new(); // (content, signature)
    let mut pending_tool_uses: Vec<(String, String, Value)> = Vec::new(); // (id, name, input)
    let mut pending_tool_results: Vec<crate::tools::ToolResult> = Vec::new();
    let mut open_tool_uses: Vec<String> = Vec::new();

    /// Flushes pending assistant content (thinking + tool_use) and tool results into messages.
    fn flush_pending_assistant(
        messages: &mut Vec<ChatMessage>,
        pending_thinking: &mut Vec<(String, Option<String>)>,
        pending_tool_uses: &mut Vec<(String, String, Value)>,
        pending_tool_results: &mut Vec<crate::tools::ToolResult>,
    ) {
        // First, flush any pending thinking/tool_use as an assistant message
        if !pending_thinking.is_empty() || !pending_tool_uses.is_empty() {
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
        }

        // Then, flush any pending tool results as a user message
        // (This is separate because tool_results may need to be flushed
        // even when thinking/tool_use have already been flushed)
        if !pending_tool_results.is_empty() {
            messages.push(ChatMessage::tool_results(std::mem::take(
                pending_tool_results,
            )));
        }
    }

    for event in events {
        match event {
            ThreadEvent::Meta { .. } => {
                // Skip meta events
            }
            ThreadEvent::Message { role, text, .. } => {
                // For assistant messages with pending thinking (but no pending tool uses),
                // combine them into one message. The API requires thinking blocks and
                // subsequent content in the same turn.
                //
                // If there are pending tool uses, the thinking belongs with those,
                // so we flush normally (thinking + tool_use go together).
                if role == "assistant"
                    && !pending_thinking.is_empty()
                    && pending_tool_uses.is_empty()
                {
                    // First, flush any pending tool results as a user message
                    // (This happens when tool_result was processed before this thinking block)
                    if !pending_tool_results.is_empty() {
                        messages.push(ChatMessage::tool_results(std::mem::take(
                            &mut pending_tool_results,
                        )));
                    }

                    let mut blocks: Vec<ChatContentBlock> = Vec::new();

                    // Add thinking blocks first
                    for (content, signature) in std::mem::take(&mut pending_thinking) {
                        blocks.push(ChatContentBlock::Thinking {
                            thinking: content,
                            // Use empty string if signature is missing (aborted thinking)
                            // The API serialization will convert this to a text block
                            signature: signature.unwrap_or_default(),
                        });
                    }

                    // Add the text block
                    if !text.is_empty() {
                        blocks.push(ChatContentBlock::Text(text));
                    }

                    messages.push(ChatMessage::assistant_blocks(blocks));
                } else {
                    // Flush any pending assistant content before adding a new message
                    flush_pending_assistant(
                        &mut messages,
                        &mut pending_thinking,
                        &mut pending_tool_uses,
                        &mut pending_tool_results,
                    );

                    messages.push(ChatMessage {
                        role,
                        content: MessageContent::Text(text),
                    });
                }
            }
            ThreadEvent::Thinking {
                content, signature, ..
            } => {
                pending_thinking.push((content, signature));
            }
            ThreadEvent::ToolUse {
                id, name, input, ..
            } => {
                open_tool_uses.push(id.clone());
                pending_tool_uses.push((id, name, input));
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                ..
            } => {
                open_tool_uses.retain(|id| id != &tool_use_id);
                // Flush pending assistant content (thinking + tool_use) before adding results.
                // This ensures the tool_use assistant message is closed, so any subsequent
                // thinking blocks belong to the next assistant turn, not the tool_use turn.
                flush_pending_assistant(
                    &mut messages,
                    &mut pending_thinking,
                    &mut pending_tool_uses,
                    &mut pending_tool_results,
                );

                pending_tool_results.push(crate::tools::ToolResult {
                    tool_use_id,
                    content: crate::tools::ToolResultContent::Text(
                        serde_json::to_string(&output).unwrap_or_default(),
                    ),
                    is_error: !ok,
                });
            }
            ThreadEvent::Interrupted { .. } => {
                // Skip interrupted events when loading for API
            }
            ThreadEvent::Usage { .. } => {
                // Skip usage events when loading for API (they're for thread tracking only)
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

    // If the thread ends with open tool uses, synthesize error results for resume.
    if !open_tool_uses.is_empty() {
        for tool_use_id in open_tool_uses.drain(..) {
            let output = serde_json::json!({
                "ok": false,
                "error": {
                    "code": "interrupted",
                    "message": "Tool call was interrupted; no result recorded."
                }
            });
            pending_tool_results.push(crate::tools::ToolResult {
                tool_use_id,
                content: crate::tools::ToolResultContent::Text(
                    serde_json::to_string(&output).unwrap_or_default(),
                ),
                is_error: true,
            });
        }

        flush_pending_assistant(
            &mut messages,
            &mut pending_thinking,
            &mut pending_tool_uses,
            &mut pending_tool_results,
        );
    }

    messages
}

/// Extracts usage from thread events for thread restore.
///
/// With per-request delta storage (event sourcing), we:
/// - Sum all Usage events → cumulative totals (for cost display)
/// - Take last Usage event → latest request (for context % display)
///
/// Returns (cumulative, latest) as Usage structs.
pub fn extract_usage_from_thread_events(events: &[ThreadEvent]) -> (Usage, Usage) {
    let mut cumulative = Usage::default();
    let mut latest = Usage::default();

    for event in events {
        if let ThreadEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            ..
        } = event
        {
            let usage = Usage::new(
                *input_tokens,
                *output_tokens,
                *cache_read_tokens,
                *cache_write_tokens,
            );

            // Sum for cumulative
            cumulative += usage;

            // Track latest (will be overwritten each time, ending with last)
            latest = usage;
        }
    }

    (cumulative, latest)
}

/// Formats a SystemTime as a simple date/time string (YYYY-MM-DD HH:MM).
pub fn format_timestamp(time: SystemTime) -> Option<String> {
    let datetime: DateTime<Utc> = time.into();
    Some(datetime.format("%Y-%m-%d %H:%M").to_string())
}

/// Formats a SystemTime as a short relative age (e.g., "2m ago", "3h ago", "5d ago").
pub fn format_timestamp_relative(time: SystemTime) -> Option<String> {
    let datetime: DateTime<Utc> = time.into();
    let now = Utc::now();
    let seconds = now.signed_duration_since(datetime).num_seconds().max(0);

    let mins = seconds / 60;
    if mins < 1 {
        return Some("just now".to_string());
    }
    if mins < 60 {
        return Some(format!("{}m ago", mins));
    }

    let hours = mins / 60;
    if hours < 24 {
        return Some(format!("{}h ago", hours));
    }

    let days = hours / 24;
    if days < 7 {
        return Some(format!("{}d ago", days));
    }

    let weeks = days / 7;
    if weeks < 5 {
        return Some(format!("{}w ago", weeks));
    }

    let months = days / 30;
    if months < 12 {
        return Some(format!("{}mo ago", months));
    }

    let years = days / 365;
    Some(format!("{}y ago", years))
}

/// Formats a thread transcript in a human-readable format.
pub fn format_transcript(events: &[ThreadEvent]) -> String {
    let mut output = String::new();

    for event in events {
        match event {
            ThreadEvent::Meta { schema_version, .. } => {
                output.push_str(&format!("### Thread (schema v{})\n\n", schema_version));
            }
            ThreadEvent::Message { role, text, .. } => {
                let role_label = match role.as_str() {
                    "user" => "You",
                    "assistant" => "Assistant",
                    _ => role,
                };
                output.push_str(&format!("### {}\n", role_label));
                output.push_str(text);
                output.push_str("\n\n");
            }
            ThreadEvent::Thinking { content, .. } => {
                output.push_str("### Thinking\n");
                // Truncate long thinking content for display
                if content.len() > 500 {
                    output.push_str(truncate_str(content, 500));
                    output.push_str("...");
                } else {
                    output.push_str(content);
                }
                output.push_str("\n\n");
            }
            ThreadEvent::ToolUse { name, input, .. } => {
                output.push_str(&format!("### Tool: {}\n", name));
                output.push_str(&format!(
                    "```json\n{}\n```\n\n",
                    serde_json::to_string_pretty(input).unwrap_or_default()
                ));
            }
            ThreadEvent::ToolResult {
                ok, output: out, ..
            } => {
                let status = if *ok { "✓" } else { "✗" };
                output.push_str(&format!("### Result {}\n", status));
                // Truncate long outputs for display
                let out_str = serde_json::to_string_pretty(out).unwrap_or_default();
                if out_str.len() > 500 {
                    output.push_str(&format!(
                        "```json\n{}...\n```\n\n",
                        truncate_str(&out_str, 500)
                    ));
                } else {
                    output.push_str(&format!("```json\n{}\n```\n\n", out_str));
                }
            }
            ThreadEvent::Interrupted { .. } => {
                output.push_str("### Interrupted\n\n");
            }
            ThreadEvent::Usage { .. } => {
                // Skip usage events in transcript display
            }
        }
    }

    output.trim_end().to_string()
}

/// ThreadLog options for CLI commands.
#[derive(Debug, Clone, Default)]
pub struct ThreadPersistenceOptions {
    /// Append to an existing thread by ID.
    pub thread_id: Option<String>,
    /// Do not save the thread.
    pub no_save: bool,
}

impl ThreadPersistenceOptions {
    /// Resolves thread options into an optional ThreadLog.
    ///
    /// Returns None if no_save is true.
    /// Returns existing thread if thread_id is provided.
    /// Returns new thread otherwise.
    pub fn resolve(&self, root: &Path) -> Result<Option<ThreadLog>> {
        if self.no_save {
            return Ok(None);
        }

        if let Some(ref id) = self.thread_id {
            return Ok(Some(ThreadLog::with_id(id.clone())?));
        }

        Ok(Some(ThreadLog::new_with_root(root)?))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn setup_temp_zdx_home() -> &'static TempDir {
        static ZDX_HOME: OnceLock<TempDir> = OnceLock::new();
        ZDX_HOME.get_or_init(|| {
            let temp = TempDir::new().unwrap();
            unsafe {
                std::env::set_var("ZDX_HOME", temp.path());
            }
            temp
        })
    }

    fn unique_thread_id(prefix: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        format!("{}-{}", prefix, nanos)
    }

    #[test]
    fn test_thread_creates_file_with_meta() {
        let _temp = setup_temp_zdx_home();

        let mut thread = ThreadLog::with_id(unique_thread_id("creates-meta")).unwrap();
        thread.append(&ThreadEvent::user_message("hello")).unwrap();

        // Read raw file content to verify meta is first
        let content = fs::read_to_string(&thread.path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.len() >= 2);
        assert!(lines[0].contains("\"type\":\"meta\""));
        assert!(lines[0].contains("\"schema_version\":1"));
    }

    #[test]
    fn test_thread_appends_jsonl_with_tool_events() {
        let _temp = setup_temp_zdx_home();

        let mut thread = ThreadLog::with_id(unique_thread_id("tool-events")).unwrap();
        thread
            .append(&ThreadEvent::user_message("read main.rs"))
            .unwrap();
        thread
            .append(&ThreadEvent::tool_use(
                "tool-1",
                "read",
                json!({"path": "main.rs"}),
            ))
            .unwrap();
        thread
            .append(&ThreadEvent::tool_result(
                "tool-1",
                json!({"ok": true, "data": {"content": "fn main() {}"}}),
                true,
            ))
            .unwrap();
        thread
            .append(&ThreadEvent::assistant_message("Here's the file"))
            .unwrap();

        let events = thread.read_events().unwrap();
        // meta + user + tool_use + tool_result + assistant = 5 events
        assert_eq!(events.len(), 5);
        assert!(matches!(events[0], ThreadEvent::Meta { .. }));
        assert!(matches!(events[1], ThreadEvent::Message { ref role, .. } if role == "user"));
        assert!(matches!(events[2], ThreadEvent::ToolUse { ref name, .. } if name == "read"));
        assert!(matches!(
            events[3],
            ThreadEvent::ToolResult { ok: true, .. }
        ));
        assert!(matches!(events[4], ThreadEvent::Message { ref role, .. } if role == "assistant"));
    }

    #[test]
    fn test_thread_event_serialization() {
        let meta = ThreadEvent::meta_with_root(None);
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"type\":\"meta\""));
        assert!(json.contains("\"schema_version\":1"));

        let tool_use = ThreadEvent::tool_use("t1", "bash", json!({"command": "ls"}));
        let json = serde_json::to_string(&tool_use).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"name\":\"bash\""));

        let tool_result = ThreadEvent::tool_result("t1", json!({"stdout": "file.txt"}), true);
        let json = serde_json::to_string(&tool_result).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        assert!(json.contains("\"ok\":true"));

        let interrupted = ThreadEvent::interrupted();
        let json = serde_json::to_string(&interrupted).unwrap();
        assert!(json.contains("\"type\":\"interrupted\""));
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"text\":\"Interrupted\""));
    }

    #[test]
    fn test_events_to_messages_with_tools() {
        // Test the conversion logic directly without env var dependency
        let events = vec![
            ThreadEvent::user_message("list files"),
            ThreadEvent::tool_use("t1", "bash", json!({"command": "ls"})),
            ThreadEvent::tool_result("t1", json!({"stdout": "file.txt\n"}), true),
            ThreadEvent::assistant_message("Found file.txt"),
        ];

        let messages = thread_events_to_messages(events);

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
    fn test_thread_persistence_options_no_save() {
        let opts = ThreadPersistenceOptions {
            no_save: true,
            ..Default::default()
        };
        assert!(opts.resolve(Path::new(".")).unwrap().is_none());
    }

    #[test]
    fn test_thread_persistence_options_with_id() {
        let _temp = setup_temp_zdx_home();

        let id = unique_thread_id("existing");
        let opts = ThreadPersistenceOptions {
            thread_id: Some(id.clone()),
            ..Default::default()
        };
        let thread = opts.resolve(Path::new(".")).unwrap().unwrap();
        assert_eq!(thread.id, id);
    }

    #[test]
    fn test_format_transcript_with_tools() {
        let events = vec![
            ThreadEvent::meta_with_root(None),
            ThreadEvent::user_message("read main.rs"),
            ThreadEvent::tool_use("t1", "read", json!({"path": "main.rs"})),
            ThreadEvent::tool_result(
                "t1",
                json!({"ok": true, "data": {"content": "fn main() {}"}}),
                true,
            ),
            ThreadEvent::assistant_message("Here's the file content."),
        ];

        let transcript = format_transcript(&events);
        assert!(transcript.contains("Thread (schema v1)"));
        assert!(transcript.contains("### You"));
        assert!(transcript.contains("### Tool: read"));
        assert!(transcript.contains("### Result ✓"));
        assert!(transcript.contains("### Assistant"));
    }

    #[test]
    fn test_thinking_event_serialization() {
        // Test thinking with signature
        let thinking = ThreadEvent::thinking("Let me analyze this...", Some("sig123".to_string()));
        let json = serde_json::to_string(&thinking).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        assert!(json.contains("\"content\":\"Let me analyze this...\""));
        assert!(json.contains("\"signature\":\"sig123\""));

        // Test thinking without signature (aborted)
        let aborted = ThreadEvent::thinking("Partial thought...", None);
        let json = serde_json::to_string(&aborted).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        assert!(json.contains("\"signature\":null"));
    }

    #[test]
    fn test_thinking_event_deserialization() {
        // Test deserialization with signature
        let json = r#"{"type":"thinking","content":"Deep analysis","signature":"abc123","ts":"2024-01-01T00:00:00Z"}"#;
        let event: ThreadEvent = serde_json::from_str(json).unwrap();
        match event {
            ThreadEvent::Thinking {
                content, signature, ..
            } => {
                assert_eq!(content, "Deep analysis");
                assert_eq!(signature, Some("abc123".to_string()));
            }
            _ => panic!("Expected Thinking event"),
        }

        // Test deserialization without signature (backward compat)
        let json_no_sig = r#"{"type":"thinking","content":"Partial","ts":"2024-01-01T00:00:00Z"}"#;
        let event: ThreadEvent = serde_json::from_str(json_no_sig).unwrap();
        match event {
            ThreadEvent::Thinking { signature, .. } => {
                assert_eq!(signature, None);
            }
            _ => panic!("Expected Thinking event"),
        }
    }

    #[test]
    fn test_events_to_messages_with_thinking() {
        use crate::providers::{ChatContentBlock, MessageContent};

        let events = vec![
            ThreadEvent::user_message("solve this problem"),
            ThreadEvent::thinking(
                "Let me think about this...".to_string(),
                Some("sig123".to_string()),
            ),
            ThreadEvent::tool_use("t1", "bash", json!({"command": "echo test"})),
            ThreadEvent::tool_result("t1", json!({"stdout": "test\n"}), true),
            ThreadEvent::assistant_message("Done!"),
        ];

        let messages = thread_events_to_messages(events);

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
    fn test_events_to_messages_thinking_then_text() {
        // Test case for the bug: thinking followed directly by assistant text (no tool use)
        // This should produce a SINGLE assistant message with [thinking, text] blocks,
        // NOT two separate messages. The API rejects modifications to thinking blocks
        // in the latest assistant message, so they must be in the same message.
        use crate::providers::{ChatContentBlock, MessageContent};

        let events = vec![
            ThreadEvent::user_message("explain this"),
            ThreadEvent::thinking("Let me analyze...".to_string(), Some("sig456".to_string())),
            ThreadEvent::assistant_message("Here's my explanation."),
        ];

        let messages = thread_events_to_messages(events);

        // user + assistant(thinking + text) = 2 messages (NOT 3!)
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");

        // Second message should be assistant with BOTH thinking AND text blocks
        assert_eq!(messages[1].role, "assistant");
        if let MessageContent::Blocks(blocks) = &messages[1].content {
            assert_eq!(blocks.len(), 2, "Should have 2 blocks: thinking + text");
            assert!(
                matches!(&blocks[0], ChatContentBlock::Thinking { thinking, signature }
                    if thinking == "Let me analyze..." && signature == "sig456"
                ),
                "First block should be thinking"
            );
            assert!(
                matches!(&blocks[1], ChatContentBlock::Text(text) if text == "Here's my explanation."),
                "Second block should be text"
            );
        } else {
            panic!("Expected Blocks content, got {:?}", messages[1].content);
        }
    }

    #[test]
    fn test_events_to_messages_tool_use_then_thinking() {
        // Regression test for the bug: when a tool call is followed by another thinking block,
        // the second thinking must belong to the FINAL assistant message, not the tool_use message.
        //
        // Sequence: user → thinking1 → tool_use → tool_result → thinking2 → assistant_text
        //
        // Expected messages:
        // 1. User: "question"
        // 2. Assistant: [Thinking1, ToolUse]
        // 3. User: [ToolResult]
        // 4. Assistant: [Thinking2, Text]
        use crate::providers::{ChatContentBlock, MessageContent};

        let events = vec![
            ThreadEvent::user_message("run a command"),
            ThreadEvent::thinking("Let me run this...".to_string(), Some("sig1".to_string())),
            ThreadEvent::tool_use("t1", "bash", json!({"command": "echo hello"})),
            ThreadEvent::tool_result("t1", json!({"stdout": "hello\n"}), true),
            ThreadEvent::thinking(
                "Now let me explain...".to_string(),
                Some("sig2".to_string()),
            ),
            ThreadEvent::assistant_message("The command output was 'hello'."),
        ];

        let messages = thread_events_to_messages(events);

        // user + assistant(thinking1 + tool_use) + user(tool_result) + assistant(thinking2 + text) = 4
        assert_eq!(messages.len(), 4, "Should have 4 messages");

        // Message 0: User
        assert_eq!(messages[0].role, "user");

        // Message 1: Assistant with thinking1 + tool_use
        assert_eq!(messages[1].role, "assistant");
        if let MessageContent::Blocks(blocks) = &messages[1].content {
            assert_eq!(blocks.len(), 2, "First assistant should have 2 blocks");
            assert!(
                matches!(&blocks[0], ChatContentBlock::Thinking { thinking, signature }
                    if thinking == "Let me run this..." && signature == "sig1"
                ),
                "First block should be thinking1"
            );
            assert!(
                matches!(&blocks[1], ChatContentBlock::ToolUse { name, .. } if name == "bash"),
                "Second block should be tool_use"
            );
        } else {
            panic!("Expected Blocks content for message 1");
        }

        // Message 2: User with tool_result
        assert_eq!(messages[2].role, "user");

        // Message 3: Assistant with thinking2 + text (THE KEY ASSERTION)
        assert_eq!(messages[3].role, "assistant");
        if let MessageContent::Blocks(blocks) = &messages[3].content {
            assert_eq!(
                blocks.len(),
                2,
                "Final assistant should have 2 blocks: thinking2 + text"
            );
            assert!(
                matches!(&blocks[0], ChatContentBlock::Thinking { thinking, signature }
                    if thinking == "Now let me explain..." && signature == "sig2"
                ),
                "First block should be thinking2 (not attached to tool_use message!)"
            );
            assert!(
                matches!(&blocks[1], ChatContentBlock::Text(text)
                    if text == "The command output was 'hello'."
                ),
                "Second block should be text"
            );
        } else {
            panic!(
                "Expected Blocks content for message 3, got {:?}",
                messages[3].content
            );
        }
    }

    #[test]
    fn test_thread_thinking_roundtrip() {
        let _temp = setup_temp_zdx_home();

        let mut thread = ThreadLog::with_id(unique_thread_id("thinking-roundtrip")).unwrap();
        thread
            .append(&ThreadEvent::user_message("explain"))
            .unwrap();
        thread
            .append(&ThreadEvent::thinking(
                "Deep analysis here...",
                Some("signature456".to_string()),
            ))
            .unwrap();
        thread
            .append(&ThreadEvent::assistant_message("Here's my answer"))
            .unwrap();

        let events = thread.read_events().unwrap();
        // meta + user + thinking + assistant = 4 events
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], ThreadEvent::Meta { .. }));
        assert!(matches!(events[1], ThreadEvent::Message { ref role, .. } if role == "user"));
        assert!(
            matches!(events[2], ThreadEvent::Thinking { ref content, ref signature, .. }
                if content == "Deep analysis here..." && signature == &Some("signature456".to_string())
            )
        );
        assert!(matches!(events[3], ThreadEvent::Message { ref role, .. } if role == "assistant"));
    }

    #[test]
    fn test_format_transcript_with_thinking() {
        let events = vec![
            ThreadEvent::meta_with_root(None),
            ThreadEvent::user_message("explain this"),
            ThreadEvent::thinking(
                "Analyzing the request...".to_string(),
                Some("sig".to_string()),
            ),
            ThreadEvent::assistant_message("Here's my explanation."),
        ];

        let transcript = format_transcript(&events);
        assert!(transcript.contains("### Thinking"));
        assert!(transcript.contains("Analyzing the request..."));
    }

    #[test]
    fn test_usage_event_serialization() {
        let usage = ThreadEvent::usage(Usage::new(1000, 500, 2000, 100));
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"type\":\"usage\""));
        assert!(json.contains("\"input_tokens\":1000"));
        assert!(json.contains("\"output_tokens\":500"));
        assert!(json.contains("\"cache_read_tokens\":2000"));
        assert!(json.contains("\"cache_write_tokens\":100"));
    }

    #[test]
    fn test_usage_event_deserialization() {
        let json = r#"{"type":"usage","input_tokens":1000,"output_tokens":500,"cache_read_tokens":2000,"cache_write_tokens":100,"ts":"2024-01-01T00:00:00Z"}"#;
        let event: ThreadEvent = serde_json::from_str(json).unwrap();
        match event {
            ThreadEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 1000);
                assert_eq!(output_tokens, 500);
                assert_eq!(cache_read_tokens, 2000);
                assert_eq!(cache_write_tokens, 100);
            }
            _ => panic!("Expected Usage event"),
        }
    }

    #[test]
    fn test_extract_usage_from_events() {
        // Usage events are per-request deltas (event sourcing)
        // Cumulative = sum of all events, Latest = last event
        let events = vec![
            ThreadEvent::user_message("hello"),
            ThreadEvent::assistant_message("hi"),
            ThreadEvent::usage(Usage::new(100, 50, 200, 25)), // Request 1
            ThreadEvent::user_message("bye"),
            ThreadEvent::assistant_message("goodbye"),
            ThreadEvent::usage(Usage::new(150, 75, 300, 30)), // Request 2
        ];

        let (cumulative, latest) = extract_usage_from_thread_events(&events);
        // Cumulative = sum of all usage events
        assert_eq!(cumulative, Usage::new(250, 125, 500, 55));
        // Latest = last usage event (for context %)
        assert_eq!(latest, Usage::new(150, 75, 300, 30));
    }

    #[test]
    fn test_extract_usage_from_events_empty() {
        let events = vec![
            ThreadEvent::user_message("hello"),
            ThreadEvent::assistant_message("hi"),
        ];

        let (cumulative, latest) = extract_usage_from_thread_events(&events);
        assert_eq!(cumulative, Usage::default());
        assert_eq!(latest, Usage::default());
    }

    #[test]
    fn test_thread_usage_roundtrip() {
        let _temp = setup_temp_zdx_home();

        let mut thread = ThreadLog::with_id(unique_thread_id("usage-roundtrip")).unwrap();
        thread.append(&ThreadEvent::user_message("hello")).unwrap();
        thread
            .append(&ThreadEvent::assistant_message("hi"))
            .unwrap();
        thread
            .append(&ThreadEvent::usage(Usage::new(1000, 500, 2000, 100)))
            .unwrap();

        let events = thread.read_events().unwrap();
        // meta + user + assistant + usage = 4 events
        assert_eq!(events.len(), 4);

        let (cumulative, latest) = extract_usage_from_thread_events(&events);
        // Single event: cumulative = latest
        assert_eq!(cumulative, Usage::new(1000, 500, 2000, 100));
        assert_eq!(latest, Usage::new(1000, 500, 2000, 100));
    }

    #[test]
    fn test_usage_struct_operations() {
        let u1 = Usage::new(100, 50, 200, 25);
        let u2 = Usage::new(150, 75, 300, 30);

        // Test add
        let sum = u1 + u2;
        assert_eq!(sum, Usage::new(250, 125, 500, 55));

        // Test add_assign
        let mut u3 = u1;
        u3 += u2;
        assert_eq!(u3, Usage::new(250, 125, 500, 55));

        // Test total
        assert_eq!(u1.total(), 375);

        // Test context_input
        assert_eq!(u1.context_input(), 325);

        // Test is_empty
        assert!(!u1.is_empty());
        assert!(Usage::default().is_empty());
    }
}
