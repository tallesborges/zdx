//! Subagent execution helpers.
//!
//! Provides a reusable way to run an isolated child `zdx exec` process and
//! capture response text only.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tempfile::{Builder, TempPath};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::core::agent::EventSender;
use crate::core::events::{AgentEvent, TurnStatus};

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
    /// Logical role for the child run (e.g. `"subagent"`).
    pub activity_kind: Option<String>,
    /// Parent thread id for the child run.
    pub activity_parent_thread_id: Option<String>,
    /// Named subagent (e.g. `"explorer"`) for the child run.
    pub activity_subagent_name: Option<String>,
    /// Origin kind recorded in the child thread's meta (e.g. `"subagent"`,
    /// `"helper:title"`). When set, the child persists a tagged thread instead
    /// of running throwaway. `None` leaves the child thread untagged.
    pub thread_origin_kind: Option<String>,
    /// Parent thread id recorded in the child thread's meta.
    pub thread_parent_id: Option<String>,
    /// Named subagent recorded in the child thread's meta (subagent runs).
    pub thread_subagent_name: Option<String>,
}

#[derive(Debug)]
struct TempPromptFile {
    path: TempPath,
}

impl TempPromptFile {
    fn as_path(&self) -> &Path {
        &self.path
    }
}

/// Runs an isolated child `zdx exec` process and returns response text only.
///
/// The child persists its own thread (tagged via `thread_origin_kind` and
/// friends) so its usage is captured by thread-scanning stats.
///
/// # Errors
/// Returns an error if the child process fails, times out, or produces empty output.
pub async fn run_exec_subagent(
    root: &Path,
    prompt: &str,
    options: &ExecSubagentOptions,
) -> Result<String> {
    run_exec_subagent_with_cancel(root, prompt, options, None, None).await
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
    stream: Option<SubagentStreamSink>,
) -> Result<String> {
    let prompt = prompt.trim();
    ensure!(!prompt.is_empty(), "Subagent prompt cannot be empty");

    let exe = std::env::current_exe().context("Failed to get executable path")?;
    let prompt_file = write_prompt_file(prompt)?;
    let effective_system_prompt_file = options
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(write_effective_system_prompt_file)
        .transpose()?;
    let args = build_exec_args(
        root,
        prompt_file.as_path(),
        options,
        effective_system_prompt_file
            .as_ref()
            .map(TempPromptFile::as_path),
    );

    let mut command = Command::new(exe);
    command
        .args(args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command
        .spawn()
        .map_err(|err| anyhow::anyhow!("Failed to spawn subagent: {err}"))?;

    if let Some(sink) = stream {
        return run_child_streaming(child, cancel, options.timeout, sink).await;
    }

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

    process_subagent_output(&output)
}

/// Live sink for relaying a child subagent's tool activity to the parent.
///
/// Each child tool lifecycle event is re-emitted as an `AgentEvent::ToolOutputDelta`
/// on the parent's event stream, keyed by the parent `invoke_subagent` tool id.
/// The `chunk` is a compact JSON object (`{"t":"start"|"input"|"done"|"error", ...}`)
/// that the TUI parses into a child tool activity list.
pub struct SubagentStreamSink {
    pub sender: EventSender,
    pub parent_tool_id: String,
}

impl SubagentStreamSink {
    fn emit(&self, chunk: &Value) {
        self.sender.send(AgentEvent::ToolOutputDelta {
            id: self.parent_tool_id.clone(),
            chunk: chunk.to_string(),
        });
    }

    fn emit_start(&self, id: &str, name: &str) {
        self.emit(&serde_json::json!({ "t": "start", "id": id, "name": name }));
    }

    fn emit_input(&self, id: &str, arg: &str) {
        self.emit(&serde_json::json!({ "t": "input", "id": id, "arg": arg }));
    }

    fn emit_done(&self, id: &str) {
        self.emit(&serde_json::json!({ "t": "done", "id": id }));
    }

    fn emit_error(&self, id: &str) {
        self.emit(&serde_json::json!({ "t": "error", "id": id }));
    }
}

/// Result of draining a child subagent's stdout event stream.
struct StreamOutcome {
    final_text: Option<String>,
    turn_failed: Option<String>,
}

/// Streams a child `zdx exec` process: relays tool activity live via `sink`,
/// drains stderr concurrently to avoid pipe deadlocks, and returns the final
/// turn text (preserving the same completion/failure semantics as the
/// non-streaming path).
async fn run_child_streaming(
    mut child: tokio::process::Child,
    cancel: Option<CancellationToken>,
    timeout: Option<Duration>,
    sink: SubagentStreamSink,
) -> Result<String> {
    let stdout = child
        .stdout
        .take()
        .context("Subagent stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("Subagent stderr was not piped")?;

    // Drain stderr concurrently so the child never blocks on a full pipe.
    let stderr_handle = tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut reader = BufReader::new(stderr);
        let _ = reader.read_to_end(&mut buf).await;
        buf
    });

    let read_future = read_stdout_events(stdout, &sink);

    let outcome = match (cancel, timeout) {
        (Some(cancel), Some(timeout)) => tokio::select! {
            () = cancel.cancelled() => bail!("Subagent cancelled"),
            result = tokio::time::timeout(timeout, read_future) => result
                .with_context(|| format!("Subagent timed out after {} seconds", timeout.as_secs()))??,
        },
        (Some(cancel), None) => tokio::select! {
            () = cancel.cancelled() => bail!("Subagent cancelled"),
            result = read_future => result?,
        },
        (None, Some(timeout)) => tokio::time::timeout(timeout, read_future)
            .await
            .with_context(|| format!("Subagent timed out after {} seconds", timeout.as_secs()))??,
        (None, None) => read_future.await?,
    };

    // Await the child exit and stderr drain before returning so diagnostics are
    // complete and no pipe is left dangling.
    let status = child.wait().await.context("Failed to wait for subagent")?;
    let stderr_buf = stderr_handle.await.unwrap_or_default();

    if let Some(message) = outcome.turn_failed {
        bail!("Subagent turn failed: {message}");
    }

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            let code = status.code().unwrap_or(-1);
            bail!("Subagent failed with exit code {code}");
        }
        bail!("Subagent failed: {stderr}");
    }

    match outcome.final_text {
        Some(text) if !text.trim().is_empty() => Ok(text),
        _ => bail!("Subagent returned empty output"),
    }
}

/// Reads JSONL agent events from a child's stdout, relaying tool lifecycle
/// events through `sink` and capturing the terminal turn result.
async fn read_stdout_events<R: AsyncRead + Unpin>(
    reader: R,
    sink: &SubagentStreamSink,
) -> Result<StreamOutcome> {
    let mut lines = BufReader::new(reader).lines();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut final_text = None;
    let mut turn_failed = None;

    while let Some(line) = lines
        .next_line()
        .await
        .context("Failed to read subagent stdout")?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentEvent>(trimmed) else {
            continue;
        };
        match event {
            AgentEvent::ToolInputCompleted { id, name, input } => {
                let name = name.to_ascii_lowercase();
                if seen_ids.insert(id.clone()) {
                    sink.emit_start(&id, &name);
                }
                if let Some(arg) = extract_key_arg(&name, &input) {
                    sink.emit_input(&id, &arg);
                }
            }
            AgentEvent::ToolStarted { id, name } if seen_ids.insert(id.clone()) => {
                sink.emit_start(&id, &name.to_ascii_lowercase());
            }
            AgentEvent::ToolCompleted { id, result } => {
                if result.is_ok() {
                    sink.emit_done(&id);
                } else {
                    sink.emit_error(&id);
                }
            }
            AgentEvent::TurnFinished {
                status,
                final_text: text,
                ..
            } => match status {
                TurnStatus::Completed | TurnStatus::Interrupted => final_text = Some(text),
                TurnStatus::Failed { message, .. } => turn_failed = Some(message),
            },
            _ => {}
        }
    }

    Ok(StreamOutcome {
        final_text,
        turn_failed,
    })
}

/// Extracts the most useful single argument to display for a child tool call.
///
/// Tool names are matched lowercase (the engine normalizes them). Reads only
/// the needed field so large `write`/`edit` inputs are not cloned wholesale.
fn extract_key_arg(tool_name: &str, input: &Value) -> Option<String> {
    match tool_name {
        "bash" => input
            .get("command")
            .and_then(Value::as_str)
            .map(truncate_command),
        "read" | "edit" | "write" => input
            .get("file_path")
            .and_then(Value::as_str)
            .map(str::to_string),
        "glob" | "grep" => input
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::to_string),
        "fetch_webpage" => input.get("url").and_then(Value::as_str).map(str::to_string),
        "web_search" => input
            .get("search_queries")
            .and_then(Value::as_array)
            .and_then(|queries| queries.first())
            .and_then(Value::as_str)
            .map(str::to_string),
        "invoke_subagent" => Some(
            input
                .get("subagent")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("task")
                .to_string(),
        ),
        _ => None,
    }
}

fn truncate_command(command: &str) -> String {
    const MAX_CHARS: usize = 60;
    let trimmed = command.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_CHARS).collect();
    format!("{head}…")
}

fn build_exec_args(
    root: &Path,
    prompt_file: &Path,
    options: &ExecSubagentOptions,
    effective_system_prompt_file: Option<&Path>,
) -> Vec<OsString> {
    let mut args = vec![OsString::from("--root"), root.as_os_str().to_os_string()];

    // Global thread-lineage flags (before the subcommand) so the persisted
    // child thread records its subagent/helper origin in its meta line.
    if let Some(kind) = normalize_optional(options.thread_origin_kind.as_deref()) {
        args.push(OsString::from("--thread-origin-kind"));
        args.push(OsString::from(kind));
    }
    if let Some(parent) = normalize_optional(options.thread_parent_id.as_deref()) {
        args.push(OsString::from("--thread-parent-id"));
        args.push(OsString::from(parent));
    }
    if let Some(name) = normalize_optional(options.thread_subagent_name.as_deref()) {
        args.push(OsString::from("--thread-subagent-name"));
        args.push(OsString::from(name));
    }

    args.extend([
        OsString::from("exec"),
        OsString::from("--prompt-file"),
        prompt_file.as_os_str().to_os_string(),
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

    if let Some(kind) = normalize_optional(options.activity_kind.as_deref()) {
        args.push(OsString::from("--activity-kind"));
        args.push(OsString::from(kind));
    }
    if let Some(parent) = normalize_optional(options.activity_parent_thread_id.as_deref()) {
        args.push(OsString::from("--activity-parent-thread-id"));
        args.push(OsString::from(parent));
    }
    if let Some(name) = normalize_optional(options.activity_subagent_name.as_deref()) {
        args.push(OsString::from("--activity-subagent-name"));
        args.push(OsString::from(name));
    }

    args
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn write_prompt_file(prompt: &str) -> Result<TempPromptFile> {
    write_temp_text_file("zdx-subagent-prompt", prompt)
}

fn write_effective_system_prompt_file(system_prompt: &str) -> Result<TempPromptFile> {
    write_temp_text_file("zdx-effective-system-prompt", system_prompt)
}

fn write_temp_text_file(prefix: &str, contents: &str) -> Result<TempPromptFile> {
    let mut file = Builder::new()
        .prefix(prefix)
        .suffix(".md")
        .tempfile()
        .context("create temp text file")?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("write temp text file {}", file.path().display()))?;
    file.flush()
        .with_context(|| format!("flush temp text file {}", file.path().display()))?;
    let path = file.into_temp_path();
    Ok(TempPromptFile { path })
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
            Path::new("/tmp/subagent-prompt.md"),
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
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md"
            ]
        );
    }

    #[test]
    fn build_exec_args_includes_optional_flags() {
        let prompt_file = Path::new("/tmp/subagent-prompt.md");
        let system_prompt_file = Path::new("/tmp/effective-system-prompt.md");
        let args = build_exec_args(
            Path::new("/tmp/project"),
            prompt_file,
            &ExecSubagentOptions {
                model: Some("openai:gpt-5.2".to_string()),
                system_prompt: Some("You are a focused assistant".to_string()),
                thinking_level: Some(crate::config::ThinkingLevel::Low),
                no_tools: false,
                no_system_prompt: true,
                tools_override: Some(vec!["read".to_string(), "glob".to_string()]),
                event_filter: Some(vec!["turn_finished".to_string()]),
                timeout: None,
                activity_kind: None,
                activity_parent_thread_id: None,
                activity_subagent_name: None,
                thread_origin_kind: None,
                thread_parent_id: None,
                thread_subagent_name: None,
            },
            Some(system_prompt_file),
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
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md",
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
    fn build_exec_args_emits_thread_lineage_flags() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            Path::new("/tmp/subagent-prompt.md"),
            &ExecSubagentOptions {
                thread_origin_kind: Some("subagent".to_string()),
                thread_parent_id: Some("thread-parent".to_string()),
                thread_subagent_name: Some("explorer".to_string()),
                ..Default::default()
            },
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
                "--thread-origin-kind",
                "subagent",
                "--thread-parent-id",
                "thread-parent",
                "--thread-subagent-name",
                "explorer",
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md"
            ]
        );
    }

    #[test]
    fn build_exec_args_propagates_activity_metadata_when_tracking() {
        let args = build_exec_args(
            Path::new("/tmp/project"),
            Path::new("/tmp/subagent-prompt.md"),
            &ExecSubagentOptions {
                activity_kind: Some("subagent".to_string()),
                activity_parent_thread_id: Some("thread-parent".to_string()),
                activity_subagent_name: Some("explorer".to_string()),
                ..Default::default()
            },
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
                "exec",
                "--prompt-file",
                "/tmp/subagent-prompt.md",
                "--activity-kind",
                "subagent",
                "--activity-parent-thread-id",
                "thread-parent",
                "--activity-subagent-name",
                "explorer"
            ]
        );
    }

    #[test]
    fn process_subagent_output_extracts_turn_finished_text() {
        let terminal = serde_json::to_string(&AgentEvent::TurnFinished {
            status: crate::core::events::TurnStatus::Completed,
            final_text: "final answer".to_string(),
            messages: Vec::new(),
            prior_message_count: 0,
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

    #[test]
    fn temp_prompt_file_is_removed_on_drop() {
        let file = write_effective_system_prompt_file("prompt body").unwrap();
        let path = file.as_path().to_path_buf();
        assert!(path.exists());

        drop(file);

        assert!(!path.exists());
    }

    #[test]
    fn prompt_file_is_removed_on_drop() {
        let file = write_prompt_file("prompt body").unwrap();
        let path = file.as_path().to_path_buf();
        assert!(path.exists());

        drop(file);

        assert!(!path.exists());
    }

    #[test]
    fn extract_key_arg_reads_expected_field_per_tool() {
        use serde_json::json;

        assert_eq!(
            extract_key_arg("read", &json!({ "file_path": "Cargo.toml" })).as_deref(),
            Some("Cargo.toml")
        );
        assert_eq!(
            extract_key_arg("write", &json!({ "file_path": "src/lib.rs" })).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            extract_key_arg("glob", &json!({ "pattern": "**/*.rs" })).as_deref(),
            Some("**/*.rs")
        );
        assert_eq!(
            extract_key_arg("grep", &json!({ "pattern": "TODO" })).as_deref(),
            Some("TODO")
        );
        assert_eq!(
            extract_key_arg("fetch_webpage", &json!({ "url": "https://example.com" })).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            extract_key_arg(
                "web_search",
                &json!({ "search_queries": ["rust async", "tokio"] })
            )
            .as_deref(),
            Some("rust async")
        );
        assert_eq!(
            extract_key_arg("invoke_subagent", &json!({ "subagent": "explorer" })).as_deref(),
            Some("explorer")
        );
        // invoke_subagent with no explicit subagent falls back to the default alias.
        assert_eq!(
            extract_key_arg("invoke_subagent", &json!({ "prompt": "go" })).as_deref(),
            Some("task")
        );
        // Unknown tools have no key arg.
        assert_eq!(extract_key_arg("todo_write", &json!({ "todos": [] })), None);
    }

    #[test]
    fn truncate_command_caps_long_input() {
        let short = "cargo build";
        assert_eq!(truncate_command(short), short);

        let long = "a".repeat(200);
        let truncated = truncate_command(&long);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 61); // 60 chars + ellipsis
    }

    fn make_sink() -> (SubagentStreamSink, crate::core::agent::AgentEventRx) {
        let (tx, rx) = crate::core::agent::create_event_channel();
        let sink = SubagentStreamSink {
            sender: EventSender::new(tx),
            parent_tool_id: "parent-tool".to_string(),
        };
        (sink, rx)
    }

    fn jsonl(events: &[AgentEvent]) -> String {
        events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn read_stdout_events_relays_child_tools_and_final_text() {
        let (sink, mut rx) = make_sink();
        // Engine order: ToolInputCompleted (carries the arg) before ToolStarted.
        let events = vec![
            AgentEvent::ToolInputCompleted {
                id: "tu1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({ "file_path": "Cargo.toml" }),
            },
            AgentEvent::ToolStarted {
                id: "tu1".to_string(),
                name: "Read".to_string(),
            },
            AgentEvent::ToolCompleted {
                id: "tu1".to_string(),
                result: crate::core::events::ToolOutput::success(serde_json::json!("ok")),
            },
            AgentEvent::TurnFinished {
                status: crate::core::events::TurnStatus::Completed,
                final_text: "all done".to_string(),
                messages: Vec::new(),
                prior_message_count: 0,
            },
        ];
        let bytes = jsonl(&events);

        let outcome = read_stdout_events(bytes.as_bytes(), &sink).await.unwrap();
        assert_eq!(outcome.final_text.as_deref(), Some("all done"));
        assert!(outcome.turn_failed.is_none());

        drop(sink);
        let mut kinds = Vec::new();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::ToolOutputDelta { id, chunk } = event.as_ref() {
                assert_eq!(id, "parent-tool");
                let value: Value = serde_json::from_str(chunk).unwrap();
                kinds.push(value["t"].as_str().unwrap().to_string());
            }
        }
        // start + input from ToolInputCompleted (ToolStarted deduped), then done.
        assert_eq!(kinds, vec!["start", "input", "done"]);
    }

    #[tokio::test]
    async fn read_stdout_events_reports_turn_failure() {
        let (sink, _rx) = make_sink();
        let events = vec![AgentEvent::TurnFinished {
            status: crate::core::events::TurnStatus::Failed {
                kind: crate::core::events::ErrorKind::Internal,
                message: "boom".to_string(),
                details: None,
            },
            final_text: String::new(),
            messages: Vec::new(),
            prior_message_count: 0,
        }];
        let bytes = jsonl(&events);

        let outcome = read_stdout_events(bytes.as_bytes(), &sink).await.unwrap();
        assert_eq!(outcome.turn_failed.as_deref(), Some("boom"));
        assert!(outcome.final_text.is_none());
    }
}
