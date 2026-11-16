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

use super::prompt::{OrdinaryLayer, prepare_turn, turn_config};
use super::protocol::{
    Delivery, ErrorCode, Event, Follow, InputRef, ReplayStatus, Route, StatusReport, Submit,
    ThreadState, ThreadStatus, TurnKind,
};
use super::thread_ops::{coded, validate_thread_id};
use super::updates::{InputImage, record_user_input, worker_update_prompt};
use crate::config::{Config, TextVerbosity, paths};
use crate::core::agent::{
    self, AgentEventRx, AgentEventTx, AgentOptions, ToolConfig, ToolSelection, create_event_channel,
};
use crate::core::events::{AgentEvent, ErrorKind, TurnStatus};
use crate::core::steer::{SOURCE_USER, SOURCE_WORKER_UPDATE, SteerInbox, SteerItem, SteerSource};
use crate::core::thread_lease::ThreadRunLease;
use crate::core::thread_persistence::{self, Thread};
use crate::core::workers::{WorkerEvent, WorkerManager};
use crate::subagents::{self, ORCHESTRATOR_SUBAGENT_NAME};
use crate::tools::ToolRegistry;
use crate::tools::orchestrator::OrchestratorTool;

/// `activity_kind` of turns run here. The agent loop refuses turns on a
/// persistent-profile thread from any other run, so this service is the one
/// writer of those threads.
pub const ACTIVITY_KIND: &str = "orchestrator";
/// Surface and activity kind of an interactive terminal chat's turns
/// ([`Submit::chat`]).
pub const CHAT_ACTIVITY_KIND: &str = "chat";

/// Longest a renderer may hold a thread after a turn before it continues.
const RELEASE_TIMEOUT: Duration = Duration::from_mins(10);

/// How often a drain retries the run lease another run holds.
const LEASE_RETRY: Duration = Duration::from_millis(500);

/// What [`Engine::cancel`] stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelTarget {
    Thread { clear_queue: bool },
    Input(u64),
    Turn(u64),
}

/// One turn's work handed to the turn runner.
pub struct TurnRequest {
    pub thread_id: String,
    /// An ordinary agent thread ([`Submit::ordinary`]), not a persistent
    /// profile.
    pub ordinary: bool,
    /// An ordinary turn of an interactive terminal chat ([`Submit::chat`]).
    pub chat: bool,
    /// Run on the persisted history without recording a user message
    /// ([`Submit::rerun`]); `text` and `images` are empty.
    pub rerun: bool,
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
    /// Steers for this turn (user messages, worker updates), taken at safe
    /// points. Unread ones go back to the queue when the turn ends.
    pub steer: SteerInbox,
    /// The thread's run lease, held by the queue drain from its first turn
    /// until the thread is idle (delivery holds included). `None` when it
    /// could not be taken; the runner then takes its own or refuses.
    pub run_lease: Option<Arc<ThreadRunLease>>,
}

/// Result of [`Engine::submit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub thread_id: String,
    pub input_id: u64,
    /// Turns and inputs ahead of this one on the thread.
    pub queued_ahead: usize,
    /// The input was steered into the running turn instead of queued.
    pub in_turn: bool,
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
    /// Run again on the persisted history, recording nothing new
    /// ([`Submit::rerun`]).
    Rerun,
}

struct Input {
    id: u64,
    /// A client streams this input's turn on its own connection.
    followed: bool,
    /// The follower that delivers this turn's reply in the thread's Telegram
    /// route; the turn is held until it releases.
    holder: Option<HolderRef>,
    kind: InputKind,
}

impl Input {
    fn is_worker_update(&self) -> bool {
        matches!(self.kind, InputKind::WorkerUpdate { .. })
    }
}

/// How a turn ended.
struct TurnOutcome {
    status: ReplayStatus,
    final_text: String,
    error: Option<String>,
    touched_workers: Vec<String>,
}

/// What [`Engine::run_turn`] needs besides the thread and turn ids.
struct TurnSetup {
    cancel: CancellationToken,
    steer: SteerInbox,
    batch: Vec<Input>,
    route: Option<Route>,
    config_root: PathBuf,
    ordinary: bool,
    chat: bool,
    run_lease: Option<Arc<ThreadRunLease>>,
}

/// Who owns a hold: a subscription, its token, and whether its holds may
/// be replayed to the renderer when it goes away before delivering.
#[derive(Debug, Clone)]
pub struct HolderRef {
    pub id: u64,
    pub token: String,
    pub replay: bool,
}

/// A thread waiting for the delivery of one turn's reply.
struct Hold {
    turn_id: u64,
    holder: HolderRef,
    /// A delivery record of this turn came in: never replay it.
    started: bool,
    /// The latest record of this turn, as sent.
    record: Option<crate::core::delivery::DeliveryRecord>,
    /// Marking a started delivery `unknown` failed; wait for the timeout or
    /// a forced release.
    stuck: bool,
    /// What a replay renders.
    reply: Option<ReplayReply>,
}

#[derive(Debug, Clone)]
struct ReplayReply {
    route: Route,
    status: ReplayStatus,
    final_text: String,
    error: Option<String>,
}

/// What [`Engine::wait_for_release`] does about a hold whose owner is
/// gone.
enum Orphan {
    Wait,
    Release,
    MarkUnknown(crate::core::delivery::DeliveryRecord),
    Replay(u64, Event),
}

struct Running {
    turn_id: u64,
    /// Inputs this turn holds: the batch it started with, then every steer
    /// admitted into it (so a cancel naming one finds the turn).
    inputs: Vec<u64>,
    cancel: CancellationToken,
    steer: SteerInbox,
    /// A thread cancel that cleared the queue: unread steers are dropped, not
    /// put back, when the turn ends.
    drop_steers: bool,
}

struct ThreadEntry {
    /// Runs as an ordinary agent thread ([`Submit::ordinary`]).
    ordinary: bool,
    /// Its ordinary turns are an interactive chat's ([`Submit::chat`]); the
    /// latest submit decides.
    chat: bool,
    config_root: PathBuf,
    route: Option<Route>,
    queue: VecDeque<Input>,
    running: Option<Running>,
    held: Option<Hold>,
    draining: bool,
    /// The thread's run lease, taken when the first input is accepted and
    /// kept until the thread is idle again.
    lease: Option<Arc<ThreadRunLease>>,
}

impl ThreadEntry {
    fn new(config_root: PathBuf) -> Self {
        Self {
            ordinary: false,
            chat: false,
            config_root,
            route: None,
            queue: VecDeque::new(),
            running: None,
            held: None,
            draining: false,
            lease: None,
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
    token: String,
    thread: Option<String>,
    renderer: bool,
    replay: bool,
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

pub struct Engine {
    workers: Arc<WorkerManager>,
    registry: ToolRegistry,
    runner: TurnRunner,
    threads: Mutex<HashMap<String, ThreadEntry>>,
    subscribers: Mutex<Vec<Subscriber>>,
    release: Notify,
    next_id: AtomicU64,
    control: mpsc::UnboundedSender<Control>,
    /// `[workers] update_delivery`: whether a worker result joins a running
    /// turn of its thread at the next safe point (`Steer`) or always queues.
    update_delivery: crate::config::UpdateDelivery,
    /// Builds read-only views; installed by the binary that hosts the service.
    views: std::sync::OnceLock<Arc<dyn super::views::ViewProvider>>,
    /// In-flight feedback submissions and decode slots.
    feedback: super::feedback::FeedbackGate,
}

/// Receives the events of one subscription; unsubscribes on drop.
pub struct Subscription {
    orchestrator: Arc<Engine>,
    id: u64,
    token: String,
    pub events: mpsc::UnboundedReceiver<Event>,
}

impl Subscription {
    /// Id that names this subscription as a turn's holder.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Token a client names this subscription's holds with.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// This subscription as the owner of a hold.
    #[must_use]
    pub fn holder(&self, replay: bool) -> HolderRef {
        HolderRef {
            id: self.id,
            token: self.token.clone(),
            replay,
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.orchestrator.unsubscribe(self.id);
    }
}

impl Engine {
    /// Starts a runtime whose workers run through the configured runner
    /// (`[workers]`: `zdx --thread <id> exec` by default) and whose turns run
    /// the thread's persistent profile. Must run inside a Tokio runtime.
    ///
    /// # Errors
    /// Returns an error when the worker runner is not validly configured.
    pub fn start(workers: &crate::config::WorkersConfig) -> anyhow::Result<Arc<Self>> {
        let update_delivery = workers.update_delivery;
        let (workers, worker_events) = WorkerManager::from_config(workers)?;
        let runner: TurnRunner = Arc::new(|request| {
            Box::pin(async move {
                if request.ordinary {
                    run_ordinary_turn(request).await;
                } else {
                    run_profile_turn(request).await;
                }
            })
        });
        let orchestrator =
            Self::start_with_delivery(workers, worker_events, runner, update_delivery);
        orchestrator.recover_mirrors();
        Ok(orchestrator)
    }

    /// Starts a runtime over an existing worker manager and turn runner
    /// (tests use fakes for both).
    #[must_use]
    pub fn start_with(
        workers: Arc<WorkerManager>,
        worker_events: mpsc::UnboundedReceiver<WorkerEvent>,
        runner: TurnRunner,
    ) -> Arc<Self> {
        Self::start_with_delivery(
            workers,
            worker_events,
            runner,
            crate::config::UpdateDelivery::Queue,
        )
    }

    /// [`Self::start_with`] with an explicit worker-update delivery.
    #[must_use]
    pub fn start_with_delivery(
        workers: Arc<WorkerManager>,
        worker_events: mpsc::UnboundedReceiver<WorkerEvent>,
        runner: TurnRunner,
        update_delivery: crate::config::UpdateDelivery,
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
            update_delivery,
            views: std::sync::OnceLock::new(),
            feedback: super::feedback::FeedbackGate::default(),
        });
        tokio::spawn(Arc::clone(&orchestrator).deliver_worker_events(worker_events, control_rx));
        orchestrator
    }

    #[must_use]
    pub fn workers(&self) -> &Arc<WorkerManager> {
        &self.workers
    }

    pub(super) fn feedback_gate(&self) -> &super::feedback::FeedbackGate {
        &self.feedback
    }

    /// Installs the view provider (once); its capabilities are listed from
    /// then on.
    pub fn install_views(&self, provider: Arc<dyn super::views::ViewProvider>) {
        let _ = self.views.set(provider);
    }

    #[must_use]
    pub fn views(&self) -> Option<&Arc<dyn super::views::ViewProvider>> {
        self.views.get()
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
            if let Some(mirror) =
                crate::core::workers::WorkerMirror::from_topic_thread(&topic_thread_id)
            {
                self.workers.set_mirror(&worker_thread_id, mirror);
            }
        }
    }

    /// Queues one input on a persistent-profile thread, or with
    /// [`Submit::ordinary`] an ordinary agent thread, creating the thread
    /// when it does not exist yet. Returns the thread id, the input id, and
    /// how many turns or inputs are ahead of it.
    ///
    /// # Errors
    /// Returns an error when the thread id is unusable, an existing thread
    /// is of the other kind (or has a different profile), the profile cannot
    /// be loaded, or the thread cannot be created.
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub fn submit(self: &Arc<Self>, submit: Submit) -> Result<Submitted> {
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
    #[allow(clippy::too_many_lines)]
    pub fn submit_held(
        self: &Arc<Self>,
        submit: Submit,
        holder: Option<HolderRef>,
    ) -> Result<Submitted> {
        if submit.rerun {
            if !submit.ordinary {
                return Err(coded(
                    ErrorCode::Invalid,
                    "`rerun` applies only to an ordinary thread",
                ));
            }
            if !submit.text.is_empty() || !submit.images.is_empty() {
                return Err(coded(
                    ErrorCode::Invalid,
                    "`rerun` runs the persisted history; send no text or images",
                ));
            }
            match submit.thread_id.as_deref() {
                Some(id) if thread_persistence::thread_exists(id) => {}
                _ => {
                    return Err(coded(
                        ErrorCode::NotFound,
                        "`rerun` needs an existing thread",
                    ));
                }
            }
        } else if submit.text.trim().is_empty() && submit.images.is_empty() {
            return Err(coded(ErrorCode::Invalid, "message is empty"));
        }
        if submit.chat && (!submit.ordinary || submit.route.is_some()) {
            return Err(coded(
                ErrorCode::Invalid,
                "`chat` applies only to an ordinary thread without a route",
            ));
        }
        let root = submit
            .root
            .canonicalize()
            .with_context(|| format!("project root does not exist: {}", submit.root.display()))?;
        // Creating the thread and queueing its first input happen under the
        // queue lock, so no turn (or concurrent creator) sees a thread whose
        // metadata is still being written.
        let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
        // A thread this run already queued keeps its kind: a submit of the
        // other kind could otherwise create a profile thread under an
        // ordinary one whose file is not written yet.
        if let Some(id) = submit.thread_id.as_deref()
            && let Some(entry) = threads.get(id)
            && entry.ordinary != submit.ordinary
        {
            return Err(coded(
                ErrorCode::Conflict,
                format!(
                    "thread '{id}' runs as {} thread here; submit it {} `ordinary`",
                    if entry.ordinary {
                        "an ordinary"
                    } else {
                        "a persistent-profile"
                    },
                    if entry.ordinary { "with" } else { "without" },
                ),
            ));
        }
        // A chat turn must not take the route a Telegram submit left on the
        // thread: its reply would be held for the bot.
        if submit.chat
            && let Some(id) = submit.thread_id.as_deref()
            && threads.get(id).is_some_and(|entry| entry.route.is_some())
        {
            return Err(coded(
                ErrorCode::Conflict,
                format!("thread '{id}' answers in Telegram here; open it in its topic"),
            ));
        }
        // The run lease is taken when the first input is accepted, so no
        // other run can start on the thread before this one's turn does.
        let mut accepted_lease = match submit.thread_id.as_deref() {
            Some(id) if threads.get(id).is_none_or(|entry| entry.lease.is_none()) => {
                Some(Arc::new(acquire_for_submit(id)?))
            }
            _ => None,
        };
        let thread_id = if submit.ordinary {
            if submit.profile.is_some() {
                return Err(coded(
                    ErrorCode::Invalid,
                    "`profile` does not apply to an ordinary thread",
                ));
            }
            open_ordinary_thread(submit.thread_id.as_deref(), &root, submit.model.as_deref())?
        } else {
            open_profile_thread(
                submit.thread_id.as_deref(),
                &root,
                submit.profile.as_deref(),
                submit.model.as_deref(),
            )?
        };

        let followed = submit.follow != Follow::None;
        let input_id = self.next_id();
        let entry = threads
            .entry(thread_id.clone())
            .or_insert_with(|| ThreadEntry::new(root.clone()));
        if entry.lease.is_none() {
            entry.lease = Some(match accepted_lease.take() {
                Some(lease) => lease,
                None => Arc::new(acquire_for_submit(&thread_id)?),
            });
        }
        entry.ordinary = submit.ordinary;
        entry.chat = submit.chat;
        entry.config_root = root;
        if submit.route.is_some() {
            entry.route = submit.route;
        }
        // A steer joins the running turn at its next safe point; images are
        // not steered, and a closed or absent turn means the normal queue.
        let text = if submit.delivery == Delivery::Steer
            && submit.images.is_empty()
            && !submit.rerun
        {
            match admit_steer(entry, input_id, SOURCE_USER, submit.text) {
                Ok(()) => {
                    tracing::info!(thread = %thread_id, input_id, "Steer accepted for the running turn");
                    return Ok(Submitted {
                        thread_id,
                        input_id,
                        queued_ahead: 0,
                        in_turn: true,
                    });
                }
                Err(text) => text,
            }
        } else {
            submit.text
        };
        let input = Input {
            id: input_id,
            followed,
            holder: holder.filter(|_| submit.route.is_some()),
            kind: if submit.rerun {
                InputKind::Rerun
            } else {
                InputKind::User {
                    text,
                    images: submit.images,
                }
            },
        };
        let ahead = entry.queue.len()
            + usize::from(entry.running.is_some())
            + usize::from(entry.held.is_some());
        entry.queue.push_back(input);
        drop(threads);
        self.ensure_draining(&thread_id);
        Ok(Submitted {
            thread_id,
            input_id,
            queued_ahead: ahead,
            in_turn: false,
        })
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
                .as_mut()
                .filter(|running| matches(running))
                .is_some_and(|running| {
                    running.cancel.cancel();
                    // No more steers join a cancelled turn; its unread ones
                    // are settled when it ends.
                    running.steer.close();
                    true
                });
            let dropped: Vec<u64> = match target {
                CancelTarget::Thread { clear_queue: true } => {
                    let mut dropped: Vec<u64> = Vec::new();
                    if let Some(running) = entry.running.as_mut() {
                        running.drop_steers = true;
                        dropped.extend(
                            running
                                .steer
                                .drain_unread()
                                .iter()
                                .filter_map(steer_input_id),
                        );
                    }
                    dropped.extend(entry.queue.drain(..).map(|input| input.id));
                    dropped
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

    /// Deletes a thread's history ([`super::thread_ops::reset_thread`]) while
    /// holding the queue lock, so no turn of this service starts on it
    /// meanwhile.
    ///
    /// # Errors
    /// `conflict` while the thread has a turn queued, running, or held here;
    /// otherwise as [`super::thread_ops::reset_thread`].
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub fn reset_thread(&self, thread_id: &str) -> Result<()> {
        let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
        if threads
            .get(thread_id)
            .is_some_and(|entry| !entry.is_idle() || entry.draining)
        {
            return Err(coded(
                ErrorCode::Conflict,
                format!("thread '{thread_id}' has work queued or running; cancel it first"),
            ));
        }
        super::thread_ops::reset_thread(thread_id)?;
        threads.remove(thread_id);
        Ok(())
    }

    /// Applies a metadata change ([`super::thread_ops::update_thread`]). A
    /// thread this service has work for keeps its kind: making it a
    /// persistent-profile thread is refused then.
    ///
    /// # Errors
    /// `conflict` for a profile on a thread with work here; otherwise as
    /// [`super::thread_ops::update_thread`].
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub fn update_thread(
        &self,
        update: &super::thread_ops::UpdateThread,
    ) -> Result<super::thread_ops::ThreadUpdated> {
        let threads = self.threads.lock().expect("orchestrator lock poisoned");
        if update.persistent_profile.is_some()
            && threads
                .get(&update.thread_id)
                .is_some_and(|entry| !entry.is_idle() || entry.draining || entry.ordinary)
        {
            return Err(coded(
                ErrorCode::Conflict,
                format!(
                    "thread '{}' runs here as an ordinary thread; it cannot take a profile",
                    update.thread_id
                ),
            ));
        }
        super::thread_ops::update_thread(update)
    }

    /// The holder delivered `turn_id`; the thread may run its next turn.
    /// `holder` must be the hold's token, unless `force`. A turn that is not
    /// held (released, timed out) is released already.
    ///
    /// # Errors
    /// `invalid` without a token or `force`; `conflict` for another token.
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub fn release(
        &self,
        thread_id: &str,
        turn_id: u64,
        holder: Option<&str>,
        force: bool,
    ) -> Result<()> {
        if holder.is_none() && !force {
            return Err(coded(
                ErrorCode::Invalid,
                "release needs the hold's `holder` token, or `force`",
            ));
        }
        {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            let Some(entry) = threads.get_mut(thread_id) else {
                return Ok(());
            };
            let Some(hold) = entry.held.as_ref().filter(|hold| hold.turn_id == turn_id) else {
                return Ok(());
            };
            if !force && holder != Some(hold.holder.token.as_str()) {
                return Err(coded(
                    ErrorCode::Conflict,
                    format!("turn {turn_id} of thread '{thread_id}' is held by another client"),
                ));
            }
            if force {
                tracing::warn!(thread = %thread_id, turn_id, "Hold released by force");
            }
            entry.held = None;
        }
        self.release.notify_waiters();
        Ok(())
    }

    /// Records a delivery of a thread's reply. While the thread is held,
    /// `turn_id` and `holder` must name the hold; the turn then counts as
    /// started before anything is written, so it is never replayed.
    /// Returns the record's user summary.
    ///
    /// # Errors
    /// `conflict` when they do not name the hold; else as
    /// [`super::thread_ops::record_delivery`].
    ///
    /// # Panics
    /// Panics if an internal lock is poisoned.
    pub async fn record_delivery(
        &self,
        thread_id: String,
        record: crate::core::delivery::DeliveryRecord,
        turn_id: Option<u64>,
        holder: Option<String>,
    ) -> Result<Option<String>> {
        {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            if let Some(hold) = threads
                .get_mut(&thread_id)
                .and_then(|entry| entry.held.as_mut())
            {
                if turn_id != Some(hold.turn_id) || holder.as_deref() != Some(&hold.holder.token) {
                    return Err(coded(
                        ErrorCode::Conflict,
                        format!(
                            "thread '{thread_id}' is held for turn {}; a record for it must name that turn and its holder",
                            hold.turn_id
                        ),
                    ));
                }
                hold.started = true;
                hold.record = Some(record.clone());
            }
        }
        tokio::task::spawn_blocking(move || super::thread_ops::record_delivery(&thread_id, record))
            .await
            .context("record delivery task")?
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
            protocol: super::protocol::PROTOCOL_VERSION,
            capabilities: super::protocol::CAPABILITIES
                .iter()
                .chain(
                    self.views
                        .get()
                        .map_or(&[][..], |_| super::protocol::VIEW_CAPABILITIES),
                )
                .map(|name| (*name).to_string())
                .collect(),
            settled,
            threads: statuses,
            views_root: self.views.get().map(|views| views.root().to_path_buf()),
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
    /// # Errors
    /// `conflict` for a renderer while another renderer is subscribed.
    pub fn subscribe(
        self: &Arc<Self>,
        thread: Option<String>,
        renderer: bool,
        replay: bool,
    ) -> Result<Subscription> {
        let (tx, events) = mpsc::unbounded_channel();
        let id = self.next_id();
        let token = uuid::Uuid::new_v4().to_string();
        {
            let mut subscribers = self.subscribers.lock().expect("orchestrator lock poisoned");
            if renderer && subscribers.iter().any(|subscriber| subscriber.renderer) {
                return Err(coded(
                    ErrorCode::Conflict,
                    "another renderer is subscribed; only one may render this service's threads",
                ));
            }
            subscribers.push(Subscriber {
                id,
                token: token.clone(),
                thread,
                renderer,
                replay: renderer && replay,
                tx,
            });
        }
        if renderer && replay {
            // A hold waiting for a replay renderer can go to this one.
            self.release.notify_waiters();
        }
        Ok(Subscription {
            orchestrator: Arc::clone(self),
            id,
            token,
            events,
        })
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

    /// The renderer subscribed now, if any, as a hold owner.
    fn current_renderer(&self) -> Option<HolderRef> {
        self.subscribers
            .lock()
            .expect("orchestrator lock poisoned")
            .iter()
            .find(|subscriber| subscriber.renderer)
            .map(|subscriber| HolderRef {
                id: subscriber.id,
                token: subscriber.token.clone(),
                replay: subscriber.replay,
            })
    }

    /// Sends `event` to one subscription only.
    fn emit_to(&self, id: u64, event: Event) {
        if let Some(subscriber) = self
            .subscribers
            .lock()
            .expect("orchestrator lock poisoned")
            .iter()
            .find(|subscriber| subscriber.id == id)
        {
            let _ = subscriber.tx.send(event);
        }
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
    #[allow(clippy::too_many_lines)]
    async fn drain(self: Arc<Self>, thread_id: String) {
        loop {
            // Held until the thread is idle, delivery holds included, so no
            // other run writes the thread between this service's turns.
            let Some(run_lease) = self.lease_for_drain(&thread_id).await else {
                return;
            };
            let next = {
                let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
                let Some(entry) = threads.get_mut(&thread_id) else {
                    return;
                };
                match entry.queue.pop_front() {
                    None => {
                        entry.draining = false;
                        entry.lease = None;
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
                        let steer = SteerInbox::new();
                        entry.running = Some(Running {
                            turn_id,
                            inputs: batch.iter().map(|input| input.id).collect(),
                            cancel: cancel.clone(),
                            steer: steer.clone(),
                            drop_steers: false,
                        });
                        Some((
                            turn_id,
                            cancel,
                            steer,
                            batch,
                            entry.route,
                            entry.config_root.clone(),
                            entry.ordinary,
                            entry.chat,
                        ))
                    }
                }
            };
            let Some((turn_id, cancel, steer, batch, route, config_root, ordinary, chat)) = next
            else {
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
            let holder = if route.is_none() {
                None
            } else if let Some(holder) = batch.iter().find_map(|input| input.holder.clone()) {
                Some(holder)
            } else if batch.iter().all(|input| !input.followed) {
                self.current_renderer()
            } else {
                None
            };
            let reply = self
                .run_turn(
                    &thread_id,
                    turn_id,
                    TurnSetup {
                        cancel,
                        steer,
                        batch,
                        route,
                        config_root,
                        ordinary,
                        chat,
                        run_lease: Some(run_lease),
                    },
                )
                .await;

            // A holder that is gone keeps the hold only when its reply may
            // be replayed to the renderer.
            let hold = holder
                .filter(|holder| holder.replay || self.is_subscribed(holder.id))
                .map(|holder| Hold {
                    turn_id,
                    holder,
                    started: false,
                    record: None,
                    stuck: false,
                    reply: route.map(|route| ReplayReply {
                        route,
                        status: reply.status,
                        final_text: reply.final_text.clone(),
                        error: reply.error.clone(),
                    }),
                });
            let is_held = self.end_turn(&thread_id, turn_id, hold);
            self.emit(&Event::TurnEnded {
                thread_id: thread_id.clone(),
                turn_id,
                held: is_held,
                touched_workers: reply.touched_workers,
            });
            if is_held {
                self.wait_for_release(&thread_id, turn_id).await;
            }
        }
    }

    /// Ends a turn: settles its unread steers in the same step that clears it,
    /// so the thread never looks idle with one, and holds the thread for its
    /// renderer. Returns whether it is held.
    fn end_turn(&self, thread_id: &str, turn_id: u64, hold: Option<Hold>) -> bool {
        let (is_held, not_added) = {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            threads
                .get_mut(thread_id)
                .map_or((false, Vec::new()), |entry| {
                    let not_added = entry
                        .running
                        .take()
                        .map(|running| settle_steers(entry, &running))
                        .unwrap_or_default();
                    let holding = hold.is_some();
                    entry.held = hold.filter(|hold| hold.turn_id == turn_id);
                    (holding, not_added)
                })
        };
        if !not_added.is_empty() {
            self.emit(&Event::InputsDropped {
                thread_id: thread_id.to_string(),
                input_ids: not_added,
            });
        }
        is_held
    }

    /// Waits until the hold of `turn_id` ends: released by its holder,
    /// [`RELEASE_TIMEOUT`] after the turn ended, or settled here when its
    /// holder goes away (see [`Orphan`]).
    async fn wait_for_release(&self, thread_id: &str, turn_id: u64) {
        let deadline = tokio::time::Instant::now() + RELEASE_TIMEOUT;
        loop {
            let notified = self.release.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (subscribed, replay_renderer) = self.subscriber_view();
            let orphan = {
                let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
                let Some(hold) = threads
                    .get_mut(thread_id)
                    .and_then(|entry| entry.held.as_mut())
                    .filter(|hold| hold.turn_id == turn_id)
                else {
                    return;
                };
                orphan_action(thread_id, hold, &subscribed, replay_renderer.as_ref())
            };
            match orphan {
                Orphan::Wait => {}
                Orphan::Release => {
                    tracing::info!(thread = %thread_id, turn_id, "Hold owner gone; releasing the turn");
                    break;
                }
                Orphan::Replay(renderer, event) => {
                    tracing::info!(thread = %thread_id, turn_id, "Hold owner gone before delivering; replaying the turn to the renderer");
                    self.emit_to(renderer, event);
                    continue;
                }
                Orphan::MarkUnknown(record) => {
                    let id = thread_id.to_string();
                    let written = tokio::task::spawn_blocking(move || {
                        super::thread_ops::record_delivery(&id, record)
                    })
                    .await
                    .map_err(anyhow::Error::new)
                    .and_then(|result| result);
                    match written {
                        Ok(_) => {
                            tracing::warn!(thread = %thread_id, turn_id, "Hold owner gone mid-delivery; pending items marked unknown");
                            break;
                        }
                        Err(err) => {
                            tracing::error!(thread = %thread_id, turn_id, err = %format!("{err:#}"), "Could not mark an interrupted delivery unknown; the thread stays held until the release timeout");
                            if let Some(hold) = self
                                .threads
                                .lock()
                                .expect("orchestrator lock poisoned")
                                .get_mut(thread_id)
                                .and_then(|entry| entry.held.as_mut())
                                .filter(|hold| hold.turn_id == turn_id)
                            {
                                hold.stuck = true;
                            }
                            continue;
                        }
                    }
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                tracing::warn!(thread = %thread_id, turn_id, "Turn never released; continuing (an undelivered reply is lost)");
                break;
            }
        }
        let cleared = {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            threads
                .get_mut(thread_id)
                .filter(|entry| {
                    entry
                        .held
                        .as_ref()
                        .is_some_and(|hold| hold.turn_id == turn_id)
                })
                .map(|entry| entry.held = None)
                .is_some()
        };
        if cleared {
            self.release.notify_waiters();
        }
    }

    /// Ids of every subscription, and the replay renderer when one is
    /// subscribed.
    fn subscriber_view(&self) -> (Vec<u64>, Option<HolderRef>) {
        let subscribers = self.subscribers.lock().expect("orchestrator lock poisoned");
        let ids = subscribers.iter().map(|subscriber| subscriber.id).collect();
        let renderer = subscribers
            .iter()
            .find(|subscriber| subscriber.renderer && subscriber.replay)
            .map(|subscriber| HolderRef {
                id: subscriber.id,
                token: subscriber.token.clone(),
                replay: true,
            });
        (ids, renderer)
    }

    /// The thread's run lease for the drain: the one taken at acceptance,
    /// else a new one. While another run holds the thread, waits for it,
    /// unless the queue empties meanwhile (`None`: the drain ends).
    async fn lease_for_drain(&self, thread_id: &str) -> Option<Arc<ThreadRunLease>> {
        let mut logged = false;
        loop {
            {
                let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
                let entry = threads.get_mut(thread_id)?;
                if let Some(lease) = &entry.lease {
                    return Some(Arc::clone(lease));
                }
                if entry.queue.is_empty() {
                    entry.draining = false;
                    drop(threads);
                    let _ = self
                        .control
                        .send(Control::ThreadIdle(thread_id.to_string()));
                    return None;
                }
                match ThreadRunLease::try_acquire(thread_id) {
                    Ok(lease) => {
                        let lease = Arc::new(lease);
                        entry.lease = Some(Arc::clone(&lease));
                        return Some(lease);
                    }
                    Err(err) if crate::core::thread_lease::is_busy(&err) => {}
                    Err(err) => {
                        tracing::warn!(thread = %thread_id, err = %format!("{err:#}"), "Run lease unavailable; retrying");
                    }
                }
            }
            if !logged {
                logged = true;
                tracing::info!(thread = %thread_id, "Another run holds the thread; queued input waits for it");
            }
            tokio::time::sleep(LEASE_RETRY).await;
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
            .is_some_and(|entry| {
                entry
                    .held
                    .as_ref()
                    .is_some_and(|hold| hold.turn_id == turn_id)
            })
    }

    /// Runs one turn and returns how it ended.
    #[allow(clippy::too_many_lines)]
    async fn run_turn(&self, thread_id: &str, turn_id: u64, setup: TurnSetup) -> TurnOutcome {
        let TurnSetup {
            cancel,
            steer,
            batch,
            route,
            config_root,
            ordinary,
            chat,
            run_lease,
        } = setup;
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
        let mut rerun = false;
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
                InputKind::Rerun => rerun = true,
            }
        }
        let (events, mut rx) = create_event_channel();
        let run = (self.runner)(TurnRequest {
            thread_id: thread_id.to_string(),
            ordinary,
            chat,
            rerun,
            config_root,
            route,
            // Batched inputs in arrival order.
            text: texts.join("\n\n"),
            images,
            events,
            cancel,
            registry: self.registry.clone(),
            steer,
            run_lease,
        });
        let mut touched_workers: Vec<String> = Vec::new();
        let forward = async {
            let mut finished = None;
            // Tool call id → name: `ToolCompleted` carries only the id.
            let mut tool_names: HashMap<String, String> = HashMap::new();
            while let Some(event) = rx.recv().await {
                match &*event {
                    AgentEvent::ToolStarted { id, name } => {
                        tool_names.insert(id.clone(), name.clone());
                    }
                    AgentEvent::ToolCompleted { id, result, .. } => {
                        if let Some(name) = tool_names.remove(id)
                            && let Some(worker) =
                                crate::tools::orchestrator::touched_worker_id(&name, result)
                            && !touched_workers.contains(&worker)
                        {
                            touched_workers.push(worker);
                        }
                    }
                    _ => {}
                }
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
        let (status, final_text, error) = match &finished {
            AgentEvent::TurnFinished {
                status, final_text, ..
            } => {
                let (status, error) = match status {
                    TurnStatus::Completed => (ReplayStatus::Completed, None),
                    TurnStatus::Interrupted => (ReplayStatus::Interrupted, None),
                    TurnStatus::Failed { message, .. } => {
                        (ReplayStatus::Failed, Some(message.clone()))
                    }
                };
                (status, final_text.clone(), error)
            }
            _ => (ReplayStatus::Failed, String::new(), None),
        };
        let outcome = TurnOutcome {
            status,
            final_text,
            error,
            touched_workers,
        };
        self.emit(&Event::Agent {
            thread_id: thread_id.to_string(),
            turn_id,
            event: finished,
        });
        outcome
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
        let text = worker_update_prompt(completion, &worker_context);
        let steered = {
            let mut threads = self.threads.lock().expect("orchestrator lock poisoned");
            // Allocated under the lock, so input ids follow admission order.
            let id = self.next_id();
            let entry = threads
                .entry(owner.clone())
                .or_insert_with(|| ThreadEntry::new(config_root));
            let text = if self.update_delivery == crate::config::UpdateDelivery::Steer {
                admit_steer(entry, id, SOURCE_WORKER_UPDATE, text).err()
            } else {
                Some(text)
            };
            match text {
                None => true,
                Some(text) => {
                    entry.queue.push_back(Input {
                        id,
                        followed: false,
                        holder: None,
                        kind: InputKind::WorkerUpdate { text },
                    });
                    false
                }
            }
        };
        tracing::info!(
            owner = %owner,
            worker = %completion.worker_thread_id,
            status = completion.status.as_str(),
            steered,
            "Queued worker update"
        );
        if !steered {
            self.ensure_draining(owner);
        }
    }
}

/// Puts `text` into the thread's running turn as a steer. Hands the text back
/// when no turn runs or its inbox is closed; the caller queues it then.
/// Caller holds the threads lock.
fn admit_steer(
    entry: &mut ThreadEntry,
    input_id: u64,
    source: &str,
    text: String,
) -> std::result::Result<(), String> {
    let Some(running) = entry.running.as_mut() else {
        return Err(text);
    };
    let item = SteerItem {
        id: input_id.to_string(),
        source: source.to_string(),
        text,
    };
    match running.steer.try_push(item) {
        Ok(_) => {
            running.inputs.push(input_id);
            Ok(())
        }
        Err(item) => Err(item.text),
    }
}

fn steer_input_id(item: &SteerItem) -> Option<u64> {
    item.id.parse().ok()
}

/// Settles the steers a turn never read, when the turn ends. Caller holds the
/// threads lock. Unread worker updates go back to the queue (results are
/// never lost unless a cancel cleared it); unread user messages go back to
/// the queue too, except after a cancel, which gives them back instead.
/// Returns the input ids not added (given back or dropped).
fn settle_steers(entry: &mut ThreadEntry, running: &Running) -> Vec<u64> {
    let unread = running.steer.drain_unread();
    let cancelled = running.cancel.is_cancelled();
    let mut not_added = Vec::new();
    for item in unread {
        let Some(id) = steer_input_id(&item) else {
            continue;
        };
        let worker_update = item.source == SOURCE_WORKER_UPDATE;
        if running.drop_steers || (cancelled && !worker_update) {
            not_added.push(id);
            continue;
        }
        let input = Input {
            id,
            followed: false,
            holder: None,
            kind: if worker_update {
                InputKind::WorkerUpdate { text: item.text }
            } else {
                InputKind::User {
                    text: item.text,
                    images: Vec::new(),
                }
            },
        };
        // Ids follow admission order, so this keeps arrival order against
        // anything queued after the turn closed its inbox.
        let at = entry
            .queue
            .iter()
            .position(|queued| queued.id > id)
            .unwrap_or(entry.queue.len());
        entry.queue.insert(at, input);
    }
    not_added
}

/// Opens (or creates) an ordinary agent thread and returns its id. A new
/// thread is written with its root (and `model`), so it groups by project and
/// no submit of another kind can claim the id.
fn open_ordinary_thread(
    thread_id: Option<&str>,
    root: &Path,
    model: Option<&str>,
) -> Result<String> {
    if let Some(id) = thread_id {
        validate_thread_id(id)?;
        if thread_persistence::thread_exists(id) {
            if let Some(profile) = thread_persistence::read_persistent_profile(id)? {
                return Err(coded(
                    ErrorCode::Conflict,
                    format!(
                        "thread '{id}' runs the '{profile}' persistent profile; submit it without `ordinary`"
                    ),
                ));
            }
            if model.is_some() {
                return Err(coded(
                    ErrorCode::Invalid,
                    format!("--model applies only when a thread is created; thread '{id}' exists"),
                ));
            }
            return Ok(id.to_string());
        }
    }
    if let Some(model) = model
        && crate::config::mode_ref_name(model).is_some()
    {
        let mut config = load_config(root)?;
        config.apply_model_selection(model)?;
    }
    let _lease = thread_id.map(lease_for_create).transpose()?;
    let mut thread = match thread_id {
        Some(id) => Thread::with_id(id.to_string())?,
        None => Thread::new_with_root(root)?,
    };
    thread.set_root_path(root)?;
    if let Some(model) = model {
        thread.set_model_override(Some(model.to_string()))?;
    }
    Ok(thread.id.clone())
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
            let existing = thread_persistence::read_persistent_profile(id)?.ok_or_else(|| {
                coded(
                    ErrorCode::Conflict,
                    format!("thread '{id}' is not a persistent-profile (orchestrator) thread"),
                )
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
    if let Some(model) = model
        && crate::config::mode_ref_name(model).is_some()
    {
        // Refuse a mode the thread's root does not define before anything is
        // written, instead of failing every later turn.
        let mut config = load_config(root)?;
        config.apply_model_selection(model)?;
    }
    let _lease = thread_id.map(lease_for_create).transpose()?;
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

/// The run lease while a new thread with a chosen id is created, so its
/// first write cannot interleave with another run creating it. `conflict`
/// when another holder has it.
fn lease_for_create(thread_id: &str) -> Result<Option<ThreadRunLease>> {
    if ThreadRunLease::held_here(thread_id) {
        return Ok(None);
    }
    ThreadRunLease::try_acquire(thread_id)
        .map(Some)
        .map_err(|err| {
            if crate::core::thread_lease::is_busy(&err) {
                coded(ErrorCode::Conflict, err.to_string())
            } else {
                err
            }
        })
}

/// The run lease for an accepted input: `conflict` while another run holds
/// the thread.
fn acquire_for_submit(thread_id: &str) -> Result<ThreadRunLease> {
    ThreadRunLease::try_acquire(thread_id).map_err(|err| {
        if crate::core::thread_lease::is_busy(&err) {
            coded(ErrorCode::Conflict, err.to_string())
        } else {
            err
        }
    })
}

/// What to do about `hold` now. Its owner still subscribed: wait. Gone
/// after its delivery started: mark the pending items `unknown` (never
/// resend), or release when none is pending. Gone before: hand it to the
/// replay renderer when the owner opted in (or wait for one), else release.
fn orphan_action(
    thread_id: &str,
    hold: &mut Hold,
    subscribed: &[u64],
    replay_renderer: Option<&HolderRef>,
) -> Orphan {
    if subscribed.contains(&hold.holder.id) || hold.stuck {
        return Orphan::Wait;
    }
    if hold.started {
        return match hold.record.as_ref().filter(|record| record.has_pending()) {
            Some(record) => Orphan::MarkUnknown(record.interrupted()),
            None => Orphan::Release,
        };
    }
    if !hold.holder.replay {
        return Orphan::Release;
    }
    let (Some(renderer), Some(reply)) = (replay_renderer, hold.reply.as_ref()) else {
        return Orphan::Wait;
    };
    hold.holder = renderer.clone();
    Orphan::Replay(
        renderer.id,
        Event::TurnReplay {
            thread_id: thread_id.to_string(),
            turn_id: hold.turn_id,
            route: reply.route,
            status: reply.status,
            final_text: reply.final_text.clone(),
            error: reply.error.clone(),
        },
    )
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
        ordinary: _,
        chat: _,
        rerun: _,
        config_root,
        route,
        text,
        images,
        events,
        cancel,
        registry,
        steer,
        run_lease,
    } = request;
    let prepared = match prepare_profile_turn(
        &thread_id,
        &config_root,
        route,
        text,
        &images,
        run_lease,
    ) {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::error!(thread = %thread_id, err = %format!("{err:#}"), "Failed to prepare orchestrator turn");
            let _ = events.send(Arc::new(failed(format!("{err:#}"))));
            return;
        }
    };
    let ReadyTurn {
        run_lease,
        config,
        run_root,
        system_prompt,
        tools,
        allowed_subagents,
        skill_scope,
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
            skill_scope,
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
        steer: Some(SteerSource::Inbox(steer)),
        run_lease: Some(run_lease),
    };
    run_and_persist(
        &thread_id,
        thread,
        messages,
        &config,
        &options,
        Some(&system_prompt),
        events,
        cancel,
    )
    .await;
}

/// Runs the agent and resolves once every event of the turn is persisted.
#[allow(clippy::too_many_arguments)]
async fn run_and_persist(
    thread_id: &str,
    thread: Thread,
    messages: Vec<crate::providers::ChatMessage>,
    config: &Config,
    options: &AgentOptions,
    system_prompt: Option<&str>,
    events: AgentEventTx,
    cancel: CancellationToken,
) {
    let (agent_tx, agent_rx) = create_event_channel();
    let (persist_tx, persist_rx): (AgentEventTx, AgentEventRx) = create_event_channel();
    let broadcaster = agent::spawn_broadcaster(agent_rx, vec![events, persist_tx]);
    let persist = thread_persistence::spawn_thread_persist_task(thread, persist_rx);
    let _ = agent::run_turn_with_cancel(
        messages,
        config,
        options,
        system_prompt,
        Some(thread_id),
        agent_tx,
        Some(cancel),
    )
    .await;
    let _ = broadcaster.await;
    if let Err(err) = persist.await {
        tracing::warn!(thread = %thread_id, %err, "Thread persistence task failed");
    }
}

/// The ordinary-thread turn runner: the agent turn the bot runs in a topic
/// (every builtin tool, the Telegram layer when routed), recorded and
/// persisted here like a profile turn.
async fn run_ordinary_turn(request: TurnRequest) {
    let TurnRequest {
        thread_id,
        ordinary: _,
        chat,
        rerun,
        config_root,
        route,
        text,
        images,
        events,
        cancel,
        registry: _,
        steer,
        run_lease,
    } = request;
    let input = (!rerun).then_some((text, images.as_slice()));
    let layer = if route.is_some() {
        OrdinaryLayer::Telegram
    } else if chat {
        OrdinaryLayer::Chat
    } else {
        OrdinaryLayer::Exec
    };
    let prepared = match prepare_ordinary_turn(&thread_id, &config_root, layer, input, run_lease) {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::error!(thread = %thread_id, err = %format!("{err:#}"), "Failed to prepare ordinary turn");
            let _ = events.send(Arc::new(failed(format!("{err:#}"))));
            return;
        }
    };
    // Routed turns are the bot's topics and keep its surface and activity
    // kind, chat turns the terminal chat's; others run as this service's.
    let surface = match layer {
        OrdinaryLayer::Telegram => "telegram",
        OrdinaryLayer::Chat => CHAT_ACTIVITY_KIND,
        OrdinaryLayer::Exec => ACTIVITY_KIND,
    };
    let options = AgentOptions {
        conversation_id: None,
        root: prepared.run_root,
        tool_config: ToolConfig::new(ToolRegistry::builtins(), ToolSelection::default()),
        surface: Some(surface.to_string()),
        text_verbosity: route.map(|_| TextVerbosity::Low),
        service_tier: None,
        activity_kind: Some(surface.to_string()),
        activity_parent_thread_id: None,
        activity_subagent_name: None,
        tool_timeout: None,
        steer: Some(SteerSource::Inbox(steer)),
        run_lease: Some(prepared.run_lease),
    };
    run_and_persist(
        &thread_id,
        prepared.thread,
        prepared.messages,
        &prepared.config,
        &options,
        prepared.system_prompt.as_deref(),
        events,
        cancel,
    )
    .await;
}

struct ReadyOrdinaryTurn {
    run_lease: Arc<ThreadRunLease>,
    config: Config,
    run_root: PathBuf,
    system_prompt: Option<String>,
    thread: Thread,
    messages: Vec<crate::providers::ChatMessage>,
}

/// `input` is the user message to record; `None` reruns the persisted
/// history (the last message stays the one answered).
fn prepare_ordinary_turn(
    thread_id: &str,
    config_root: &Path,
    layer: OrdinaryLayer,
    input: Option<(String, &[InputImage])>,
    run_lease: Option<Arc<ThreadRunLease>>,
) -> Result<ReadyOrdinaryTurn> {
    let run_lease = lease_for_turn(thread_id, run_lease)?;
    let replay = thread_persistence::load_thread_replay(thread_id).context("load thread")?;
    let mut thread = Thread::with_id(thread_id.to_string()).context("open thread")?;
    let mut messages = replay.messages;
    let stored_root = thread_persistence::read_thread_root_path(thread_id)?.map(PathBuf::from);
    let run_root = stored_root
        .clone()
        .unwrap_or_else(|| config_root.to_path_buf());

    let prepared = (|| {
        if let Some(profile) = thread_persistence::read_persistent_profile(thread_id)? {
            bail!(
                "thread '{thread_id}' runs the '{profile}' persistent profile, not as an ordinary thread"
            );
        }
        let mut config = load_config(config_root)?;
        // A thread stored in another project than the submitting root keeps
        // the modes of the root it runs in.
        if stored_root.is_some() {
            config.use_modes_of(config_root, &run_root)?;
        }
        config
            .apply_thread_model_override(
                thread_persistence::read_thread_model_override(thread_id)?.as_deref(),
                thread_persistence::read_thread_thinking_override(thread_id)?,
            )
            .context("This topic's model selection cannot run; pick another with /model")?;
        // Tools get the run's ZDX_* values from their context; never the
        // process environment, which concurrent turns share.
        let _ = crate::skills::ensure_bundled_skills_materialized();
        let prompt = super::prompt::prepare_ordinary_turn(&config, &run_root, layer)?;
        Ok((config, prompt))
    })();
    if stored_root.is_none() {
        thread.set_root_path(&run_root)?;
    }
    // Recorded even when preparation failed, next to the error.
    if let Some((text, images)) = input {
        record_user_input(
            &mut thread,
            &mut messages,
            text,
            images,
            prepared
                .as_ref()
                .ok()
                .and_then(|(_, prompt)| prompt.runtime_context.as_ref()),
            replay.trailing_note.as_deref(),
        )?;
    }
    if let Err(err) = &prepared {
        record_failure(&mut thread, err);
    }
    let (config, prompt) = prepared?;
    Ok(ReadyOrdinaryTurn {
        run_lease,
        config,
        run_root,
        system_prompt: prompt.system_prompt,
        thread,
        messages,
    })
}

/// Saves why a turn that could not start failed, as the agent's own
/// failures are saved (a `turn_failed` notice).
fn record_failure(thread: &mut Thread, err: &anyhow::Error) {
    let notice = thread_persistence::ThreadEvent::turn_failed(&format!("{err:#}"));
    if let Err(write_err) = thread.append(&notice) {
        tracing::warn!(thread = %thread.id, err = %format!("{write_err:#}"), "Failed to save the turn failure");
    }
}

/// The drain's lease for this thread, else a lease of the turn's own: this
/// service's turn is the thread's only writer from before the user message
/// is recorded until the turn is persisted.
fn lease_for_turn(
    thread_id: &str,
    held: Option<Arc<ThreadRunLease>>,
) -> Result<Arc<ThreadRunLease>> {
    match held.filter(|lease| lease.thread_id() == thread_id) {
        Some(lease) => Ok(lease),
        None => Ok(Arc::new(ThreadRunLease::try_acquire(thread_id)?)),
    }
}

struct ReadyTurn {
    run_lease: Arc<ThreadRunLease>,
    config: Config,
    run_root: PathBuf,
    system_prompt: String,
    tools: Vec<String>,
    allowed_subagents: Option<Vec<String>>,
    skill_scope: crate::skills::SkillScope,
    thread: Thread,
    messages: Vec<crate::providers::ChatMessage>,
}

fn prepare_profile_turn(
    thread_id: &str,
    config_root: &Path,
    route: Option<Route>,
    text: String,
    images: &[InputImage],
    run_lease: Option<Arc<ThreadRunLease>>,
) -> Result<ReadyTurn> {
    let run_lease = lease_for_turn(thread_id, run_lease)?;
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
        // Modes mean what the thread's own run root defines, whichever root
        // submitted this turn.
        let mut config = load_config(config_root)?;
        config.use_modes_of(config_root, &run_root)?;
        let config = turn_config(
            config,
            &definition,
            thread_persistence::read_thread_model_override(thread_id)?.as_deref(),
            thread_persistence::read_thread_thinking_override(thread_id)?,
        )?;
        // Tools get the run's ZDX_* values from their context; never the
        // process environment, which concurrent turns share.
        let _ = crate::skills::ensure_bundled_skills_materialized();
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
    if let Err(err) = &prepared {
        record_failure(&mut thread, err);
    }
    let prepared = prepared?;
    Ok(ReadyTurn {
        run_lease,
        config: prepared.config,
        run_root,
        system_prompt: prepared.system_prompt,
        tools: prepared.tools,
        allowed_subagents: prepared.allowed_subagents,
        skill_scope: prepared.skill_scope,
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

    /// A turn that cannot start keeps the message and saves why, after it.
    #[test]
    fn a_turn_that_cannot_start_saves_a_turn_failed_notice() {
        let home = crate::test_support::temp_zdx_home();
        let root = home.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let mut thread = Thread::with_id("broken".to_string()).unwrap();
        thread
            .set_model_override(Some("mode:gone".to_string()))
            .unwrap();
        let err = prepare_ordinary_turn(
            "broken",
            &root,
            OrdinaryLayer::Exec,
            Some(("hello".to_string(), &[])),
            None,
        )
        .err()
        .expect("a mode that does not exist cannot run");
        let events = thread_persistence::Thread::with_id("broken".to_string())
            .unwrap()
            .read_events()
            .unwrap();
        let user = events
            .iter()
            .position(|event| matches!(event, thread_persistence::ThreadEvent::Message { role, .. } if role == "user"))
            .expect("the message is recorded");
        match &events[user + 1..] {
            [thread_persistence::ThreadEvent::Notice { kind, message, .. }] => {
                assert_eq!(*kind, zdx_types::NoticeKind::TurnFailed);
                assert_eq!(*message, format!("{err:#}"));
            }
            other => panic!("expected one notice after the message: {other:?}"),
        }
    }

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
            ordinary: false,
            rerun: false,
            chat: false,
            model: None,
            route,
            text: text.to_string(),
            images: Vec::new(),
            follow: Follow::None,
            delivery: Delivery::Queue,
            replay: false,
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
    async fn ordinary_submits_stay_ordinary_and_never_cross_kinds() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let kinds: Arc<StdMutex<Vec<(String, bool)>>> = Arc::default();
        let recorded = Arc::clone(&kinds);
        let runner: TurnRunner = Arc::new(move |request: TurnRequest| {
            let recorded = Arc::clone(&recorded);
            Box::pin(async move {
                recorded
                    .lock()
                    .unwrap()
                    .push((request.thread_id.clone(), request.ordinary));
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: String::new(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        });
        let orchestrator = Engine::start_with(workers, events, runner);
        let ordinary = |id: &str| Submit {
            ordinary: true,
            ..submit(root.path(), Some(id), "hi", None)
        };

        let mut subscription = orchestrator
            .subscribe(Some("telegram-1-topic-2".to_string()), false, false)
            .unwrap();
        orchestrator.submit(ordinary("telegram-1-topic-2")).unwrap();
        while !matches!(next_event(&mut subscription).await, Event::TurnEnded { .. }) {}
        // Created with its root, never with a profile.
        assert!(
            thread_persistence::read_persistent_profile("telegram-1-topic-2")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            thread_persistence::read_thread_root_path("telegram-1-topic-2").unwrap(),
            Some(root.path().canonicalize().unwrap().display().to_string())
        );
        assert_eq!(
            kinds.lock().unwrap().clone(),
            [("telegram-1-topic-2".to_string(), true)]
        );

        // The other kind is refused both ways.
        let err = orchestrator
            .submit(submit(root.path(), Some("telegram-1-topic-2"), "hi", None))
            .unwrap_err();
        assert!(format!("{err:#}").contains("ordinary"), "{err:#}");
        orchestrator
            .submit(submit(root.path(), Some("home"), "hi", None))
            .unwrap();
        let err = orchestrator.submit(ordinary("home")).unwrap_err();
        assert!(format!("{err:#}").contains("persistent"), "{err:#}");
        let err = orchestrator
            .submit(Submit {
                profile: Some("orchestrator".to_string()),
                ..ordinary("other")
            })
            .unwrap_err();
        assert!(format!("{err:#}").contains("profile"), "{err:#}");

        // A rerun runs the saved history of an existing ordinary thread only.
        let rerun = |id: &str| Submit {
            rerun: true,
            text: String::new(),
            ..ordinary(id)
        };
        orchestrator.submit(rerun("telegram-1-topic-2")).unwrap();
        while !matches!(next_event(&mut subscription).await, Event::TurnEnded { .. }) {}
        assert_eq!(
            kinds.lock().unwrap().last().cloned(),
            Some(("telegram-1-topic-2".to_string(), true))
        );
        let err = orchestrator.submit(rerun("never-written")).unwrap_err();
        assert_eq!(
            super::super::thread_ops::error_code(&err),
            Some(ErrorCode::NotFound)
        );
        let err = orchestrator
            .submit(Submit {
                text: "again".to_string(),
                ..rerun("telegram-1-topic-2")
            })
            .unwrap_err();
        assert_eq!(
            super::super::thread_ops::error_code(&err),
            Some(ErrorCode::Invalid)
        );

        let report = orchestrator.status(None).await;
        assert_eq!(report.protocol, super::super::protocol::PROTOCOL_VERSION);
        assert!(
            report
                .capabilities
                .iter()
                .any(|name| name == super::super::protocol::CAPABILITY_ORDINARY_SUBMIT)
        );
    }

    #[tokio::test]
    async fn chat_submits_reach_the_runner_only_as_unrouted_ordinary_turns() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let chats: Arc<StdMutex<Vec<bool>>> = Arc::default();
        let recorded = Arc::clone(&chats);
        let runner: TurnRunner = Arc::new(move |request: TurnRequest| {
            let recorded = Arc::clone(&recorded);
            Box::pin(async move {
                recorded.lock().unwrap().push(request.chat);
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: String::new(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        });
        let orchestrator = Engine::start_with(workers, events, runner);
        let chat = |id: &str| Submit {
            ordinary: true,
            chat: true,
            ..submit(root.path(), Some(id), "hi", None)
        };
        let invalid = |submit: Submit| {
            let err = orchestrator.submit(submit).unwrap_err();
            assert_eq!(
                super::super::thread_ops::error_code(&err),
                Some(ErrorCode::Invalid)
            );
        };
        invalid(Submit {
            ordinary: false,
            ..chat("chat-a")
        });
        invalid(Submit {
            route: Some(Route {
                chat: 1,
                topic: None,
            }),
            ..chat("chat-a")
        });

        let mut subscription = orchestrator
            .subscribe(Some("chat-a".to_string()), false, false)
            .unwrap();
        orchestrator.submit(chat("chat-a")).unwrap();
        while !matches!(next_event(&mut subscription).await, Event::TurnEnded { .. }) {}
        // The latest submit decides: a plain ordinary submit runs unchatted.
        orchestrator
            .submit(Submit {
                chat: false,
                ..chat("chat-a")
            })
            .unwrap();
        while !matches!(next_event(&mut subscription).await, Event::TurnEnded { .. }) {}
        assert_eq!(*chats.lock().unwrap(), vec![true, false]);

        // A thread a Telegram submit routed refuses chat turns.
        let mut routed = orchestrator
            .subscribe(Some("chat-b".to_string()), false, false)
            .unwrap();
        orchestrator
            .submit(Submit {
                chat: false,
                route: Some(Route {
                    chat: 1,
                    topic: None,
                }),
                ..chat("chat-b")
            })
            .unwrap();
        while !matches!(next_event(&mut routed).await, Event::TurnEnded { .. }) {}
        let err = orchestrator.submit(chat("chat-b")).unwrap_err();
        assert_eq!(
            super::super::thread_ops::error_code(&err),
            Some(ErrorCode::Conflict)
        );
        assert!(
            orchestrator
                .status(None)
                .await
                .capabilities
                .iter()
                .any(|name| name == super::super::protocol::CAPABILITY_CHAT_SUBMIT)
        );
    }

    #[tokio::test]
    async fn a_worker_result_wakes_the_thread_that_queued_it_then_it_settles() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with(workers, events, runner);

        let mut subscription = orchestrator
            .subscribe(Some("issue-1".to_string()), false, false)
            .unwrap();
        let Submitted {
            thread_id,
            queued_ahead: ahead,
            ..
        } = orchestrator
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
        let orchestrator = Engine::start_with(Arc::clone(&workers), events, runner);
        let mut subscription = orchestrator
            .subscribe(Some("batch".to_string()), false, false)
            .unwrap();

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
        let orchestrator = Engine::start_with(workers, events, runner);
        let mut renderer = orchestrator.subscribe(None, true, false).unwrap();
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

        let token = renderer.token().to_string();
        orchestrator
            .release("routed", turn_id, Some(&token), false)
            .unwrap();
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
        let orchestrator = Engine::start_with(workers, events, runner);
        let mut watcher = orchestrator
            .subscribe(Some("late".to_string()), false, false)
            .unwrap();
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });

        orchestrator
            .submit(submit(root.path(), Some("late"), "wait", route))
            .unwrap();
        while !matches!(next_event(&mut watcher).await, Event::TurnStarted { .. }) {}
        let _late_renderer = orchestrator.subscribe(None, true, false).unwrap();
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
        let orchestrator = Engine::start_with(Arc::clone(&workers), events, runner);
        let mut subscription = orchestrator
            .subscribe(Some("first".to_string()), false, false)
            .unwrap();

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
        let orchestrator = Engine::start_with(Arc::clone(&workers), events, runner);
        let mut owner = orchestrator
            .subscribe(Some("owner".to_string()), false, false)
            .unwrap();
        let mut asker = orchestrator
            .subscribe(Some("asker".to_string()), false, false)
            .unwrap();

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
        let orchestrator = Engine::start_with(workers, events, runner);
        let mut follower = orchestrator
            .subscribe(Some("follow".to_string()), false, false)
            .unwrap();
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });

        orchestrator
            .submit_held(
                submit(root.path(), Some("follow"), "first", route),
                Some(follower.holder(false)),
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
        assert_eq!(
            super::super::thread_ops::error_code(
                &orchestrator
                    .release("follow", turn_id, Some("not-the-holder"), false)
                    .unwrap_err()
            ),
            Some(ErrorCode::Conflict)
        );
        assert!(
            orchestrator
                .release("follow", turn_id, None, false)
                .is_err()
        );
        orchestrator
            .release("follow", turn_id, Some(follower.token()), false)
            .unwrap();
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
        let orchestrator = Engine::start_with(workers, events, runner);
        let mut watcher = orchestrator
            .subscribe(Some("gone".to_string()), false, false)
            .unwrap();
        let mut follower = orchestrator
            .subscribe(Some("gone".to_string()), false, false)
            .unwrap();
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });

        orchestrator
            .submit_held(
                submit(root.path(), Some("gone"), "first", route),
                Some(follower.holder(false)),
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

    /// Holds belong to their owner: one renderer at a time, a release or a
    /// delivery record needs the hold's token, and a record marks the turn
    /// started. An owner that goes away mid-delivery leaves its pending
    /// items `unknown` (never resent) and frees the thread.
    #[tokio::test]
    async fn holds_are_bound_to_their_owner_and_interrupted_deliveries_end_unknown() {
        use crate::core::delivery::{DeliveryItem, DeliveryRecord, DeliveryStatus};

        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with(workers, events, runner);
        let renderer = orchestrator.subscribe(None, true, false).unwrap();
        let second = orchestrator.subscribe(None, true, false);
        assert_eq!(
            super::super::thread_ops::error_code(&second.err().unwrap()),
            Some(ErrorCode::Conflict)
        );
        drop(renderer);

        let thread = "bound";
        let mut watcher = orchestrator
            .subscribe(Some(thread.to_string()), false, false)
            .unwrap();
        let follower = orchestrator
            .subscribe(Some(thread.to_string()), false, false)
            .unwrap();
        let route = Some(Route {
            chat: -100,
            topic: Some(7),
        });
        orchestrator
            .submit_held(
                submit(root.path(), Some(thread), "first", route),
                Some(follower.holder(false)),
            )
            .unwrap();
        let turn_id = loop {
            if let Event::TurnEnded { turn_id, held, .. } = next_event(&mut watcher).await {
                assert!(held);
                break turn_id;
            }
        };
        let record =
            DeliveryRecord::new("telegram", Some(-100), Some(7), vec![DeliveryItem::text()]);
        let refused = orchestrator
            .record_delivery(thread.to_string(), record.clone(), Some(turn_id), None)
            .await
            .unwrap_err();
        assert_eq!(
            super::super::thread_ops::error_code(&refused),
            Some(ErrorCode::Conflict)
        );
        orchestrator
            .record_delivery(
                thread.to_string(),
                record.clone(),
                Some(turn_id),
                Some(follower.token().to_string()),
            )
            .await
            .unwrap();
        drop(follower);
        until_settled(&mut watcher).await;
        let events = thread_persistence::load_thread_events(thread).unwrap();
        let records: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                thread_persistence::ThreadEvent::Delivery { record, .. } => Some(record),
                _ => None,
            })
            .collect();
        assert_eq!(records.len(), 2, "pending, then unknown");
        assert_eq!(records[1].id, record.id);
        assert_eq!(records[1].items[0].status, DeliveryStatus::Unknown);
        assert_eq!(
            records[1].items[0].error.as_deref(),
            Some(crate::core::delivery::INTERRUPTED)
        );
    }

    /// A follower that opted in to replay and goes away before it starts
    /// delivering hands its hold to the replay renderer, which gets the
    /// reply alone and owns the hold from then on.
    #[tokio::test]
    async fn an_undelivered_reply_is_replayed_to_the_renderer() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with(workers, events, runner);
        let thread = "replayed";
        let mut watcher = orchestrator
            .subscribe(Some(thread.to_string()), false, false)
            .unwrap();
        let follower = orchestrator
            .subscribe(Some(thread.to_string()), false, false)
            .unwrap();
        let route = Route {
            chat: -100,
            topic: Some(9),
        };
        orchestrator
            .submit_held(
                submit(root.path(), Some(thread), "first", Some(route)),
                Some(follower.holder(true)),
            )
            .unwrap();
        orchestrator
            .submit(submit(root.path(), Some(thread), "second", Some(route)))
            .unwrap();
        let turn_id = loop {
            if let Event::TurnEnded { turn_id, held, .. } = next_event(&mut watcher).await {
                assert!(held);
                break turn_id;
            }
        };
        drop(follower);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(seen.lock().unwrap().len(), 1, "held for a replay renderer");

        let mut renderer = orchestrator.subscribe(None, true, true).unwrap();
        let replay = loop {
            if let replay @ Event::TurnReplay { .. } = next_event(&mut renderer).await {
                break replay;
            }
        };
        let Event::TurnReplay {
            turn_id: replayed,
            route: replay_route,
            status,
            final_text,
            ..
        } = replay
        else {
            unreachable!()
        };
        assert_eq!(replayed, turn_id);
        assert_eq!(replay_route, route);
        assert_eq!(status, ReplayStatus::Completed);
        assert_eq!(final_text, "reply");
        assert!(
            orchestrator
                .release(thread, turn_id, Some("old"), false)
                .is_err()
        );
        let token = renderer.token().to_string();
        orchestrator
            .release(thread, turn_id, Some(&token), false)
            .unwrap();
        loop {
            if let Event::TurnStarted { .. } = next_event(&mut watcher).await {
                break;
            }
        }
    }

    /// The run lease is taken when an input is accepted: a submit on a
    /// thread another run holds is refused at once, and nothing is queued.
    #[tokio::test]
    async fn a_submit_on_a_thread_another_run_holds_is_refused() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with(workers, events, runner);
        let other = ThreadRunLease::try_acquire("leased").unwrap();
        let err = orchestrator
            .submit(submit(root.path(), Some("leased"), "hi", None))
            .unwrap_err();
        assert_eq!(
            super::super::thread_ops::error_code(&err),
            Some(ErrorCode::Conflict)
        );
        drop(other);
        let mut watcher = orchestrator
            .subscribe(Some("leased".to_string()), false, false)
            .unwrap();
        orchestrator
            .submit(submit(root.path(), Some("leased"), "wait", None))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            ThreadRunLease::try_acquire("leased").is_err(),
            "held while the turn runs"
        );
        gate.notify_waiters();
        until_settled(&mut watcher).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            ThreadRunLease::try_acquire("leased").is_ok(),
            "freed when idle"
        );
    }

    /// `turn_ended` names the workers a turn created or prompted, once each,
    /// from successful `Create_Thread` / `Send_Thread_Message` results only.
    #[tokio::test]
    async fn turn_ended_lists_the_workers_the_turn_touched() {
        use crate::core::events::ToolOutput;

        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let runner: TurnRunner = Arc::new(|request: TurnRequest| {
            Box::pin(async move {
                let tool = |id: &str, name: &str, result: ToolOutput| {
                    [
                        Arc::new(AgentEvent::ToolStarted {
                            id: id.to_string(),
                            name: name.to_string(),
                        }),
                        Arc::new(AgentEvent::ToolCompleted {
                            id: id.to_string(),
                            result,
                            duration_ms: None,
                            started_at: None,
                            completed_at: None,
                        }),
                    ]
                };
                let calls = [
                    tool(
                        "a",
                        "Create_Thread",
                        ToolOutput::success(serde_json::json!({"thread_id": "w1"})),
                    ),
                    tool(
                        "b",
                        "send_thread_message",
                        ToolOutput::success(serde_json::json!({"worker": {"thread_id": "w2"}})),
                    ),
                    tool(
                        "c",
                        "Send_Thread_Message",
                        ToolOutput::success(serde_json::json!({"thread_id": "w1"})),
                    ),
                    tool(
                        "d",
                        "Read",
                        ToolOutput::success(serde_json::json!({"thread_id": "not-a-worker"})),
                    ),
                ];
                for event in calls.into_iter().flatten() {
                    let _ = request.events.send(event);
                }
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: "done".to_string(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        });
        let orchestrator = Engine::start_with(workers, events, runner);
        let mut subscription = orchestrator
            .subscribe(Some("touch".to_string()), false, false)
            .unwrap();
        orchestrator
            .submit(submit(root.path(), Some("touch"), "go", None))
            .unwrap();
        let touched = loop {
            if let Event::TurnEnded {
                touched_workers, ..
            } = next_event(&mut subscription).await
            {
                break touched_workers;
            }
        };
        assert_eq!(touched, ["w1", "w2"]);
    }

    #[tokio::test]
    async fn cancel_can_drop_waiting_inputs() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with(workers, events, runner);
        let mut subscription = orchestrator
            .subscribe(Some("cancel".to_string()), false, false)
            .unwrap();

        orchestrator
            .submit(submit(root.path(), Some("cancel"), "wait", None))
            .unwrap();
        let Submitted {
            input_id: dropped_id,
            ..
        } = orchestrator
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

    /// Turn runner with one safe point: records the text, starts workers for
    /// `delegate <name>` lines, waits for `gate` on `wait`, takes the pending
    /// steers (recorded as `steered:<text>`), then on `late` waits for `gate`
    /// again so a steer sent now is left unread.
    fn steering_turns(
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
                    for item in request.steer.take() {
                        seen.lock().unwrap().push(format!("steered:{}", item.text));
                        let _ = request
                            .events
                            .send(Arc::new(AgentEvent::UserMessageInjected {
                                id: item.id,
                                source: item.source,
                                text: item.text,
                            }));
                    }
                }
                if request.text.contains("late") {
                    gate.notified().await;
                }
                request.steer.close();
                let _ = request.events.send(Arc::new(AgentEvent::TurnFinished {
                    status: TurnStatus::Completed,
                    final_text: "reply".to_string(),
                    messages: Vec::new(),
                    prior_message_count: 0,
                }));
            })
        })
    }

    fn steer_submit(root: &Path, thread_id: &str, text: &str) -> Submit {
        Submit {
            delivery: Delivery::Steer,
            ..submit(root, Some(thread_id), text, None)
        }
    }

    async fn queued_inputs(orchestrator: &Arc<Engine>, thread: &str) -> usize {
        orchestrator.status(Some(thread)).await.threads[0].queued_inputs
    }

    #[tokio::test]
    async fn worker_results_and_user_steers_join_the_running_turn_and_late_ones_queue() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = steering_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with_delivery(
            Arc::clone(&workers),
            events,
            runner,
            crate::config::UpdateDelivery::Steer,
        );
        let mut subscription = orchestrator
            .subscribe(Some("steer".to_string()), false, false)
            .unwrap();

        orchestrator
            .submit(submit(
                root.path(),
                Some("steer"),
                "delegate one\nwait\nlate",
                None,
            ))
            .unwrap();
        // The worker's result arrives while the turn runs: steered, not queued.
        while !matches!(
            next_event(&mut subscription).await,
            Event::Worker {
                event: WorkerEvent::Completed(_),
                ..
            }
        ) {}
        assert_eq!(queued_inputs(&orchestrator, "steer").await, 0);
        let detail = orchestrator
            .submit(steer_submit(root.path(), "steer", "more detail"))
            .unwrap();
        assert!(detail.in_turn);
        assert!(!orchestrator.settled(Some("steer")).await);
        gate.notify_one();

        // At the second wait the turn is past its safe point: this one is
        // left unread and runs as the next turn.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while seen.lock().unwrap().len() < 3 {
            assert!(tokio::time::Instant::now() < deadline, "steers never taken");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let late = orchestrator
            .submit(steer_submit(root.path(), "steer", "missed it"))
            .unwrap();
        assert!(late.in_turn);
        gate.notify_one();

        let events = until_settled(&mut subscription).await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 4, "{seen:?}");
        assert!(
            seen[1].starts_with("steered:[worker update]"),
            "{}",
            seen[1]
        );
        assert!(seen[1].contains("done:one"));
        assert_eq!(seen[2], "steered:more detail");
        assert_eq!(seen[3], "missed it");
        let started: Vec<&Vec<InputRef>> = events
            .iter()
            .filter_map(|event| match event {
                Event::TurnStarted { inputs, .. } => Some(inputs),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 1, "only the late steer started a turn");
        assert_eq!(started[0][0].input_id, late.input_id);
    }

    #[tokio::test]
    async fn a_cancelled_turn_gives_back_its_unread_user_steers() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = steering_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with(workers, events, runner);
        let mut subscription = orchestrator
            .subscribe(Some("given".to_string()), false, false)
            .unwrap();

        orchestrator
            .submit(submit(root.path(), Some("given"), "late", None))
            .unwrap();
        while !matches!(
            next_event(&mut subscription).await,
            Event::TurnStarted { .. }
        ) {}
        let steer = orchestrator
            .submit(steer_submit(root.path(), "given", "never read"))
            .unwrap();
        assert!(steer.in_turn);
        let (cancelled, dropped) =
            orchestrator.cancel("given", CancelTarget::Thread { clear_queue: false });
        assert!(cancelled && dropped.is_empty());
        let after = orchestrator
            .submit(steer_submit(root.path(), "given", "after cancel"))
            .unwrap();
        assert!(!after.in_turn, "a cancelled turn takes no steers");
        gate.notify_one();

        let events = until_settled(&mut subscription).await;
        assert!(events.iter().any(|event| matches!(
            event,
            Event::InputsDropped { input_ids, .. } if input_ids == &[steer.input_id]
        )));
        assert_eq!(*seen.lock().unwrap(), ["late", "after cancel"]);
    }

    #[tokio::test]
    async fn submit_refuses_unusable_threads() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let (workers, events) = instant_workers();
        let seen = Seen::default();
        let gate = Arc::new(Notify::new());
        let runner = fake_turns(&workers, root.path().to_path_buf(), &seen, &gate);
        let orchestrator = Engine::start_with(workers, events, runner);

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
