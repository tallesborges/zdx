use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::config::paths::threads_dir;
use crate::core::agent::create_event_channel;
use crate::core::events::{AgentEvent, TurnStatus};
use crate::providers::ReplayToken;

#[test]
fn extract_handoff_from_reads_meta_parent() {
    let with_parent = vec![ThreadEvent::meta_with_root_and_source(
        None,
        Some("parent-123".to_string()),
    )];
    assert_eq!(
        extract_handoff_from_from_events(&with_parent),
        Some("parent-123".to_string())
    );

    let without_parent = vec![ThreadEvent::meta_with_root_and_source(None, None)];
    assert_eq!(extract_handoff_from_from_events(&without_parent), None);

    assert_eq!(extract_handoff_from_from_events(&[]), None);
}

fn setup_temp_zdx_home() -> &'static TempDir {
    static ZDX_HOME: OnceLock<TempDir> = OnceLock::new();
    ZDX_HOME.get_or_init(|| {
        let temp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("ZDX_HOME", temp.path());
        }
        temp
    })
}

fn unique_thread_id(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{prefix}-{nanos}")
}

#[test]
fn test_thread_creates_file_with_meta() {
    let _temp = setup_temp_zdx_home();

    let mut thread = Thread::with_id(unique_thread_id("creates-meta")).unwrap();
    thread.append(&ThreadEvent::user_message("hello")).unwrap();

    // Read raw file content to verify meta is first
    let content = fs::read_to_string(&thread.path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert!(lines.len() >= 2);
    assert!(lines[0].contains("\"type\":\"meta\""));
    assert!(lines[0].contains("\"schema_version\":1"));
}

#[test]
fn test_thread_appends_jsonl_with_tool_events() {
    let _temp = setup_temp_zdx_home();

    let mut thread = Thread::with_id(unique_thread_id("tool-events")).unwrap();
    thread
        .append(&ThreadEvent::user_message("read main.rs"))
        .unwrap();
    thread
        .append(&ThreadEvent::tool_use(
            "tool-1",
            "read",
            json!({"file_path": "main.rs"}),
        ))
        .unwrap();
    thread
        .append(&ThreadEvent::tool_result(
            "tool-1",
            json!({"ok": true, "data": {"content": "fn main() {}"}}),
            true,
        ))
        .unwrap();
    thread
        .append(&ThreadEvent::assistant_message("Here's the file"))
        .unwrap();

    let events = thread.read_events().unwrap();
    // meta + user + tool_use + tool_result + assistant = 5 events
    assert_eq!(events.len(), 5);
    assert!(matches!(events[0], ThreadEvent::Meta { .. }));
    assert!(matches!(events[1], ThreadEvent::Message { ref role, .. } if role == "user"));
    assert!(matches!(events[2], ThreadEvent::ToolUse { ref name, .. } if name == "read"));
    assert!(matches!(
        events[3],
        ThreadEvent::ToolResult { ok: true, .. }
    ));
    assert!(matches!(events[4], ThreadEvent::Message { ref role, .. } if role == "assistant"));
}

#[test]
fn test_thread_event_serialization() {
    let meta = ThreadEvent::meta_with_root(None);
    let json = serde_json::to_string(&meta).unwrap();
    assert!(json.contains("\"type\":\"meta\""));
    assert!(json.contains("\"schema_version\":1"));

    let tool_use = ThreadEvent::tool_use("t1", "bash", json!({"command": "ls"}));
    let json = serde_json::to_string(&tool_use).unwrap();
    assert!(json.contains("\"type\":\"tool_use\""));
    assert!(json.contains("\"name\":\"bash\""));

    let tool_result = ThreadEvent::tool_result("t1", json!({"stdout": "file.txt"}), true);
    let json = serde_json::to_string(&tool_result).unwrap();
    assert!(json.contains("\"type\":\"tool_result\""));
    assert!(json.contains("\"ok\":true"));

    let interrupted = ThreadEvent::interrupted();
    let json = serde_json::to_string(&interrupted).unwrap();
    assert!(json.contains("\"type\":\"interrupted\""));
    assert!(json.contains("\"role\":\"system\""));
    assert!(json.contains("\"text\":\"Interrupted\""));

    let reasoning = ThreadEvent::reasoning(
        Some("summary".to_string()),
        Some(crate::providers::ReplayToken::OpenAI {
            id: "r1".to_string(),
            encrypted_content: "encrypted".to_string(),
        }),
    );
    let json = serde_json::to_string(&reasoning).unwrap();
    assert!(json.contains("\"type\":\"reasoning\""));
    assert!(json.contains("\"provider\":\"openai\""));
    assert!(json.contains("\"id\":\"r1\""));
    assert!(json.contains("\"encrypted_content\":\"encrypted\""));
    assert!(json.contains("\"text\":\"summary\""));

    let assistant =
        ThreadEvent::assistant_message_with_phase("Done.", Some("final_answer".to_string()));
    let json = serde_json::to_string(&assistant).unwrap();
    assert!(json.contains("\"type\":\"message\""));
    assert!(json.contains("\"role\":\"assistant\""));
    assert!(json.contains("\"phase\":\"final_answer\""));

    // Test Gemini replay token serialization
    let reasoning_gemini = ThreadEvent::reasoning(
        Some("thought summary".to_string()),
        Some(crate::providers::ReplayToken::Gemini {
            signature: "base64sig".to_string(),
            model: String::new(),
        }),
    );
    let json = serde_json::to_string(&reasoning_gemini).unwrap();
    assert!(json.contains("\"type\":\"reasoning\""));
    assert!(json.contains("\"provider\":\"gemini\""));
    assert!(json.contains("\"signature\":\"base64sig\""));
    assert!(json.contains("\"text\":\"thought summary\""));
}

#[test]
fn test_events_to_messages_with_tools() {
    // Test the conversion logic directly without env var dependency
    let events = vec![
        ThreadEvent::user_message("list files"),
        ThreadEvent::tool_use("t1", "bash", json!({"command": "ls"})),
        ThreadEvent::tool_result("t1", json!({"stdout": "file.txt\n"}), true),
        ThreadEvent::assistant_message("Found file.txt"),
    ];

    let messages = thread_events_to_messages(events);

    // user message + assistant with tool_use block + tool_results + assistant message = 4
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, "user");
    // Second message should be assistant with tool_use blocks
    assert_eq!(messages[1].role, "assistant");
    // Third message should be tool results (role "user")
    assert_eq!(messages[2].role, "user");
    // Fourth is final assistant message
    assert_eq!(messages[3].role, "assistant");
}

/// Replay-side contract: adjacent assistant `Message` events with
/// per-event `replay` tokens round-trip as separate
/// `ChatContentBlock::Text` blocks. TUI restore coalesces these for
/// display, but the replay path must preserve each block's signature
/// so implicit-cache fingerprints line up on the next request.
#[test]
fn test_thread_events_to_messages_preserves_replay_tokens_across_fragments() {
    let events = vec![
        ThreadEvent::user_message("hi"),
        ThreadEvent::Message {
            role: "assistant".to_string(),
            text: "Hello **wor".to_string(),
            phase: None,
            replay: Some(crate::providers::ReplayToken::Gemini {
                signature: "sig-first".to_string(),
                model: "gemini-3-pro-preview".to_string(),
            }),
            ts: "2026-05-15T00:00:00Z".to_string(),
        },
        ThreadEvent::Message {
            role: "assistant".to_string(),
            text: "ld**".to_string(),
            phase: None,
            replay: Some(crate::providers::ReplayToken::Gemini {
                signature: "sig-second".to_string(),
                model: "gemini-3-pro-preview".to_string(),
            }),
            ts: "2026-05-15T00:00:01Z".to_string(),
        },
    ];

    let messages = thread_events_to_messages(events);
    assert_eq!(messages.len(), 2, "expected user + assistant messages");
    assert_eq!(messages[1].role, "assistant");

    let blocks = match &messages[1].content {
        crate::providers::MessageContent::Blocks(b) => b,
        other @ crate::providers::MessageContent::Text(_) => {
            panic!("expected assistant message in Blocks form, got {other:?}")
        }
    };

    let text_blocks: Vec<(&str, Option<&crate::providers::ReplayToken>)> = blocks
        .iter()
        .filter_map(|b| match b {
            crate::providers::ChatContentBlock::Text { text, replay } => {
                Some((text.as_str(), replay.as_ref()))
            }
            _ => None,
        })
        .collect();

    assert_eq!(
        text_blocks.len(),
        2,
        "replay must keep adjacent assistant text fragments as separate blocks; got {text_blocks:#?}"
    );
    assert_eq!(text_blocks[0].0, "Hello **wor");
    assert_eq!(text_blocks[1].0, "ld**");
    assert!(
        matches!(
            text_blocks[0].1,
            Some(crate::providers::ReplayToken::Gemini { signature, .. }) if signature == "sig-first"
        ),
        "first block must keep sig-first; got {:?}",
        text_blocks[0].1
    );
    assert!(
        matches!(
            text_blocks[1].1,
            Some(crate::providers::ReplayToken::Gemini { signature, .. }) if signature == "sig-second"
        ),
        "second block must keep sig-second; got {:?}",
        text_blocks[1].1
    );
}

#[test]
fn notice_agent_event_persists_as_thread_notice_not_message() {
    use crate::core::events::{AgentEvent, NoticeKind};

    let evt = AgentEvent::Notice {
        kind: NoticeKind::Refusal,
        message: "Claude declined.".to_string(),
        details: Some("stop_reason=refusal".to_string()),
    };

    let persisted = ThreadEvent::from_agent(&evt).expect("Notice should persist");
    match persisted {
        ThreadEvent::Notice { kind, message, .. } => {
            assert_eq!(kind, NoticeKind::Refusal);
            assert_eq!(message, "Claude declined.");
        }
        other => panic!("expected ThreadEvent::Notice, got {other:?}"),
    }
}

#[test]
fn notice_thread_event_is_not_replayed_as_chat_message() {
    // Critical contract: notices MUST NOT be re-sent to providers as
    // part of the conversation history on thread reload. They are
    // UI-only informational events.
    let events = vec![
        ThreadEvent::user_message("hi"),
        ThreadEvent::Notice {
            kind: zdx_types::NoticeKind::ContextWindowExceeded,
            message: "Context window exceeded.".to_string(),
            ts: "2026-04-16T00:00:00Z".to_string(),
        },
        ThreadEvent::assistant_message("hello"),
    ];

    let messages = thread_events_to_messages(events);
    // Only user + assistant — notice must be filtered out.
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].role, "assistant");
    // No message should contain the notice text.
    for m in &messages {
        let text = format!("{:?}", m.content);
        assert!(
            !text.contains("Context window exceeded"),
            "notice text leaked into chat history: {text}"
        );
    }
}

#[test]
fn notice_thread_event_wire_format_roundtrips_as_tagged_json() {
    // Locks the on-disk JSONL contract: notice events are written as
    // `{"type":"notice","kind":"refusal","message":"...","ts":"..."}`
    // and round-trip cleanly through serde.
    let line = r#"{"type":"notice","kind":"refusal","message":"Claude declined.","ts":"2026-04-16T00:00:00Z"}"#;
    let parsed: ThreadEvent = serde_json::from_str(line).expect("parse notice");
    match &parsed {
        ThreadEvent::Notice { kind, message, ts } => {
            assert_eq!(*kind, zdx_types::NoticeKind::Refusal);
            assert_eq!(message, "Claude declined.");
            assert_eq!(ts, "2026-04-16T00:00:00Z");
        }
        other => panic!("expected ThreadEvent::Notice, got {other:?}"),
    }
    let reserialized = serde_json::to_string(&parsed).expect("serialize");
    assert!(reserialized.contains("\"type\":\"notice\""));
    assert!(reserialized.contains("\"kind\":\"refusal\""));
    // And it still does not leak into chat message replay.
    let messages = thread_events_to_messages(vec![parsed]);
    assert!(messages.is_empty(), "notice must be filtered from replay");
}

#[test]
fn test_events_to_messages_with_openai_reasoning() {
    use crate::providers::{ChatContentBlock, MessageContent, ReasoningBlock, ReplayToken};

    let events = vec![
        ThreadEvent::user_message("explain this"),
        ThreadEvent::reasoning(
            Some("summary".to_string()),
            Some(ReplayToken::OpenAI {
                id: "r1".to_string(),
                encrypted_content: "enc".to_string(),
            }),
        ),
        ThreadEvent::assistant_message("Here is the answer."),
    ];

    let messages = thread_events_to_messages(events);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");

    match &messages[1].content {
        MessageContent::Blocks(blocks) => {
            assert!(matches!(
                blocks.first(),
                Some(ChatContentBlock::Reasoning(ReasoningBlock {
                    replay: Some(ReplayToken::OpenAI { id, .. }),
                    ..
                })) if id == "r1"
            ));
            assert!(matches!(
                blocks.last(),
                Some(ChatContentBlock::Text { text, .. }) if text == "Here is the answer."
            ));
        }
        MessageContent::Text(_) => panic!("Expected assistant message with blocks"),
    }
}

#[test]
fn test_events_to_messages_preserves_assistant_phase() {
    let events = vec![ThreadEvent::assistant_message_with_phase(
        "Working on it.",
        Some("commentary".to_string()),
    )];

    let messages = thread_events_to_messages(events);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "assistant");
    assert_eq!(messages[0].phase.as_deref(), Some("commentary"));
}

#[test]
fn test_thread_persistence_options_no_save() {
    let opts = ThreadPersistenceOptions {
        no_save: true,
        ..Default::default()
    };
    assert!(opts.resolve(Path::new(".")).unwrap().is_none());
}

#[test]
fn test_thread_persistence_options_with_id() {
    let _temp = setup_temp_zdx_home();

    let id = unique_thread_id("existing");
    let opts = ThreadPersistenceOptions {
        thread_id: Some(id.clone()),
        ..Default::default()
    };
    let thread = opts.resolve(Path::new(".")).unwrap().unwrap();
    assert_eq!(thread.id, id);
}

#[test]
fn test_format_transcript_with_tools() {
    let events = vec![
        ThreadEvent::meta_with_root(None),
        ThreadEvent::user_message("read main.rs"),
        ThreadEvent::tool_use("t1", "read", json!({"file_path": "main.rs"})),
        ThreadEvent::tool_result(
            "t1",
            json!({"ok": true, "data": {"content": "fn main() {}"}}),
            true,
        ),
        ThreadEvent::assistant_message("Here's the file content."),
    ];

    let transcript = format_transcript(&events);
    assert!(transcript.contains("Thread (schema v1)"));
    assert!(transcript.contains("### You"));
    assert!(transcript.contains("### Tool: read"));
    assert!(transcript.contains("### Result ✓"));
    assert!(transcript.contains("### Assistant"));
}

#[test]
fn test_reasoning_event_deserialization() {
    let json = r#"{"type":"reasoning","text":"sum","replay":{"provider":"openai","id":"r1","encrypted_content":"enc"},"ts":"2024-01-01T00:00:00Z"}"#;
    let event: ThreadEvent = serde_json::from_str(json).unwrap();
    match event {
        ThreadEvent::Reasoning { text, replay, .. } => {
            assert_eq!(text, Some("sum".to_string()));
            assert_eq!(
                replay,
                Some(crate::providers::ReplayToken::OpenAI {
                    id: "r1".to_string(),
                    encrypted_content: "enc".to_string(),
                })
            );
        }
        _ => panic!("Expected Reasoning event"),
    }
}

#[test]
fn test_gemini_reasoning_event_deserialization() {
    let json = r#"{"type":"reasoning","text":"thought summary","replay":{"provider":"gemini","signature":"base64sig"},"ts":"2024-01-01T00:00:00Z"}"#;
    let event: ThreadEvent = serde_json::from_str(json).unwrap();
    match event {
        ThreadEvent::Reasoning { text, replay, .. } => {
            assert_eq!(text, Some("thought summary".to_string()));
            assert_eq!(
                replay,
                Some(crate::providers::ReplayToken::Gemini {
                    signature: "base64sig".to_string(),
                    model: String::new(),
                })
            );
        }
        _ => panic!("Expected Reasoning event"),
    }
}

#[test]
fn test_events_to_messages_with_reasoning() {
    use crate::providers::{ChatContentBlock, MessageContent, ReasoningBlock, ReplayToken};

    let events = vec![
        ThreadEvent::user_message("solve this problem"),
        ThreadEvent::reasoning(
            Some("Let me think about this...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig123".to_string(),
            }),
        ),
        ThreadEvent::tool_use("t1", "bash", json!({"command": "echo test"})),
        ThreadEvent::tool_result("t1", json!({"stdout": "test\n"}), true),
        ThreadEvent::assistant_message("Done!"),
    ];

    let messages = thread_events_to_messages(events);

    // user + assistant(thinking + tool_use) + tool_results + assistant = 4
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, "user");

    // Second message should be assistant with thinking + tool_use blocks
    assert_eq!(messages[1].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[1].content {
        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            &blocks[0],
            ChatContentBlock::Reasoning(ReasoningBlock {
                text: Some(thinking),
                replay: Some(ReplayToken::Anthropic { signature }),
            }) if thinking == "Let me think about this..." && signature == "sig123"
        ));
        assert!(matches!(&blocks[1], ChatContentBlock::ToolUse { .. }));
    } else {
        panic!("Expected Blocks content");
    }
}

#[test]
fn test_events_to_messages_reasoning_then_text() {
    // Test case for the bug: reasoning followed directly by assistant text (no tool use)
    // This should produce a SINGLE assistant message with [reasoning, text] blocks,
    // NOT two separate messages. The API rejects modifications to thinking blocks
    // in the latest assistant message, so they must be in the same message.
    use crate::providers::{ChatContentBlock, MessageContent, ReasoningBlock, ReplayToken};

    let events = vec![
        ThreadEvent::user_message("explain this"),
        ThreadEvent::reasoning(
            Some("Let me analyze...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig456".to_string(),
            }),
        ),
        ThreadEvent::assistant_message("Here's my explanation."),
    ];

    let messages = thread_events_to_messages(events);

    // user + assistant(thinking + text) = 2 messages (NOT 3!)
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");

    // Second message should be assistant with BOTH thinking AND text blocks
    assert_eq!(messages[1].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[1].content {
        assert_eq!(blocks.len(), 2, "Should have 2 blocks: reasoning + text");
        assert!(
            matches!(
                &blocks[0],
                ChatContentBlock::Reasoning(ReasoningBlock {
                    text: Some(thinking),
                    replay: Some(ReplayToken::Anthropic { signature }),
                }) if thinking == "Let me analyze..." && signature == "sig456"
            ),
            "First block should be reasoning"
        );
        assert!(
            matches!(&blocks[1], ChatContentBlock::Text { text, .. } if text == "Here's my explanation."),
            "Second block should be text"
        );
    } else {
        panic!("Expected Blocks content, got {:?}", messages[1].content);
    }
}

#[test]
fn test_events_to_messages_tool_use_then_reasoning() {
    // Regression test for the bug: when a tool call is followed by another reasoning block,
    // the second reasoning must belong to the FINAL assistant message, not the tool_use message.
    //
    // Sequence: user → reasoning1 → tool_use → tool_result → reasoning2 → assistant_text
    //
    // Expected messages:
    // 1. User: "question"
    // 2. Assistant: [Reasoning1, ToolUse]
    // 3. User: [ToolResult]
    // 4. Assistant: [Reasoning2, Text]
    use crate::providers::{ChatContentBlock, MessageContent, ReasoningBlock, ReplayToken};

    let events = vec![
        ThreadEvent::user_message("run a command"),
        ThreadEvent::reasoning(
            Some("Let me run this...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig1".to_string(),
            }),
        ),
        ThreadEvent::tool_use("t1", "bash", json!({"command": "echo hello"})),
        ThreadEvent::tool_result("t1", json!({"stdout": "hello\n"}), true),
        ThreadEvent::reasoning(
            Some("Now let me explain...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig2".to_string(),
            }),
        ),
        ThreadEvent::assistant_message("The command output was 'hello'."),
    ];

    let messages = thread_events_to_messages(events);

    // user + assistant(reasoning1 + tool_use) + user(tool_result) + assistant(reasoning2 + text) = 4
    assert_eq!(messages.len(), 4, "Should have 4 messages");

    // Message 0: User
    assert_eq!(messages[0].role, "user");

    // Message 1: Assistant with reasoning1 + tool_use
    assert_eq!(messages[1].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[1].content {
        assert_eq!(blocks.len(), 2, "First assistant should have 2 blocks");
        assert!(
            matches!(
                &blocks[0],
                ChatContentBlock::Reasoning(ReasoningBlock {
                    text: Some(thinking),
                    replay: Some(ReplayToken::Anthropic { signature }),
                }) if thinking == "Let me run this..." && signature == "sig1"
            ),
            "First block should be reasoning1"
        );
        assert!(
            matches!(&blocks[1], ChatContentBlock::ToolUse { name, .. } if name == "bash"),
            "Second block should be tool_use"
        );
    } else {
        panic!("Expected Blocks content for message 1");
    }

    // Message 2: User with tool_result
    assert_eq!(messages[2].role, "user");

    // Message 3: Assistant with reasoning2 + text (THE KEY ASSERTION)
    assert_eq!(messages[3].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[3].content {
        assert_eq!(
            blocks.len(),
            2,
            "Final assistant should have 2 blocks: reasoning2 + text"
        );
        assert!(
            matches!(
                &blocks[0],
                ChatContentBlock::Reasoning(ReasoningBlock {
                    text: Some(thinking),
                    replay: Some(ReplayToken::Anthropic { signature }),
                }) if thinking == "Now let me explain..." && signature == "sig2"
            ),
            "First block should be reasoning2 (not attached to tool_use message!)"
        );
        assert!(
            matches!(&blocks[1], ChatContentBlock::Text { text, .. }
                if text == "The command output was 'hello'."
            ),
            "Second block should be text"
        );
    } else {
        panic!(
            "Expected Blocks content for message 3, got {:?}",
            messages[3].content
        );
    }
}

#[test]
fn test_events_to_messages_consecutive_tool_calls() {
    // Regression test: consecutive tool calls must have their results
    // placed immediately after each tool_use, not after the next tool_use.
    //
    // Sequence: user → reasoning1 → tool_use1 → tool_result1 → reasoning2 → tool_use2 → tool_result2
    //
    // Expected messages:
    // 1. User: "question"
    // 2. Assistant: [Reasoning1, ToolUse1]
    // 3. User: [ToolResult1]
    // 4. Assistant: [Reasoning2, ToolUse2]
    // 5. User: [ToolResult2]
    //
    // The bug: tool_result1 was being placed AFTER reasoning2+tool_use2, causing
    // "tool_use ids were found without tool_result blocks immediately after" error.
    use crate::providers::{ChatContentBlock, MessageContent, ReplayToken};

    let events = vec![
        ThreadEvent::user_message("run two commands"),
        ThreadEvent::reasoning(
            Some("First, let me run the first command...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig1".to_string(),
            }),
        ),
        ThreadEvent::tool_use("t1", "bash", json!({"command": "echo one"})),
        ThreadEvent::tool_result("t1", json!({"stdout": "one\n"}), true),
        ThreadEvent::reasoning(
            Some("Now let me run the second command...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig2".to_string(),
            }),
        ),
        ThreadEvent::tool_use("t2", "bash", json!({"command": "echo two"})),
        ThreadEvent::tool_result("t2", json!({"stdout": "two\n"}), true),
        ThreadEvent::assistant_message("Both commands completed."),
    ];

    let messages = thread_events_to_messages(events);

    // user + assistant1 + result1 + assistant2 + result2 + assistant_final = 6
    assert_eq!(messages.len(), 6, "Should have 6 messages");

    // Message 0: User
    assert_eq!(messages[0].role, "user");

    // Message 1: Assistant with reasoning1 + tool_use1
    assert_eq!(messages[1].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[1].content {
        assert_eq!(blocks.len(), 2, "First assistant should have 2 blocks");
        assert!(
            matches!(&blocks[1], ChatContentBlock::ToolUse { id, .. } if id == "t1"),
            "Should have tool_use t1"
        );
    } else {
        panic!("Expected Blocks content");
    }

    // Message 2: User with tool_result1 - THE KEY ASSERTION
    // Before the fix, this was message 4 (after the second assistant message)
    assert_eq!(
        messages[2].role, "user",
        "Message 2 should be user (tool_result)"
    );
    if let MessageContent::Blocks(blocks) = &messages[2].content {
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                ChatContentBlock::ToolResult(result) if result.tool_use_id == "t1"
            )),
            "Message 2 should contain result for t1"
        );
    } else {
        panic!("Expected Blocks content with tool_result");
    }

    // Message 3: Assistant with reasoning2 + tool_use2
    assert_eq!(messages[3].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[3].content {
        assert!(
            matches!(&blocks[1], ChatContentBlock::ToolUse { id, .. } if id == "t2"),
            "Should have tool_use t2"
        );
    } else {
        panic!("Expected Blocks content");
    }

    // Message 4: User with tool_result2
    assert_eq!(messages[4].role, "user");

    // Message 5: Final assistant message
    assert_eq!(messages[5].role, "assistant");
}

#[test]
fn test_events_to_messages_parallel_tool_results_do_not_cancel_pending_tools() {
    use std::collections::HashMap;

    use crate::providers::{ChatContentBlock, MessageContent, ReplayToken};

    // Regression: when one tool_result arrives before another from the same
    // assistant tool_use turn, replay must NOT inject a cancelled result for
    // the still-pending tool.
    let events = vec![
        ThreadEvent::user_message("find wife contact"),
        ThreadEvent::reasoning(
            Some("Checking tools".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig1".to_string(),
            }),
        ),
        ThreadEvent::tool_use("t1", "read", json!({"file_path": "a.md"})),
        ThreadEvent::tool_use("t2", "read", json!({"file_path": "b.md"})),
        ThreadEvent::tool_result("t1", json!({"ok": true}), true),
        ThreadEvent::tool_result("t2", json!({"ok": true}), true),
        ThreadEvent::assistant_message("Done."),
    ];

    let messages = thread_events_to_messages(events);

    let mut counts: HashMap<String, usize> = HashMap::new();
    for message in &messages {
        let MessageContent::Blocks(blocks) = &message.content else {
            continue;
        };
        for block in blocks {
            if let ChatContentBlock::ToolResult(result) = block {
                *counts.entry(result.tool_use_id.clone()).or_insert(0) += 1;
                assert!(
                    !result.is_error,
                    "tool result for {} should not be cancelled",
                    result.tool_use_id
                );
            }
        }
    }

    assert_eq!(counts.get("t1"), Some(&1));
    assert_eq!(counts.get("t2"), Some(&1));
}

#[test]
fn test_thread_reasoning_roundtrip() {
    let _temp = setup_temp_zdx_home();

    let mut thread = Thread::with_id(unique_thread_id("reasoning-roundtrip")).unwrap();
    thread
        .append(&ThreadEvent::user_message("explain"))
        .unwrap();
    thread
        .append(&ThreadEvent::reasoning(
            Some("Deep analysis here...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "signature456".to_string(),
            }),
        ))
        .unwrap();
    thread
        .append(&ThreadEvent::assistant_message("Here's my answer"))
        .unwrap();

    let events = thread.read_events().unwrap();
    // meta + user + reasoning + assistant = 4 events
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], ThreadEvent::Meta { .. }));
    assert!(matches!(events[1], ThreadEvent::Message { ref role, .. } if role == "user"));
    assert!(
        matches!(events[2], ThreadEvent::Reasoning { ref text, ref replay, .. }
            if text == &Some("Deep analysis here...".to_string())
                && replay == &Some(ReplayToken::Anthropic { signature: "signature456".to_string() })
        )
    );
    assert!(matches!(events[3], ThreadEvent::Message { ref role, .. } if role == "assistant"));
}

#[test]
fn test_format_transcript_with_reasoning() {
    let events = vec![
        ThreadEvent::meta_with_root(None),
        ThreadEvent::user_message("explain this"),
        ThreadEvent::reasoning(
            Some("Analyzing the request...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig".to_string(),
            }),
        ),
        ThreadEvent::assistant_message("Here's my explanation."),
    ];

    let transcript = format_transcript(&events);
    assert!(transcript.contains("### Thinking"));
    assert!(transcript.contains("Analyzing the request..."));
}

#[test]
fn test_usage_event_serialization() {
    let usage = ThreadEvent::usage(Usage::new(1000, 500, 2000, 100), None, None, None, None);
    let json = serde_json::to_string(&usage).unwrap();
    assert!(json.contains("\"type\":\"usage\""));
    assert!(json.contains("\"input_tokens\":1000"));
    assert!(json.contains("\"output_tokens\":500"));
    assert!(json.contains("\"cache_read_tokens\":2000"));
    assert!(json.contains("\"cache_write_tokens\":100"));
}

#[test]
fn test_usage_event_deserialization() {
    let json = r#"{"type":"usage","input_tokens":1000,"output_tokens":500,"cache_read_tokens":2000,"cache_write_tokens":100,"ts":"2024-01-01T00:00:00Z"}"#;
    let event: ThreadEvent = serde_json::from_str(json).unwrap();
    match event {
        ThreadEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            ..
        } => {
            assert_eq!(input_tokens, 1000);
            assert_eq!(output_tokens, 500);
            assert_eq!(cache_read_tokens, 2000);
            assert_eq!(cache_write_tokens, 100);
        }
        _ => panic!("Expected Usage event"),
    }
}

#[test]
fn test_extract_usage_from_events() {
    let events = vec![
        ThreadEvent::user_message("hello"),
        ThreadEvent::assistant_message("hi"),
        ThreadEvent::usage(Usage::new(100, 50, 200, 25), None, None, None, None),
        ThreadEvent::user_message("bye"),
        ThreadEvent::assistant_message("goodbye"),
        ThreadEvent::usage(Usage::new(150, 75, 300, 30), None, None, None, None),
    ];

    let (cumulative, latest) = extract_usage_from_thread_events(&events);
    assert_eq!(cumulative, Usage::new(250, 125, 500, 55));
    assert_eq!(latest, Usage::new(150, 75, 300, 30));
}

#[test]
fn test_extract_usage_latest_folds_output_only_tail_fragments() {
    // Latest must keep context tokens and fold the output-only tail rather
    // than collapsing to the final zero-context fragment.
    let events = vec![
        ThreadEvent::usage(Usage::new(2, 3, 250_000, 880), None, None, None, None),
        ThreadEvent::usage(Usage::new(0, 1522, 0, 0), None, None, None, None),
        ThreadEvent::usage(Usage::new(0, 480, 0, 0), None, None, None, None),
    ];

    let (cumulative, latest) = extract_usage_from_thread_events(&events);
    assert_eq!(cumulative, Usage::new(2, 2005, 250_000, 880));
    assert_eq!(latest, Usage::new(2, 2005, 250_000, 880));
    assert_eq!(latest.context_input(), 250_882);
}

#[test]
fn test_extract_usage_from_events_empty() {
    let events = vec![
        ThreadEvent::user_message("hello"),
        ThreadEvent::assistant_message("hi"),
    ];

    let (cumulative, latest) = extract_usage_from_thread_events(&events);
    assert_eq!(cumulative, Usage::default());
    assert_eq!(latest, Usage::default());
}

#[test]
fn test_thread_usage_roundtrip() {
    let _temp = setup_temp_zdx_home();

    let mut thread = Thread::with_id(unique_thread_id("usage-roundtrip")).unwrap();
    thread.append(&ThreadEvent::user_message("hello")).unwrap();
    thread
        .append(&ThreadEvent::assistant_message("hi"))
        .unwrap();
    thread
        .append(&ThreadEvent::usage(
            Usage::new(1000, 500, 2000, 100),
            None,
            None,
            None,
            None,
        ))
        .unwrap();

    let events = thread.read_events().unwrap();
    // meta + user + assistant + usage = 4 events
    assert_eq!(events.len(), 4);

    let (cumulative, latest) = extract_usage_from_thread_events(&events);
    // Single event: cumulative = latest
    assert_eq!(cumulative, Usage::new(1000, 500, 2000, 100));
    assert_eq!(latest, Usage::new(1000, 500, 2000, 100));
}

#[tokio::test]
async fn test_persist_task_records_latency_on_terminal_usage() {
    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("usage-latency")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    // Interim usage (input only) carries no latency.
    tx.send(Arc::new(AgentEvent::UsageUpdate {
        input_tokens: 100,
        output_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        model: "m".to_string(),
        provider: "p".to_string(),
        duration_ms: None,
        ttft_ms: None,
    }))
    .unwrap();
    // Terminal usage (output) carries per-request latency.
    tx.send(Arc::new(AgentEvent::UsageUpdate {
        input_tokens: 0,
        output_tokens: 50,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        model: "m".to_string(),
        provider: "p".to_string(),
        duration_ms: Some(1234),
        ttft_ms: Some(56),
    }))
    .unwrap();
    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: "done".to_string(),
        messages: Vec::new(),
        prior_message_count: 0,
    }))
    .unwrap();
    drop(tx);

    persist_handle.await.unwrap();

    // Reload from on-disk JSONL: the merged usage event preserves latency
    // through a serde round-trip.
    let events = thread.read_events().unwrap();
    let usage = events
        .iter()
        .find_map(|e| match e {
            ThreadEvent::Usage {
                input_tokens,
                output_tokens,
                duration_ms,
                ttft_ms,
                ..
            } => Some((*input_tokens, *output_tokens, *duration_ms, *ttft_ms)),
            _ => None,
        })
        .expect("expected a persisted usage event");
    assert_eq!(usage, (100, 50, Some(1234), Some(56)));
}

#[tokio::test]
async fn test_persist_task_saves_completed_usage_from_agent_events() {
    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("persist-complete-usage")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    tx.send(Arc::new(AgentEvent::UsageUpdate {
        input_tokens: 100,
        output_tokens: 0,
        cache_read_input_tokens: 20,
        cache_creation_input_tokens: 5,
        model: String::new(),
        provider: String::new(),
        duration_ms: None,
        ttft_ms: None,
    }))
    .unwrap();
    tx.send(Arc::new(AgentEvent::UsageUpdate {
        input_tokens: 0,
        output_tokens: 50,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        model: String::new(),
        provider: String::new(),
        duration_ms: None,
        ttft_ms: None,
    }))
    .unwrap();
    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: "done".to_string(),
        messages: Vec::new(),
        prior_message_count: 0,
    }))
    .unwrap();
    drop(tx);

    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let (cumulative, latest) = extract_usage_from_thread_events(&events);
    assert_eq!(cumulative, Usage::new(100, 50, 20, 5));
    assert_eq!(latest, Usage::new(100, 50, 20, 5));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ThreadEvent::Usage { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn test_persist_task_records_model_and_provider_on_usage() {
    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("persist-usage-attribution")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    tx.send(Arc::new(AgentEvent::UsageUpdate {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 20,
        cache_creation_input_tokens: 5,
        model: "claude-opus-4-6".to_string(),
        provider: "claude-cli".to_string(),
        duration_ms: None,
        ttft_ms: None,
    }))
    .unwrap();
    drop(tx);

    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let usage = events
        .iter()
        .find_map(|e| match e {
            ThreadEvent::Usage {
                model, provider, ..
            } => Some((model.clone(), provider.clone())),
            _ => None,
        })
        .expect("expected a usage event");
    assert_eq!(usage.0.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(usage.1.as_deref(), Some("claude-cli"));
}

#[tokio::test]
async fn test_persist_task_flushes_partial_usage_on_interrupted_turn() {
    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("persist-interrupted-usage")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    tx.send(Arc::new(AgentEvent::UsageUpdate {
        input_tokens: 500,
        output_tokens: 0,
        cache_read_input_tokens: 100,
        cache_creation_input_tokens: 25,
        model: String::new(),
        provider: String::new(),
        duration_ms: None,
        ttft_ms: None,
    }))
    .unwrap();
    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Interrupted,
        final_text: "partial".to_string(),
        messages: vec![crate::providers::ChatMessage::assistant_text(
            "partial",
            Some("commentary".to_string()),
        )],
        prior_message_count: 0,
    }))
    .unwrap();
    drop(tx);

    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let (cumulative, latest) = extract_usage_from_thread_events(&events);
    assert_eq!(cumulative, Usage::new(500, 0, 100, 25));
    assert_eq!(latest, Usage::new(500, 0, 100, 25));
    // Partial assistant text is flushed via `flush_messages` from the
    // snapshot, not embedded in the `Interrupted` event itself. The
    // `Interrupted` event is emitted as a marker; the partial text
    // appears as an `assistant` `Message` event with
    // `phase: Some("commentary")`.
    assert!(matches!(
        events.last(),
        Some(ThreadEvent::Interrupted { .. })
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        ThreadEvent::Message {
            role,
            text,
            phase: Some(phase),
            ..
        } if role == "assistant" && text == "partial" && phase == "commentary"
    )));
}

#[tokio::test]
async fn test_persist_task_flushes_pending_usage_on_channel_close() {
    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("persist-close-usage")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    tx.send(Arc::new(AgentEvent::UsageUpdate {
        input_tokens: 321,
        output_tokens: 0,
        cache_read_input_tokens: 45,
        cache_creation_input_tokens: 6,
        model: String::new(),
        provider: String::new(),
        duration_ms: None,
        ttft_ms: None,
    }))
    .unwrap();
    drop(tx);

    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let (cumulative, latest) = extract_usage_from_thread_events(&events);
    assert_eq!(cumulative, Usage::new(321, 0, 45, 6));
    assert_eq!(latest, Usage::new(321, 0, 45, 6));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ThreadEvent::Usage { .. }))
            .count(),
        1
    );
}

// ---------------------------------------------------------------------
// Persistence: checkpoint-batched message flush
// ---------------------------------------------------------------------

/// `TurnFinished` with `[reasoning, text, tool_use, text, tool_use]`
/// ordered blocks → JSONL on disk → rehydrate → same order.
#[tokio::test]
async fn test_persistence_round_trip_preserves_order() {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent, ReasoningBlock};

    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("rt-order")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    let assistant = ChatMessage {
        role: "assistant".to_string(),
        phase: None,
        content: MessageContent::Blocks(vec![
            ChatContentBlock::Reasoning(ReasoningBlock {
                text: Some("first thoughts".to_string()),
                replay: None,
            }),
            ChatContentBlock::Text {
                text: "first text".to_string(),
                replay: None,
            },
            ChatContentBlock::tool_use("t1", "bash", json!({"command": "ls"})),
            ChatContentBlock::Text {
                text: "second text".to_string(),
                replay: None,
            },
            ChatContentBlock::tool_use("t2", "bash", json!({"command": "pwd"})),
        ]),
    };

    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: "second text".to_string(),
        messages: vec![assistant],
        prior_message_count: 0,
    }))
    .unwrap();
    drop(tx);
    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    // Skip the meta event and inspect the assistant-side events in order.
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ThreadEvent::Reasoning { .. } => Some("reasoning"),
            ThreadEvent::Message { role, .. } if role == "assistant" => Some("text"),
            ThreadEvent::ToolUse { .. } => Some("tool_use"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["reasoning", "text", "tool_use", "text", "tool_use"],
        "events on disk: {events:#?}"
    );
}

/// Per-part `replay` and `id_origin` survive a JSONL round-trip.
#[tokio::test]
async fn test_persistence_carries_per_part_signatures_and_id_origin() {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent, ReplayToken};

    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("rt-sig")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    let assistant = ChatMessage {
        role: "assistant".to_string(),
        phase: None,
        content: MessageContent::Blocks(vec![
            ChatContentBlock::Text {
                text: "answer".to_string(),
                replay: Some(ReplayToken::Gemini {
                    signature: "txt-sig".to_string(),
                    model: "gemini-3-pro-preview".to_string(),
                }),
            },
            ChatContentBlock::ToolUse {
                id: "real-id".to_string(),
                name: "bash".to_string(),
                input: json!({"command": "ls"}),
                id_origin: zdx_types::IdOrigin::Real,
                replay: Some(ReplayToken::Gemini {
                    signature: "tu-sig".to_string(),
                    model: "gemini-3-pro-preview".to_string(),
                }),
            },
        ]),
    };

    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: "answer".to_string(),
        messages: vec![assistant],
        prior_message_count: 0,
    }))
    .unwrap();
    drop(tx);
    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let mut text_replay = None;
    let mut tool_replay = None;
    let mut tool_origin = None;
    for e in &events {
        match e {
            ThreadEvent::Message {
                role, text, replay, ..
            } if role == "assistant" && text == "answer" => {
                text_replay = replay.clone();
            }
            ThreadEvent::ToolUse {
                id_origin, replay, ..
            } => {
                tool_replay = replay.clone();
                tool_origin = Some(*id_origin);
            }
            _ => {}
        }
    }
    assert_eq!(
        text_replay,
        Some(ReplayToken::Gemini {
            signature: "txt-sig".to_string(),
            model: "gemini-3-pro-preview".to_string(),
        })
    );
    assert_eq!(
        tool_replay,
        Some(ReplayToken::Gemini {
            signature: "tu-sig".to_string(),
            model: "gemini-3-pro-preview".to_string(),
        })
    );
    assert_eq!(tool_origin, Some(zdx_types::IdOrigin::Real));
}

/// Old transcripts (no `id_origin`/`replay` on `tool_use`) load with
/// `id_origin: Synthesized` and `replay: None`.
#[test]
fn test_old_transcript_loads_with_synthesized_default() {
    let line = r#"{"type":"tool_use","id":"abc","name":"bash","input":{"command":"ls"},"ts":"2024-01-01T00:00:00Z"}"#;
    let event: ThreadEvent = serde_json::from_str(line).unwrap();
    match event {
        ThreadEvent::ToolUse {
            id_origin, replay, ..
        } => {
            assert_eq!(id_origin, zdx_types::IdOrigin::Synthesized);
            assert_eq!(replay, None);
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

/// Streaming `ToolInputCompleted` followed by `TurnFinished` produces
/// exactly one `ThreadEvent::ToolUse` (from the batched flush), not two.
#[tokio::test]
async fn test_streaming_events_no_longer_persisted() {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};

    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("no-stream-persist")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    // Streaming ToolInputCompleted should NOT be persisted any more.
    tx.send(Arc::new(AgentEvent::ToolInputCompleted {
        id: "t1".to_string(),
        name: "bash".to_string(),
        input: json!({"command": "ls"}),
    }))
    .unwrap();

    // ReasoningCompleted should NOT be persisted either.
    tx.send(Arc::new(AgentEvent::ReasoningCompleted {
        block: crate::providers::ReasoningBlock {
            text: Some("thinking".to_string()),
            replay: None,
        },
    }))
    .unwrap();

    let assistant = ChatMessage {
        role: "assistant".to_string(),
        phase: None,
        content: MessageContent::Blocks(vec![ChatContentBlock::tool_use(
            "t1",
            "bash",
            json!({"command": "ls"}),
        )]),
    };

    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: String::new(),
        messages: vec![assistant],
        prior_message_count: 0,
    }))
    .unwrap();
    drop(tx);
    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let tool_use_count = events
        .iter()
        .filter(|e| matches!(e, ThreadEvent::ToolUse { .. }))
        .count();
    let reasoning_count = events
        .iter()
        .filter(|e| matches!(e, ThreadEvent::Reasoning { .. }))
        .count();
    assert_eq!(
        tool_use_count, 1,
        "expected exactly one ToolUse from batched flush, got {tool_use_count}: {events:#?}"
    );
    assert_eq!(
        reasoning_count, 0,
        "ReasoningCompleted should not be persisted as a streaming event"
    );
}

/// `TurnFinished` containing `assistant→tool_use→user→tool_result` produces
/// `ThreadEvent`s in that order.
#[tokio::test]
async fn test_tool_result_persisted_with_tool_use_in_order() {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};
    use crate::tools::{ToolResult, ToolResultContent};

    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("tool-order")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    let assistant = ChatMessage {
        role: "assistant".to_string(),
        phase: None,
        content: MessageContent::Blocks(vec![ChatContentBlock::tool_use(
            "t1",
            "bash",
            json!({"command": "ls"}),
        )]),
    };
    let tool_results = ChatMessage::tool_results(vec![ToolResult {
        tool_use_id: "t1".to_string(),
        content: ToolResultContent::Text(r#"{"ok":true,"data":"file.txt"}"#.to_string()),
        is_error: false,
    }]);

    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: String::new(),
        messages: vec![assistant, tool_results],
        prior_message_count: 0,
    }))
    .unwrap();
    drop(tx);
    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let positions: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ThreadEvent::ToolUse { .. } => Some("tool_use"),
            ThreadEvent::ToolResult { .. } => Some("tool_result"),
            _ => None,
        })
        .collect();
    assert_eq!(
        positions,
        vec!["tool_use", "tool_result"],
        "events: {events:#?}"
    );
}

/// `messages_to_events` (the public ChatMessage→ThreadEvent converter
/// used by external callers + the Gemini golden test) and the live
/// `flush_messages` write path must produce identical event sequences
/// for the same input. Specifically, intermixed user `Text + ToolResult`
/// blocks must NOT coalesce text into a single joined message — that
/// drops per-text `replay` and reorders against the on-disk schema.
#[test]
fn test_messages_to_events_matches_flush_messages_for_user_blocks() {
    let messages = get_test_messages();

    // Direct path used by external callers (bot/handoff/golden helper).
    let via_messages_to_events = messages_to_events(&messages);

    // Same input run through the live write path's flush helper. The
    // two MUST produce identical event sequences modulo timestamps.
    let mut via_flush = Vec::new();
    let mut persistor = UsagePersistor::new();
    persistor.flush_messages(&messages, 0, &mut via_flush);

    assert_eq!(
        via_messages_to_events.len(),
        via_flush.len(),
        "event count mismatch: m2e={via_messages_to_events:#?}\nflush={via_flush:#?}",
    );

    for (a, b) in via_messages_to_events.iter().zip(via_flush.iter()) {
        // Compare structurally, ignoring `ts` (clock-based, will differ).
        assert_eq!(
            strip_ts(a),
            strip_ts(b),
            "event payload mismatch:\nm2e={a:#?}\nflush={b:#?}"
        );
    } // Spot-check that the user-side per-text `replay` survived the
    // conversion (the regression the old coalescing code introduced).
    let user_msgs: Vec<&ThreadEvent> = via_messages_to_events
        .iter()
        .filter(|e| matches!(e, ThreadEvent::Message { role, .. } if role == "user"))
        .collect();
    assert_eq!(user_msgs.len(), 2, "expected two separate user text events");
    if let ThreadEvent::Message {
        text,
        replay: Some(ReplayToken::Gemini { signature, .. }),
        ..
    } = user_msgs[0]
    {
        assert_eq!(text, "before");
        assert_eq!(signature, "DDDDDDDDDDDDDDDD");
    } else {
        panic!(
            "first user text lost its replay metadata: {:#?}",
            user_msgs[0]
        );
    }
    if let ThreadEvent::Message {
        text,
        replay: Some(ReplayToken::Gemini { signature, .. }),
        ..
    } = user_msgs[1]
    {
        assert_eq!(text, "after");
        assert_eq!(signature, "EEEEEEEEEEEEEEEE");
    } else {
        panic!(
            "second user text lost its replay metadata: {:#?}",
            user_msgs[1]
        );
    }
}

fn get_test_messages() -> Vec<crate::providers::ChatMessage> {
    use crate::providers::{
        ChatContentBlock, ChatMessage, MessageContent, ReasoningBlock, ReplayToken,
    };
    use crate::tools::{ToolResult, ToolResultContent};

    vec![
        // Assistant turn: reasoning + per-part text + real-id tool_use.
        ChatMessage {
            role: "assistant".to_string(),
            phase: None,
            content: MessageContent::Blocks(vec![
                ChatContentBlock::Reasoning(ReasoningBlock {
                    text: Some("planning".to_string()),
                    replay: Some(ReplayToken::Gemini {
                        signature: "AAAAAAAAAAAAAAAA".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    }),
                }),
                ChatContentBlock::Text {
                    text: "first".to_string(),
                    replay: Some(ReplayToken::Gemini {
                        signature: "BBBBBBBBBBBBBBBB".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    }),
                },
                ChatContentBlock::ToolUse {
                    id: "call_real_001".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "README.md"}),
                    id_origin: zdx_types::IdOrigin::Real,
                    replay: Some(ReplayToken::Gemini {
                        signature: "CCCCCCCCCCCCCCCC".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    }),
                },
            ]),
        },
        // User turn with intermixed text + tool_result + text — the
        // case the old `messages_to_events` coalesced into a single
        // joined `user_message`, dropping the second text's `replay`
        // and the per-text ordering.
        ChatMessage {
            role: "user".to_string(),
            phase: None,
            content: MessageContent::Blocks(vec![
                ChatContentBlock::Text {
                    text: "before".to_string(),
                    replay: Some(ReplayToken::Gemini {
                        signature: "DDDDDDDDDDDDDDDD".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    }),
                },
                ChatContentBlock::ToolResult(ToolResult {
                    tool_use_id: "call_real_001".to_string(),
                    content: ToolResultContent::Text("\"# zdx\"".to_string()),
                    is_error: false,
                }),
                ChatContentBlock::Text {
                    text: "after".to_string(),
                    replay: Some(ReplayToken::Gemini {
                        signature: "EEEEEEEEEEEEEEEE".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    }),
                },
            ]),
        },
    ]
}

/// Returns a `serde_json::Value` representation of `event` with the
/// `ts` field cleared so two events recorded at slightly different
/// clock instants compare equal by payload alone.
fn strip_ts(event: &ThreadEvent) -> serde_json::Value {
    let mut value = serde_json::to_value(event).expect("serialize event");
    if let Some(map) = value.as_object_mut() {
        map.remove("ts");
    }
    value
}

/// `TurnCheckpoint` flushes messages 0..3, then `TurnFinished` with
/// messages 0..5 flushes only 3..5 — no re-write of 0..3. Both events
/// carry the same `prior_message_count` (run-entry cursor).
#[tokio::test]
async fn test_checkpoint_then_turn_finished_idempotent() {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};
    use crate::tools::{ToolResult, ToolResultContent};

    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("checkpoint-idempotent")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    let user_msg = ChatMessage::user("do two things");
    let assistant_t1 = ChatMessage {
        role: "assistant".to_string(),
        phase: None,
        content: MessageContent::Blocks(vec![ChatContentBlock::tool_use(
            "t1",
            "bash",
            json!({"command": "echo one"}),
        )]),
    };
    let tool_result_t1 = ChatMessage::tool_results(vec![ToolResult {
        tool_use_id: "t1".to_string(),
        content: ToolResultContent::Text(r#"{"ok":true,"data":"one"}"#.to_string()),
        is_error: false,
    }]);

    // Caller appended user_msg directly before kicking off the engine, so
    // `prior_message_count` is 1 (covers user_msg). The engine then
    // appended assistant_t1 + tool_result_t1, giving 3 total messages.
    tx.send(Arc::new(AgentEvent::TurnCheckpoint {
        messages: vec![
            user_msg.clone(),
            assistant_t1.clone(),
            tool_result_t1.clone(),
        ],
        prior_message_count: 1,
    }))
    .unwrap();

    // Second tool turn:
    let assistant_t2 = ChatMessage {
        role: "assistant".to_string(),
        phase: None,
        content: MessageContent::Blocks(vec![ChatContentBlock::tool_use(
            "t2",
            "bash",
            json!({"command": "echo two"}),
        )]),
    };
    let tool_result_t2 = ChatMessage::tool_results(vec![ToolResult {
        tool_use_id: "t2".to_string(),
        content: ToolResultContent::Text(r#"{"ok":true,"data":"two"}"#.to_string()),
        is_error: false,
    }]);

    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: String::new(),
        messages: vec![
            user_msg,
            assistant_t1,
            tool_result_t1,
            assistant_t2,
            tool_result_t2,
        ],
        prior_message_count: 1,
    }))
    .unwrap();
    drop(tx);
    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let tool_use_ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ThreadEvent::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let tool_result_ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ThreadEvent::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_use_ids,
        vec!["t1", "t2"],
        "each tool_use should appear exactly once: {events:#?}"
    );
    assert_eq!(
        tool_result_ids,
        vec!["t1", "t2"],
        "each tool_result should appear exactly once: {events:#?}"
    );
    // The user message at index 0 was already persisted by the caller
    // (prior_message_count=1), so flush_messages must not re-emit it.
    let user_message_count = events
        .iter()
        .filter(|e| matches!(e, ThreadEvent::Message { role, .. } if role == "user"))
        .count();
    // Tool-result messages have role="user" but `Blocks` content; only
    // `MessageContent::Text` user messages serialize as `Message{role:"user"}`.
    assert_eq!(
        user_message_count, 0,
        "caller-persisted user message must not be flushed again: {events:#?}"
    );
}

/// A final interrupted snapshot can be shorter than the most recent
/// checkpoint when the stream is interrupted before a durable final turn
/// is assembled. Persistence should keep the checkpointed messages and
/// append the interruption marker instead of asserting on the shrink.
#[tokio::test]
async fn test_interrupted_turn_finished_allows_shorter_snapshot_than_checkpoint() {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};

    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("checkpoint-interrupted-shrink")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    let user_msg = ChatMessage::user("start streaming");
    let checkpointed_assistant = ChatMessage {
        role: "assistant".to_string(),
        phase: Some("commentary".to_string()),
        content: MessageContent::Blocks(vec![ChatContentBlock::Text {
            text: "partial".to_string(),
            replay: None,
        }]),
    };

    tx.send(Arc::new(AgentEvent::TurnCheckpoint {
        messages: vec![user_msg.clone(), checkpointed_assistant],
        prior_message_count: 1,
    }))
    .unwrap();

    tx.send(Arc::new(AgentEvent::TurnFinished {
        status: TurnStatus::Interrupted,
        final_text: String::new(),
        messages: vec![user_msg],
        prior_message_count: 1,
    }))
    .unwrap();
    drop(tx);
    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let partial_assistant_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            ThreadEvent::Message {
                role,
                text,
                phase: Some(phase),
                ..
            } if role == "assistant" && text == "partial" && phase == "commentary" => Some(index),
            _ => None,
        })
        .collect();
    assert_eq!(
        partial_assistant_indices.len(),
        1,
        "partial assistant message should be persisted exactly once: {events:#?}"
    );
    assert!(matches!(
        events.last(),
        Some(ThreadEvent::Interrupted { .. })
    ));
    assert!(
        partial_assistant_indices[0] < events.len() - 1,
        "partial assistant message should appear before interrupted marker: {events:#?}"
    );
    assert!(!events.iter().any(|e| matches!(
        e,
        ThreadEvent::Message {
            role,
            text,
            ..
        } if role == "user" && text == "start streaming"
    )));
}

/// Crash simulation: emit `TurnCheckpoint` after tool turn 1, drop the
/// consumer (simulating crash), reload from disk; messages from tool
/// turn 1 must be present, in-flight tool turn 2 must not.
#[tokio::test]
async fn test_checkpoint_persistence_survives_crash_simulation() {
    use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};
    use crate::tools::{ToolResult, ToolResultContent};

    let _temp = setup_temp_zdx_home();

    let thread = Thread::with_id(unique_thread_id("checkpoint-crash")).unwrap();
    let (tx, rx) = create_event_channel();
    let persist_handle = spawn_thread_persist_task(thread.clone(), rx);

    let user_msg = ChatMessage::user("two things");
    let assistant_t1 = ChatMessage {
        role: "assistant".to_string(),
        phase: None,
        content: MessageContent::Blocks(vec![ChatContentBlock::tool_use(
            "t1",
            "bash",
            json!({"command": "echo one"}),
        )]),
    };
    let tool_result_t1 = ChatMessage::tool_results(vec![ToolResult {
        tool_use_id: "t1".to_string(),
        content: ToolResultContent::Text(r#"{"ok":true,"data":"one"}"#.to_string()),
        is_error: false,
    }]);

    tx.send(Arc::new(AgentEvent::TurnCheckpoint {
        messages: vec![user_msg, assistant_t1, tool_result_t1],
        prior_message_count: 1,
    }))
    .unwrap();

    // Simulate crash: drop the sender without sending TurnFinished. The
    // in-flight second tool turn (t2) is never persisted.
    drop(tx);
    persist_handle.await.unwrap();

    let events = thread.read_events().unwrap();
    let tool_use_ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ThreadEvent::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_use_ids,
        vec!["t1"],
        "only the completed tool turn should be on disk: {events:#?}"
    );
    let tool_result_ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ThreadEvent::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_result_ids, vec!["t1"]);
}

#[test]
fn test_pending_topic_title_roundtrip() {
    let _temp = setup_temp_zdx_home();

    let thread_id = unique_thread_id("pending-topic-title");
    let mut thread = Thread::with_id(thread_id.clone()).unwrap();

    assert!(!read_thread_pending_topic_title(&thread_id).unwrap());

    thread.set_pending_topic_title(true).unwrap();
    assert!(read_thread_pending_topic_title(&thread_id).unwrap());

    let events = thread.read_events().unwrap();
    assert!(matches!(
        events[0],
        ThreadEvent::Meta {
            pending_topic_title: true,
            ..
        }
    ));

    thread.set_pending_topic_title(false).unwrap();
    assert!(!read_thread_pending_topic_title(&thread_id).unwrap());
}

#[test]
fn test_thread_lineage_roundtrip_and_list_filtering() {
    let _temp = setup_temp_zdx_home();

    // A normal top-level thread.
    let normal_id = unique_thread_id("lineage-normal");
    let mut normal = Thread::with_id(normal_id.clone()).unwrap();
    normal.append(&ThreadEvent::user_message("hi")).unwrap();

    // A tagged subagent child thread.
    let child_id = unique_thread_id("lineage-child");
    let mut child = Thread::with_id(child_id.clone()).unwrap();
    child.set_origin(
        Some("subagent".to_string()),
        Some(normal_id.clone()),
        Some("explorer".to_string()),
    );
    child.append(&ThreadEvent::user_message("do work")).unwrap();

    // Lineage round-trips through the meta line; a normal thread has none.
    let child_meta = read_meta(&threads_dir().join(format!("{child_id}.jsonl")))
        .unwrap()
        .unwrap();
    assert_eq!(child_meta.origin_kind.as_deref(), Some("subagent"));
    assert_eq!(
        child_meta.parent_thread_id.as_deref(),
        Some(normal_id.as_str())
    );
    assert_eq!(child_meta.subagent_name.as_deref(), Some("explorer"));

    let normal_meta = read_meta(&threads_dir().join(format!("{normal_id}.jsonl")))
        .unwrap()
        .unwrap();
    assert!(normal_meta.origin_kind.is_none());

    // Default listing hides the child; the all-listing includes it.
    let default_ids: Vec<String> = list_threads().unwrap().into_iter().map(|s| s.id).collect();
    assert!(default_ids.contains(&normal_id));
    assert!(!default_ids.contains(&child_id));

    let all_ids: Vec<String> = list_all_threads()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert!(all_ids.contains(&normal_id));
    assert!(all_ids.contains(&child_id));
}

#[test]
fn test_usage_struct_operations() {
    let u1 = Usage::new(100, 50, 200, 25);
    let u2 = Usage::new(150, 75, 300, 30);

    // Test add
    let sum = u1 + u2;
    assert_eq!(sum, Usage::new(250, 125, 500, 55));

    // Test add_assign
    let mut u3 = u1;
    u3 += u2;
    assert_eq!(u3, Usage::new(250, 125, 500, 55));

    // Test total
    assert_eq!(u1.total(), 375);

    // Test context_input
    assert_eq!(u1.context_input(), 325);
}

/// Regression test: orphaned `tool_use` (bot crashed mid-execution) followed by
/// user message must produce a cancelled `tool_result` immediately after the
/// assistant `tool_use` block, not at the end of the conversation.
#[test]
fn test_orphaned_tool_use_gets_cancelled_before_user_message() {
    let events = vec![
        ThreadEvent::user_message("do something"),
        ThreadEvent::tool_use("t1", "bash", json!({"command": "sleep 999"})),
        // No tool_result — bot crashed here
        ThreadEvent::user_message("hello again"),
    ];
    let messages = thread_events_to_messages(events);

    // Expected: user, assistant(tool_use), tool_result(cancelled), user
    assert_eq!(messages.len(), 4, "messages: {messages:#?}");
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[2].role, "user"); // tool_results role is "user" in Anthropic API
    assert_eq!(messages[3].role, "user");

    // Verify the tool_result is marked as cancelled
    if let crate::providers::MessageContent::Blocks(blocks) = &messages[2].content {
        match &blocks[0] {
            crate::providers::ChatContentBlock::ToolResult(result) => {
                assert_eq!(result.tool_use_id, "t1");
                assert!(result.is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    } else {
        panic!("expected blocks content for tool_result message");
    }
}

/// Verifies that when a turn is interrupted mid-stream, the partial assistant
/// content (reasoning, tool calls, and streamed text) is preserved in the
/// reconstructed messages so the model sees them on the next request. The
/// production write path emits partial assistant text as a `Message` event
/// with `phase: Some("commentary")` before the `Interrupted` marker.
#[test]
fn test_interrupted_turn_preserves_reasoning_tools_and_partial_text() {
    use crate::providers::{ChatContentBlock, MessageContent};

    let events = vec![
        ThreadEvent::user_message("analyze the codebase"),
        // Reasoning arrived before interruption
        ThreadEvent::reasoning(
            Some("Let me look at the project structure first...".to_string()),
            Some(ReplayToken::Anthropic {
                signature: "sig_abc".to_string(),
            }),
        ),
        // Tool was requested but not completed
        ThreadEvent::tool_use("t1", "bash", json!({"command": "find . -name '*.rs'"})),
        // Partial assistant text streamed before the interrupt — written
        // as a `commentary`-phase Message by the production write path.
        ThreadEvent::Message {
            role: "assistant".to_string(),
            text: "Here are the files I found so far".to_string(),
            phase: Some("commentary".to_string()),
            replay: None,
            ts: chrono_timestamp(),
        },
        // User interrupted here — no tool_result, no final assistant message.
        ThreadEvent::interrupted(),
        // User sends a new message
        ThreadEvent::user_message("actually, just focus on the tests"),
    ];

    let messages = thread_events_to_messages(events);

    // Expected structure:
    // 1. user: "analyze the codebase"
    // 2. assistant: [reasoning, tool_use, partial text]
    // 3. user (tool_results): [cancelled tool_result for t1]
    // 4. user: "actually, just focus on the tests"
    assert_eq!(messages.len(), 4, "messages: {messages:#?}");

    // Message 0: user input
    assert_eq!(messages[0].role, "user");

    // Message 1: assistant with reasoning + tool_use + partial text
    assert_eq!(messages[1].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[1].content {
        assert!(
            blocks.len() >= 2,
            "Expected reasoning + tool_use + text blocks, got: {blocks:#?}"
        );
        // Should have reasoning block
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, ChatContentBlock::Reasoning(..))),
            "Missing reasoning block in interrupted assistant message"
        );
        // Should have tool_use block
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, ChatContentBlock::ToolUse { .. })),
            "Missing tool_use block in interrupted assistant message"
        );
        // Should have partial text
        assert!(
                blocks
                    .iter()
                    .any(|b| matches!(b, ChatContentBlock::Text { text: t, .. } if t.contains("files I found"))),
                "Missing partial text in interrupted assistant message"
            );
    } else {
        panic!("Expected blocks content for interrupted assistant message");
    }

    // Message 2: cancelled tool result
    assert_eq!(messages[2].role, "user"); // tool_results use "user" role
    if let MessageContent::Blocks(blocks) = &messages[2].content {
        match &blocks[0] {
            ChatContentBlock::ToolResult(result) => {
                assert_eq!(result.tool_use_id, "t1");
                assert!(
                    result.is_error,
                    "Cancelled tool result should be marked as error"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    } else {
        panic!("expected blocks content for tool_result message");
    }

    // Message 3: new user message
    assert_eq!(messages[3].role, "user");
}

/// Verifies that an interrupted turn with NO partial content still preserves
/// reasoning and tool blocks (e.g., interrupt during API request after tools).
#[test]
fn test_interrupted_turn_without_partial_text_preserves_tools() {
    use crate::providers::{ChatContentBlock, MessageContent};

    let events = vec![
        ThreadEvent::user_message("do something"),
        ThreadEvent::tool_use("t1", "read", json!({"file_path": "src/main.rs"})),
        ThreadEvent::tool_result("t1", json!({"ok": true, "data": "fn main() {}"}), true),
        // Second tool requested but user interrupted before completion
        ThreadEvent::tool_use("t2", "bash", json!({"command": "cargo test"})),
        ThreadEvent::interrupted(),
        ThreadEvent::user_message("stop, let me rethink"),
    ];

    let messages = thread_events_to_messages(events);

    // Expected: user, assistant(t1+t2), tool_results(t1), assistant(t2 only), cancelled(t2), user
    // Actually let me trace through MessageReplay:
    // - user_message → messages.push(user)
    // - tool_use t1 → open_tool_uses=[t1], pending_assistant_blocks=[(t1,...)]
    // - tool_result t1 → removes t1 from open, adds to pending_results, flushes assistant blocks (t1 tool_use), then flushes tool_results
    // - tool_use t2 → open_tool_uses=[t2], pending_assistant_blocks=[(t2,...)]
    // - interrupted(None) → flush_tool_results (empty), drain pending_assistant_blocks → [t2 tool_use], push assistant, cancel open t2
    // - user_message → push user
    //
    // Result: user, assistant(tool_use t1), user(tool_result t1), assistant(tool_use t2), user(cancelled t2), user
    assert_eq!(messages.len(), 6, "messages: {messages:#?}");

    // The interrupted assistant turn should have the t2 tool_use
    assert_eq!(messages[3].role, "assistant");
    if let MessageContent::Blocks(blocks) = &messages[3].content {
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, ChatContentBlock::ToolUse { name, .. } if name == "bash")),
            "Missing tool_use for t2 in interrupted assistant message"
        );
    } else {
        panic!("Expected blocks for interrupted assistant");
    }

    // Cancelled tool result for t2
    assert_eq!(messages[4].role, "user");
    if let MessageContent::Blocks(blocks) = &messages[4].content {
        match &blocks[0] {
            ChatContentBlock::ToolResult(result) => {
                assert_eq!(result.tool_use_id, "t2");
                assert!(result.is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    } else {
        panic!("expected blocks for cancelled tool result");
    }

    // New user message
    assert_eq!(messages[5].role, "user");
}

#[test]
fn test_search_threads_excludes_specified_thread_id() {
    let _temp = setup_temp_zdx_home();

    let current_id = unique_thread_id("excl-current");
    let other_id = unique_thread_id("excl-other");
    // Use the thread IDs as part of the query so results are scoped to
    // these two threads only, avoiding interference from other test threads.
    let marker = format!("excl-marker-{current_id}");

    let mut other = Thread::with_id(other_id.clone()).unwrap();
    other.append(&ThreadEvent::user_message(&marker)).unwrap();

    // Create "current" last so it is newest and first in list_threads().
    let mut current = Thread::with_id(current_id.clone()).unwrap();
    current.append(&ThreadEvent::user_message(&marker)).unwrap();

    let options = ThreadSearchOptions {
        query: Some(marker),
        exclude_thread_id: Some(current_id.clone()),
        limit: 20,
        ..ThreadSearchOptions::default()
    };

    let results = search_threads(&options).unwrap();
    let ids: Vec<&str> = results.iter().map(|r| r.thread_id.as_str()).collect();

    assert!(
        !ids.contains(&current_id.as_str()),
        "current thread must be excluded"
    );
    assert!(
        ids.contains(&other_id.as_str()),
        "other thread must be present"
    );
    assert_eq!(ids.len(), 1, "exactly one result expected");
}

#[test]
fn test_search_threads_exclusion_does_not_underfill_limit() {
    let _temp = setup_temp_zdx_home();

    let a_id = unique_thread_id("underfill-a");
    let b_id = unique_thread_id("underfill-b");
    // Create a and b first so current_id ends up newest (first candidate).
    let current_id = unique_thread_id("underfill-current");
    let marker = format!("underfill-marker-{current_id}");

    for (id, msg) in [
        (&a_id, marker.as_str()),
        (&b_id, marker.as_str()),
        // current_id is created last → newest → first candidate hit by
        // list_threads(). A buggy post-filter would stop at limit=2 before
        // reaching a_id and b_id, then strip current_id, returning only 1.
        (&current_id, marker.as_str()),
    ] {
        let mut t = Thread::with_id(id.clone()).unwrap();
        t.append(&ThreadEvent::user_message(msg)).unwrap();
    }

    // limit=2 with current excluded must still yield 2 results.
    let options = ThreadSearchOptions {
        query: Some(marker),
        exclude_thread_id: Some(current_id.clone()),
        limit: 2,
        ..ThreadSearchOptions::default()
    };

    let results = search_threads(&options).unwrap();
    assert_eq!(
        results.len(),
        2,
        "limit must be filled even when the excluded thread is the newest candidate"
    );
    assert!(
        results.iter().all(|r| r.thread_id != current_id),
        "current thread must not appear in results"
    );
}
