//! Orchestrator thread-control tools.
//!
//! Seven controls the reserved `orchestrator` profile uses to manage worker
//! threads: `Create_Thread`, `Send_Thread_Message`, `Get_Thread_Status`,
//! `Wait_For_Threads`, `Update_Thread`, `Remove_Thread_Prompt`, and
//! `Cancel_Thread`.
//!
//! The registry always contains unbound stubs (so tool-name validation and
//! schemas work everywhere); surfaces that host a live
//! [`WorkerManager`](crate::core::workers::WorkerManager) — currently the
//! Telegram bot — replace them with bound instances via
//! [`ToolRegistry::register_boxed`](super::ToolRegistry::register_boxed).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolDefinition, ToolFuture};
use crate::core::events::ToolOutput;
use crate::core::thread_persistence;
use crate::core::workers::{WorkerManager, WorkerSnapshot};

/// Longest final-text excerpt returned inside tool output.
const MAX_FINAL_TEXT_CHARS: usize = 4000;
/// Longest queued-prompt excerpt returned per queue entry.
const MAX_QUEUED_PROMPT_CHARS: usize = 300;
/// Default and maximum `wait_for_threads` timeouts.
const DEFAULT_WAIT_SECS: u64 = 60;
const MAX_WAIT_SECS: u64 = 600;
/// How long `create_thread` waits for the surface bridge to open the worker's
/// mirror before returning without a link. The worker is already queued.
const MIRROR_LINK_WAIT: Duration = Duration::from_secs(5);

/// Which orchestrator control a tool instance implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Create,
    Send,
    Status,
    Wait,
    Update,
    RemovePrompt,
    Cancel,
}

const ALL_OPS: [Op; 7] = [
    Op::Create,
    Op::Send,
    Op::Status,
    Op::Wait,
    Op::Update,
    Op::RemovePrompt,
    Op::Cancel,
];

/// One orchestrator thread-control tool, optionally bound to a live manager.
pub struct OrchestratorTool {
    op: Op,
    manager: Option<Arc<WorkerManager>>,
}

impl OrchestratorTool {
    /// Unbound stubs registered in the default registry. They expose the
    /// schemas but fail at execution time on surfaces without a manager.
    #[must_use]
    pub fn stubs() -> Vec<Self> {
        ALL_OPS
            .into_iter()
            .map(|op| Self { op, manager: None })
            .collect()
    }

    /// Instances bound to a live worker manager.
    #[must_use]
    pub fn bound(manager: &Arc<WorkerManager>) -> Vec<Self> {
        ALL_OPS
            .into_iter()
            .map(|op| Self {
                op,
                manager: Some(Arc::clone(manager)),
            })
            .collect()
    }
}

impl Tool for OrchestratorTool {
    fn definition(&self) -> ToolDefinition {
        definition_for(self.op)
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolFuture {
        let op = self.op;
        let manager = self.manager.clone();
        let input = input.clone();
        let ctx = ctx.clone();
        Box::pin(async move {
            let Some(manager) = manager else {
                return ToolOutput::failure(
                    "orchestrator_unavailable",
                    "Orchestrator thread controls are not available on this surface",
                    Some("These tools require the Telegram bot's worker manager.".to_string()),
                );
            };
            let Some(owner) = ctx.current_thread_id.clone() else {
                return ToolOutput::failure(
                    "no_thread",
                    "Orchestrator tools require a persisted thread",
                    None,
                );
            };
            match op {
                Op::Create => create_thread(&manager, &owner, &input, &ctx).await,
                Op::Send => send_thread_message(&manager, &owner, &input),
                Op::Status => tokio::task::spawn_blocking(move || {
                    get_thread_status(&manager, &owner, &input, ctx.config.as_ref())
                })
                .await
                .unwrap_or_else(|err| {
                    ToolOutput::failure(
                        "status_failed",
                        "Failed to read thread status",
                        Some(err.to_string()),
                    )
                }),
                Op::Wait => wait_for_threads(&manager, &input).await,
                Op::Update => update_thread(&manager, &input),
                Op::RemovePrompt => remove_thread_prompt(&manager, &input),
                Op::Cancel => cancel_thread(&manager, &input),
            }
        })
    }
}

#[allow(clippy::too_many_lines)] // one schema literal per tool; splitting adds no clarity
fn definition_for(op: Op) -> ToolDefinition {
    match op {
        Op::Create => ToolDefinition {
            name: "Create_Thread".to_string(),
            description: "Create a new worker thread in an existing project directory and queue its first prompt. Returns the worker thread_id (and, when the surface opened one, the mirror_url of the Telegram topic where the user can follow it) while the worker runs in the background; you are notified automatically when its turn finishes. The worker already has the project's AGENTS.md rules, skills, and memory; the prompt is the user's request in their own words plus only what the worker cannot know (decisions from your conversation, threads to read, its slice of a split task, what to report back). Do not rewrite it into a specification.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "root": {
                        "type": "string",
                        "description": "Absolute path to an existing project directory the worker runs in"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Self-contained first prompt for the worker"
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional short thread title"
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional model override (`provider:model[@thinking][@fast]`). Omit to inherit configuration."
                    }
                },
                "required": ["root", "prompt"],
                "additionalProperties": false
            }),
        },
        Op::Send => ToolDefinition {
            name: "Send_Thread_Message".to_string(),
            description: "Queue another prompt on an existing worker thread. Prompts on one worker run strictly one at a time in order, with the worker's full prior context. The returned snapshot's `queue` lists every waiting prompt with its `prompt_id`; the one you just sent is last. Also re-attaches a thread that is no longer managed (e.g. after a restart) using its persisted project root.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": {
                        "type": "string",
                        "description": "Worker thread id to message"
                    },
                    "message": {
                        "type": "string",
                        "description": "Prompt to queue on the worker"
                    }
                },
                "required": ["thread_id", "message"],
                "additionalProperties": false
            }),
        },
        Op::Status => ToolDefinition {
            name: "Get_Thread_Status".to_string(),
            description: "Report a worker thread's status (queued/running/completed/failed/cancelled), queue depth, the waiting prompts (`queue`: `prompt_id` + a bounded excerpt, in run order; the running prompt is not listed), and latest final text. Managed workers also report `current_tool`, `current_tool_input` (a one-line command/path/pattern preview, at most 200 characters), `seconds_since_last_activity`, and `turn_elapsed_seconds`. The name and input belong to the same tool-use id; with concurrent tools, they describe the most recently started unfinished call. Inspect the preview before judging a long wait: a build can be quiet, and activity age alone does not prove a hang. Input is null until available or when the tool has no primary argument. For a thread this process does not manage, live tool information is unavailable; it reports `seconds_since_last_write` from the file mtime. Both paths include `context`: the latest recorded request's input tokens INCLUDING cache reads/writes, its recorded model/provider, context_limit, percent_used, and recorded_at. This estimates context occupancy, not cumulative spend; output tokens are excluded, and it is not an exact count of the next resumed request. Context is null when no input-bearing usage is found in the last 256 KiB or the file cannot be read. Unknown model/provider/limit leaves tokens visible but limit and percentage null. Use percent_used to decide whether to move work to a fresh thread. Omit thread_id to list every worker owned by this orchestrator in the current process.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": {
                        "type": "string",
                        "description": "Optional worker thread id; omit to list all owned workers"
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
        },
        Op::Wait => ToolDefinition {
            name: "Wait_For_Threads".to_string(),
            description: "Wait until all listed workers are idle (no running turn, empty queue) or a bounded timeout expires. Returns each worker's snapshot plus whether the wait timed out. Only one active wait is allowed per owner thread at a time. Prefer short waits — worker completions also wake you automatically.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Worker thread ids to wait on"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 600,
                        "description": "Maximum seconds to wait (default: 60, max: 600)"
                    }
                },
                "required": ["thread_ids"],
                "additionalProperties": false
            }),
        },
        Op::Update => ToolDefinition {
            name: "Update_Thread".to_string(),
            description: "Update a thread's title. Title is the only supported field. Fails while the target worker is mid-turn; retry when it is idle.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": {
                        "type": "string",
                        "description": "Thread id to update"
                    },
                    "title": {
                        "type": "string",
                        "description": "New thread title"
                    }
                },
                "required": ["thread_id", "title"],
                "additionalProperties": false
            }),
        },
        Op::RemovePrompt => ToolDefinition {
            name: "Remove_Thread_Prompt".to_string(),
            description: "Drop one prompt that is still waiting in a worker's queue, without touching the running turn or the other queued prompts. Use it to retract a prompt that became stale or was sent to the wrong worker; to change what runs next, remove the old prompt and Send_Thread_Message the new one. Get `prompt_id` from Get_Thread_Status or the snapshot returned by Send_Thread_Message. Fails when the id is not waiting (already running, finished, or never queued).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": {
                        "type": "string",
                        "description": "Worker thread id whose queue holds the prompt"
                    },
                    "prompt_id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Id of the queued prompt to remove"
                    }
                },
                "required": ["thread_id", "prompt_id"],
                "additionalProperties": false
            }),
        },
        Op::Cancel => ToolDefinition {
            name: "Cancel_Thread".to_string(),
            description: "Cancel a worker: stop the current turn, terminate its process tree, and clear all queued prompts. The thread itself is preserved — a later Send_Thread_Message resumes it.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread_id": {
                        "type": "string",
                        "description": "Worker thread id to cancel"
                    }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
        },
    }
}

fn required_str<'a>(input: &'a Value, field: &str) -> Result<&'a str, ToolOutput> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ToolOutput::failure(
                "invalid_input",
                format!("Missing required string field: {field}"),
                None,
            )
        })
}

fn optional_str<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn snapshot_json(snapshot: &WorkerSnapshot) -> Value {
    let title = thread_persistence::read_thread_title(&snapshot.thread_id)
        .ok()
        .flatten();
    json!({
        "thread_id": snapshot.thread_id,
        "title": title,
        "root": snapshot.root.display().to_string(),
        "status": snapshot.status.as_str(),
        "queue_depth": snapshot.queue_depth,
        "queue": snapshot
            .queue
            .iter()
            .map(|prompt| json!({
                "prompt_id": prompt.id,
                "prompt": truncate_chars(&prompt.text, MAX_QUEUED_PROMPT_CHARS),
            }))
            .collect::<Vec<_>>(),
        "idle": snapshot.is_idle(),
        "latest_final_text": snapshot
            .latest_final_text
            .as_deref()
            .map(|text| truncate_chars(text, MAX_FINAL_TEXT_CHARS)),
        "last_error": snapshot.last_error,
        "mirror_url": snapshot.mirror_url,
        "current_tool": snapshot.current_tool,
        "current_tool_input": snapshot.current_tool_input,
        "seconds_since_last_activity": snapshot.seconds_since_last_activity,
        "turn_elapsed_seconds": snapshot.turn_elapsed_seconds,
    })
}

/// On-disk JSONL path for a thread id.
fn thread_file_path(thread_id: &str) -> std::path::PathBuf {
    crate::config::paths::threads_dir().join(format!("{thread_id}.jsonl"))
}

/// Status for a thread this process does not manage.
///
/// Live tool activity only exists in the owning process, so the current tool is
/// genuinely unavailable here rather than unknown-and-guessable: the thread
/// JSONL has no `tool_started` event, and a running tool's `tool_use` is not
/// flushed until its turn checkpoints. The file mtime does advance during a
/// turn, so it is a real staleness signal and is all this path reports.
fn unmanaged_thread_json(thread_id: &str) -> Value {
    let title = thread_persistence::read_thread_title(thread_id)
        .ok()
        .flatten();
    let seconds_since_last_write = thread_file_path(thread_id)
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(|elapsed| elapsed.as_secs());

    json!({
        "thread_id": thread_id,
        "title": title,
        "managed": false,
        "seconds_since_last_write": seconds_since_last_write,
        "current_tool": Value::Null,
        "note": "Not a managed worker in this process: live tool activity is unavailable. `seconds_since_last_write` is the thread file's mtime and is the only progress signal here; a turn writes to it as it goes, so a large value means the thread is idle or stuck.",
    })
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}

async fn create_thread(
    manager: &Arc<WorkerManager>,
    owner: &str,
    input: &Value,
    ctx: &ToolContext,
) -> ToolOutput {
    let root = match required_str(input, "root") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let prompt = match required_str(input, "prompt") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let title = optional_str(input, "title");
    let (model, thinking_level) = match optional_str(input, "model") {
        Some(model) => {
            let inherited = ctx.thinking_level.unwrap_or_default();
            let (model, thinking) = crate::models::resolve_model_spec(model, inherited);
            (Some(model), Some(thinking))
        }
        None => (None, None),
    };

    let worker_id = match manager.create_worker(
        owner,
        &PathBuf::from(root),
        prompt,
        title,
        model,
        thinking_level,
    ) {
        Ok(worker_id) => worker_id,
        Err(err) => {
            return ToolOutput::failure("create_thread_failed", format!("{err:#}"), None);
        }
    };
    // The mirror topic opens asynchronously; wait a bounded moment so the
    // link can be handed back in the same result. The worker is already queued.
    let mirror_url = manager
        .wait_for_mirror_url(&worker_id, MIRROR_LINK_WAIT)
        .await;
    match manager.snapshot(&worker_id) {
        Some(snapshot) => ToolOutput::success(json!({
            "thread_id": worker_id,
            "mirror_url": mirror_url,
            "worker": snapshot_json(&snapshot),
        })),
        None => ToolOutput::success(json!({ "thread_id": worker_id, "mirror_url": mirror_url })),
    }
}

fn send_thread_message(manager: &Arc<WorkerManager>, owner: &str, input: &Value) -> ToolOutput {
    let thread_id = match required_str(input, "thread_id") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let message = match required_str(input, "message") {
        Ok(value) => value,
        Err(failure) => return failure,
    };

    match manager.send_message(owner, thread_id, message) {
        Ok(snapshot) => ToolOutput::success(json!({ "worker": snapshot_json(&snapshot) })),
        Err(err) => ToolOutput::failure("send_thread_message_failed", format!("{err:#}"), None),
    }
}

fn thread_context_json(thread_id: &str, config: Option<&crate::config::Config>) -> Value {
    let Some(usage) = thread_persistence::read_latest_context_usage(thread_id)
        .ok()
        .flatten()
    else {
        return Value::Null;
    };
    let models = crate::models::available_models().iter().chain(
        config
            .into_iter()
            .flat_map(|config| crate::models::custom_provider_models(&config.providers)),
    );
    usage_context_json(&usage, models)
}

fn usage_context_json<'a>(
    usage: &thread_persistence::LatestContextUsage,
    mut models: impl Iterator<Item = &'a crate::models::ModelOption>,
) -> Value {
    let model = usage
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let provider = usage
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let limit = provider.zip(model).and_then(|(provider, model)| {
        models
            .find(|entry| {
                entry.provider.eq_ignore_ascii_case(provider)
                    && entry.id.eq_ignore_ascii_case(model)
            })
            .map(|entry| entry.context_limit)
            .filter(|limit| *limit > 0)
    });
    json!({
        "input_tokens": usage.input_tokens,
        "model": model,
        "provider": provider,
        "context_limit": limit,
        "percent_used": limit.map(|limit| usage.input_tokens as f64 / limit as f64 * 100.0),
        "recorded_at": usage.recorded_at,
        "basis": "last_recorded_request_input",
    })
}

fn get_thread_status(
    manager: &Arc<WorkerManager>,
    owner: &str,
    input: &Value,
    config: Option<&crate::config::Config>,
) -> ToolOutput {
    if let Some(thread_id) = optional_str(input, "thread_id") {
        let mut worker = match manager.snapshot(thread_id) {
            Some(snapshot) => snapshot_json(&snapshot),
            None if thread_file_path(thread_id).is_file() => {
                unmanaged_thread_json(thread_id)
            }
            None => return ToolOutput::failure(
                "not_managed",
                format!("Thread '{thread_id}' is not a managed worker in this process"),
                Some(
                    "Use Send_Thread_Message to re-attach an existing thread, or Read_Thread to inspect its transcript.".to_string(),
                ),
            ),
        };
        worker["context"] = thread_context_json(thread_id, config);
        return ToolOutput::success(json!({ "worker": worker }));
    }

    let workers: Vec<Value> = manager
        .list_for_owner(owner)
        .iter()
        .map(|snapshot| {
            let mut worker = snapshot_json(snapshot);
            worker["context"] = thread_context_json(&snapshot.thread_id, config);
            worker
        })
        .collect();
    ToolOutput::success(json!({ "count": workers.len(), "workers": workers }))
}

async fn wait_for_threads(manager: &Arc<WorkerManager>, input: &Value) -> ToolOutput {
    let Some(ids) = input.get("thread_ids").and_then(Value::as_array) else {
        return ToolOutput::failure(
            "invalid_input",
            "Missing required array field: thread_ids",
            None,
        );
    };
    let thread_ids: Vec<String> = ids
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    if thread_ids.is_empty() {
        return ToolOutput::failure("invalid_input", "thread_ids cannot be empty", None);
    }

    let timeout_secs = input
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_WAIT_SECS)
        .clamp(1, MAX_WAIT_SECS);

    match manager
        .wait_for(&thread_ids, Duration::from_secs(timeout_secs))
        .await
    {
        Ok((timed_out, snapshots)) => {
            let workers: Vec<Value> = snapshots.iter().map(snapshot_json).collect();
            ToolOutput::success(json!({
                "timed_out": timed_out,
                "waited_seconds_max": timeout_secs,
                "workers": workers,
            }))
        }
        Err(err) => ToolOutput::failure("wait_for_threads_failed", format!("{err:#}"), None),
    }
}

fn update_thread(manager: &Arc<WorkerManager>, input: &Value) -> ToolOutput {
    let thread_id = match required_str(input, "thread_id") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let title = match required_str(input, "title") {
        Ok(value) => value,
        Err(failure) => return failure,
    };

    match manager.update_title(thread_id, title) {
        Ok(applied) => ToolOutput::success(json!({
            "thread_id": thread_id,
            "title": applied,
        })),
        Err(err) => ToolOutput::failure("update_thread_failed", format!("{err:#}"), None),
    }
}

fn remove_thread_prompt(manager: &Arc<WorkerManager>, input: &Value) -> ToolOutput {
    let thread_id = match required_str(input, "thread_id") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let Some(prompt_id) = input.get("prompt_id").and_then(Value::as_u64) else {
        return ToolOutput::failure(
            "invalid_input",
            "Missing required integer field: prompt_id",
            None,
        );
    };

    match manager.remove_queued_prompt(thread_id, prompt_id) {
        Ok((removed, snapshot)) => ToolOutput::success(json!({
            "removed": {
                "prompt_id": removed.id,
                "prompt": truncate_chars(&removed.text, MAX_QUEUED_PROMPT_CHARS),
            },
            "worker": snapshot_json(&snapshot),
        })),
        Err(err) => ToolOutput::failure("remove_thread_prompt_failed", format!("{err:#}"), None),
    }
}

fn cancel_thread(manager: &Arc<WorkerManager>, input: &Value) -> ToolOutput {
    let thread_id = match required_str(input, "thread_id") {
        Ok(value) => value,
        Err(failure) => return failure,
    };

    match manager.cancel(thread_id) {
        Ok(snapshot) => ToolOutput::success(json!({ "worker": snapshot_json(&snapshot) })),
        Err(err) => ToolOutput::failure("cancel_thread_failed", format!("{err:#}"), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_model(provider: &'static str, context_limit: u64) -> crate::models::ModelOption {
        crate::models::ModelOption {
            id: "gpt-6-astra",
            provider,
            account: None,
            display_name: "Astra",
            pricing: crate::models::ModelPricing {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_limit,
            capabilities: crate::models::ModelCapabilities::default(),
        }
    }

    #[test]
    fn context_percentage_uses_the_recorded_provider_and_model() {
        let models = [
            context_model("other-provider", 1000),
            context_model("openai-codex", 1_050_000),
        ];
        let context = usage_context_json(
            &thread_persistence::LatestContextUsage {
                input_tokens: 105_000,
                model: Some("gpt-6-astra".into()),
                provider: Some("openai-codex".into()),
                recorded_at: "2026-09-07T10:00:00Z".into(),
            },
            models.iter(),
        );
        assert_eq!(context["input_tokens"], 105_000);
        assert_eq!(context["context_limit"], 1_050_000);
        assert_eq!(context["percent_used"], 10.0);
        assert_eq!(context["recorded_at"], "2026-09-07T10:00:00Z");
    }

    #[test]
    fn context_without_attribution_or_limit_keeps_tokens_but_not_percentage() {
        for (model, provider, limit) in [
            (None, None, 1_050_000),
            (Some("gpt-6-astra"), None, 1_050_000),
            (Some("gpt-6-astra"), Some("unknown-provider"), 1_050_000),
            (Some("unknown-model"), Some("openai-codex"), 1_050_000),
            (Some("gpt-6-astra"), Some("openai-codex"), 0),
        ] {
            let models = [context_model("openai-codex", limit)];
            let context = usage_context_json(
                &thread_persistence::LatestContextUsage {
                    input_tokens: 42,
                    model: model.map(str::to_string),
                    provider: provider.map(str::to_string),
                    recorded_at: "sample".into(),
                },
                models.iter(),
            );
            assert_eq!(context["input_tokens"], 42);
            assert!(context["context_limit"].is_null());
            assert!(context["percent_used"].is_null());
        }
    }

    #[tokio::test]
    async fn status_context_reports_latest_input_for_managed_and_unmanaged_threads() {
        let home = crate::test_support::temp_zdx_home();
        let (manager, _events) =
            WorkerManager::with_runner(Arc::new(|_| Box::pin(async { Ok("done".into()) })));
        let id = manager
            .create_worker("owner", home.path(), "go", None, None, None)
            .unwrap();
        let empty = get_thread_status(&manager, "owner", &json!({"thread_id": id}), None);
        assert!(empty.data().unwrap()["worker"]["context"].is_null());
        let mut thread = thread_persistence::Thread::with_id(id.clone()).unwrap();
        for usage in [
            thread_persistence::Usage::new(900_000, 1000, 0, 0),
            thread_persistence::Usage::new(5_000, 1000, 90_000, 10_000),
            thread_persistence::Usage::new(0, 500, 0, 0),
        ] {
            thread
                .append(&thread_persistence::ThreadEvent::usage(
                    usage,
                    Some("gpt-6-astra".into()),
                    Some("openai-codex".into()),
                    None,
                    None,
                ))
                .unwrap();
        }
        thread
            .set_model_override(Some("different-current-model".into()))
            .unwrap();
        let tool = OrchestratorTool {
            op: Op::Status,
            manager: Some(Arc::clone(&manager)),
        };
        let ctx =
            ToolContext::new(home.path().to_path_buf(), None).with_current_thread_id(Some("owner"));
        let response = tool.execute(&json!({"thread_id": id}), &ctx).await;
        let expected = response.data().unwrap()["worker"]["context"].clone();
        assert_eq!(expected["input_tokens"], 105_000);
        assert_eq!(expected["model"], "gpt-6-astra");
        let listing = get_thread_status(&manager, "owner", &json!({}), None);
        assert_eq!(listing.data().unwrap()["workers"][0]["context"], expected);

        let (other_manager, _other_events) = WorkerManager::new();
        let unmanaged = get_thread_status(&other_manager, "owner", &json!({"thread_id": id}), None);
        assert_eq!(unmanaged.data().unwrap()["worker"]["managed"], false);
        assert_eq!(unmanaged.data().unwrap()["worker"]["context"], expected);
    }

    #[test]
    fn status_exposes_preview_only_for_managed_workers() {
        let _home = crate::test_support::temp_zdx_home();
        let snapshot = WorkerSnapshot {
            thread_id: "worker-status".into(),
            owner_thread_id: "owner".into(),
            root: std::path::PathBuf::from("/tmp"),
            status: crate::core::workers::WorkerStatus::Running,
            queue_depth: 0,
            queue: Vec::new(),
            latest_final_text: None,
            last_error: None,
            mirror_url: None,
            current_tool: Some("bash".into()),
            current_tool_input: Some("./gradlew assembleDebug".into()),
            seconds_since_last_activity: Some(360),
            turn_elapsed_seconds: Some(400),
        };
        let payload = snapshot_json(&snapshot);
        assert_eq!(payload["current_tool"], "bash");
        assert_eq!(payload["current_tool_input"], "./gradlew assembleDebug");
        assert!(
            unmanaged_thread_json("unowned")
                .get("current_tool_input")
                .is_none()
        );
    }

    #[test]
    fn definitions_cover_all_seven_controls() {
        let names: Vec<String> = OrchestratorTool::stubs()
            .iter()
            .map(|tool| tool.definition().name)
            .collect();
        assert_eq!(
            names,
            vec![
                "Create_Thread",
                "Send_Thread_Message",
                "Get_Thread_Status",
                "Wait_For_Threads",
                "Update_Thread",
                "Remove_Thread_Prompt",
                "Cancel_Thread",
            ]
        );
    }

    #[tokio::test]
    async fn remove_thread_prompt_lists_and_drops_a_queued_prompt() {
        let home = crate::test_support::temp_zdx_home();
        let (release_tx, release_rx) = tokio::sync::watch::channel(false);
        let (manager, _events) = WorkerManager::with_runner(Arc::new(move |request| {
            let mut release = release_rx.clone();
            Box::pin(async move {
                release.wait_for(|released| *released).await.unwrap();
                Ok(request.prompt)
            })
        }));
        let id = manager
            .create_worker("owner", home.path(), "first", None, None, None)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let sent = send_thread_message(
            &manager,
            "owner",
            &json!({"thread_id": id, "message": "stale"}),
        );
        let queue = sent.data().unwrap()["worker"]["queue"].clone();
        assert_eq!(queue[0]["prompt"], "stale");
        let prompt_id = queue[0]["prompt_id"].as_u64().unwrap();

        let removed =
            remove_thread_prompt(&manager, &json!({"thread_id": id, "prompt_id": prompt_id}));
        let data = removed.data().unwrap();
        assert_eq!(data["removed"]["prompt"], "stale");
        assert_eq!(data["worker"]["queue_depth"], 0);
        assert_eq!(data["worker"]["status"], "running");

        let again =
            remove_thread_prompt(&manager, &json!({"thread_id": id, "prompt_id": prompt_id}));
        let (code, _, _) = again.error_info().unwrap();
        assert_eq!(code, "remove_thread_prompt_failed");
        release_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn stub_fails_without_manager() {
        let _home = crate::test_support::temp_zdx_home();
        let stub = OrchestratorTool {
            op: Op::Status,
            manager: None,
        };
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None)
            .with_current_thread_id(Some("owner"));
        let output = stub.execute(&json!({}), &ctx).await;
        assert!(!output.is_ok());
        let (code, _, _) = output.error_info().unwrap();
        assert_eq!(code, "orchestrator_unavailable");
    }

    #[tokio::test]
    async fn bound_tools_require_current_thread_id() {
        let _home = crate::test_support::temp_zdx_home();
        let (manager, _rx) = WorkerManager::with_runner(std::sync::Arc::new(|_req| {
            Box::pin(async { Ok(String::new()) })
        }));
        let tool = OrchestratorTool {
            op: Op::Status,
            manager: Some(manager),
        };
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        let output = tool.execute(&json!({}), &ctx).await;
        let (code, _, _) = output.error_info().unwrap();
        assert_eq!(code, "no_thread");
    }
}
