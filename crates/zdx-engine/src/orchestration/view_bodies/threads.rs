//! Thread bodies: the browser list, one thread's transcript projection, its
//! trajectory, and its workers.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zdx_types::tool_view::{ToolView, ViewLimits, tool_view};

use super::{
    EngineViews, LiveWorkers, internal, invalid, refused, resolve_alias_id, resolve_telegram_link,
    resolve_thread_param, valid_thread_id,
};
use crate::agent_activity::{self, RunStatus};
use crate::core::artifacts::{self, ArtifactAttachment};
use crate::core::events::NoticeKind;
use crate::core::thread_index;
use crate::core::thread_persistence::{self, ThreadEvent, load_thread_events};
use crate::core::thread_trajectory::{
    LiveToolSpan, ThreadTrajectoryReport, inspect_thread_trajectory_with_live,
};
use crate::core::workers::{WorkerSnapshot, WorkerStatus};
use crate::models::ModelOption;
use crate::orchestration::protocol::ErrorCode;
use crate::orchestration::views::ViewError;
use crate::providers::ReplayToken;
use crate::service;

/// How many recent threads the Mini App browser lists. Applied in SQL.
const THREAD_LIST_LIMIT: usize = 50;

/// Reads a thread's events for [`EngineViews::thread`]. A surface that
/// delivers replies wraps the read to sample its own delivery state around
/// it; the bool it returns is the body's `delivering`.
pub type EventsLoad<'a> = &'a dyn Fn() -> anyhow::Result<Vec<ThreadEvent>>;

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
    /// A reply delivery for this thread is running in the process that
    /// serves it. Clients keep polling while it is set, so its pending warning
    /// is replaced by the outcome without a reload. A pending record left by a
    /// crash is never reported as running. Always false where nothing
    /// delivers.
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

/// One worker of an orchestrator thread.
///
/// Rows come from two sources that carry different amounts of truth. A live row
/// is backed by the orchestrator service's in-memory worker state and has real
/// status and progress.
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
        /// Input-only view for a call with no result in the thread
        /// (interrupted, or still running); a call with a result carries its
        /// view on the result instead.
        #[serde(skip_serializing_if = "Option::is_none")]
        view: Option<ToolView>,
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
        /// Display projection from the input alone (head, edit diff).
        view: ToolView,
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
        /// Display projection of the call (`zdx_types::tool_view`); `output`
        /// stays the raw envelope.
        view: ToolView,
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

/// Which threads the browser lists.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ThreadListKind {
    /// Every top-level thread, workers included.
    #[default]
    All,
    /// Only orchestrator workers.
    Workers,
}

/// Lineage and worker counts attached to a single thread response.
#[derive(Default)]
struct ThreadLineage {
    parent_thread_id: Option<String>,
    parent_title: Option<String>,
    worker_count: usize,
}

/// `active` resolved and checked, as every thread body takes its id.
fn target_thread(id: String) -> Result<String, ViewError> {
    let target_id = resolve_thread_param(id);
    if !valid_thread_id(&target_id) {
        return Err(invalid("Invalid thread ID"));
    }
    Ok(target_id)
}

impl EngineViews {
    /// The most recently active top-level threads for the browser, or only
    /// orchestrator workers with [`ThreadListKind::Workers`].
    ///
    /// # Errors
    /// `Internal` when the thread list cannot be read.
    pub async fn threads(&self, kind: ThreadListKind) -> Result<ThreadListResponse, ViewError> {
        let manager = Arc::clone(&self.workers);
        tokio::task::spawn_blocking(move || {
            let now = SystemTime::now();
            let summaries = match kind {
                ThreadListKind::All => thread_persistence::list_recent_threads(THREAD_LIST_LIMIT),
                ThreadListKind::Workers => {
                    thread_persistence::list_recent_workers(THREAD_LIST_LIMIT)
                }
            }
            .map_err(|error| {
                tracing::warn!(error = %format!("{error:#}"), "Failed to list Mini App threads");
                internal("Thread list failed")
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
                        Path::new(root)
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
            Ok(ThreadListResponse { threads })
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App thread list task failed");
            internal("Thread list failed")
        })?
    }

    /// One thread's transcript projection, with the thread its events come
    /// from (`active` resolved, then one alias hop).
    ///
    /// `observe` reads the events through `load` and says whether a reply
    /// delivery is running for the source thread (the body's `delivering`);
    /// a surface that delivers nothing answers `(load(), false)`. With
    /// `after`, only activity at or after that cursor is returned, unless a
    /// delivery outcome replaces a record the client already holds.
    ///
    /// # Errors
    /// `Invalid` for a bad id, `NotFound` when the thread cannot be read.
    pub async fn thread<O>(
        &self,
        id: String,
        after: Option<usize>,
        observe: O,
    ) -> Result<(ThreadResponse, String), ViewError>
    where
        O: FnOnce(&str, EventsLoad<'_>) -> (anyhow::Result<Vec<ThreadEvent>>, bool)
            + Send
            + 'static,
    {
        let workers = Arc::clone(&self.workers);
        tokio::task::spawn_blocking(move || {
            let target_id = target_thread(id)?;

            // A resumed or mirror topic is an alias: its events, and the reply
            // deliveries the bot tracks, live under the source thread. The
            // response keeps the requested id and its topic link.
            let source_id = resolve_alias_id(&target_id);
            // Sampled around the read so the flag can never be false while the
            // events still show a pending record.
            let (events, delivering) = observe(&source_id, &|| load_thread_events(&source_id));
            let events = events.map_err(|error| {
                tracing::warn!(thread_id = source_id, error = %format!("{error:#}"), "Failed to load Mini App thread");
                refused(ErrorCode::NotFound, "Thread not found")
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
            if let Some(after) = after
                && !replaces_seen_delivery(&events_for_cursor, after)
            {
                response.activity.retain(|item| item.sequence() >= after);
                response.partial = true;
            }
            Ok((response, source_id))
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App thread load task failed");
            internal("Thread load failed")
        })?
    }

    /// The thread's trajectory (`core::thread_trajectory`) with the calls of
    /// its running round as live spans.
    ///
    /// # Errors
    /// `Invalid` for a bad id, `NotFound` when the thread cannot be read.
    pub async fn trajectory(&self, id: String) -> Result<ThreadTrajectoryReport, ViewError> {
        tokio::task::spawn_blocking(move || {
            let target_id = target_thread(id)?;

            let events = load_thread_events(&target_id).map_err(|error| {
                tracing::warn!(
                    thread_id = target_id,
                    error = %format!("{error:#}"),
                    "Failed to load Mini App thread trajectory"
                );
                refused(ErrorCode::NotFound, "Thread not found")
            })?;
            let live_tools = active_trajectory_tools(&target_id, events.len());
            Ok(inspect_thread_trajectory_with_live(
                &events,
                &live_tools,
                chrono::Utc::now(),
            ))
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App thread trajectory task failed");
            internal("Thread trajectory failed")
        })?
    }

    /// The workers of an orchestrator thread (one alias hop resolved).
    ///
    /// # Errors
    /// `Invalid` for a bad id.
    pub async fn thread_workers(&self, id: String) -> Result<WorkersResponse, ViewError> {
        let workers = Arc::clone(&self.workers);
        tokio::task::spawn_blocking(move || {
            let target_id = target_thread(id)?;
            Ok(WorkersResponse {
                workers: collect_workers(&workers, &resolve_alias_id(&target_id)),
            })
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Mini App worker list task failed");
            internal("Worker list failed")
        })?
    }
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

/// Resolves the orchestrator a thread belongs to and how many workers it owns.
///
/// Both halves are indexed lookups: the parent comes off the thread's own
/// cached summary, and the worker count is the same union the Workers pane
/// renders, so the tab never appears for a thread with nothing behind it.
fn resolve_lineage(workers: &LiveWorkers, thread_id: &str) -> ThreadLineage {
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
/// - The orchestrator service holds live state, but only for its own process.
///   Everything it knows disappears when it restarts.
/// - `thread_meta.parent_thread_id` survives restarts, but records only that a
///   worker existed. It restores *discovery*, never status.
///
/// Persisted children are filtered by the canonical worker rule
/// (`ThreadSummary::is_worker`), because the same parent link is used by
/// subagent and helper runs, which are not workers and must not show up here.
fn collect_workers(workers: &LiveWorkers, owner_thread_id: &str) -> Vec<WorkerItem> {
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
                Path::new(root)
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
        let name = tool.name.to_ascii_lowercase();
        let view = tool_view(&name, &tool.input, None, None, ViewLimits::DISPLAY);
        response.activity.push(ThreadActivity::ToolRunning {
            sequence,
            time: event_time(&started_at),
            ts: started_at.clone(),
            started_at,
            running_for,
            id: tool.id,
            name,
            summary: tool.summary,
            input: tool.input,
            output_tail: tool.output_tail,
            completed_at: tool.completed_at,
            ok: tool.ok,
            duration_ms: tool.duration_ms,
            view,
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
    let superseded = thread_persistence::superseded_deliveries(&events);
    // Tool calls by id, so a result's view knows its tool and input.
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();

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
            } => {
                calls.insert(id.clone(), (name.clone(), input.clone()));
                activity.push(ThreadActivity::ToolUse {
                    sequence,
                    time: event_time(&ts),
                    ts,
                    id,
                    name,
                    input,
                    view: None,
                });
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                duration_ms,
                started_at,
                completed_at,
                ts,
            } => {
                let (name, input) = calls.remove(&tool_use_id).unwrap_or_default();
                // The app keeps the raw `output` for the complete text.
                let view = tool_view(
                    &name,
                    &input,
                    Some(&output),
                    duration_ms,
                    ViewLimits::DISPLAY,
                );
                activity.push(ThreadActivity::ToolResult {
                    sequence,
                    time: event_time(&ts),
                    ts,
                    tool_use_id,
                    ok,
                    duration_ms,
                    started_at,
                    completed_at,
                    output,
                    view,
                });
            }
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
                if let Some(message) = crate::core::delivery::notice_text(&record) {
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
                message: crate::core::posted::notice_text(&post),
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

    // What is left in `calls` never got a result.
    for item in &mut activity {
        if let ThreadActivity::ToolUse {
            id,
            name,
            input,
            view,
            ..
        } = item
            && calls.contains_key(id.as_str())
        {
            *view = Some(tool_view(name, input, None, None, ViewLimits::DISPLAY));
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
        assert!(replaces_seen_delivery(&deliveries, 6));
        // Both already seen, or both new: a delta is enough.
        assert!(!replaces_seen_delivery(&deliveries, 10));
        assert!(!replaces_seen_delivery(&deliveries, 0));
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
                id_origin: crate::providers::IdOrigin::Real,
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
    fn tool_results_carry_the_shared_view_of_their_call() {
        let events = vec![
            ThreadEvent::ToolUse {
                id: "call-1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "cd /repo && sleep 99"}),
                id_origin: crate::providers::IdOrigin::Real,
                replay: None,
                ts: "2026-08-24T10:00:02Z".to_string(),
            },
            ThreadEvent::ToolResult {
                tool_use_id: "call-1".to_string(),
                output: serde_json::json!({"ok": true, "data": {
                    "stdout": "partial\n", "stderr": "", "exit_code": -1, "timed_out": true
                }}),
                ok: true,
                duration_ms: Some(1500),
                started_at: None,
                completed_at: None,
                ts: "2026-08-24T10:00:03Z".to_string(),
            },
            // Interrupted: no result ever landed.
            ThreadEvent::ToolUse {
                id: "call-2".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                id_origin: crate::providers::IdOrigin::Real,
                replay: None,
                ts: "2026-08-24T10:00:04Z".to_string(),
            },
        ];

        let response = project_thread(
            "thread-1".to_string(),
            events,
            None,
            ThreadLineage::default(),
        );
        let use_json = serde_json::to_value(&response.activity[0]).expect("serialize tool use");
        assert!(
            use_json.get("view").is_none(),
            "a call with a result has its view on the result"
        );
        let interrupted = serde_json::to_value(&response.activity[2]).expect("serialize tool use");
        assert_eq!(interrupted["view"]["outcome"], "pending");
        assert_eq!(interrupted["view"]["head"]["path"], "src/lib.rs");

        let json = serde_json::to_value(&response.activity[1]).expect("serialize tool result");

        assert_eq!(
            json["output"]["data"]["stdout"], "partial\n",
            "raw output kept"
        );
        let view = &json["view"];
        assert_eq!(view["outcome"], "timed_out");
        assert_eq!(view["head"]["kind"], "shell");
        assert_eq!(view["head"]["cwd"], "/repo");
        assert_eq!(view["head"]["command"], "sleep 99");
        assert_eq!(view["body"]["streams"][0]["text"], "partial\n");
        assert_eq!(view["hint"]["text"], "timed out");
        assert_eq!(view["status"][0]["kind"], "outcome");
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
        let _home = crate::test_support::temp_zdx_home();
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
}
