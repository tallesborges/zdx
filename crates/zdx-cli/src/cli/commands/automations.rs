//! Automation command handlers.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use zdx_core::automations::{self, AutomationDefinition};
use zdx_core::config;
use zdx_core::core::thread_persistence::ThreadPersistenceOptions;

use super::exec;

/// Trigger source for automation runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunTrigger {
    /// User-triggered run via `zdx automations run <name>`.
    Manual,
    /// Scheduled run via `zdx daemon`.
    Daemon,
}

#[derive(Debug, Serialize, Deserialize)]
struct AutomationRunRecord {
    automation: String,
    trigger: RunTrigger,
    thread_id: Option<String>,
    attempt: u32,
    max_attempts: u32,
    started_at: String,
    finished_at: String,
    duration_ms: u64,
    ok: bool,
    error: Option<String>,
    schedule: Option<String>,
    model: Option<String>,
}

const ERROR_SUMMARY_MAX_LEN: usize = 400;

/// Prints automation run history from JSONL.
pub fn runs(name: Option<&str>) -> Result<()> {
    let path = runs_log_path();
    if !path.exists() {
        println!("No automation runs found.");
        return Ok(());
    }

    let records = read_run_records(&path)?;
    let filtered: Vec<&AutomationRunRecord> = if let Some(raw) = name {
        let needle = raw.trim();
        if needle.is_empty() {
            Vec::new()
        } else {
            records.iter().filter(|r| r.automation == needle).collect()
        }
    } else {
        records.iter().collect()
    };

    if filtered.is_empty() {
        if let Some(name) = name {
            println!("No runs found for automation '{}'.", name.trim());
        } else {
            println!("No automation runs found.");
        }
        return Ok(());
    }

    for record in filtered.into_iter().rev() {
        println!(
            "{} | {} | {} | {} | {}ms | attempt {}/{}",
            record.finished_at,
            record.automation,
            match record.trigger {
                RunTrigger::Manual => "manual",
                RunTrigger::Daemon => "daemon",
            },
            if record.ok { "ok" } else { "failed" },
            record.duration_ms,
            record.attempt,
            record.max_attempts
        );
        if let Some(err) = &record.error {
            println!("  error: {err}");
        }
    }

    Ok(())
}

/// Lists discovered automations.
pub fn list(root: &Path) -> Result<()> {
    let automations = automations::discover(root)
        .with_context(|| format!("discover automations from {}", root.display()))?;

    if automations.is_empty() {
        println!("No automations found.");
        return Ok(());
    }

    for automation in automations {
        println!(
            "{} ({}) - {}",
            automation.name,
            automation.source.as_str(),
            automation.schedule.as_deref().unwrap_or("manual")
        );
    }

    Ok(())
}

/// Validates all discovered automations.
pub fn validate(root: &Path) -> Result<()> {
    let automations = automations::discover(root)
        .with_context(|| format!("discover automations from {}", root.display()))?;

    println!("Validated {} automation(s).", automations.len());
    for automation in automations {
        print_validation_line(&automation);
    }

    Ok(())
}

/// Runs one automation by name.
pub async fn run(
    root: &Path,
    thread_opts: &ThreadPersistenceOptions,
    config: &config::Config,
    name: &str,
) -> Result<()> {
    let automation = automations::load_by_name(root, name)
        .with_context(|| format!("load automation '{name}' from {}", root.display()))?;

    run_definition(root, thread_opts, config, &automation, RunTrigger::Manual).await
}

/// Runs one parsed automation definition.
///
/// # Errors
/// Returns an error when execution fails after retries.
pub async fn run_definition(
    root: &Path,
    thread_opts: &ThreadPersistenceOptions,
    config: &config::Config,
    automation: &AutomationDefinition,
    trigger: RunTrigger,
) -> Result<()> {
    let attempts = automation.max_retries.saturating_add(1);
    let root_string = root.to_string_lossy().to_string();
    let effective_thread_opts = resolve_automation_thread_opts(thread_opts, &automation.name);

    for attempt in 1..=attempts {
        let started_at = Utc::now();
        let started = Instant::now();

        let result = exec::run(exec::ExecRunOptions {
            root: &root_string,
            thread_opts: &effective_thread_opts,
            prompt: &automation.prompt,
            config,
            model_override: automation.model.as_deref(),
            tool_timeout_override: automation.timeout_secs,
            thinking_override: None,
            tools_override: None,
            no_tools: false,
        })
        .await;

        let finished_at = Utc::now();
        let elapsed_ms = started.elapsed().as_millis();
        let duration_ms = u64::try_from(elapsed_ms).unwrap_or(u64::MAX);
        let error = result.as_ref().err().map(summarize_error);

        let record = AutomationRunRecord {
            automation: automation.name.clone(),
            trigger,
            thread_id: effective_thread_opts.thread_id.clone(),
            attempt,
            max_attempts: attempts,
            started_at: started_at.to_rfc3339(),
            finished_at: finished_at.to_rfc3339(),
            duration_ms,
            ok: result.is_ok(),
            error,
            schedule: automation.schedule.clone(),
            model: automation.model.clone(),
        };

        if let Err(err) = append_run_record(&record) {
            eprintln!("Warning: failed to append automation run log: {err:#}");
        }

        match result {
            Ok(()) => return Ok(()),
            Err(err) if attempt < attempts => {
                eprintln!(
                    "Automation '{}' failed (attempt {attempt}/{attempts}): {err:#}",
                    automation.name
                );
                eprintln!("Retrying...");
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "automation '{}' failed after {attempts} attempt(s)",
                        automation.name
                    )
                });
            }
        }
    }

    Ok(())
}

fn resolve_automation_thread_opts(
    thread_opts: &ThreadPersistenceOptions,
    automation_name: &str,
) -> ThreadPersistenceOptions {
    if thread_opts.no_save || thread_opts.thread_id.is_some() {
        return thread_opts.clone();
    }

    ThreadPersistenceOptions {
        thread_id: Some(format!("automation-{automation_name}")),
        no_save: false,
    }
}

fn print_validation_line(automation: &AutomationDefinition) {
    println!(
        "- {} ({}): schedule={}, model={}, timeout_secs={}, max_retries={}",
        automation.name,
        automation.source.as_str(),
        automation.schedule.as_deref().unwrap_or("manual"),
        automation.model.as_deref().unwrap_or("<default>"),
        automation
            .timeout_secs
            .map_or_else(|| "<default>".to_string(), |v| v.to_string()),
        automation.max_retries
    );
}

fn summarize_error(err: &anyhow::Error) -> String {
    let text = format!("{err:#}").replace('\n', " | ");
    if text.len() > ERROR_SUMMARY_MAX_LEN {
        format!("{}...", &text[..ERROR_SUMMARY_MAX_LEN])
    } else {
        text
    }
}

fn append_run_record(record: &AutomationRunRecord) -> Result<()> {
    let path = runs_log_path();
    append_run_record_to(&path, record)
}

fn runs_log_path() -> std::path::PathBuf {
    config::paths::zdx_home().join("automations_runs.jsonl")
}

fn append_run_record_to(path: &Path, record: &AutomationRunRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create automation runs dir {}", parent.display()))?;
    }

    let line = serde_json::to_string(record).context("serialize automation run record")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open automation runs log {}", path.display()))?;
    writeln!(file, "{line}")
        .with_context(|| format!("append automation run log {}", path.display()))?;
    Ok(())
}

fn read_run_records(path: &Path) -> Result<Vec<AutomationRunRecord>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read automation runs log {}", path.display()))?;

    let mut records = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record = serde_json::from_str::<AutomationRunRecord>(trimmed).with_context(|| {
            format!(
                "parse automation run record at {} line {}",
                path.display(),
                idx + 1
            )
        })?;
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn append_run_record_writes_jsonl_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runs.jsonl");

        let record = AutomationRunRecord {
            automation: "morning-report".to_string(),
            trigger: RunTrigger::Manual,
            thread_id: Some("automation-morning-report".to_string()),
            attempt: 1,
            max_attempts: 1,
            started_at: "2026-02-11T08:00:00Z".to_string(),
            finished_at: "2026-02-11T08:00:03Z".to_string(),
            duration_ms: 3000,
            ok: true,
            error: None,
            schedule: Some("0 8 * * *".to_string()),
            model: Some("gemini-cli:gemini-2.5-flash".to_string()),
        };

        append_run_record_to(&path, &record).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["automation"], "morning-report");
        assert_eq!(parsed["trigger"], "manual");
        assert_eq!(parsed["thread_id"], "automation-morning-report");
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn resolve_automation_thread_opts_defaults_to_prefixed_thread() {
        let opts = ThreadPersistenceOptions {
            thread_id: None,
            no_save: false,
        };

        let resolved = resolve_automation_thread_opts(&opts, "daily-report");
        assert_eq!(
            resolved.thread_id.as_deref(),
            Some("automation-daily-report")
        );
        assert!(!resolved.no_save);
    }

    #[test]
    fn resolve_automation_thread_opts_honors_no_thread() {
        let opts = ThreadPersistenceOptions {
            thread_id: None,
            no_save: true,
        };

        let resolved = resolve_automation_thread_opts(&opts, "daily-report");
        assert!(resolved.thread_id.is_none());
        assert!(resolved.no_save);
    }

    #[test]
    fn resolve_automation_thread_opts_honors_explicit_thread() {
        let opts = ThreadPersistenceOptions {
            thread_id: Some("custom-thread".to_string()),
            no_save: false,
        };

        let resolved = resolve_automation_thread_opts(&opts, "daily-report");
        assert_eq!(resolved.thread_id.as_deref(), Some("custom-thread"));
        assert!(!resolved.no_save);
    }

    #[test]
    fn read_run_records_parses_jsonl() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runs.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"automation":"a","trigger":"manual","attempt":1,"max_attempts":1,"started_at":"2026-02-11T08:00:00Z","finished_at":"2026-02-11T08:00:01Z","duration_ms":1000,"ok":true,"error":null,"schedule":null,"model":null}"#,
                "\n",
                r#"{"automation":"b","trigger":"daemon","attempt":1,"max_attempts":2,"started_at":"2026-02-11T08:01:00Z","finished_at":"2026-02-11T08:01:02Z","duration_ms":2000,"ok":false,"error":"oops","schedule":"0 8 * * *","model":"m"}"#,
                "\n"
            ),
        )
        .unwrap();

        let records = read_run_records(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].automation, "a");
        assert_eq!(records[1].automation, "b");
        assert!(!records[1].ok);
    }
}
