//! Todo write tool for structured task tracking.
//!
//! The call is a whole-list snapshot: every call carries the complete list and
//! replaces the previous one. There is no server-held state to reconcile, so
//! the tool is a pure function of its input — no thread scanning, no ids, no
//! per-item mutation ops.
//!
//! Statuses are validated, never rewritten. Several `in_progress` items are
//! legal because concurrent work is normal here, and a list with nothing
//! active is equally legal; the model owns the plan.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ToolDefinition;
use crate::core::events::ToolOutput;

/// Returns the tool definition for the `Todo_Write` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Todo_Write".to_string(),
        description: "Create and manage a structured todo list for the current thread. Use this for tasks with 3+ meaningful steps, multiple requested changes, or work that benefits from visible progress. Send the complete list every time: `todos` fully replaces the previous list, so include finished items with their final status, and send `[]` to clear the list. `todos` must be a real JSON array, not a quoted JSON string, and every item needs `content` and `status`. Mark an item `in_progress` when you start it and `completed` as soon as it lands; several items may be `in_progress` at once when work genuinely runs in parallel. Use `abandoned` to close an item you are not going to do. Statuses are stored exactly as sent. Example: {\"todos\":[{\"content\":\"Inspect bug\",\"status\":\"completed\"},{\"content\":\"Write fix\",\"status\":\"in_progress\"},{\"content\":\"Verify\",\"status\":\"pending\"}]}".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete todo list, replacing the previous one. Must be a JSON array, not a stringified JSON array. Send `[]` to clear the list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "Short todo label."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "abandoned"],
                                "description": "Current status of this todo."
                            }
                        },
                        "required": ["content", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["todos"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TodoItem {
    content: String,
    status: TodoStatus,
}

#[derive(Debug, Clone, Deserialize)]
struct TodoInput {
    todos: Vec<TodoItem>,
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
pub fn execute(input: &Value) -> ToolOutput {
    let normalized_input = match normalize_input(input) {
        Ok(value) => value,
        Err((message, detail)) => {
            return ToolOutput::failure("invalid_input", message, detail);
        }
    };

    let input: TodoInput = match serde_json::from_value(normalized_input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for Todo_Write tool",
                Some(describe_parse_error(&normalized_input, &error)),
            );
        }
    };

    let todos = match validate(input.todos) {
        Ok(todos) => todos,
        Err(message) => return ToolOutput::failure("invalid_input", message, None),
    };

    let summary_text = summary(&todos);
    ToolOutput::success(json!({
        "todos": todos,
        "counts": counts(&todos),
        "summary": summary_text,
    }))
}

/// Trims labels and rejects anything the schema cannot express. Statuses are
/// checked, never rewritten: the caller owns which items are active.
fn validate(todos: Vec<TodoItem>) -> Result<Vec<TodoItem>, String> {
    todos
        .into_iter()
        .map(|todo| {
            let content = todo.content.trim();
            if content.is_empty() {
                return Err("every todo needs non-empty content".to_string());
            }
            Ok(TodoItem {
                content: content.to_string(),
                status: todo.status,
            })
        })
        .collect()
}

/// Accepts a stringified `todos` array. Models occasionally send the array as a
/// JSON string; parsing it is cheaper than failing a whole turn over quoting.
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
            && items
                .iter()
                .any(|item| item.as_object().is_some_and(|item| item.contains_key("op")))
        {
            return format!(
                "todos is the complete list, not a list of mutations; send every todo as {{\"content\":…,\"status\":…}}. Parse error: {error}"
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
    if todos.is_empty() {
        return "No todos tracked.".to_string();
    }

    let active: Vec<&str> = todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::InProgress))
        .map(|todo| todo.content.as_str())
        .collect();
    let remaining = todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Pending | TodoStatus::InProgress))
        .count();

    if active.is_empty() {
        return format!("No active todo. Remaining todos: {remaining}.");
    }
    format!(
        "Active: {}. Remaining todos: {remaining}.",
        active.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn todos_from(output: &ToolOutput) -> Vec<TodoItem> {
        let data = output.data().expect("Todo_Write should return data");
        serde_json::from_value(data.get("todos").expect("todos field").clone())
            .expect("todos should deserialize")
    }

    #[test]
    fn replaces_the_whole_list_on_every_call() {
        let first = execute(&json!({"todos": [
            {"content": "Inspect bug", "status": "in_progress"},
            {"content": "Write fix", "status": "pending"}
        ]}));
        assert!(first.is_ok());
        assert_eq!(todos_from(&first).len(), 2);

        let second = execute(&json!({"todos": [
            {"content": "Ship it", "status": "pending"}
        ]}));
        let todos = todos_from(&second);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "Ship it");
        assert_eq!(todos[0].status, TodoStatus::Pending);
    }

    #[test]
    fn empty_list_clears_and_is_not_an_error() {
        let output = execute(&json!({"todos": []}));
        assert!(output.is_ok());
        assert!(todos_from(&output).is_empty());

        let data = output.data().unwrap();
        assert_eq!(data["counts"]["total"], 0);
        assert_eq!(data["summary"], "No todos tracked.");
    }

    #[test]
    fn statuses_are_stored_exactly_as_sent() {
        // Neither demoted nor promoted: several active items are legal, and so
        // is a list with pending work and nothing active.
        let output = execute(&json!({"todos": [
            {"content": "Worker A", "status": "in_progress"},
            {"content": "Worker B", "status": "in_progress"},
            {"content": "Later", "status": "pending"}
        ]}));
        let todos = todos_from(&output);
        assert_eq!(todos[0].status, TodoStatus::InProgress);
        assert_eq!(todos[1].status, TodoStatus::InProgress);
        assert_eq!(todos[2].status, TodoStatus::Pending);
        assert_eq!(output.data().unwrap()["counts"]["in_progress"], 2);

        let idle = execute(&json!({"todos": [
            {"content": "Not started", "status": "pending"}
        ]}));
        assert_eq!(todos_from(&idle)[0].status, TodoStatus::Pending);
        assert_eq!(
            idle.data().unwrap()["summary"],
            "No active todo. Remaining todos: 1."
        );
    }

    #[test]
    fn summary_lists_every_active_todo() {
        let output = execute(&json!({"todos": [
            {"content": "Alpha", "status": "in_progress"},
            {"content": "Beta", "status": "in_progress"},
            {"content": "Gamma", "status": "completed"}
        ]}));
        assert_eq!(
            output.data().unwrap()["summary"],
            "Active: Alpha, Beta. Remaining todos: 2."
        );
    }

    #[test]
    fn rejects_empty_content() {
        let output = execute(&json!({"todos": [{"content": "  ", "status": "pending"}]}));
        assert!(!output.is_ok());
        let (code, message, _) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "every todo needs non-empty content");
    }

    #[test]
    fn rejects_missing_status() {
        let output = execute(&json!({"todos": [{"content": "Inspect bug"}]}));
        assert!(!output.is_ok());
        let (code, message, _) = output.error_info().unwrap();
        assert_eq!(code, "invalid_input");
        assert_eq!(message, "Invalid input for Todo_Write tool");
    }

    #[test]
    fn rejects_unknown_status() {
        let output = execute(&json!({"todos": [{"content": "Inspect", "status": "blocked"}]}));
        assert!(!output.is_ok());
        let (_, _, detail) = output.error_info().unwrap();
        assert!(detail.unwrap_or_default().contains("unknown variant"));
    }

    #[test]
    fn coerces_a_stringified_todos_array() {
        let output = execute(&json!({
            "todos": "[{\"content\":\"Inspect codebase\",\"status\":\"pending\"}]"
        }));
        assert!(output.is_ok());
        assert_eq!(todos_from(&output)[0].content, "Inspect codebase");
    }

    #[test]
    fn rejects_a_stringified_non_array() {
        let output = execute(&json!({
            "todos": "{\"content\":\"Inspect codebase\",\"status\":\"pending\"}"
        }));
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
    fn reports_non_array_todos_type() {
        let output = execute(&json!({"todos": {"content": "Inspect", "status": "pending"}}));
        assert!(!output.is_ok());
        let (_, _, detail) = output.error_info().unwrap();
        assert!(
            detail
                .unwrap_or_default()
                .contains("field 'todos' must be an array; received object")
        );
    }

    #[test]
    fn points_legacy_mutation_ops_at_the_whole_list_shape() {
        let output = execute(&json!({
            "todos": [{"op": "add", "content": "Inspect codebase"}]
        }));
        assert!(!output.is_ok());
        let (_, _, detail) = output.error_info().unwrap();
        assert!(
            detail
                .unwrap_or_default()
                .contains("todos is the complete list, not a list of mutations")
        );
    }

    #[test]
    fn trims_content() {
        let output =
            execute(&json!({"todos": [{"content": "  Inspect bug  ", "status": "pending"}]}));
        assert_eq!(todos_from(&output)[0].content, "Inspect bug");
    }
}
