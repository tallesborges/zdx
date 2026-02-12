//! Thread command handlers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use zdx_core::config;
use zdx_core::core::thread_persistence;

use crate::modes;

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

pub fn list() -> Result<()> {
    let threads = thread_persistence::list_threads().context("list threads")?;
    if threads.is_empty() {
        println!("No threads found.");
    } else {
        for info in threads {
            let modified_str = info
                .modified
                .and_then(thread_persistence::format_timestamp)
                .unwrap_or_else(|| "unknown".to_string());
            let display_title = info.display_title();
            println!("{}  {}  {}", display_title, info.id, modified_str);
        }
    }
    Ok(())
}

pub fn show(id: &str) -> Result<()> {
    let events = thread_persistence::load_thread_events(id)
        .with_context(|| format!("load thread '{id}'"))?;
    if events.is_empty() {
        println!("Thread '{id}' is empty or not found.");
    } else {
        println!("{}", thread_persistence::format_transcript(&events));
    }
    Ok(())
}

pub fn rename(id: &str, title: &str) -> Result<()> {
    let normalized = thread_persistence::set_thread_title(id, Some(title.to_string()))
        .with_context(|| format!("rename thread '{id}'"))?;
    let display_title = normalized.unwrap_or_else(|| thread_persistence::short_thread_id(id));
    println!("Renamed thread {id} → {display_title}");
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

    let query_supplied = search_options
        .query
        .as_deref()
        .is_some_and(|q| !q.trim().is_empty());

    for result in results {
        println!("[{}] {}", result.thread_id, result.display_title());
        if let Some(activity_at) = &result.activity_at {
            println!("  Activity: {activity_at}");
        }
        if !result.preview.is_empty() {
            println!("  Preview: {}", result.preview);
        }
        if query_supplied {
            println!("  Score: {}", result.score);
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
