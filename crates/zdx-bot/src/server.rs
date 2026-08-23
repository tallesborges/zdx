use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path as FilePath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::{Mutex, RwLock};
use zdx_engine::config::{Config, paths};
use zdx_engine::core::thread_persistence::{ThreadEvent, load_thread_events};
use zdx_engine::core::usage_stats::{self, UsageStats};
use zdx_engine::providers::subscription_quota::{self, QuotaError, SubscriptionQuota};
use zdx_engine::service::{self, Service};
use zdx_engine::{agent_activity, automations, background_activity};

const MINIAPP_HTML: &str = include_str!("miniapp.html");
const MONITOR_HTML: &str = include_str!("monitor.html");
const INIT_DATA_MAX_AGE_SECS: u64 = 60 * 60;
const INIT_DATA_FUTURE_SKEW_SECS: u64 = 30;
const MONITOR_CACHE_TTL: Duration = Duration::from_secs(30);
const SUBSCRIPTION_CACHE_TTL: Duration = Duration::from_mins(5);

type HmacSha256 = Hmac<Sha256>;
type ApiError = (StatusCode, &'static str);

#[derive(Serialize)]
pub struct DialogueLine {
    pub id: usize,
    pub time: String,
    pub speaker: String,
    pub speaker_key: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct ThreadResponse {
    pub id: String,
    pub title: String,
    pub total_lines: usize,
    pub dialogue: Vec<DialogueLine>,
}

#[derive(Serialize)]
pub struct ThreadListItem {
    pub id: String,
    pub raw_id: String,
    pub title: String,
}

#[derive(Serialize)]
pub struct ThreadListResponse {
    pub count: usize,
    pub threads: Vec<ThreadListItem>,
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
        .route("/threads", get(list_threads))
        .route("/threads/{id}", get(get_thread))
        .route("/monitor", get(get_monitor))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorize_api,
        ));

    Router::new()
        .route("/threads", get(serve_miniapp))
        .route("/monitor", get(serve_monitor))
        .nest("/api", api)
        .with_state(state)
}

async fn serve_miniapp() -> impl IntoResponse {
    Html(MINIAPP_HTML)
}

async fn serve_monitor() -> impl IntoResponse {
    Html(MONITOR_HTML)
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

async fn list_threads() -> Result<Json<ThreadListResponse>, ApiError> {
    let threads_dir = paths::threads_dir();
    let mut items = Vec::new();

    if let Ok(entries) = std::fs::read_dir(threads_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                let title = if let Some(topic) = stem.strip_prefix("telegram-") {
                    if let Some((_, topic_id)) = topic.split_once("-topic-") {
                        format!("Telegram Topic #{topic_id}")
                    } else {
                        stem.to_string()
                    }
                } else if stem.len() > 18 {
                    stem[..18].to_string()
                } else {
                    stem.to_string()
                };

                items.push(ThreadListItem {
                    id: urlencoding_encode(stem),
                    raw_id: stem.to_string(),
                    title,
                });
            }
        }
    }

    let count = items.len();
    Ok(Json(ThreadListResponse {
        count,
        threads: items,
    }))
}

async fn get_thread(Path(id): Path<String>) -> Result<Json<ThreadResponse>, ApiError> {
    let target_id = if id == "active" {
        find_latest_telegram_thread().unwrap_or(id)
    } else {
        id
    };

    let events = load_thread_events(&target_id).map_err(|error| {
        tracing::warn!(thread_id = target_id, %error, "Failed to load Mini App thread");
        (StatusCode::NOT_FOUND, "Thread not found")
    })?;

    let mut title = "Thread Transcript".to_string();
    let mut dialogue = Vec::new();
    let mut line_id = 1;

    for event in events {
        match event {
            ThreadEvent::Meta { title: Some(t), .. } => {
                title = t;
            }
            ThreadEvent::Message { role, text, ts, .. } => {
                let speaker = if role == "user" { "You" } else { "Z" };
                let speaker_key = if role == "user" {
                    "speaker-user"
                } else {
                    "speaker-assistant"
                };

                let time = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&ts) {
                    dt.format("%I:%M %p").to_string()
                } else {
                    "--:--".to_string()
                };

                let clean_text = clean_message_text(&text);
                if !clean_text.is_empty() {
                    dialogue.push(DialogueLine {
                        id: line_id,
                        time,
                        speaker: speaker.to_string(),
                        speaker_key: speaker_key.to_string(),
                        text: clean_text,
                    });
                    line_id += 1;
                }
            }
            _ => {}
        }
    }

    Ok(Json(ThreadResponse {
        id: target_id,
        title,
        total_lines: dialogue.len(),
        dialogue,
    }))
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

fn find_latest_telegram_thread() -> Option<String> {
    let threads_dir = paths::threads_dir();
    let mut latest_file: Option<(String, std::time::SystemTime)> = None;

    if let Ok(entries) = std::fs::read_dir(threads_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.starts_with("telegram-")
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                && let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
            {
                let stem = path.file_stem()?.to_str()?.to_string();
                if latest_file.as_ref().is_none_or(|(_, m)| modified > *m) {
                    latest_file = Some((stem, modified));
                }
            }
        }
    }

    latest_file.map(|(name, _)| name)
}

fn urlencoding_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
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
        .map(|record| MonitorAgent {
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

    const SAMPLE_BOT_TOKEN: &str = "5768337691:AAH5YkoiEuPk8-FZa32hStHTqXiLPtAEhx8";
    const SAMPLE_AUTH_DATE: u64 = 1_662_771_648;
    const SAMPLE_INIT_DATA: &str = concat!(
        "query_id=AAHdF6IQAAAAAN0XohDhrOrc&",
        "user=%7B%22id%22%3A279058397%2C%22first_name%22%3A%22Vladislav%22%2C",
        "%22last_name%22%3A%22Kibenko%22%2C%22username%22%3A%22vdkfrost%22%2C",
        "%22language_code%22%3A%22ru%22%2C%22is_premium%22%3Atrue%7D&",
        "auth_date=1662771648&",
        "hash=c501b71e775f74ce10e377dea85a7ea24ecd640b223ea86dfe453e0eaed2e2b2"
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

        for path in ["/api/threads", "/api/threads/active", "/api/monitor"] {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .expect("request protected API route");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        for path in ["/threads", "/monitor"] {
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
