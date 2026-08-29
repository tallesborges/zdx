use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Component, Path as FilePath, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};
use zdx_engine::config::{Config, paths};
use zdx_engine::core::events::NoticeKind;
use zdx_engine::core::thread_index;
use zdx_engine::core::thread_persistence::{self, ThreadEvent, load_thread_events};
use zdx_engine::core::usage_stats::{self, UsageStats};
use zdx_engine::providers::ReplayToken;
use zdx_engine::providers::subscription_quota::{self, QuotaError, SubscriptionQuota};
use zdx_engine::service::{self, Service};
use zdx_engine::{agent_activity, automations, background_activity};

const APP_HTML: &str = include_str!("app.html");
const INIT_DATA_MAX_AGE_SECS: u64 = 60 * 60;
const INIT_DATA_FUTURE_SKEW_SECS: u64 = 30;
const MONITOR_CACHE_TTL: Duration = Duration::from_secs(30);
const SUBSCRIPTION_CACHE_TTL: Duration = Duration::from_mins(5);
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const GIT_DIFF_LIMIT_BYTES: usize = 256 * 1024;
const GIT_COMMIT_LIMIT: usize = 24;

type HmacSha256 = Hmac<Sha256>;
type ApiError = (StatusCode, &'static str);

#[derive(Serialize)]
pub struct ThreadResponse {
    pub id: String,
    pub title: String,
    pub total_messages: usize,
    pub total_events: usize,
    pub activity: Vec<ThreadActivity>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadActivity {
    Message {
        sequence: usize,
        time: String,
        role: String,
        speaker: &'static str,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    Reasoning {
        sequence: usize,
        time: String,
        text: String,
        redacted: bool,
    },
    ToolUse {
        sequence: usize,
        time: String,
        id: String,
        name: String,
        input: Value,
    },
    /// A tool call currently executing (from the run's activity marker), not
    /// yet persisted to the thread. Always the tail of `activity`.
    ToolRunning {
        sequence: usize,
        time: String,
        id: String,
        name: String,
        summary: String,
        /// Full tool input, or `Value::Null` when the marker only kept the
        /// summary (oversized input).
        input: Value,
        /// Tail of the tool's streaming output so far (bounded).
        output_tail: String,
        /// Human elapsed time since the tool started, e.g. `42s`.
        running_for: String,
    },
    ToolResult {
        sequence: usize,
        time: String,
        tool_use_id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        output: Value,
    },
    Usage {
        sequence: usize,
        time: String,
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
    },
    Notice {
        sequence: usize,
        time: String,
        kind: &'static str,
        message: String,
    },
    Interrupted {
        sequence: usize,
        time: String,
        role: String,
        text: String,
    },
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
    automations: Vec<MonitorAutomation>,
    config: MonitorConfig,
    usage: Option<MonitorUsage>,
    subscriptions: Vec<MonitorSubscription>,
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
    /// Currently executing tool call (`"bash: cargo build …"`), when the run
    /// is inside a tool round.
    current_tool: Option<String>,
    /// Coarse run phase (`waiting`/`thinking`/`answering`/`retrying`).
    phase: Option<String>,
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
struct MonitorAutomation {
    name: String,
    schedule: Option<String>,
}

#[derive(Clone, Serialize)]
struct MonitorConfig {
    model: String,
    thinking: String,
    max_tokens: Option<u32>,
    tool_timeout_secs: u32,
    subagents_enabled: bool,
    favorite_count: usize,
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
    monitor_cache: RwLock<Option<CachedMonitor>>,
    subscription_cache: Mutex<Option<CachedSubscriptions>>,
}

impl ServerState {
    fn new(bot_token: String, allowlist_user_ids: HashSet<i64>, root: PathBuf) -> Self {
        Self {
            bot_token,
            allowlist_user_ids,
            root,
            monitor_cache: RwLock::new(None),
            subscription_cache: Mutex::new(None),
        }
    }
}

#[derive(Deserialize)]
struct InitDataUser {
    id: i64,
}

/// Creates the router for the embedded web server.
pub(crate) fn create_router(state: Arc<ServerState>) -> Router {
    let api = Router::new()
        .route("/threads/{id}", get(get_thread))
        .route("/monitor", get(get_monitor))
        .route("/git", get(get_git))
        .route("/git/diff", get(get_git_diff))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorize_api,
        ));

    Router::new()
        .route("/app", get(serve_app))
        .route("/threads", get(serve_app))
        .route("/monitor", get(serve_app))
        .nest("/api", api)
        .with_state(state)
}

async fn serve_app() -> impl IntoResponse {
    Html(APP_HTML)
}

async fn authorize_api(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    authorize(&headers, &state)?;
    Ok(next.run(request).await)
}

async fn get_thread(Path(id): Path<String>) -> Result<Json<ThreadResponse>, ApiError> {
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
            tracing::warn!(thread_id = target_id, %error, "Failed to load Mini App thread");
            (StatusCode::NOT_FOUND, "Thread not found")
        })?;

        let mut response = project_thread(target_id, events);
        append_running_tools(&mut response);
        Ok(Json(response))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Mini App thread load task failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Thread load failed")
    })?
}

/// Appends a `ToolRunning` record for each in-flight tool from the active-run
/// marker bound to this thread. Skips ids already persisted as `tool_use`
/// (a finished round lands in the JSONL just before the marker entry clears).
fn append_running_tools(response: &mut ThreadResponse) {
    let Some(record) = agent_activity::list_active()
        .into_iter()
        .find(|r| r.thread_id.as_deref() == Some(response.id.as_str()))
    else {
        return;
    };
    let persisted: HashSet<String> = response
        .activity
        .iter()
        .filter_map(|a| match a {
            ThreadActivity::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut sequence = response.total_events;
    for tool in record.current_tools {
        if persisted.contains(&tool.id) {
            continue;
        }
        response.activity.push(ThreadActivity::ToolRunning {
            sequence,
            time: event_time(&tool.started_at),
            running_for: agent_activity::uptime_since(&tool.started_at),
            id: tool.id,
            name: tool.name.to_ascii_lowercase(),
            summary: tool.summary,
            input: tool.input,
            output_tail: tool.output_tail,
        });
        sequence += 1;
    }
}

#[allow(clippy::too_many_lines)]
fn project_thread(target_id: String, events: Vec<ThreadEvent>) -> ThreadResponse {
    let mut title = "Thread Transcript".to_string();
    let mut activity = Vec::new();
    let mut total_messages = 0;

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
                let clean_text = clean_message_text(&text);
                if !clean_text.is_empty() {
                    activity.push(ThreadActivity::Message {
                        sequence,
                        time: event_time(&ts),
                        role,
                        speaker,
                        text: clean_text,
                        phase,
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
                id,
                name,
                input,
            }),
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                duration_ms,
                ts,
            } => activity.push(ThreadActivity::ToolResult {
                sequence,
                time: event_time(&ts),
                tool_use_id,
                ok,
                duration_ms,
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
                ts,
            } => activity.push(ThreadActivity::Usage {
                sequence,
                time: event_time(&ts),
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                model,
                provider,
                duration_ms,
                ttft_ms,
            }),
            ThreadEvent::Notice { kind, message, ts } => {
                let kind = match kind {
                    NoticeKind::Refusal => "refusal",
                    NoticeKind::ContextWindowExceeded => "context_window_exceeded",
                    NoticeKind::Goal => "goal",
                };
                activity.push(ThreadActivity::Notice {
                    sequence,
                    time: event_time(&ts),
                    kind,
                    message,
                });
            }
            ThreadEvent::Interrupted { role, text, ts } => {
                activity.push(ThreadActivity::Interrupted {
                    sequence,
                    time: event_time(&ts),
                    role,
                    text,
                });
            }
            ThreadEvent::Meta { .. } => {}
        }
    }

    ThreadResponse {
        id: target_id,
        title,
        total_messages,
        total_events: activity.len(),
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
    if let Some(start) = clean.find("<medias>")
        && let Some(end) = clean.find("</medias>")
    {
        clean.replace_range(start..end + "</medias>".len(), "");
    }
    if let Some(start) = clean.find("<media>")
        && let Some(end) = clean.find("</media>")
    {
        clean.replace_range(start..end + "</media>".len(), "");
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

#[derive(Clone, Copy)]
enum GitDiffKind {
    Staged,
    Unstaged,
    Untracked,
}

impl GitDiffKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "staged" => Some(Self::Staged),
            "unstaged" => Some(Self::Unstaged),
            "untracked" => Some(Self::Untracked),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
        }
    }
}

async fn get_git(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitQuery>,
) -> Result<Json<GitResponse>, ApiError> {
    let repository = resolve_git_repository(&state, query.thread_id).await?;
    let status = load_git_status(&repository.root).await.map_err(|error| {
        tracing::warn!(root = %repository.root.display(), %error, "Failed to read Mini App Git state");
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
            tracing::warn!(root = %repository.root.display(), %error, "Failed to read Mini App Git state");
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

async fn get_git_diff(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<GitDiffQuery>,
) -> Result<Json<GitDiffResponse>, ApiError> {
    let kind = GitDiffKind::parse(&query.kind)
        .ok_or((StatusCode::BAD_REQUEST, "Invalid Git file category"))?;
    if !valid_git_path(&query.path) {
        return Err((StatusCode::BAD_REQUEST, "Invalid Git file path"));
    }

    let repository = resolve_git_repository(&state, query.thread_id).await?;
    let status = load_git_status(&repository.root).await.map_err(|error| {
        tracing::warn!(root = %repository.root.display(), %error, "Failed to validate Mini App Git diff");
        (StatusCode::INTERNAL_SERVER_ERROR, "Git state unavailable")
    })?;
    let files = match kind {
        GitDiffKind::Staged => &status.files.staged,
        GitDiffKind::Unstaged => &status.files.unstaged,
        GitDiffKind::Untracked => &status.files.untracked,
    };
    if !files.iter().any(|file| file.path == query.path) {
        return Err((
            StatusCode::NOT_FOUND,
            "Git file is no longer in that category",
        ));
    }

    let (bytes, truncated) = load_git_diff(&repository.root, &query.path, kind)
        .await
        .map_err(|error| {
            tracing::warn!(root = %repository.root.display(), path = query.path, %error, "Failed to read Mini App Git diff");
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

async fn load_git_diff(
    root: &FilePath,
    path: &str,
    kind: GitDiffKind,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    match kind {
        GitDiffKind::Staged => {
            command.args([
                "diff",
                "--cached",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                path,
            ]);
        }
        GitDiffKind::Unstaged => {
            command.args([
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                path,
            ]);
        }
        GitDiffKind::Untracked => {
            command.args([
                "diff",
                "--no-index",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                "/dev/null",
                path,
            ]);
        }
    }

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

async fn get_monitor(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<MonitorResponse>, ApiError> {
    if let Some(cached) = state.monitor_cache.read().await.as_ref()
        && cached.cached_at.elapsed() < MONITOR_CACHE_TTL
    {
        return Ok(Json(cached.response.clone()));
    }

    let root = state.root.clone();
    let snapshot_task = tokio::task::spawn_blocking(move || build_monitor_response(&root));
    let subscriptions = load_subscription_quotas(&state).await;
    let mut response = snapshot_task
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App monitor snapshot task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Monitor snapshot unavailable",
            )
        })?
        .map_err(|error| {
            tracing::warn!(%error, "Failed to build Mini App monitor snapshot");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Monitor snapshot unavailable",
            )
        })?;
    response.subscriptions = subscriptions;

    *state.monitor_cache.write().await = Some(CachedMonitor {
        cached_at: Instant::now(),
        response: response.clone(),
    });
    Ok(Json(response))
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

    let active_agents = agent_activity::list_active()
        .into_iter()
        .map(|record| {
            let current_tool = record.current_tools.first().map(|tool| {
                let mut label = if tool.summary.is_empty() {
                    tool.name.clone()
                } else {
                    format!("{}: {}", tool.name, tool.summary)
                };
                if record.current_tools.len() > 1 {
                    use std::fmt::Write as _;
                    let _ = write!(label, " (+{} more)", record.current_tools.len() - 1);
                }
                label
            });
            MonitorAgent {
                pid: record.pid,
                thread_id: record.thread_id,
                parent_thread_id: record.parent_thread_id,
                surface: record.surface,
                role: record.subagent_name.or(record.kind),
                model: record.model,
                provider: record.provider,
                account: record.account,
                thinking: record.thinking,
                uptime: agent_activity::uptime_since(&record.started_at),
                current_tool,
                phase: record.phase,
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

    let automations = automations::discover(root)
        .unwrap_or_default()
        .into_iter()
        .map(|automation| MonitorAutomation {
            name: automation.name,
            schedule: automation.schedule,
        })
        .collect();

    let usage = usage_stats::aggregate_usage(
        &config.model,
        Some(usage_stats::today_utc().saturating_sub(29)),
    )
    .map(monitor_usage)
    .map_err(|error| {
        tracing::warn!(%error, "Mini App monitor usage aggregation failed");
        error
    })
    .ok();

    Ok(MonitorResponse {
        generated_at: chrono::Utc::now().to_rfc3339(),
        services,
        active_agents,
        background_processes,
        automations,
        config: monitor_config(&config),
        usage,
        subscriptions: Vec::new(),
    })
}

async fn load_subscription_quotas(state: &ServerState) -> Vec<MonitorSubscription> {
    let mut cache = state.subscription_cache.lock().await;
    if let Some(cached) = cache.as_ref()
        && cached.cached_at.elapsed() < SUBSCRIPTION_CACHE_TTL
    {
        return cached.subscriptions.clone();
    }

    let snapshot = match subscription_quota::fetch_snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(%error, "Failed to fetch subscription quota snapshot");
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
    *cache = Some(CachedSubscriptions {
        cached_at: Instant::now(),
        subscriptions: subscriptions.clone(),
    });
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
        tool_timeout_secs: config.tool_timeout_secs,
        subagents_enabled: config.subagents.enabled,
        favorite_count: config.favorites.len(),
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
) {
    let state = Arc::new(ServerState::new(bot_token, allowlist_user_ids, root));
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

    #[tokio::test]
    async fn protects_every_api_route_but_not_the_miniapp_page() {
        let app = create_router(Arc::new(ServerState::new(
            SAMPLE_BOT_TOKEN.to_string(),
            sample_allowlist(),
            PathBuf::from("."),
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
            "/api/threads/active",
            "/api/monitor",
            "/api/git?thread_id=active",
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
                ts: "2026-08-24T10:00:04Z".to_string(),
            },
            ThreadEvent::Message {
                role: "assistant".to_string(),
                text: "Done".to_string(),
                phase: Some("final_answer".to_string()),
                replay: None,
                ts: "2026-08-24T10:00:05Z".to_string(),
            },
        ];

        let response = project_thread("thread-1".to_string(), events);
        let json = serde_json::to_string(&response).expect("serialize thread activity");

        assert_eq!(response.title, "Inspect the agent");
        assert_eq!(response.total_messages, 1);
        assert_eq!(response.total_events, 5);
        assert!(json.contains("\"type\":\"reasoning\""));
        assert!(json.contains("\"redacted\":true"));
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"type\":\"tool_result\""));
        assert!(json.contains("\"type\":\"usage\""));
        assert!(!json.contains("/private/project"));
        assert!(!json.contains("private-replay-blob"));
        assert!(!json.contains("private-tool-signature"));
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

        let (diff, truncated) = load_git_diff(&root, "large.txt", GitDiffKind::Untracked)
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

        let (diff, truncated) = load_git_diff(&root, "literal*.txt", GitDiffKind::Unstaged)
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
}
