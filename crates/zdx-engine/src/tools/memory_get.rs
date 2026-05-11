//! Memory get tool.
//!
//! Reads canonical memory records by stable memory ref.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::thread_persistence as tp;

/// Returns the tool definition for the `memory_get` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Memory_Get".to_string(),
        description: "Read a canonical ZDX memory record by memory ref, such as `thread:<thread_id>`, `note:<relative_path>`, or `calendar:<relative_path>` returned by Memory_Search. For thread refs, reads `$ZDX_HOME/threads/<thread_id>.jsonl` as the source of truth and returns transcript data derived from that canonical JSONL, not exported Markdown. For note and calendar refs, reads canonical Markdown under `$ZDX_MEMORY_ROOT/Notes` or `$ZDX_MEMORY_ROOT/Calendar`. Use this after Memory_Search when you need canonical evidence for a returned ref. If you already have a thread_id and need a focused answer to a specific goal, prefer Read_Thread instead."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Stable memory reference to read, such as `thread:<thread_id>`, `note:<relative_path>`, or `calendar:<relative_path>`"
                }
            },
            "required": ["ref"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct MemoryGetInput {
    #[serde(rename = "ref")]
    memory_ref: String,
}

/// Executes the memory get tool and returns canonical memory data.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: MemoryGetInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for memory_get tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let memory_ref = input.memory_ref.trim();
    if memory_ref.is_empty() {
        return ToolOutput::failure("invalid_input", "ref cannot be empty", None);
    }

    let Some((source, id)) = memory_ref.split_once(':') else {
        return ToolOutput::failure("invalid_input", "ref must use '<source>:<id>' format", None);
    };

    let source = source.trim();
    let id = id.trim();
    if source.is_empty() || id.is_empty() {
        return ToolOutput::failure("invalid_input", "ref must include both source and id", None);
    }

    let config = ctx.config.clone().unwrap_or_default();
    match source {
        "thread" => read_thread_ref(id),
        "note" => read_markdown_ref(
            "note",
            id,
            &config.memory.effective_notes_path(),
            "note_not_found",
        ),
        "calendar" => read_markdown_ref(
            "calendar",
            id,
            &config.memory.effective_daily_path(),
            "calendar_not_found",
        ),
        unsupported => ToolOutput::failure(
            "unsupported_source",
            format!("Unsupported memory ref source '{unsupported}'"),
            Some("Supported source types: thread, note, calendar".to_string()),
        ),
    }
}

fn read_markdown_ref(
    source: &str,
    relative_path: &str,
    root: &Path,
    missing_code: &str,
) -> ToolOutput {
    let Some(target) = safe_memory_file_path(root, relative_path) else {
        return ToolOutput::failure(
            "invalid_ref",
            format!("{source} ref must stay within its canonical memory root"),
            None,
        );
    };

    let content = match fs::read_to_string(&target) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ToolOutput::failure(
                missing_code,
                format!("{source} '{relative_path}' not found"),
                Some(format!("Canonical path: {}", target.display())),
            );
        }
        Err(e) => {
            return ToolOutput::failure(
                "read_failed",
                format!("Failed to read {source} '{relative_path}'"),
                Some(format!("Read error: {e}")),
            );
        }
    };

    ToolOutput::success(json!({
        "ref": format!("{source}:{relative_path}"),
        "source": source,
        "relative_path": relative_path,
        "content": content,
    }))
}

fn safe_memory_file_path(root: &Path, relative_path: &str) -> Option<PathBuf> {
    let relative_path = relative_path.trim();
    if relative_path.is_empty() {
        return None;
    }
    let parsed = Path::new(relative_path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }

    let root = fs::canonicalize(root).ok()?;
    let target = root.join(parsed);
    match fs::canonicalize(&target) {
        Ok(canonical_target) => canonical_target
            .starts_with(&root)
            .then_some(canonical_target),
        Err(_) => Some(target),
    }
}

fn read_thread_ref(thread_id: &str) -> ToolOutput {
    let events = match tp::load_thread_events(thread_id) {
        Ok(events) => events,
        Err(e) => {
            return ToolOutput::failure(
                "thread_not_found",
                format!("Thread '{thread_id}' not found"),
                Some(format!("Load error: {e}")),
            );
        }
    };

    if events.is_empty() {
        return ToolOutput::failure(
            "thread_not_found",
            format!("Thread '{thread_id}' not found"),
            Some("Canonical thread JSONL is missing or empty".to_string()),
        );
    }

    ToolOutput::success(json!({
        "ref": format!("thread:{thread_id}"),
        "source": "thread",
        "thread_id": thread_id,
        "title": tp::extract_title_from_events(&events),
        "root_path": tp::extract_root_path_from_events(&events),
        "event_count": events.len(),
        "transcript": tp::format_transcript(&events),
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::config::Config;
    use crate::core::thread_persistence::{Thread, ThreadEvent};

    fn setup_temp_zdx_home() -> &'static TempDir {
        static ZDX_HOME: OnceLock<TempDir> = OnceLock::new();
        ZDX_HOME.get_or_init(|| {
            let temp = TempDir::new().unwrap();
            unsafe { std::env::set_var("ZDX_HOME", temp.path()) };
            temp
        })
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."), None)
    }

    fn test_ctx_with_memory_root(root: &Path) -> ToolContext {
        let mut config = Config::default();
        config.memory.root = Some(root.display().to_string());
        ToolContext::new(PathBuf::from("."), None).with_config(&config)
    }

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Memory_Get");
        assert!(def.description.contains("thread:<thread_id>"));
        assert!(def.description.contains("note:<relative_path>"));
        assert!(def.description.contains("calendar:<relative_path>"));
        assert!(
            def.description
                .contains("$ZDX_HOME/threads/<thread_id>.jsonl")
        );
        assert!(def.description.contains("not exported Markdown"));
        assert!(def.description.contains("prefer Read_Thread"));
        assert_eq!(def.input_schema["required"], json!(["ref"]));
    }

    #[test]
    fn test_rejects_empty_ref() {
        let output = execute(&json!({ "ref": "  " }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "ref cannot be empty");
    }

    #[test]
    fn test_rejects_malformed_ref() {
        let output = execute(&json!({ "ref": "thread-id-only" }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(
            payload["error"]["message"],
            "ref must use '<source>:<id>' format"
        );
    }

    #[test]
    fn test_rejects_unsupported_source() {
        let output = execute(&json!({ "ref": "task:abc" }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "unsupported_source");
        assert_eq!(
            payload["error"]["message"],
            "Unsupported memory ref source 'task'"
        );
    }

    #[test]
    fn test_note_ref_reads_canonical_markdown() {
        let temp = TempDir::new().unwrap();
        let note_path = temp.path().join("Notes").join("Projects").join("ZDX.md");
        fs::create_dir_all(note_path.parent().unwrap()).unwrap();
        fs::write(&note_path, "# ZDX\n\nCanonical note content").unwrap();

        let output = execute(
            &json!({ "ref": "note:Projects/ZDX.md" }),
            &test_ctx_with_memory_root(temp.path()),
        );

        assert!(output.is_ok());
        let data = output.data().unwrap();
        assert_eq!(data["ref"], "note:Projects/ZDX.md");
        assert_eq!(data["source"], "note");
        assert_eq!(data["relative_path"], "Projects/ZDX.md");
        assert!(
            data["content"]
                .as_str()
                .unwrap()
                .contains("Canonical note content")
        );
    }

    #[test]
    fn test_calendar_ref_reads_canonical_markdown() {
        let temp = TempDir::new().unwrap();
        let calendar_path = temp.path().join("Calendar").join("2026-05-11.md");
        fs::create_dir_all(calendar_path.parent().unwrap()).unwrap();
        fs::write(&calendar_path, "# 2026-05-11\n\nCalendar note content").unwrap();

        let output = execute(
            &json!({ "ref": "calendar:2026-05-11.md" }),
            &test_ctx_with_memory_root(temp.path()),
        );

        assert!(output.is_ok());
        let data = output.data().unwrap();
        assert_eq!(data["ref"], "calendar:2026-05-11.md");
        assert_eq!(data["source"], "calendar");
        assert_eq!(data["relative_path"], "2026-05-11.md");
        assert!(
            data["content"]
                .as_str()
                .unwrap()
                .contains("Calendar note content")
        );
    }

    #[test]
    fn test_note_ref_rejects_path_traversal() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("Notes")).unwrap();

        let output = execute(
            &json!({ "ref": "note:../secret.md" }),
            &test_ctx_with_memory_root(temp.path()),
        );

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_ref");
    }

    #[cfg(unix)]
    #[test]
    fn test_calendar_ref_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let calendar_dir = temp.path().join("Calendar");
        fs::create_dir_all(&calendar_dir).unwrap();
        let outside = temp.path().join("outside.md");
        fs::write(&outside, "outside").unwrap();
        let link = calendar_dir.join("escape.md");
        symlink(&outside, &link).unwrap();

        let output = execute(
            &json!({ "ref": "calendar:escape.md" }),
            &test_ctx_with_memory_root(temp.path()),
        );

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_ref");
    }

    #[test]
    fn test_missing_thread_ref_returns_clear_error() {
        let _temp = setup_temp_zdx_home();
        let output = execute(
            &json!({ "ref": "thread:missing-memory-get-thread" }),
            &test_ctx(),
        );

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "thread_not_found");
        assert_eq!(
            payload["error"]["message"],
            "Thread 'missing-memory-get-thread' not found"
        );
    }

    #[test]
    fn test_thread_ref_reads_canonical_thread_jsonl() {
        let _temp = setup_temp_zdx_home();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = format!("memory-get-{nanos}");
        let mut thread = Thread::with_id(thread_id.clone()).unwrap();
        thread
            .append(&ThreadEvent::user_message("canonical user question"))
            .unwrap();
        thread
            .append(&ThreadEvent::assistant_message(
                "canonical assistant answer",
            ))
            .unwrap();

        let output = execute(
            &json!({ "ref": format!("thread:{thread_id}") }),
            &test_ctx(),
        );

        assert!(output.is_ok());
        let data = output.data().unwrap();
        assert_eq!(data["ref"], format!("thread:{thread_id}"));
        assert_eq!(data["source"], "thread");
        assert_eq!(data["thread_id"], thread_id);
        assert_eq!(data["event_count"], 3);
        let transcript = data["transcript"].as_str().unwrap();
        assert!(transcript.contains("canonical user question"));
        assert!(transcript.contains("canonical assistant answer"));
    }
}
