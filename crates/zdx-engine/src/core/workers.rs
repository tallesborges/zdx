//! In-memory worker-thread manager backing the orchestrator profile.
//!
//! Workers are ordinary visible ZDX threads bound to one project root. The
//! manager owns one FIFO per worker: prompts for a single worker run strictly
//! serially through a child `zdx --thread <id> exec` process, while different
//! workers run concurrently. All manager state (ownership, queues, status,
//! completion channel) is process-lifetime only by design — on restart the
//! thread JSONL transcripts survive and workers can be re-attached with
//! `send_message`, but queued prompts and pending callbacks are lost.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::config::ThinkingLevel;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent_with_cancel};
use crate::core::thread_persistence;

/// Lifecycle status of a managed worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub owner_thread_id: String,
    pub worker_thread_id: String,
    pub status: WorkerStatus,
    pub final_text: Option<String>,
    pub error: Option<String>,
}

/// Manager lifecycle events consumed by the surface bridge (Telegram bot).
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    /// A brand-new worker thread was created via `create_worker` (re-attached
    /// workers do not re-emit this). Carries the first prompt so the mirror
    /// topic can show it.
    Created {
        owner_thread_id: String,
        worker_thread_id: String,
        root: PathBuf,
        title: Option<String>,
        prompt: String,
    },
    /// The orchestrator queued a follow-up prompt via `send_message`.
    /// Topic-originated prompts do not emit this (the user's own message is
    /// already visible in the mirror topic).
    Prompted {
        worker_thread_id: String,
        prompt: String,
    },
    /// A worker prompt finished (any terminal status).
    Completed(CompletionEvent),
}

/// Point-in-time view of a managed worker.
#[derive(Debug, Clone)]
pub struct WorkerSnapshot {
    pub thread_id: String,
    pub owner_thread_id: String,
    pub root: PathBuf,
    pub status: WorkerStatus,
    pub queue_depth: usize,
    pub latest_final_text: Option<String>,
    pub last_error: Option<String>,
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
    pub owner_thread_id: String,
    pub root: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub cancel: CancellationToken,
}

type RunnerFuture = Pin<Box<dyn Future<Output = Result<String>> + Send>>;
type WorkerRunner = Arc<dyn Fn(WorkerRunRequest) -> RunnerFuture + Send + Sync>;

struct WorkerState {
    owner_thread_id: String,
    root: PathBuf,
    model: Option<String>,
    thinking_level: Option<ThinkingLevel>,
    queue: VecDeque<String>,
    status: WorkerStatus,
    current_cancel: Option<CancellationToken>,
    latest_final_text: Option<String>,
    last_error: Option<String>,
    wake: Arc<Notify>,
}

impl WorkerState {
    fn snapshot(&self, thread_id: &str) -> WorkerSnapshot {
        WorkerSnapshot {
            thread_id: thread_id.to_string(),
            owner_thread_id: self.owner_thread_id.clone(),
            root: self.root.clone(),
            status: self.status,
            queue_depth: self.queue.len(),
            latest_final_text: self.latest_final_text.clone(),
            last_error: self.last_error.clone(),
        }
    }
}

/// Process-lifetime manager for orchestrator-owned worker threads.
pub struct WorkerManager {
    state: Mutex<HashMap<String, WorkerState>>,
    /// Manager-wide change signal used by `wait_for`.
    changed: Notify,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
    runner: WorkerRunner,
}

impl WorkerManager {
    /// Creates a manager whose workers run through `zdx --thread <id> exec`.
    #[must_use]
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<WorkerEvent>) {
        Self::with_runner(Arc::new(|request| Box::pin(run_worker_prompt(request))))
    }

    /// Creates a manager with a custom prompt runner (used by tests).
    #[must_use]
    pub fn with_runner(runner: WorkerRunner) -> (Arc<Self>, mpsc::UnboundedReceiver<WorkerEvent>) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let manager = Arc::new(Self {
            state: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            events_tx,
            runner,
        });
        (manager, events_rx)
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
        let worker_id = thread.id.clone();

        // Emit Created before the FIFO task can exist: a fast first prompt
        // could otherwise complete and emit Completed ahead of Created, and
        // the bridge would have no mirror topic for the first result.
        let event = WorkerEvent::Created {
            owner_thread_id: owner_thread_id.to_string(),
            worker_thread_id: worker_id.clone(),
            root: root.clone(),
            title: title
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string),
            prompt: prompt.to_string(),
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
            prompt.to_string(),
        );
        Ok(worker_id)
    }

    /// Queues another prompt on a worker. Unmanaged threads (e.g. after a bot
    /// restart) are re-attached from their persisted root and owned by the
    /// calling orchestrator from then on.
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
        Ok(self.attach_or_enqueue(
            worker_thread_id,
            Some(owner_thread_id),
            root,
            None,
            None,
            message.to_string(),
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
        Ok(self.attach_or_enqueue(
            worker_thread_id,
            None,
            root,
            None,
            None,
            message.to_string(),
        ))
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
            state.queue.push_back(message.to_string());
            if state.status != WorkerStatus::Running {
                state.status = WorkerStatus::Queued;
            }
            state.wake.notify_one();
            state.snapshot(worker_thread_id)
        };
        self.changed.notify_waiters();
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
        prompt: String,
    ) -> WorkerSnapshot {
        let (snapshot, spawn_wake) = {
            let mut map = self.state.lock().expect("worker state lock poisoned");
            match map.entry(worker_thread_id.to_string()) {
                Entry::Occupied(mut occupied) => {
                    let state = occupied.get_mut();
                    if let Some(owner) = owner {
                        state.owner_thread_id = owner.to_string();
                    }
                    state.queue.push_back(prompt);
                    if state.status != WorkerStatus::Running {
                        state.status = WorkerStatus::Queued;
                    }
                    state.wake.notify_one();
                    (state.snapshot(worker_thread_id), None)
                }
                Entry::Vacant(vacant) => {
                    let wake = Arc::new(Notify::new());
                    let mut queue = VecDeque::new();
                    queue.push_back(prompt);
                    let state = vacant.insert(WorkerState {
                        owner_thread_id: owner.unwrap_or(worker_thread_id).to_string(),
                        root,
                        model,
                        thinking_level,
                        queue,
                        status: WorkerStatus::Queued,
                        current_cancel: None,
                        latest_final_text: None,
                        last_error: None,
                        wake: Arc::clone(&wake),
                    });
                    (state.snapshot(worker_thread_id), Some(wake))
                }
            }
        };
        self.changed.notify_waiters();
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
        self.state
            .lock()
            .expect("worker state lock poisoned")
            .get(worker_thread_id)
            .map(|state| state.snapshot(worker_thread_id))
    }

    /// Snapshots of every worker owned by `owner_thread_id`.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    #[must_use]
    pub fn list_for_owner(&self, owner_thread_id: &str) -> Vec<WorkerSnapshot> {
        let map = self.state.lock().expect("worker state lock poisoned");
        let mut workers: Vec<WorkerSnapshot> = map
            .iter()
            .filter(|(_, state)| state.owner_thread_id == owner_thread_id)
            .map(|(id, state)| state.snapshot(id))
            .collect();
        workers.sort_by(|a, b| a.thread_id.cmp(&b.thread_id));
        workers
    }

    /// Waits until every listed worker is idle/terminal or `timeout` expires.
    /// Returns `(timed_out, snapshots)`.
    ///
    /// # Errors
    /// Returns an error if any id is not currently managed.
    ///
    /// # Panics
    /// Panics if the internal worker state lock is poisoned.
    pub async fn wait_for(
        &self,
        worker_thread_ids: &[String],
        timeout: Duration,
    ) -> Result<(bool, Vec<WorkerSnapshot>)> {
        let snapshots = |manager: &Self| -> Result<Vec<WorkerSnapshot>> {
            worker_thread_ids
                .iter()
                .map(|id| {
                    manager
                        .snapshot(id)
                        .ok_or_else(|| anyhow::anyhow!("Worker '{id}' is not managed"))
                })
                .collect()
        };

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register interest before checking so a state change between the
            // check and the await cannot be missed.
            let notified = self.changed.notified();
            let current = snapshots(self)?;
            if current.iter().all(WorkerSnapshot::is_idle) {
                return Ok((false, current));
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(deadline) => {
                    return Ok((true, snapshots(self)?));
                }
            }
        }
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
            state.snapshot(worker_thread_id)
        };
        self.changed.notify_waiters();
        Ok(snapshot)
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

/// Default runner: executes one prompt through `zdx --thread <id> exec` in the
/// worker's project root, resuming the worker thread's persisted history.
async fn run_worker_prompt(request: WorkerRunRequest) -> Result<String> {
    let options = ExecSubagentOptions {
        model: request.model.clone(),
        thinking_level: request.thinking_level,
        thread_id: Some(request.worker_thread_id.clone()),
        activity_kind: Some("worker".to_string()),
        activity_parent_thread_id: Some(request.owner_thread_id.clone()),
        ..Default::default()
    };
    run_exec_subagent_with_cancel(
        &request.root,
        &request.prompt,
        &options,
        Some(request.cancel.clone()),
        None,
    )
    .await
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
                    state.status = WorkerStatus::Running;
                    WorkerRunRequest {
                        worker_thread_id: worker_thread_id.clone(),
                        owner_thread_id: state.owner_thread_id.clone(),
                        root: state.root.clone(),
                        prompt,
                        model: state.model.clone(),
                        thinking_level: state.thinking_level,
                        cancel,
                    }
                })
            };

            let Some(request) = next else {
                let notified = wake.notified();
                manager.changed.notify_waiters();
                notified.await;
                continue;
            };
            manager.changed.notify_waiters();

            let cancel = request.cancel.clone();
            let result = (manager.runner)(request).await;

            let event = {
                let mut map = manager.state.lock().expect("worker state lock poisoned");
                let Some(state) = map.get_mut(&worker_thread_id) else {
                    break;
                };
                state.current_cancel = None;
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
                CompletionEvent {
                    owner_thread_id: state.owner_thread_id.clone(),
                    worker_thread_id: worker_thread_id.clone(),
                    status: state.status,
                    final_text: result.as_ref().ok().cloned(),
                    error: state.last_error.clone(),
                }
            };
            manager.changed.notify_waiters();
            if manager
                .events_tx
                .send(WorkerEvent::Completed(event))
                .is_err()
            {
                tracing::debug!(worker = %worker_thread_id, "Worker event channel closed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

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
                WorkerEvent::Created { .. } | WorkerEvent::Prompted { .. } => {}
            }
        }
    }

    async fn wait_idle(manager: &Arc<WorkerManager>, id: &str) -> WorkerSnapshot {
        let (timed_out, snapshots) = manager
            .wait_for(&[id.to_string()], Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!timed_out, "worker should settle quickly");
        snapshots.into_iter().next().unwrap()
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
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        static MAX_SEEN: AtomicUsize = AtomicUsize::new(0);
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

    #[tokio::test]
    async fn different_workers_run_concurrently() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        static MAX_SEEN: AtomicUsize = AtomicUsize::new(0);
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

    #[tokio::test]
    async fn concurrent_reattach_keeps_both_prompts_and_one_fifo() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let mut thread = thread_persistence::Thread::new_with_root(project.path()).unwrap();
        thread.set_root_path(project.path()).unwrap();
        let existing_id = thread.id.clone();

        static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
        static MAX_SEEN: AtomicUsize = AtomicUsize::new(0);
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
                title,
                ..
            } => {
                assert_eq!(owner_thread_id, "owner");
                assert_eq!(worker_thread_id, worker_id);
                assert_eq!(title.as_deref(), Some("Mirror Me"));
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
    async fn wait_for_times_out_on_busy_worker() {
        let _home = temp_zdx_home();
        let project = tempfile::tempdir().unwrap();

        let runner: WorkerRunner = Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                tokio::select! {
                    () = request.cancel.cancelled() => anyhow::bail!("cancelled"),
                    () = tokio::time::sleep(Duration::from_secs(30)) => Ok(request.prompt),
                }
            })
        });
        let (manager, _completion_rx) = WorkerManager::with_runner(runner);
        let worker_id = manager
            .create_worker("owner", project.path(), "slow", None, None, None)
            .unwrap();

        let (timed_out, snapshots) = manager
            .wait_for(&[worker_id.clone()], Duration::from_millis(50))
            .await
            .unwrap();
        assert!(timed_out);
        assert_eq!(snapshots.len(), 1);
        manager.cancel(&worker_id).unwrap();
    }
}
