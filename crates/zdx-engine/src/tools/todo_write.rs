//! Todo write tool for structured task tracking.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::thread_persistence::{self as tp, ThreadEvent};

/// Returns the tool definition for the `todo_write` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Todo_Write".to_string(),
        description: "Create and manage a structured todo list for the current thread. Use this for tasks with 3+ meaningful steps, multiple requested changes, or work that benefits from visible progress. Send an `ops` array of mutations such as `replace`, `add`, `update`, and `remove` — `ops` must be a real JSON array, not a quoted JSON string. Prefer updating tasks immediately as work advances instead of keeping a long implicit plan in prose. While unfinished work remains, keep exactly one task `in_progress`. Example: {\"ops\":[{\"op\":\"add\",\"content\":\"Inspect bug\",\"status\":\"in_progress\"}]}".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "ops": {
                    "type": "array",
                    "description": "Ordered todo-list mutations to apply. Must be a JSON array, not a stringified JSON array.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["replace", "add", "update", "remove"],
                                "description": "Mutation kind to apply."
                            },
                            "tasks": {
                                "type": "array",
                                "description": "Full replacement task list for `replace`.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "content": { "type": "string", "description": "Short task label." },
                                        "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "abandoned"] },
                                        "details": { "type": "string", "description": "Optional temporary task context for this thread, such as paths, constraints, or next checks." }
                                    },
                                    "required": ["content"]
                                }
                            },
                            "id": { "type": "string", "description": "Task ID, e.g. task-2, used by `update` and `remove`." },
                            "content": { "type": "string", "description": "Task label for `add`, or updated label for `update`." },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "abandoned"] },
                            "details": { "type": "string", "description": "Optional temporary task context for this thread, such as paths, constraints, or next checks." }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["ops"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TodoItem {
    id: String,
    content: String,
    status: TodoStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TodoInput {
    ops: Vec<TodoOp>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum TodoOp {
    Replace {
        tasks: Vec<InputTask>,
    },
    Add {
        content: String,
        status: Option<TodoStatus>,
        details: Option<String>,
    },
    Update {
        id: String,
        content: Option<String>,
        status: Option<TodoStatus>,
        details: Option<String>,
    },
    Remove {
        id: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct InputTask {
    content: String,
    status: Option<TodoStatus>,
    details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TodoState {
    tasks: Vec<TodoItem>,
    next_id: usize,
}

impl Default for TodoState {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct TodoCounts {
    total: usize,
    pending: usize,
    in_progress: usize,
    completed: usize,
    abandoned: usize,
}

/// Executes the todo-write tool and returns a structured envelope.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_with_state(input, None, ctx.current_thread_id.as_deref())
}

pub(crate) fn execute_with_state(
    input: &Value,
    previous: Option<&TodoState>,
    current_thread_id: Option<&str>,
) -> ToolOutput {
    let normalized_input = match normalize_input(input) {
        Ok(value) => value,
        Err((message, detail)) => {
            return ToolOutput::failure("invalid_input", message, detail);
        }
    };

    let input: TodoInput = match serde_json::from_value(normalized_input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for todo_write tool",
                Some(describe_parse_error(&normalized_input, &e)),
            );
        }
    };

    if input.ops.is_empty() {
        return ToolOutput::failure("invalid_input", "ops cannot be empty", None);
    }

    let previous = previous
        .cloned()
        .or_else(|| load_current_state(current_thread_id))
        .unwrap_or_default();
    let updated = match apply_ops(&previous, &input.ops) {
        Ok(state) => state,
        Err(message) => return ToolOutput::failure("invalid_input", message, None),
    };

    ToolOutput::success(json!({
        "tasks": updated.tasks,
        "counts": counts(&updated.tasks),
        "summary": summary(&updated.tasks)
    }))
}

fn normalize_input(input: &Value) -> Result<Value, (&'static str, Option<String>)> {
    let Some(obj) = input.as_object() else {
        return Ok(input.clone());
    };

    let Some(ops_value) = obj.get("ops") else {
        return Ok(input.clone());
    };

    let Some(ops_str) = ops_value.as_str() else {
        return Ok(input.clone());
    };

    let parsed = serde_json::from_str::<Value>(ops_str).map_err(|e| {
        (
            "field 'ops' must be an array",
            Some(format!(
                "received a string for 'ops' but it could not be parsed as JSON: {e}"
            )),
        )
    })?;

    let parsed_array = parsed.as_array().ok_or_else(|| {
        (
            "field 'ops' must be an array",
            Some(
                "received a string that parsed as JSON, but not as a JSON array; remove the surrounding quotes and send an array directly".to_string(),
            ),
        )
    })?;

    let mut normalized = obj.clone();
    normalized.insert("ops".to_string(), Value::Array(parsed_array.clone()));
    Ok(Value::Object(normalized))
}

fn describe_parse_error(input: &Value, error: &serde_json::Error) -> String {
    if let Some(obj) = input.as_object()
        && let Some(ops) = obj.get("ops")
    {
        if ops.is_string() {
            return format!(
                "field 'ops' must be an array; received a string that looks like JSON. Remove the surrounding quotes. Parse error: {error}"
            );
        }
        if !ops.is_array() {
            return format!(
                "field 'ops' must be an array; received {}. Parse error: {error}",
                json_type_name(ops)
            );
        }
    }

    format!("Parse error: {error}")
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn load_current_state(origin_thread_id: Option<&str>) -> Option<TodoState> {
    let thread_id = origin_thread_id
        .map(str::trim)
        .filter(|thread_id| !thread_id.is_empty())?;
    let events = tp::load_thread_events(thread_id).ok()?;
    extract_latest_state(&events)
}

fn extract_latest_state(events: &[ThreadEvent]) -> Option<TodoState> {
    let mut todo_tool_use_ids = std::collections::HashSet::new();
    let mut latest = None;

    for event in events {
        match event {
            ThreadEvent::ToolUse { id, name, .. } if is_todo_tool(name) => {
                todo_tool_use_ids.insert(id.clone());
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                ..
            } if *ok && todo_tool_use_ids.contains(tool_use_id) => {
                if let Some(state) = state_from_tool_output(output) {
                    latest = Some(state);
                }
            }
            _ => {}
        }
    }

    latest
}

fn is_todo_tool(name: &str) -> bool {
    name.to_ascii_lowercase().replace('-', "_") == "todo_write"
}

pub(crate) fn is_todo_tool_name(name: &str) -> bool {
    is_todo_tool(name)
}

fn state_from_tool_output(output: &Value) -> Option<TodoState> {
    let output: ToolOutput = serde_json::from_value(output.clone()).ok()?;
    let data = output.data()?;
    let tasks: Vec<TodoItem> = serde_json::from_value(data.get("tasks")?.clone()).ok()?;
    Some(TodoState {
        next_id: next_task_id(&tasks),
        tasks,
    })
}

pub(crate) fn state_from_output(output: &ToolOutput) -> Option<TodoState> {
    let output_value = serde_json::to_value(output).ok()?;
    state_from_tool_output(&output_value)
}

fn apply_ops(previous: &TodoState, ops: &[TodoOp]) -> Result<TodoState, String> {
    let mut next = previous.clone();

    for op in ops {
        match op {
            TodoOp::Replace { tasks } => {
                next.tasks.clear();
                next.next_id = 1;
                for task in tasks {
                    let content = task.content.trim();
                    if content.is_empty() {
                        return Err("replace tasks cannot contain empty content".to_string());
                    }
                    next.tasks.push(TodoItem {
                        id: format!("task-{}", next.next_id),
                        content: content.to_string(),
                        status: task.status.clone().unwrap_or(TodoStatus::Pending),
                        details: normalize_optional_string(task.details.clone()),
                    });
                    next.next_id += 1;
                }
            }
            TodoOp::Add {
                content,
                status,
                details,
            } => {
                let content = content.trim();
                if content.is_empty() {
                    return Err("add requires non-empty content".to_string());
                }
                next.tasks.push(TodoItem {
                    id: format!("task-{}", next.next_id),
                    content: content.to_string(),
                    status: status.clone().unwrap_or(TodoStatus::Pending),
                    details: normalize_optional_string(details.clone()),
                });
                next.next_id += 1;
            }
            TodoOp::Update {
                id,
                content,
                status,
                details,
            } => {
                let task = next
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == *id)
                    .ok_or_else(|| format!("Task '{id}' not found"))?;

                if let Some(content) = content {
                    let content = content.trim();
                    if content.is_empty() {
                        return Err(format!("Task '{id}' cannot have empty content"));
                    }
                    task.content = content.to_string();
                }
                if let Some(status) = status {
                    task.status = status.clone();
                }
                if details.is_some() {
                    task.details = normalize_optional_string(details.clone());
                }
            }
            TodoOp::Remove { id } => {
                let original_len = next.tasks.len();
                next.tasks.retain(|task| task.id != *id);
                if next.tasks.len() == original_len {
                    return Err(format!("Task '{id}' not found"));
                }
            }
        }
    }

    normalize_in_progress(&mut next.tasks);
    next.next_id = next_task_id(&next.tasks);
    Ok(next)
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_in_progress(tasks: &mut [TodoItem]) {
    let mut first_in_progress = None;
    let mut has_pending = false;

    for (idx, task) in tasks.iter_mut().enumerate() {
        match task.status {
            TodoStatus::InProgress => {
                if first_in_progress.is_none() {
                    first_in_progress = Some(idx);
                } else {
                    task.status = TodoStatus::Pending;
                    has_pending = true;
                }
            }
            TodoStatus::Pending => has_pending = true,
            TodoStatus::Completed | TodoStatus::Abandoned => {}
        }
    }

    if first_in_progress.is_some() {
        return;
    }

    if !has_pending {
        return;
    }

    if let Some(task) = tasks
        .iter_mut()
        .find(|task| matches!(task.status, TodoStatus::Pending))
    {
        task.status = TodoStatus::InProgress;
    }
}

fn next_task_id(tasks: &[TodoItem]) -> usize {
    tasks
        .iter()
        .filter_map(|task| task.id.strip_prefix("task-"))
        .filter_map(|suffix| suffix.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

fn counts(tasks: &[TodoItem]) -> TodoCounts {
    let mut counts = TodoCounts {
        total: tasks.len(),
        pending: 0,
        in_progress: 0,
        completed: 0,
        abandoned: 0,
    };

    for task in tasks {
        match task.status {
            TodoStatus::Pending => counts.pending += 1,
            TodoStatus::InProgress => counts.in_progress += 1,
            TodoStatus::Completed => counts.completed += 1,
            TodoStatus::Abandoned => counts.abandoned += 1,
        }
    }

    counts
}

fn summary(tasks: &[TodoItem]) -> String {
    let active = tasks
        .iter()
        .find(|task| matches!(task.status, TodoStatus::InProgress))
        .map(|task| format!("Active: {} ({})", task.id, task.content));
    let remaining = tasks
        .iter()
        .filter(|task| matches!(task.status, TodoStatus::Pending | TodoStatus::InProgress))
        .count();

    match active {
        Some(active) => format!("{active}. Remaining tasks: {remaining}."),
        None if tasks.is_empty() => "No tasks tracked.".to_string(),
        None => format!("No active task. Remaining tasks: {remaining}."),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_replace_promotes_first_pending_task() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Replace {
                tasks: vec![
                    InputTask {
                        content: "Inspect codebase".to_string(),
                        status: Some(TodoStatus::Pending),
                        details: None,
                    },
                    InputTask {
                        content: "Implement fix".to_string(),
                        status: Some(TodoStatus::Pending),
                        details: None,
                    },
                ],
            }],
        )
        .unwrap();

        assert_eq!(next.tasks.len(), 2);
        assert_eq!(next.tasks[0].id, "task-1");
        assert!(matches!(next.tasks[0].status, TodoStatus::InProgress));
        assert!(matches!(next.tasks[1].status, TodoStatus::Pending));
    }

    #[test]
    fn test_add_from_empty_starts_at_task_1() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Add {
                content: "Inspect codebase".to_string(),
                status: None,
                details: None,
            }],
        )
        .unwrap();

        assert_eq!(next.tasks.len(), 1);
        assert_eq!(next.tasks[0].id, "task-1");
        assert!(matches!(next.tasks[0].status, TodoStatus::InProgress));
        assert_eq!(next.next_id, 2);
    }

    #[test]
    fn test_add_then_update_first_task_uses_task_1() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Add {
                content: "Inspect codebase".to_string(),
                status: None,
                details: None,
            }],
        )
        .unwrap();

        let updated = apply_ops(
            &next,
            &[TodoOp::Update {
                id: "task-1".to_string(),
                content: None,
                status: Some(TodoStatus::Completed),
                details: None,
            }],
        )
        .unwrap();

        assert!(matches!(updated.tasks[0].status, TodoStatus::Completed));
    }

    #[test]
    fn test_update_completion_promotes_next_pending_task() {
        let previous = TodoState {
            tasks: vec![
                TodoItem {
                    id: "task-1".to_string(),
                    content: "Inspect codebase".to_string(),
                    status: TodoStatus::InProgress,
                    details: None,
                },
                TodoItem {
                    id: "task-2".to_string(),
                    content: "Implement fix".to_string(),
                    status: TodoStatus::Pending,
                    details: None,
                },
            ],
            next_id: 3,
        };

        let next = apply_ops(
            &previous,
            &[TodoOp::Update {
                id: "task-1".to_string(),
                content: None,
                status: Some(TodoStatus::Completed),
                details: None,
            }],
        )
        .unwrap();

        assert!(matches!(next.tasks[0].status, TodoStatus::Completed));
        assert!(matches!(next.tasks[1].status, TodoStatus::InProgress));
    }

    #[test]
    fn test_multiple_in_progress_tasks_are_normalized() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Replace {
                tasks: vec![
                    InputTask {
                        content: "Inspect codebase".to_string(),
                        status: Some(TodoStatus::InProgress),
                        details: None,
                    },
                    InputTask {
                        content: "Implement fix".to_string(),
                        status: Some(TodoStatus::InProgress),
                        details: None,
                    },
                ],
            }],
        )
        .unwrap();

        assert!(matches!(next.tasks[0].status, TodoStatus::InProgress));
        assert!(matches!(next.tasks[1].status, TodoStatus::Pending));
    }

    #[test]
    fn test_extract_latest_state_from_thread_events() {
        let output = ToolOutput::success(json!({
            "tasks": [
                {
                    "id": "task-1",
                    "content": "Inspect codebase",
                    "status": "completed"
                },
                {
                    "id": "task-2",
                    "content": "Implement fix",
                    "status": "in_progress"
                }
            ],
            "counts": {
                "total": 2,
                "pending": 0,
                "in_progress": 1,
                "completed": 1,
                "abandoned": 0
            },
            "summary": "Active: task-2 (Implement fix). Remaining tasks: 1."
        }));
        let events = vec![
            ThreadEvent::tool_use("tool-1", "read", json!({"path": "src/lib.rs"})),
            ThreadEvent::tool_result(
                "tool-1",
                json!({"ok": true, "data": {"file_path": "src/lib.rs"}}),
                true,
            ),
            ThreadEvent::tool_use("tool-2", "todo_write", json!({"ops": []})),
            ThreadEvent::tool_result("tool-2", serde_json::to_value(output).unwrap(), true),
        ];

        let state = extract_latest_state(&events).unwrap();
        assert_eq!(state.tasks.len(), 2);
        assert_eq!(state.tasks[1].id, "task-2");
        assert!(matches!(state.tasks[1].status, TodoStatus::InProgress));
    }

    #[test]
    fn test_recovered_state_supports_follow_up_update_and_add() {
        let output = ToolOutput::success(json!({
            "tasks": [
                {
                    "id": "task-1",
                    "content": "Inspect codebase",
                    "status": "completed"
                },
                {
                    "id": "task-2",
                    "content": "Implement fix",
                    "status": "in_progress"
                }
            ],
            "counts": {
                "total": 2,
                "pending": 0,
                "in_progress": 1,
                "completed": 1,
                "abandoned": 0
            },
            "summary": "Active: task-2 (Implement fix). Remaining tasks: 1."
        }));
        let events = vec![
            ThreadEvent::tool_use("tool-2", "todo_write", json!({"ops": []})),
            ThreadEvent::tool_result("tool-2", serde_json::to_value(output).unwrap(), true),
        ];

        let recovered = extract_latest_state(&events).unwrap();
        assert_eq!(recovered.next_id, 3);

        let next = apply_ops(
            &recovered,
            &[
                TodoOp::Update {
                    id: "task-2".to_string(),
                    content: None,
                    status: Some(TodoStatus::Completed),
                    details: None,
                },
                TodoOp::Add {
                    content: "Ship fix".to_string(),
                    status: None,
                    details: None,
                },
            ],
        )
        .unwrap();

        assert_eq!(next.tasks.len(), 3);
        assert!(matches!(next.tasks[1].status, TodoStatus::Completed));
        assert_eq!(next.tasks[2].id, "task-3");
        assert_eq!(next.tasks[2].content, "Ship fix");
        assert!(matches!(next.tasks[2].status, TodoStatus::InProgress));
        assert_eq!(next.next_id, 4);
    }

    #[test]
    fn test_execute_rejects_empty_ops() {
        let output = execute(
            &json!({"ops": []}),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );
        assert!(!output.is_ok());
        let (code, message, _) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "ops cannot be empty");
    }

    #[test]
    fn test_execute_coerces_stringified_ops_array() {
        let output = execute(
            &json!({
                "ops": "[{\"op\":\"add\",\"content\":\"Inspect codebase\"}]"
            }),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(output.is_ok());
        let data = output.data().expect("todo_write should return data");
        let tasks = data
            .get("tasks")
            .and_then(|tasks| tasks.as_array())
            .expect("todo_write should return tasks array");
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].get("content").and_then(|v| v.as_str()),
            Some("Inspect codebase")
        );
    }

    #[test]
    fn test_execute_rejects_stringified_non_array_ops() {
        let output = execute(
            &json!({
                "ops": "{\"op\":\"add\",\"content\":\"Inspect codebase\"}"
            }),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(!output.is_ok());
        let (code, message, detail) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "field 'ops' must be an array");
        assert!(
            detail
                .unwrap_or_default()
                .contains("parsed as JSON, but not as a JSON array")
        );
    }

    #[test]
    fn test_execute_reports_non_array_ops_type() {
        let output = execute(
            &json!({"ops": {"op": "add", "content": "Inspect codebase"}}),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(!output.is_ok());
        let (code, message, detail) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "Invalid input for todo_write tool");
        assert!(
            detail
                .unwrap_or_default()
                .contains("field 'ops' must be an array; received object")
        );
    }

    #[test]
    fn test_execute_reports_invalid_op_after_stringified_ops_coercion() {
        let output = execute(
            &json!({
                "ops": "[{\"op\":\"bogus\",\"content\":\"Inspect codebase\"}]"
            }),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(!output.is_ok());
        let (code, message, detail) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "Invalid input for todo_write tool");
        assert!(
            detail
                .unwrap_or_default()
                .contains("unknown variant `bogus`")
        );
    }

    #[test]
    fn test_summary_for_empty_tasks() {
        assert_eq!(summary(&[]), "No tasks tracked.");
    }

    #[test]
    fn test_completed_only_list_has_no_active_task() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Replace {
                tasks: vec![InputTask {
                    content: "Done".to_string(),
                    status: Some(TodoStatus::Completed),
                    details: None,
                }],
            }],
        )
        .unwrap();

        assert!(matches!(next.tasks[0].status, TodoStatus::Completed));
    }
}
