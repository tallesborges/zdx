//! Session command handlers.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::core::session;
use crate::{config, ui};

pub fn list() -> Result<()> {
    let sessions = session::list_sessions().context("list sessions")?;
    if sessions.is_empty() {
        println!("No sessions found.");
    } else {
        for info in sessions {
            let modified_str = info
                .modified
                .and_then(session::format_timestamp)
                .unwrap_or_else(|| "unknown".to_string());
            println!("{}  {}", info.id, modified_str);
        }
    }
    Ok(())
}

pub fn show(id: &str) -> Result<()> {
    let events = session::load_session(id).with_context(|| format!("load session '{id}'"))?;
    if events.is_empty() {
        println!("Session '{}' is empty or not found.", id);
    } else {
        println!("{}", session::format_transcript(&events));
    }
    Ok(())
}

pub async fn resume(id: Option<String>, config: &config::Config) -> Result<()> {
    let session_id = match id {
        Some(id) => id,
        None => session::latest_session_id()
            .context("find latest session id")?
            .context("No sessions found to resume")?,
    };

    let history = session::load_session_as_messages(&session_id)
        .with_context(|| format!("load history for '{session_id}'"))?;

    let session = session::Session::with_id(session_id.clone())
        .with_context(|| format!("open session '{session_id}'"))?;

    let root_path = PathBuf::from(".");
    ui::run_interactive_chat_with_history(config, Some(session), history, root_path)
        .await
        .context("resume chat failed")?;

    Ok(())
}
