use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Component, Path as FilePath, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use hmac::{Hmac, Mac};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};
use tower_http::compression::CompressionLayer;
use zdx_engine::agent_activity::RunStatus;
use zdx_engine::config::{Config, paths};
use zdx_engine::core::artifact_feedback::{
    self, FeedbackError, FeedbackSubmission, SaveOutcome, SavedFeedback,
};
use zdx_engine::core::artifacts::{self, Artifact, ArtifactAttachment};
use zdx_engine::core::events::NoticeKind;
use zdx_engine::core::thread_index;
use zdx_engine::core::thread_persistence::{self, ThreadEvent, load_thread_events};
use zdx_engine::core::thread_trajectory::{
    LiveToolSpan, ThreadTrajectoryReport, inspect_thread_trajectory_with_live,
};
use zdx_engine::core::usage_stats::{self, UsageStats};
use zdx_engine::core::workers::{WorkerManager, WorkerSnapshot, WorkerStatus};
use zdx_engine::models::ModelOption;
use zdx_engine::providers::ReplayToken;
use zdx_engine::providers::subscription_quota::{self, QuotaError, SubscriptionQuota};
use zdx_engine::service::{self, Service};
use zdx_engine::workspace_config::{self, WorkspaceReport};
use zdx_engine::{agent_activity, background_activity, jobs};

use crate::artifact_feedback::{
    FeedbackDelivery, FeedbackRoute, FeedbackSink, resolve_destination,
};
use crate::delivery_tracker::ActiveDeliveries;
use crate::types::LocalAttachment;

const INIT_DATA_MAX_AGE_SECS: u64 = 60 * 60;
const INIT_DATA_FUTURE_SKEW_SECS: u64 = 30;
const MONITOR_CACHE_TTL: Duration = Duration::from_secs(30);
const SUBSCRIPTION_CACHE_TTL: Duration = Duration::from_mins(5);
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const GIT_DIFF_LIMIT_BYTES: usize = 256 * 1024;
const GIT_COMMIT_LIMIT: usize = 24;
/// How many recent threads the Mini App browser lists. Applied in SQL.
const THREAD_LIST_LIMIT: usize = 50;
/// An API request slower than this is logged at `info` so it reaches the
/// default log file: this is roughly where a request becomes a visible wait.
const SLOW_REQUEST: Duration = Duration::from_millis(750);

type HmacSha256 = Hmac<Sha256>;
type ApiError = (StatusCode, &'static str);

#[derive(Serialize)]
pub struct ThreadResponse {
    pub id: String,
    pub title: String,
    pub total_messages: usize,
    pub total_events: usize,
    /// `t.me` link to the Telegram topic this thread is bound to, derived from
    /// the *resolved* id so it is present even when the client asked for
    /// `active`. `None` for TUI/CLI threads and plain DMs.
    pub telegram_link: Option<String>,
    /// Cursor to send back as `?after=` on the next poll. Items at or above it
    /// have not been delivered yet.
    pub cursor: usize,
    /// True when `activity` holds only items at or after the requested `after`
    /// cursor, so the client must merge rather than replace.
    pub partial: bool,
    /// Orchestrator that owns this thread as a worker, when it has one. Drives
    /// the breadcrumb back out of a worker thread.
    pub parent_thread_id: Option<String>,
    /// Title of `parent_thread_id`, resolved so the client needs no second call.
    pub parent_title: Option<String>,
    /// Workers owned by this thread, live plus recovered from lineage. Gates
    /// the Workers tab without the client having to fetch the list first.
    pub worker_count: usize,
    /// A reply delivery for this thread is running in this process. Clients
    /// keep polling while it is set, so its pending warning is replaced by the
    /// outcome without a reload. A pending record left by a crash is never
    /// reported as running.
    pub delivering: bool,
    /// Shared live status of a turn running on this thread in any process
    /// (`agent_activity::RunStatus`), or `None` when no turn runs. Clients
    /// keep polling while it is set.
    pub run: Option<RunStatus>,
    pub activity: Vec<ThreadActivity>,
}

#[derive(Serialize)]
pub struct ThreadListResponse {
    pub threads: Vec<ThreadListItem>,
}

#[derive(Serialize)]
pub struct WorkersResponse {
    pub workers: Vec<WorkerItem>,
}

/// Thread artifact tab payload. Rows come from the shared engine artifact
/// model: files in the thread artifact dir plus files the thread sent, merged
/// on the absolute path. `Artifact` is defined there so other surfaces reuse
/// the same shape instead of re-deriving it.
#[derive(Serialize)]
pub struct ArtifactsResponse {
    pub artifacts: Vec<Artifact>,
}

/// `GET /api/threads/{id}/artifacts/version`: the version of one artifact file
/// as it is now. `sha256` is the hash of the file bytes, the same value the
/// Mini App names when it sends feedback, so a viewer can tell the file changed
/// under it without downloading it.
#[derive(Serialize)]
struct ArtifactVersionResponse {
    sha256: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

/// File hashes by path, valid while the file keeps the same length and
/// modified time, so a viewer checking an open artifact every 30 s does not
/// re-read it each time. Bounded: cleared when it grows past
/// `MAX_VERSION_CACHE` paths.
type VersionCache = std::sync::Mutex<HashMap<PathBuf, (u64, SystemTime, String)>>;
const MAX_VERSION_CACHE: usize = 256;

/// Query for the artifact download endpoint: the artifact's `path` as listed.
#[derive(Deserialize)]
struct ArtifactFileQuery {
    path: String,
}

/// One worker of an orchestrator thread.
///
/// Rows come from two sources that carry different amounts of truth. A live row
/// is backed by the in-memory `WorkerManager` and has real status and progress.
/// A row recovered from persisted lineage after a restart knows only that the
/// worker existed; `live` is false and `status` is `unknown` for those, and the
/// progress fields are all `None`. Clients must not render the two the same
/// way — reporting a stale worker as settled would be a lie.
#[derive(Serialize)]
pub struct WorkerItem {
    pub thread_id: String,
    pub title: String,
    /// `queued` | `running` | `completed` | `failed` | `cancelled` | `unknown`.
    pub status: &'static str,
    /// Whether runtime state was available for this worker.
    pub live: bool,
    pub queue_depth: usize,
    pub root_path: Option<String>,
    /// Trailing component of `root_path`, for a compact project label.
    pub project: Option<String>,
    /// Shared live status of the worker's running turn, from its run marker.
    pub run: Option<RunStatus>,
    pub last_error: Option<String>,
    /// Time since the worker thread's last write (`12m`, `3h`), when known.
    pub age: Option<String>,
    /// `t.me` link to the worker's mirror topic, when it has one.
    pub telegram_link: Option<String>,
}

/// Worker counts for one orchestrator, as shown on a thread-list row.
///
/// `failed` is deliberately its own bucket rather than part of `settled`: a
/// failed worker is the one state the reader has to act on, and averaging it
/// into "settled" is how it gets missed.
#[derive(Serialize, Default)]
pub struct WorkerRollup {
    pub total: usize,
    pub running: usize,
    pub queued: usize,
    pub failed: usize,
    /// Completed or cancelled — finished, nothing to do.
    pub settled: usize,
    /// Known to exist from persisted lineage, with no live state to report.
    pub unknown: usize,
}

#[derive(Serialize)]
pub struct ThreadListItem {
    pub id: String,
    pub title: String,
    pub root_path: Option<String>,
    /// Trailing component of `root_path`, for a compact project label.
    pub project: Option<String>,
    /// Time since the last write (`12m`, `3h`, `2d`), or `None` when the
    /// thread's mtime is unknown or in the future.
    pub age: Option<String>,
    /// `t.me` link to the Telegram topic this thread is bound to, when it has
    /// one. `None` for TUI/CLI threads and plain DMs, which have no linkable
    /// topic.
    pub telegram_link: Option<String>,
    /// Worker counts when this thread is an orchestrator, else `None`. Drives
    /// the `orch` badge and the roll-up line on the row.
    pub workers: Option<WorkerRollup>,
    /// Whether this thread is an orchestrator worker (the canonical
    /// `ThreadSummary::is_worker` rule). Drives the `worker` badge.
    pub is_worker: bool,
    /// Shared live status when a turn is running on this thread.
    pub run: Option<RunStatus>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadActivity {
    Message {
        sequence: usize,
        time: String,
        ts: String,
        role: String,
        speaker: &'static str,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        /// Files this message sent (`<media>` paths), resolved through the
        /// shared artifact model so the Mini App can preview them inline.
        /// `None` when the message sent nothing.
        #[serde(skip_serializing_if = "Option::is_none")]
        artifacts: Option<Vec<ArtifactAttachment>>,
    },
    Reasoning {
        sequence: usize,
        time: String,
        ts: String,
        text: String,
        redacted: bool,
    },
    ToolUse {
        sequence: usize,
        time: String,
        ts: String,
        id: String,
        name: String,
        input: Value,
    },
    /// A tool call of the running round (from the run's activity marker), not
    /// yet persisted to the thread. Always the tail of `activity`. A call that
    /// already finished carries `completed_at`/`ok` until its round lands in
    /// the thread file.
    ToolRunning {
        sequence: usize,
        time: String,
        ts: String,
        started_at: String,
        id: String,
        name: String,
        summary: String,
        /// Full tool input, or `Value::Null` when the marker only kept the
        /// summary (oversized input).
        input: Value,
        /// Tail of the tool's streaming output so far (bounded).
        output_tail: String,
        /// Human elapsed time since the tool started, e.g. `42s`; for a
        /// finished call, its duration.
        running_for: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ok: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    ToolResult {
        sequence: usize,
        time: String,
        ts: String,
        tool_use_id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_at: Option<String>,
        output: Value,
    },
    Usage {
        sequence: usize,
        time: String,
        ts: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ttft_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_at: Option<String>,
        /// Context window of the model that served this request, so clients can
        /// show usage as a percentage without their own model registry.
        #[serde(skip_serializing_if = "Option::is_none")]
        context_limit: Option<u64>,
    },
    Notice {
        sequence: usize,
        time: String,
        ts: String,
        kind: &'static str,
        message: String,
    },
    Interrupted {
        sequence: usize,
        time: String,
        ts: String,
        role: String,
        text: String,
    },
}

impl ThreadActivity {
    /// Position of this item in the thread, used as the delta cursor and as the
    /// client-side merge key.
    fn sequence(&self) -> usize {
        match self {
            Self::Message { sequence, .. }
            | Self::Reasoning { sequence, .. }
            | Self::ToolUse { sequence, .. }
            | Self::ToolRunning { sequence, .. }
            | Self::ToolResult { sequence, .. }
            | Self::Usage { sequence, .. }
            | Self::Notice { sequence, .. }
            | Self::Interrupted { sequence, .. } => *sequence,
        }
    }
}

#[derive(Deserialize)]
struct ThreadQuery {
    /// Return only activity at or after this cursor. Live polling sends the
    /// `cursor` from its previous response; a first load omits it.
    after: Option<usize>,
}

/// Which threads `/api/threads` lists.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ThreadListKind {
    /// Every top-level thread, workers included.
    #[default]
    All,
    /// Only orchestrator workers.
    Workers,
}

#[derive(Deserialize, Default)]
struct ThreadListQuery {
    #[serde(default)]
    kind: ThreadListKind,
}

#[derive(Deserialize)]
struct GitQuery {
    thread_id: Option<String>,
}

#[derive(Deserialize)]
struct GitDiffQuery {
    thread_id: Option<String>,
    path: String,
    kind: String,
    /// Commit hash for `kind=commit`. Ignored by the other kinds.
    commit: Option<String>,
}

#[derive(Deserialize)]
struct GitScopeQuery {
    thread_id: Option<String>,
    /// `all` or a commit hash.
    scope: String,
}

#[derive(Serialize)]
struct GitScopeResponse {
    /// `all` or `commit`.
    scope: &'static str,
    /// Resolved base revision (`all`) or the commit itself.
    base: Option<String>,
    /// For `all`: the main branch the base was taken from (`master`,
    /// `origin/main`). `None` for commits, or when no main branch was found.
    base_ref: Option<String>,
    /// For `all`: commits on this branch since the base.
    ahead: usize,
    files: Vec<GitFile>,
    /// Subset of `files` that are untracked, so the client knows to request
    /// those diffs with `kind=untracked`.
    untracked: Vec<String>,
}

#[derive(Serialize)]
struct GitResponse {
    repository: GitRepository,
    worktrees: Vec<GitWorktree>,
    files: GitFiles,
    commits: Vec<GitCommit>,
}

#[derive(Serialize)]
struct GitRepository {
    name: String,
    root: String,
    source: &'static str,
    thread_id: Option<String>,
    branch: String,
    head: Option<String>,
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    detached: bool,
    clean: bool,
}

#[derive(Default, Serialize)]
struct GitFiles {
    staged: Vec<GitFile>,
    unstaged: Vec<GitFile>,
    untracked: Vec<GitFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct GitFile {
    path: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct GitWorktree {
    path: String,
    head: Option<String>,
    branch: Option<String>,
    current: bool,
    flags: Vec<&'static str>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct GitCommit {
    hash: String,
    short_hash: String,
    author: String,
    authored_at: String,
    relative_time: String,
    refs: String,
    subject: String,
}

#[derive(Serialize)]
struct GitDiffResponse {
    path: String,
    kind: &'static str,
    content: String,
    bytes: usize,
    lines: usize,
    truncated: bool,
    limit_bytes: usize,
}

#[derive(Clone, Serialize)]
struct MonitorResponse {
    generated_at: String,
    services: Vec<MonitorService>,
    active_agents: Vec<MonitorAgent>,
    background_processes: Vec<MonitorBackgroundProcess>,
    /// Shared job summaries (`zdx_engine::jobs::summaries`).
    jobs: Vec<jobs::JobSummary>,
    /// Set when jobs could not be listed at all.
    jobs_error: Option<String>,
    config: MonitorConfig,
    usage: Option<MonitorUsage>,
    subscriptions: Vec<MonitorSubscription>,
    /// Freshness of `subscriptions`, which are refreshed off the request path:
    /// `"ready"`, `"pending"` (never fetched since startup, a refresh is in
    /// flight) or `"stale"` (older than the refresh window, refresh in flight).
    /// The client must not read an empty `pending` list as "no subscriptions".
    subscriptions_status: &'static str,
    /// Age of the cached quota snapshot, absent while it is `pending`.
    subscriptions_age: Option<String>,
}

#[derive(Clone, Serialize)]
struct MonitorService {
    name: String,
    running: bool,
    installed: bool,
    pid: Option<u32>,
    uptime: Option<String>,
}

#[derive(Clone, Serialize)]
struct MonitorAgent {
    pid: u32,
    thread_id: Option<String>,
    parent_thread_id: Option<String>,
    surface: Option<String>,
    role: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    account: Option<String>,
    thinking: Option<String>,
    uptime: String,
    /// Shared live status (state, label, activity and heartbeat ages).
    run: RunStatus,
}

#[derive(Clone, Serialize)]
struct MonitorBackgroundProcess {
    id: String,
    pid: u32,
    thread_id: Option<String>,
    command: String,
    uptime: String,
}

#[derive(Clone, Serialize)]
struct MonitorConfig {
    model: String,
    thinking: String,
    max_tokens: Option<u32>,
    subagents_enabled: bool,
    mode_count: usize,
    helper_models: Vec<MonitorConfigModel>,
    server_enabled: bool,
    server_port: u16,
}

#[derive(Clone, Serialize)]
struct MonitorConfigModel {
    role: &'static str,
    model: String,
}

#[derive(Clone, Serialize)]
struct MonitorUsage {
    span: &'static str,
    requests: u64,
    tokens: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    billed_usd: f64,
    subscription_tokens: u64,
    unknown_pricing_rows: u64,
    threads_scanned: usize,
    by_provider: Vec<MonitorUsageRow>,
    by_model: Vec<MonitorUsageRow>,
    daily: Vec<MonitorDailyUsage>,
}

#[derive(Clone, Serialize)]
struct MonitorUsageRow {
    provider: String,
    model: Option<String>,
    requests: u64,
    tokens: u64,
    cost_usd: f64,
    subscription: bool,
    estimated: bool,
}

#[derive(Clone, Serialize)]
struct MonitorDailyUsage {
    day: i32,
    tokens: u64,
}

#[derive(Clone, Serialize)]
struct MonitorSubscription {
    provider: String,
    name: String,
    plan: Option<String>,
    windows: Vec<MonitorQuotaWindow>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct MonitorQuotaWindow {
    label: String,
    used_percent: f64,
    resets_at: Option<String>,
    scope: Option<String>,
}

struct CachedMonitor {
    cached_at: Instant,
    response: MonitorResponse,
}

struct CachedSubscriptions {
    cached_at: Instant,
    subscriptions: Vec<MonitorSubscription>,
}

pub(crate) struct ServerState {
    bot_token: String,
    allowlist_user_ids: HashSet<i64>,
    root: PathBuf,
    /// Live worker state for orchestrator threads. In-memory and
    /// process-scoped: it is empty for workers that ran before a restart, which
    /// is why worker listings merge it with persisted lineage.
    workers: Arc<WorkerManager>,
    monitor_cache: RwLock<Option<CachedMonitor>>,
    /// Single-flight guard for building `monitor_cache`; see `monitor_snapshot`.
    monitor_build: Mutex<()>,
    subscription_cache: Mutex<Option<CachedSubscriptions>>,
    /// Single-flight guard for the background quota refresh, held for the whole
    /// fetch. Every open Mini App polls `/api/monitor` every 30s, so without it
    /// a slow provider would accumulate overlapping refreshes.
    subscription_refresh: Arc<Mutex<()>>,
    /// Reply deliveries running in this process (shared with the reply path).
    deliveries: ActiveDeliveries,
    /// Where artifact feedback goes. `None` refuses feedback submissions.
    feedback: Option<Arc<dyn FeedbackSink>>,
    feedback_in_flight: FeedbackInFlight,
    feedback_permits: Arc<tokio::sync::Semaphore>,
    /// Image previews served at once (`GET /artifacts/image`): each reads and
    /// hashes an original of up to 25 MiB (`canonical::MAX_SOURCE_BYTES`,
    /// checked from the file length first). Making the copy is bounded
    /// separately, process-wide, by the engine
    /// (`canonical::MAX_NORMALIZATIONS`, shared with feedback saves).
    /// Requests wait for a permit instead of failing: a preview is a read.
    image_permits: Arc<tokio::sync::Semaphore>,
    artifact_versions: Arc<VersionCache>,
}

impl ServerState {
    fn new(
        bot_token: String,
        allowlist_user_ids: HashSet<i64>,
        root: PathBuf,
        workers: Arc<WorkerManager>,
    ) -> Self {
        Self {
            bot_token,
            allowlist_user_ids,
            root,
            workers,
            monitor_cache: RwLock::new(None),
            monitor_build: Mutex::new(()),
            subscription_cache: Mutex::new(None),
            subscription_refresh: Arc::new(Mutex::new(())),
            deliveries: ActiveDeliveries::default(),
            feedback: None,
            feedback_in_flight: FeedbackInFlight::default(),
            feedback_permits: Arc::new(tokio::sync::Semaphore::new(FEEDBACK_CONCURRENCY)),
            image_permits: Arc::new(tokio::sync::Semaphore::new(IMAGE_READS)),
            artifact_versions: Arc::new(VersionCache::default()),
        }
    }

    fn with_deliveries(mut self, deliveries: ActiveDeliveries) -> Self {
        self.deliveries = deliveries;
        self
    }

    fn with_feedback(mut self, feedback: Arc<dyn FeedbackSink>) -> Self {
        self.feedback = Some(feedback);
        self
    }
}

#[derive(Deserialize)]
struct InitDataUser {
    id: i64,
}

/// Creates the router for the embedded web server.
pub(crate) fn create_router(state: Arc<ServerState>) -> Router {
    let api = Router::new()
        .route("/threads", get(get_threads))
        .route("/threads/{id}/trajectory", get(get_thread_trajectory))
        .route("/threads/{id}/artifacts", get(get_thread_artifacts))
        .route(
            "/threads/{id}/artifacts/file",
            get(get_thread_artifact_file),
        )
        .route(
            "/threads/{id}/artifacts/version",
            get(get_thread_artifact_version),
        )
        .route(
            "/threads/{id}/artifacts/image",
            get(get_thread_artifact_image),
        )
        .route(
            "/threads/{id}/artifacts/feedback",
            post(post_artifact_feedback)
                .layer(DefaultBodyLimit::max(
                    artifact_feedback::MAX_SUBMISSION_BYTES,
                ))
                .layer(middleware::from_fn_with_state(
                    Arc::clone(&state),
                    limit_feedback,
                )),
        )
        .route("/threads/{id}", get(get_thread))
        .route("/threads/{id}/workers", get(get_thread_workers))
        .route("/monitor", get(get_monitor))
        .route("/workspaces", get(get_workspaces))
        .route("/git", get(get_git))
        .route("/git/scope", get(get_git_scope))
        .route("/git/diff", get(get_git_diff))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorize_api,
        ))
        .layer(middleware::from_fn(log_api_timing));

    Router::new()
        .route("/app", get(serve_app))
        .route("/app/{*path}", get(serve_asset))
        .route("/threads", get(serve_app))
        .route("/monitor", get(serve_app))
        .nest("/api", api)
        // Shell HTML and thread transcripts are highly compressible text fetched
        // over a tunnel from a phone, so compress whatever the client accepts.
        // The default predicate already skips tiny bodies and images.
        .layer(CompressionLayer::new())
        .with_state(state)
}

/// The built Svelte Mini App (`apps/web/dist`), embedded so the binary stays
/// self-contained. Built by `just web-build`; a missing folder is a compile
/// error rather than a silently empty UI.
#[derive(RustEmbed)]
#[folder = "../../apps/web/dist"]
struct Assets;

/// Serves the SPA entry point. Never cached, so a new build's content-hashed
/// asset names are picked up on the next open.
async fn serve_app() -> Response {
    match Assets::get("index.html") {
        Some(file) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Mini App assets are missing; run `just web-build`",
        )
            .into_response(),
    }
}

/// Serves a content-hashed asset. Vite renames these on every build, so they are
/// safe to cache permanently — the only caching layer Telegram gives us, since
/// service workers do not work in the iOS `WebView`.
async fn serve_asset(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_owned();
            (
                [
                    (header::CONTENT_TYPE, mime.as_str()),
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

/// The Telegram user `authorize_api` verified, for handlers that act on it.
/// Read from request extensions so a long upload is not re-checked against
/// `initData` that expired while the body was arriving.
#[derive(Clone, Copy)]
struct AuthorizedUser(i64);

async fn authorize_api(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let user_id = authorize(&headers, &state)?;
    request.extensions_mut().insert(AuthorizedUser(user_id));
    Ok(next.run(request).await)
}

/// Times every `/api/*` request, including authorization.
///
/// The Mini App renders its shell from embedded assets, so a visible "loading"
/// is always one of these requests still in flight. Without this line a stall
/// reported from a phone cannot be attributed to a route at all. Slow requests
/// are logged at `info` so they land in the default log file; the rest are
/// `debug` because an open thread view polls every few seconds.
async fn log_api_timing(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let status = response.status().as_u16();
    if elapsed >= SLOW_REQUEST {
        tracing::info!(%method, path, status, elapsed_ms, "Slow Mini App API request");
    } else {
        tracing::debug!(%method, path, status, elapsed_ms, "Mini App API request");
    }
    response
}

/// Builds a `t.me/c/<internal_id>/<topic_id>` link for a Telegram topic thread.
///
/// Thread ids are minted as `telegram-{chat_id}-topic-{topic_id}`; the link
/// form itself is [`crate::telegram::topic_link`]. Threads with no topic (plain
/// DMs) and non-Telegram threads have no direct linkable target.
fn telegram_topic_link(thread_id: &str) -> Option<String> {
    let (chat_id, topic_id) = crate::handlers::message::parse_topic_thread_id(thread_id)?;
    crate::telegram::topic_link(chat_id, topic_id)
}

/// Resolves a Telegram topic link for either a direct Telegram thread or a
/// worker thread linked to a mirror topic.
///
/// The worker→mirror lookup is answered by the thread index from an indexed
/// column, so a thread that is not a Telegram topic costs one SQL row read
/// instead of opening every saved thread file.
fn resolve_telegram_link(thread_id: &str) -> Option<String> {
    if let Some(link) = telegram_topic_link(thread_id) {
        return Some(link);
    }
    let mirror_id = thread_index::mirror_thread_id_for_worker(thread_id)
        .inspect_err(|error| {
            tracing::warn!(thread_id, error = %format!("{error:#}"), "Failed to resolve worker mirror topic");
        })
        .ok()
        .flatten()?;
    telegram_topic_link(&mirror_id)
}

/// Lists the most recently active top-level threads for the Mini App browser,
/// or only orchestrator workers with `?kind=workers`.
async fn get_threads(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ThreadListQuery>,
) -> Result<Json<ThreadListResponse>, ApiError> {
    let manager = Arc::clone(&state.workers);
    tokio::task::spawn_blocking(move || {
        let now = SystemTime::now();
        let summaries = match query.kind {
            ThreadListKind::All => thread_persistence::list_recent_threads(THREAD_LIST_LIMIT),
            ThreadListKind::Workers => thread_persistence::list_recent_workers(THREAD_LIST_LIMIT),
        }
        .map_err(|error| {
            tracing::warn!(error = %format!("{error:#}"), "Failed to list Mini App threads");
            (StatusCode::INTERNAL_SERVER_ERROR, "Thread list failed")
        })?;

        // One indexed lookup for the page being rendered, not a corpus scan:
        // the id list is bounded by THREAD_LIST_LIMIT.
        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        let persisted_counts =
            thread_index::worker_counts_for_parents(&ids).unwrap_or_else(|error| {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "Failed to count workers for the Mini App thread list"
                );
                HashMap::new()
            });
        let live_statuses = manager.statuses_by_owner();
        let runs = agent_activity::list_active();
        let run_now = chrono::Utc::now();

        let threads = summaries
            .into_iter()
            .map(|summary| ThreadListItem {
                run: agent_activity::run_for_thread(&runs, &summary.id)
                    .map(|run| run.status(run_now)),
                is_worker: summary.is_worker(),
                workers: worker_rollup(
                    live_statuses.get(&summary.id).map(Vec::as_slice),
                    persisted_counts.get(&summary.id).copied().unwrap_or(0),
                ),
                project: summary.root_path.as_deref().and_then(|root| {
                    FilePath::new(root)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                }),
                age: summary
                    .modified
                    .and_then(|modified| now.duration_since(modified).ok())
                    .map(service::format_uptime),
                telegram_link: resolve_telegram_link(&summary.id),
                title: summary
                    .title
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| summary.id.clone()),
                root_path: summary.root_path,
                id: summary.id,
            })
            .collect();
        Ok(Json(ThreadListResponse { threads }))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App thread list task failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Thread list failed")
    })?
}

/// Combines live worker statuses with the persisted child count for one thread.
///
/// `persisted` can lag a just-created worker (the index syncs on a freshness
/// window), so the total is whichever source knows about more workers, and
/// `unknown` is only what live state cannot account for.
fn worker_rollup(live: Option<&[WorkerStatus]>, persisted: usize) -> Option<WorkerRollup> {
    let live = live.unwrap_or(&[]);
    if live.is_empty() && persisted == 0 {
        return None;
    }

    let mut rollup = WorkerRollup {
        total: live.len().max(persisted),
        ..WorkerRollup::default()
    };
    for status in live {
        match status {
            WorkerStatus::Running => rollup.running += 1,
            WorkerStatus::Queued => rollup.queued += 1,
            WorkerStatus::Failed => rollup.failed += 1,
            WorkerStatus::Completed | WorkerStatus::Cancelled => rollup.settled += 1,
        }
    }
    rollup.unknown = rollup.total.saturating_sub(live.len());
    Some(rollup)
}

async fn get_thread(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<ThreadQuery>,
) -> Result<Json<ThreadResponse>, ApiError> {
    let workers = Arc::clone(&state.workers);
    let deliveries = state.deliveries.clone();
    tokio::task::spawn_blocking(move || {
        let target_id = if id == "active" {
            thread_index::latest_thread_id_with_prefix("telegram-")
                .ok()
                .flatten()
                .unwrap_or(id)
        } else {
            id
        };
        if !valid_thread_id(&target_id) {
            return Err((StatusCode::BAD_REQUEST, "Invalid thread ID"));
        }

        // A resumed or mirror topic is an alias: its events, and the reply
        // deliveries the bot tracks, live under the source thread. The
        // response keeps the requested id and its topic link.
        let source_id = resolve_alias_id(&target_id);
        // Sampled around the read so the flag can never be false while the
        // events still show a pending record.
        let (events, delivering) =
            deliveries.observe(&source_id, || load_thread_events(&source_id));
        let events = events.map_err(|error| {
            tracing::warn!(thread_id = source_id, error = %format!("{error:#}"), "Failed to load Mini App thread");
            (StatusCode::NOT_FOUND, "Thread not found")
        })?;

        let telegram_link = resolve_telegram_link(&target_id);
        let lineage = resolve_lineage(&workers, &target_id);

        let events_for_cursor = delivery_ids(&events);
        let mut response = project_thread(target_id, events, telegram_link, lineage);
        response.delivering = delivering;
        attach_live_run(&mut response, &source_id);

        // Live polling asks only for what it has not seen. Dropping delivered
        // items keeps a steady-state poll small on a thread whose transcript is
        // hundreds of kilobytes, without changing what a first load returns.
        // A delivery outcome that replaces a record the client already holds
        // (pending → sent/failed) needs the full projection instead, since a
        // delta cannot remove the stale warning.
        if let Some(after) = query.after
            && !replaces_seen_delivery(&events_for_cursor, after)
        {
            response.activity.retain(|item| item.sequence() >= after);
            response.partial = true;
        }
        Ok(Json(response))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App thread load task failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Thread load failed")
    })?
}

/// `(sequence, record id)` of every delivery event, for the poll cursor check.
fn delivery_ids(events: &[ThreadEvent]) -> Vec<(usize, String)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(sequence, event)| match event {
            ThreadEvent::Delivery { record, .. } => Some((sequence, record.id.clone())),
            _ => None,
        })
        .collect()
}

/// Whether a delivery event at or after `after` replaces one before it.
fn replaces_seen_delivery(deliveries: &[(usize, String)], after: usize) -> bool {
    deliveries.iter().any(|(sequence, id)| {
        *sequence >= after
            && deliveries
                .iter()
                .any(|(earlier, other)| *earlier < after && other == id)
    })
}

async fn get_thread_trajectory(
    State(_state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<ThreadTrajectoryReport>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let target_id = if id == "active" {
            thread_index::latest_thread_id_with_prefix("telegram-")
                .ok()
                .flatten()
                .unwrap_or(id)
        } else {
            id
        };
        if !valid_thread_id(&target_id) {
            return Err((StatusCode::BAD_REQUEST, "Invalid thread ID"));
        }

        let events = load_thread_events(&target_id).map_err(|error| {
            tracing::warn!(
                thread_id = target_id,
                error = %format!("{error:#}"),
                "Failed to load Mini App thread trajectory"
            );
            (StatusCode::NOT_FOUND, "Thread not found")
        })?;
        let live_tools = active_trajectory_tools(&target_id, events.len());
        Ok(Json(inspect_thread_trajectory_with_live(
            &events,
            &live_tools,
            chrono::Utc::now(),
        )))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App thread trajectory task failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Thread trajectory failed",
        )
    })?
}

fn active_trajectory_tools(thread_id: &str, first_sequence: usize) -> Vec<LiveToolSpan> {
    let Some(record) = agent_activity::active_run_for_thread(thread_id) else {
        return Vec::new();
    };
    // Trajectory spans of finished calls come from the thread file; only
    // calls still running are live spans.
    record
        .current_tools
        .into_iter()
        .filter(agent_activity::ActiveToolCall::is_running)
        .enumerate()
        .map(|(index, tool)| LiveToolSpan {
            sequence: first_sequence + index,
            id: tool.id,
            name: tool.name,
            summary: tool.summary,
            input: tool.input,
            output_tail: tool.output_tail,
            started_at: tool.started_at,
        })
        .collect()
}

/// Follows a single `alias_to` hop, so an id taken from a Telegram worker
/// mirror topic resolves to the worker thread it mirrors.
///
/// Mini App deep links are built from the *topic session* id
/// (`?startapp={thread_id}` in the pinned header), and a worker mirror topic is
/// an alias whose own thread file carries neither lineage nor workers. Without
/// this hop, opening a worker from its mirror topic looks like a thread with no
/// orchestrator and no workers. The Git route already resolves the same way.
fn resolve_alias_id(id: &str) -> String {
    thread_persistence::read_thread_alias(id)
        .ok()
        .flatten()
        .filter(|alias| valid_thread_id(alias))
        .unwrap_or_else(|| id.to_string())
}

/// Lineage and worker counts attached to a single thread response.
#[derive(Default)]
struct ThreadLineage {
    parent_thread_id: Option<String>,
    parent_title: Option<String>,
    worker_count: usize,
}

/// Resolves the orchestrator a thread belongs to and how many workers it owns.
///
/// Both halves are indexed lookups: the parent comes off the thread's own
/// cached summary, and the worker count is the same union the Workers pane
/// renders, so the tab never appears for a thread with nothing behind it.
fn resolve_lineage(workers: &WorkerManager, thread_id: &str) -> ThreadLineage {
    let effective_id = resolve_alias_id(thread_id);
    let parent_thread_id = thread_persistence::read_thread_summary_cached(&effective_id)
        .ok()
        .flatten()
        .and_then(|summary| summary.parent_thread_id);
    let parent_title = parent_thread_id.as_deref().map(thread_display_title);

    ThreadLineage {
        parent_thread_id,
        parent_title,
        worker_count: collect_workers(workers, &effective_id).len(),
    }
}

/// Best-effort display title for a thread id, falling back to a short id.
fn thread_display_title(thread_id: &str) -> String {
    thread_persistence::read_thread_title(thread_id)
        .ok()
        .flatten()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| thread_id.chars().take(8).collect())
}

/// Ordering rank so active work sorts above anything settled or stale.
fn status_rank(status: &str) -> u8 {
    match status {
        "running" => 0,
        "queued" => 1,
        "failed" => 2,
        "completed" => 3,
        "cancelled" => 4,
        _ => 5,
    }
}

/// Builds the worker list for an orchestrator thread.
///
/// Two sources, deliberately not equivalent:
///
/// - `WorkerManager` holds live state, but only for this process. Everything it
///   knows disappears on restart.
/// - `thread_meta.parent_thread_id` survives restarts, but records only that a
///   worker existed. It restores *discovery*, never status.
///
/// Persisted children are filtered by the canonical worker rule
/// (`ThreadSummary::is_worker`), because the same parent link is used by
/// subagent and helper runs, which are not workers and must not show up here.
fn collect_workers(workers: &WorkerManager, owner_thread_id: &str) -> Vec<WorkerItem> {
    let now = SystemTime::now();
    let live = workers.list_for_owner(owner_thread_id);
    let live_ids: HashSet<String> = live.iter().map(|w| w.thread_id.clone()).collect();

    let runs = agent_activity::list_active();
    let mut items: Vec<WorkerItem> = live
        .into_iter()
        .map(|worker| live_worker_item(worker, &runs))
        .collect();

    let persisted = thread_index::child_runs_cached(owner_thread_id).unwrap_or_else(|error| {
        tracing::warn!(
            owner = owner_thread_id,
            error = %format!("{error:#}"),
            "Failed to read persisted worker lineage"
        );
        Vec::new()
    });
    for summary in persisted {
        if !summary.is_worker() || live_ids.contains(&summary.id) {
            continue;
        }
        items.push(WorkerItem {
            title: summary
                .title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| summary.id.chars().take(8).collect()),
            status: "unknown",
            live: false,
            queue_depth: 0,
            project: summary.root_path.as_deref().and_then(|root| {
                FilePath::new(root)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            }),
            root_path: summary.root_path,
            run: None,
            last_error: None,
            age: summary
                .modified
                .and_then(|modified| now.duration_since(modified).ok())
                .map(service::format_uptime),
            telegram_link: resolve_telegram_link(&summary.id),
            thread_id: summary.id,
        });
    }

    items.sort_by(|a, b| {
        status_rank(a.status)
            .cmp(&status_rank(b.status))
            .then_with(|| a.title.cmp(&b.title))
    });
    items
}

fn live_worker_item(worker: WorkerSnapshot, runs: &[agent_activity::RunRecord]) -> WorkerItem {
    let root_path = worker.root.to_string_lossy().into_owned();
    let run = agent_activity::run_for_thread(runs, &worker.thread_id)
        .map(|run| run.status(chrono::Utc::now()));
    WorkerItem {
        title: thread_display_title(&worker.thread_id),
        status: match worker.status {
            WorkerStatus::Queued => "queued",
            WorkerStatus::Running => "running",
            WorkerStatus::Completed => "completed",
            WorkerStatus::Failed => "failed",
            WorkerStatus::Cancelled => "cancelled",
        },
        live: true,
        queue_depth: worker.queue_depth,
        project: worker
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        root_path: Some(root_path),
        run,
        last_error: worker.last_error,
        age: None,
        telegram_link: worker
            .mirror_url
            .or_else(|| resolve_telegram_link(&worker.thread_id)),
        thread_id: worker.thread_id,
    }
}

async fn get_thread_workers(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<WorkersResponse>, ApiError> {
    let workers = Arc::clone(&state.workers);
    tokio::task::spawn_blocking(move || {
        let target_id = if id == "active" {
            thread_index::latest_thread_id_with_prefix("telegram-")
                .ok()
                .flatten()
                .unwrap_or(id)
        } else {
            id
        };
        if !valid_thread_id(&target_id) {
            return Err((StatusCode::BAD_REQUEST, "Invalid thread ID"));
        }
        Ok(Json(WorkersResponse {
            workers: collect_workers(&workers, &resolve_alias_id(&target_id)),
        }))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App worker list task failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Worker list failed")
    })?
}

/// Resolves a thread path param the way `get_thread` does: `active` means the
/// most recent Telegram thread. Callers still apply `resolve_alias_id` when
/// they need the effective thread behind a mirror alias.
fn resolve_thread_param(id: String) -> String {
    if id == "active" {
        thread_index::latest_thread_id_with_prefix("telegram-")
            .ok()
            .flatten()
            .unwrap_or(id)
    } else {
        id
    }
}

async fn get_thread_artifacts(Path(id): Path<String>) -> Result<Json<ArtifactsResponse>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let target_id = resolve_thread_param(id);
        if !valid_thread_id(&target_id) {
            return Err((StatusCode::BAD_REQUEST, "Invalid thread ID"));
        }
        let effective = resolve_alias_id(&target_id);
        let artifacts = artifacts::list_thread_artifacts(&effective).map_err(|error| {
            tracing::warn!(thread_id = effective, error = %format!("{error:#}"), "Failed to list Mini App artifacts");
            (StatusCode::INTERNAL_SERVER_ERROR, "Artifact list failed")
        })?;
        Ok(Json(ArtifactsResponse { artifacts }))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App artifact list task failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Artifact list failed")
    })?
}

async fn get_thread_artifact_file(
    Path(id): Path<String>,
    Query(query): Query<ArtifactFileQuery>,
) -> Result<Response, ApiError> {
    let target_id = resolve_thread_param(id);
    if !valid_thread_id(&target_id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid thread ID"));
    }
    if query.path.trim().is_empty() || query.path.contains('\0') {
        return Err((StatusCode::BAD_REQUEST, "Invalid artifact path"));
    }
    let effective = resolve_alias_id(&target_id);
    let path_param = query.path.clone();
    let (bytes, mime, filename) = tokio::task::spawn_blocking(move || {
        let (path, mime) =
            artifacts::resolve_artifact_for_download(&effective, &path_param).map_err(
                |error| {
                    tracing::warn!(thread_id = effective, error = %format!("{error:#}"), "Failed to resolve Mini App artifact");
                    (StatusCode::NOT_FOUND, "Artifact not found")
                },
            )?;
        let bytes = std::fs::read(&path).map_err(|error| {
            tracing::warn!(path = %path.display(), error = %format!("{error:#}"), "Failed to read Mini App artifact");
            (StatusCode::NOT_FOUND, "Artifact not found")
        })?;
        let filename = path.file_name().map_or_else(
            || "artifact".to_string(),
            |name| name.to_string_lossy().replace('"', "_"),
        );
        Ok::<_, ApiError>((bytes, mime, filename))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App artifact read task failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Artifact read failed")
    })??;

    let body = axum::body::Body::from(bytes);
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .map_err(|_build| (StatusCode::INTERNAL_SERVER_ERROR, "Artifact read failed"))
}

async fn get_thread_artifact_version(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<ArtifactFileQuery>,
) -> Result<Json<ArtifactVersionResponse>, ApiError> {
    let target_id = resolve_thread_param(id);
    if !valid_thread_id(&target_id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid thread ID"));
    }
    if query.path.trim().is_empty() || query.path.contains('\0') {
        return Err((StatusCode::BAD_REQUEST, "Invalid artifact path"));
    }
    let effective = resolve_alias_id(&target_id);
    let cache = Arc::clone(&state.artifact_versions);
    tokio::task::spawn_blocking(move || {
        let (path, _mime) = artifacts::resolve_artifact_for_download(&effective, &query.path)
            .map_err(|error| {
                tracing::warn!(thread_id = effective, error = %format!("{error:#}"), "Failed to resolve Mini App artifact version");
                (StatusCode::NOT_FOUND, "Artifact not found")
            })?;
        let not_found = |error: std::io::Error| {
            tracing::warn!(path = %path.display(), %error, "Failed to read Mini App artifact version");
            (StatusCode::NOT_FOUND, "Artifact not found")
        };
        let stamp = |meta: &std::fs::Metadata| (meta.len(), meta.modified().unwrap_or(UNIX_EPOCH));
        let before = stamp(&std::fs::metadata(&path).map_err(not_found)?);
        let cached = cache
            .lock()
            .expect("version cache lock poisoned")
            .get(&path)
            .filter(|(len, modified, _)| (*len, *modified) == before)
            .map(|(_, _, sha)| sha.clone());
        let sha256 = if let Some(sha) = cached {
            sha
        } else {
            let sha = artifacts::file_sha256(&path).map_err(not_found)?;
            // Cached only when the file did not change while it was read;
            // a mixed read can only cause one extra "new version" notice,
            // because the viewer hashes the bytes it loads on Update.
            if std::fs::metadata(&path).is_ok_and(|meta| stamp(&meta) == before) {
                let mut cache = cache.lock().expect("version cache lock poisoned");
                if cache.len() >= MAX_VERSION_CACHE {
                    cache.clear();
                }
                cache.insert(path.clone(), (before.0, before.1, sha.clone()));
            }
            sha
        };
        let modified_at = (before.1 != UNIX_EPOCH)
            .then(|| chrono::DateTime::<chrono::Utc>::from(before.1).to_rfc3339());
        Ok(Json(ArtifactVersionResponse {
            sha256,
            size_bytes: before.0,
            modified_at,
        }))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App artifact version task failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Artifact version failed")
    })?
}

/// Image previews read at once (see `ServerState::image_permits`).
const IMAGE_READS: usize = 2;

/// The canonical copy of an image artifact (`zdx_engine::core::
/// artifact_feedback::canonical`): small, upright, made once and cached. The
/// Mini App previews and annotates exactly these bytes and sends back their
/// hash, so marks refer to the pixels the agent gets. The original is served
/// unchanged by `/artifacts/file`. Identity travels in headers:
/// `X-Zdx-Source-Sha256` (the artifact version), `X-Zdx-Canonical-Sha256`,
/// `X-Zdx-Canonical-Width` and `-Height`, and the original's upright size in
/// `X-Zdx-Source-Width` and `-Height` (so the app can say it shows an
/// optimized copy). `422` with a reason when the image
/// cannot be annotated (format or limits); the app then shows the original.
async fn get_thread_artifact_image(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<ArtifactFileQuery>,
) -> Response {
    let target_id = resolve_thread_param(id);
    if !valid_thread_id(&target_id) {
        return (StatusCode::BAD_REQUEST, "Invalid thread ID").into_response();
    }
    if query.path.trim().is_empty() || query.path.contains('\0') {
        return (StatusCode::BAD_REQUEST, "Invalid artifact path").into_response();
    }
    let effective = resolve_alias_id(&target_id);
    let Ok(permit) = Arc::clone(&state.image_permits).acquire_owned().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Image previews are unavailable",
        )
            .into_response();
    };
    // The permit lives in the blocking task: a client that leaves early does
    // not free it while the read runs. The engine's normalization slot is
    // taken inside, after the cache check, and held until the decode and
    // shrink finish.
    let made = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        artifact_feedback::canonical::canonical_for_artifact(&effective, &query.path)
    })
    .await;
    let image = match made {
        Ok(Ok(image)) => image,
        Ok(Err(FeedbackError::ArtifactNotFound)) => {
            return (StatusCode::NOT_FOUND, "Artifact not found").into_response();
        }
        Ok(Err(FeedbackError::UnsupportedArtifact(kind))) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Annotation works on PNG, JPEG, and WebP images, not {kind} files."),
            )
                .into_response();
        }
        Ok(Err(FeedbackError::Invalid(message))) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, message).into_response();
        }
        Ok(Err(error @ FeedbackError::Busy)) => {
            return (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response();
        }
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "Failed to make an image copy");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Image preview failed").into_response();
        }
        Err(error) => {
            tracing::warn!(%error, "Image copy task failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Image preview failed").into_response();
        }
    };
    let meta = image.meta;
    Response::builder()
        .header(header::CONTENT_TYPE, meta.mime.as_str())
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("x-zdx-source-sha256", meta.source_sha256.as_str())
        .header("x-zdx-canonical-sha256", meta.sha256.as_str())
        .header("x-zdx-canonical-width", meta.width.to_string())
        .header("x-zdx-canonical-height", meta.height.to_string())
        .header("x-zdx-source-width", meta.source_upright().0.to_string())
        .header("x-zdx-source-height", meta.source_upright().1.to_string())
        .body(axum::body::Body::from(image.bytes))
        .unwrap_or_else(|_build| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Image preview failed").into_response()
        })
}

#[derive(Serialize)]
struct FeedbackResponse {
    /// `queued`: on the chat's queue. `duplicate`: this submission was already
    /// queued, so nothing was sent again. The queue is in memory: neither means
    /// a model has read it, and a restart before the turn starts loses it.
    status: &'static str,
    annotations: usize,
    /// Sent although the file changed after annotation (`allow_stale`).
    stale: bool,
}

type FeedbackFailure = (StatusCode, String);

/// Submissions being saved or delivered right now, keyed by effective thread
/// and `submission_id`. One bot process serves the API, so this is the only
/// place two requests for the same submission can meet.
#[derive(Default)]
struct FeedbackInFlight(std::sync::Mutex<HashSet<String>>);

struct InFlightGuard {
    state: Arc<ServerState>,
    key: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut keys) = self.state.feedback_in_flight.0.lock() {
            keys.remove(&self.key);
        }
    }
}

/// Feedback submissions decoded at once. A submission can be tens of MiB of
/// base64, so the cap holds before the body is read.
const FEEDBACK_CONCURRENCY: usize = 2;

/// The upload permit, handed from [`limit_feedback`] to the handler. The
/// handler moves it into the blocking save (decode, render, write), so a
/// client that disconnects mid-save does not free the slot while that work
/// still runs. Until then the request owns it, so a body that never finishes
/// arriving frees it when the request is dropped.
#[derive(Clone)]
struct FeedbackPermit(Arc<std::sync::Mutex<Option<tokio::sync::OwnedSemaphorePermit>>>);

impl FeedbackPermit {
    fn take(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }
}

async fn limit_feedback(
    State(state): State<Arc<ServerState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Ok(permit) = Arc::clone(&state.feedback_permits).try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Another feedback upload is in progress. Try again in a moment.",
        )
            .into_response();
    };
    request
        .extensions_mut()
        .insert(FeedbackPermit(Arc::new(std::sync::Mutex::new(Some(
            permit,
        )))));
    next.run(request).await
}

/// Accepts annotations on a thread artifact and queues them as a turn in the
/// chat that owns the thread.
///
/// Ordering is what keeps this safe: the route and the sender are checked
/// before anything is saved, the engine validates the payload and the artifact
/// version before writing, and a bundle counts as sent only once the sink
/// marked it queued, so a retry after a lost response is answered `duplicate`
/// while a retry after a failure queues it. Delivery runs on its own task: a
/// client that disconnects mid-request cannot cancel it halfway through.
async fn post_artifact_feedback(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    axum::Extension(AuthorizedUser(user_id)): axum::Extension<AuthorizedUser>,
    axum::Extension(permit): axum::Extension<FeedbackPermit>,
    Json(submission): Json<FeedbackSubmission>,
) -> Response {
    let Some(sink) = state.feedback.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Feedback delivery is not available on this server",
        )
            .into_response();
    };

    let task_state = Arc::clone(&state);
    let permit = permit.take();
    let saved = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        save_feedback_for_thread(&task_state, user_id, &id, &submission)
    })
    .await;
    let (route, saved, guard) = match saved {
        Ok(Ok(Some(saved))) => saved,
        Ok(Ok(None)) => {
            return (
                StatusCode::OK,
                Json(FeedbackResponse {
                    status: "duplicate",
                    annotations: 0,
                    stale: false,
                }),
            )
                .into_response();
        }
        Ok(Err(failure)) => return failure.into_response(),
        Err(error) => {
            tracing::warn!(%error, "Mini App feedback task failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Saving feedback failed").into_response();
        }
    };

    let annotations = saved.annotation_count;
    let sent_stale = saved.stale;
    let delivered = tokio::spawn(async move {
        let _guard = guard;
        let delivery = FeedbackDelivery {
            route,
            user_id,
            attachments: feedback_attachments(&saved),
            prompt: saved.prompt,
            artifact_name: saved.artifact_name,
            annotation_count: saved.annotation_count,
            bundle_dir: saved.dir,
        };
        sink.deliver(delivery).await
    })
    .await;
    match delivered {
        Ok(Ok(())) => (
            StatusCode::ACCEPTED,
            Json(FeedbackResponse {
                status: "queued",
                annotations,
                stale: sent_stale,
            }),
        )
            .into_response(),
        Ok(Err(error)) => {
            tracing::error!(error = %format!("{error:#}"), "Failed to queue artifact feedback");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Feedback could not be queued. Try again.",
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "Artifact feedback delivery task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Feedback could not be queued. Try again.",
            )
                .into_response()
        }
    }
}

type SavedForDelivery = (FeedbackRoute, SavedFeedback, InFlightGuard);

/// Blocking half of [`post_artifact_feedback`]: route, permission, then save.
/// `Ok(None)` is a duplicate submission.
fn save_feedback_for_thread(
    state: &Arc<ServerState>,
    user_id: i64,
    id: &str,
    submission: &FeedbackSubmission,
) -> Result<Option<SavedForDelivery>, FeedbackFailure> {
    // `active` moves to whichever thread was used last, so a write through it
    // could land in a different conversation than the one previewed.
    if id == "active" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Reopen the thread from its chat to send feedback.".to_string(),
        ));
    }
    if !valid_thread_id(id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid thread ID".to_string()));
    }
    let Some(sink) = state.feedback.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Feedback delivery is not available on this server".to_string(),
        ));
    };
    // Routing uses the requested id (the chat topic); the artifact belongs to
    // the effective thread behind a resumed or mirror alias.
    let route =
        resolve_destination(id).map_err(|message| (StatusCode::CONFLICT, message.to_string()))?;
    if !sink.allows(user_id, route) {
        return Err((
            StatusCode::FORBIDDEN,
            "You cannot send feedback into this chat".to_string(),
        ));
    }
    let effective = resolve_alias_id(id);
    let key = format!("{effective}/{}", submission.submission_id);
    let fresh = state
        .feedback_in_flight
        .0
        .lock()
        .is_ok_and(|mut keys| keys.insert(key.clone()));
    if !fresh {
        return Err((
            StatusCode::CONFLICT,
            "This feedback is still being sent. Try again in a moment.".to_string(),
        ));
    }
    let guard = InFlightGuard {
        state: Arc::clone(state),
        key,
    };
    // Part of the retry identity: the same id from another user or for
    // another destination is a conflict, not a duplicate.
    let binding = format!("user={user_id};dest={id}");
    match artifact_feedback::save_feedback(&effective, &binding, submission) {
        Ok(SaveOutcome::Saved(saved)) => Ok(Some((route, saved, guard))),
        Ok(SaveOutcome::Duplicate) => Ok(None),
        Err(error) => Err(feedback_failure(&effective, &error)),
    }
}

fn feedback_failure(thread_id: &str, error: &FeedbackError) -> FeedbackFailure {
    let status = match error {
        FeedbackError::Invalid(_) => StatusCode::BAD_REQUEST,
        FeedbackError::ArtifactNotFound => StatusCode::NOT_FOUND,
        FeedbackError::UnsupportedArtifact(_) => StatusCode::UNPROCESSABLE_ENTITY,
        // Distinct from the 409s so the client can offer "send anyway".
        FeedbackError::VersionMismatch => StatusCode::PRECONDITION_FAILED,
        FeedbackError::IdConflict => StatusCode::CONFLICT,
        FeedbackError::Busy => StatusCode::SERVICE_UNAVAILABLE,
        FeedbackError::Storage(err) => {
            tracing::error!(thread_id, error = %format!("{err:#}"), "Failed to save artifact feedback");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Saving feedback failed".to_string(),
            );
        }
    };
    (status, error.to_string())
}

pub(crate) fn feedback_attachments(saved: &SavedFeedback) -> Vec<LocalAttachment> {
    saved
        .audios
        .iter()
        .map(|audio| LocalAttachment::Audio {
            path: audio.path.clone(),
            mime: audio.mime.clone(),
            label: audio.label.clone(),
        })
        .chain(saved.images.iter().map(|image| LocalAttachment::Image {
            path: image.path.clone(),
            label: image.label.clone(),
        }))
        .collect()
}

/// Attaches the shared live status of the turn running on `source_id` (the
/// alias-resolved thread the marker is bound to), and appends a `ToolRunning`
/// record for each call of its current tool round. Skips ids already persisted
/// as `tool_use`: a finished round lands in the JSONL while its calls are still
/// in the marker.
fn attach_live_run(response: &mut ThreadResponse, source_id: &str) {
    let Some(record) = agent_activity::active_run_for_thread(source_id) else {
        return;
    };
    response.run = Some(record.status(chrono::Utc::now()));
    let persisted: HashSet<String> = response
        .activity
        .iter()
        .filter_map(|a| match a {
            ThreadActivity::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut sequence = response.cursor;
    for tool in record.current_tools {
        if persisted.contains(&tool.id) {
            continue;
        }
        let started_at = tool.started_at;
        let running_for = match (&tool.completed_at, tool.duration_ms) {
            (None, _) => agent_activity::uptime_since(&started_at),
            (Some(_), Some(ms)) => agent_activity::format_secs(ms / 1000),
            (Some(_), None) => String::new(),
        };
        response.activity.push(ThreadActivity::ToolRunning {
            sequence,
            time: event_time(&started_at),
            ts: started_at.clone(),
            started_at,
            running_for,
            id: tool.id,
            name: tool.name.to_ascii_lowercase(),
            summary: tool.summary,
            input: tool.input,
            output_tail: tool.output_tail,
            completed_at: tool.completed_at,
            ok: tool.ok,
            duration_ms: tool.duration_ms,
        });
        sequence += 1;
    }
}

#[allow(clippy::too_many_lines)]
fn project_thread(
    target_id: String,
    events: Vec<ThreadEvent>,
    telegram_link: Option<String>,
    lineage: ThreadLineage,
) -> ThreadResponse {
    let cursor = events.len();
    let mut title = "Thread Transcript".to_string();
    let mut activity = Vec::new();
    let mut total_messages = 0;
    let superseded = zdx_engine::core::thread_persistence::superseded_deliveries(&events);

    for (sequence, event) in events.into_iter().enumerate() {
        match event {
            ThreadEvent::Meta { title: Some(t), .. } => {
                title = t;
            }
            ThreadEvent::Message {
                role,
                text,
                phase,
                ts,
                ..
            } => {
                let speaker = if role == "user" { "You" } else { "Z" };
                let attachments = artifacts::attachments_for_message(&text);
                let artifacts = (!attachments.is_empty()).then_some(attachments);
                let clean_text = clean_message_text(&text);
                if !clean_text.is_empty() || artifacts.is_some() {
                    activity.push(ThreadActivity::Message {
                        sequence,
                        time: event_time(&ts),
                        ts,
                        role,
                        speaker,
                        text: clean_text,
                        phase,
                        artifacts,
                    });
                    total_messages += 1;
                }
            }
            ThreadEvent::Reasoning { text, replay, ts } => {
                let visible = text.filter(|text| !text.trim().is_empty());
                let redacted = visible.is_none()
                    && matches!(replay, Some(ReplayToken::AnthropicRedacted { .. }));
                if visible.is_some() || redacted {
                    activity.push(ThreadActivity::Reasoning {
                        sequence,
                        time: event_time(&ts),
                        ts,
                        text: visible.unwrap_or_default(),
                        redacted,
                    });
                }
            }
            ThreadEvent::ToolUse {
                id,
                name,
                input,
                ts,
                ..
            } => activity.push(ThreadActivity::ToolUse {
                sequence,
                time: event_time(&ts),
                ts,
                id,
                name,
                input,
            }),
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                duration_ms,
                started_at,
                completed_at,
                ts,
            } => activity.push(ThreadActivity::ToolResult {
                sequence,
                time: event_time(&ts),
                ts,
                tool_use_id,
                ok,
                duration_ms,
                started_at,
                completed_at,
                output,
            }),
            ThreadEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                model,
                provider,
                duration_ms,
                ttft_ms,
                started_at,
                completed_at,
                ts,
            } => activity.push(ThreadActivity::Usage {
                sequence,
                time: event_time(&ts),
                ts,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                context_limit: model
                    .as_deref()
                    .and_then(ModelOption::find_by_id)
                    .map(|m| m.context_limit)
                    .filter(|limit| *limit > 0),
                model,
                provider,
                duration_ms,
                ttft_ms,
                started_at,
                completed_at,
            }),
            ThreadEvent::Delivery { record, ts } => {
                if superseded.contains(&sequence) {
                    continue;
                }
                if let Some(message) = zdx_engine::core::delivery::notice_text(&record) {
                    activity.push(ThreadActivity::Notice {
                        sequence,
                        time: event_time(&ts),
                        ts,
                        kind: "delivery",
                        message,
                    });
                }
            }
            ThreadEvent::Posted { post, ts } => activity.push(ThreadActivity::Notice {
                sequence,
                time: event_time(&ts),
                ts,
                kind: "posted",
                message: zdx_engine::core::posted::notice_text(&post),
            }),
            ThreadEvent::Notice { kind, message, ts } => {
                let kind = match kind {
                    NoticeKind::Refusal => "refusal",
                    NoticeKind::ContextWindowExceeded => "context_window_exceeded",
                    NoticeKind::OutputLimit => "output_limit",
                    NoticeKind::OutputLoop => "output_loop",
                };
                activity.push(ThreadActivity::Notice {
                    sequence,
                    time: event_time(&ts),
                    ts,
                    kind,
                    message,
                });
            }
            ThreadEvent::Interrupted { role, text, ts } => {
                activity.push(ThreadActivity::Interrupted {
                    sequence,
                    time: event_time(&ts),
                    ts,
                    role,
                    text,
                });
            }
            ThreadEvent::Meta { .. } => {}
        }
    }

    ThreadResponse {
        telegram_link,
        id: target_id,
        title,
        total_messages,
        total_events: activity.len(),
        cursor,
        partial: false,
        parent_thread_id: lineage.parent_thread_id,
        parent_title: lineage.parent_title,
        worker_count: lineage.worker_count,
        delivering: false,
        run: None,
        activity,
    }
}

fn event_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts).map_or_else(
        |_error| "--:--".to_string(),
        |date_time| date_time.format("%I:%M %p").to_string(),
    )
}

fn valid_thread_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\', '\0'])
}

#[derive(Debug, PartialEq, Eq)]
enum InitDataError {
    Missing,
    Malformed,
    InvalidSignature,
    Expired,
    Forbidden,
}

fn authorize(headers: &HeaderMap, state: &ServerState) -> Result<i64, ApiError> {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "Telegram authorization required"))?;
    let (scheme, init_data) = authorization
        .split_once(' ')
        .ok_or((StatusCode::UNAUTHORIZED, "Invalid Telegram authorization"))?;
    if !scheme.eq_ignore_ascii_case("tma") || init_data.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "Invalid Telegram authorization"));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| (StatusCode::INTERNAL_SERVER_ERROR, "System clock error"))?
        .as_secs();
    validate_init_data(init_data, &state.bot_token, &state.allowlist_user_ids, now).map_err(
        |error| match error {
            InitDataError::Forbidden => (StatusCode::FORBIDDEN, "Telegram user is not allowed"),
            InitDataError::Missing => (StatusCode::UNAUTHORIZED, "Telegram authorization required"),
            InitDataError::Malformed | InitDataError::InvalidSignature | InitDataError::Expired => {
                (
                    StatusCode::UNAUTHORIZED,
                    "Invalid or expired Telegram authorization",
                )
            }
        },
    )
}

fn validate_init_data(
    init_data: &str,
    bot_token: &str,
    allowlist_user_ids: &HashSet<i64>,
    now: u64,
) -> Result<i64, InitDataError> {
    if init_data.is_empty() {
        return Err(InitDataError::Missing);
    }

    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(init_data.as_bytes())
        .into_owned()
        .collect();
    let mut hash = None;
    let mut auth_date = None;
    let mut user = None;
    for (key, value) in &pairs {
        match key.as_str() {
            "hash" if hash.replace(value.as_str()).is_some() => {
                return Err(InitDataError::Malformed);
            }
            "auth_date" if auth_date.replace(value.as_str()).is_some() => {
                return Err(InitDataError::Malformed);
            }
            "user" if user.replace(value.as_str()).is_some() => {
                return Err(InitDataError::Malformed);
            }
            _ => {}
        }
    }

    let received_hash = decode_hash(hash.ok_or(InitDataError::Malformed)?)?;
    let auth_date = auth_date
        .ok_or(InitDataError::Malformed)?
        .parse::<u64>()
        .map_err(|_error| InitDataError::Malformed)?;
    if auth_date > now.saturating_add(INIT_DATA_FUTURE_SKEW_SECS)
        || now.saturating_sub(auth_date) > INIT_DATA_MAX_AGE_SECS
    {
        return Err(InitDataError::Expired);
    }

    let mut checked_pairs: Vec<&(String, String)> =
        pairs.iter().filter(|(key, _)| key != "hash").collect();
    checked_pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let data_check_string = checked_pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut secret_mac =
        HmacSha256::new_from_slice(b"WebAppData").expect("HMAC accepts keys of any length");
    secret_mac.update(bot_token.as_bytes());
    let secret_key = secret_mac.finalize().into_bytes();

    let mut data_mac =
        HmacSha256::new_from_slice(&secret_key).expect("HMAC accepts keys of any length");
    data_mac.update(data_check_string.as_bytes());
    data_mac
        .verify_slice(&received_hash)
        .map_err(|_error| InitDataError::InvalidSignature)?;

    let user: InitDataUser = serde_json::from_str(user.ok_or(InitDataError::Malformed)?)
        .map_err(|_error| InitDataError::Malformed)?;
    if !allowlist_user_ids.contains(&user.id) {
        return Err(InitDataError::Forbidden);
    }

    Ok(user.id)
}

fn decode_hash(hash: &str) -> Result<[u8; 32], InitDataError> {
    if hash.len() != 64 {
        return Err(InitDataError::Malformed);
    }

    let mut bytes = [0_u8; 32];
    for (index, pair) in hash.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_digit(pair[0]).ok_or(InitDataError::Malformed)?;
        let low = decode_hex_digit(pair[1]).ok_or(InitDataError::Malformed)?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn decode_hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn clean_message_text(text: &str) -> String {
    let mut clean = text.to_string();
    if let Some(start) = clean.find("<followups>")
        && let Some(end) = clean.find("</followups>")
    {
        clean.replace_range(start..end + "</followups>".len(), "");
    }
    // Attachments render as their own rows (see `artifacts` on the message),
    // so strip every media tag. The wrapper goes first: it spans the inner
    // tags, and the second pass removes any stray tag outside a wrapper.
    while let Some(start) = clean.find("<medias>") {
        let Some(end_rel) = clean[start..].find("</medias>") else {
            break;
        };
        clean.replace_range(start..start + end_rel + "</medias>".len(), "");
    }
    while let Some(start) = clean.find("<media>") {
        let Some(end_rel) = clean[start..].find("</media>") else {
            break;
        };
        clean.replace_range(start..start + end_rel + "</media>".len(), "");
    }
    clean.trim().to_string()
}

struct GitSelection {
    thread_id: Option<String>,
    thread_root: Option<PathBuf>,
}

struct ResolvedGitRepository {
    root: PathBuf,
    source: &'static str,
    thread_id: Option<String>,
}

#[derive(Default)]
struct ParsedGitStatus {
    oid: Option<String>,
    branch: Option<String>,
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    files: GitFiles,
}

#[derive(Default)]
struct GitWorktreeBuilder {
    path: Option<String>,
    head: Option<String>,
    branch: Option<String>,
    flags: Vec<&'static str>,
}

#[derive(Clone)]
enum GitDiffKind {
    Staged,
    Unstaged,
    Untracked,
    /// One commit, addressed by a validated hash (`git show <hash>`).
    Commit(String),
    /// Everything since a base revision *including* the working tree, so a
    /// single view covers committed-but-unpushed work plus local edits.
    Range(String),
}

impl GitDiffKind {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
            Self::Commit(_) => "commit",
            Self::Range(_) => "all",
        }
    }
}

/// Accepts only an abbreviated-or-full hex object id.
///
/// Everything here reaches a `git` argument list, so a revision that could be
/// read as an option or a range never gets built in the first place.
fn valid_commit_hash(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The revision "all changes" diffs against, plus what it was derived from.
struct AllChangesBase {
    /// Object id (or `HEAD`) handed to `git diff`.
    rev: String,
    /// Branch the base was taken from (`master`, `origin/main`, ...). `None`
    /// when no main branch could be found and the scope fell back to `HEAD`.
    reference: Option<String>,
    /// Commits on this branch since the base, for the selector subtitle.
    ahead: usize,
}

/// Resolves the base for the "all changes" scope: the merge base with the
/// repository's main branch, so the view shows what *this branch* adds, the way
/// a pull request would. On the main branch itself the base is `HEAD` and the
/// scope equals "uncommitted", which is the correct answer there.
///
/// The main branch is read from `origin/HEAD` and falls back to `main` then
/// `master`. A local branch is preferred over its remote so that unpushed
/// commits on main do not appear as "changes"; the remote is used only when no
/// local branch of that name exists.
async fn resolve_all_base(root: &FilePath) -> AllChangesBase {
    let fallback = AllChangesBase {
        rev: "HEAD".to_string(),
        reference: None,
        ahead: 0,
    };

    let Some(reference) = main_branch_ref(root).await else {
        return fallback;
    };
    let merge_base = git_output(root, &["merge-base", &reference, "HEAD"])
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|base| base.trim().to_string())
        .filter(|base| valid_commit_hash(base));
    let Some(rev) = merge_base else {
        return fallback;
    };

    let ahead = git_output(root, &["rev-list", "--count", &format!("{rev}..HEAD")])
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0);

    AllChangesBase {
        rev,
        reference: Some(reference),
        ahead,
    }
}

/// Finds the repository's main branch as a revision `git merge-base` accepts.
async fn main_branch_ref(root: &FilePath) -> Option<String> {
    // `origin/HEAD` names the default branch on a normal clone.
    let from_origin = git_output(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    .ok()
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .map(|name| name.trim().to_string())
    .and_then(|name| name.strip_prefix("origin/").map(str::to_string));

    let mut candidates: Vec<String> = Vec::new();
    if let Some(name) = from_origin {
        candidates.push(name);
    }
    candidates.extend(["main".to_string(), "master".to_string()]);

    for name in &candidates {
        if git_ref_exists(root, &format!("refs/heads/{name}")).await {
            return Some(name.clone());
        }
    }
    for name in &candidates {
        if git_ref_exists(root, &format!("refs/remotes/origin/{name}")).await {
            return Some(format!("origin/{name}"));
        }
    }
    None
}

async fn git_ref_exists(root: &FilePath, full_ref: &str) -> bool {
    git_output(root, &["rev-parse", "--verify", "--quiet", full_ref])
        .await
        .is_ok()
}

async fn get_git(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitQuery>,
) -> Result<Json<GitResponse>, ApiError> {
    let repository = resolve_git_repository(&state, query.thread_id).await?;
    let status = load_git_status(&repository.root).await.map_err(|error| {
        tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to read Mini App Git state");
        (StatusCode::INTERNAL_SERVER_ERROR, "Git state unavailable")
    })?;
    let commits = async {
        if status.oid.is_some() {
            load_git_commits(&repository.root).await
        } else {
            Ok(Vec::new())
        }
    };
    let (mut worktrees, commits) =
        tokio::try_join!(load_git_worktrees(&repository.root), commits).map_err(|error| {
            tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to read Mini App Git state");
            (StatusCode::INTERNAL_SERVER_ERROR, "Git state unavailable")
        })?;

    for worktree in &mut worktrees {
        worktree.current = FilePath::new(&worktree.path) == repository.root;
    }

    let detached = status.branch.as_deref() == Some("(detached)");
    let branch = if detached {
        "detached".to_string()
    } else {
        status.branch.clone().unwrap_or_else(|| "HEAD".to_string())
    };
    let clean = status.files.staged.is_empty()
        && status.files.unstaged.is_empty()
        && status.files.untracked.is_empty();
    let name = repository
        .root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
        .to_string();
    let head = status.oid.as_deref().map(short_hash);

    Ok(Json(GitResponse {
        repository: GitRepository {
            name,
            root: repository.root.to_string_lossy().into_owned(),
            source: repository.source,
            thread_id: repository.thread_id,
            branch,
            head,
            upstream: status.upstream,
            ahead: status.ahead,
            behind: status.behind,
            detached,
            clean,
        },
        worktrees,
        files: status.files,
        commits,
    }))
}

/// Lists the files a non-uncommitted scope touches.
///
/// `scope=all` diffs the base against the working tree, so it already covers
/// committed, staged and unstaged changes in one pass; untracked files are not
/// part of a diff and are appended from `status`. A commit scope lists exactly
/// what that commit changed.
async fn load_scope_files(root: &FilePath, kind: &GitDiffKind) -> anyhow::Result<Vec<GitFile>> {
    let output = match kind {
        GitDiffKind::Range(base) => {
            git_output(root, &["diff", "--name-status", "--no-renames", base]).await?
        }
        GitDiffKind::Commit(hash) => {
            git_output(
                root,
                &["show", "--name-status", "--no-renames", "--format=", hash],
            )
            .await?
        }
        _ => return Ok(Vec::new()),
    };
    let output = String::from_utf8(output).context("Git file list contains non-UTF-8 text")?;

    Ok(output
        .lines()
        .filter_map(|line| {
            let (status, path) = line.split_once('\t')?;
            let path = path.trim();
            (!path.is_empty() && valid_git_path(path)).then(|| GitFile {
                path: path.to_string(),
                status: status.trim().to_string(),
                original_path: None,
            })
        })
        .collect())
}

async fn get_git_scope(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitScopeQuery>,
) -> Result<Json<GitScopeResponse>, ApiError> {
    let repository = resolve_git_repository(&state, query.thread_id).await?;
    let scope = query.scope.trim();

    let (kind, base_ref, ahead) = if scope == "all" {
        let base = resolve_all_base(&repository.root).await;
        (GitDiffKind::Range(base.rev), base.reference, base.ahead)
    } else if valid_commit_hash(scope) {
        (GitDiffKind::Commit(scope.to_string()), None, 0)
    } else {
        return Err((StatusCode::BAD_REQUEST, "Invalid Git scope"));
    };

    let mut files = load_scope_files(&repository.root, &kind).await.map_err(|error| {
        tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to list Mini App Git scope");
        (StatusCode::INTERNAL_SERVER_ERROR, "Git scope unavailable")
    })?;

    // Untracked files never appear in a diff, so "all changes" would silently
    // omit brand-new files without this.
    let mut untracked = Vec::new();
    if matches!(kind, GitDiffKind::Range(_)) {
        let status = load_git_status(&repository.root).await.map_err(|error| {
            tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to read Mini App Git state");
            (StatusCode::INTERNAL_SERVER_ERROR, "Git state unavailable")
        })?;
        untracked = status.files.untracked;
    }
    files.append(&mut untracked.clone());

    Ok(Json(GitScopeResponse {
        scope: kind.as_str(),
        base: match &kind {
            GitDiffKind::Range(base) => Some(base.clone()),
            GitDiffKind::Commit(hash) => Some(hash.clone()),
            _ => None,
        },
        base_ref,
        ahead,
        untracked: untracked.into_iter().map(|file| file.path).collect(),
        files,
    }))
}

async fn get_git_diff(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitDiffQuery>,
) -> Result<Json<GitDiffResponse>, ApiError> {
    if !valid_git_path(&query.path) {
        return Err((StatusCode::BAD_REQUEST, "Invalid Git file path"));
    }

    let repository = resolve_git_repository(&state, query.thread_id).await?;

    // `all` and `commit` address history, so their base has to be resolved
    // against the repository before the file can be validated.
    let kind = match query.kind.as_str() {
        "staged" => GitDiffKind::Staged,
        "unstaged" => GitDiffKind::Unstaged,
        "untracked" => GitDiffKind::Untracked,
        "all" => GitDiffKind::Range(resolve_all_base(&repository.root).await.rev),
        "commit" => {
            let hash = query
                .commit
                .as_deref()
                .map(str::trim)
                .filter(|hash| valid_commit_hash(hash))
                .ok_or((StatusCode::BAD_REQUEST, "Invalid commit hash"))?;
            GitDiffKind::Commit(hash.to_string())
        }
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid Git file category")),
    };

    match &kind {
        GitDiffKind::Staged | GitDiffKind::Unstaged | GitDiffKind::Untracked => {
            let status = load_git_status(&repository.root).await.map_err(|error| {
                tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to validate Mini App Git diff");
                (StatusCode::INTERNAL_SERVER_ERROR, "Git state unavailable")
            })?;
            let files = match kind {
                GitDiffKind::Staged => &status.files.staged,
                GitDiffKind::Unstaged => &status.files.unstaged,
                _ => &status.files.untracked,
            };
            if !files.iter().any(|file| file.path == query.path) {
                return Err((
                    StatusCode::NOT_FOUND,
                    "Git file is no longer in that category",
                ));
            }
        }
        GitDiffKind::Commit(_) | GitDiffKind::Range(_) => {
            let files = load_scope_files(&repository.root, &kind).await.map_err(|error| {
                tracing::warn!(root = %repository.root.display(), error = %format!("{error:#}"), "Failed to validate Mini App Git diff");
                (StatusCode::INTERNAL_SERVER_ERROR, "Git scope unavailable")
            })?;
            if !files.iter().any(|file| file.path == query.path) {
                return Err((StatusCode::NOT_FOUND, "Git file is not in that scope"));
            }
        }
    }

    let (bytes, truncated) = load_git_diff(&repository.root, &query.path, &kind)
        .await
        .map_err(|error| {
            tracing::warn!(root = %repository.root.display(), path = query.path, error = %format!("{error:#}"), "Failed to read Mini App Git diff");
            (StatusCode::INTERNAL_SERVER_ERROR, "Git diff unavailable")
        })?;
    let byte_count = bytes.len().min(GIT_DIFF_LIMIT_BYTES);
    let content = String::from_utf8_lossy(&bytes[..byte_count]).into_owned();
    let lines = content.lines().count();

    Ok(Json(GitDiffResponse {
        path: query.path,
        kind: kind.as_str(),
        content,
        bytes: byte_count,
        lines,
        truncated,
        limit_bytes: GIT_DIFF_LIMIT_BYTES,
    }))
}

async fn resolve_git_repository(
    state: &ServerState,
    thread_id: Option<String>,
) -> Result<ResolvedGitRepository, ApiError> {
    let requested_id = thread_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| "active".to_string());
    if requested_id != "active" && !valid_thread_id(&requested_id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid thread ID"));
    }

    let bot_root = state.root.clone();
    let selection = tokio::task::spawn_blocking(move || {
        let thread_id = if requested_id == "active" {
            thread_index::latest_thread_id_with_prefix("telegram-")
                .ok()
                .flatten()
        } else {
            Some(requested_id)
        };
        let effective_id = thread_id.as_deref().map(|id| {
            thread_persistence::read_thread_alias(id)
                .ok()
                .flatten()
                .filter(|alias| valid_thread_id(alias))
                .unwrap_or_else(|| id.to_string())
        });
        let thread_root = effective_id.as_deref().and_then(|id| {
            thread_persistence::read_thread_root_path(id)
                .ok()
                .flatten()
                .map(PathBuf::from)
        });
        GitSelection {
            thread_id,
            thread_root,
        }
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App Git thread resolution task failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Git repository resolution failed",
        )
    })?;

    let mut candidates = Vec::with_capacity(2);
    if let Some(root) = selection.thread_root {
        candidates.push(("thread", root));
    }
    if candidates
        .first()
        .is_none_or(|(_, candidate)| candidate != &bot_root)
    {
        candidates.push(("bot_root", bot_root));
    }

    for (source, candidate) in candidates {
        if let Ok(output) = git_output(&candidate, &["rev-parse", "--show-toplevel"]).await {
            let Ok(output) = String::from_utf8(output) else {
                continue;
            };
            let root = output.trim_end_matches(['\n', '\r']).to_string();
            if !root.is_empty() {
                return Ok(ResolvedGitRepository {
                    root: PathBuf::from(root),
                    source,
                    thread_id: selection.thread_id,
                });
            }
        }
    }

    Err((StatusCode::NOT_FOUND, "Git repository not found"))
}

async fn git_output(root: &FilePath, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_COMMAND_TIMEOUT, command.output())
        .await
        .context("Git command timed out")?
        .context("Failed to run Git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git exited with {}: {}", output.status, stderr.trim());
    }
    Ok(output.stdout)
}

async fn load_git_status(root: &FilePath) -> anyhow::Result<ParsedGitStatus> {
    let output = git_output(
        root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
    )
    .await?;
    let output = String::from_utf8(output).context("Git status contains non-UTF-8 paths")?;
    Ok(parse_git_status(&output))
}

fn parse_git_status(output: &str) -> ParsedGitStatus {
    let mut records = output.split('\0');
    let mut status = ParsedGitStatus::default();

    while let Some(record) = records.next() {
        if let Some(oid) = record.strip_prefix("# branch.oid ") {
            if oid != "(initial)" {
                status.oid = Some(oid.to_string());
            }
        } else if let Some(branch) = record.strip_prefix("# branch.head ") {
            status.branch = Some(branch.to_string());
        } else if let Some(upstream) = record.strip_prefix("# branch.upstream ") {
            status.upstream = Some(upstream.to_string());
        } else if let Some(counts) = record.strip_prefix("# branch.ab ") {
            for count in counts.split_whitespace() {
                if let Some(ahead) = count.strip_prefix('+') {
                    status.ahead = ahead.parse().unwrap_or_default();
                } else if let Some(behind) = count.strip_prefix('-') {
                    status.behind = behind.parse().unwrap_or_default();
                }
            }
        } else if let Some(path) = record.strip_prefix("? ") {
            status.files.untracked.push(GitFile {
                path: path.to_string(),
                status: "?".to_string(),
                original_path: None,
            });
        } else if record.starts_with("1 ") {
            if let Some((xy, path)) = parse_status_path(record, 9) {
                push_changed_file(&mut status.files, xy, path, None);
            }
        } else if record.starts_with("2 ") {
            if let Some((xy, path)) = parse_status_path(record, 10) {
                let original_path = records.next().map(str::to_string);
                push_changed_file(&mut status.files, xy, path, original_path);
            }
        } else if record.starts_with("u ")
            && let Some((xy, path)) = parse_status_path(record, 11)
        {
            push_changed_file(&mut status.files, xy, path, None);
        }
    }

    status
}

fn parse_status_path(record: &str, field_count: usize) -> Option<(&str, &str)> {
    let fields: Vec<&str> = record.splitn(field_count, ' ').collect();
    (fields.len() == field_count).then(|| (fields[1], fields[field_count - 1]))
}

fn push_changed_file(files: &mut GitFiles, xy: &str, path: &str, original_path: Option<String>) {
    let mut status = xy.chars();
    if let Some(index) = status.next()
        && index != '.'
    {
        files.staged.push(GitFile {
            path: path.to_string(),
            status: index.to_string(),
            original_path: original_path.clone(),
        });
    }
    if let Some(worktree) = status.next()
        && worktree != '.'
    {
        files.unstaged.push(GitFile {
            path: path.to_string(),
            status: worktree.to_string(),
            original_path,
        });
    }
}

async fn load_git_worktrees(root: &FilePath) -> anyhow::Result<Vec<GitWorktree>> {
    let output = git_output(root, &["worktree", "list", "--porcelain", "-z"]).await?;
    let output = String::from_utf8(output).context("Git worktrees contain non-UTF-8 paths")?;
    Ok(parse_git_worktrees(&output))
}

fn parse_git_worktrees(output: &str) -> Vec<GitWorktree> {
    let mut worktrees = Vec::new();
    let mut current = GitWorktreeBuilder::default();

    for field in output.split('\0') {
        if field.is_empty() {
            finish_worktree(&mut worktrees, &mut current);
        } else if let Some(path) = field.strip_prefix("worktree ") {
            finish_worktree(&mut worktrees, &mut current);
            current.path = Some(path.to_string());
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            current.head = Some(short_hash(head));
        } else if let Some(branch) = field.strip_prefix("branch ") {
            current.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if field == "detached" {
            current.flags.push("detached");
        } else if field == "bare" {
            current.flags.push("bare");
        } else if field == "locked" || field.starts_with("locked ") {
            current.flags.push("locked");
        } else if field == "prunable" || field.starts_with("prunable ") {
            current.flags.push("prunable");
        }
    }
    finish_worktree(&mut worktrees, &mut current);
    worktrees
}

fn finish_worktree(worktrees: &mut Vec<GitWorktree>, builder: &mut GitWorktreeBuilder) {
    if let Some(path) = builder.path.take() {
        worktrees.push(GitWorktree {
            path,
            head: builder.head.take(),
            branch: builder.branch.take(),
            current: false,
            flags: std::mem::take(&mut builder.flags),
        });
    }
}

async fn load_git_commits(root: &FilePath) -> anyhow::Result<Vec<GitCommit>> {
    let limit = format!("-n{GIT_COMMIT_LIMIT}");
    let output = git_output(
        root,
        &[
            "log",
            &limit,
            "--date=iso-strict",
            "--format=%H%x1f%h%x1f%an%x1f%aI%x1f%ar%x1f%D%x1f%s%x1e",
        ],
    )
    .await?;
    let output = String::from_utf8(output).context("Git log contains non-UTF-8 text")?;
    Ok(parse_git_commits(&output))
}

fn parse_git_commits(output: &str) -> Vec<GitCommit> {
    output
        .split('\x1e')
        .filter_map(|record| {
            let record = record.trim_matches(['\n', '\r']);
            if record.is_empty() {
                return None;
            }
            let fields: Vec<&str> = record.splitn(7, '\x1f').collect();
            (fields.len() == 7).then(|| GitCommit {
                hash: fields[0].to_string(),
                short_hash: fields[1].to_string(),
                author: fields[2].to_string(),
                authored_at: fields[3].to_string(),
                relative_time: fields[4].to_string(),
                refs: fields[5].to_string(),
                subject: fields[6].to_string(),
            })
        })
        .collect()
}

/// Builds the `git` argument list that renders one file's diff for a scope.
fn git_diff_args<'a>(kind: &'a GitDiffKind, path: &'a str) -> Vec<&'a str> {
    const FLAGS: [&str; 3] = ["--no-color", "--no-ext-diff", "--no-textconv"];
    let mut args = Vec::with_capacity(9);
    match kind {
        GitDiffKind::Staged => {
            args.extend(["diff", "--cached"]);
            args.extend(FLAGS);
            args.extend(["--", path]);
        }
        GitDiffKind::Unstaged => {
            args.push("diff");
            args.extend(FLAGS);
            args.extend(["--", path]);
        }
        GitDiffKind::Untracked => {
            args.extend(["diff", "--no-index"]);
            args.extend(FLAGS);
            args.extend(["--", "/dev/null", path]);
        }
        GitDiffKind::Commit(hash) => {
            args.push("show");
            args.extend(FLAGS);
            args.extend(["--format=", hash.as_str(), "--", path]);
        }
        GitDiffKind::Range(base) => {
            // Base against the working tree, so one diff covers the committed,
            // staged and unstaged state of this file.
            args.push("diff");
            args.extend(FLAGS);
            args.extend([base.as_str(), "--", path]);
        }
    }
    args
}

async fn load_git_diff(
    root: &FilePath,
    path: &str,
    kind: &GitDiffKind,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .args(git_diff_args(kind, path));

    let mut child = command.spawn().context("Failed to start Git diff")?;
    let mut stdout = child.stdout.take().context("Git diff stdout unavailable")?;
    let mut bytes = Vec::with_capacity(GIT_DIFF_LIMIT_BYTES + 1);
    let result = tokio::time::timeout(GIT_COMMAND_TIMEOUT, async {
        (&mut stdout)
            .take((GIT_DIFF_LIMIT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .context("Failed to read Git diff")?;
        if bytes.len() > GIT_DIFF_LIMIT_BYTES {
            bytes.truncate(GIT_DIFF_LIMIT_BYTES);
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Ok::<_, anyhow::Error>((true, None));
        }
        let status = child.wait().await.context("Failed to wait for Git diff")?;
        Ok((false, Some(status)))
    })
    .await;

    let (truncated, exit_status) = match result {
        Ok(result) => result?,
        Err(_elapsed) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("Git diff timed out");
        }
    };
    if let Some(exit_status) = exit_status
        && !exit_status.success()
        && !(matches!(kind, GitDiffKind::Untracked) && exit_status.code() == Some(1))
    {
        anyhow::bail!("Git diff exited with {exit_status}");
    }
    Ok((bytes, truncated))
}

fn valid_git_path(path: &str) -> bool {
    !path.is_empty()
        && FilePath::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(8).collect()
}

#[derive(Serialize)]
struct WorkspacesResponse {
    workspaces: Vec<WorkspaceReport>,
}

/// Read-only model settings of every workspace (Telegram bindings + the bot
/// root), each value tagged with the layer that supplied it and what a reset
/// of the workspace layer would fall back to. A thin adapter over
/// `zdx_engine::workspace_config`; it reads a few small TOML files, so it is
/// not cached.
async fn get_workspaces(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<WorkspacesResponse>, ApiError> {
    let root = state.root.clone();
    tokio::task::spawn_blocking(move || workspace_config::report(Some(&root)))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App workspaces task failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Workspaces unavailable")
        })?
        .map(|workspaces| Json(WorkspacesResponse { workspaces }))
        .map_err(|error| {
            tracing::warn!(error = %format!("{error:#}"), "Failed to list Mini App workspaces");
            (StatusCode::INTERNAL_SERVER_ERROR, "Workspaces unavailable")
        })
}

async fn get_monitor(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<MonitorResponse>, ApiError> {
    let mut response = monitor_snapshot(&state).await?;
    response.active_agents = tokio::task::spawn_blocking(active_monitor_agents)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App active agents task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Active agents unavailable",
            )
        })?;
    attach_subscriptions(&state, &mut response).await;
    Ok(Json(response))
}

/// Active runs from their markers, read fresh on every request (a directory
/// of a few small files), so the dashboard cache never delays them.
fn active_monitor_agents() -> Vec<MonitorAgent> {
    let now = chrono::Utc::now();
    agent_activity::list_active()
        .into_iter()
        .map(|record| MonitorAgent {
            run: record.status(now),
            pid: record.pid,
            uptime: agent_activity::uptime_since(&record.started_at),
            thread_id: record.thread_id,
            parent_thread_id: record.parent_thread_id,
            surface: record.surface,
            role: record.subagent_name.or(record.kind),
            model: record.model,
            provider: record.provider,
            account: record.account,
            thinking: record.thinking,
        })
        .collect()
}

/// Builds (or reuses) the local part of the dashboard: services, jobs,
/// config and usage. Everything here is local state, so it is
/// cached for [`MONITOR_CACHE_TTL`] and never waits on the network.
///
/// Quotas are deliberately *not* cached with it — they are overlaid per request
/// by [`attach_subscriptions`], so a refresh that lands mid-window is served
/// immediately instead of waiting for this cache to expire.
async fn monitor_snapshot(state: &Arc<ServerState>) -> Result<MonitorResponse, ApiError> {
    let root = state.root.clone();
    cached_monitor_or_build(state, move || build_monitor_response(&root)).await
}

async fn cached_monitor_or_build(
    state: &ServerState,
    build: impl FnOnce() -> anyhow::Result<MonitorResponse> + Send + 'static,
) -> Result<MonitorResponse, ApiError> {
    if let Some(cached) = fresh_monitor(state).await {
        return Ok(cached);
    }
    // Single-flight: the Mini App opens several views on `/api/monitor` at
    // once, and each build may sync the usage cache. Later callers wait for
    // the first build and reuse it.
    let _build = state.monitor_build.lock().await;
    if let Some(cached) = fresh_monitor(state).await {
        return Ok(cached);
    }

    let response = tokio::task::spawn_blocking(build)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App monitor snapshot task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Monitor snapshot unavailable",
            )
        })?
        .map_err(|error| {
            tracing::warn!(error = %format!("{error:#}"), "Failed to build Mini App monitor snapshot");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Monitor snapshot unavailable",
            )
        })?;

    *state.monitor_cache.write().await = Some(CachedMonitor {
        cached_at: Instant::now(),
        response: response.clone(),
    });
    Ok(response)
}

async fn fresh_monitor(state: &ServerState) -> Option<MonitorResponse> {
    state
        .monitor_cache
        .read()
        .await
        .as_ref()
        .filter(|cached| cached.cached_at.elapsed() < MONITOR_CACHE_TTL)
        .map(|cached| cached.response.clone())
}

fn build_monitor_response(root: &FilePath) -> anyhow::Result<MonitorResponse> {
    let config = Config::load_layered(&paths::config_layer_paths_for(root))?;
    let services = Service::ALL
        .into_iter()
        .map(|service| {
            let state = service::state(service);
            MonitorService {
                name: service.name().to_string(),
                running: state.running(),
                installed: state.installed,
                pid: state.pid,
                uptime: state.uptime.map(service::format_uptime),
            }
        })
        .collect();

    let background_processes = background_activity::list_background()
        .into_iter()
        .filter(background_activity::BackgroundProcess::is_running)
        .map(|process| {
            let uptime = process.uptime();
            MonitorBackgroundProcess {
                id: process.bg_id,
                pid: process.pid,
                thread_id: process.thread_id,
                command: process.command,
                uptime,
            }
        })
        .collect();

    let (jobs, jobs_error) = match jobs::summaries(&jobs::JobsHome::current(), chrono::Utc::now()) {
        Ok(jobs) => (jobs, None),
        Err(error) => (Vec::new(), Some(format!("{error:#}"))),
    };

    let usage = usage_stats::aggregate_usage(
        &config.model,
        Some(usage_stats::today_utc().saturating_sub(29)),
    )
    .map(monitor_usage)
    .map_err(|error| {
        tracing::warn!(error = %format!("{error:#}"), "Mini App monitor usage aggregation failed");
        error
    })
    .ok();

    Ok(MonitorResponse {
        generated_at: chrono::Utc::now().to_rfc3339(),
        services,
        // Overlaid per request by `get_monitor`: run markers are cheap to read
        // and go stale long before the rest of the snapshot.
        active_agents: Vec::new(),
        background_processes,
        jobs,
        jobs_error,
        config: monitor_config(&config),
        usage,
        subscriptions: Vec::new(),
        subscriptions_status: "pending",
        subscriptions_age: None,
    })
}

/// Overlays the last known quota snapshot onto a dashboard response and, when
/// it is missing or past [`SUBSCRIPTION_CACHE_TTL`], starts a refresh in the
/// background.
///
/// This never awaits a provider call. A quota fetch talks to every stored OAuth
/// account with a 10s per-request timeout and no shared deadline, so awaiting it
/// here made a cold dashboard open stall for as long as the slowest provider —
/// intermittently, since a warm cache hid it. The client polls every 30s and
/// picks the values up on a later tick; `subscriptions_status` says which of
/// "not fetched yet" and "no subscriptions" an empty list means.
async fn attach_subscriptions(state: &Arc<ServerState>, response: &mut MonitorResponse) {
    let cached = state
        .subscription_cache
        .lock()
        .await
        .as_ref()
        .map(|cached| (cached.subscriptions.clone(), cached.cached_at.elapsed()));

    let (status, refresh) = classify_subscriptions(cached.as_ref().map(|(_, age)| *age));
    if let Some((subscriptions, age)) = cached {
        response.subscriptions = subscriptions;
        response.subscriptions_age = Some(service::format_uptime(age));
    }
    response.subscriptions_status = status;
    if refresh {
        spawn_subscription_refresh(state);
    }
}

/// Maps the age of the cached quota snapshot to the status reported to the
/// client and whether a background refresh is due.
///
/// `None` is "never fetched since startup", which must stay distinguishable
/// from "fetched, and there are no subscriptions" — both are an empty list.
fn classify_subscriptions(age: Option<Duration>) -> (&'static str, bool) {
    match age {
        None => ("pending", true),
        Some(age) if age < SUBSCRIPTION_CACHE_TTL => ("ready", false),
        Some(_) => ("stale", true),
    }
}

/// Starts one background quota refresh unless one is already running.
///
/// The guard is an owned mutex guard rather than a flag so it is released even
/// if the fetch panics or the task is cancelled; a stuck flag would freeze
/// quotas for the life of the process.
fn spawn_subscription_refresh(state: &Arc<ServerState>) {
    let Ok(guard) = Arc::clone(&state.subscription_refresh).try_lock_owned() else {
        return;
    };
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let _guard = guard;
        let started = Instant::now();
        let subscriptions = fetch_subscription_quotas().await;
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            count = subscriptions.len(),
            "Refreshed Mini App subscription quotas"
        );
        *state.subscription_cache.lock().await = Some(CachedSubscriptions {
            cached_at: Instant::now(),
            subscriptions,
        });
    });
}

/// Fetches live quotas for every stored account, sorted by display name.
///
/// A provider failure stays attached to its account, and a snapshot error
/// yields an empty list rather than failing the caller.
async fn fetch_subscription_quotas() -> Vec<MonitorSubscription> {
    let snapshot = match subscription_quota::fetch_snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "Failed to fetch subscription quota snapshot");
            subscription_quota::SubscriptionQuotaSnapshot {
                providers: Vec::new(),
            }
        }
    };
    let mut subscriptions: Vec<MonitorSubscription> = snapshot
        .providers
        .into_iter()
        .filter_map(|result| {
            let name =
                subscription_quota::account_display(result.provider, result.account.as_deref());
            monitor_subscription(result.provider, name, result.quota)
        })
        .collect();
    subscriptions.sort_by(|a, b| a.name.cmp(&b.name));
    subscriptions
}

fn monitor_subscription(
    provider: &str,
    name: String,
    result: Result<SubscriptionQuota, QuotaError>,
) -> Option<MonitorSubscription> {
    match result {
        Ok(quota) => Some(MonitorSubscription {
            provider: provider.to_string(),
            name,
            plan: quota.plan,
            windows: quota
                .windows
                .into_iter()
                .map(|window| MonitorQuotaWindow {
                    label: window.label,
                    used_percent: if window.used_percent.is_finite() {
                        window.used_percent.clamp(0.0, 100.0)
                    } else {
                        0.0
                    },
                    resets_at: window.resets_at.map(|reset| reset.to_rfc3339()),
                    scope: window.scope,
                })
                .collect(),
            error: None,
        }),
        Err(QuotaError::NotAuthenticated) => None,
        Err(error) => Some(MonitorSubscription {
            provider: provider.to_string(),
            name,
            plan: None,
            windows: Vec::new(),
            error: Some(error.reason()),
        }),
    }
}

fn monitor_config(config: &Config) -> MonitorConfig {
    let server = config.telegram.server.as_ref();
    MonitorConfig {
        model: config.model.clone(),
        thinking: config.thinking_level.display_name().to_string(),
        max_tokens: config.max_tokens,
        subagents_enabled: config.subagents.enabled,
        mode_count: config.model_modes.len(),
        helper_models: vec![
            MonitorConfigModel {
                role: "title",
                model: config.title_model.clone(),
            },
            MonitorConfigModel {
                role: "tldr",
                model: config.tldr_model.clone(),
            },
            MonitorConfigModel {
                role: "handoff",
                model: config.handoff_model.clone(),
            },
            MonitorConfigModel {
                role: "prompt builder",
                model: config.prompt_builder_model.clone(),
            },
            MonitorConfigModel {
                role: "read thread",
                model: config.read_thread_model.clone(),
            },
        ],
        server_enabled: server.is_some_and(|server| server.enabled),
        server_port: server.map_or(4141, |server| server.port),
    }
}

fn monitor_usage(stats: UsageStats) -> MonitorUsage {
    let row = |item: zdx_engine::core::usage_stats::UsageRow| {
        let tokens = item.tokens();
        MonitorUsageRow {
            provider: item.provider,
            model: item.model,
            requests: item.requests,
            tokens,
            cost_usd: item.cost_usd,
            subscription: item.subscription,
            estimated: item.estimated,
        }
    };
    MonitorUsage {
        span: "30 days",
        requests: stats.totals.requests,
        tokens: stats.totals.tokens(),
        input: stats.totals.input,
        output: stats.totals.output,
        cache_read: stats.totals.cache_read,
        cache_write: stats.totals.cache_write,
        billed_usd: stats.totals.billed_usd,
        subscription_tokens: stats.totals.subscription_tokens,
        unknown_pricing_rows: stats.totals.unknown_pricing_rows,
        threads_scanned: stats.threads_scanned,
        by_provider: stats.by_provider.into_iter().map(row).collect(),
        by_model: stats.by_model.into_iter().take(8).map(row).collect(),
        daily: stats
            .daily
            .into_iter()
            .map(|day| MonitorDailyUsage {
                day: day.day,
                tokens: day.tokens,
            })
            .collect(),
    }
}

/// Spawns the embedded server on a background tokio task.
pub(crate) fn spawn_server(
    bot_token: String,
    allowlist_user_ids: HashSet<i64>,
    port: u16,
    root: PathBuf,
    workers: Arc<WorkerManager>,
    deliveries: ActiveDeliveries,
    feedback: Arc<dyn FeedbackSink>,
) {
    let state = Arc::new(
        ServerState::new(bot_token, allowlist_user_ids, root, workers)
            .with_deliveries(deliveries)
            .with_feedback(feedback),
    );
    let app = create_router(state);

    tokio::spawn(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        tracing::info!("ZDX Mini App server listening on http://{}", addr);

        if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
            let _ = axum::serve(listener, app).await;
        } else {
            tracing::error!("Failed to bind ZDX Mini App server to port {}", port);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delivery_outcome_for_a_seen_record_forces_a_full_poll() {
        let deliveries = vec![
            (4, "a".to_string()),
            (9, "a".to_string()),
            (12, "b".to_string()),
        ];
        // The outcome of `a` lands after the cursor; the client holds its
        // pending record.
        assert!(super::replaces_seen_delivery(&deliveries, 6));
        // Both already seen, or both new: a delta is enough.
        assert!(!super::replaces_seen_delivery(&deliveries, 10));
        assert!(!super::replaces_seen_delivery(&deliveries, 0));
    }

    #[test]
    fn worker_rollup_keeps_failed_out_of_settled() {
        let rollup = worker_rollup(
            Some(&[
                WorkerStatus::Running,
                WorkerStatus::Queued,
                WorkerStatus::Failed,
                WorkerStatus::Completed,
                WorkerStatus::Cancelled,
            ]),
            5,
        )
        .expect("workers present");

        assert_eq!(rollup.total, 5);
        assert_eq!(rollup.running, 1);
        assert_eq!(rollup.queued, 1);
        assert_eq!(rollup.failed, 1);
        // Completed + cancelled only; a failure must stay visible on its own.
        assert_eq!(rollup.settled, 2);
        assert_eq!(rollup.unknown, 0);
    }

    #[test]
    fn worker_rollup_reports_lineage_only_workers_as_unknown() {
        let rollup = worker_rollup(None, 3).expect("persisted children present");
        assert_eq!(rollup.total, 3);
        assert_eq!(rollup.unknown, 3);
        assert_eq!(rollup.settled, 0);
        assert_eq!(rollup.failed, 0);
    }

    #[test]
    fn worker_rollup_trusts_live_state_when_the_index_lags_a_new_worker() {
        // The index syncs on a freshness window, so a just-created worker can be
        // live while the persisted count is still behind.
        let rollup = worker_rollup(Some(&[WorkerStatus::Running]), 0).expect("live worker");
        assert_eq!(rollup.total, 1);
        assert_eq!(rollup.running, 1);
        assert_eq!(rollup.unknown, 0);
    }

    #[test]
    fn worker_rollup_is_absent_for_a_thread_with_no_workers() {
        assert!(worker_rollup(None, 0).is_none());
        assert!(worker_rollup(Some(&[]), 0).is_none());
    }

    #[test]
    fn unfetched_quotas_are_pending_rather_than_an_empty_result() {
        // The dashboard must never wait on a provider call, so an empty list
        // before the first refresh lands has to stay distinguishable from
        // "this user has no subscriptions".
        assert_eq!(classify_subscriptions(None), ("pending", true));
        assert_eq!(
            classify_subscriptions(Some(SUBSCRIPTION_CACHE_TTL / 2)),
            ("ready", false)
        );
        assert_eq!(
            classify_subscriptions(Some(SUBSCRIPTION_CACHE_TTL * 2)),
            ("stale", true)
        );
    }

    const SAMPLE_BOT_TOKEN: &str = "1234567890:TEST-BOT-TOKEN-FOR-UNIT-TESTS-ONLY";
    const SAMPLE_AUTH_DATE: u64 = 1_662_771_648;
    const SAMPLE_INIT_DATA: &str = concat!(
        "query_id=AAHdF6IQAAAAAN0XohDhrOrc&",
        "user=%7B%22id%22%3A279058397%2C%22first_name%22%3A%22Vladislav%22%2C",
        "%22last_name%22%3A%22Kibenko%22%2C%22username%22%3A%22vdkfrost%22%2C",
        "%22language_code%22%3A%22ru%22%2C%22is_premium%22%3Atrue%7D&",
        "auth_date=1662771648&",
        "hash=c4d45b6d00cd75400ade8589bba0ac122375ae8594ecc3495de5fb63c3697c17"
    );

    fn sample_allowlist() -> HashSet<i64> {
        HashSet::from([279_058_397])
    }

    /// Builds `initData` the middleware accepts right now, so a route can be
    /// exercised end to end instead of only its 401 path.
    fn fresh_init_data(user_id: i64) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs()
            .to_string();
        let user = format!("{{\"id\":{user_id}}}");
        let data_check_string = format!("auth_date={now}\nuser={user}");

        let mut secret_mac = HmacSha256::new_from_slice(b"WebAppData").expect("hmac key");
        secret_mac.update(SAMPLE_BOT_TOKEN.as_bytes());
        let secret_key = secret_mac.finalize().into_bytes();
        let mut data_mac = HmacSha256::new_from_slice(&secret_key).expect("hmac key");
        data_mac.update(data_check_string.as_bytes());
        let hash = data_mac
            .finalize()
            .into_bytes()
            .iter()
            .fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            });

        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("auth_date", &now)
            .append_pair("user", &user)
            .append_pair("hash", &hash)
            .finish()
    }

    /// Pulls the first content-hashed script URL out of the served shell.
    fn first_script_asset(html: &str) -> String {
        html.split(['"', '\''])
            .find(|s| {
                s.starts_with("/app/assets/")
                    && FilePath::new(s)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
            })
            .expect("shell references a hashed script")
            .to_string()
    }

    #[tokio::test]
    async fn protects_every_api_route_but_not_the_miniapp_page() {
        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            WorkerManager::new().0,
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();

        for path in [
            "/api/threads",
            "/api/threads/active",
            "/api/threads/active/trajectory",
            "/api/threads/active/artifacts",
            "/api/threads/active/artifacts/file?path=x",
            "/api/monitor",
            "/api/workspaces",
            "/api/git?thread_id=active",
            "/api/git/scope?thread_id=active&scope=all",
            "/api/git/diff?thread_id=active&kind=unstaged&path=README.md",
        ] {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .expect("request protected API route");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        for path in ["/app", "/threads", "/monitor"] {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .expect("request public Mini App page");
            assert_eq!(response.status(), StatusCode::OK);
        }

        server.abort();
    }

    /// Exercises the artifact routes over HTTP with accepted `initData`: the
    /// list must answer once with the thread's files, a listed file must be
    /// downloadable, and a path the thread never produced must be refused.
    #[tokio::test]
    async fn serves_thread_artifacts_and_refuses_foreign_paths() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let thread_id = format!("telegram-artifacts-{}", uuid::Uuid::new_v4());
        let dir = paths::artifact_dir_for_thread(Some(&thread_id));
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        std::fs::write(dir.join("report.html"), "<h1>report</h1>").expect("write artifact");

        let mut thread = thread_persistence::Thread::with_id(thread_id.clone()).expect("thread");
        thread
            .append(&ThreadEvent::assistant_message(format!(
                "attached <media>{}</media>",
                dir.join("report.html").display()
            )))
            .expect("append media message");

        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            WorkerManager::new().0,
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));

        let list = client
            .get(format!("http://{addr}/api/threads/{thread_id}/artifacts"))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request artifact list");
        assert_eq!(list.status(), StatusCode::OK);
        let body = list.text().await.expect("read artifact list");
        assert!(body.contains("report.html"), "{body}");
        assert!(body.contains("\"kind\":\"html\""), "{body}");
        assert!(body.contains("\"source\":\"both\""), "{body}");

        let file = client
            .get(format!(
                "http://{addr}/api/threads/{thread_id}/artifacts/file?path=report.html"
            ))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request artifact file");
        assert_eq!(file.status(), StatusCode::OK);
        assert_eq!(
            file.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html")
        );
        assert_eq!(file.text().await.expect("read artifact"), "<h1>report</h1>");

        let foreign = client
            .get(format!(
                "http://{addr}/api/threads/{thread_id}/artifacts/file?path=/etc/hosts"
            ))
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .expect("request foreign path");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

        server.abort();
        drop(home);
    }

    /// The version route answers the SHA-256 of the file bytes (the value
    /// feedback names), follows a change even when the size stays the same,
    /// and keeps the file route's login and path rules.
    #[tokio::test]
    async fn artifact_version_follows_the_file_bytes() {
        use sha2::Digest as _;

        let home = zdx_engine::test_support::temp_zdx_home();
        let thread_id = format!("telegram-version-{}", uuid::Uuid::new_v4());
        let dir = paths::artifact_dir_for_thread(Some(&thread_id));
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        let file = dir.join("report.html");
        std::fs::write(&file, "<h1>one</h1>").expect("write artifact");

        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            WorkerManager::new().0,
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let version = |path: &str| {
            client
                .get(format!(
                    "http://{addr}/api/threads/{thread_id}/artifacts/version?path={path}"
                ))
                .header(AUTHORIZATION, &auth)
                .send()
        };
        let sha_of = |bytes: &[u8]| {
            sha2::Sha256::digest(bytes)
                .iter()
                .fold(String::new(), |mut out, byte| {
                    use std::fmt::Write as _;
                    let _ = write!(out, "{byte:02x}");
                    out
                })
        };
        let read = |response: reqwest::Response| async move {
            assert_eq!(response.status(), StatusCode::OK);
            response.json::<Value>().await.expect("version json")
        };

        let first = read(version("report.html").await.expect("version")).await;
        assert_eq!(first["sha256"], sha_of(b"<h1>one</h1>"));
        assert_eq!(first["size_bytes"], 12);
        assert!(first["modified_at"].is_string());
        // Cached answer, unchanged file.
        let again = read(version("report.html").await.expect("version")).await;
        assert_eq!(again["sha256"], first["sha256"]);

        // Same length, new bytes, a later modified time.
        std::fs::write(&file, "<h1>two</h1>").expect("rewrite artifact");
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|handle| handle.set_modified(later))
            .expect("set mtime");
        let changed = read(version("report.html").await.expect("version")).await;
        assert_eq!(changed["sha256"], sha_of(b"<h1>two</h1>"));

        let foreign = version("/etc/hosts").await.expect("foreign");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        let anonymous = reqwest::Client::new()
            .get(format!(
                "http://{addr}/api/threads/{thread_id}/artifacts/version?path=report.html"
            ))
            .send()
            .await
            .expect("anonymous");
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        server.abort();
        drop(home);
    }

    /// Records deliveries instead of queueing a turn, so feedback tests never
    /// reach a real chat or worker.
    struct RecordingSink {
        allow: bool,
        fail_next: std::sync::atomic::AtomicBool,
        deliveries: std::sync::Mutex<Vec<FeedbackDelivery>>,
    }

    impl RecordingSink {
        fn new(allow: bool) -> Arc<Self> {
            Arc::new(Self {
                allow,
                fail_next: std::sync::atomic::AtomicBool::new(false),
                deliveries: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn count(&self) -> usize {
            self.deliveries.lock().unwrap().len()
        }
    }

    impl FeedbackSink for RecordingSink {
        fn allows(&self, _user_id: i64, _route: FeedbackRoute) -> bool {
            self.allow
        }

        fn deliver(
            &self,
            delivery: FeedbackDelivery,
        ) -> crate::artifact_feedback::DeliverFuture<'_> {
            if self
                .fail_next
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Box::pin(async { Err(anyhow::anyhow!("queue refused")) });
            }
            if let Err(err) = artifact_feedback::mark_queued(&delivery.bundle_dir) {
                return Box::pin(async move { Err(err) });
            }
            self.deliveries.lock().unwrap().push(delivery);
            Box::pin(async { Ok(()) })
        }
    }

    const FEEDBACK_PAGE: &str = "<html><body><h1>Example</h1></body></html>";
    /// Smallest valid PNG (1×1, transparent).
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn feedback_body(submission_id: &str, sha: &str) -> serde_json::Value {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD.encode(PNG_1X1);
        serde_json::json!({
            "submission_id": submission_id,
            "artifact_path": "report.html",
            "artifact_sha256": sha,
            "annotations": [
                {
                    "intent": "change",
                    "comment": "Make the heading larger",
                    "target": {
                        "kind": "element",
                        "element": {
                            "selector": "body > h1",
                            "tag": "h1",
                            "text": "Example",
                            "rect": { "x": 8.0, "y": 21.0, "width": 300.0, "height": 37.0 },
                        },
                    },
                },
                {
                    "intent": "keep",
                    "target": {
                        "kind": "drawing",
                        "bounds": { "x": 0.0, "y": 0.0, "width": 40.0, "height": 40.0 },
                        "elements": [],
                        "sketch": { "kind": "image", "mime": "image/png", "data": png },
                    },
                },
            ],
        })
    }

    fn sha_of(text: &str) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(text.as_bytes())
            .iter()
            .fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            })
    }

    fn new_id() -> String {
        format!("sub-{}", uuid::Uuid::new_v4())
    }

    fn write_feedback_thread(thread_id: &str) {
        let dir = paths::artifact_dir_for_thread(Some(thread_id));
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        std::fs::write(dir.join("report.html"), FEEDBACK_PAGE).expect("write artifact");
        thread_persistence::Thread::with_id(thread_id.to_string())
            .expect("thread")
            .append(&ThreadEvent::assistant_message("made a report"))
            .expect("append");
    }

    async fn spawn_feedback_app(
        sink: Arc<RecordingSink>,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = create_router(Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                WorkerManager::new().0,
            )
            .with_feedback(sink),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        (addr, server)
    }

    struct FeedbackClient {
        client: reqwest::Client,
        addr: SocketAddr,
        auth: String,
    }

    impl FeedbackClient {
        async fn post(&self, thread: &str, body: &serde_json::Value) -> (StatusCode, String) {
            let response = self
                .client
                .post(format!(
                    "http://{}/api/threads/{thread}/artifacts/feedback",
                    self.addr
                ))
                .header(AUTHORIZATION, &self.auth)
                .json(body)
                .send()
                .await
                .expect("post feedback");
            let status = response.status();
            (status, response.text().await.unwrap_or_default())
        }
    }

    /// Feedback over HTTP: authenticated, routed to the thread's own topic,
    /// queued once per submission, a failed queue retried, and refused without
    /// side effects for a reused id, a changed file, `active`, a foreign path,
    /// a thread with no chat, or a chat the sender may not use.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted request sequence
    async fn queues_artifact_feedback_once_into_the_threads_topic() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let sha = sha_of(FEEDBACK_PAGE);

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let submission_id = new_id();

        let unauthenticated = api
            .client
            .post(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
            ))
            .json(&feedback_body(&submission_id, &sha))
            .send()
            .await
            .expect("post without auth");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        sink.fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (status, _) = api
            .post(topic_thread, &feedback_body(&submission_id, &sha))
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(sink.count(), 0);

        // The retry after a failure queues; it is not a duplicate.
        let (status, body) = api
            .post(topic_thread, &feedback_body(&submission_id, &sha))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.contains("\"queued\""), "{body}");

        let (status, body) = api
            .post(topic_thread, &feedback_body(&submission_id, &sha))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"duplicate\""), "{body}");
        assert_eq!(sink.count(), 1, "a retry must not queue a second turn");

        // The same id with different content is refused, not deduplicated.
        let mut changed = feedback_body(&submission_id, &sha);
        changed["note"] = serde_json::json!("something else");
        let (status, _) = api.post(topic_thread, &changed).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(sink.count(), 1);

        {
            let deliveries = sink.deliveries.lock().unwrap();
            let delivery = &deliveries[0];
            assert_eq!(
                delivery.route,
                FeedbackRoute {
                    chat_id: -100_777,
                    topic_id: Some(9)
                }
            );
            assert_eq!(delivery.user_id, 279_058_397);
            assert!(delivery.prompt.contains("Make the heading larger"));
            assert!(delivery.prompt.contains("note-2-drawing.png"));
            assert!(matches!(
                delivery.attachments.as_slice(),
                [LocalAttachment::Image { path, label }]
                    if path.is_file() && label == "Drawing sketch for note 2"
            ));
        }

        // A changed file needs the user's explicit "send anyway".
        let stale_id = new_id();
        let mut stale = feedback_body(&stale_id, &"0".repeat(64));
        let (status, body) = api.post(topic_thread, &stale).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert!(body.contains("changed"), "{body}");
        stale["allow_stale"] = serde_json::json!(true);
        let (status, body) = api.post(topic_thread, &stale).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.contains("\"stale\":true"), "{body}");
        assert_eq!(sink.count(), 2);

        let mut foreign = feedback_body(&new_id(), &sha);
        foreign["artifact_path"] = serde_json::json!("/etc/hosts");
        let (status, _) = api.post(topic_thread, &foreign).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = api.post("active", &feedback_body(&new_id(), &sha)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = api
            .post("cli-thread-1", &feedback_body(&new_id(), &sha))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(sink.count(), 2);
        server.abort();

        let refusing = RecordingSink::new(false);
        let (addr, server) = spawn_feedback_app(Arc::clone(&refusing)).await;
        let api = FeedbackClient { addr, ..api };
        let refused_id = new_id();
        let (status, _) = api
            .post(topic_thread, &feedback_body(&refused_id, &sha))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(refusing.count(), 0);
        assert!(
            !artifact_feedback::feedback_dir_for_thread(topic_thread)
                .join(&refused_id)
                .exists(),
            "a refused submission saves nothing"
        );

        let oversized = api
            .client
            .post(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
            ))
            .header(AUTHORIZATION, &api.auth)
            .header(header::CONTENT_TYPE, "application/json")
            .body(vec![b' '; artifact_feedback::MAX_SUBMISSION_BYTES + 1])
            .send()
            .await
            .expect("post oversized body");
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

        server.abort();
        drop(home);
    }

    /// A text note whose quote and context run to the client's limits with
    /// an emoji at each edge is queued. The same note with half an emoji (a
    /// lone surrogate escape, what a plain UTF-16 cut produced) is refused by
    /// the JSON extractor before anything is saved, which is why the Mini App
    /// cuts on code points and repairs page strings (`lib/annotate.ts`).
    #[tokio::test]
    async fn text_notes_with_emoji_edges_are_queued_and_lone_surrogates_refused() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-11";
        write_feedback_thread(topic_thread);
        let sha = sha_of(FEEDBACK_PAGE);
        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let rockets = "x🚀".repeat(1000);
        let units = |text: &str| text.encode_utf16().count();
        // What the Mini App sends at its limits: whole emoji only.
        let quote: String = rockets.chars().take(1332).collect();
        let prefix: String = rockets.chars().skip(rockets.chars().count() - 42).collect();
        let suffix: String = rockets.chars().take(42).collect();
        assert!(units(&quote) <= 2000 && units(&prefix) <= 64 && units(&suffix) <= 64);
        assert!(prefix.ends_with('🚀') && suffix.ends_with('🚀'));
        let body = |id: &str, prefix: &str| {
            serde_json::json!({
                "submission_id": id,
                "artifact_path": "report.html",
                "artifact_sha256": sha,
                "annotations": [{
                    "intent": "question",
                    "comment": "Why 🚀?",
                    "target": {
                        "kind": "text",
                        "quote": quote,
                        "prefix": prefix,
                        "suffix": suffix,
                        "element": {
                            "selector": "body > h1",
                            "tag": "h1",
                            "text": rockets.chars().take(600).collect::<String>(),
                            "rect": { "x": 8.0, "y": 21.0, "width": 300.0, "height": 37.0 },
                        },
                        "rect": { "x": 9.0, "y": 22.0, "width": 30.0, "height": 12.0 },
                    },
                }],
            })
        };

        let (status, response) = api.post(topic_thread, &body(&new_id(), &prefix)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{response}");
        assert_eq!(sink.count(), 1);
        assert!(
            sink.deliveries.lock().unwrap()[0]
                .prompt
                .contains("Text just before it")
        );

        let lone_id = new_id();
        let lone = serde_json::to_string(&body(&lone_id, "LONE"))
            .expect("json")
            .replace("\"LONE\"", "\"\\ude80x🚀\"");
        let refused = api
            .client
            .post(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
            ))
            .header(AUTHORIZATION, &api.auth)
            .header(header::CONTENT_TYPE, "application/json")
            .body(lone)
            .send()
            .await
            .expect("post lone surrogate");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(sink.count(), 1);
        assert!(
            !artifact_feedback::feedback_dir_for_thread(topic_thread)
                .join(&lone_id)
                .exists()
        );

        server.abort();
        drop(home);
    }

    /// Holds every save inside `allows` (which runs in the blocking save
    /// task) until the test releases it.
    struct GatedSink {
        entered: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl FeedbackSink for GatedSink {
        fn allows(&self, _user_id: i64, _route: FeedbackRoute) -> bool {
            let _ = self.entered.lock().unwrap().send(());
            let _ = self.release.lock().unwrap().recv();
            true
        }

        fn deliver(
            &self,
            _delivery: FeedbackDelivery,
        ) -> crate::artifact_feedback::DeliverFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    /// The upload slot stays taken while the blocking save runs, even after
    /// the client hung up: the decode and render keep running, so freeing the
    /// slot on disconnect would let more of them pile up than the cap allows.
    #[tokio::test]
    async fn feedback_slot_is_held_until_the_save_finishes_after_a_disconnect() {
        use tokio::io::AsyncWriteExt as _;

        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let state = Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                WorkerManager::new().0,
            )
            .with_feedback(Arc::new(GatedSink {
                entered: std::sync::Mutex::new(entered_tx),
                release: std::sync::Mutex::new(release_rx),
            })),
        );
        let permits = Arc::clone(&state.feedback_permits);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let app = create_router(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });

        let body = feedback_body(&new_id(), &sha_of(FEEDBACK_PAGE)).to_string();
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let request = format!(
            "POST /api/threads/{topic_thread}/artifacts/feedback HTTP/1.1\r\nHost: test\r\nAuthorization: tma {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            fresh_init_data(279_058_397),
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("send request");
        tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(10)))
            .await
            .expect("join")
            .expect("save started");
        assert_eq!(permits.available_permits(), FEEDBACK_CONCURRENCY - 1);

        // Hang up mid-save and give the server time to drop the request.
        drop(stream);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            permits.available_permits(),
            FEEDBACK_CONCURRENCY - 1,
            "the slot is held while the save still runs"
        );

        release_tx.send(()).expect("release the save");
        let deadline = Instant::now() + Duration::from_secs(10);
        while permits.available_permits() < FEEDBACK_CONCURRENCY {
            assert!(Instant::now() < deadline, "slot freed once the save ends");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        server.abort();
        drop(home);
    }

    /// The image route serves the canonical copy (bounded, upright) with its
    /// identity in headers, only for the thread's artifacts and only with
    /// auth; feedback naming that copy is accepted and a different frame is
    /// refused; formats that cannot be annotated answer `422` with a reason.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted request sequence
    async fn image_route_serves_the_canonical_copy_feedback_refers_to() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let dir = paths::artifact_dir_for_thread(Some(topic_thread));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2000,
            1000,
            image::Rgb([10, 80, 10]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        std::fs::write(dir.join("chart.png"), &png).unwrap();
        std::fs::write(dir.join("anim.gif"), b"GIF89a\x01\0\x01\0\0\0\0;").unwrap();
        // One byte over the annotation limit (the PNG is padded at the end).
        let mut over = png.clone();
        over.resize(
            usize::try_from(artifact_feedback::canonical::MAX_SOURCE_BYTES + 1).unwrap(),
            0,
        );
        std::fs::write(dir.join("over.png"), over).unwrap();

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let get = |path: &str, auth: bool| {
            let request = api.client.get(format!(
                "http://{addr}/api/threads/{topic_thread}/artifacts/image?path={path}"
            ));
            if auth {
                request.header(AUTHORIZATION, &api.auth)
            } else {
                request
            }
        };

        let unauthenticated = get("chart.png", false).send().await.expect("get");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let foreign = get("/etc/hosts", true).send().await.expect("get");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        let gif = get("anim.gif", true).send().await.expect("get");
        assert_eq!(gif.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(gif.text().await.unwrap().contains("not GIF files"));
        let over = get("over.png", true).send().await.expect("get");
        assert_eq!(over.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(over.text().await.unwrap().contains("larger than 25 MiB"));

        let response = get("chart.png", true).send().await.expect("get");
        assert_eq!(response.status(), StatusCode::OK);
        let header = |name: &str| {
            response.headers()[name]
                .to_str()
                .expect("header")
                .to_string()
        };
        assert_eq!(header("content-type"), "image/jpeg");
        assert_eq!(header("x-zdx-source-sha256"), sha_of_bytes(&png));
        let (canonical, width, height) = (
            header("x-zdx-canonical-sha256"),
            header("x-zdx-canonical-width"),
            header("x-zdx-canonical-height"),
        );
        assert_eq!((width.as_str(), height.as_str()), ("1568", "784"));
        assert_eq!(
            (header("x-zdx-source-width"), header("x-zdx-source-height")),
            ("2000".to_string(), "1000".to_string())
        );
        let bytes = response.bytes().await.expect("body");
        assert_eq!(
            sha_of_bytes(&bytes),
            canonical,
            "the header names these bytes"
        );
        assert_eq!(
            std::fs::read(dir.join("chart.png")).unwrap(),
            png,
            "original kept"
        );

        let body = |frame: serde_json::Value| {
            serde_json::json!({
                "submission_id": new_id(),
                "artifact_path": "chart.png",
                "artifact_sha256": sha_of_bytes(&png),
                "image": frame,
                "annotations": [
                    { "intent": "change", "comment": "Bigger", "target": { "kind": "image_pin", "point": { "x": 0.5, "y": 0.5 } } },
                ],
            })
        };
        let (status, text) = api
            .post(
                topic_thread,
                &body(serde_json::json!({ "canonical_sha256": canonical, "width": 2000, "height": 1000 })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(text.contains("different preview"), "{text}");
        let (status, text) = api
            .post(
                topic_thread,
                &body(serde_json::json!({ "canonical_sha256": canonical, "width": 1568, "height": 784 })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{text}");
        assert_eq!(sink.count(), 1);
        server.abort();
        drop(home);
    }

    fn sha_of_bytes(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            })
    }

    /// Previews and feedback saves share the engine's normalization limit, a
    /// slot outlives a client that hangs up mid-decode, and a waiter proceeds
    /// once a slot frees: no deadlock with the request permits.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted sequence
    async fn previews_and_feedback_share_the_normalization_limit() {
        use tokio::io::AsyncWriteExt as _;
        use zdx_engine::core::artifact_feedback::canonical::{
            self, MAX_NORMALIZATIONS, normalizations_running, test_hooks,
        };

        let home = zdx_engine::test_support::temp_zdx_home();
        let topic_thread = "telegram--100777-topic-9";
        write_feedback_thread(topic_thread);
        let dir = paths::artifact_dir_for_thread(Some(topic_thread));
        let mut shas = Vec::new();
        for (name, shade) in [("a.png", 10u8), ("b.png", 90), ("c.png", 170)] {
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                300,
                200,
                image::Rgb([shade, 40, 40]),
            ))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
            std::fs::write(dir.join(name), &png).unwrap();
            shas.push(sha_of_bytes(&png));
        }
        let pause_a = test_hooks::pause(&shas[0]);
        let pause_b = test_hooks::pause(&shas[1]);

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let wait_until = |what: &'static str, done: Box<dyn Fn() -> bool + Send>| async move {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting: {what}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let cached = |sha: &str| canonical::cached(sha).is_some();

        // A preview of a.png from a client that will hang up mid-decode.
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(
                format!(
                    "GET /api/threads/{topic_thread}/artifacts/image?path=a.png HTTP/1.1\r\nHost: test\r\nAuthorization: {auth}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("send request");
        // A feedback save on b.png: it normalizes the original too.
        let client = reqwest::Client::new();
        let post_body = serde_json::json!({
            "submission_id": new_id(),
            "artifact_path": "b.png",
            "artifact_sha256": shas[1],
            "image": { "canonical_sha256": "a".repeat(64), "width": 1, "height": 1 },
            "annotations": [
                { "intent": "change", "comment": "x", "target": { "kind": "image_pin", "point": { "x": 0.5, "y": 0.5 } } },
            ],
        });
        let post = tokio::spawn({
            let client = client.clone();
            let auth = auth.clone();
            async move {
                client
                    .post(format!(
                        "http://{addr}/api/threads/{topic_thread}/artifacts/feedback"
                    ))
                    .header(AUTHORIZATION, auth)
                    .json(&post_body)
                    .send()
                    .await
                    .expect("post")
                    .status()
            }
        });
        wait_until(
            "both slots taken by the paused preview and save",
            Box::new(|| normalizations_running() == MAX_NORMALIZATIONS),
        )
        .await;

        // A third normalization (an unpaused preview) has to wait.
        let third = tokio::spawn({
            let client = client.clone();
            let auth = auth.clone();
            async move {
                client
                    .get(format!(
                        "http://{addr}/api/threads/{topic_thread}/artifacts/image?path=c.png"
                    ))
                    .header(AUTHORIZATION, auth)
                    .send()
                    .await
                    .expect("get c")
                    .status()
            }
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !third.is_finished(),
            "a third normalization waits for a slot"
        );
        assert!(!cached(&shas[2]));

        // The preview's client hangs up: its decode keeps its slot.
        drop(stream);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(normalizations_running(), MAX_NORMALIZATIONS);
        assert!(!third.is_finished(), "the disconnect freed no slot");

        // a.png finishes for nobody (and is cached); its slot goes to c.png.
        drop(pause_a);
        assert_eq!(third.await.expect("join"), StatusCode::OK);
        assert!(cached(&shas[0]), "the abandoned decode completed");
        assert!(cached(&shas[2]));
        assert!(!post.is_finished(), "the save is still paused");

        drop(pause_b);
        assert_eq!(
            post.await.expect("join"),
            StatusCode::BAD_REQUEST,
            "the frame does not match"
        );
        wait_until(
            "every slot released",
            Box::new(|| normalizations_running() == 0),
        )
        .await;
        server.abort();
        drop(home);
    }

    /// A worker thread opened directly is refused, drafts stay on the client,
    /// nothing is saved or queued, and nothing is rerouted to its orchestrator.
    #[tokio::test]
    async fn worker_feedback_is_refused_not_rerouted() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let worker = format!("worker-{}", uuid::Uuid::new_v4());
        // Lineage is recorded when the thread file is created.
        let mut thread = thread_persistence::Thread::with_id(worker.clone()).expect("thread");
        thread.set_origin(None, Some("telegram--100777-topic-9".to_string()), None);
        thread
            .append(&ThreadEvent::assistant_message("started"))
            .expect("append");
        write_feedback_thread(&worker);

        let sink = RecordingSink::new(true);
        let (addr, server) = spawn_feedback_app(Arc::clone(&sink)).await;
        let api = FeedbackClient {
            client: reqwest::Client::new(),
            addr,
            auth: format!("tma {}", fresh_init_data(279_058_397)),
        };
        let id = new_id();
        let (status, body) = api
            .post(&worker, &feedback_body(&id, &sha_of(FEEDBACK_PAGE)))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("worker thread"), "{body}");
        assert_eq!(sink.count(), 0);
        assert!(
            !artifact_feedback::feedback_dir_for_thread(&worker)
                .join(&id)
                .exists()
        );
        server.abort();
        drop(home);
    }

    /// Live polling over HTTP while a reply is delivered: the pending record
    /// shows a warning; its outcome (sent or failed) replaces it through a
    /// full, non-partial response, so the client never keeps a stale warning;
    /// other new events still arrive as a partial delta.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scripted pending → outcome → delta sequence
    async fn polling_replaces_a_pending_delivery_warning_with_its_outcome() {
        use zdx_engine::core::delivery::{DeliveryItem, DeliveryRecord};

        let home = zdx_engine::test_support::temp_zdx_home();
        let deliveries = ActiveDeliveries::default();
        let app = create_router(Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                WorkerManager::new().0,
            )
            .with_deliveries(deliveries.clone()),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let fetch = |thread_id: String, after: Option<usize>| {
            let client = client.clone();
            let auth = auth.clone();
            async move {
                let query = after.map_or_else(String::new, |after| format!("?after={after}"));
                let url = format!("http://{addr}/api/threads/{thread_id}{query}");
                let response = client
                    .get(url)
                    .header(AUTHORIZATION, &auth)
                    .send()
                    .await
                    .expect("request thread");
                assert_eq!(response.status(), StatusCode::OK);
                response
                    .json::<serde_json::Value>()
                    .await
                    .expect("thread json")
            }
        };
        let delivery_notices = |body: &serde_json::Value| -> Vec<String> {
            body["activity"]
                .as_array()
                .expect("activity")
                .iter()
                .filter(|item| item["type"] == "notice" && item["kind"] == "delivery")
                .map(|item| item["message"].as_str().unwrap_or_default().to_string())
                .collect()
        };

        for (outcome_failed, expected_after) in [(false, 0), (true, 1)] {
            let thread_id = format!("telegram-delivery-{}", uuid::Uuid::new_v4());
            let mut thread =
                thread_persistence::Thread::with_id(thread_id.clone()).expect("thread");
            thread
                .append(&ThreadEvent::user_message("make it"))
                .expect("append");
            thread
                .append(&ThreadEvent::assistant_message("Ready"))
                .expect("append");
            let mut record = DeliveryRecord::new(
                "telegram",
                Some(-100),
                Some(7),
                vec![
                    DeliveryItem::text(),
                    DeliveryItem::file("/tmp/out/report.html"),
                ],
            );
            let running = deliveries.start(&thread_id);
            thread
                .append(&ThreadEvent::delivery(record.clone()))
                .expect("append pending");

            let first = fetch(thread_id.clone(), None).await;
            assert_eq!(first["partial"], false);
            assert_eq!(first["delivering"], true, "clients keep polling");
            assert_eq!(delivery_notices(&first).len(), 1, "pending shows a warning");
            let cursor = first["cursor"].as_u64().expect("cursor") as usize;

            record.items[0].mark_sent(Some(10));
            if outcome_failed {
                record.items[1].mark_failed("Telegram API error for sendDocument (429)");
            } else {
                record.items[1].mark_sent(Some(11));
            }
            thread
                .append(&ThreadEvent::delivery(record.clone()))
                .expect("append outcome");
            drop(running);

            let poll = fetch(thread_id.clone(), Some(cursor)).await;
            assert_eq!(poll["partial"], false, "outcome must replace, not merge");
            assert_eq!(poll["delivering"], false);
            let notices = delivery_notices(&poll);
            assert_eq!(notices.len(), expected_after, "{notices:?}");
            if outcome_failed {
                assert!(notices[0].contains("report.html: not delivered"));
            }

            thread
                .append(&ThreadEvent::user_message("next"))
                .expect("append");
            let cursor = poll["cursor"].as_u64().expect("cursor") as usize;
            let delta = fetch(thread_id.clone(), Some(cursor)).await;
            assert_eq!(delta["partial"], true, "unrelated events stay a delta");
            assert_eq!(delta["activity"].as_array().map(Vec::len), Some(1));
        }

        server.abort();
        drop(home);
    }

    /// A thread browser can open a resumed topic by its alias id. Its events
    /// and the running delivery live under the source thread, so the alias
    /// response must show the source transcript, report `delivering` while
    /// the source delivery runs, and settle when the outcome lands.
    #[tokio::test]
    async fn alias_requests_follow_the_source_thread_and_its_delivery() {
        use zdx_engine::core::delivery::{DeliveryItem, DeliveryRecord};

        let home = zdx_engine::test_support::temp_zdx_home();
        let deliveries = ActiveDeliveries::default();
        let app = create_router(Arc::new(
            ServerState::new(
                SAMPLE_BOT_TOKEN.to_string(),
                sample_allowlist(),
                PathBuf::from("."),
                WorkerManager::new().0,
            )
            .with_deliveries(deliveries.clone()),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));

        let source_id = format!("telegram-source-{}", uuid::Uuid::new_v4());
        let alias_id = "telegram--1001234567890-topic-4242".to_string();
        let mut alias = thread_persistence::Thread::with_id(alias_id.clone()).expect("alias");
        alias.set_alias(Some(source_id.clone())).expect("set alias");
        let mut source = thread_persistence::Thread::with_id(source_id.clone()).expect("source");
        source
            .append(&ThreadEvent::user_message("make it"))
            .expect("append");
        source
            .append(&ThreadEvent::assistant_message("Ready"))
            .expect("append");
        let running = deliveries.start(&source_id);
        let mut record = DeliveryRecord::new(
            "telegram",
            Some(-1_001_234_567_890),
            Some(4242),
            vec![
                DeliveryItem::text(),
                DeliveryItem::file("/tmp/out/report.html"),
            ],
        );
        source
            .append(&ThreadEvent::delivery(record.clone()))
            .expect("append pending");

        let fetch = |after: Option<usize>| {
            let client = client.clone();
            let auth = auth.clone();
            let alias_id = alias_id.clone();
            async move {
                let query = after.map_or_else(String::new, |after| format!("?after={after}"));
                let response = client
                    .get(format!("http://{addr}/api/threads/{alias_id}{query}"))
                    .header(AUTHORIZATION, &auth)
                    .send()
                    .await
                    .expect("request alias thread");
                assert_eq!(response.status(), StatusCode::OK);
                response
                    .json::<serde_json::Value>()
                    .await
                    .expect("thread json")
            }
        };
        let delivery_notices = |body: &serde_json::Value| {
            body["activity"]
                .as_array()
                .expect("activity")
                .iter()
                .filter(|item| item["type"] == "notice" && item["kind"] == "delivery")
                .count()
        };

        let first = fetch(None).await;
        assert_eq!(first["id"], alias_id.as_str(), "requested id is kept");
        assert_eq!(
            first["telegram_link"], "https://t.me/c/1234567890/4242",
            "the alias topic link is kept"
        );
        assert_eq!(first["total_messages"], 2, "source transcript is shown");
        assert_eq!(first["delivering"], true, "source delivery is running");
        assert_eq!(delivery_notices(&first), 1);
        let cursor = first["cursor"].as_u64().expect("cursor") as usize;

        record.items[0].mark_sent(Some(10));
        record.items[1].mark_sent(Some(11));
        source
            .append(&ThreadEvent::delivery(record))
            .expect("append outcome");
        drop(running);

        let settled = fetch(Some(cursor)).await;
        assert_eq!(settled["delivering"], false);
        assert_eq!(
            settled["partial"], false,
            "outcome replaces the pending warning"
        );
        assert_eq!(delivery_notices(&settled), 0);

        server.abort();
        drop(home);
    }

    /// `/api/threads?kind=workers` lists only workers and flags them; the
    /// default list keeps every top-level thread with the same flag.
    #[tokio::test]
    async fn thread_list_filters_and_flags_workers() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let orchestrator_id = format!("orch-{}", uuid::Uuid::new_v4());
        let mut orchestrator =
            thread_persistence::Thread::with_id(orchestrator_id.clone()).expect("thread");
        orchestrator
            .append(&ThreadEvent::user_message("plan"))
            .expect("append");
        let worker_id = format!("worker-{}", uuid::Uuid::new_v4());
        let mut worker = thread_persistence::Thread::with_id(worker_id.clone()).expect("thread");
        worker.set_origin(None, Some(orchestrator_id.clone()), None);
        worker
            .append(&ThreadEvent::user_message("work"))
            .expect("append");

        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            WorkerManager::new().0,
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();
        let auth = format!("tma {}", fresh_init_data(279_058_397));
        let fetch = |query: &'static str| {
            let client = client.clone();
            let auth = auth.clone();
            async move {
                let response = client
                    .get(format!("http://{addr}/api/threads{query}"))
                    .header(AUTHORIZATION, &auth)
                    .send()
                    .await
                    .expect("request thread list");
                let status = response.status();
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                (status, body)
            }
        };
        let flags = |body: &serde_json::Value| -> Vec<(String, bool)> {
            body["threads"]
                .as_array()
                .expect("threads array")
                .iter()
                .map(|t| {
                    (
                        t["id"].as_str().unwrap().to_string(),
                        t["is_worker"].as_bool().unwrap(),
                    )
                })
                .collect()
        };

        let (status, all) = fetch("").await;
        assert_eq!(status, StatusCode::OK);
        let all = flags(&all);
        assert!(all.contains(&(worker_id.clone(), true)), "{all:?}");
        assert!(all.contains(&(orchestrator_id.clone(), false)), "{all:?}");

        let (status, workers) = fetch("?kind=workers").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(flags(&workers), vec![(worker_id, true)]);

        let (status, _) = fetch("?kind=bogus").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        server.abort();
        drop(home);
    }

    #[tokio::test]
    async fn serves_the_built_miniapp_with_cacheable_assets() {
        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            WorkerManager::new().0,
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        let client = reqwest::Client::new();

        let index = client
            .get(format!("http://{addr}/app"))
            .send()
            .await
            .expect("request shell");
        assert_eq!(index.status(), StatusCode::OK);
        // The entry point must revalidate so new content hashes are picked up.
        assert_eq!(
            index
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-cache")
        );
        let html = index.text().await.expect("read shell");
        assert!(
            html.contains("<div id=\"app\">"),
            "expected the Svelte shell"
        );

        // Pull a hashed asset straight out of the shell and confirm it is served
        // immutable — the only caching layer Telegram WebViews give us.
        let asset = first_script_asset(&html);

        let response = client
            .get(format!("http://{addr}{asset}"))
            .send()
            .await
            .expect("request hashed asset");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=31536000, immutable")
        );

        let missing = client
            .get(format!("http://{addr}/app/assets/nope.js"))
            .send()
            .await
            .expect("request missing asset");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        server.abort();
    }

    #[tokio::test]
    async fn compresses_miniapp_assets_when_the_client_accepts_gzip() {
        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            WorkerManager::new().0,
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });

        // reqwest transparently decodes gzip, so ask for the raw bytes instead.
        let client = reqwest::Client::builder()
            .no_gzip()
            .build()
            .expect("build client");

        // The SPA entry point is tiny; the bundle it pulls in is what actually
        // benefits, so assert the saving there.
        let html = client
            .get(format!("http://{addr}/app"))
            .send()
            .await
            .expect("request shell")
            .text()
            .await
            .expect("read shell");
        let asset = first_script_asset(&html);

        let compressed = client
            .get(format!("http://{addr}{asset}"))
            .header("accept-encoding", "gzip")
            .send()
            .await
            .expect("request compressed asset");
        assert_eq!(compressed.status(), StatusCode::OK);
        assert_eq!(
            compressed
                .headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let compressed_len = compressed.bytes().await.expect("read body").len();

        let identity = client
            .get(format!("http://{addr}{asset}"))
            .header("accept-encoding", "identity")
            .send()
            .await
            .expect("request uncompressed asset");
        assert!(identity.headers().get("content-encoding").is_none());
        let identity_len = identity.bytes().await.expect("read body").len();

        assert!(
            compressed_len * 2 < identity_len,
            "gzip should at least halve the bundle: {compressed_len} vs {identity_len}"
        );

        server.abort();
    }

    #[test]
    fn projects_thread_activity_without_private_replay_data() {
        let events = vec![
            ThreadEvent::Meta {
                schema_version: 1,
                title: Some("Inspect the agent".to_string()),
                root_path: Some("/private/project".to_string()),
                handoff_from: None,
                origin_kind: None,
                parent_thread_id: None,
                subagent_name: None,
                model_override: None,
                thinking_override: None,
                pending_topic_title: false,
                alias_to: None,
                worker_topic: false,
                ts: "2026-08-24T10:00:00Z".to_string(),
            },
            ThreadEvent::Reasoning {
                text: None,
                replay: Some(ReplayToken::AnthropicRedacted {
                    data: "private-replay-blob".to_string(),
                }),
                ts: "2026-08-24T10:00:01Z".to_string(),
            },
            ThreadEvent::ToolUse {
                id: "call-1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "src/main.rs"}),
                id_origin: zdx_engine::providers::IdOrigin::Real,
                replay: Some(ReplayToken::Anthropic {
                    signature: "private-tool-signature".to_string(),
                }),
                ts: "2026-08-24T10:00:02Z".to_string(),
            },
            ThreadEvent::ToolResult {
                tool_use_id: "call-1".to_string(),
                output: serde_json::json!({"ok": true, "data": "fn main() {}"}),
                ok: true,
                duration_ms: Some(12),
                started_at: Some("2026-08-24T10:00:02.100Z".to_string()),
                completed_at: Some("2026-08-24T10:00:02.112Z".to_string()),
                ts: "2026-08-24T10:00:03Z".to_string(),
            },
            ThreadEvent::Usage {
                input_tokens: 120,
                output_tokens: 30,
                cache_read_tokens: 10,
                cache_write_tokens: 0,
                model: Some("test-model".to_string()),
                provider: Some("test-provider".to_string()),
                duration_ms: Some(500),
                ttft_ms: Some(100),
                started_at: Some("2026-08-24T10:00:03.500Z".to_string()),
                completed_at: Some("2026-08-24T10:00:04.000Z".to_string()),
                ts: "2026-08-24T10:00:04Z".to_string(),
            },
            ThreadEvent::Message {
                role: "assistant".to_string(),
                text: "Done".to_string(),
                phase: Some("final_answer".to_string()),
                context: None,
                context_key: None,
                replay: None,
                ts: "2026-08-24T10:00:05Z".to_string(),
            },
        ];

        let response = project_thread(
            "thread-1".to_string(),
            events,
            None,
            ThreadLineage::default(),
        );
        let json = serde_json::to_string(&response).expect("serialize thread activity");

        assert_eq!(response.title, "Inspect the agent");
        assert_eq!(response.total_messages, 1);
        assert_eq!(response.total_events, 5);
        assert_eq!(response.cursor, 6, "cursor counts hidden source events too");
        assert!(json.contains("\"type\":\"reasoning\""));
        assert!(json.contains("\"redacted\":true"));
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"type\":\"tool_result\""));
        assert!(json.contains("\"type\":\"usage\""));
        assert!(json.contains("\"ts\":\"2026-08-24T10:00:02Z\""));
        assert!(json.contains("\"started_at\":\"2026-08-24T10:00:02.100Z\""));
        assert!(json.contains("\"completed_at\":\"2026-08-24T10:00:04.000Z\""));
        assert!(!json.contains("/private/project"));
        assert!(!json.contains("private-replay-blob"));
        assert!(!json.contains("private-tool-signature"));
    }

    #[test]
    fn projects_message_media_as_inline_attachments() {
        let events = vec![ThreadEvent::Message {
            role: "assistant".to_string(),
            text:
                "Full details attached ↓<media>/tmp/definitely-not-here-12345/report.html</media>"
                    .to_string(),
            phase: None,
            context: None,
            context_key: None,
            replay: None,
            ts: "2026-08-24T10:00:05Z".to_string(),
        }];

        let response = project_thread(
            "thread-media".to_string(),
            events,
            None,
            ThreadLineage::default(),
        );
        assert_eq!(response.activity.len(), 1);
        let ThreadActivity::Message {
            text, artifacts, ..
        } = &response.activity[0]
        else {
            panic!("expected a message activity");
        };
        assert_eq!(text, "Full details attached ↓");
        let attachments = artifacts.as_deref().expect("media becomes attachments");
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments[0].path,
            "/tmp/definitely-not-here-12345/report.html"
        );
        assert_eq!(attachments[0].kind, artifacts::ArtifactKind::Html);
    }

    /// The live poll asks for a window instead of the whole transcript. The
    /// window must drop delivered items, keep the totals a first load reports,
    /// and leave sequences untouched so the client can merge on them.
    #[test]
    fn delta_window_returns_only_undelivered_activity() {
        let events = vec![
            ThreadEvent::user_message("first"),
            ThreadEvent::Message {
                role: "assistant".to_string(),
                text: "second".to_string(),
                phase: None,
                context: None,
                context_key: None,
                replay: None,
                ts: "2026-08-24T10:00:01Z".to_string(),
            },
            ThreadEvent::Message {
                role: "assistant".to_string(),
                text: "third".to_string(),
                phase: None,
                context: None,
                context_key: None,
                replay: None,
                ts: "2026-08-24T10:00:02Z".to_string(),
            },
        ];

        let full = project_thread(
            "thread-delta".to_string(),
            events.clone(),
            None,
            ThreadLineage::default(),
        );
        assert_eq!(full.activity.len(), 3);
        assert_eq!(full.cursor, 3);
        assert!(!full.partial, "a first load is never partial");

        // Simulate the handler's windowing at the cursor the client last saw.
        let mut delta = project_thread(
            "thread-delta".to_string(),
            events,
            None,
            ThreadLineage::default(),
        );
        let after = 2;
        delta.activity.retain(|item| item.sequence() >= after);
        delta.partial = true;

        assert_eq!(delta.activity.len(), 1, "only the undelivered item");
        assert_eq!(delta.activity[0].sequence(), 2);
        assert_eq!(
            delta.total_messages, full.total_messages,
            "totals describe the whole thread, not the window"
        );
        assert_eq!(delta.cursor, full.cursor);

        let json = serde_json::to_string(&delta).expect("serialize delta");
        assert!(json.contains("third"));
        assert!(!json.contains("first"), "delivered items are not resent");
        assert!(!json.contains("second"));
    }

    /// A concurrent round: the finished call must stay visible next to the
    /// running one until the checkpoint writes both, and must not be listed
    /// twice once it is in the thread file. The run status stays set across
    /// the handoff so clients keep polling.
    #[test]
    fn live_run_keeps_finished_calls_until_the_checkpoint_lands() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let thread_id = format!("telegram-live-{}", uuid::Uuid::new_v4());
        let mut thread = thread_persistence::Thread::with_id(thread_id.clone()).expect("thread");
        thread
            .append(&ThreadEvent::user_message("run checks"))
            .expect("append user message");

        let guard = agent_activity::start(agent_activity::StartParams {
            thread_id: Some(&thread_id),
            ..Default::default()
        })
        .expect("start run marker");
        guard.set_tools_started(vec![
            agent_activity::ActiveToolCall::new(
                "read-1",
                "read",
                &serde_json::json!({ "path": "src/lib.rs" }),
            ),
            agent_activity::ActiveToolCall::new(
                "bash-1",
                "bash",
                &serde_json::json!({ "command": "cargo test" }),
            ),
        ]);
        guard.set_tool_finished("read-1", true, Some(2400));

        let project = || {
            let events = load_thread_events(&thread_id).expect("load thread");
            let mut response =
                project_thread(thread_id.clone(), events, None, ThreadLineage::default());
            attach_live_run(&mut response, &thread_id);
            response
        };
        let live_tools = |response: &ThreadResponse| -> Vec<(String, Option<bool>)> {
            response
                .activity
                .iter()
                .filter_map(|item| match item {
                    ThreadActivity::ToolRunning { id, ok, .. } => Some((id.clone(), *ok)),
                    _ => None,
                })
                .collect()
        };

        let before = project();
        let run = before.run.as_ref().expect("run status while the turn runs");
        assert_eq!(run.state, "tool");
        assert_eq!(run.label, "bash: cargo test");
        assert_eq!((run.running_tools, run.finished_tools), (1, 1));
        assert_eq!(
            live_tools(&before),
            vec![
                ("read-1".to_string(), Some(true)),
                ("bash-1".to_string(), None)
            ]
        );
        assert!(
            before
                .activity
                .iter()
                .filter(|item| matches!(item, ThreadActivity::ToolRunning { .. }))
                .all(|item| item.sequence() >= before.cursor),
            "live rows sit at or after the cursor, so a delta poll keeps them"
        );

        // The round ends: the checkpoint persists both calls while the
        // marker still lists them until the next round.
        guard.set_tool_finished("bash-1", true, Some(9000));
        guard.set_phase("waiting");
        for (id, name, input) in [
            (
                "read-1",
                "read",
                serde_json::json!({ "path": "src/lib.rs" }),
            ),
            (
                "bash-1",
                "bash",
                serde_json::json!({ "command": "cargo test" }),
            ),
        ] {
            thread
                .append(&ThreadEvent::tool_use(id, name, input))
                .expect("append tool use");
            thread
                .append(&ThreadEvent::tool_result(id, serde_json::json!({}), true))
                .expect("append tool result");
        }

        let after = project();
        assert!(
            live_tools(&after).is_empty(),
            "persisted calls are not listed twice"
        );
        let run = after.run.as_ref().expect("still running between rounds");
        assert_eq!(run.state, "waiting");
        assert_eq!(run.label, "waiting for model");

        drop(guard);
        assert!(project().run.is_none(), "no run once the marker is gone");
    }

    #[test]
    fn accepts_only_hex_object_ids_as_revisions() {
        assert!(valid_commit_hash("723a11e"));
        assert!(valid_commit_hash(
            "8e66e93a1b2c3d4e5f60718293a4b5c6d7e8f900"
        ));

        // Too short to be an abbreviated hash, or too long to be a hash at all.
        assert!(!valid_commit_hash("723a11"));
        assert!(!valid_commit_hash(&"a".repeat(41)));
        assert!(!valid_commit_hash(""));
        // Anything that git could read as an option, a range or a second
        // argument must never reach the command line.
        assert!(!valid_commit_hash("--output=/tmp/x"));
        assert!(!valid_commit_hash("HEAD~1"));
        assert!(!valid_commit_hash("main..HEAD"));
        assert!(!valid_commit_hash("723a11e; rm -rf /"));
    }

    #[test]
    fn builds_topic_links_only_for_supergroup_topic_threads() {
        assert_eq!(
            telegram_topic_link("telegram--1001234567890-topic-17771").as_deref(),
            Some("https://t.me/c/1234567890/17771")
        );

        // Plain DM threads carry no topic, and DM chat ids have no `-100`
        // prefix, so neither form yields a linkable topic.
        assert_eq!(telegram_topic_link("telegram--1001234567890"), None);
        assert_eq!(telegram_topic_link("telegram-12345-topic-7"), None);
        // Threads started outside Telegram (TUI/CLI/zbar).
        assert_eq!(
            telegram_topic_link("3d6f20e3-78d4-4d1f-9f8e-4d442f3bba31"),
            None
        );
        // Non-numeric segments must not be pasted into a URL.
        assert_eq!(telegram_topic_link("telegram--100abc-topic-7"), None);
        assert_eq!(telegram_topic_link("telegram--1001-topic-"), None);
    }

    #[test]
    fn resolves_telegram_link_for_direct_topic_threads() {
        // Direct topic threads resolve from the ID alone and never consult the
        // thread index. The worker→mirror branch is covered in zdx-engine
        // (`mirror_thread_id_for_worker`), which owns the indexed lookup and
        // has a temp-ZDX_HOME harness to persist mirror metadata.
        assert_eq!(
            resolve_telegram_link("telegram--1001234567890-topic-17771").as_deref(),
            Some("https://t.me/c/1234567890/17771")
        );
    }

    #[test]
    fn rejects_thread_ids_that_can_escape_the_threads_directory() {
        assert!(valid_thread_id("telegram--100-topic-42"));
        assert!(valid_thread_id("3d6f20e3-78d4-4d1f-9f8e-4d442f3bba31"));
        assert!(!valid_thread_id(""));
        assert!(!valid_thread_id(".."));
        assert!(!valid_thread_id("../config"));
        assert!(!valid_thread_id("folder/thread"));
        assert!(!valid_thread_id("folder\\thread"));
    }

    #[test]
    fn parses_branch_state_and_changed_file_categories() {
        let output = concat!(
            "# branch.oid 0123456789abcdef\0",
            "# branch.head main\0",
            "# branch.upstream origin/main\0",
            "# branch.ab +2 -1\0",
            "1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb src/lib.rs\0",
            "1 M. N... 100644 100644 100644 aaaaaaa bbbbbbb Cargo.toml\0",
            "2 R. N... 100644 100644 100644 aaaaaaa bbbbbbb R100 src/new name.rs\0",
            "src/old name.rs\0",
            "? notes/todo.txt\0",
        );

        let status = parse_git_status(output);

        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert_eq!(status.files.unstaged[0].path, "src/lib.rs");
        assert_eq!(status.files.staged[0].path, "Cargo.toml");
        assert_eq!(status.files.staged[1].status, "R");
        assert_eq!(
            status.files.staged[1].original_path.as_deref(),
            Some("src/old name.rs")
        );
        assert_eq!(status.files.untracked[0].path, "notes/todo.txt");
    }

    #[test]
    fn parses_worktree_flags_and_commit_rows() {
        let worktrees = parse_git_worktrees(concat!(
            "worktree /repo\0",
            "HEAD 0123456789abcdef\0",
            "branch refs/heads/main\0\0",
            "worktree /repo-review\0",
            "HEAD fedcba9876543210\0",
            "detached\0",
            "locked review\0\0",
        ));
        let commits = parse_git_commits(
            "0123456789abcdef\x1f01234567\x1fAlice\x1f2026-08-24T10:00:00Z\x1f2 hours ago\x1fHEAD -> main\x1fBuild Git view\x1e",
        );

        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(worktrees[0].head.as_deref(), Some("01234567"));
        assert!(worktrees[1].flags.contains(&"detached"));
        assert!(worktrees[1].flags.contains(&"locked"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].short_hash, "01234567");
        assert_eq!(commits[0].subject, "Build Git view");
    }

    #[test]
    fn accepts_only_repository_relative_git_paths() {
        assert!(valid_git_path("src/main.rs"));
        assert!(valid_git_path("notes/file with spaces.md"));
        assert!(!valid_git_path(""));
        assert!(!valid_git_path("../outside"));
        assert!(!valid_git_path("/absolute/path"));
    }

    #[tokio::test]
    async fn caps_lazy_untracked_diffs() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zdx-bot-git-diff-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create temporary Git repository");
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("initialize temporary Git repository");
        assert!(init.success());
        std::fs::write(root.join("large.txt"), vec![b'x'; GIT_DIFF_LIMIT_BYTES * 2])
            .expect("write oversized untracked file");

        let (diff, truncated) = load_git_diff(&root, "large.txt", &GitDiffKind::Untracked)
            .await
            .expect("read bounded Git diff");

        assert!(truncated);
        assert_eq!(diff.len(), GIT_DIFF_LIMIT_BYTES);
        std::fs::remove_dir_all(root).expect("remove temporary Git repository");
    }

    #[tokio::test]
    async fn treats_diff_file_names_as_literal_paths() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zdx-bot-git-literal-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create temporary Git repository");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("run Git fixture command")
        };
        assert!(git(&["init", "--quiet"]).success());
        std::fs::write(root.join("literal*.txt"), "initial\n").expect("write literal path");
        std::fs::write(root.join("literal-match.txt"), "initial\n").expect("write matching path");
        assert!(git(&["add", "."]).success());
        assert!(
            git(&[
                "-c",
                "user.name=ZDX Test",
                "-c",
                "user.email=zdx@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ])
            .success()
        );
        std::fs::write(root.join("literal*.txt"), "selected\n").expect("modify literal path");
        std::fs::write(root.join("literal-match.txt"), "other\n").expect("modify matching path");

        let (diff, truncated) = load_git_diff(&root, "literal*.txt", &GitDiffKind::Unstaged)
            .await
            .expect("read literal Git diff");
        let diff = String::from_utf8(diff).expect("UTF-8 fixture diff");

        assert!(!truncated);
        assert!(diff.contains("literal*.txt"));
        assert!(!diff.contains("literal-match.txt"));
        std::fs::remove_dir_all(root).expect("remove temporary Git repository");
    }

    #[test]
    fn validates_official_telegram_sample() {
        assert_eq!(
            validate_init_data(
                SAMPLE_INIT_DATA,
                SAMPLE_BOT_TOKEN,
                &sample_allowlist(),
                SAMPLE_AUTH_DATE + 60,
            ),
            Ok(279_058_397)
        );
    }

    #[test]
    fn rejects_tampered_init_data() {
        let tampered = SAMPLE_INIT_DATA.replace("Vladislav", "Mallory");
        assert_eq!(
            validate_init_data(
                &tampered,
                SAMPLE_BOT_TOKEN,
                &sample_allowlist(),
                SAMPLE_AUTH_DATE + 60,
            ),
            Err(InitDataError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_expired_init_data() {
        assert_eq!(
            validate_init_data(
                SAMPLE_INIT_DATA,
                SAMPLE_BOT_TOKEN,
                &sample_allowlist(),
                SAMPLE_AUTH_DATE + INIT_DATA_MAX_AGE_SECS + 1,
            ),
            Err(InitDataError::Expired)
        );
    }

    #[test]
    fn rejects_users_outside_allowlist() {
        assert_eq!(
            validate_init_data(
                SAMPLE_INIT_DATA,
                SAMPLE_BOT_TOKEN,
                &HashSet::new(),
                SAMPLE_AUTH_DATE + 60,
            ),
            Err(InitDataError::Forbidden)
        );
    }

    #[test]
    fn monitor_config_never_serializes_credentials_or_prompts() {
        let mut config = Config::default();
        config.telegram.bot_token = Some("never-expose-this-token".to_string());
        config.telegram.allowlist_user_ids = vec![123_456];
        config.system_prompt = Some("private system prompt".to_string());

        let json = serde_json::to_string(&monitor_config(&config)).expect("serialize summary");

        assert!(!json.contains("never-expose-this-token"));
        assert!(!json.contains("123456"));
        assert!(!json.contains("private system prompt"));
        assert!(json.contains(&config.model));
    }

    #[test]
    fn monitor_subscription_serializes_bounded_quota_windows() {
        let reset = chrono::DateTime::parse_from_rfc3339("2026-08-22T10:00:00Z")
            .expect("valid reset")
            .with_timezone(&chrono::Utc);
        let quota = SubscriptionQuota {
            plan: Some("max".to_string()),
            windows: vec![zdx_engine::providers::subscription_quota::QuotaWindow {
                label: "5h".to_string(),
                used_percent: 140.0,
                resets_at: Some(reset),
                scope: Some("Opus".to_string()),
            }],
        };

        let subscription = monitor_subscription("claude-cli", "Claude".to_string(), Ok(quota))
            .expect("authenticated subscription");

        assert_eq!(subscription.plan.as_deref(), Some("max"));
        assert!((subscription.windows[0].used_percent - 100.0).abs() < f64::EPSILON);
        assert_eq!(
            subscription.windows[0].resets_at.as_deref(),
            Some("2026-08-22T10:00:00+00:00")
        );
        assert_eq!(subscription.windows[0].scope.as_deref(), Some("Opus"));
        assert!(
            monitor_subscription(
                "claude-cli",
                "Claude".to_string(),
                Err(QuotaError::NotAuthenticated),
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn concurrent_monitor_requests_share_one_build() {
        let state = Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
            WorkerManager::new().0,
        ));
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = (0..4).map(|_| {
            let (state, builds) = (Arc::clone(&state), Arc::clone(&builds));
            tokio::spawn(async move {
                cached_monitor_or_build(&state, move || {
                    builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(MonitorResponse {
                        generated_at: String::new(),
                        services: Vec::new(),
                        active_agents: Vec::new(),
                        background_processes: Vec::new(),
                        jobs: Vec::new(),
                        jobs_error: None,
                        config: monitor_config(&Config::default()),
                        usage: None,
                        subscriptions: Vec::new(),
                        subscriptions_status: "pending",
                        subscriptions_age: None,
                    })
                })
                .await
                .is_ok()
            })
        });
        for request in requests.collect::<Vec<_>>() {
            assert!(request.await.expect("join monitor request"));
        }
        assert_eq!(builds.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
