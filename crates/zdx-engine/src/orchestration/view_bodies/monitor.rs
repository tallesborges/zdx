//! Dashboard bodies: the monitor snapshot, active agents, workspaces, and the
//! usage report. Each is its own view; the surface (the gateway's Mini App)
//! composes and caches the dashboard and adds quotas and job summaries, which
//! it reads with the shared Go packages (`apps/shared/quota`,
//! `apps/shared/jobs`).

use std::time::{Duration, Instant};

use serde::Serialize;

use super::{EngineViews, internal, invalid};
use crate::agent_activity::{self, RunStatus};
use crate::background_activity;
use crate::config::{Config, paths};
use crate::core::usage_report::{self, ProjectUsage, RunUsage, UsageReport};
use crate::orchestration::views::ViewError;
use crate::service::{self, Service};
use crate::workspace_config::{self, WorkspaceReport};

const MONITOR_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Serialize)]
pub struct MonitorResponse {
    generated_at: String,
    services: Vec<MonitorService>,
    active_agents: Vec<MonitorAgent>,
    background_processes: Vec<MonitorBackgroundProcess>,
    config: MonitorConfig,
    /// Quotas are not part of the snapshot: always `[]`, `"pending"` and
    /// null here, and the surface overlays its own (the gateway's quota cache
    /// sets `"ready"`, `"pending"` or `"stale"` and the age). The client must
    /// not read an empty `pending` list as "no subscriptions".
    subscriptions: [(); 0],
    subscriptions_status: &'static str,
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

#[derive(Serialize)]
pub struct WorkspacesResponse {
    workspaces: Vec<WorkspaceReport>,
    /// The global config file on its own: what a write to it changes.
    global: workspace_config::GlobalReport,
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
        tokio::task::spawn_blocking(move || {
            workspace_config::report(Some(&root))
                .map(|workspaces| (workspaces, workspace_config::global_report()))
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App workspaces task failed");
            internal("Workspaces unavailable")
        })?
        .map(|(workspaces, global)| WorkspacesResponse { workspaces, global })
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

    Ok(MonitorResponse {
        generated_at: chrono::Utc::now().to_rfc3339(),
        services,
        // A view of its own (`active_agents`): run markers go stale long
        // before the rest of the snapshot, so a surface reads them fresh.
        active_agents: Vec::new(),
        background_processes,
        config: monitor_config(&config),
        subscriptions: [],
        subscriptions_status: "pending",
        subscriptions_age: None,
    })
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
}
