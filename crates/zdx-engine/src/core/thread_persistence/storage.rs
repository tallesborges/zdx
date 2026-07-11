use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow, bail};

use super::event::{ThreadEvent, normalize_title};
use super::format::display_title_or_short_id;
use crate::config::paths::threads_dir;

/// Truncates a string to at most `max_bytes`, ensuring we don't split a UTF-8 character.
pub(crate) fn truncate_str(s: &str, max_bytes: usize) -> &str {
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
pub struct Thread {
    pub id: String,
    pub(crate) path: PathBuf,
    /// Whether this is a new thread (needs meta event written).
    is_new: bool,
    /// Root path for the thread (workspace association).
    root_path: Option<String>,
    /// The ID of the parent thread this was handed off from (if any).
    handoff_from: Option<String>,
    /// Subagent/helper lineage recorded in the meta line for new threads.
    origin_kind: Option<String>,
    parent_thread_id: Option<String>,
    subagent_name: Option<String>,
}

impl Thread {
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
        assert!(
            std::env::var("ZDX_HOME").is_ok(),
            "Tests must set ZDX_HOME to a temp directory!\n\
                 Thread would be created in user's home directory.\n\
                 Use `setup_temp_zdx_home()` or set ZDX_HOME env var."
        );

        // Runtime guard for integration tests
        #[cfg(not(test))]
        assert!(
            !std::env::var("ZDX_BLOCK_THREAD_WRITES").is_ok_and(|v| v == "1"),
            "ZDX_BLOCK_THREAD_WRITES=1 but trying to create a thread!\n\
                 Use --no-thread flag or set ZDX_HOME to a temp directory."
        );
    }

    /// Creates a new thread and associates it with a root path.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn new_with_root(root: &Path) -> Result<Self> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root_path = Some(root.display().to_string());
        Self::new_with_root_path_and_source(root_path, None)
    }

    /// Creates a new thread with a root path and handoff source.
    ///
    /// Use this when creating a thread from a `/handoff` command to record
    /// the parent thread relationship.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn new_with_root_and_source(root: &Path, handoff_from: Option<String>) -> Result<Self> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root_path = Some(root.display().to_string());
        Self::new_with_root_path_and_source(root_path, handoff_from)
    }

    fn new_with_root_path_and_source(
        root_path: Option<String>,
        handoff_from: Option<String>,
    ) -> Result<Self> {
        Self::guard_thread_creation();

        let id = generate_thread_id();
        let dir = threads_dir();
        fs::create_dir_all(&dir).context("Failed to create threads directory")?;

        let path = dir.join(format!("{id}.jsonl"));
        let is_new = !path.exists();

        Ok(Self {
            id,
            path,
            is_new,
            root_path,
            handoff_from,
            origin_kind: None,
            parent_thread_id: None,
            subagent_name: None,
        })
    }

    /// Creates or opens a thread with a specific ID.
    ///
    /// # Panics
    /// In tests, panics if `ZDX_HOME` is not set (to prevent polluting user's home).
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn with_id(id: String) -> Result<Self> {
        Self::guard_thread_creation();

        let dir = threads_dir();
        fs::create_dir_all(&dir).context("Failed to create threads directory")?;

        let path = dir.join(format!("{id}.jsonl"));
        let is_new = !path.exists();

        Ok(Self {
            id,
            path,
            is_new,
            root_path: None,
            handoff_from: None,
            origin_kind: None,
            parent_thread_id: None,
            subagent_name: None,
        })
    }

    /// Records subagent/helper lineage in the thread's meta line.
    ///
    /// Only effective before the meta event is written (i.e. on a new thread);
    /// on an existing thread the meta line is already persisted and this is a
    /// no-op for on-disk state.
    pub fn set_origin(
        &mut self,
        origin_kind: Option<String>,
        parent_thread_id: Option<String>,
        subagent_name: Option<String>,
    ) {
        self.origin_kind = origin_kind;
        self.parent_thread_id = parent_thread_id;
        self.subagent_name = subagent_name;
    }

    /// Records the parent thread this thread was handed off from.
    ///
    /// Use with [`Thread::with_id`] when the thread ID is fixed by the caller
    /// (e.g. Telegram topic threads). Only effective before the meta event is
    /// written (i.e. on a new thread); on an existing thread the meta line is
    /// already persisted and this is a no-op for on-disk state.
    pub fn set_handoff_from(&mut self, handoff_from: Option<String>) {
        self.handoff_from = handoff_from;
    }

    /// Ensures the meta event is written for new threads.
    fn ensure_meta(&mut self) -> Result<()> {
        if self.is_new {
            self.append_raw(&ThreadEvent::meta_with_lineage(
                self.root_path.clone(),
                self.handoff_from.clone(),
                self.origin_kind.clone(),
                self.parent_thread_id.clone(),
                self.subagent_name.clone(),
            ))?;
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
        writeln!(file, "{json}").context("Failed to write to thread file")?;

        Ok(())
    }

    /// Appends an event to the thread file.
    ///
    /// For new threads, automatically writes the meta event first.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn append(&mut self, event: &ThreadEvent) -> Result<()> {
        // Don't write meta before another meta
        if !matches!(event, ThreadEvent::Meta { .. }) {
            self.ensure_meta()?;
        }
        self.append_raw(event)
    }

    /// Reads all events from the thread file.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn read_events(&self) -> Result<Vec<ThreadEvent>> {
        read_thread_events(&self.path)
    }

    /// Updates the thread title stored in the meta event.
    ///
    /// Writes the meta line with the provided title (or clears it if None/empty),
    /// preserving all subsequent events. The update is performed atomically via
    /// write-to-temp-then-rename.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn set_title(&mut self, title: Option<String>) -> Result<Option<String>> {
        self.ensure_meta()?;
        let normalized = title.and_then(normalize_title);
        rewrite_meta_with_title(&self.path, normalized.clone())?;
        Ok(normalized)
    }

    /// Updates the thread root path stored in the meta event.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn set_root_path(&mut self, root: &Path) -> Result<()> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root_path = Some(root.display().to_string());
        self.ensure_meta()?;
        rewrite_meta_with_root(&self.path, root_path)?;
        Ok(())
    }

    /// Updates the model override stored in the meta event.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn set_model_override(&mut self, model_override: Option<String>) -> Result<()> {
        self.ensure_meta()?;
        rewrite_meta_with_model_override(&self.path, model_override)?;
        Ok(())
    }

    /// Updates the thinking override stored in the meta event.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn set_thinking_override(
        &mut self,
        thinking_override: Option<crate::config::ThinkingLevel>,
    ) -> Result<()> {
        self.ensure_meta()?;
        rewrite_meta_with_thinking_override(&self.path, thinking_override)?;
        Ok(())
    }

    /// Updates the pending topic-title flag stored in the meta event.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn set_pending_topic_title(&mut self, pending_topic_title: bool) -> Result<()> {
        self.ensure_meta()?;
        rewrite_meta_with_pending_topic_title(&self.path, pending_topic_title)?;
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
    writeln!(temp, "{new_meta}").context("Failed to write updated meta")?;

    for line in lines {
        let line = line.context("Failed to read thread line")?;
        writeln!(temp, "{line}").context("Failed to write thread line")?;
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
    writeln!(temp, "{new_meta}").context("Failed to write updated meta")?;

    for line in lines {
        let line = line.context("Failed to read thread line")?;
        writeln!(temp, "{line}").context("Failed to write thread line")?;
    }

    temp.sync_all().context("Failed to sync temp thread file")?;
    fs::rename(&temp_path, path).context("Failed to replace thread file")?;
    Ok(())
}

/// Rewrites the meta event with an updated model override, preserving the rest of the file.
fn rewrite_meta_with_model_override(path: &PathBuf, model_override: Option<String>) -> Result<()> {
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
            model_override: ref mut meta_model,
            ..
        } => {
            *meta_model = model_override;
        }
        _ => bail!("First thread event is not a meta event"),
    }

    let new_meta =
        serde_json::to_string(&meta_event).context("Failed to serialize updated meta event")?;
    writeln!(temp, "{new_meta}").context("Failed to write updated meta")?;

    for line in lines {
        let line = line.context("Failed to read thread line")?;
        writeln!(temp, "{line}").context("Failed to write thread line")?;
    }

    temp.sync_all().context("Failed to sync temp thread file")?;
    fs::rename(&temp_path, path).context("Failed to replace thread file")?;
    Ok(())
}

/// Rewrites the meta event with an updated thinking override, preserving the rest of the file.
fn rewrite_meta_with_thinking_override(
    path: &PathBuf,
    thinking_override: Option<crate::config::ThinkingLevel>,
) -> Result<()> {
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
            thinking_override: ref mut meta_thinking,
            ..
        } => {
            *meta_thinking = thinking_override;
        }
        _ => bail!("First thread event is not a meta event"),
    }

    let new_meta =
        serde_json::to_string(&meta_event).context("Failed to serialize updated meta event")?;
    writeln!(temp, "{new_meta}").context("Failed to write updated meta")?;

    for line in lines {
        let line = line.context("Failed to read thread line")?;
        writeln!(temp, "{line}").context("Failed to write thread line")?;
    }

    temp.sync_all().context("Failed to sync temp thread file")?;
    fs::rename(&temp_path, path).context("Failed to replace thread file")?;
    Ok(())
}

/// Rewrites the meta event with an updated pending topic-title flag, preserving the rest of the file.
fn rewrite_meta_with_pending_topic_title(path: &PathBuf, pending_topic_title: bool) -> Result<()> {
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
            pending_topic_title: ref mut meta_pending,
            ..
        } => {
            *meta_pending = pending_topic_title;
        }
        _ => bail!("First thread event is not a meta event"),
    }

    let new_meta =
        serde_json::to_string(&meta_event).context("Failed to serialize updated meta event")?;
    writeln!(temp, "{new_meta}").context("Failed to write updated meta")?;

    for line in lines {
        let line = line.context("Failed to read thread line")?;
        writeln!(temp, "{line}").context("Failed to write thread line")?;
    }

    temp.sync_all().context("Failed to sync temp thread file")?;
    fs::rename(&temp_path, path).context("Failed to replace thread file")?;
    Ok(())
}

/// Reads only the meta line to extract title (backward compatible).
/// Parsed meta fields from the first line of a thread file.
pub(crate) struct ThreadMeta {
    title: Option<String>,
    root_path: Option<String>,
    handoff_from: Option<String>,
    pub(crate) origin_kind: Option<String>,
    pub(crate) parent_thread_id: Option<String>,
    pub(crate) subagent_name: Option<String>,
    model_override: Option<String>,
    thinking_override: Option<crate::config::ThinkingLevel>,
    pending_topic_title: bool,
}

/// Reads and parses the meta line from a thread file (single open + parse).
pub(crate) fn read_meta(path: &PathBuf) -> Result<Option<ThreadMeta>> {
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
            return Ok(None);
        }
        if !first_line.trim().is_empty() {
            break;
        }
    }

    let parsed: ThreadEvent = match serde_json::from_str(&first_line) {
        Ok(event) => event,
        Err(_) => return Ok(None),
    };

    if let ThreadEvent::Meta {
        title,
        root_path,
        handoff_from,
        origin_kind,
        parent_thread_id,
        subagent_name,
        model_override,
        thinking_override,
        pending_topic_title,
        ..
    } = parsed
    {
        Ok(Some(ThreadMeta {
            title,
            root_path,
            handoff_from,
            origin_kind,
            parent_thread_id,
            subagent_name,
            model_override,
            thinking_override,
            pending_topic_title,
        }))
    } else {
        Ok(None)
    }
}

fn read_meta_title(path: &PathBuf) -> Result<Option<String>> {
    Ok(read_meta(path)?.and_then(|m| m.title))
}

fn read_meta_root_path(path: &PathBuf) -> Result<Option<String>> {
    Ok(read_meta(path)?.and_then(|m| m.root_path))
}

fn read_meta_model_override(path: &PathBuf) -> Result<Option<String>> {
    Ok(read_meta(path)?.and_then(|m| m.model_override))
}

fn read_meta_thinking_override(path: &PathBuf) -> Result<Option<crate::config::ThinkingLevel>> {
    Ok(read_meta(path)?.and_then(|m| m.thinking_override))
}

fn read_meta_pending_topic_title(path: &PathBuf) -> Result<bool> {
    Ok(read_meta(path)?.is_some_and(|m| m.pending_topic_title))
}

/// Generates a unique thread ID using UUID v4.
fn generate_thread_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Summary information about a saved thread.
#[derive(Debug, Clone, Default)]
pub struct ThreadSummary {
    pub id: String,
    pub title: Option<String>,
    pub root_path: Option<String>,
    pub modified: Option<SystemTime>,
    /// The ID of the parent thread this was handed off from (if any).
    pub handoff_from: Option<String>,
    /// Origin kind for threads spawned by another agent run (subagent/helper).
    /// `None` for top-level user threads.
    pub origin_kind: Option<String>,
    /// Parent thread id that spawned this run (subagent/helper).
    pub parent_thread_id: Option<String>,
    /// Named subagent when `origin_kind == "subagent"`.
    pub subagent_name: Option<String>,
}

impl ThreadSummary {
    /// Returns a display-friendly title (or short ID fallback).
    pub fn display_title(&self) -> String {
        display_title_or_short_id(self.title.as_deref(), &self.id)
    }

    /// Whether this thread was spawned by another agent run (subagent/helper)
    /// rather than started directly by a user.
    pub fn is_child_run(&self) -> bool {
        self.origin_kind.is_some()
    }
}

/// One thread `.jsonl` file with its filesystem metadata. This is a cheap
/// directory walk + per-file `stat` only — no file content is read (unlike
/// `list_threads`, which additionally parses each thread's meta line).
pub(crate) struct ThreadFileMeta {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) size: u64,
}

/// Lists `*.jsonl` thread files under `dir` with their mtime + size. A missing
/// directory yields an empty list; files whose metadata can't be read are still
/// listed (with `modified: None`, `size: 0`).
///
/// # Errors
/// Returns an error if the directory exists but cannot be read.
pub(crate) fn list_thread_files(dir: &Path) -> Result<Vec<ThreadFileMeta>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("Failed to read threads directory"),
    };

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem() else {
            continue;
        };
        let id = stem.to_string_lossy().to_string();
        let md = entry.metadata().ok();
        files.push(ThreadFileMeta {
            id,
            modified: md.as_ref().and_then(|m| m.modified().ok()),
            size: md.as_ref().map_or(0, std::fs::Metadata::len),
            path,
        });
    }
    Ok(files)
}

/// Lists top-level saved threads (sorted by modification time, newest first).
///
/// Child runs spawned by another agent (subagents and internal helpers, i.e.
/// any thread with `origin_kind` set) are excluded so they don't clutter
/// pickers, `threads list`, search, memory export, or latest-thread resume.
/// Use [`list_all_threads`] to include them. Usage stats are computed from a
/// separate raw file scan (`list_thread_files`) and still count child runs.
///
/// # Errors
/// Returns an error if the threads directory cannot be read.
pub fn list_threads() -> Result<Vec<ThreadSummary>> {
    Ok(list_all_threads()?
        .into_iter()
        .filter(|thread| !thread.is_child_run())
        .collect())
}

/// Lists all saved threads including child runs (subagents/helpers), sorted by
/// modification time, newest first.
///
/// # Errors
/// Returns an error if the threads directory cannot be read.
pub fn list_all_threads() -> Result<Vec<ThreadSummary>> {
    let mut threads: Vec<ThreadSummary> = list_thread_files(&threads_dir())?
        .into_iter()
        .map(|file| {
            let meta = read_meta(&file.path).unwrap_or(None);
            ThreadSummary {
                id: file.id,
                title: meta.as_ref().and_then(|m| m.title.clone()),
                root_path: meta.as_ref().and_then(|m| m.root_path.clone()),
                modified: file.modified,
                handoff_from: meta.as_ref().and_then(|m| m.handoff_from.clone()),
                origin_kind: meta.as_ref().and_then(|m| m.origin_kind.clone()),
                parent_thread_id: meta.as_ref().and_then(|m| m.parent_thread_id.clone()),
                subagent_name: meta.and_then(|m| m.subagent_name),
            }
        })
        .collect();

    // Sort by modification time (newest first)
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.modified));

    Ok(threads)
}

/// Loads and returns the events from a thread by ID.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn load_thread_events(id: &str) -> Result<Vec<ThreadEvent>> {
    let thread = Thread::with_id(id.to_string())?;
    thread.read_events()
}

/// Extracts the root path from thread events (if present).
pub fn extract_root_path_from_events(events: &[ThreadEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        ThreadEvent::Meta { root_path, .. } => root_path.clone(),
        _ => None,
    })
}

/// Extracts the thread title from events (if present).
pub fn extract_title_from_events(events: &[ThreadEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        ThreadEvent::Meta { title, .. } => title.clone(),
        _ => None,
    })
}

/// Extracts the parent handoff thread ID from events (if this thread was
/// created by a `/handoff`).
pub fn extract_handoff_from_from_events(events: &[ThreadEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        ThreadEvent::Meta { handoff_from, .. } => handoff_from.clone(),
        _ => None,
    })
}

/// Returns the ID of the most recently modified thread.
///
/// Returns None if no threads exist.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn latest_thread_id() -> Result<Option<String>> {
    let threads = list_threads()?;
    Ok(threads.into_iter().next().map(|s| s.id))
}

/// Reads a thread's title by ID (if present in meta).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn read_thread_title(id: &str) -> Result<Option<String>> {
    let path = threads_dir().join(format!("{id}.jsonl"));
    read_meta_title(&path)
}

/// Reads a thread's root path by ID (if present in meta).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn read_thread_root_path(id: &str) -> Result<Option<String>> {
    let path = threads_dir().join(format!("{id}.jsonl"));
    read_meta_root_path(&path)
}

/// Reads a thread's model override by ID (if present in meta).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn read_thread_model_override(id: &str) -> Result<Option<String>> {
    let path = threads_dir().join(format!("{id}.jsonl"));
    read_meta_model_override(&path)
}

/// Reads a thread's thinking override by ID (if present in meta).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn read_thread_thinking_override(id: &str) -> Result<Option<crate::config::ThinkingLevel>> {
    let path = threads_dir().join(format!("{id}.jsonl"));
    read_meta_thinking_override(&path)
}

/// Reads a thread's pending topic-title flag by ID.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn read_thread_pending_topic_title(id: &str) -> Result<bool> {
    let path = threads_dir().join(format!("{id}.jsonl"));
    read_meta_pending_topic_title(&path)
}

/// Updates a thread's title by ID.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn set_thread_title(id: &str, title: Option<String>) -> Result<Option<String>> {
    let path = threads_dir().join(format!("{id}.jsonl"));
    if !path.exists() {
        bail!("Thread '{id}' not found");
    }

    let mut thread = Thread::with_id(id.to_string())?;
    thread.set_title(title)
}

/// Thread options for CLI commands.
#[derive(Debug, Clone, Default)]
pub struct ThreadPersistenceOptions {
    /// Append to an existing thread by ID.
    pub thread_id: Option<String>,
    /// Do not save the thread.
    pub no_save: bool,
    /// Origin kind recorded in a new thread's meta (e.g. `subagent`,
    /// `helper:title`). Applied only when creating a new thread.
    pub origin_kind: Option<String>,
    /// Parent thread id recorded in a new thread's meta.
    pub parent_thread_id: Option<String>,
    /// Named subagent recorded in a new thread's meta.
    pub subagent_name: Option<String>,
}

impl ThreadPersistenceOptions {
    /// Resolves thread options into an optional Thread.
    ///
    /// Returns None if `no_save` is true.
    /// Returns existing thread if `thread_id` is provided.
    /// Returns new thread otherwise.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn resolve(&self, root: &Path) -> Result<Option<Thread>> {
        if self.no_save {
            return Ok(None);
        }

        if let Some(ref id) = self.thread_id {
            return Ok(Some(Thread::with_id(id.clone())?));
        }

        let mut thread = Thread::new_with_root(root)?;
        thread.set_origin(
            self.origin_kind.clone(),
            self.parent_thread_id.clone(),
            self.subagent_name.clone(),
        );
        Ok(Some(thread))
    }
}
