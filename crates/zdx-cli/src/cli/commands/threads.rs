//! Thread command handlers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use zdx_engine::config;
use zdx_engine::core::thread_export::{self, ThreadExportOptions};
use zdx_engine::core::thread_persistence::{self, ThreadSummary};
use zdx_engine::core::usage_stats::{self, UsageTotals};

use super::stats::{format_cost, format_tokens};
use crate::modes;

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

pub fn list(include_children: bool) -> Result<()> {
    let threads = if include_children {
        thread_persistence::list_all_threads().context("list threads")?
    } else {
        thread_persistence::list_threads().context("list threads")?
    };
    if threads.is_empty() {
        println!("No threads found.");
    } else {
        for info in threads {
            let modified_str = info
                .modified
                .and_then(thread_persistence::format_timestamp)
                .unwrap_or_else(|| "unknown".to_string());
            let display_title = info.display_title();
            let origin = info
                .origin_kind
                .as_deref()
                .map_or_else(String::new, |kind| match info.parent_thread_id.as_deref() {
                    Some(parent) => {
                        format!(
                            "  [{kind} ← {}]",
                            thread_persistence::short_thread_id(parent)
                        )
                    }
                    None => format!("  [{kind}]"),
                });
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

    // Lineage (needs the full list so hidden child runs are visible here).
    let all = thread_persistence::list_all_threads().unwrap_or_default();

    if let Some(this) = all.iter().find(|s| s.id == id)
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

    print_child_runs(id, &all, config);
    Ok(())
}

/// Prints the child runs (subagents/helpers) spawned by thread `id`, each with
/// its token/cost totals. Subagent children are listed before helper children.
fn print_child_runs(id: &str, all: &[ThreadSummary], config: &config::Config) {
    let mut children: Vec<&ThreadSummary> = all
        .iter()
        .filter(|s| s.parent_thread_id.as_deref() == Some(id))
        .collect();
    if children.is_empty() {
        return;
    }
    // Subagents first, then helpers; `all` is already newest-first within each.
    children.sort_by_key(|s| {
        s.origin_kind
            .as_deref()
            .is_some_and(|kind| kind.starts_with("helper"))
    });

    println!("\n── Child runs ({}) ──", children.len());
    for child in children {
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

pub async fn resume(id: Option<String>, config: &config::Config) -> Result<()> {
    let thread_id = match id {
        Some(id) => id,
        None => thread_persistence::latest_thread_id()
            .context("find latest thread id")?
            .context("No threads found to resume")?,
    };

    let history = thread_persistence::load_thread_as_messages(&thread_id)
        .with_context(|| format!("load history for '{thread_id}'"))?;

    let thread = thread_persistence::Thread::with_id(thread_id.clone())
        .with_context(|| format!("open thread '{thread_id}'"))?;

    let root_path = PathBuf::from(".");
    modes::run_interactive_chat_with_history(config, Some(thread), history, root_path)
        .await
        .context("resume chat failed")?;

    Ok(())
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
