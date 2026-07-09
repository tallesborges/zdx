//! Todo write tool for structured task tracking.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::thread_persistence::{self as tp, ThreadEvent};

/// Returns the tool definition for the `Todo_Write` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Todo_Write".to_string(),
        description: "Create and manage a structured todo list for the current thread. Use this for tasks with 3+ meaningful steps, multiple requested changes, or work that benefits from visible progress. Send a `todos` array of mutations such as `replace`, `add`, `update`, and `remove` — `todos` must be a real JSON array, not a quoted JSON string. Each element must be a mutation object with `op`; do not pass raw todo items like `{content,status}` directly. Use `replace` to initialize or fully reset the list, then prefer incremental `update`/`add`/`remove` ops as work advances. For `update`, send the todo `id` and only the fields that change; omit unchanged fields instead of sending empty strings. While unfinished work remains, keep exactly one todo `in_progress`. Examples: initialize with {\"todos\":[{\"op\":\"replace\",\"todos\":[{\"content\":\"Inspect bug\",\"status\":\"in_progress\"}]}]}; update status with {\"todos\":[{\"op\":\"update\",\"id\":\"todo-1\",\"status\":\"completed\"}]}".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Ordered todo-list mutations to apply. Must be a JSON array, not a stringified JSON array. Each element must include `op`; to initialize the list, use `{ \"op\": \"replace\", \"todos\": [...] }`.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["replace", "add", "update", "remove"],
                                "description": "Mutation kind to apply."
                            },
                            "todos": {
                                "type": "array",
                                "description": "Full replacement todo list for `replace`.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "content": { "type": "string", "description": "Short todo label." },
                                        "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "abandoned"] },
                                        "details": { "type": "string", "description": "Optional temporary todo context for this thread, such as paths, constraints, or next checks." }
                                    },
                                    "required": ["content"]
                                }
                            },
                            "id": { "type": "string", "description": "Todo ID, e.g. todo-2, used by `update` and `remove`." },
                            "content": { "type": "string", "description": "Required non-empty todo label for `add`. For `update`, omit this field unless changing the label; empty values are ignored." },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "abandoned"] },
                            "details": { "type": "string", "description": "Optional temporary todo context. For `update`, omit this field unless changing the details; empty values are ignored." }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["todos"],
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
    todos: Vec<TodoOp>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum TodoOp {
    Replace {
        todos: Vec<InputTodo>,
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
struct InputTodo {
    content: String,
    status: Option<TodoStatus>,
    details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TodoState {
    todos: Vec<TodoItem>,
    next_id: usize,
}

impl Default for TodoState {
    fn default() -> Self {
        Self {
            todos: Vec::new(),
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
                "Invalid input for Todo_Write tool",
                Some(describe_parse_error(&normalized_input, &e)),
            );
        }
    };

    if input.todos.is_empty() {
        return ToolOutput::failure("invalid_input", "todos cannot be empty", None);
    }

    let previous = previous
        .cloned()
        .or_else(|| load_current_state(current_thread_id))
        .unwrap_or_default();
    let updated = match apply_ops(&previous, &input.todos) {
        Ok(state) => state,
        Err(message) => return ToolOutput::failure("invalid_input", message, None),
    };

    let summary_text = summary(&updated.todos);
    ToolOutput::success(json!({
        "todos": updated.todos,
        "previous_todos": previous.todos,
        "counts": counts(&updated.todos),
        "summary": summary_text,
        "reminder": "Todo list updated. Continue using Todo_Write to mark items in_progress before starting and completed as soon as you finish — keep exactly one todo in_progress while work remains."
    }))
}

fn normalize_input(input: &Value) -> Result<Value, (&'static str, Option<String>)> {
    let Some(obj) = input.as_object() else {
        return Ok(input.clone());
    };

    let Some(todos_value) = obj.get("todos") else {
        return Ok(input.clone());
    };

    let Some(todos_str) = todos_value.as_str() else {
        return Ok(input.clone());
    };

    let parsed = serde_json::from_str::<Value>(todos_str).map_err(|e| {
        (
            "field 'todos' must be an array",
            Some(format!(
                "received a string for 'todos' but it could not be parsed as JSON: {e}"
            )),
        )
    })?;

    let parsed_array = parsed.as_array().ok_or_else(|| {
        (
            "field 'todos' must be an array",
            Some(
                "received a string that parsed as JSON, but not as a JSON array; remove the surrounding quotes and send an array directly".to_string(),
            ),
        )
    })?;

    let mut normalized = obj.clone();
    normalized.insert("todos".to_string(), Value::Array(parsed_array.clone()));
    Ok(Value::Object(normalized))
}

fn describe_parse_error(input: &Value, error: &serde_json::Error) -> String {
    if let Some(obj) = input.as_object()
        && let Some(todos) = obj.get("todos")
    {
        if todos.is_string() {
            return format!(
                "field 'todos' must be an array; received a string that looks like JSON. Remove the surrounding quotes. Parse error: {error}"
            );
        }
        if !todos.is_array() {
            return format!(
                "field 'todos' must be an array; received {}. Parse error: {error}",
                json_type_name(todos)
            );
        }
        if let Some(items) = todos.as_array()
            && items.iter().any(|item| {
                item.as_object().is_some_and(|item| {
                    !item.contains_key("op")
                        && (item.contains_key("content") || item.contains_key("status"))
                })
            })
        {
            return format!(
                "todo items must be wrapped in mutation ops; use {{\"op\":\"replace\",\"todos\":[...]}} to initialize. Parse error: {error}"
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
    let normalized: String = name
        .chars()
        .filter(|c| !matches!(c, '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect();
    normalized == "todowrite"
}

pub(crate) fn is_todo_tool_name(name: &str) -> bool {
    is_todo_tool(name)
}

fn state_from_tool_output(output: &Value) -> Option<TodoState> {
    let output: ToolOutput = serde_json::from_value(output.clone()).ok()?;
    let data = output.data()?;
    let todos: Vec<TodoItem> = serde_json::from_value(data.get("todos")?.clone()).ok()?;
    Some(TodoState {
        next_id: next_todo_id(&todos),
        todos,
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
            TodoOp::Replace { todos } => {
                next.todos.clear();
                next.next_id = 1;
                for todo in todos {
                    let content = todo.content.trim();
                    if content.is_empty() {
                        return Err("replace todos cannot contain empty content".to_string());
                    }
                    next.todos.push(TodoItem {
                        id: format!("todo-{}", next.next_id),
                        content: content.to_string(),
                        status: todo.status.clone().unwrap_or(TodoStatus::Pending),
                        details: normalize_optional_string(todo.details.clone()),
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
                next.todos.push(TodoItem {
                    id: format!("todo-{}", next.next_id),
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
                let todo = next
                    .todos
                    .iter_mut()
                    .find(|todo| todo.id == *id)
                    .ok_or_else(|| format!("Todo '{id}' not found"))?;

                if let Some(content) = content {
                    let content = content.trim();
                    if !content.is_empty() {
                        todo.content = content.to_string();
                    }
                }
                if let Some(status) = status {
                    todo.status = status.clone();
                }
                if let Some(details) = normalize_optional_string(details.clone()) {
                    todo.details = Some(details);
                }
            }
            TodoOp::Remove { id } => {
                let original_len = next.todos.len();
                next.todos.retain(|todo| todo.id != *id);
                if next.todos.len() == original_len {
                    return Err(format!("Todo '{id}' not found"));
                }
            }
        }
    }

    normalize_in_progress(&mut next.todos);
    next.next_id = next_todo_id(&next.todos);
    Ok(next)
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_in_progress(todos: &mut [TodoItem]) {
    let mut first_in_progress = None;
    let mut has_pending = false;

    for (idx, todo) in todos.iter_mut().enumerate() {
        match todo.status {
            TodoStatus::InProgress => {
                if first_in_progress.is_none() {
                    first_in_progress = Some(idx);
                } else {
                    todo.status = TodoStatus::Pending;
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

    if let Some(todo) = todos
        .iter_mut()
        .find(|todo| matches!(todo.status, TodoStatus::Pending))
    {
        todo.status = TodoStatus::InProgress;
    }
}

fn next_todo_id(todos: &[TodoItem]) -> usize {
    todos
        .iter()
        .filter_map(|todo| todo.id.strip_prefix("todo-"))
        .filter_map(|suffix| suffix.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

fn counts(todos: &[TodoItem]) -> TodoCounts {
    let mut counts = TodoCounts {
        total: todos.len(),
        pending: 0,
        in_progress: 0,
        completed: 0,
        abandoned: 0,
    };

    for todo in todos {
        match todo.status {
            TodoStatus::Pending => counts.pending += 1,
            TodoStatus::InProgress => counts.in_progress += 1,
            TodoStatus::Completed => counts.completed += 1,
            TodoStatus::Abandoned => counts.abandoned += 1,
        }
    }

    counts
}

fn summary(todos: &[TodoItem]) -> String {
    let active = todos
        .iter()
        .find(|todo| matches!(todo.status, TodoStatus::InProgress))
        .map(|todo| format!("Active: {} ({})", todo.id, todo.content));
    let remaining = todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Pending | TodoStatus::InProgress))
        .count();

    match active {
        Some(active) => format!("{active}. Remaining todos: {remaining}."),
        None if todos.is_empty() => "No todos tracked.".to_string(),
        None => format!("No active todo. Remaining todos: {remaining}."),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_replace_promotes_first_pending_todo() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Replace {
                todos: vec![
                    InputTodo {
                        content: "Inspect codebase".to_string(),
                        status: Some(TodoStatus::Pending),
                        details: None,
                    },
                    InputTodo {
                        content: "Implement fix".to_string(),
                        status: Some(TodoStatus::Pending),
                        details: None,
                    },
                ],
            }],
        )
        .unwrap();

        assert_eq!(next.todos.len(), 2);
        assert_eq!(next.todos[0].id, "todo-1");
        assert!(matches!(next.todos[0].status, TodoStatus::InProgress));
        assert!(matches!(next.todos[1].status, TodoStatus::Pending));
    }

    #[test]
    fn test_add_from_empty_starts_at_todo_1() {
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

        assert_eq!(next.todos.len(), 1);
        assert_eq!(next.todos[0].id, "todo-1");
        assert!(matches!(next.todos[0].status, TodoStatus::InProgress));
        assert_eq!(next.next_id, 2);
    }

    #[test]
    fn test_add_then_update_first_todo_uses_todo_1() {
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
                id: "todo-1".to_string(),
                content: None,
                status: Some(TodoStatus::Completed),
                details: None,
            }],
        )
        .unwrap();

        assert!(matches!(updated.todos[0].status, TodoStatus::Completed));
    }

    #[test]
    fn test_update_completion_promotes_next_pending_todo() {
        let previous = TodoState {
            todos: vec![
                TodoItem {
                    id: "todo-1".to_string(),
                    content: "Inspect codebase".to_string(),
                    status: TodoStatus::InProgress,
                    details: None,
                },
                TodoItem {
                    id: "todo-2".to_string(),
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
                id: "todo-1".to_string(),
                content: None,
                status: Some(TodoStatus::Completed),
                details: None,
            }],
        )
        .unwrap();

        assert!(matches!(next.todos[0].status, TodoStatus::Completed));
        assert!(matches!(next.todos[1].status, TodoStatus::InProgress));
    }

    #[test]
    fn test_update_ignores_empty_optional_fields() {
        let previous = TodoState {
            todos: vec![TodoItem {
                id: "todo-1".to_string(),
                content: "Inspect codebase".to_string(),
                status: TodoStatus::InProgress,
                details: Some("Check provider schemas".to_string()),
            }],
            next_id: 2,
        };

        let output = execute_with_state(
            &json!({
                "todos": [{
                    "op": "update",
                    "id": "todo-1",
                    "content": "",
                    "details": "",
                    "todos": [],
                    "status": "completed"
                }]
            }),
            Some(&previous),
            None,
        );

        assert!(output.is_ok());
        let state = state_from_output(&output).expect("state from update");
        assert_eq!(state.todos[0].content, "Inspect codebase");
        assert_eq!(
            state.todos[0].details.as_deref(),
            Some("Check provider schemas")
        );
        assert!(matches!(state.todos[0].status, TodoStatus::Completed));
    }

    #[test]
    fn test_multiple_in_progress_todos_are_normalized() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Replace {
                todos: vec![
                    InputTodo {
                        content: "Inspect codebase".to_string(),
                        status: Some(TodoStatus::InProgress),
                        details: None,
                    },
                    InputTodo {
                        content: "Implement fix".to_string(),
                        status: Some(TodoStatus::InProgress),
                        details: None,
                    },
                ],
            }],
        )
        .unwrap();

        assert!(matches!(next.todos[0].status, TodoStatus::InProgress));
        assert!(matches!(next.todos[1].status, TodoStatus::Pending));
    }

    #[test]
    fn test_extract_latest_state_from_thread_events() {
        let output = ToolOutput::success(json!({
            "todos": [
                {
                    "id": "todo-1",
                    "content": "Inspect codebase",
                    "status": "completed"
                },
                {
                    "id": "todo-2",
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
            "summary": "Active: todo-2 (Implement fix). Remaining todos: 1."
        }));
        let events = vec![
            ThreadEvent::tool_use("tool-1", "read", json!({"path": "src/lib.rs"})),
            ThreadEvent::tool_result(
                "tool-1",
                json!({"ok": true, "data": {"file_path": "src/lib.rs"}}),
                true,
            ),
            ThreadEvent::tool_use("tool-2", "Todo_Write", json!({"todos": []})),
            ThreadEvent::tool_result("tool-2", serde_json::to_value(output).unwrap(), true),
        ];

        let state = extract_latest_state(&events).unwrap();
        assert_eq!(state.todos.len(), 2);
        assert_eq!(state.todos[1].id, "todo-2");
        assert!(matches!(state.todos[1].status, TodoStatus::InProgress));
    }

    #[test]
    fn test_recovered_state_supports_follow_up_update_and_add() {
        let output = ToolOutput::success(json!({
            "todos": [
                {
                    "id": "todo-1",
                    "content": "Inspect codebase",
                    "status": "completed"
                },
                {
                    "id": "todo-2",
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
            "summary": "Active: todo-2 (Implement fix). Remaining todos: 1."
        }));
        let events = vec![
            ThreadEvent::tool_use("tool-2", "Todo_Write", json!({"todos": []})),
            ThreadEvent::tool_result("tool-2", serde_json::to_value(output).unwrap(), true),
        ];

        let recovered = extract_latest_state(&events).unwrap();
        assert_eq!(recovered.next_id, 3);

        let next = apply_ops(
            &recovered,
            &[
                TodoOp::Update {
                    id: "todo-2".to_string(),
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

        assert_eq!(next.todos.len(), 3);
        assert!(matches!(next.todos[1].status, TodoStatus::Completed));
        assert_eq!(next.todos[2].id, "todo-3");
        assert_eq!(next.todos[2].content, "Ship fix");
        assert!(matches!(next.todos[2].status, TodoStatus::InProgress));
        assert_eq!(next.next_id, 4);
    }

    #[test]
    fn test_execute_rejects_empty_todos() {
        let output = execute(
            &json!({"todos": []}),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );
        assert!(!output.is_ok());
        let (code, message, _) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "todos cannot be empty");
    }

    #[test]
    fn test_execute_coerces_stringified_todos_array() {
        let output = execute(
            &json!({
                "todos": "[{\"op\":\"add\",\"content\":\"Inspect codebase\"}]"
            }),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(output.is_ok());
        let data = output.data().expect("Todo_Write should return data");
        let todos = data
            .get("todos")
            .and_then(|todos| todos.as_array())
            .expect("Todo_Write should return todos array");
        assert_eq!(todos.len(), 1);
        assert_eq!(
            todos[0].get("content").and_then(|v| v.as_str()),
            Some("Inspect codebase")
        );
    }

    #[test]
    fn test_execute_returns_previous_todos_and_reminder() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        let first = execute_with_state(
            &json!({"todos": [{"op": "add", "content": "Inspect codebase"}]}),
            None,
            None,
        );
        assert!(first.is_ok());
        let state = state_from_output(&first).expect("state from first call");

        let second = execute_with_state(
            &json!({"todos": [{"op": "add", "content": "Implement fix"}]}),
            Some(&state),
            None,
        );
        assert!(second.is_ok());

        let data = second.data().expect("data envelope");
        let previous = data
            .get("previous_todos")
            .and_then(|p| p.as_array())
            .expect("previous_todos array");
        assert_eq!(previous.len(), 1);
        assert_eq!(
            previous[0].get("id").and_then(|id| id.as_str()),
            Some("todo-1")
        );

        let todos = data
            .get("todos")
            .and_then(|t| t.as_array())
            .expect("todos array");
        assert_eq!(todos.len(), 2);

        let reminder = data
            .get("reminder")
            .and_then(|r| r.as_str())
            .expect("reminder string");
        assert!(reminder.contains("Todo_Write"));

        let _ = ctx;
    }

    #[test]
    fn test_execute_rejects_stringified_non_array_todos() {
        let output = execute(
            &json!({
                "todos": "{\"op\":\"add\",\"content\":\"Inspect codebase\"}"
            }),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(!output.is_ok());
        let (code, message, detail) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "field 'todos' must be an array");
        assert!(
            detail
                .unwrap_or_default()
                .contains("parsed as JSON, but not as a JSON array")
        );
    }

    #[test]
    fn test_execute_reports_non_array_todos_type() {
        let output = execute(
            &json!({"todos": {"op": "add", "content": "Inspect codebase"}}),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(!output.is_ok());
        let (code, message, detail) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "Invalid input for Todo_Write tool");
        assert!(
            detail
                .unwrap_or_default()
                .contains("field 'todos' must be an array; received object")
        );
    }

    #[test]
    fn test_execute_reports_invalid_op_after_stringified_todos_coercion() {
        let output = execute(
            &json!({
                "todos": "[{\"op\":\"bogus\",\"content\":\"Inspect codebase\"}]"
            }),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(!output.is_ok());
        let (code, message, detail) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "Invalid input for Todo_Write tool");
        assert!(
            detail
                .unwrap_or_default()
                .contains("unknown variant `bogus`")
        );
    }

    #[test]
    fn test_execute_reports_raw_todo_items_need_mutation_ops() {
        let output = execute(
            &json!({
                "todos": [{"content": "Inspect codebase", "status": "in_progress"}]
            }),
            &ToolContext::new(std::path::PathBuf::from("."), None),
        );

        assert!(!output.is_ok());
        let (code, message, detail) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "Invalid input for Todo_Write tool");
        assert!(
            detail
                .unwrap_or_default()
                .contains("todo items must be wrapped in mutation ops")
        );
    }

    #[test]
    fn test_summary_for_empty_todos() {
        assert_eq!(summary(&[]), "No todos tracked.");
    }

    #[test]
    fn test_is_todo_tool_matches_known_aliases() {
        assert!(is_todo_tool("TodoWrite"));
        assert!(is_todo_tool("todowrite"));
        assert!(is_todo_tool("todo_write"));
        assert!(is_todo_tool("Todo_Write"));
        assert!(is_todo_tool("todo-write"));
        assert!(!is_todo_tool("read"));
        assert!(!is_todo_tool("write"));
    }

    #[test]
    fn test_completed_only_list_has_no_active_todo() {
        let previous = TodoState::default();
        let next = apply_ops(
            &previous,
            &[TodoOp::Replace {
                todos: vec![InputTodo {
                    content: "Done".to_string(),
                    status: Some(TodoStatus::Completed),
                    details: None,
                }],
            }],
        )
        .unwrap();

        assert!(matches!(next.todos[0].status, TodoStatus::Completed));
    }
}
