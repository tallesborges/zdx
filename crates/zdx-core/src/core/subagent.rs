//! Subagent execution helpers.
//!
//! Provides a reusable way to run an isolated child `zdx exec` process and
//! capture response text only.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;

/// Options for a child `zdx exec` subagent run.
#[derive(Debug, Clone, Default)]
pub struct ExecSubagentOptions {
    /// Optional model override (`-m`).
    pub model: Option<String>,
    /// Optional thinking override (`-t`).
    pub thinking_level: Option<crate::config::ThinkingLevel>,
    /// Disable tools for the child run (`--no-tools`).
    pub no_tools: bool,
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
    let prompt = prompt.trim();
    ensure!(!prompt.is_empty(), "Subagent prompt cannot be empty");

    let exe = std::env::current_exe().context("Failed to get executable path")?;
    let args = build_exec_args(root, prompt, options);

    let mut command = Command::new(exe);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command.spawn().context("Failed to spawn subagent")?;

    let output = if let Some(timeout) = options.timeout {
        tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .with_context(|| format!("Subagent timed out after {} seconds", timeout.as_secs()))?
            .context("Failed to get subagent output")?
    } else {
        child
            .wait_with_output()
            .await
            .context("Failed to get subagent output")?
    };

    process_subagent_output(&output)
}

fn build_exec_args(root: &Path, prompt: &str, options: &ExecSubagentOptions) -> Vec<OsString> {
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

    if let Some(model) = normalize_optional(options.model.as_deref()) {
        args.push(OsString::from("-m"));
        args.push(OsString::from(model));
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
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn build_exec_args_includes_required_flags() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            "do work",
            &ExecSubagentOptions::default(),
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
        let args = build_exec_args(
            Path::new("/tmp/project"),
            "task",
            &ExecSubagentOptions {
                model: Some("openai:gpt-5.2".to_string()),
                thinking_level: Some(crate::config::ThinkingLevel::Low),
                no_tools: true,
                timeout: None,
            },
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
                "--no-tools",
                "-m",
                "openai:gpt-5.2",
                "-t",
                "low"
            ]
        );
    }
}
