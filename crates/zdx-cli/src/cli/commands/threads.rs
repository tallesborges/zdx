//! Thread command handlers.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;
use zdx_engine::config;
use zdx_engine::core::thread_export::{self, ThreadExportOptions};
use zdx_engine::core::thread_index;
use zdx_engine::core::thread_persistence::{self};
use zdx_engine::core::thread_trajectory::{
    ThreadTrajectoryReport, format_thread_trajectory_report, inspect_thread_trajectory,
};
use zdx_engine::core::usage_stats::{self, UsageTotals};

use super::stats::{format_cost, format_tokens};

/// Appends a message to an existing thread.
pub fn append(thread_id: &str, role: &str, text: &str) -> Result<()> {
    let event = match role {
        "user" => thread_persistence::ThreadEvent::user_message(text),
        "assistant" => thread_persistence::ThreadEvent::assistant_message_with_phase(
            text,
            Some("final_answer".to_string()),
        ),
        _ => anyhow::bail!("unsupported role '{role}' (use 'user' or 'assistant')"),
    };

    let mut thread =
        thread_persistence::Thread::with_id(thread_id.to_string()).context("open thread")?;
    thread.append(&event).context("append message")?;
    println!("Appended {role} message to thread '{thread_id}'.");
    Ok(())
}

/// Input options for `zdx threads search`.
#[derive(Debug, Clone)]
pub struct SearchCommandOptions {
    pub query: Option<String>,
    pub date: Option<String>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub limit: usize,
    pub json: bool,
}

/// Input options for `zdx threads tools`.
#[derive(Debug, Clone)]
pub struct ToolsCommandOptions {
    pub tool: Option<String>,
    pub failed: bool,
    pub date: Option<String>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub limit: usize,
    pub json: bool,
}

pub fn list(include_children: bool, workers_only: bool) -> Result<()> {
    let threads = if workers_only {
        thread_persistence::list_recent_workers(usize::MAX).context("list worker threads")?
    } else if include_children {
        thread_persistence::list_all_threads_cached().context("list threads")?
    } else {
        thread_persistence::list_threads().context("list threads")?
    };
    if threads.is_empty() {
        println!(
            "{}",
            if workers_only {
                "No worker threads found."
            } else {
                "No threads found."
            }
        );
    } else {
        for info in threads {
            let modified_str = info
                .modified
                .and_then(thread_persistence::format_timestamp)
                .unwrap_or_else(|| "unknown".to_string());
            let display_title = info.display_title();
            let origin = if info.is_worker() {
                info.parent_thread_id
                    .as_deref()
                    .map_or_else(String::new, |parent| {
                        format!(
                            "  [worker ← {}]",
                            thread_persistence::short_thread_id(parent)
                        )
                    })
            } else {
                info.origin_kind
                    .as_deref()
                    .map_or_else(String::new, |kind| match info.parent_thread_id.as_deref() {
                        Some(parent) => {
                            format!(
                                "  [{kind} ← {}]",
                                thread_persistence::short_thread_id(parent)
                            )
                        }
                        None => format!("  [{kind}]"),
                    })
            };
            println!("{display_title}  {}  {modified_str}{origin}", info.id);
        }
    }
    Ok(())
}

pub fn show(id: &str, config: &config::Config) -> Result<()> {
    let events = thread_persistence::load_thread_events(id)
        .with_context(|| format!("load thread '{id}'"))?;
    if events.is_empty() {
        println!("Thread '{id}' is empty or not found.");
        return Ok(());
    }

    // Lineage: one indexed row for this thread, one indexed query for its
    // children — never a full-corpus listing.
    if let Ok(Some(this)) = thread_persistence::read_thread_summary_cached(id)
        && let Some(kind) = this.origin_kind.as_deref()
    {
        let label = this
            .subagent_name
            .as_deref()
            .map_or_else(|| kind.to_string(), |name| format!("{kind}/{name}"));
        match this.parent_thread_id.as_deref() {
            Some(parent) => println!(
                "↳ Child run [{label}] of {}\n",
                thread_persistence::short_thread_id(parent)
            ),
            None => println!("↳ Child run [{label}]\n"),
        }
    }

    println!("{}", thread_persistence::format_transcript(&events));

    print_child_runs(id, config);
    Ok(())
}

#[derive(Serialize)]
struct ThreadInspectOutput<'a> {
    thread_id: &'a str,
    title: Option<String>,
    trajectory: ThreadTrajectoryReport,
}

pub fn inspect(id: &str, json: bool) -> Result<()> {
    let events = thread_persistence::load_thread_events(id)
        .with_context(|| format!("load thread '{id}'"))?;
    if events.is_empty() {
        if json {
            let output = ThreadInspectOutput {
                thread_id: id,
                title: None,
                trajectory: inspect_thread_trajectory(&events),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
            return Ok(());
        }
        println!("Thread '{id}' is empty or not found.");
        return Ok(());
    }

    let title = thread_persistence::extract_title_from_events(&events);
    let trajectory = inspect_thread_trajectory(&events);
    if json {
        let output = ThreadInspectOutput {
            thread_id: id,
            title,
            trajectory,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    let title = title.map_or_else(String::new, |title| format!(" · {title}"));
    println!("Thread: {id}{title}");
    for line in format_thread_trajectory_report(&trajectory) {
        println!("{line}");
    }
    Ok(())
}

/// Prints the child runs (subagents/helpers) spawned by thread `id`, each with
/// its token/cost totals. Subagent children are listed before helper children.
fn print_child_runs(id: &str, config: &config::Config) {
    let mut children = thread_persistence::child_runs(id).unwrap_or_default();
    if children.is_empty() {
        return;
    }
    // Subagents first, then helpers; the query is already newest-first within
    // each, and `sort_by_key` is stable.
    children.sort_by_key(|s| {
        s.origin_kind
            .as_deref()
            .is_some_and(|kind| kind.starts_with("helper"))
    });

    println!("\n── Child runs ({}) ──", children.len());
    for child in &children {
        let kind = child.origin_kind.as_deref().unwrap_or("child");
        let name = child
            .subagent_name
            .as_deref()
            .map_or_else(String::new, |n| format!("/{n}"));
        let (tokens, cost) = match usage_stats::thread_usage_stats(&child.id, &config.model) {
            Ok(stats) => (
                format_tokens(stats.totals.tokens()),
                thread_cost_cell(&stats.totals),
            ),
            Err(_) => ("—".to_string(), "—".to_string()),
        };
        println!(
            "  {:<22} {:<10} {:>8} tok  {:>10}",
            format!("{kind}{name}"),
            thread_persistence::short_thread_id(&child.id),
            tokens,
            cost,
        );
    }
}

/// Cost cell for a whole thread's totals: billed USD, `subscription`,
/// `unknown`, or `$0.00` when there is nothing billable.
fn thread_cost_cell(t: &UsageTotals) -> String {
    if t.billed_usd > 0.0 {
        format_cost(t.billed_usd)
    } else if t.subscription_tokens > 0 {
        "subscription".to_string()
    } else if t.unknown_pricing_rows > 0 {
        "unknown".to_string()
    } else {
        format_cost(0.0)
    }
}

pub fn rename(id: &str, title: &str) -> Result<()> {
    let normalized = thread_persistence::set_thread_title(id, Some(title.to_string()))
        .with_context(|| format!("rename thread '{id}'"))?;
    let display_title = normalized.unwrap_or_else(|| thread_persistence::short_thread_id(id));
    println!("Renamed thread {id} → {display_title}");
    Ok(())
}

pub fn export(force: bool, dry_run: bool) -> Result<()> {
    let summary = thread_export::export_threads_incremental(ThreadExportOptions { force, dry_run })
        .context("export threads")?;

    println!(
        "Thread exports: exported={}, skipped={}, removed={}, failed={}",
        summary.exported, summary.skipped, summary.removed, summary.failed
    );

    Ok(())
}

/// Brings the thread index up to date without touching exports or the memory
/// index.
///
/// Run from `just install` so a `SCHEMA_VERSION` bump pays its rebuild there
/// rather than inside the first request after a restart, which would otherwise
/// block the bot while it lists recent threads.
pub fn reindex() -> Result<()> {
    let started = std::time::Instant::now();
    let summary = thread_index::reindex().context("reindex threads")?;

    println!(
        "Thread index: {} files, {} read, {} updated, {} removed ({:.2}s)",
        summary.files_enumerated,
        summary.metas_read,
        summary.rows_upserted,
        summary.rows_removed,
        started.elapsed().as_secs_f64()
    );

    Ok(())
}

pub fn resume(id: Option<String>, root: &Path) -> Result<()> {
    let thread_id = match id {
        Some(id) => id,
        None => thread_persistence::latest_thread_id()
            .context("find latest thread id")?
            .context("No threads found to resume")?,
    };
    zdx_engine::orchestration::ensure_direct_run_allowed(&thread_id)?;
    super::chat::tui(root, Some(&thread_id), None)
}

pub fn search(options: SearchCommandOptions) -> Result<()> {
    let date = parse_date_filter(options.date.as_deref(), "date")?;
    let date_start = parse_date_filter(options.date_start.as_deref(), "date-start")?;
    let date_end = parse_date_filter(options.date_end.as_deref(), "date-end")?;

    if let (Some(start), Some(end)) = (date_start, date_end)
        && start > end
    {
        anyhow::bail!("--date-start must be on or before --date-end");
    }

    let search_options = thread_persistence::ThreadSearchOptions {
        query: options.query,
        date,
        date_start,
        date_end,
        limit: options.limit.max(1),
        exclude_thread_id: None,
    };

    let results = thread_persistence::search_threads(&search_options).context("search threads")?;

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).context("serialize thread search results")?
        );
        return Ok(());
    }

    if results.is_empty() {
        println!("No threads found matching the criteria.");
        return Ok(());
    }

    for result in results {
        println!("[{}] {}", result.thread_id, result.display_title());
        if let Some(activity_at) = &result.activity_at {
            println!("  Activity: {activity_at}");
        }
        if !result.preview.is_empty() {
            println!("  Preview: {}", result.preview);
        }
        println!();
    }

    Ok(())
}

pub fn tools(options: ToolsCommandOptions) -> Result<()> {
    let date = parse_date_filter(options.date.as_deref(), "date")?;
    let date_start = parse_date_filter(options.date_start.as_deref(), "date-start")?;
    let date_end = parse_date_filter(options.date_end.as_deref(), "date-end")?;

    if let (Some(start), Some(end)) = (date_start, date_end)
        && start > end
    {
        anyhow::bail!("--date-start must be on or before --date-end");
    }

    let tool_options = thread_persistence::ThreadToolSearchOptions {
        tool_name: options.tool,
        failed_only: options.failed,
        date,
        date_start,
        date_end,
        limit: options.limit.max(1),
    };

    let results =
        thread_persistence::search_thread_tools(&tool_options).context("search thread tools")?;

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).context("serialize thread tool results")?
        );
        return Ok(());
    }

    if results.is_empty() {
        println!("No tool calls found matching the criteria.");
        return Ok(());
    }

    for result in results {
        println!("[{}] {}", result.thread_id, result.display_title());
        if let Some(tool_ts) = &result.tool_ts {
            println!("  Time: {tool_ts}");
        }
        println!("  Tool: {} ({})", result.tool_name, result.status);
        println!("  Args: {}", result.args_summary);
        if let Some(error_message) = &result.error_message {
            let error_code = result.error_code.as_deref().unwrap_or("error");
            println!("  Error: {error_code} - {error_message}");
            if let Some(details) = &result.error_details {
                println!("  Details: {details}");
            }
        }
        println!();
    }

    Ok(())
}

fn parse_date_filter(raw: Option<&str>, flag: &str) -> Result<Option<NaiveDate>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--{flag} cannot be empty");
    }

    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .with_context(|| format!("invalid --{flag} value '{trimmed}' (expected YYYY-MM-DD)"))
        .map(Some)
}

/// Reads a JSON value from `path`, or stdin for `-`.
fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    let text = if path.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin()).context("read stdin")?
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    };
    serde_json::from_str(&text).context("parse JSON")
}

fn print_reply(reply: &zdx_engine::orchestration::protocol::Reply) -> Result<()> {
    println!("{}", serde_json::to_string(reply)?);
    Ok(())
}

/// Runs a thread write in the engine service while it runs (it may own
/// the thread's queue or a held turn), and directly only when no service is
/// running. A running service that does not answer fails the command.
async fn via_service(
    request: zdx_engine::orchestration::protocol::Request,
    direct: impl FnOnce() -> Result<zdx_engine::orchestration::protocol::Reply>,
) -> Result<()> {
    use zdx_engine::orchestration::client::{EngineClient, ok};

    let reply = match EngineClient::running() {
        Some(client) => ok(client.call(&request).await?)?,
        None => direct()?,
    };
    print_reply(&reply)
}

/// `zdx threads update`: the CLI twin of the `update_thread` socket op.
pub async fn update(id: &str, file: &std::path::Path) -> Result<()> {
    use zdx_engine::orchestration::protocol::{Reply, Request};
    use zdx_engine::orchestration::thread_ops;

    let mut request: thread_ops::UpdateThread = read_json(file)?;
    request.thread_id = id.to_string();
    let direct = request.clone();
    via_service(Request::UpdateThread(request), move || {
        Ok(Reply::ThreadUpdated(thread_ops::update_thread(&direct)?))
    })
    .await
}

/// `zdx threads record-delivery`: the CLI twin of `record_delivery`.
pub async fn record_delivery(
    id: &str,
    file: &std::path::Path,
    turn_id: Option<u64>,
    holder: Option<String>,
) -> Result<()> {
    use zdx_engine::orchestration::protocol::{Reply, Request};

    let record: zdx_engine::core::delivery::DeliveryRecord = read_json(file)?;
    let direct = record.clone();
    via_service(
        Request::RecordDelivery {
            thread_id: id.to_string(),
            record,
            turn_id,
            holder,
        },
        || {
            let user_summary = zdx_engine::orchestration::thread_ops::record_delivery(id, direct)?;
            Ok(Reply::DeliveryRecorded { user_summary })
        },
    )
    .await
}

/// `zdx threads reset`: the CLI twin of `reset_thread`. Goes through the
/// service while it runs, so a queued or held thread is never deleted.
pub async fn reset(id: &str) -> Result<()> {
    use zdx_engine::orchestration::protocol::{Reply, Request};

    via_service(
        Request::ResetThread {
            thread_id: id.to_string(),
        },
        || {
            zdx_engine::orchestration::thread_ops::reset_thread(id)?;
            Ok(Reply::Ok)
        },
    )
    .await
}

/// `zdx threads resolve`: the CLI twin of `resolve_thread`.
pub fn resolve(chat_id: i64, topic_id: Option<i64>) -> Result<()> {
    let route = zdx_engine::orchestration::thread_ops::resolve_thread(chat_id, topic_id)?;
    print_reply(&zdx_engine::orchestration::protocol::Reply::ThreadRoute(
        route,
    ))
}
