//! Dashboard bodies: the monitor snapshot and its live overlays (active
//! agents, subscription quotas), workspaces, and the usage report.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;

use super::{EngineViews, internal, invalid};
use crate::agent_activity::{self, RunStatus};
use crate::config::{Config, paths};
use crate::core::usage_report::{self, ProjectUsage, RunUsage, UsageReport};
use crate::orchestration::views::ViewError;
use crate::providers::subscription_quota::{self, QuotaError, SubscriptionQuota};
use crate::service::{self, Service};
use crate::workspace_config::{self, WorkspaceReport};
use crate::{background_activity, jobs};

const MONITOR_CACHE_TTL: Duration = Duration::from_secs(30);
const SUBSCRIPTION_CACHE_TTL: Duration = Duration::from_mins(5);

#[derive(Clone, Serialize)]
pub struct MonitorResponse {
    generated_at: String,
    services: Vec<MonitorService>,
    active_agents: Vec<MonitorAgent>,
    background_processes: Vec<MonitorBackgroundProcess>,
    /// Shared job summaries (`crate::jobs::summaries`).
    jobs: Vec<jobs::JobSummary>,
    /// Set when jobs could not be listed at all.
    jobs_error: Option<String>,
    config: MonitorConfig,
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
pub struct MonitorAgent {
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
    /// The resolved spec the default runs.
    model: String,
    /// `mode:<name>` when the default is a mode selection.
    model_mode: Option<String>,
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

/// Recorded usage in a day window, from the shared
/// `crate::core::usage_report`.
#[derive(Clone, Serialize)]
pub struct UsageResponse {
    days: u32,
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
    by_project: Vec<ProjectUsage>,
    by_run: Vec<RunUsage>,
    unindexed_threads: usize,
}

/// Longest usage window, in days.
pub const MAX_USAGE_DAYS: u32 = 365;

pub(super) struct CachedUsage {
    cached_at: Instant,
    response: UsageResponse,
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
pub struct MonitorSubscription {
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

pub(super) struct CachedMonitor {
    cached_at: Instant,
    response: MonitorResponse,
}

pub(super) struct CachedSubscriptions {
    cached_at: Instant,
    subscriptions: Vec<MonitorSubscription>,
}

#[derive(Serialize)]
pub struct WorkspacesResponse {
    workspaces: Vec<WorkspaceReport>,
}

impl EngineViews {
    /// Read-only model settings of every workspace (Telegram bindings + the
    /// views root), each value tagged with the layer that supplied it and what
    /// a reset of the workspace layer would fall back to. It reads a few small
    /// TOML files, so it is not cached.
    ///
    /// # Errors
    /// `Internal` when the workspaces cannot be read.
    pub async fn workspaces(&self) -> Result<WorkspacesResponse, ViewError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || workspace_config::report(Some(&root)))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "Mini App workspaces task failed");
                internal("Workspaces unavailable")
            })?
            .map(|workspaces| WorkspacesResponse { workspaces })
            .map_err(|error| {
                tracing::warn!(error = %format!("{error:#}"), "Failed to list Mini App workspaces");
                internal("Workspaces unavailable")
            })
    }

    /// Recorded usage for the last `days` days (default
    /// [`usage_report::DEFAULT_DAYS`], at most [`MAX_USAGE_DAYS`]), split by
    /// project and run kind. Cached per window for `MONITOR_CACHE_TTL`
    /// because a build syncs the usage cache.
    ///
    /// # Errors
    /// `Invalid` for a window out of range, `Internal` when usage cannot be
    /// aggregated.
    pub async fn usage(&self, days: Option<u32>) -> Result<UsageResponse, ViewError> {
        let days = days.unwrap_or(usage_report::DEFAULT_DAYS);
        if !(1..=MAX_USAGE_DAYS).contains(&days) {
            return Err(invalid("days must be between 1 and 365"));
        }
        let mut cache = self.usage_cache.lock().await;
        if let Some(cached) = cache
            .get(&days)
            .filter(|cached| cached.cached_at.elapsed() < MONITOR_CACHE_TTL)
        {
            return Ok(cached.response.clone());
        }
        let root = self.root.clone();
        let response = tokio::task::spawn_blocking(move || {
            let config = Config::load_layered(&paths::config_layer_paths_for(&root))?;
            usage_report::report(&config.model, Some(days))
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App usage task failed");
            internal("Usage unavailable")
        })?
        .map(|report| usage_response(days, report))
        .map_err(|error| {
            tracing::warn!(error = %format!("{error:#}"), "Mini App usage aggregation failed");
            internal("Usage unavailable")
        })?;
        cache.insert(
            days,
            CachedUsage {
                cached_at: Instant::now(),
                response: response.clone(),
            },
        );
        Ok(response)
    }

    /// The whole dashboard: the cached local snapshot, active agents read
    /// fresh, and the last known quotas (a refresh starts in the background
    /// when they are missing or old).
    ///
    /// # Errors
    /// `Internal` when the snapshot or the active agents cannot be read.
    pub async fn monitor(&self) -> Result<MonitorResponse, ViewError> {
        let mut response = self.cached_monitor().await?;
        response.active_agents = tokio::task::spawn_blocking(active_monitor_agents)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "Mini App active agents task failed");
                internal("Active agents unavailable")
            })?;
        self.attach_subscriptions(&mut response).await;
        Ok(response)
    }

    /// The local part of the dashboard built now, uncached, without
    /// `active_agents` and quotas (`subscriptions_status` is `pending`).
    ///
    /// # Errors
    /// `Internal` when it cannot be built.
    pub async fn monitor_snapshot(&self) -> Result<MonitorResponse, ViewError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || build_monitor_response(&root))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "View task failed");
                internal("Monitor snapshot unavailable")
            })?
            .map_err(|error| {
                tracing::warn!(error = %format!("{error:#}"), "Failed to build monitor view");
                internal("Monitor snapshot unavailable")
            })
    }

    /// Active runs from their markers, read fresh.
    ///
    /// # Errors
    /// `Internal` when the read task fails.
    pub async fn active_agents(&self) -> Result<Vec<MonitorAgent>, ViewError> {
        tokio::task::spawn_blocking(active_monitor_agents)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "View task failed");
                internal("Active agents unavailable")
            })
    }

    /// Live quotas of every stored account, fetched now. Slow (a network call
    /// per account); never on a request path.
    pub async fn subscription_quotas(&self) -> Vec<MonitorSubscription> {
        fetch_subscription_quotas().await
    }

    /// Builds (or reuses) the local part of the dashboard: services, jobs, and
    /// config (usage has its own body). Everything here is local state, so it
    /// is cached for [`MONITOR_CACHE_TTL`] and never waits on the network.
    ///
    /// Quotas are deliberately *not* cached with it — they are overlaid per
    /// request by `attach_subscriptions`, so a refresh that lands mid-window
    /// is served immediately instead of waiting for this cache to expire.
    async fn cached_monitor(&self) -> Result<MonitorResponse, ViewError> {
        let root = self.root.clone();
        self.cached_monitor_or_build(move || build_monitor_response(&root))
            .await
    }

    async fn cached_monitor_or_build(
        &self,
        build: impl FnOnce() -> anyhow::Result<MonitorResponse> + Send + 'static,
    ) -> Result<MonitorResponse, ViewError> {
        if let Some(cached) = self.fresh_monitor().await {
            return Ok(cached);
        }
        // Single-flight: the Mini App opens several views on `/api/monitor` at
        // once, and each build lists jobs and services. Later callers wait for
        // the first build and reuse it.
        let _build = self.monitor_build.lock().await;
        if let Some(cached) = self.fresh_monitor().await {
            return Ok(cached);
        }

        let response = tokio::task::spawn_blocking(build)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "Mini App monitor snapshot task failed");
                internal("Monitor snapshot unavailable")
            })?
            .map_err(|error| {
                tracing::warn!(error = %format!("{error:#}"), "Failed to build Mini App monitor snapshot");
                internal("Monitor snapshot unavailable")
            })?;

        *self.monitor_cache.write().await = Some(CachedMonitor {
            cached_at: Instant::now(),
            response: response.clone(),
        });
        Ok(response)
    }

    async fn fresh_monitor(&self) -> Option<MonitorResponse> {
        self.monitor_cache
            .read()
            .await
            .as_ref()
            .filter(|cached| cached.cached_at.elapsed() < MONITOR_CACHE_TTL)
            .map(|cached| cached.response.clone())
    }

    /// Overlays the last known quota snapshot onto a dashboard response and,
    /// when it is missing or past [`SUBSCRIPTION_CACHE_TTL`], starts a refresh
    /// in the background.
    ///
    /// This never awaits a provider call. A quota fetch talks to every stored
    /// OAuth account with a 10s per-request timeout and no shared deadline, so
    /// awaiting it here made a cold dashboard open stall for as long as the
    /// slowest provider — intermittently, since a warm cache hid it. The client
    /// polls every 30s and picks the values up on a later tick;
    /// `subscriptions_status` says which of "not fetched yet" and "no
    /// subscriptions" an empty list means.
    async fn attach_subscriptions(&self, response: &mut MonitorResponse) {
        let cached = self
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
            spawn_subscription_refresh(&self.subscription_refresh, &self.subscription_cache);
        }
    }
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

fn build_monitor_response(root: &std::path::Path) -> anyhow::Result<MonitorResponse> {
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

    Ok(MonitorResponse {
        generated_at: chrono::Utc::now().to_rfc3339(),
        services,
        // Overlaid per request by `EngineViews::monitor`: run markers are cheap to read
        // and go stale long before the rest of the snapshot.
        active_agents: Vec::new(),
        background_processes,
        jobs,
        jobs_error,
        config: monitor_config(&config),
        subscriptions: Vec::new(),
        subscriptions_status: "pending",
        subscriptions_age: None,
    })
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
fn spawn_subscription_refresh(
    refresh: &Arc<Mutex<()>>,
    cache: &Arc<Mutex<Option<CachedSubscriptions>>>,
) {
    let Ok(guard) = Arc::clone(refresh).try_lock_owned() else {
        return;
    };
    let cache = Arc::clone(cache);
    tokio::spawn(async move {
        let _guard = guard;
        let started = Instant::now();
        let subscriptions = fetch_subscription_quotas().await;
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            count = subscriptions.len(),
            "Refreshed Mini App subscription quotas"
        );
        *cache.lock().await = Some(CachedSubscriptions {
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
        model_mode: config.mode_selection(),
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

fn usage_response(days: u32, report: UsageReport) -> UsageResponse {
    let row = |item: crate::core::usage_stats::UsageRow| {
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
    let stats = report.stats;
    UsageResponse {
        days,
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
        by_project: report.by_project,
        by_run: report.by_run,
        unindexed_threads: report.unindexed_threads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            windows: vec![crate::providers::subscription_quota::QuotaWindow {
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
        let views = Arc::new(EngineViews::with_client(
            std::path::PathBuf::from("."),
            crate::orchestration::OrchestratorClient::at(
                std::env::temp_dir().join(format!("zdx-test-{}.sock", uuid::Uuid::new_v4())),
            ),
        ));
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = (0..4).map(|_| {
            let (views, builds) = (Arc::clone(&views), Arc::clone(&builds));
            tokio::spawn(async move {
                views
                    .cached_monitor_or_build(move || {
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
