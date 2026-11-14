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

/// Protocol revision this service speaks; [`StatusReport::protocol`].
pub const PROTOCOL_VERSION: u32 = 2;

/// Features a client may rely on, named in [`StatusReport::capabilities`].
/// A name is listed only once its feature ships: an older service ignores
/// unknown fields of a known op, so clients check the name, not the version.
pub const CAPABILITIES: &[&str] = &[
    CAPABILITY_ORDINARY_SUBMIT,
    CAPABILITY_UPDATE_THREAD,
    CAPABILITY_RECORD_DELIVERY,
    CAPABILITY_RESET_THREAD,
    CAPABILITY_RESOLVE_THREAD,
    CAPABILITY_COMPOSE_USER_TEXT,
    CAPABILITY_REPLY_PARTS,
    CAPABILITY_RERUN,
    CAPABILITY_PREPARE_IMAGE,
    CAPABILITY_TRANSCRIBE_AUDIO,
    CAPABILITY_HOLD_TOKENS,
    CAPABILITY_FEEDBACK_SUBMIT,
    CAPABILITY_PREPARE_PHOTO,
    CAPABILITY_TOUCHED_WORKERS,
    CAPABILITY_WORKER_MIRROR_ROUTE,
    CAPABILITY_GENERATE,
    CAPABILITY_BTW_SEED,
    CAPABILITY_TOPIC_START,
    CAPABILITY_CATALOG,
    CAPABILITY_SET_CHAT_MODEL,
];

/// `Submit::ordinary`: ordinary agent threads run in the service.
pub const CAPABILITY_ORDINARY_SUBMIT: &str = "ordinary_submit";
/// [`Request::UpdateThread`].
pub const CAPABILITY_UPDATE_THREAD: &str = "update_thread";
/// [`Request::RecordDelivery`].
pub const CAPABILITY_RECORD_DELIVERY: &str = "record_delivery";
/// [`Request::ResetThread`].
pub const CAPABILITY_RESET_THREAD: &str = "reset_thread";
/// [`Request::ResolveThread`].
pub const CAPABILITY_RESOLVE_THREAD: &str = "resolve_thread";
/// [`Request::ComposeUserText`].
pub const CAPABILITY_COMPOSE_USER_TEXT: &str = "compose_user_text";
/// [`Request::ReplyParts`].
pub const CAPABILITY_REPLY_PARTS: &str = "reply_parts";
/// `Submit::rerun`.
pub const CAPABILITY_RERUN: &str = "rerun";
/// [`Request::PrepareImage`].
pub const CAPABILITY_PREPARE_IMAGE: &str = "prepare_image";
/// [`Request::TranscribeAudio`].
pub const CAPABILITY_TRANSCRIBE_AUDIO: &str = "transcribe_audio";
/// Holds bound to their owner: one renderer at a time, holder tokens on
/// `release` and `record_delivery`, and replay of an undelivered reply.
pub const CAPABILITY_HOLD_TOKENS: &str = "hold_tokens";
/// [`Request::PreparePhoto`].
pub const CAPABILITY_PREPARE_PHOTO: &str = "prepare_photo";
/// `turn_ended.touched_workers`.
pub const CAPABILITY_TOUCHED_WORKERS: &str = "touched_workers";
/// `WorkerSnapshot.mirror`: a worker's legacy mirror topic as
/// `{chat, topic, thread_id}`.
pub const CAPABILITY_WORKER_MIRROR_ROUTE: &str = "worker_mirror_route";
/// [`Request::Generate`].
pub const CAPABILITY_GENERATE: &str = "generate";
/// [`Request::BtwSeed`].
pub const CAPABILITY_BTW_SEED: &str = "btw_seed";
/// [`Request::TopicStart`].
pub const CAPABILITY_TOPIC_START: &str = "topic_start";
/// [`Request::Catalog`].
pub const CAPABILITY_CATALOG: &str = "catalog";
/// [`Request::SetChatModel`].
pub const CAPABILITY_SET_CHAT_MODEL: &str = "set_chat_model";
/// [`Request::FeedbackSubmit`].
pub const CAPABILITY_FEEDBACK_SUBMIT: &str = "feedback_submit";
/// Thread views ([`super::views::ViewRequest`]): list, thread, trajectory,
/// workers. Listed only when the service has a view provider.
pub const CAPABILITY_THREAD_VIEWS: &str = "thread_views";
/// Artifact views: list, resolve, version, canonical image.
pub const CAPABILITY_ARTIFACT_VIEWS: &str = "artifact_views";
/// Monitor views: snapshot, active agents, quotas, workspaces, usage.
pub const CAPABILITY_MONITOR_VIEWS: &str = "monitor_views";
/// Git views: status, scope, diff.
pub const CAPABILITY_GIT_VIEWS: &str = "git_views";
/// The `thread_root` view: a thread's stored root and the views root.
pub const CAPABILITY_THREAD_ROOT: &str = "thread_root";
/// Every view capability, listed together when a provider is installed.
pub const VIEW_CAPABILITIES: &[&str] = &[
    CAPABILITY_THREAD_VIEWS,
    CAPABILITY_ARTIFACT_VIEWS,
    CAPABILITY_MONITOR_VIEWS,
    CAPABILITY_GIT_VIEWS,
    CAPABILITY_THREAD_ROOT,
];

/// One message for a thread the service runs: a persistent-profile thread,
/// or with `ordinary` an ordinary agent thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submit {
    /// Thread to continue or create. `None` creates a new thread.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Project root: config layers load from here, and a new thread runs here.
    pub root: PathBuf,
    /// Profile of a new thread (default `orchestrator`). An existing thread
    /// keeps its own. Must be `None` with `ordinary`.
    #[serde(default)]
    pub profile: Option<String>,
    /// Run an ordinary agent thread (the coding agent the bot runs in a
    /// topic) instead of a persistent profile. The thread stays ordinary: an
    /// existing persistent-profile thread is refused, and a missing one is
    /// created on its first write.
    #[serde(default)]
    pub ordinary: bool,
    /// Run an ordinary thread again on its persisted history without
    /// recording a new user message (a retry after a failed turn). `text`
    /// and `images` must be empty; never steered.
    #[serde(default)]
    pub rerun: bool,
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
    /// A routed follower's holds may be replayed to the renderer when it
    /// goes away before it starts delivering (`turn_replay`).
    #[serde(default)]
    pub replay: bool,
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
    /// The holder finished delivering this turn; the thread may continue.
    /// `holder` must be the hold's token; `force` releases without one.
    Release {
        thread_id: String,
        turn_id: u64,
        #[serde(default)]
        holder: Option<String>,
        #[serde(default)]
        force: bool,
    },
    /// Streams every event. A `renderer` (the bot) holds Telegram-routed
    /// threads after each turn until it releases them; only one renderer may
    /// be subscribed. `replay`: give this renderer the holds whose owner went
    /// away before it started delivering.
    Subscribe {
        #[serde(default)]
        renderer: bool,
        #[serde(default)]
        replay: bool,
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
    /// Changes thread metadata ([`super::thread_ops::update_thread`]).
    UpdateThread(super::thread_ops::UpdateThread),
    /// Appends a delivery record to the thread that holds the turn.
    /// While the thread is held, `turn_id` and `holder` must name the hold.
    RecordDelivery {
        thread_id: String,
        record: crate::core::delivery::DeliveryRecord,
        #[serde(default)]
        turn_id: Option<u64>,
        #[serde(default)]
        holder: Option<String>,
    },
    /// Deletes a thread's history. Refused while it has work here or a turn
    /// running anywhere.
    ResetThread {
        thread_id: String,
    },
    /// Mini App artifact feedback, saved and queued as a turn in the chat
    /// that owns the thread ([`super::feedback`]).
    FeedbackSubmit(super::feedback::FeedbackSubmit),
    /// Generated helper text ([`super::helpers::Generate`]).
    Generate(super::helpers::Generate),
    /// The `/btw` seed for a question about `thread_id`; no model call.
    BtwSeed {
        thread_id: String,
        question: String,
    },
    /// A read-only catalog: picker, launcher, resumable threads, resume
    /// source, thread status, custom commands ([`super::catalog`]).
    Catalog(super::catalog::CatalogQuery),
    /// Stores the chat's default model in the workspace config of `root`.
    SetChatModel {
        root: PathBuf,
        model: String,
    },
    /// The starting model of a seeded topic ([`super::helpers::topic_start`]).
    TopicStart(super::helpers::TopicStartRequest),
    /// Where a chat conversation's messages go.
    ResolveThread {
        chat_id: i64,
        #[serde(default)]
        topic_id: Option<i64>,
    },
    /// The user text a chat turn records for one incoming message.
    ComposeUserText(crate::core::chat_turn::UserInput),
    /// A final answer split into chat text, follow-ups, and media files.
    ReplyParts {
        final_text: String,
    },
    /// An incoming image file made ready for a turn (inline copy or why not).
    PrepareImage {
        path: PathBuf,
    },
    /// A file made ready for `sendPhoto` ([`crate::telegram::photo`]).
    PreparePhoto {
        path: PathBuf,
    },
    /// An incoming audio file transcribed with the transcription config of
    /// `root` (the chat's root). `transcript` is `None` when no provider is
    /// configured.
    TranscribeAudio {
        path: PathBuf,
        file_name: String,
        #[serde(default)]
        mime_type: Option<String>,
        root: PathBuf,
    },
    /// A read-only view ([`super::views::ViewRequest`]); its `op` names it.
    #[serde(untagged)]
    View(super::views::ViewRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Error {
        message: String,
        /// Machine-readable reason ([`ErrorCode`]); absent for an unexpected
        /// failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<ErrorCode>,
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
        /// Token of this follower's holds (a routed follow submit).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder: Option<String>,
    },
    /// A renderer subscription; `holder` is the token of its holds.
    Subscribed {
        holder: String,
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
    ThreadUpdated(super::thread_ops::ThreadUpdated),
    DeliveryRecorded {
        /// The record's problem summary; post it to the chat when the
        /// final record has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_summary: Option<String>,
    },
    ThreadRoute(super::thread_ops::ThreadRoute),
    Text {
        text: String,
    },
    ReplyParts(crate::core::chat_turn::ReplyParts),
    View(super::views::ViewOutput),
    PreparedImage(crate::core::chat_turn::PreparedImage),
    Transcript {
        transcript: Option<String>,
    },
    Feedback(super::feedback::FeedbackReceipt),
    Generated {
        text: String,
    },
    TopicStart(super::helpers::TopicStartReply),
    Catalog {
        body: serde_json::Value,
    },
    ChatModel {
        selection: String,
    },
    /// `data` (base64) is present only when `changed`: otherwise upload the
    /// file as it is.
    PreparedPhoto {
        changed: bool,
        file_name: String,
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
}

/// Why a request failed, for clients that map it (the Mini App maps it to an
/// HTTP status). New codes may appear; treat an unknown one as `internal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request is malformed or names something unusable.
    Invalid,
    /// The thing it names does not exist.
    NotFound,
    /// It conflicts with the current state (e.g. a thread of the other kind).
    Conflict,
    /// The service is busy; retry later.
    Busy,
    /// The caller may not do this.
    Forbidden,
    /// Anything else.
    Internal,
    /// A request body is not JSON (HTTP 400).
    JsonSyntax,
    /// A request body is JSON of the wrong shape (HTTP 422).
    JsonData,
    /// The same submission is being processed now (HTTP 409).
    InFlight,
    /// The artifact changed since the client read it (HTTP 412).
    VersionMismatch,
    /// A submission id was reused for other content (HTTP 409).
    IdConflict,
    /// The thread has no chat to deliver into (HTTP 409).
    NotRoutable,
    /// The artifact does not exist for this thread (HTTP 404).
    ArtifactNotFound,
    /// The artifact cannot be used this way (HTTP 422).
    UnsupportedArtifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    /// [`PROTOCOL_VERSION`] of the service; 1 when it predates the field.
    #[serde(default = "protocol_v1")]
    pub protocol: u32,
    /// [`CAPABILITIES`] of the service; empty when it predates the field.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// No thread has a turn queued, running, or held, no worker is running
    /// or queued, and no worker result is waiting.
    pub settled: bool,
    pub threads: Vec<ThreadStatus>,
    /// The root the service's views read config, usage, and the Git
    /// fallback from (its `--root`), when it serves views. A front end whose
    /// own root differs gets other monitor/usage/Git answers than the Rust
    /// bot gave it: compare before serving them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views_root: Option<std::path::PathBuf>,
}

fn protocol_v1() -> u32 {
    1
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
        /// Workers the turn created or prompted (successful `Create_Thread`
        /// and `Send_Thread_Message` calls), first touch first.
        #[serde(default)]
        touched_workers: Vec<String>,
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
    /// A held turn whose owner went away before it started delivering,
    /// handed to this renderer (sent to it alone). The hold is now the
    /// renderer's: deliver `final_text` to `route`, then release.
    TurnReplay {
        thread_id: String,
        turn_id: u64,
        route: Route,
        status: ReplayStatus,
        final_text: String,
        /// The failure message when `status` is `failed`.
        error: Option<String>,
    },
}

/// How a replayed turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Completed,
    Failed,
    Interrupted,
}

impl Event {
    /// Thread the event belongs to (the owner, for worker events).
    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Self::TurnStarted { thread_id, .. }
            | Self::Agent { thread_id, .. }
            | Self::TurnEnded { thread_id, .. }
            | Self::TurnReplay { thread_id, .. }
            | Self::InputsDropped { thread_id, .. }
            | Self::Settled { thread_id } => Some(thread_id),
            Self::Worker {
                owner_thread_id, ..
            } => owner_thread_id.as_deref(),
        }
    }
}
