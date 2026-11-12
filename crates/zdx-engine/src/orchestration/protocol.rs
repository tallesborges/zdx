//! Wire protocol of the orchestrator service: newline-delimited JSON over a
//! Unix socket at `$ZDX_HOME/run/orchestrator.sock`.
//!
//! A connection carries one [`Request`]. Most requests get one [`Reply`] and
//! the connection closes. `Submit` with a [`Follow`] mode and `Subscribe`
//! keep the connection open and stream [`Reply::Event`] lines.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::updates::InputImage;
use crate::core::events::AgentEvent;
use crate::core::workers::{WorkerEvent, WorkerSnapshot};

/// Socket the service listens on.
#[must_use]
pub fn socket_path() -> PathBuf {
    crate::config::paths::zdx_home()
        .join("run")
        .join("orchestrator.sock")
}

/// Telegram chat and topic a thread answers in. Turns with a route get the
/// Telegram instruction layer, and the bot renders them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub chat: i64,
    pub topic: Option<i64>,
}

/// What a `Submit` connection streams after the reply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Follow {
    /// Reply and close.
    #[default]
    None,
    /// Stream the turn that runs this input, then close.
    Turn,
    /// Stream every turn of the thread from this input's turn until the
    /// thread settles: no turn queued, running, or held, no prompt it queued
    /// waiting or running on a worker, and no worker result waiting.
    Settled,
}

/// How a `Submit` reaches the thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// A new turn after the running and queued ones.
    #[default]
    Queue,
    /// Added to the running turn at its next safe point (after its current
    /// tool calls). With no running turn, a closing one, or images, it is
    /// queued like `Queue`. Admitted into the running turn, the connection
    /// closes after the reply: the running turn has its own renderer.
    Steer,
}

/// One message for a persistent-profile thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submit {
    /// Thread to continue or create. `None` creates a new thread.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Project root: config layers load from here, and a new thread runs here.
    pub root: PathBuf,
    /// Profile of a new thread (default `orchestrator`). An existing thread
    /// keeps its own.
    #[serde(default)]
    pub profile: Option<String>,
    /// Model spec stored on a new thread, so every later turn keeps it.
    #[serde(default)]
    pub model: Option<String>,
    /// Telegram route; sticky per thread until another submit replaces it.
    #[serde(default)]
    pub route: Option<Route>,
    pub text: String,
    #[serde(default)]
    pub images: Vec<InputImage>,
    #[serde(default)]
    pub follow: Follow,
    #[serde(default)]
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Submit(Submit),
    /// One thread, or every thread the service knows plus global settlement.
    Status {
        #[serde(default)]
        thread_id: Option<String>,
    },
    /// Stops work on a thread. With `input_id` or `turn_id`, only that work:
    /// the running turn that holds it, or the waiting input. Without either,
    /// the running turn; `clear_queue` also drops every waiting input.
    Cancel {
        thread_id: String,
        #[serde(default)]
        input_id: Option<u64>,
        #[serde(default)]
        turn_id: Option<u64>,
        #[serde(default)]
        clear_queue: bool,
    },
    /// The renderer finished delivering this turn; the thread may continue.
    Release {
        thread_id: String,
        turn_id: u64,
    },
    /// Streams every event. A `renderer` (the bot) holds Telegram-routed
    /// threads after each turn until it releases them.
    Subscribe {
        #[serde(default)]
        renderer: bool,
    },
    /// Managed workers, all or of one owner.
    Workers {
        #[serde(default)]
        owner_thread_id: Option<String>,
    },
    /// One managed worker.
    Worker {
        thread_id: String,
    },
    CancelWorker {
        thread_id: String,
    },
    /// A message typed in a worker's legacy mirror topic.
    EnqueueFromTopic {
        worker_thread_id: String,
        prompt: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Error {
        message: String,
    },
    Submitted {
        thread_id: String,
        input_id: u64,
        /// Turns and inputs ahead of this one on the thread.
        queued_ahead: usize,
        /// The input joined the running turn as a steer (`Delivery::Steer`).
        /// Its `UserMessageInjected` agent event carries `input_id`; if the
        /// turn ends first it is queued, or reported in `InputsDropped` after
        /// a cancel.
        #[serde(default)]
        in_turn: bool,
    },
    Status {
        report: StatusReport,
    },
    Cancelled {
        thread_id: String,
        cancelled_turn: bool,
        dropped_inputs: Vec<u64>,
    },
    Workers {
        workers: Vec<WorkerSnapshot>,
    },
    Worker {
        worker: Option<WorkerSnapshot>,
    },
    Ok,
    Event {
        event: Event,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    /// No thread has a turn queued, running, or held, no worker is running
    /// or queued, and no worker result is waiting.
    pub settled: bool,
    pub threads: Vec<ThreadStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStatus {
    pub thread_id: String,
    pub profile: Option<String>,
    pub state: ThreadState,
    pub running_turn: Option<u64>,
    pub queued_inputs: usize,
    pub route: Option<Route>,
    /// See [`Follow::Settled`].
    pub settled: bool,
    pub workers: Vec<WorkerSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Idle,
    Queued,
    Running,
    /// A turn ended and its reply is still being delivered.
    Delivering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    /// One message from a client.
    User,
    /// One or more worker results, batched in arrival order.
    WorkerUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputRef {
    pub input_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TurnStarted {
        thread_id: String,
        turn_id: u64,
        kind: TurnKind,
        inputs: Vec<InputRef>,
        route: Option<Route>,
        /// A client follows this turn on its own connection and renders it.
        followed: bool,
    },
    /// One agent event of a turn. `TurnFinished` comes after the turn is on
    /// disk, without its message snapshot.
    Agent {
        thread_id: String,
        turn_id: u64,
        event: AgentEvent,
    },
    /// The turn is over and persisted. `held` means the thread waits for a
    /// `Release` from the renderer before its next turn.
    TurnEnded {
        thread_id: String,
        turn_id: u64,
        held: bool,
    },
    /// Inputs removed without running: waiting inputs dropped by a cancel,
    /// and steers a cancelled turn never read.
    InputsDropped {
        thread_id: String,
        input_ids: Vec<u64>,
    },
    Worker {
        /// Orchestrator that owns the worker (for `Completed`, the one that
        /// queued the finished prompt).
        owner_thread_id: Option<String>,
        /// The owner's Telegram route, when it has one.
        route: Option<Route>,
        event: WorkerEvent,
    },
    /// See [`Follow::Settled`].
    Settled { thread_id: String },
}

impl Event {
    /// Thread the event belongs to (the owner, for worker events).
    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Self::TurnStarted { thread_id, .. }
            | Self::Agent { thread_id, .. }
            | Self::TurnEnded { thread_id, .. }
            | Self::InputsDropped { thread_id, .. }
            | Self::Settled { thread_id } => Some(thread_id),
            Self::Worker {
                owner_thread_id, ..
            } => owner_thread_id.as_deref(),
        }
    }
}
