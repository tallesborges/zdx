//! Thread command handlers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_core::config;
use zdx_core::core::thread_persistence;

use crate::modes;

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
        println!("Thread '{}' is empty or not found.", id);
    } else {
        println!("{}", thread_persistence::format_transcript(&events));
    }
    Ok(())
}

pub fn rename(id: &str, title: &str) -> Result<()> {
    let normalized = thread_persistence::set_thread_title(id, Some(title.to_string()))
        .with_context(|| format!("rename thread '{id}'"))?;
    let display_title = normalized.unwrap_or_else(|| thread_persistence::short_thread_id(id));
    println!("Renamed thread {} → {}", id, display_title);
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
