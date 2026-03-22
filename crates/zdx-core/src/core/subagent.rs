//! Subagent execution helpers.
//!
//! Provides a reusable way to run an isolated child `zdx exec` process and
//! capture response text only.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::core::events::AgentEvent;

/// Options for a child `zdx exec` subagent run.
#[derive(Debug, Clone, Default)]
pub struct ExecSubagentOptions {
    /// Optional model override (`-m`).
    pub model: Option<String>,
    /// Optional final system prompt override (`--effective-system-prompt`).
    pub system_prompt: Option<String>,
    /// Optional thinking override (`-t`).
    pub thinking_level: Option<crate::config::ThinkingLevel>,
    /// Disable tools for the child run (`--no-tools`).
    pub no_tools: bool,
    /// Disable system prompt/context composition for the child run (`--no-system-prompt`).
    pub no_system_prompt: bool,
    /// Optional explicit tool allowlist for the child run (`--tools`).
    pub tools_override: Option<Vec<String>>,
    /// Optional event type filters for exec output (`--filter`).
    pub event_filter: Option<Vec<String>>,
    /// Optional timeout for the child process.
    pub timeout: Option<Duration>,
}

/// Runs an isolated child `zdx exec` process and returns response text only.
///
/// The child process always runs with `--no-thread` to avoid thread pollution.
///
/// # Errors
/// Returns an error if the child process fails, times out, or produces empty output.
pub async fn run_exec_subagent(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
) -> Result<String> {
    run_exec_subagent_with_cancel(root, prompt, options, None).await
}

/// Runs an isolated child `zdx exec` process with optional cancellation support.
///
/// # Errors
/// Returns an error if the child process fails, times out, is canceled, or
/// produces invalid/empty output.
pub async fn run_exec_subagent_with_cancel(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
    cancel: Option<CancellationToken>,
) -> Result<String> {
    let prompt = prompt.trim();
    ensure!(!prompt.is_empty(), "Subagent prompt cannot be empty");

    let exe = std::env::current_exe().context("Failed to get executable path")?;
    let effective_system_prompt_file = options
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(write_effective_system_prompt_file)
        .transpose()?;
    let args = build_exec_args(
        root,
        prompt,
        options,
        effective_system_prompt_file.as_deref(),
    );

    let mut command = Command::new(exe);
    command
        .args(args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command.spawn().context("Failed to spawn subagent")?;

    let wait_future = child.wait_with_output();
    let output = match (cancel, options.timeout) {
        (Some(cancel), Some(timeout)) => {
            tokio::select! {
                () = cancel.cancelled() => bail!("Subagent cancelled"),
                result = tokio::time::timeout(timeout, wait_future) => {
                    result
                        .with_context(|| format!("Subagent timed out after {} seconds", timeout.as_secs()))?
                        .context("Failed to get subagent output")?
                }
            }
        }
        (Some(cancel), None) => {
            tokio::select! {
                () = cancel.cancelled() => bail!("Subagent cancelled"),
                result = wait_future => result.context("Failed to get subagent output")?,
            }
        }
        (None, Some(timeout)) => tokio::time::timeout(timeout, wait_future)
            .await
            .with_context(|| format!("Subagent timed out after {} seconds", timeout.as_secs()))?
            .context("Failed to get subagent output")?,
        (None, None) => wait_future.await.context("Failed to get subagent output")?,
    };

    let result = process_subagent_output(&output);

    if let Some(path) = &effective_system_prompt_file {
        let _ = fs::remove_file(path);
    }

    result
}

fn build_exec_args(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
    effective_system_prompt_file: Option<&Path>,
) -> Vec<OsString> {
    let mut args = vec![OsString::from("--root"), root.as_os_str().to_os_string()];

    args.extend([
        OsString::from("--no-thread"),
        OsString::from("exec"),
        OsString::from("-p"),
        OsString::from(prompt),
    ]);

    if options.no_tools {
        args.push(OsString::from("--no-tools"));
    }

    if options.no_system_prompt {
        args.push(OsString::from("--no-system-prompt"));
    }

    if let Some(tools) = options
        .tools_override
        .as_ref()
        .filter(|tools| !tools.is_empty())
    {
        args.push(OsString::from("--tools"));
        args.push(OsString::from(tools.join(",")));
    }

    if let Some(filters) = options
        .event_filter
        .as_ref()
        .filter(|filters| !filters.is_empty())
    {
        args.push(OsString::from("--filter"));
        args.push(OsString::from(filters.join(",")));
    }

    if let Some(model) = normalize_optional(options.model.as_deref()) {
        args.push(OsString::from("-m"));
        args.push(OsString::from(model));
    }

    if let Some(system_prompt_file) = effective_system_prompt_file {
        args.push(OsString::from("--effective-system-prompt-file"));
        args.push(system_prompt_file.as_os_str().to_os_string());
    }

    if let Some(level) = options.thinking_level {
        args.push(OsString::from("-t"));
        args.push(OsString::from(level.display_name()));
    }

    args
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn write_effective_system_prompt_file(system_prompt: &str) -> Result<PathBuf> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "zdx-effective-system-prompt-{}-{unique}.md",
        std::process::id()
    ));
    fs::write(&path, system_prompt)
        .with_context(|| format!("write effective system prompt file {}", path.display()))?;
    Ok(path)
}

fn process_subagent_output(output: &std::process::Output) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            let code = output.status.code().unwrap_or(-1);
            bail!("Subagent failed with exit code {code}");
        }
        bail!("Subagent failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    ensure!(!stdout.is_empty(), "Subagent returned empty output");

    if let Some(final_text) = extract_turn_finished_text(&stdout)? {
        return Ok(final_text);
    }

    Ok(stdout)
}

fn extract_turn_finished_text(stdout: &str) -> Result<Option<String>> {
    let mut saw_json_event = false;
    let mut final_text = None;

    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        match serde_json::from_str::<AgentEvent>(line) {
            Ok(AgentEvent::TurnFinished {
                status,
                final_text: text,
                ..
            }) => {
                saw_json_event = true;
                match status {
                    crate::core::events::TurnStatus::Completed
                    | crate::core::events::TurnStatus::Interrupted => {
                        final_text = Some(text);
                    }
                    crate::core::events::TurnStatus::Failed { message, .. } => {
                        bail!("Subagent turn failed: {message}");
                    }
                }
            }
            Ok(_) => {
                saw_json_event = true;
            }
            Err(_) => {
                if saw_json_event {
                    bail!("Subagent produced malformed JSONL output");
                }
                return Ok(None);
            }
        }
    }

    if saw_json_event {
        return final_text
            .filter(|text| !text.trim().is_empty())
            .map(Some)
            .ok_or_else(|| {
                anyhow::anyhow!("Subagent JSONL output missing turn_finished.final_text")
            });
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;

    use super::*;

    #[test]
    fn build_exec_args_includes_required_flags() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            "do work",
            &ExecSubagentOptions::default(),
            None,
        );
        let args: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            args,
            vec![
                "--root",
                "/tmp/project",
                "--no-thread",
                "exec",
                "-p",
                "do work"
            ]
        );
    }

    #[test]
    fn build_exec_args_includes_optional_flags() {
        let prompt_file = Path::new("/tmp/effective-system-prompt.md");
        let args = build_exec_args(
            Path::new("/tmp/project"),
            "task",
            &ExecSubagentOptions {
                model: Some("openai:gpt-5.2".to_string()),
                system_prompt: Some("You are a focused assistant".to_string()),
                thinking_level: Some(crate::config::ThinkingLevel::Low),
                no_tools: false,
                no_system_prompt: true,
                tools_override: Some(vec!["read".to_string(), "glob".to_string()]),
                event_filter: Some(vec!["turn_finished".to_string()]),
                timeout: None,
            },
            Some(prompt_file),
        );
        let args: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            args,
            vec![
                "--root",
                "/tmp/project",
                "--no-thread",
                "exec",
                "-p",
                "task",
                "--no-system-prompt",
                "--tools",
                "read,glob",
                "--filter",
                "turn_finished",
                "-m",
                "openai:gpt-5.2",
                "--effective-system-prompt-file",
                "/tmp/effective-system-prompt.md",
                "-t",
                "low"
            ]
        );
    }

    #[test]
    fn process_subagent_output_extracts_turn_finished_text() {
        let terminal = serde_json::to_string(&AgentEvent::TurnFinished {
            status: crate::core::events::TurnStatus::Completed,
            final_text: "final answer".to_string(),
            messages: Vec::new(),
        })
        .unwrap();
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: format!(
                "{{\"type\":\"usage_update\",\"input_tokens\":1,\"output_tokens\":2,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n{{\"type\":\"assistant_completed\",\"text\":\"partial\"}}\n{terminal}\n"
            )
            .into_bytes(),
            stderr: Vec::new(),
        };

        let text = process_subagent_output(&output).expect("should parse");
        assert_eq!(text, "final answer");
    }

    #[test]
    fn process_subagent_output_falls_back_to_plain_text() {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"plain text output\n".to_vec(),
            stderr: Vec::new(),
        };

        let text = process_subagent_output(&output).expect("should keep plain text");
        assert_eq!(text, "plain text output");
    }
}
