//! In-memory worker-thread manager backing the orchestrator profile.
//!
//! Workers are ordinary visible ZDX threads bound to one project root. The
//! manager owns one FIFO per worker: prompts for a single worker run strictly
//! serially through a child `zdx --thread <id> exec` process, while different
//! workers run concurrently. All manager state (ownership, queues, status,
//! completion channel, mirror links) is process-lifetime only by design — on
//! restart the thread JSONL transcripts survive and workers can be re-attached
//! with `send_message` (recovering their root and model override from the
//! transcript), but queued prompts and pending callbacks are lost.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::config::ThinkingLevel;
use crate::core::agent::{AgentEventRx, EventSender, create_event_channel};
use crate::core::events::AgentEvent;
use crate::core::subagent::{
    ExecSubagentOptions, SubagentStreamSink, run_exec_subagent_with_cancel, tool_input_preview,
};
use crate::core::thread_persistence;

/// Lifecycle status of a managed worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    /// Prompts are queued but none has started yet.
    Queued,
    /// A prompt is currently executing.
    Running,
    /// The last prompt finished successfully.
    Completed,
    /// The last prompt failed.
    Failed,
    /// The worker was cancelled (queue cleared, current turn stopped).
    Cancelled,
}

impl WorkerStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Emitted after every finished worker prompt (one event per worker turn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionEvent {
    /// Orchestrator that queued this turn; may differ from the current owner.
    pub owner_thread_id: String,
    pub worker_thread_id: String,
    pub status: WorkerStatus,
    pub final_text: Option<String>,
    pub error: Option<String>,
    /// Link to the worker's surface mirror (Telegram topic), when registered.
    pub mirror_url: Option<String>,
}

/// One live tool-activity update from a running worker turn, decoded from the
/// child runner's stream chunks (`{"t":"start"|"input"|"done"|"error", ...}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerActivity {
    ToolStarted { id: String, name: String },
    ToolInput { id: String, arg: String },
    ToolFinished { id: String, ok: bool },
}

impl WorkerActivity {
    fn from_stream_chunk(chunk: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(chunk).ok()?;
        let id = value.get("id")?.as_str()?.to_string();
        match value.get("t")?.as_str()? {
            "start" => Some(Self::ToolStarted {
                id,
                name: value.get("name")?.as_str()?.to_string(),
            }),
            "input" => Some(Self::ToolInput {
                id,
                arg: value.get("arg")?.as_str()?.to_string(),
            }),
            "done" => Some(Self::ToolFinished { id, ok: true }),
            "error" => Some(Self::ToolFinished { id, ok: false }),
            _ => None,
        }
    }
}

/// Manager lifecycle events consumed by the surface bridge (Telegram bot).
///
/// Per worker the channel order is `Created` → (`Prompted` | `Activity`)* →
/// `Completed` for every turn: `Created` is sent before the FIFO task exists,
/// and the runner drains all `Activity` for a turn before that turn's
/// `Completed` is emitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerEvent {
    /// A brand-new worker thread was created via `create_worker` (re-attached
    /// workers do not re-emit this).
    Created {
        owner_thread_id: String,
        worker_thread_id: String,
    },
    /// The orchestrator queued a follow-up prompt via `send_message`.
    /// Prompts typed in a legacy mirror topic do not emit this (the user's
    /// own message is already visible there).
    Prompted {
        worker_thread_id: String,
        prompt: String,
    },
    /// Live tool activity from the worker's running turn.
    Activity {
        worker_thread_id: String,
        activity: WorkerActivity,
    },
    /// A worker prompt finished (any terminal status).
    Completed(CompletionEvent),
    /// Waiting prompts were removed without running (cancel, remove), so an
    /// orchestrator may now be owed nothing more. No turn follows.
    QueueChanged { worker_thread_id: String },
}

/// A prompt waiting in a worker's FIFO. Ids are unique per manager process
/// and are how the orchestrator names one queued prompt to remove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedPrompt {
    pub id: u64,
    pub text: String,
    /// Orchestrator that queued the prompt; its completion is routed there.
    pub requester_thread_id: String,
}

/// Point-in-time view of a managed worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSnapshot {
    pub thread_id: String,
    pub owner_thread_id: String,
    pub root: PathBuf,
    pub status: WorkerStatus,
    pub queue_depth: usize,
    /// Prompts still waiting, in run order (the running prompt is not here).
    pub queue: Vec<QueuedPrompt>,
    pub latest_final_text: Option<String>,
    pub last_error: Option<String>,
    /// Link to the worker's surface mirror (Telegram topic), when registered.
    pub mirror_url: Option<String>,
    /// Tool the current turn is running right now, when one is in flight.
    /// `None` between tools or when no turn is running.
    pub current_tool: Option<String>,
    /// One-line primary input for `current_tool`, capped at 200 characters.
    pub current_tool_input: Option<String>,
    /// Seconds since the last tool activity on the current turn. `None` when
    /// no turn is running or no activity has arrived yet.
    pub seconds_since_last_activity: Option<u64>,
    /// Seconds the current turn has been running. `None` when idle.
    pub turn_elapsed_seconds: Option<u64>,
}

impl WorkerSnapshot {
    /// Idle means no prompt is running and nothing is queued.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.status != WorkerStatus::Running && self.queue_depth == 0
    }
}

/// One prompt execution request handed to the worker runner.
pub struct WorkerRunRequest {
    pub worker_thread_id: String,
    /// The orchestrator that queued this prompt (`QueuedPrompt::requester_thread_id`).
    pub owner_thread_id: String,
    pub root: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub cancel: CancellationToken,
    /// Manager event channel, for runners that relay live `Activity`.
    pub events: mpsc::UnboundedSender<WorkerEvent>,
}

type RunnerFuture = Pin<Box<dyn Future<Output = Result<String>> + Send>>;
type WorkerRunner = Arc<dyn Fn(WorkerRunRequest) -> RunnerFuture + Send + Sync>;

struct WorkerTool {
    id: String,
    name: String,
    input: Option<String>,
}

struct WorkerState {
    owner_thread_id: String,
    root: PathBuf,
    model: Option<String>,
    thinking_level: Option<ThinkingLevel>,
    queue: VecDeque<QueuedPrompt>,
    status: WorkerStatus,
    current_cancel: Option<CancellationToken>,
    latest_final_text: Option<String>,
    last_error: Option<String>,
    wake: Arc<Notify>,
    /// In-flight calls in start order; the newest supplies the status preview.
    current_tools: Vec<WorkerTool>,
    /// When the last tool activity arrived on the current turn.
    last_activity_at: Option<Instant>,
    /// When the current turn started running.
    turn_started_at: Option<Instant>,
    /// Last time this worker was created/attached, prompted, started a turn,
    /// or finished one. Orders `list_for_owner` newest first.
    touched_at: Instant,
    /// Orchestrator that queued the running prompt; its result goes there.
    running_requester: Option<String>,
}

impl WorkerState {
    fn snapshot(&self, thread_id: &str, mirror_url: Option<String>) -> WorkerSnapshot {
        WorkerSnapshot {
            thread_id: thread_id.to_string(),
            owner_thread_id: self.owner_thread_id.clone(),
            root: self.root.clone(),
            status: self.status,
            queue_depth: self.queue.len(),
            queue: self.queue.iter().cloned().collect(),
            latest_final_text: self.latest_final_text.clone(),
            last_error: self.last_error.clone(),
            mirror_url,
            current_tool: self.current_tools.last().map(|tool| tool.name.clone()),
            current_tool_input: self
                .current_tools
                .last()
                .and_then(|tool| tool.input.clone()),
            seconds_since_last_activity: self.last_activity_at.map(|at| at.elapsed().as_secs()),
            turn_elapsed_seconds: self.turn_started_at.map(|at| at.elapsed().as_secs()),
        }
    }
}

/// Process-lifetime manager for orchestrator-owned worker threads.
pub struct WorkerManager {
    state: Mutex<HashMap<String, WorkerState>>,
    /// Source of `QueuedPrompt::id`, unique across all workers in this process.
    next_prompt_id: std::sync::atomic::AtomicU64,
    /// Worker thread id → legacy surface mirror link, recovered by the bridge
    /// after a restart. Kept apart from `state` so a link can outlive/precede
    /// the worker's managed state. Lock order when both are needed: `state`
    /// first, then `mirrors`.
    mirrors: Mutex<HashMap<String, String>>,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
    runner: WorkerRunner,
}

impl WorkerManager {
    /// Creates a manager whose workers run through `zdx --thread <id> exec`.
    #[must_use]
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<WorkerEvent>) {
        // The default runner reaches back into the manager to record live tool
        // activity, so it is built with a weak self-reference.
        Self::with_runner_cyclic(|manager| {
            Arc::new(move |request| Box::pin(run_worker_prompt(request, Weak::clone(&manager))))
        })
    }

    /// Creates a manager with a custom prompt runner (used by tests).
    #[must_use]
    pub fn with_runner(runner: WorkerRunner) -> (Arc<Self>, mpsc::UnboundedReceiver<WorkerEvent>) {
        Self::with_runner_cyclic(|_| runner)
    }

    /// Creates a manager whose runner may hold a weak reference back to it.
    fn with_runner_cyclic(
        make_runner: impl FnOnce(Weak<Self>) -> WorkerRunner,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<WorkerEvent>) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut make_runner = Some(make_runner);
        let manager = Arc::new_cyclic(|weak| Self {
            state: Mutex::new(HashMap::new()),
            next_prompt_id: std::sync::atomic::AtomicU64::new(1),
            mirrors: Mutex::new(HashMap::new()),
            events_tx,
            runner: (make_runner.take().expect("runner factory runs once"))(Weak::clone(weak)),
        });
        (manager, events_rx)
    }

    /// Records live tool activity against a worker's current turn, so
    /// `Get_Thread_Status` can tell a worker mid-tool from one that is hung.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub fn record_activity(&self, worker_thread_id: &str, activity: &WorkerActivity) {
        let mut map = self.state.lock().expect("worker state lock poisoned");
        let Some(state) = map.get_mut(worker_thread_id) else {
            return;
        };
        state.last_activity_at = Some(Instant::now());
        match activity {
            WorkerActivity::ToolStarted { id, name } => {
                if !state.current_tools.iter().any(|tool| tool.id == *id) {
                    state.current_tools.push(WorkerTool {
                        id: id.clone(),
                        name: name.clone(),
                        input: None,
                    });
                }
            }
            WorkerActivity::ToolFinished { id, .. } => {
                state.current_tools.retain(|tool| tool.id != *id);
            }
            WorkerActivity::ToolInput { id, arg } => {
                if let Some(tool) = state.current_tools.iter_mut().find(|tool| tool.id == *id) {
                    let preview = tool_input_preview(arg);
                    tool.input = (!preview.is_empty()).then_some(preview);
                }
            }
        }
    }

    /// Registers the link of a worker's legacy surface mirror (a Telegram
    /// topic recovered after a restart). New workers are silent and never
    /// register one.
    ///
    /// # Panics
    /// Panics if the internal mirror lock is poisoned.
    pub fn set_mirror_url(&self, worker_thread_id: &str, url: String) {
        self.mirrors
            .lock()
            .expect("worker mirror lock poisoned")
            .insert(worker_thread_id.to_string(), url);
    }

    /// The registered legacy mirror link for a worker, if any.
    ///
    /// # Panics
    /// Panics if the internal mirror lock is poisoned.
    #[must_use]
    pub fn mirror_url(&self, worker_thread_id: &str) -> Option<String> {
        self.mirrors
            .lock()
            .expect("worker mirror lock poisoned")
            .get(worker_thread_id)
            .cloned()
    }

    /// Creates a new visible worker thread in `root`, queues its first prompt,
    /// and returns the worker thread id immediately.
    ///
    /// # Errors
    /// Returns an error if `root` is not an existing directory or the thread
    /// cannot be created.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub fn create_worker(
        self: &Arc<Self>,
        owner_thread_id: &str,
        root: &Path,
        prompt: &str,
        title: Option<&str>,
        model: Option<String>,
        thinking_level: Option<ThinkingLevel>,
    ) -> Result<String> {
        ensure!(!prompt.trim().is_empty(), "Worker prompt cannot be empty");
        let root = root
            .canonicalize()
            .with_context(|| format!("Project root does not exist: {}", root.display()))?;
        ensure!(
            root.is_dir(),
            "Project root is not a directory: {}",
            root.display()
        );

        let mut thread = thread_persistence::Thread::new_with_root(&root)
            .context("Failed to create worker thread")?;
        // The owning orchestrator is the worker's parent (lineage only: no
        // `origin_kind`, so the worker stays a visible top-level thread).
        // Persisted so surfaces can link back to the orchestrator after a
        // restart, when the manager no longer knows the owner.
        thread.set_origin(None, Some(owner_thread_id.to_string()), None);
        // Write the meta line now so the worker thread exists on disk (visible
        // in listings, re-attachable after restart) before the first turn runs.
        thread
            .set_root_path(&root)
            .context("Failed to persist worker thread root")?;
        if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
            thread
                .set_title(Some(title.to_string()))
                .context("Failed to set worker thread title")?;
        }
        // Record the overrides on the thread too, so surfaces that describe
        // the worker (status cards, `zdx threads`) report what it really runs
        // with; the runner still passes them explicitly to the child.
        if let Some(model_spec) = model.as_deref() {
            let persisted = thinking_level.map_or_else(
                || model_spec.to_string(),
                |level| crate::models::format_model_thinking(model_spec, level),
            );
            thread
                .set_model_override(Some(persisted))
                .context("Failed to set worker model override")?;
        }
        let worker_id = thread.id.clone();

        // Emit Created before the FIFO task can exist: a fast first prompt
        // could otherwise complete and emit Completed ahead of Created.
        let event = WorkerEvent::Created {
            owner_thread_id: owner_thread_id.to_string(),
            worker_thread_id: worker_id.clone(),
        };
        if self.events_tx.send(event).is_err() {
            tracing::debug!(worker = %worker_id, "Worker event channel closed");
        }

        self.attach_or_enqueue(
            &worker_id,
            Some(owner_thread_id),
            root,
            model,
            thinking_level,
            prompt,
        );
        Ok(worker_id)
    }

    /// Queues another prompt on a worker. Unmanaged threads (e.g. after a bot
    /// restart) are re-attached from their persisted root and model override,
    /// and owned by the calling orchestrator from then on.
    ///
    /// # Errors
    /// Returns an error if the thread does not exist or has no usable root.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub fn send_message(
        self: &Arc<Self>,
        owner_thread_id: &str,
        worker_thread_id: &str,
        message: &str,
    ) -> Result<WorkerSnapshot> {
        ensure!(!message.trim().is_empty(), "Worker message cannot be empty");

        // Emitted before the enqueue so the mirror post always precedes the
        // prompt's Completed event in the channel.
        let event = WorkerEvent::Prompted {
            worker_thread_id: worker_thread_id.to_string(),
            prompt: message.to_string(),
        };
        if self.events_tx.send(event).is_err() {
            tracing::debug!(worker = %worker_thread_id, "Worker event channel closed");
        }

        if let Some(snapshot) = self.try_enqueue(worker_thread_id, Some(owner_thread_id), message) {
            return Ok(snapshot);
        }

        let root = resolve_persisted_root(worker_thread_id)?;
        let model = resolve_persisted_model(worker_thread_id);
        Ok(self.attach_or_enqueue(
            worker_thread_id,
            Some(owner_thread_id),
            root,
            model,
            None,
            message,
        ))
    }

    /// Queues a prompt originating from a worker's mirror Telegram topic.
    ///
    /// Keeps the current owner when the worker is managed (so orchestrator
    /// callbacks continue to flow); an unmanaged thread (post-restart) is
    /// re-attached owning itself, which surfaces results only in the mirror
    /// topic.
    ///
    /// # Errors
    /// Returns an error if the thread does not exist or has no usable root.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub fn enqueue_from_topic(
        self: &Arc<Self>,
        worker_thread_id: &str,
        message: &str,
    ) -> Result<WorkerSnapshot> {
        ensure!(!message.trim().is_empty(), "Worker message cannot be empty");

        if let Some(snapshot) = self.try_enqueue(worker_thread_id, None, message) {
            return Ok(snapshot);
        }

        let root = resolve_persisted_root(worker_thread_id)?;
        let model = resolve_persisted_model(worker_thread_id);
        Ok(self.attach_or_enqueue(worker_thread_id, None, root, model, None, message))
    }

    /// Enqueues onto an already-managed worker; `None` when unmanaged.
    /// `owner` of `None` keeps the current owner.
    fn try_enqueue(
        &self,
        worker_thread_id: &str,
        owner: Option<&str>,
        message: &str,
    ) -> Option<WorkerSnapshot> {
        let snapshot = {
            let mut map = self.state.lock().expect("worker state lock poisoned");
            let state = map.get_mut(worker_thread_id)?;
            if let Some(owner) = owner {
                state.owner_thread_id = owner.to_string();
            }
            let prompt = self.queued_prompt(message, &state.owner_thread_id);
            state.queue.push_back(prompt);
            if state.status != WorkerStatus::Running {
                state.status = WorkerStatus::Queued;
            }
            state.wake.notify_one();
            state.snapshot(worker_thread_id, self.mirror_url(worker_thread_id))
        };
        Some(snapshot)
    }

    /// Enqueues onto an existing worker or registers a new one, atomically.
    ///
    /// The entry API makes concurrent attachments for the same thread safe:
    /// exactly one caller inserts state and spawns the FIFO task; every other
    /// caller enqueues onto the winner instead of overwriting it (model tool
    /// calls run concurrently, so two `Send_Thread_Message` calls can race).
    /// `owner` of `None` keeps the current owner (vacant entries own themselves).
    fn attach_or_enqueue(
        self: &Arc<Self>,
        worker_thread_id: &str,
        owner: Option<&str>,
        root: PathBuf,
        model: Option<String>,
        thinking_level: Option<ThinkingLevel>,
        prompt: &str,
    ) -> WorkerSnapshot {
        let (snapshot, spawn_wake) = {
            let mirror_url = self.mirror_url(worker_thread_id);
            let mut map = self.state.lock().expect("worker state lock poisoned");
            match map.entry(worker_thread_id.to_string()) {
                Entry::Occupied(mut occupied) => {
                    let state = occupied.get_mut();
                    if let Some(owner) = owner {
                        state.owner_thread_id = owner.to_string();
                    }
                    let prompt = self.queued_prompt(prompt, &state.owner_thread_id);
                    state.queue.push_back(prompt);
                    state.touched_at = Instant::now();
                    if state.status != WorkerStatus::Running {
                        state.status = WorkerStatus::Queued;
                    }
                    state.wake.notify_one();
                    (state.snapshot(worker_thread_id, mirror_url), None)
                }
                Entry::Vacant(vacant) => {
                    let wake = Arc::new(Notify::new());
                    let owner_thread_id = owner.unwrap_or(worker_thread_id).to_string();
                    let mut queue = VecDeque::new();
                    queue.push_back(self.queued_prompt(prompt, &owner_thread_id));
                    let state = vacant.insert(WorkerState {
                        owner_thread_id,
                        root,
                        model,
                        thinking_level,
                        queue,
                        status: WorkerStatus::Queued,
                        current_cancel: None,
                        latest_final_text: None,
                        last_error: None,
                        wake: Arc::clone(&wake),
                        current_tools: Vec::new(),
                        last_activity_at: None,
                        turn_started_at: None,
                        touched_at: Instant::now(),
                        running_requester: None,
                    });
                    (state.snapshot(worker_thread_id, mirror_url), Some(wake))
                }
            }
        };
        if let Some(wake) = spawn_wake {
            spawn_worker_task(Arc::clone(self), worker_thread_id.to_string(), wake);
        }
        snapshot
    }

    /// Snapshot of a single worker, when managed.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    #[must_use]
    pub fn snapshot(&self, worker_thread_id: &str) -> Option<WorkerSnapshot> {
        let mirror_url = self.mirror_url(worker_thread_id);
        self.state
            .lock()
            .expect("worker state lock poisoned")
            .get(worker_thread_id)
            .map(|state| state.snapshot(worker_thread_id, mirror_url))
    }

    /// Snapshots of every worker owned by `owner_thread_id`, most recently
    /// touched first (created, prompted, started, or finished a turn); ties
    /// fall back to thread id so the order is stable.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    #[must_use]
    pub fn list_for_owner(&self, owner_thread_id: &str) -> Vec<WorkerSnapshot> {
        self.list_where(|state| state.owner_thread_id == owner_thread_id)
    }

    /// Every managed worker, most recently touched first.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    #[must_use]
    pub fn list_all(&self) -> Vec<WorkerSnapshot> {
        self.list_where(|_| true)
    }

    fn list_where(&self, keep: impl Fn(&WorkerState) -> bool) -> Vec<WorkerSnapshot> {
        let map = self.state.lock().expect("worker state lock poisoned");
        let mut workers: Vec<(Instant, WorkerSnapshot)> = map
            .iter()
            .filter(|(_, state)| keep(state))
            .map(|(id, state)| (state.touched_at, state.snapshot(id, self.mirror_url(id))))
            .collect();
        workers.sort_by(|(a_at, a), (b_at, b)| {
            b_at.cmp(a_at).then_with(|| a.thread_id.cmp(&b.thread_id))
        });
        workers.into_iter().map(|(_, snapshot)| snapshot).collect()
    }

    /// True when no worker is running a prompt or has one queued.
    ///
    /// A worker that settled has already sent its `Completed` event (both
    /// happen under one lock), so a caller that checks this first and then
    /// finds the event channel empty knows no completion is still undelivered.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    #[must_use]
    pub fn all_idle(&self) -> bool {
        let map = self.state.lock().expect("worker state lock poisoned");
        map.values()
            .all(|state| state.status != WorkerStatus::Running && state.queue.is_empty())
    }

    /// True when no prompt queued by `requester_thread_id` is waiting or
    /// running on any worker, so no result is still owed to that thread
    /// except one already in the event channel (completions are sent under
    /// the lock that clears the running prompt, as for [`Self::all_idle`]).
    /// Results route by requester, not by the current owner.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    #[must_use]
    pub fn idle_for_requester(&self, requester_thread_id: &str) -> bool {
        let map = self.state.lock().expect("worker state lock poisoned");
        map.values().all(|state| {
            state.running_requester.as_deref() != Some(requester_thread_id)
                && !state
                    .queue
                    .iter()
                    .any(|prompt| prompt.requester_thread_id == requester_thread_id)
        })
    }

    /// Cancels the current turn (if any) and clears all queued prompts. The
    /// worker stays managed and a later `send_message` resumes it.
    ///
    /// # Errors
    /// Returns an error if the worker is not currently managed.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub fn cancel(&self, worker_thread_id: &str) -> Result<WorkerSnapshot> {
        let snapshot = {
            let mut map = self.state.lock().expect("worker state lock poisoned");
            let Some(state) = map.get_mut(worker_thread_id) else {
                bail!("Worker '{worker_thread_id}' is not managed");
            };
            state.queue.clear();
            if let Some(cancel) = state.current_cancel.as_ref() {
                // Status flips to Cancelled when the run returns; the FIFO task
                // will not start another prompt before the child is reaped.
                cancel.cancel();
            } else {
                state.status = WorkerStatus::Cancelled;
            }
            state.snapshot(worker_thread_id, self.mirror_url(worker_thread_id))
        };
        self.queue_changed(worker_thread_id);
        Ok(snapshot)
    }

    /// Sent after the state change, so a reader that sees the old state
    /// still gets this event to check again.
    fn queue_changed(&self, worker_thread_id: &str) {
        let event = WorkerEvent::QueueChanged {
            worker_thread_id: worker_thread_id.to_string(),
        };
        if self.events_tx.send(event).is_err() {
            tracing::debug!(worker = %worker_thread_id, "Worker event channel closed");
        }
    }

    /// Removes one prompt that is still waiting in a worker's FIFO. The
    /// running turn (if any) and every other queued prompt are untouched.
    ///
    /// # Errors
    /// Returns an error if the worker is not managed or no queued prompt has
    /// that id (it may already have started running).
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub fn remove_queued_prompt(
        &self,
        worker_thread_id: &str,
        prompt_id: u64,
    ) -> Result<(QueuedPrompt, WorkerSnapshot)> {
        let removed = {
            let mut map = self.state.lock().expect("worker state lock poisoned");
            let Some(state) = map.get_mut(worker_thread_id) else {
                bail!("Worker '{worker_thread_id}' is not managed");
            };
            let Some(index) = state.queue.iter().position(|p| p.id == prompt_id) else {
                bail!(
                    "Worker '{worker_thread_id}' has no queued prompt {prompt_id}; it may already be running or finished"
                );
            };
            let prompt = state.queue.remove(index).expect("index from position");
            (
                prompt,
                state.snapshot(worker_thread_id, self.mirror_url(worker_thread_id)),
            )
        };
        self.queue_changed(worker_thread_id);
        Ok(removed)
    }

    fn queued_prompt(&self, text: &str, requester_thread_id: &str) -> QueuedPrompt {
        QueuedPrompt {
            id: self
                .next_prompt_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            text: text.to_string(),
            requester_thread_id: requester_thread_id.to_string(),
        }
    }

    /// Updates a thread's title.
    ///
    /// For a managed worker the rewrite runs while holding the manager lock and
    /// only when no prompt is running: the title rewrite copies the whole JSONL
    /// aside and renames it, which would race a worker child appending to the
    /// same file. Starting a queued prompt also requires this lock, so nothing
    /// can begin mid-rewrite. Unmanaged threads update directly.
    ///
    /// # Errors
    /// Returns an error if the worker is mid-turn or the update fails.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub fn update_title(&self, thread_id: &str, title: &str) -> Result<Option<String>> {
        let map = self.state.lock().expect("worker state lock poisoned");
        if let Some(state) = map.get(thread_id) {
            ensure!(
                state.status != WorkerStatus::Running,
                "Worker '{thread_id}' is running; retry the title update when it is idle"
            );
        }
        thread_persistence::set_thread_title(thread_id, Some(title.to_string()))
    }
}

/// Resolves an existing thread's persisted project root for re-attachment.
fn resolve_persisted_root(worker_thread_id: &str) -> Result<PathBuf> {
    ensure!(
        thread_persistence::thread_exists(worker_thread_id),
        "Thread '{worker_thread_id}' not found"
    );
    let root = thread_persistence::read_thread_root_path(worker_thread_id)?
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow::anyhow!("Thread '{worker_thread_id}' has no recorded project root")
        })?;
    root.canonicalize().with_context(|| {
        format!(
            "Recorded project root for '{worker_thread_id}' no longer exists: {}",
            root.display()
        )
    })
}

/// Resolves an existing thread's persisted model override (`provider:model@thinking`)
/// for re-attachment, so a resumed worker keeps running the model its status
/// cards report instead of falling back to whatever the project root's config
/// layers resolve to now. `None` means the worker was never pinned to a model
/// and follows those layers by design.
fn resolve_persisted_model(worker_thread_id: &str) -> Option<String> {
    match thread_persistence::read_thread_model_override(worker_thread_id) {
        Ok(model) => model,
        Err(err) => {
            tracing::debug!(
                worker = %worker_thread_id,
                err = %format!("{err:#}"),
                "Failed to read worker model override"
            );
            None
        }
    }
}

/// Default runner: executes one prompt through `zdx --thread <id> exec` in the
/// worker's project root, resuming the worker thread's persisted history, and
/// relays the child's tool activity as `WorkerEvent::Activity`.
async fn run_worker_prompt(
    request: WorkerRunRequest,
    manager: Weak<WorkerManager>,
) -> Result<String> {
    let options = ExecSubagentOptions {
        model: request.model.clone(),
        thinking_level: request.thinking_level,
        thread_id: Some(request.worker_thread_id.clone()),
        activity_kind: Some("worker".to_string()),
        activity_parent_thread_id: Some(request.owner_thread_id.clone()),
        ..Default::default()
    };
    let (tx, rx) = create_event_channel();
    let sink = SubagentStreamSink {
        sender: EventSender::new(tx),
        parent_tool_id: request.worker_thread_id.clone(),
    };
    let forwarder = tokio::spawn(forward_activity(
        rx,
        request.worker_thread_id.clone(),
        request.events.clone(),
        manager,
    ));
    let result = run_exec_subagent_with_cancel(
        &request.root,
        &request.prompt,
        &options,
        Some(request.cancel.clone()),
        Some(sink),
    )
    .await;
    // The sink's sender dies with the child's stdout reader, so the forwarder
    // finishing means every Activity for this turn is already on the channel,
    // ahead of the Completed the FIFO emits after we return.
    let _ = forwarder.await;
    result
}

/// Translates the streaming sink's `ToolOutputDelta` chunks into
/// `WorkerEvent::Activity` until the sink is dropped.
async fn forward_activity(
    mut rx: AgentEventRx,
    worker_thread_id: String,
    events: mpsc::UnboundedSender<WorkerEvent>,
    manager: Weak<WorkerManager>,
) {
    while let Some(event) = rx.recv().await {
        let AgentEvent::ToolOutputDelta { chunk, .. } = event.as_ref() else {
            continue;
        };
        let Some(activity) = WorkerActivity::from_stream_chunk(chunk) else {
            continue;
        };
        // Record before forwarding so a status read racing the mirror update
        // never sees stale activity.
        if let Some(manager) = manager.upgrade() {
            manager.record_activity(&worker_thread_id, &activity);
        }
        let event = WorkerEvent::Activity {
            worker_thread_id: worker_thread_id.clone(),
            activity,
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

/// One FIFO task per worker: drains queued prompts strictly serially, updating
/// status and emitting one `CompletionEvent` per finished prompt.
fn spawn_worker_task(manager: Arc<WorkerManager>, worker_thread_id: String, wake: Arc<Notify>) {
    tokio::spawn(async move {
        loop {
            let next = {
                let mut map = manager.state.lock().expect("worker state lock poisoned");
                let Some(state) = map.get_mut(&worker_thread_id) else {
                    break;
                };
                state.queue.pop_front().map(|prompt| {
                    let cancel = CancellationToken::new();
                    state.current_cancel = Some(cancel.clone());
                    state.running_requester = Some(prompt.requester_thread_id.clone());
                    state.status = WorkerStatus::Running;
                    let now = Instant::now();
                    state.turn_started_at = Some(now);
                    state.touched_at = now;
                    state.last_activity_at = None;
                    state.current_tools.clear();
                    WorkerRunRequest {
                        worker_thread_id: worker_thread_id.clone(),
                        owner_thread_id: prompt.requester_thread_id,
                        root: state.root.clone(),
                        prompt: prompt.text,
                        model: state.model.clone(),
                        thinking_level: state.thinking_level,
                        cancel,
                        events: manager.events_tx.clone(),
                    }
                })
            };

            let Some(request) = next else {
                wake.notified().await;
                continue;
            };

            let cancel = request.cancel.clone();
            let requester_thread_id = request.owner_thread_id.clone();
            let result = (manager.runner)(request).await;

            // The event is sent under the same lock that settles the status, so
            // a reader that sees this worker idle also sees its completion
            // already in the channel (see `all_idle`).
            let mut map = manager.state.lock().expect("worker state lock poisoned");
            let Some(state) = map.get_mut(&worker_thread_id) else {
                break;
            };
            state.current_cancel = None;
            state.running_requester = None;
            state.turn_started_at = None;
            state.touched_at = Instant::now();
            state.last_activity_at = None;
            state.current_tools.clear();
            match &result {
                Ok(text) => {
                    state.status = WorkerStatus::Completed;
                    state.latest_final_text = Some(text.clone());
                    state.last_error = None;
                }
                Err(err) => {
                    state.status = if cancel.is_cancelled() {
                        WorkerStatus::Cancelled
                    } else {
                        WorkerStatus::Failed
                    };
                    state.last_error = Some(format!("{err:#}"));
                }
            }
            // Not the current owner: ownership can move mid-turn.
            let event = CompletionEvent {
                owner_thread_id: requester_thread_id,
                worker_thread_id: worker_thread_id.clone(),
                status: state.status,
                final_text: result.as_ref().ok().cloned(),
                error: state.last_error.clone(),
                mirror_url: manager.mirror_url(&worker_thread_id),
            };
            if manager
                .events_tx
                .send(WorkerEvent::Completed(event))
                .is_err()
            {
                tracing::debug!(worker = %worker_thread_id, "Worker event channel closed");
            }
            drop(map);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::test_support::temp_zdx_home;

    fn instant_runner(marker: &'static str) -> WorkerRunner {
        Arc::new(move |request: WorkerRunRequest| {
            Box::pin(async move { Ok(format!("{marker}:{}", request.prompt)) })
        })
    }

    /// Drains the event channel until the next completion, skipping the rest.
    async fn next_completion(rx: &mut mpsc::UnboundedReceiver<WorkerEvent>) -> CompletionEvent {
        loop {
            match rx.recv().await.expect("worker event channel open") {
                WorkerEvent::Completed(event) => return event,
                WorkerEvent::Created { .. }
                | WorkerEvent::Prompted { .. }
                | WorkerEvent::Activity { .. }
                | WorkerEvent::QueueChanged { .. } => {}
            }
        }
    }

    /// Polls until the worker is idle. The manager has no blocking wait: the
    /// orchestrator is resumed by completion callbacks, not by holding a turn.
    async fn wait_idle(manager: &Arc<WorkerManager>, id: &str) -> WorkerSnapshot {
        for _ in 0..500 {
            let snapshot = manager.snapshot(id).expect("worker is managed");
            if snapshot.is_idle() {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("worker should settle quickly");
    }

    #[tokio::test]
    async fn idle_workers_have_already_sent_their_completion() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();
        let release = Arc::new(Notify::new());
        let gate = Arc::clone(&release);
        let runner: WorkerRunner = Arc::new(move |_request: WorkerRunRequest| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                gate.notified().await;
                Ok("done".to_string())
            })
        });
        let (manager, mut rx) = WorkerManager::with_runner(runner);
        assert!(manager.all_idle());

        manager
            .create_worker("owner", project.path(), "work", None, None, None)
            .unwrap();
        assert!(!manager.all_idle());

        release.notify_one();
        for _ in 0..500 {
            if manager.all_idle() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(manager.all_idle());
        // Already queued the moment the worker reads as idle: never a later send.
        let mut saw_completion = false;
        while let Ok(event) = rx.try_recv() {
            saw_completion |= matches!(event, WorkerEvent::Completed(_));
        }
        assert!(saw_completion);
    }

    #[tokio::test]
    async fn create_worker_persists_thread_and_completes_prompt() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();
        let (manager, mut completion_rx) = WorkerManager::with_runner(instant_runner("done"));

        let worker_id = manager
            .create_worker(
                "owner-thread",
                project.path(),
                "build it",
                Some("Worker A"),
                None,
                None,
            )
            .unwrap();

        // The worker thread exists on disk immediately, visible and rooted.
        let summary = thread_persistence::read_thread_summary(&worker_id)
            .unwrap()
            .expect("worker thread persisted");
        assert!(summary.origin_kind.is_none());
        assert_eq!(summary.parent_thread_id.as_deref(), Some("owner-thread"));
        assert!(!summary.is_child_run());
        assert_eq!(summary.title.as_deref(), Some("Worker A"));
        assert!(summary.root_path.is_some());

        let snapshot = wait_idle(&manager, &worker_id).await;
        assert_eq!(snapshot.status, WorkerStatus::Completed);
        assert_eq!(snapshot.latest_final_text.as_deref(), Some("done:build it"));

        let event = next_completion(&mut completion_rx).await;
        assert_eq!(event.owner_thread_id, "owner-thread");
        assert_eq!(event.worker_thread_id, worker_id);
        assert_eq!(event.status, WorkerStatus::Completed);
    }

    #[tokio::test]
    async fn prompts_on_one_worker_run_serially() {
        static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        static MAX_SEEN: AtomicUsize = AtomicUsize::new(0);
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                let now = CONCURRENT.fetch_add(1, Ordering::SeqCst) + 1;
                MAX_SEEN.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                CONCURRENT.fetch_sub(1, Ordering::SeqCst);
                Ok(request.prompt)
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);

        let worker_id = manager
            .create_worker("owner", project.path(), "first", None, None, None)
            .unwrap();
        manager.send_message("owner", &worker_id, "second").unwrap();

        let first = next_completion(&mut completion_rx).await;
        let second = next_completion(&mut completion_rx).await;
        assert_eq!(first.final_text.as_deref(), Some("first"));
        assert_eq!(second.final_text.as_deref(), Some("second"));
        assert_eq!(
            MAX_SEEN.load(Ordering::SeqCst),
            1,
            "one worker's prompts must never overlap"
        );
    }

    /// Regression: B messages the worker during A's turn; each gets its own result.
    #[tokio::test]
    async fn each_turn_reports_to_the_orchestrator_that_queued_it() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();
        let release = Arc::new(Notify::new());
        let gate = Arc::clone(&release);
        let runner: WorkerRunner = Arc::new(move |request: WorkerRunRequest| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                if request.prompt == "a-task" {
                    gate.notified().await;
                }
                Ok(format!("{}:{}", request.owner_thread_id, request.prompt))
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);

        let worker_id = manager
            .create_worker("orchestrator-a", project.path(), "a-task", None, None, None)
            .unwrap();
        for _ in 0..500 {
            if manager.snapshot(&worker_id).unwrap().status == WorkerStatus::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let snapshot = manager
            .send_message("orchestrator-b", &worker_id, "b-task")
            .unwrap();
        assert_eq!(snapshot.owner_thread_id, "orchestrator-b");
        assert_eq!(snapshot.queue[0].requester_thread_id, "orchestrator-b");
        release.notify_one();

        let first = next_completion(&mut completion_rx).await;
        assert_eq!(first.owner_thread_id, "orchestrator-a");
        assert_eq!(first.final_text.as_deref(), Some("orchestrator-a:a-task"));
        let second = next_completion(&mut completion_rx).await;
        assert_eq!(second.owner_thread_id, "orchestrator-b");
        assert_eq!(second.final_text.as_deref(), Some("orchestrator-b:b-task"));
        assert_eq!(
            manager.snapshot(&worker_id).unwrap().owner_thread_id,
            "orchestrator-b",
            "the latest orchestrator to message the worker still owns it"
        );
    }

    #[tokio::test]
    async fn different_workers_run_concurrently() {
        static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        static MAX_SEEN: AtomicUsize = AtomicUsize::new(0);
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                let now = CONCURRENT.fetch_add(1, Ordering::SeqCst) + 1;
                MAX_SEEN.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                CONCURRENT.fetch_sub(1, Ordering::SeqCst);
                Ok(request.prompt)
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);

        let a = manager
            .create_worker("owner", project.path(), "a", None, None, None)
            .unwrap();
        let b = manager
            .create_worker("owner", project.path(), "b", None, None, None)
            .unwrap();
        assert_ne!(a, b);

        next_completion(&mut completion_rx).await;
        next_completion(&mut completion_rx).await;
        assert!(
            MAX_SEEN.load(Ordering::SeqCst) >= 2,
            "independent workers should overlap"
        );
    }

    #[tokio::test]
    async fn cancel_clears_queue_and_stops_current_turn() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                if request.prompt == "long task" {
                    request.cancel.cancelled().await;
                    anyhow::bail!("Subagent cancelled")
                }
                Ok(request.prompt)
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);

        let worker_id = manager
            .create_worker("owner", project.path(), "long task", None, None, None)
            .unwrap();
        manager.send_message("owner", &worker_id, "queued").unwrap();

        // Give the FIFO a moment to start the first prompt.
        tokio::time::sleep(Duration::from_millis(20)).await;
        manager.cancel(&worker_id).unwrap();

        let event = next_completion(&mut completion_rx).await;
        assert_eq!(event.status, WorkerStatus::Cancelled);

        let snapshot = wait_idle(&manager, &worker_id).await;
        assert_eq!(snapshot.status, WorkerStatus::Cancelled);
        assert_eq!(snapshot.queue_depth, 0, "cancel must clear queued prompts");

        // The worker is preserved: a later message resumes it.
        manager.send_message("owner", &worker_id, "resume").unwrap();
        let event = next_completion(&mut completion_rx).await;
        assert_eq!(event.status, WorkerStatus::Completed);
        assert_eq!(event.final_text.as_deref(), Some("resume"));
    }

    #[tokio::test]
    async fn remove_queued_prompt_drops_one_item_and_keeps_the_turn() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let (release_tx, release_rx) = tokio::sync::watch::channel(false);
        let runner: WorkerRunner = Arc::new(move |request: WorkerRunRequest| {
            let mut release = release_rx.clone();
            Box::pin(async move {
                if request.prompt == "long task" {
                    release.wait_for(|released| *released).await.unwrap();
                }
                Ok(request.prompt)
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);

        let worker_id = manager
            .create_worker("owner", project.path(), "long task", None, None, None)
            .unwrap();
        // Give the FIFO a moment to start the first prompt.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let stale = manager.send_message("owner", &worker_id, "stale").unwrap();
        let handoff = manager
            .send_message("owner", &worker_id, "handoff")
            .unwrap();

        let stale_id = stale.queue.last().unwrap().id;
        assert_eq!(
            handoff
                .queue
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>(),
            vec!["stale", "handoff"],
            "snapshot lists waiting prompts in run order"
        );

        let (removed, snapshot) = manager.remove_queued_prompt(&worker_id, stale_id).unwrap();
        assert_eq!(removed.text, "stale");
        assert_eq!(snapshot.status, WorkerStatus::Running);
        assert_eq!(snapshot.queue.len(), 1);
        assert_eq!(snapshot.queue[0].text, "handoff");
        assert!(
            manager.remove_queued_prompt(&worker_id, stale_id).is_err(),
            "a removed id is gone"
        );

        release_tx.send(true).unwrap();
        let first = next_completion(&mut completion_rx).await;
        let second = next_completion(&mut completion_rx).await;
        assert_eq!(first.final_text.as_deref(), Some("long task"));
        assert_eq!(second.final_text.as_deref(), Some("handoff"));
    }

    #[tokio::test]
    async fn send_message_reattaches_unmanaged_thread_from_persisted_root() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        // Simulate a pre-restart worker: a thread on disk with a root, unknown
        // to the (fresh) manager.
        let mut thread = thread_persistence::Thread::new_with_root(project.path()).unwrap();
        thread.set_root_path(project.path()).unwrap();
        let existing_id = thread.id.clone();

        let (manager, mut completion_rx) = WorkerManager::with_runner(instant_runner("resumed"));
        let snapshot = manager
            .send_message("new-owner", &existing_id, "continue")
            .unwrap();
        assert_eq!(snapshot.owner_thread_id, "new-owner");

        let event = next_completion(&mut completion_rx).await;
        assert_eq!(event.worker_thread_id, existing_id);
        assert_eq!(event.final_text.as_deref(), Some("resumed:continue"));
    }

    /// A resumed worker must run the model its thread (and every status card
    /// reading it) reports, not whatever the project root's config layers
    /// resolve to at resume time.
    #[tokio::test]
    async fn reattached_worker_runs_its_persisted_model_override() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let mut orchestrator_resumed =
            thread_persistence::Thread::new_with_root(project.path()).unwrap();
        orchestrator_resumed.set_root_path(project.path()).unwrap();
        orchestrator_resumed
            .set_model_override(Some("provider:pinned-model@high".to_string()))
            .unwrap();
        let orchestrator_resumed_id = orchestrator_resumed.id.clone();

        let mut topic_resumed = thread_persistence::Thread::new_with_root(project.path()).unwrap();
        topic_resumed.set_root_path(project.path()).unwrap();
        topic_resumed
            .set_model_override(Some("provider:other-pinned-model@low".to_string()))
            .unwrap();
        let topic_resumed_id = topic_resumed.id.clone();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                Ok(request
                    .model
                    .clone()
                    .unwrap_or_else(|| "<unpinned>".to_string()))
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);

        manager
            .send_message("new-owner", &orchestrator_resumed_id, "continue")
            .unwrap();
        let event = next_completion(&mut completion_rx).await;
        assert_eq!(
            event.final_text.as_deref(),
            Some("provider:pinned-model@high")
        );

        manager
            .enqueue_from_topic(&topic_resumed_id, "continue")
            .unwrap();
        let event = next_completion(&mut completion_rx).await;
        assert_eq!(
            event.final_text.as_deref(),
            Some("provider:other-pinned-model@low")
        );
    }

    #[tokio::test]
    async fn concurrent_reattach_keeps_both_prompts_and_one_fifo() {
        static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        static MAX_SEEN: AtomicUsize = AtomicUsize::new(0);
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let mut thread = thread_persistence::Thread::new_with_root(project.path()).unwrap();
        thread.set_root_path(project.path()).unwrap();
        let existing_id = thread.id.clone();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                let now = CONCURRENT.fetch_add(1, Ordering::SeqCst) + 1;
                MAX_SEEN.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                CONCURRENT.fetch_sub(1, Ordering::SeqCst);
                Ok(request.prompt)
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);

        // Two concurrent sends to the same unmanaged thread: both prompts must
        // survive and run serially through a single FIFO.
        let a = {
            let manager = Arc::clone(&manager);
            let id = existing_id.clone();
            tokio::spawn(async move { manager.send_message("owner-a", &id, "first") })
        };
        let b = {
            let manager = Arc::clone(&manager);
            let id = existing_id.clone();
            tokio::spawn(async move { manager.send_message("owner-b", &id, "second") })
        };
        a.await.unwrap().unwrap();
        b.await.unwrap().unwrap();

        let mut texts = vec![
            next_completion(&mut completion_rx)
                .await
                .final_text
                .unwrap(),
            next_completion(&mut completion_rx)
                .await
                .final_text
                .unwrap(),
        ];
        texts.sort();
        assert_eq!(texts, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(
            MAX_SEEN.load(Ordering::SeqCst),
            1,
            "a racing reattach must never spawn a second concurrent FIFO"
        );
    }

    #[tokio::test]
    async fn update_title_rejects_running_worker_and_applies_when_idle() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                request.cancel.cancelled().await;
                anyhow::bail!("Subagent cancelled")
            })
        });
        let (manager, mut completion_rx) = WorkerManager::with_runner(runner);
        let worker_id = manager
            .create_worker("owner", project.path(), "long task", None, None, None)
            .unwrap();

        // Give the FIFO a moment to start the prompt.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let err = manager.update_title(&worker_id, "New Title").unwrap_err();
        assert!(err.to_string().contains("is running"), "got: {err}");

        manager.cancel(&worker_id).unwrap();
        next_completion(&mut completion_rx).await;
        wait_idle(&manager, &worker_id).await;

        let applied = manager.update_title(&worker_id, "New Title").unwrap();
        assert_eq!(applied.as_deref(), Some("New Title"));
        assert_eq!(
            thread_persistence::read_thread_title(&worker_id)
                .unwrap()
                .as_deref(),
            Some("New Title")
        );
    }

    #[tokio::test]
    async fn create_worker_emits_created_before_completion() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();
        let (manager, mut events_rx) = WorkerManager::with_runner(instant_runner("ok"));

        let worker_id = manager
            .create_worker(
                "owner",
                project.path(),
                "task",
                Some("Mirror Me"),
                None,
                None,
            )
            .unwrap();

        match events_rx.recv().await.unwrap() {
            WorkerEvent::Created {
                owner_thread_id,
                worker_thread_id,
            } => {
                assert_eq!(owner_thread_id, "owner");
                assert_eq!(worker_thread_id, worker_id);
            }
            other => panic!("expected Created first, got {other:?}"),
        }
        let event = next_completion(&mut events_rx).await;
        assert_eq!(event.status, WorkerStatus::Completed);
    }

    #[tokio::test]
    async fn enqueue_from_topic_keeps_owner_and_reattaches_self_owned() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();
        let (manager, mut completion_rx) = WorkerManager::with_runner(instant_runner("r"));

        // Managed: owner is preserved.
        let worker_id = manager
            .create_worker("orch-thread", project.path(), "first", None, None, None)
            .unwrap();
        let snapshot = manager.enqueue_from_topic(&worker_id, "steer").unwrap();
        assert_eq!(snapshot.owner_thread_id, "orch-thread");
        next_completion(&mut completion_rx).await;
        next_completion(&mut completion_rx).await;

        // Unmanaged (post-restart shape): re-attached owning itself.
        let mut thread = thread_persistence::Thread::new_with_root(project.path()).unwrap();
        thread.set_root_path(project.path()).unwrap();
        let orphan_id = thread.id.clone();
        let snapshot = manager.enqueue_from_topic(&orphan_id, "resume").unwrap();
        assert_eq!(snapshot.owner_thread_id, orphan_id);
        let event = next_completion(&mut completion_rx).await;
        assert_eq!(event.worker_thread_id, orphan_id);
    }

    #[tokio::test]
    async fn create_worker_rejects_missing_root() {
        let _home = temp_zdx_home();
        let (manager, _completion_rx) = WorkerManager::with_runner(instant_runner("x"));
        let err = manager
            .create_worker(
                "owner",
                Path::new("/nonexistent/zdx-worker-root"),
                "task",
                None,
                None,
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("Project root does not exist"));
    }

    #[tokio::test]
    async fn runner_activity_lands_before_completed() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                for activity in [
                    WorkerActivity::ToolStarted {
                        id: "t1".to_string(),
                        name: "bash".to_string(),
                    },
                    WorkerActivity::ToolInput {
                        id: "t1".to_string(),
                        arg: "cargo test".to_string(),
                    },
                    WorkerActivity::ToolFinished {
                        id: "t1".to_string(),
                        ok: true,
                    },
                ] {
                    request
                        .events
                        .send(WorkerEvent::Activity {
                            worker_thread_id: request.worker_thread_id.clone(),
                            activity,
                        })
                        .unwrap();
                }
                Ok(request.prompt)
            })
        });
        let (manager, mut events_rx) = WorkerManager::with_runner(runner);
        let worker_id = manager
            .create_worker("owner", project.path(), "go", None, None, None)
            .unwrap();

        let mut kinds = Vec::new();
        loop {
            match events_rx.recv().await.unwrap() {
                WorkerEvent::Created { .. } => kinds.push("created"),
                WorkerEvent::Activity {
                    worker_thread_id, ..
                } => {
                    assert_eq!(worker_thread_id, worker_id);
                    kinds.push("activity");
                }
                WorkerEvent::Completed(_) => {
                    kinds.push("completed");
                    break;
                }
                WorkerEvent::Prompted { .. } => kinds.push("prompted"),
                WorkerEvent::QueueChanged { .. } => kinds.push("queue_changed"),
            }
        }
        assert_eq!(
            kinds,
            vec!["created", "activity", "activity", "activity", "completed"]
        );
    }

    #[test]
    fn activity_decodes_stream_chunks() {
        assert_eq!(
            WorkerActivity::from_stream_chunk(r#"{"t":"start","id":"a","name":"edit"}"#),
            Some(WorkerActivity::ToolStarted {
                id: "a".to_string(),
                name: "edit".to_string()
            })
        );
        assert_eq!(
            WorkerActivity::from_stream_chunk(r#"{"t":"input","id":"a","arg":"src/x.rs"}"#),
            Some(WorkerActivity::ToolInput {
                id: "a".to_string(),
                arg: "src/x.rs".to_string()
            })
        );
        assert_eq!(
            WorkerActivity::from_stream_chunk(r#"{"t":"error","id":"a"}"#),
            Some(WorkerActivity::ToolFinished {
                id: "a".to_string(),
                ok: false
            })
        );
        assert_eq!(WorkerActivity::from_stream_chunk("not json"), None);
        assert_eq!(
            WorkerActivity::from_stream_chunk(r#"{"t":"other","id":"a"}"#),
            None
        );
    }

    /// The orchestrator card lists the first few workers, so the owner's list
    /// must put the most recently touched worker first, not the oldest id.
    #[tokio::test]
    async fn list_for_owner_puts_most_recently_touched_first() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();
        let (manager, _events_rx) = WorkerManager::with_runner(instant_runner("r"));

        let mut ids = Vec::new();
        for prompt in ["one", "two", "three"] {
            let id = manager
                .create_worker("owner", project.path(), prompt, None, None, None)
                .unwrap();
            wait_idle(&manager, &id).await;
            ids.push(id);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let order = |manager: &Arc<WorkerManager>| -> Vec<String> {
            manager
                .list_for_owner("owner")
                .into_iter()
                .map(|w| w.thread_id)
                .collect()
        };
        assert_eq!(
            order(&manager),
            vec![ids[2].clone(), ids[1].clone(), ids[0].clone()]
        );

        // Prompting the oldest worker moves it to the top.
        manager.send_message("owner", &ids[0], "again").unwrap();
        wait_idle(&manager, &ids[0]).await;
        assert_eq!(
            order(&manager),
            vec![ids[0].clone(), ids[2].clone(), ids[1].clone()]
        );
    }

    #[tokio::test]
    async fn legacy_mirror_url_flows_into_snapshots_and_completions() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                Ok(request.prompt)
            })
        });
        let (manager, mut events_rx) = WorkerManager::with_runner(runner);
        let worker_id = manager
            .create_worker("owner", project.path(), "go", None, None, None)
            .unwrap();

        // New workers are silent: nothing registers a mirror for them.
        assert_eq!(manager.mirror_url(&worker_id), None);

        manager.set_mirror_url(&worker_id, "https://t.me/c/1/2".to_string());
        assert_eq!(
            manager.snapshot(&worker_id).unwrap().mirror_url.as_deref(),
            Some("https://t.me/c/1/2")
        );
        let event = next_completion(&mut events_rx).await;
        assert_eq!(event.mirror_url.as_deref(), Some("https://t.me/c/1/2"));
    }

    #[tokio::test]
    async fn snapshot_reports_current_tool_and_turn_timing() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        // A runner that starts a tool, reports it, and blocks until released
        // so the snapshot is taken with a tool genuinely in flight.
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release = Arc::new(Mutex::new(Some(release_rx)));
        let runner: WorkerRunner = Arc::new(move |request: WorkerRunRequest| {
            let release = Arc::clone(&release);
            Box::pin(async move {
                request
                    .events
                    .send(WorkerEvent::Activity {
                        worker_thread_id: request.worker_thread_id.clone(),
                        activity: WorkerActivity::ToolStarted {
                            id: "t1".to_string(),
                            name: "bash".to_string(),
                        },
                    })
                    .unwrap();
                let rx = release.lock().expect("release lock").take().unwrap();
                let _ = rx.await;
                Ok("done".to_string())
            })
        });

        let (manager, _rx) = WorkerManager::with_runner(runner);
        let worker_id = manager
            .create_worker("owner", project.path(), "go", None, None, None)
            .unwrap();

        // The FIFO task starts the turn asynchronously; wait for it so the
        // turn timer is actually running before asserting on it.
        for _ in 0..200 {
            if manager.snapshot(&worker_id).unwrap().status == WorkerStatus::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            manager.snapshot(&worker_id).unwrap().status,
            WorkerStatus::Running
        );

        // Custom runners send Activity straight to the event channel, so drive
        // the tap the way the default runner's forwarder does.
        manager.record_activity(
            &worker_id,
            &WorkerActivity::ToolStarted {
                id: "t1".to_string(),
                name: "bash".to_string(),
            },
        );

        let running = manager.snapshot(&worker_id).unwrap();
        assert_eq!(running.current_tool.as_deref(), Some("bash"));
        assert_eq!(running.current_tool_input, None);
        assert!(running.seconds_since_last_activity.is_some());
        assert!(running.turn_elapsed_seconds.is_some());

        manager.record_activity(
            &worker_id,
            &WorkerActivity::ToolInput {
                id: "t1".into(),
                arg: "./gradlew assembleDebug".into(),
            },
        );
        assert_eq!(
            manager
                .snapshot(&worker_id)
                .unwrap()
                .current_tool_input
                .as_deref(),
            Some("./gradlew assembleDebug")
        );

        // Finishing the tool clears the current tool but keeps the turn timer.
        manager.record_activity(
            &worker_id,
            &WorkerActivity::ToolFinished {
                id: "t1".to_string(),
                ok: true,
            },
        );
        let between = manager.snapshot(&worker_id).unwrap();
        assert_eq!(between.current_tool, None);
        assert_eq!(between.current_tool_input, None);
        assert!(between.turn_elapsed_seconds.is_some());

        let _ = release_tx.send(());
        manager.cancel(&worker_id).ok();
    }

    #[test]
    fn record_activity_ignores_unknown_workers() {
        let _home = temp_zdx_home();
        let (manager, _rx) =
            WorkerManager::with_runner(Arc::new(|_| Box::pin(async { Ok(String::new()) })));
        // Must not panic or insert state for a worker it does not know.
        manager.record_activity(
            "nope",
            &WorkerActivity::ToolStarted {
                id: "t".to_string(),
                name: "bash".to_string(),
            },
        );
        assert!(manager.snapshot("nope").is_none());
    }

    /// Exercises the real tap: `forward_activity` (the single funnel the
    /// default runner uses) must record activity onto manager state, not just
    /// forward it to the mirror. The unit test above drives `record_activity`
    /// directly, so this is what proves the wiring.
    #[tokio::test]
    async fn forward_activity_records_onto_manager_state() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let ready = Arc::new(Notify::new());
        let runner_ready = Arc::clone(&ready);
        let (manager, _rx) = WorkerManager::with_runner(Arc::new(move |request| {
            let ready = Arc::clone(&runner_ready);
            Box::pin(async move {
                ready.notify_one();
                request.cancel.cancelled().await;
                Ok(String::new())
            })
        }));
        let worker_id = manager
            .create_worker("owner", project.path(), "go", None, None, None)
            .unwrap();
        ready.notified().await;

        let (tx, rx) = crate::core::agent::create_event_channel();
        let (fwd_tx, mut fwd_rx) = mpsc::unbounded_channel();
        let forwarder = tokio::spawn(forward_activity(
            rx,
            worker_id.clone(),
            fwd_tx,
            Arc::downgrade(&manager),
        ));

        tx.send(Arc::new(AgentEvent::ToolOutputDelta {
            id: worker_id.clone(),
            chunk: r#"{"t":"start","id":"t1","name":"grep"}"#.to_string(),
        }))
        .unwrap();

        // The forwarded event lands only after the tap ran, so receiving it
        // means the state write already happened.
        let activity_event = fwd_rx.recv().await.expect("activity forwarded");
        assert!(matches!(activity_event, WorkerEvent::Activity { .. }));

        let snap = manager.snapshot(&worker_id).unwrap();
        assert_eq!(snap.current_tool.as_deref(), Some("grep"));
        assert!(snap.seconds_since_last_activity.is_some());

        tx.send(Arc::new(AgentEvent::ToolOutputDelta {
            id: worker_id.clone(),
            chunk: r#"{"t":"input","id":"t1","arg":"TODO src"}"#.to_string(),
        }))
        .unwrap();
        fwd_rx.recv().await.expect("input forwarded");
        assert_eq!(
            manager
                .snapshot(&worker_id)
                .unwrap()
                .current_tool_input
                .as_deref(),
            Some("TODO src")
        );

        tx.send(Arc::new(AgentEvent::ToolOutputDelta {
            id: worker_id.clone(),
            chunk: r#"{"t":"done","id":"t1"}"#.to_string(),
        }))
        .unwrap();
        fwd_rx.recv().await.expect("finish forwarded");
        assert_eq!(manager.snapshot(&worker_id).unwrap().current_tool, None);
        assert_eq!(
            manager.snapshot(&worker_id).unwrap().current_tool_input,
            None
        );

        drop(tx);
        forwarder.await.ok();
        manager.cancel(&worker_id).ok();
        wait_idle(&manager, &worker_id).await;
    }

    #[tokio::test]
    async fn concurrent_tool_previews_match_ids_and_clear_on_turn_end() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();
        let ready = Arc::new(Notify::new());
        let runner_ready = Arc::clone(&ready);
        let (manager, _rx) = WorkerManager::with_runner(Arc::new(move |request| {
            let ready = Arc::clone(&runner_ready);
            Box::pin(async move {
                ready.notify_one();
                request.cancel.cancelled().await;
                Ok(String::new())
            })
        }));
        let id = manager
            .create_worker("owner", project.path(), "go", None, None, None)
            .unwrap();
        ready.notified().await;
        let record = |activity| manager.record_activity(&id, &activity);
        record(WorkerActivity::ToolStarted {
            id: "a".into(),
            name: "bash".into(),
        });
        record(WorkerActivity::ToolStarted {
            id: "b".into(),
            name: "read".into(),
        });
        record(WorkerActivity::ToolInput {
            id: "a".into(),
            arg: "./gradlew build".into(),
        });
        assert_eq!(manager.snapshot(&id).unwrap().current_tool_input, None);
        record(WorkerActivity::ToolInput {
            id: "b".into(),
            arg: "Cargo.toml".into(),
        });
        record(WorkerActivity::ToolInput {
            id: "unknown".into(),
            arg: "wrong".into(),
        });
        record(WorkerActivity::ToolStarted {
            id: "b".into(),
            name: "read".into(),
        });
        assert_eq!(
            manager.snapshot(&id).unwrap().current_tool_input.as_deref(),
            Some("Cargo.toml")
        );
        record(WorkerActivity::ToolFinished {
            id: "b".into(),
            ok: true,
        });
        let remaining = manager.snapshot(&id).unwrap();
        assert_eq!(remaining.current_tool.as_deref(), Some("bash"));
        assert_eq!(
            remaining.current_tool_input.as_deref(),
            Some("./gradlew build")
        );
        record(WorkerActivity::ToolStarted {
            id: "c".into(),
            name: "grep".into(),
        });
        record(WorkerActivity::ToolInput {
            id: "c".into(),
            arg: "é".repeat(1000),
        });
        record(WorkerActivity::ToolFinished {
            id: "a".into(),
            ok: false,
        });
        let latest = manager.snapshot(&id).unwrap();
        assert_eq!(latest.current_tool.as_deref(), Some("grep"));
        assert_eq!(latest.current_tool_input.unwrap().chars().count(), 200);

        manager.cancel(&id).unwrap();
        let idle = wait_idle(&manager, &id).await;
        assert_eq!(idle.current_tool, None);
        assert_eq!(idle.current_tool_input, None);
        manager.send_message("owner", &id, "next").unwrap();
        ready.notified().await;
        assert_eq!(manager.snapshot(&id).unwrap().current_tool_input, None);
        manager.cancel(&id).unwrap();
        wait_idle(&manager, &id).await;
    }
}
