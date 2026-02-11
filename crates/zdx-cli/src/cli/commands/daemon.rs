//! Daemon command handler for scheduled automations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use zdx_core::core::thread_persistence::ThreadPersistenceOptions;
use zdx_core::{automations, config};

use super::automations as automation_commands;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct DaemonState {
    /// Last minute-bucket run per automation name.
    last_run_minute: BTreeMap<String, i64>,
}

/// Runs the daemon loop for scheduled automations.
///
/// # Errors
/// Returns an error if state loading/writing fails.
pub async fn run(root: &Path, config: &config::Config, poll_interval_secs: u64) -> Result<()> {
    let poll = Duration::from_secs(poll_interval_secs.max(1));
    let state_path = daemon_state_path();
    let mut state = load_state(&state_path)?;

    eprintln!(
        "Daemon started (root: {}, poll={}s)",
        root.display(),
        poll.as_secs()
    );

    let thread_opts = ThreadPersistenceOptions {
        thread_id: None,
        no_save: true,
    };

    loop {
        let now = Local::now();
        let current_bucket = now.timestamp() / 60;

        match automations::discover(root) {
            Ok(defs) => {
                for automation in defs {
                    let Some(schedule) = automation.schedule.as_deref() else {
                        continue;
                    };

                    match automations::schedule_matches_local_time(schedule, now) {
                        Ok(false) => continue,
                        Err(err) => {
                            eprintln!(
                                "Invalid schedule for automation '{}': {err:#}",
                                automation.name
                            );
                            continue;
                        }
                        Ok(true) => {}
                    }

                    if state.last_run_minute.get(&automation.name) == Some(&current_bucket) {
                        continue;
                    }

                    eprintln!("Running automation '{}'", automation.name);
                    match automation_commands::run_definition(
                        root,
                        &thread_opts,
                        config,
                        &automation,
                        automation_commands::RunTrigger::Daemon,
                    )
                    .await
                    {
                        Ok(()) => {
                            state
                                .last_run_minute
                                .insert(automation.name.clone(), current_bucket);
                            if let Err(err) = save_state(&state_path, &state) {
                                eprintln!("Failed to persist daemon state: {err:#}");
                            }
                        }
                        Err(err) => {
                            eprintln!("Automation '{}' failed: {err:#}", automation.name);
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("Failed to discover automations: {err:#}");
            }
        }

        tokio::time::sleep(poll).await;
    }
}

fn daemon_state_path() -> PathBuf {
    config::paths::zdx_home().join("automations_daemon_state.json")
}

fn load_state(path: &Path) -> Result<DaemonState> {
    if !path.exists() {
        return Ok(DaemonState::default());
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("read daemon state file {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parse daemon state file {}", path.display()))
}

fn save_state(path: &Path, state: &DaemonState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create daemon state dir {}", parent.display()))?;
    }

    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(state).context("serialize daemon state")?;
    fs::write(&tmp, body).with_context(|| format!("write state temp file {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "persist daemon state from {} to {}",
            tmp.display(),
            path.display()
        )
    })?;

    Ok(())
}
