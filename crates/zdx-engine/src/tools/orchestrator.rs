//! Orchestrator thread-control tools.
//!
//! Six controls the reserved `orchestrator` profile uses to manage worker
//! threads: `Create_Thread`, `Send_Thread_Message`, `Get_Thread_Status`,
//! `Wait_For_Threads`, `Update_Thread`, and `Cancel_Thread`.
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
use crate::config::ThinkingLevel;
use crate::core::events::ToolOutput;
use crate::core::thread_persistence;
use crate::core::workers::{WorkerManager, WorkerSnapshot};

/// Longest final-text excerpt returned inside tool output.
const MAX_FINAL_TEXT_CHARS: usize = 4000;
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
    Cancel,
}

const ALL_OPS: [Op; 6] = [
    Op::Create,
    Op::Send,
    Op::Status,
    Op::Wait,
    Op::Update,
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
                Op::Create => create_thread(&manager, &owner, &input).await,
                Op::Send => send_thread_message(&manager, &owner, &input),
                Op::Status => get_thread_status(&manager, &owner, &input),
                Op::Wait => wait_for_threads(&manager, &input).await,
                Op::Update => update_thread(&manager, &input),
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
            description: "Create a new worker thread in an existing project directory and queue its first prompt. Returns the worker thread_id (and, when the surface opened one, the mirror_url of the Telegram topic where the user can follow it) while the worker runs in the background; you are notified automatically when its turn finishes. The worker already has the project's AGENTS.md rules, skills, and memory; the prompt should carry only what it cannot know (goal, decisions from your conversation, task-specific constraints, what to report back), written briefly like a message to a colleague.".to_string(),
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
                        "description": "Optional model override (provider:model). Omit to inherit configuration."
                    },
                    "thinking_level": {
                        "type": "string",
                        "enum": ["off", "low", "medium", "high", "xhigh", "max"],
                        "description": "Optional thinking-level override. Omit to inherit configuration."
                    }
                },
                "required": ["root", "prompt"],
                "additionalProperties": false
            }),
        },
        Op::Send => ToolDefinition {
            name: "Send_Thread_Message".to_string(),
            description: "Queue another prompt on an existing worker thread. Prompts on one worker run strictly one at a time in order, with the worker's full prior context. Also re-attaches a thread that is no longer managed (e.g. after a restart) using its persisted project root.".to_string(),
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
            description: "Report a worker thread's status (queued/running/completed/failed/cancelled), queue depth, and latest final text. Omit thread_id to list every worker owned by this orchestrator in the current process.".to_string(),
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
            description: "Wait until all listed workers are idle (no running turn, empty queue) or a bounded timeout expires. Returns each worker's snapshot plus whether the wait timed out. Prefer short waits — worker completions also wake you automatically.".to_string(),
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
        "idle": snapshot.is_idle(),
        "latest_final_text": snapshot
            .latest_final_text
            .as_deref()
            .map(|text| truncate_chars(text, MAX_FINAL_TEXT_CHARS)),
        "last_error": snapshot.last_error,
        "mirror_url": snapshot.mirror_url,
    })
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}

async fn create_thread(manager: &Arc<WorkerManager>, owner: &str, input: &Value) -> ToolOutput {
    let root = match required_str(input, "root") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let prompt = match required_str(input, "prompt") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let title = optional_str(input, "title");
    let model = optional_str(input, "model").map(str::to_string);
    let thinking_level = match optional_str(input, "thinking_level") {
        Some(raw) => match ThinkingLevel::from_name(raw) {
            Some(level) => Some(level),
            None => {
                return ToolOutput::failure(
                    "invalid_input",
                    format!("Unknown thinking_level: {raw}"),
                    Some("Valid values: off, low, medium, high, xhigh, max".to_string()),
                );
            }
        },
        None => None,
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

fn get_thread_status(manager: &Arc<WorkerManager>, owner: &str, input: &Value) -> ToolOutput {
    if let Some(thread_id) = optional_str(input, "thread_id") {
        return match manager.snapshot(thread_id) {
            Some(snapshot) => ToolOutput::success(json!({ "worker": snapshot_json(&snapshot) })),
            None => ToolOutput::failure(
                "not_managed",
                format!("Thread '{thread_id}' is not a managed worker in this process"),
                Some(
                    "Use Send_Thread_Message to re-attach an existing thread, or Read_Thread to inspect its transcript.".to_string(),
                ),
            ),
        };
    }

    let workers: Vec<Value> = manager
        .list_for_owner(owner)
        .iter()
        .map(snapshot_json)
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

    #[test]
    fn definitions_cover_all_six_controls() {
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
                "Cancel_Thread",
            ]
        );
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
