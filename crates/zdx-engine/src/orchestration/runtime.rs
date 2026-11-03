//! The orchestrator runtime: the one owner of worker processes, per-thread
//! input queues, worker-result delivery, and persistent-profile turns.
//!
//! - Each thread runs one turn at a time. Inputs wait in a FIFO; worker
//!   results waiting at the head are batched into one `[worker update]` turn,
//!   which stops at the first user message so order is kept.
//! - Every finished worker prompt is delivered to the thread that queued it
//!   (`CompletionEvent::owner_thread_id`), which starts that thread's next turn.
//! - A Telegram-routed thread is held after each turn while a renderer (the
//!   bot) is connected, until it releases the turn: the reply's delivery
//!   records then land before the next turn writes to the thread.
//! - All state is process-lifetime. Thread transcripts survive a restart;
//!   queues, holds, and running workers do not.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::prompt::{prepare_turn, turn_config};
use super::protocol::{
    Event, Follow, InputRef, Route, StatusReport, Submit, ThreadState, ThreadStatus, TurnKind,
};
use super::updates::{InputImage, record_user_input, worker_update_prompt};
use crate::config::{Config, TextVerbosity, paths};
use crate::core::agent::{
    self, AgentEventRx, AgentEventTx, AgentOptions, ToolConfig, ToolSelection, create_event_channel,
};
use crate::core::events::{AgentEvent, ErrorKind, TurnStatus};
use crate::core::thread_persistence::{self, Thread};
use crate::core::workers::{WorkerEvent, WorkerManager};
use crate::subagents::{self, ORCHESTRATOR_SUBAGENT_NAME};
use crate::tools::ToolRegistry;
use crate::tools::orchestrator::OrchestratorTool;

/// `activity_kind` of turns run here. The agent loop refuses turns on a
/// persistent-profile thread from any other run, so this service is the one
/// writer of those threads.
pub const ACTIVITY_KIND: &str = "orchestrator";

/// Longest a renderer may hold a thread after a turn before it continues.
const RELEASE_TIMEOUT: Duration = Duration::from_mins(10);

/// What [`Orchestrator::cancel`] stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelTarget {
    Thread { clear_queue: bool },
    Input(u64),
    Turn(u64),
}

/// One turn's work handed to the turn runner.
pub struct TurnRequest {
    pub thread_id: String,
    /// Root config layers load from; also the run root of a thread without one.
    pub config_root: PathBuf,
    pub route: Option<Route>,
    pub text: String,
    pub images: Vec<InputImage>,
    /// Agent events of the turn. Dropping every sender ends the stream.
    pub events: AgentEventTx,
    pub cancel: CancellationToken,
    /// Tool registry with the thread-control tools bound to the live workers.
    pub registry: ToolRegistry,
}

type RunnerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
/// Runs one turn and resolves once it is persisted.
pub type TurnRunner = Arc<dyn Fn(TurnRequest) -> RunnerFuture + Send + Sync>;

enum InputKind {
    User {
        text: String,
        images: Vec<InputImage>,
    },
    WorkerUpdate {
        text: String,
    },
}

struct Input {
    id: u64,
    /// A client streams this input's turn on its own connection.
    followed: bool,
    /// Subscription of the follower that delivers this turn's reply in the
    /// thread's Telegram route; the turn is held until it releases.
    holder: Option<u64>,
    kind: InputKind,
}

impl Input {
    fn is_worker_update(&self) -> bool {
        matches!(self.kind, InputKind::WorkerUpdate { .. })
    }
}

struct Running {
    turn_id: u64,
    inputs: Vec<u64>,
    cancel: CancellationToken,
}

struct ThreadEntry {
    config_root: PathBuf,
    route: Option<Route>,
    queue: VecDeque<Input>,
    running: Option<Running>,
    held: Option<u64>,
    draining: bool,
}

impl ThreadEntry {
    fn new(config_root: PathBuf) -> Self {
        Self {
            config_root,
            route: None,
            queue: VecDeque::new(),
            running: None,
            held: None,
            draining: false,
        }
    }

    fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.running.is_none() && self.held.is_none()
    }

    fn state(&self) -> ThreadState {
        if self.running.is_some() {
            ThreadState::Running
        } else if self.held.is_some() {
            ThreadState::Delivering
        } else if self.queue.is_empty() {
            ThreadState::Idle
        } else {
            ThreadState::Queued
        }
    }
}

struct Subscriber {
    id: u64,
    thread: Option<String>,
    renderer: bool,
    tx: mpsc::UnboundedSender<Event>,
}

/// Questions answered on the worker-event task, the only place that can tell
/// whether a worker result is still waiting in the channel.
enum Control {
    Settled {
        thread: Option<String>,
        reply: oneshot::Sender<bool>,
    },
    ThreadIdle(String),
}

pub struct Orchestrator {
    workers: Arc<WorkerManager>,
    registry: ToolRegistry,
    runner: TurnRunner,
    threads: Mutex<HashMap<String, ThreadEntry>>,
    subscribers: Mutex<Vec<Subscriber>>,
    release: Notify,
    next_id: AtomicU64,
    control: mpsc::UnboundedSender<Control>,
}

/// Receives the events of one subscription; unsubscribes on drop.
pub struct Subscription {
    orchestrator: Arc<Orchestrator>,
    id: u64,
    pub events: mpsc::UnboundedReceiver<Event>,
}

impl Subscription {
    /// Id that names this subscription as a turn's holder.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.orchestrator.unsubscribe(self.id);
    }
}

impl Orchestrator {
    /// Starts a runtime whose workers run as `zdx --thread <id> exec` and whose
    /// turns run the thread's persistent profile. Must run inside a Tokio
    /// runtime.
    #[must_use]
    pub fn start() -> Arc<Self> {
        let (workers, worker_events) = WorkerManager::new();
        let runner: TurnRunner = Arc::new(|request| Box::pin(run_profile_turn(request)));
        let orchestrator = Self::start_with(workers, worker_events, runner);
        orchestrator.recover_mirrors();
        orchestrator
    }

    /// Starts a runtime over an existing worker manager and turn runner
    /// (tests use fakes for both).
    #[must_use]
    pub fn start_with(
        workers: Arc<WorkerManager>,
        worker_events: mpsc::UnboundedReceiver<WorkerEvent>,
        runner: TurnRunner,
    ) -> Arc<Self> {
        let mut registry = ToolRegistry::builtins();
        for tool in OrchestratorTool::bound(&workers) {
            registry.register_boxed(Arc::new(tool));
        }
        let (control, control_rx) = mpsc::unbounded_channel();
        let orchestrator = Arc::new(Self {
            workers,
            registry,
            runner,
            threads: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            release: Notify::new(),
            next_id: AtomicU64::new(1),
            control,
        });
        tokio::spawn(Arc::clone(&orchestrator).deliver_worker_events(worker_events, control_rx));
        orchestrator
    }

    #[must_use]
    pub fn workers(&self) -> &Arc<WorkerManager> {
        &self.workers
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Registers legacy mirror-topic links (workers from before silent
    /// workers) so status and completions keep pointing at their topics.
    fn recover_mirrors(&self) {
        let pairs = match thread_persistence::list_worker_topics() {
            Ok(pairs) => pairs,
            Err(err) => {
                tracing::warn!(err = %format!("{err:#}"), "Failed to scan worker mirror topics");
                return;
            }
        };
        for (topic_thread_id, worker_thread_id) in pairs {
            if let Some(link) = crate::telegram::parse_topic_conversation_id(&topic_thread_id)
                .and_then(|(chat, topic)| crate::telegram::topic_link(chat, topic))
            {
                self.workers.set_mirror_url(&worker_thread_id, link);
            }
        }
    }

    /// Queues one input on a persistent-profile thread, creating the thread
    /// when it does not exist yet. Returns the thread id, the input id, and
    /// how many turns or inputs are ahead of it.
    ///
    /// # Errors
    /// Returns an error when the thread id is unusable, an existing thread
    /// has no persistent profile (or a different one), the profile cannot be
    /// loaded, or the thread cannot be created.
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub fn submit(self: &Arc<Self>, submit: Submit) -> Result<(String, u64, usize)> {
        self.submit_held(submit, None)
    }

    /// [`Self::submit`] for a follower that delivers the reply itself:
    /// `holder` is its subscription, which holds the turn until it releases
    /// it or goes away.
    ///
    /// # Errors
    /// Same as [`Self::submit`].
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub fn submit_held(
        self: &Arc<Self>,
        submit: Submit,
        holder: Option<u64>,
    ) -> Result<(String, u64, usize)> {
        if submit.text.trim().is_empty() && submit.images.is_empty() {
            bail!("message is empty");
        }
        let root = submit
            .root
            .canonicalize()
            .with_context(|| format!("project root does not exist: {}", submit.root.display()))?;
        // Creating the thread and queueing its first input happen under the
        // queue lock, so no turn (or concurrent creator) sees a thread whose
        // metadata is still being written.
        let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
        let thread_id = open_profile_thread(
            submit.thread_id.as_deref(),
            &root,
            submit.profile.as_deref(),
            submit.model.as_deref(),
        )?;

        let followed = submit.follow != Follow::None;
        let input = Input {
            id: self.next_id(),
            followed,
            holder: holder.filter(|_| submit.route.is_some()),
            kind: InputKind::User {
                text: submit.text,
                images: submit.images,
            },
        };
        let input_id = input.id;
        let entry = threads
            .entry(thread_id.clone())
            .or_insert_with(|| ThreadEntry::new(root.clone()));
        entry.config_root = root;
        if submit.route.is_some() {
            entry.route = submit.route;
        }
        let ahead = entry.queue.len()
            + usize::from(entry.running.is_some())
            + usize::from(entry.held.is_some());
        entry.queue.push_back(input);
        drop(threads);
        self.ensure_draining(&thread_id);
        Ok((thread_id, input_id, ahead))
    }

    /// Stops work on a thread. A [`CancelTarget::Input`] or
    /// [`CancelTarget::Turn`] stops only that work: the running turn that
    /// holds it, or the waiting input. [`CancelTarget::Thread`] stops the
    /// running turn and, with `clear_queue`, drops every waiting input.
    /// Returns whether a turn was cancelled and the dropped input ids.
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    #[must_use]
    pub fn cancel(&self, thread_id: &str, target: CancelTarget) -> (bool, Vec<u64>) {
        let (cancelled, dropped) = {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            let Some(entry) = threads.get_mut(thread_id) else {
                return (false, Vec::new());
            };
            let matches = |running: &Running| match target {
                CancelTarget::Thread { .. } => true,
                CancelTarget::Input(id) => running.inputs.contains(&id),
                CancelTarget::Turn(id) => running.turn_id == id,
            };
            let cancelled = entry
                .running
                .as_ref()
                .filter(|running| matches(running))
                .is_some_and(|running| {
                    running.cancel.cancel();
                    true
                });
            let dropped: Vec<u64> = match target {
                CancelTarget::Thread { clear_queue: true } => {
                    entry.queue.drain(..).map(|input| input.id).collect()
                }
                CancelTarget::Input(id) if !cancelled => {
                    let before = entry.queue.len();
                    entry.queue.retain(|input| input.id != id);
                    if entry.queue.len() < before {
                        vec![id]
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            };
            (cancelled, dropped)
        };
        if !dropped.is_empty() {
            self.emit(&Event::InputsDropped {
                thread_id: thread_id.to_string(),
                input_ids: dropped.clone(),
            });
        }
        (cancelled, dropped)
    }

    /// The renderer delivered `turn_id`; the thread may run its next turn.
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub fn release(&self, thread_id: &str, turn_id: u64) {
        let released = {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            threads
                .get_mut(thread_id)
                .filter(|entry| entry.held == Some(turn_id))
                .map(|entry| entry.held = None)
                .is_some()
        };
        if released {
            self.release.notify_waiters();
        }
    }

    /// Status of one thread, or of every known thread.
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub async fn status(self: &Arc<Self>, thread_id: Option<&str>) -> StatusReport {
        let settled = self.settled(None).await;
        let ids: Vec<String> = {
            let threads = self.threads.lock().expect("orchestrator lock poisoned");
            if let Some(id) = thread_id {
                vec![id.to_string()]
            } else {
                let mut ids: Vec<String> = threads.keys().cloned().collect();
                ids.sort();
                ids
            }
        };
        let mut statuses = Vec::with_capacity(ids.len());
        for id in ids {
            let thread_settled = self.settled(Some(&id)).await;
            let (state, running_turn, queued_inputs, route) = {
                let threads = self.threads.lock().expect("orchestrator lock poisoned");
                threads
                    .get(&id)
                    .map_or((ThreadState::Idle, None, 0, None), |entry| {
                        (
                            entry.state(),
                            entry.running.as_ref().map(|running| running.turn_id),
                            entry.queue.len(),
                            entry.route,
                        )
                    })
            };
            statuses.push(ThreadStatus {
                profile: thread_persistence::read_persistent_profile(&id)
                    .ok()
                    .flatten(),
                workers: self.workers.list_for_owner(&id),
                thread_id: id,
                state,
                running_turn,
                queued_inputs,
                route,
                settled: thread_settled,
            });
        }
        StatusReport {
            settled,
            threads: statuses,
        }
    }

    /// Whether one thread (or everything, for `None`) is settled. See
    /// [`Follow::Settled`].
    pub async fn settled(&self, thread: Option<&str>) -> bool {
        let (reply, answer) = oneshot::channel();
        if self
            .control
            .send(Control::Settled {
                thread: thread.map(str::to_string),
                reply,
            })
            .is_err()
        {
            return false;
        }
        answer.await.unwrap_or(false)
    }

    /// Subscribes to events of one thread, or of every thread. A renderer
    /// makes Telegram-routed threads wait for its release after each turn.
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    #[must_use]
    pub fn subscribe(self: &Arc<Self>, thread: Option<String>, renderer: bool) -> Subscription {
        let (tx, events) = mpsc::unbounded_channel();
        let id = self.next_id();
        self.subscribers
            .lock()
            .expect("orchestrator lock poisoned")
            .push(Subscriber {
                id,
                thread,
                renderer,
                tx,
            });
        Subscription {
            orchestrator: Arc::clone(self),
            id,
            events,
        }
    }

    fn unsubscribe(&self, id: u64) {
        let removed = {
            let mut subscribers = self.subscribers.lock().expect("orchestrator lock poisoned");
            subscribers
                .iter()
                .position(|subscriber| subscriber.id == id)
                .map(|index| subscribers.remove(index))
        };
        if removed.is_some() {
            // Any subscription can hold a turn (a renderer, or the follower
            // that delivers it); a held thread must not wait for one that is
            // gone. Each waiter checks its own holder.
            self.release.notify_waiters();
        }
    }

    /// The renderer subscribed now, if any.
    fn current_renderer(&self) -> Option<u64> {
        self.subscribers
            .lock()
            .expect("orchestrator lock poisoned")
            .iter()
            .find(|subscriber| subscriber.renderer)
            .map(|subscriber| subscriber.id)
    }

    fn is_subscribed(&self, id: u64) -> bool {
        self.subscribers
            .lock()
            .expect("orchestrator lock poisoned")
            .iter()
            .any(|subscriber| subscriber.id == id)
    }

    fn emit(&self, event: &Event) {
        let mut subscribers = self.subscribers.lock().expect("orchestrator lock poisoned");
        subscribers.retain(|subscriber| {
            let wanted = subscriber
                .thread
                .as_deref()
                .is_none_or(|thread| event.thread_id() == Some(thread));
            !wanted || subscriber.tx.send(event.clone()).is_ok()
        });
    }

    fn ensure_draining(self: &Arc<Self>, thread_id: &str) {
        let start = {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            threads
                .get_mut(thread_id)
                .filter(|entry| !entry.draining && !entry.queue.is_empty())
                .map(|entry| entry.draining = true)
                .is_some()
        };
        if start {
            tokio::spawn(Arc::clone(self).drain(thread_id.to_string()));
        }
    }

    /// Runs a thread's queued inputs one turn at a time until the queue is
    /// empty.
    async fn drain(self: Arc<Self>, thread_id: String) {
        loop {
            let next = {
                let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
                let Some(entry) = threads.get_mut(&thread_id) else {
                    return;
                };
                match entry.queue.pop_front() {
                    None => {
                        entry.draining = false;
                        None
                    }
                    Some(head) => {
                        let mut batch = vec![head];
                        if batch[0].is_worker_update() {
                            while entry.queue.front().is_some_and(Input::is_worker_update) {
                                batch.extend(entry.queue.pop_front());
                            }
                        }
                        let turn_id = self.next_id();
                        let cancel = CancellationToken::new();
                        entry.running = Some(Running {
                            turn_id,
                            inputs: batch.iter().map(|input| input.id).collect(),
                            cancel: cancel.clone(),
                        });
                        Some((
                            turn_id,
                            cancel,
                            batch,
                            entry.route,
                            entry.config_root.clone(),
                        ))
                    }
                }
            };
            let Some((turn_id, cancel, batch, route, config_root)) = next else {
                let _ = self.control.send(Control::ThreadIdle(thread_id));
                return;
            };
            if batch.len() > 1 {
                tracing::info!(
                    thread = %thread_id,
                    count = batch.len(),
                    "Batched pending worker updates into one turn"
                );
            }

            // A Telegram-routed turn is held for whoever delivers its reply:
            // the follower that submitted it, or, for a turn nobody follows
            // (a worker update), the renderer subscribed as it starts. One
            // that connects later never learned of the turn. A turn a
            // non-renderer client follows (the CLI) is not held.
            let renderer = if route.is_none() {
                None
            } else if let Some(holder) = batch.iter().find_map(|input| input.holder) {
                Some(holder)
            } else if batch.iter().all(|input| !input.followed) {
                self.current_renderer()
            } else {
                None
            };
            self.run_turn(&thread_id, turn_id, cancel, batch, route, config_root)
                .await;

            let renderer = renderer.filter(|id| self.is_subscribed(*id));
            let held = {
                let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
                threads.get_mut(&thread_id).is_some_and(|entry| {
                    entry.running = None;
                    let hold = renderer.is_some();
                    if hold {
                        entry.held = Some(turn_id);
                    }
                    hold
                })
            };
            self.emit(&Event::TurnEnded {
                thread_id: thread_id.clone(),
                turn_id,
                held,
            });
            if let (true, Some(renderer)) = (held, renderer) {
                self.wait_for_release(&thread_id, turn_id, renderer).await;
            }
        }
    }

    async fn wait_for_release(&self, thread_id: &str, turn_id: u64, renderer: u64) {
        let deadline = tokio::time::Instant::now() + RELEASE_TIMEOUT;
        loop {
            let notified = self.release.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let released = {
                let threads = self.threads.lock().expect("orchestrator lock poisoned");
                threads
                    .get(thread_id)
                    .is_none_or(|entry| entry.held != Some(turn_id))
            };
            if released || !self.is_subscribed(renderer) {
                break;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                tracing::warn!(thread = %thread_id, turn_id, "Renderer never released the turn; continuing");
                break;
            }
        }
        let cleared = {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            threads
                .get_mut(thread_id)
                .filter(|entry| entry.held == Some(turn_id))
                .map(|entry| entry.held = None)
                .is_some()
        };
        if cleared {
            self.release.notify_waiters();
        }
    }

    /// Waits until `turn_id` of the thread is no longer held (released,
    /// timed out, or its holder went away).
    pub async fn wait_released(&self, thread_id: &str, turn_id: u64) {
        loop {
            let notified = self.release.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.is_held(thread_id, turn_id) {
                return;
            }
            notified.await;
        }
    }

    fn is_held(&self, thread_id: &str, turn_id: u64) -> bool {
        self.threads
            .lock()
            .expect("orchestrator lock poisoned")
            .get(thread_id)
            .is_some_and(|entry| entry.held == Some(turn_id))
    }

    async fn run_turn(
        &self,
        thread_id: &str,
        turn_id: u64,
        cancel: CancellationToken,
        batch: Vec<Input>,
        route: Option<Route>,
        config_root: PathBuf,
    ) {
        let kind = if batch.iter().all(Input::is_worker_update) {
            TurnKind::WorkerUpdate
        } else {
            TurnKind::User
        };
        let followed = batch.iter().any(|input| input.followed);
        let inputs = batch
            .iter()
            .map(|input| InputRef { input_id: input.id })
            .collect();
        self.emit(&Event::TurnStarted {
            thread_id: thread_id.to_string(),
            turn_id,
            kind,
            inputs,
            route,
            followed,
        });

        let mut texts = Vec::with_capacity(batch.len());
        let mut images = Vec::new();
        for input in batch {
            match input.kind {
                InputKind::User {
                    text,
                    images: input_images,
                    ..
                } => {
                    texts.push(text);
                    images.extend(input_images);
                }
                InputKind::WorkerUpdate { text } => texts.push(text),
            }
        }
        let (events, mut rx) = create_event_channel();
        let run = (self.runner)(TurnRequest {
            thread_id: thread_id.to_string(),
            config_root,
            route,
            // Batched inputs in arrival order.
            text: texts.join("\n\n"),
            images,
            events,
            cancel,
            registry: self.registry.clone(),
        });
        let forward = async {
            let mut finished = None;
            while let Some(event) = rx.recv().await {
                match &*event {
                    AgentEvent::TurnFinished {
                        status, final_text, ..
                    } => {
                        // Sent after the turn is on disk (below).
                        finished = Some(AgentEvent::TurnFinished {
                            status: status.clone(),
                            final_text: final_text.clone(),
                            messages: Vec::new(),
                            prior_message_count: 0,
                        });
                    }
                    AgentEvent::TurnCheckpoint { .. } => {}
                    other => self.emit(&Event::Agent {
                        thread_id: thread_id.to_string(),
                        turn_id,
                        event: other.clone(),
                    }),
                }
            }
            finished
        };
        let ((), finished) = tokio::join!(run, forward);
        let finished = finished.unwrap_or_else(|| AgentEvent::TurnFinished {
            status: TurnStatus::Failed {
                kind: ErrorKind::Internal,
                message: "The turn ended without a result".to_string(),
                details: None,
            },
            final_text: String::new(),
            messages: Vec::new(),
            prior_message_count: 0,
        });
        self.emit(&Event::Agent {
            thread_id: thread_id.to_string(),
            turn_id,
            event: finished,
        });
    }

    /// Forwards worker events to subscribers, queues each finished prompt on
    /// the thread that queued it, and answers settlement questions. Both run
    /// on this one task: a worker reports `Completed` under the same lock that
    /// makes it idle, so "every worker idle, then no event waiting here, then
    /// no thread busy" means no result can still be on its way.
    async fn deliver_worker_events(
        self: Arc<Self>,
        mut worker_events: mpsc::UnboundedReceiver<WorkerEvent>,
        mut control: mpsc::UnboundedReceiver<Control>,
    ) {
        // Threads whose queue emptied but that were not settled yet (a worker
        // still owed them a result, or another event was waiting): checked
        // again after every worker event until they settle.
        let mut idle: HashSet<String> = HashSet::new();
        loop {
            tokio::select! {
                biased;
                event = worker_events.recv() => {
                    let Some(event) = event else { break };
                    self.handle_worker_event(event);
                }
                request = control.recv() => {
                    let Some(request) = request else { break };
                    match request {
                        Control::Settled { thread, reply } => {
                            let settled = self.is_settled(thread.as_deref(), &worker_events);
                            let _ = reply.send(settled);
                        }
                        Control::ThreadIdle(thread_id) => {
                            idle.insert(thread_id);
                        }
                    }
                }
            }
            idle.retain(|thread_id| {
                if self.is_settled(Some(thread_id), &worker_events) {
                    self.emit(&Event::Settled {
                        thread_id: thread_id.clone(),
                    });
                    false
                } else {
                    true
                }
            });
        }
    }

    fn is_settled(
        &self,
        thread: Option<&str>,
        worker_events: &mpsc::UnboundedReceiver<WorkerEvent>,
    ) -> bool {
        // The thread lock is held through every check: a turn still running
        // here may queue a worker, and that turn cannot finish (and look idle)
        // between the worker check and the thread check. Lock order is
        // threads, then worker state; nothing takes them the other way.
        let threads = self.threads.lock().expect("orchestrator lock poisoned");
        // Results route to the thread that queued the prompt, whoever owns
        // the worker now.
        let workers_idle = match thread {
            Some(thread) => self.workers.idle_for_requester(thread),
            None => self.workers.all_idle(),
        };
        if !workers_idle || !worker_events.is_empty() {
            return false;
        }
        match thread {
            Some(thread) => threads.get(thread).is_none_or(ThreadEntry::is_idle),
            None => threads.values().all(ThreadEntry::is_idle),
        }
    }

    fn handle_worker_event(self: &Arc<Self>, event: WorkerEvent) {
        let owner = match &event {
            WorkerEvent::Created {
                owner_thread_id, ..
            } => Some(owner_thread_id.clone()),
            WorkerEvent::Completed(completion) => Some(completion.owner_thread_id.clone()),
            WorkerEvent::Prompted {
                worker_thread_id, ..
            }
            | WorkerEvent::Activity {
                worker_thread_id, ..
            }
            | WorkerEvent::QueueChanged { worker_thread_id } => self
                .workers
                .snapshot(worker_thread_id)
                .map(|snapshot| snapshot.owner_thread_id),
        };
        let route = owner.as_deref().and_then(|owner| {
            self.threads
                .lock()
                .expect("orchestrator lock poisoned")
                .get(owner)
                .and_then(|entry| entry.route)
        });

        // The notice for a result goes out before the turn that reviews it.
        let completion = match &event {
            WorkerEvent::Completed(completion)
                if completion.owner_thread_id != completion.worker_thread_id =>
            {
                Some(completion.clone())
            }
            _ => None,
        };
        self.emit(&Event::Worker {
            owner_thread_id: owner,
            route,
            event,
        });
        if let Some(completion) = completion {
            self.queue_worker_update(&completion);
        }
    }

    fn queue_worker_update(self: &Arc<Self>, completion: &crate::core::workers::CompletionEvent) {
        let owner = &completion.owner_thread_id;
        let config_root = {
            let threads = self.threads.lock().expect("orchestrator lock poisoned");
            threads.get(owner).map(|entry| entry.config_root.clone())
        };
        let Some(config_root) = config_root.or_else(|| {
            thread_persistence::read_thread_root_path(owner)
                .ok()
                .flatten()
                .map(PathBuf::from)
        }) else {
            tracing::warn!(owner = %owner, worker = %completion.worker_thread_id, "No root for the worker's orchestrator; dropping its result");
            return;
        };
        let config = load_config(&config_root).ok();
        let worker_context = crate::tools::orchestrator::thread_context_json(
            &completion.worker_thread_id,
            config.as_ref(),
        );
        let input = Input {
            id: self.next_id(),
            followed: false,
            holder: None,
            kind: InputKind::WorkerUpdate {
                text: worker_update_prompt(completion, &worker_context),
            },
        };
        {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            threads
                .entry(owner.clone())
                .or_insert_with(|| ThreadEntry::new(config_root))
                .queue
                .push_back(input);
        }
        tracing::info!(
            owner = %owner,
            worker = %completion.worker_thread_id,
            status = completion.status.as_str(),
            "Queued worker update"
        );
        self.ensure_draining(owner);
    }
}

/// Opens (or creates) a persistent-profile thread and returns its id.
fn open_profile_thread(
    thread_id: Option<&str>,
    root: &Path,
    profile: Option<&str>,
    model: Option<&str>,
) -> Result<String> {
    if let Some(id) = thread_id {
        validate_thread_id(id)?;
        if thread_persistence::thread_exists(id) {
            let existing = thread_persistence::read_persistent_profile(id)?.with_context(|| {
                format!("thread '{id}' is not a persistent-profile (orchestrator) thread")
            })?;
            if let Some(requested) = profile
                && requested != existing
            {
                bail!("thread '{id}' runs profile '{existing}', not '{requested}'");
            }
            if model.is_some() {
                bail!("--model applies only when a thread is created; thread '{id}' exists");
            }
            return Ok(id.to_string());
        }
    }

    let profile = profile.unwrap_or(ORCHESTRATOR_SUBAGENT_NAME);
    // Fail before anything is written when the profile cannot run.
    subagents::load_persistent_profile(root, profile)?;
    let mut thread = match thread_id {
        Some(id) => Thread::with_id(id.to_string())?,
        None => Thread::new_with_root(root)?,
    };
    thread.set_persistent_profile(profile)?;
    thread.set_root_path(root)?;
    if let Some(model) = model {
        thread.set_model_override(Some(model.to_string()))?;
    }
    Ok(thread.id.clone())
}

/// Thread ids become file names, so only a plain name is accepted.
fn validate_thread_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        bail!("invalid thread id '{id}': use letters, digits, '-', '_', or '.'");
    }
    Ok(())
}

fn load_config(root: &Path) -> Result<Config> {
    Config::load_layered(&paths::config_layer_paths_for(root)).context("load config")
}

fn failed(message: String) -> AgentEvent {
    AgentEvent::TurnFinished {
        status: TurnStatus::Failed {
            kind: ErrorKind::Internal,
            message,
            details: None,
        },
        final_text: String::new(),
        messages: Vec::new(),
        prior_message_count: 0,
    }
}

/// The production turn runner: prepares the thread's profile, records the
/// input, runs the agent, and resolves once the turn is persisted.
async fn run_profile_turn(request: TurnRequest) {
    let TurnRequest {
        thread_id,
        config_root,
        route,
        text,
        images,
        events,
        cancel,
        registry,
    } = request;
    let prepared = match prepare_profile_turn(&thread_id, &config_root, route, text, &images) {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::error!(thread = %thread_id, err = %format!("{err:#}"), "Failed to prepare orchestrator turn");
            let _ = events.send(Arc::new(failed(format!("{err:#}"))));
            return;
        }
    };
    let ReadyTurn {
        config,
        run_root,
        system_prompt,
        tools,
        allowed_subagents,
        thread,
        messages,
    } = prepared;

    let options = AgentOptions {
        conversation_id: None,
        root: run_root,
        tool_config: ToolConfig {
            registry,
            selection: ToolSelection::Explicit(tools),
            allowed_subagents,
            background_commands: true,
        },
        surface: Some(
            if route.is_some() {
                "telegram"
            } else {
                "orchestrator"
            }
            .to_string(),
        ),
        text_verbosity: route.map(|_| TextVerbosity::Low),
        service_tier: None,
        activity_kind: Some(ACTIVITY_KIND.to_string()),
        activity_parent_thread_id: None,
        activity_subagent_name: None,
        tool_timeout: None,
    };
    let (agent_tx, agent_rx) = create_event_channel();
    let (persist_tx, persist_rx): (AgentEventTx, AgentEventRx) = create_event_channel();
    let broadcaster = agent::spawn_broadcaster(agent_rx, vec![events, persist_tx]);
    let persist = thread_persistence::spawn_thread_persist_task(thread, persist_rx);
    let _ = agent::run_turn_with_cancel(
        messages,
        &config,
        &options,
        Some(&system_prompt),
        Some(&thread_id),
        agent_tx,
        Some(cancel),
    )
    .await;
    let _ = broadcaster.await;
    if let Err(err) = persist.await {
        tracing::warn!(thread = %thread_id, %err, "Thread persistence task failed");
    }
}

struct ReadyTurn {
    config: Config,
    run_root: PathBuf,
    system_prompt: String,
    tools: Vec<String>,
    allowed_subagents: Option<Vec<String>>,
    thread: Thread,
    messages: Vec<crate::providers::ChatMessage>,
}

fn prepare_profile_turn(
    thread_id: &str,
    config_root: &Path,
    route: Option<Route>,
    text: String,
    images: &[InputImage],
) -> Result<ReadyTurn> {
    if let Some(run) = crate::agent_activity::active_run_for_thread(thread_id)
        && run.pid != std::process::id()
    {
        bail!(
            "thread '{thread_id}' has a turn running in another process (PID {}); only one process may write a thread",
            run.pid
        );
    }
    let replay = thread_persistence::load_thread_replay(thread_id).context("load thread")?;
    let mut thread = Thread::with_id(thread_id.to_string()).context("open thread")?;
    let mut messages = replay.messages;
    let stored_root = thread_persistence::read_thread_root_path(thread_id)?.map(PathBuf::from);
    let run_root = stored_root
        .clone()
        .unwrap_or_else(|| config_root.to_path_buf());

    let prepared = (|| {
        let profile = thread_persistence::read_persistent_profile(thread_id)?
            .context("thread has no persistent profile")?;
        let definition = subagents::load_persistent_profile(&run_root, &profile)?;
        let config = turn_config(
            load_config(config_root)?,
            &definition,
            thread_persistence::read_thread_model_override(thread_id)?.as_deref(),
            thread_persistence::read_thread_thinking_override(thread_id)?,
        );
        crate::core::context::set_runtime_env(&config, Some(thread_id));
        prepare_turn(&config, &run_root, &definition, route)
    })();
    if stored_root.is_none() {
        thread.set_root_path(&run_root)?;
    }
    // The input is recorded even when preparation failed, so the message is
    // in the transcript next to the error.
    record_user_input(
        &mut thread,
        &mut messages,
        text,
        images,
        prepared
            .as_ref()
            .ok()
            .and_then(|prepared| prepared.runtime_context.as_ref()),
        replay.trailing_note.as_deref(),
    )?;
    let prepared = prepared?;
    Ok(ReadyTurn {
        config: prepared.config,
        run_root,
        system_prompt: prepared.system_prompt,
        tools: prepared.tools,
        allowed_subagents: prepared.allowed_subagents,
        thread,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::core::workers::WorkerRunRequest;

    /// Texts each fake turn received, in run order.
    type Seen = Arc<StdMutex<Vec<String>>>;

    /// Worker runner that finishes at once with `done:<prompt>`.
    fn instant_workers() -> (Arc<WorkerManager>, mpsc::UnboundedReceiver<WorkerEvent>) {
        WorkerManager::with_runner(Arc::new(|request: WorkerRunRequest| {
            Box::pin(async move {
                Ok(format!(
                    "done:{}",
                    request.prompt.lines().last().unwrap_or_default()
                ))
            })
        }))
    }

    /// Turn runner that records the input text, starts one worker per line
    /// `delegate <name>`, then waits for `gate` when the text says `wait`, and
    /// answers `reply`.
    fn fake_turns(
        workers: &Arc<WorkerManager>,
        root: PathBuf,
        seen: &Seen,
        gate: &Arc<Notify>,
    ) -> TurnRunner {
        let workers = Arc::clone(workers);
        let seen = Arc::clone(seen);
        let gate = Arc::clone(gate);
        Arc::new(move |request: TurnRequest| {
            let workers = Arc::clone(&workers);
            let seen = Arc::clone(&seen);
            let gate = Arc::clone(&gate);
            let root = root.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(request.text.clone());
                for line in request.text.lines() {
                    if let Some(name) = line.strip_prefix("delegate ") {
                        workers
                            .create_worker(&request.thread_id, &root, name, None, None, None)
                            .expect("create worker");
                    }
                }
                if request.text.contains("wait") {
                    gate.notified().await;
                }
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: "reply".to_string(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        })
    }

    fn submit(root: &Path, thread_id: Option<&str>, text: &str, route: Option<Route>) -> Submit {
        Submit {
            thread_id: thread_id.map(str::to_string),
            root: root.to_path_buf(),
            profile: None,
            model: None,
            route,
            text: text.to_string(),
            images: Vec::new(),
            follow: Follow::None,
        }
    }

    async fn next_event(subscription: &mut Subscription) -> Event {
        tokio::time::timeout(Duration::from_secs(10), subscription.events.recv())
            .await
            .expect("event in time")
            .expect("subscription open")
    }

    /// Waits for the thread's `Settled` event, returning what came before.
    async fn until_settled(subscription: &mut Subscription) -> Vec<Event> {
        let mut seen = Vec::new();
        loop {
            let event = next_event(subscription).await;
            if matches!(event, Event::Settled { .. }) {
                return seen;
            }
            seen.push(event);
        }
    }

    #[tokio::test]
    async fn a_worker_result_wakes_the_thread_that_queued_it_then_it_settles() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(workers, events, runner);

        let mut subscription = orchestrator.subscribe(Some("issue-1".to_string()), false);
        let (thread_id, _, ahead) = orchestrator
            .submit(submit(
                root.path(),
                Some("issue-1"),
                "delegate triage",
                None,
            ))
            .unwrap();
        assert_eq!((thread_id.as_str(), ahead), ("issue-1", 0));
        assert_eq!(
            thread_persistence::read_persistent_profile("issue-1")
                .unwrap()
                .as_deref(),
            Some("orchestrator")
        );

        let events = until_settled(&mut subscription).await;
        let kinds: Vec<TurnKind> = events
            .iter()
            .filter_map(|event| match event {
                Event::TurnStarted { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, [TurnKind::User, TurnKind::WorkerUpdate]);
        let completed = events.iter().any(|event| {
            matches!(event, Event::Worker { owner_thread_id: Some(owner), event: WorkerEvent::Completed(_), .. } if owner == "issue-1")
        });
        assert!(completed, "the worker result is published with its owner");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].starts_with("[worker update]"), "{}", seen[1]);
        assert!(seen[1].contains("done:triage"));
        assert!(orchestrator.settled(None).await);
    }

    #[tokio::test]
    async fn waiting_worker_results_run_as_one_turn_and_a_user_message_runs_alone() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(Arc::clone(&workers), events, runner);
        let mut subscription = orchestrator.subscribe(Some("batch".to_string()), false);

        // The first turn starts two workers, then waits: both results arrive
        // while it runs, followed by a user message.
        orchestrator
            .submit(submit(
                root.path(),
                Some("batch"),
                "delegate one\ndelegate two\nwait",
                None,
            ))
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let queued = orchestrator.status(Some("batch")).await.threads[0].queued_inputs;
            if queued == 2 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "results never queued"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        orchestrator
            .submit(submit(root.path(), Some("batch"), "next question", None))
            .unwrap();
        gate.notify_one();

        until_settled(&mut subscription).await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[1].matches("[worker update]").count(), 2, "{}", seen[1]);
        assert!(seen[1].contains("done:one") && seen[1].contains("done:two"));
        assert_eq!(seen[2], "next question");
    }

    #[tokio::test]
    async fn a_renderer_holds_a_routed_thread_until_it_releases_the_turn() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(workers, events, runner);
        let mut renderer = orchestrator.subscribe(None, true);
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });

        orchestrator
            .submit(submit(root.path(), Some("routed"), "first", route))
            .unwrap();
        orchestrator
            .submit(submit(root.path(), Some("routed"), "second", route))
            .unwrap();
        let turn_id = loop {
            if let Event::TurnEnded { turn_id, held, .. } = next_event(&mut renderer).await {
                assert!(held);
                break turn_id;
            }
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "the next turn waits for the release"
        );
        assert_eq!(
            orchestrator.status(Some("routed")).await.threads[0].state,
            ThreadState::Delivering
        );

        orchestrator.release("routed", turn_id);
        loop {
            if let Event::TurnStarted { .. } = next_event(&mut renderer).await {
                break;
            }
        }
        // A renderer that goes away releases its holds.
        drop(renderer);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !orchestrator.settled(None).await {
            assert!(tokio::time::Instant::now() < deadline, "never settled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*seen.lock().unwrap(), ["first", "second"]);
    }

    /// A renderer that connects while a turn runs never saw it start, so the
    /// turn is not held for it.
    #[tokio::test]
    async fn a_renderer_that_missed_the_start_does_not_hold_the_turn() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(workers, events, runner);
        let mut watcher = orchestrator.subscribe(Some("late".to_string()), false);
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });

        orchestrator
            .submit(submit(root.path(), Some("late"), "wait", route))
            .unwrap();
        while !matches!(next_event(&mut watcher).await, Event::TurnStarted { .. }) {}
        let _late_renderer = orchestrator.subscribe(None, true);
        gate.notify_one();
        loop {
            if let Event::TurnEnded { held, .. } = next_event(&mut watcher).await {
                assert!(!held);
                break;
            }
        }
    }

    /// Results route to the thread that queued the prompt. Moving the worker
    /// to another orchestrator mid-turn must not make the first one look
    /// settled while its result is still owed.
    #[tokio::test]
    async fn settlement_follows_the_requester_not_the_current_owner() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let worker_gate = Arc::new(Notify::new());
        let held_gate = Arc::clone(&worker_gate);
        let (workers, events) = WorkerManager::with_runner(Arc::new(move |_: WorkerRunRequest| {
            let gate = Arc::clone(&held_gate);
            Box::pin(async move {
                gate.notified().await;
                Ok("done".to_string())
            })
        }));
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(Arc::clone(&workers), events, runner);
        let mut subscription = orchestrator.subscribe(Some("first".to_string()), false);

        orchestrator
            .submit(submit(root.path(), Some("first"), "delegate job", None))
            .unwrap();
        while !matches!(next_event(&mut subscription).await, Event::TurnEnded { .. }) {}
        let worker = workers.list_for_owner("first")[0].thread_id.clone();
        workers.send_message("second", &worker, "more").unwrap();
        assert!(workers.list_for_owner("first").is_empty());
        assert!(!orchestrator.settled(Some("first")).await);

        // Both prompts finish; "first" reviews its result, then settles.
        worker_gate.notify_one();
        worker_gate.notify_one();
        let events = until_settled(&mut subscription).await;
        assert!(events.iter().any(|event| matches!(
            event,
            Event::TurnStarted {
                kind: TurnKind::WorkerUpdate,
                ..
            }
        )));
    }

    /// Removing a waiting prompt sends no result, yet it can leave its
    /// orchestrator owed nothing: that thread must still get `Settled`.
    #[tokio::test]
    async fn removing_a_waiting_prompt_settles_its_requester() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let worker_gate = Arc::new(Notify::new());
        let held_gate = Arc::clone(&worker_gate);
        let (workers, events) = WorkerManager::with_runner(Arc::new(move |_: WorkerRunRequest| {
            let gate = Arc::clone(&held_gate);
            Box::pin(async move {
                gate.notified().await;
                Ok("done".to_string())
            })
        }));
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(Arc::clone(&workers), events, runner);
        let mut owner = orchestrator.subscribe(Some("owner".to_string()), false);
        let mut asker = orchestrator.subscribe(Some("asker".to_string()), false);

        // "owner" starts a worker that keeps running.
        orchestrator
            .submit(submit(root.path(), Some("owner"), "delegate job", None))
            .unwrap();
        while !matches!(next_event(&mut owner).await, Event::TurnEnded { .. }) {}
        let worker = workers.list_for_owner("owner")[0].thread_id.clone();
        // "asker" queues a prompt behind it, then finishes its own turn.
        let queued = workers.send_message("asker", &worker, "later").unwrap();
        let prompt_id = queued.queue.last().unwrap().id;
        orchestrator
            .submit(submit(root.path(), Some("asker"), "hello", None))
            .unwrap();
        while !matches!(next_event(&mut asker).await, Event::TurnEnded { .. }) {}
        assert!(!orchestrator.settled(Some("asker")).await);

        workers.remove_queued_prompt(&worker, prompt_id).unwrap();
        until_settled(&mut asker).await;
        assert!(
            !orchestrator.settled(Some("owner")).await,
            "owner's worker still runs"
        );
        worker_gate.notify_one();
    }

    /// The follower that delivers a routed turn holds it, even with no
    /// renderer subscribed; its release (or its going away) frees it.
    #[tokio::test]
    async fn the_delivering_follower_holds_its_turn() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(workers, events, runner);
        let mut follower = orchestrator.subscribe(Some("follow".to_string()), false);
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });

        orchestrator
            .submit_held(
                submit(root.path(), Some("follow"), "first", route),
                Some(follower.id()),
            )
            .unwrap();
        orchestrator
            .submit(submit(root.path(), Some("follow"), "second", route))
            .unwrap();
        let turn_id = loop {
            if let Event::TurnEnded { turn_id, held, .. } = next_event(&mut follower).await {
                assert!(held, "held for its follower");
                break turn_id;
            }
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(seen.lock().unwrap().len(), 1, "the next turn waits");
        orchestrator.release("follow", turn_id);
        orchestrator.wait_released("follow", turn_id).await;
        until_settled(&mut follower).await;
        assert_eq!(*seen.lock().unwrap(), ["first", "second"]);
    }

    /// A delivering follower that goes away without releasing frees its
    /// hold at once, not after the release timeout.
    #[tokio::test]
    async fn a_follower_that_disconnects_frees_its_hold() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(workers, events, runner);
        let mut watcher = orchestrator.subscribe(Some("gone".to_string()), false);
        let mut follower = orchestrator.subscribe(Some("gone".to_string()), false);
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });

        orchestrator
            .submit_held(
                submit(root.path(), Some("gone"), "first", route),
                Some(follower.id()),
            )
            .unwrap();
        orchestrator
            .submit(submit(root.path(), Some("gone"), "second", route))
            .unwrap();
        loop {
            if let Event::TurnEnded { held, .. } = next_event(&mut follower).await {
                assert!(held);
                break;
            }
        }
        // Let the drain task start waiting for the release.
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(follower);
        // `until_settled` times out after 10 s, far below the release timeout.
        until_settled(&mut watcher).await;
        assert_eq!(*seen.lock().unwrap(), ["first", "second"]);
    }

    #[tokio::test]
    async fn cancel_can_drop_waiting_inputs() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(workers, events, runner);
        let mut subscription = orchestrator.subscribe(Some("cancel".to_string()), false);

        orchestrator
            .submit(submit(root.path(), Some("cancel"), "wait", None))
            .unwrap();
        let (_, dropped_id, _) = orchestrator
            .submit(submit(root.path(), Some("cancel"), "never runs", None))
            .unwrap();
        while !matches!(
            next_event(&mut subscription).await,
            Event::TurnStarted { .. }
        ) {}
        let (cancelled, dropped) =
            orchestrator.cancel("cancel", CancelTarget::Thread { clear_queue: true });
        assert!(cancelled);
        assert_eq!(dropped, [dropped_id]);
        gate.notify_one();

        let events = until_settled(&mut subscription).await;
        assert!(events.iter().any(|event| matches!(event, Event::InputsDropped { input_ids, .. } if input_ids == &[dropped_id])));
        assert_eq!(*seen.lock().unwrap(), ["wait"]);
    }

    #[tokio::test]
    async fn submit_refuses_unusable_threads() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Orchestrator::start_with(workers, events, runner);

        let plain = Thread::new_with_root(root.path()).unwrap();
        let mut plain = plain;
        plain
            .append(&crate::core::thread_persistence::ThreadEvent::user_message(
                "hi",
            ))
            .unwrap();
        let err = orchestrator
            .submit(submit(root.path(), Some(&plain.id), "hello", None))
            .unwrap_err();
        assert!(
            err.to_string().contains("not a persistent-profile"),
            "{err}"
        );

        for bad in ["../escape", "", ".hidden", "a/b"] {
            assert!(
                orchestrator
                    .submit(submit(root.path(), Some(bad), "hello", None))
                    .is_err(),
                "{bad:?} accepted"
            );
        }
        let mut unknown = submit(root.path(), Some("custom"), "hello", None);
        unknown.profile = Some("missing-profile".to_string());
        assert!(orchestrator.submit(unknown).is_err());
        assert!(!thread_persistence::thread_exists("custom"));

        // A direct run on a profile thread is refused everywhere else.
        orchestrator
            .submit(submit(root.path(), Some("owned"), "hello", None))
            .unwrap();
        assert!(super::super::ensure_direct_run_allowed("owned").is_err());
        assert!(super::super::ensure_direct_run_allowed(&plain.id).is_ok());
    }
}
