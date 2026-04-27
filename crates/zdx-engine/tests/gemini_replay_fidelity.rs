//! Gemini implicit prompt caching fidelity — golden test.
//!
//! Locks in the contract that the SSE parser → engine builder → persistence
//! → request builder pipeline must satisfy for Gemini's implicit prompt
//! cache to actually hit on subsequent turns. The pipeline helper lives
//! inline in this file (using only public engine APIs) to avoid exposing
//! test-only glue on the production surface.
//!
//! See `docs/plans/done/gemini-implicit-caching-fidelity.md`.

use std::path::PathBuf;
use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use serde_json::Value;
use zdx_engine::core::agent::{AssistantTurnBuilder, ThinkingBuilder, ToolUseBuilder};
use zdx_engine::core::thread_persistence::{messages_to_events, thread_events_to_messages};
use zdx_providers::gemini::shared::build_contents;
use zdx_providers::gemini::sse::GeminiSseParser;
use zdx_providers::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ReplayToken,
    SignatureProvider, StreamEvent,
};
use zdx_types::IdOrigin;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("gemini")
}

fn load_sse_bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join(name)).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

fn load_expected_json(name: &str) -> Value {
    let bytes = std::fs::read(fixtures_dir().join(name))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse fixture {name}: {e}"))
}

fn make_byte_stream(
    bytes: Vec<u8>,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    Box::pin(stream::iter(vec![Ok(Bytes::from(bytes))]))
}

async fn collect_stream_events(sse_bytes: Vec<u8>, model: &str) -> Vec<StreamEvent> {
    let byte_stream = make_byte_stream(sse_bytes);
    let parser = GeminiSseParser::new(byte_stream, model.to_string(), "gemini");
    let mut events = Vec::new();
    let mut stream = Box::pin(parser);
    while let Some(item) = stream.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(e) => panic!("parser error: {e:?}"),
        }
    }
    events
}

/// Mirrors the production `handle_stream_event` dispatch but ignores the
/// `EventSender` side effects — we only care about the final `parts` order
/// and per-part metadata after `finalize()`. Kept inline (not a public
/// engine API) because this is purely test scaffolding.
fn apply_stream_event(turn: &mut AssistantTurnBuilder, event: StreamEvent) {
    match event {
        StreamEvent::ContentBlockStart {
            index,
            block_type,
            id,
            name,
            id_origin,
            data,
        } => apply_content_block_start(turn, index, block_type, id, name, id_origin, data),
        StreamEvent::TextDelta { index, text } => {
            let part = turn.ensure_text_part_mut(index);
            part.text.push_str(&text);
        }
        StreamEvent::InputJsonDelta {
            index,
            partial_json,
        } => {
            if let Some(tu) = turn.find_tool_use_mut(index) {
                tu.input_json.push_str(&partial_json);
            }
        }
        StreamEvent::ReasoningDelta { index, reasoning } => {
            if let Some(tb) = turn.find_thinking_mut(index) {
                if !reasoning.is_empty() {
                    tb.had_delta = true;
                }
                tb.text.push_str(&reasoning);
            }
        }
        StreamEvent::ReasoningSignatureDelta {
            index,
            signature,
            provider,
        } => {
            if let Some(tb) = turn.find_thinking_mut(index) {
                tb.signature.push_str(&signature);
                tb.signature_provider = Some(provider);
            }
        }
        StreamEvent::ContentBlockCompleted { index, signature } => {
            apply_content_block_completed(turn, index, signature);
        }
        // Lifecycle / unrelated events: no replay-relevant state to capture.
        StreamEvent::MessageStart { .. }
        | StreamEvent::MessageDelta { .. }
        | StreamEvent::MessageCompleted
        | StreamEvent::Ping
        | StreamEvent::Ignored { .. }
        | StreamEvent::ReasoningCompleted { .. }
        | StreamEvent::Error { .. } => {}
    }
}

fn apply_content_block_start(
    turn: &mut AssistantTurnBuilder,
    index: usize,
    block_type: ContentBlockType,
    id: Option<String>,
    name: Option<String>,
    id_origin: Option<IdOrigin>,
    data: Option<String>,
) {
    match block_type {
        ContentBlockType::ToolUse => {
            turn.push_tool_use(ToolUseBuilder {
                index,
                id: id.unwrap_or_default(),
                name: name.unwrap_or_default().to_ascii_lowercase(),
                input_json: String::new(),
                input_preview_len: 0,
                id_origin: id_origin.unwrap_or_default(),
                replay: None,
            });
        }
        ContentBlockType::Text => {
            let _ = turn.ensure_text_part_mut(index);
        }
        ContentBlockType::Reasoning => {
            turn.push_reasoning(ThinkingBuilder {
                index,
                text: String::new(),
                signature: String::new(),
                signature_provider: None,
                replay: None,
                had_delta: false,
            });
        }
        ContentBlockType::RedactedThinking => {
            let data = data.unwrap_or_default();
            turn.push_reasoning(ThinkingBuilder {
                index,
                text: String::new(),
                signature: String::new(),
                signature_provider: None,
                replay: Some(ReplayToken::AnthropicRedacted { data }),
                had_delta: false,
            });
        }
    }
}

fn apply_content_block_completed(
    turn: &mut AssistantTurnBuilder,
    index: usize,
    signature: Option<(SignatureProvider, String)>,
) {
    let model_for_promotion = turn.model.clone();
    // Promote a pending reasoning signature into a `ReplayToken` so
    // the round-tripped reasoning block carries it on disk too.
    if let Some(tb) = turn.find_thinking_mut(index)
        && !tb.signature.is_empty()
        && tb.replay.is_none()
    {
        let token = match tb.signature_provider {
            Some(SignatureProvider::Gemini) => ReplayToken::Gemini {
                signature: std::mem::take(&mut tb.signature),
                model: model_for_promotion,
            },
            Some(SignatureProvider::Anthropic) | None => ReplayToken::Anthropic {
                signature: std::mem::take(&mut tb.signature),
            },
        };
        tb.replay = Some(token);
    }
    // Per-part Gemini signatures (text / tool_use) ride this channel.
    if let Some((sig_provider, sig)) = signature {
        let model = turn.model.clone();
        let token = match sig_provider {
            SignatureProvider::Gemini => ReplayToken::Gemini {
                signature: sig,
                model,
            },
            SignatureProvider::Anthropic => ReplayToken::Anthropic { signature: sig },
        };
        if let Some(text_part) = turn.find_text_mut(index) {
            text_part.replay = Some(token);
        } else if let Some(tool_part) = turn.find_tool_use_mut(index) {
            tool_part.replay = Some(token);
        }
    }
}

/// Drives Gemini SSE bytes through SSE parser → engine builder → persistence
/// round-trip → `build_contents`, returning the assistant `Content` JSON
/// that would be sent on the next request. This is the pipeline closure
/// that locks in implicit-cache fidelity.
async fn run_gemini_test_pipeline(sse_bytes: Vec<u8>, model: &str) -> Value {
    let events = collect_stream_events(sse_bytes, model).await;
    let mut turn = AssistantTurnBuilder::new(model.to_string());
    for event in events {
        apply_stream_event(&mut turn, event);
    }
    let finalized = turn.finalize();
    let assistant = ChatMessage::assistant_blocks(finalized.blocks);

    // Round-trip through persistence (gate 2.4 contract).
    let thread_events = messages_to_events(&[assistant]);
    let rehydrated = thread_events_to_messages(thread_events);

    let contents = build_contents(&rehydrated, model);
    contents
        .into_iter()
        .next()
        .expect("pipeline must produce at least one content")
}

/// Gate 2.2 contract: the SSE parser must emit one `ContentBlockStart` per
/// Gemini part, in original order, with per-part signatures.
///
/// Fixture has 5 parts: `[thought, text, functionCall_real, text, functionCall_synth]`.
#[tokio::test]
async fn test_sse_parser_per_part_fidelity() {
    use ContentBlockType::{Reasoning, Text, ToolUse};

    let bytes = load_sse_bytes("multipart_turn.sse");
    let events = collect_stream_events(bytes, "gemini-3-pro-preview").await;

    let block_starts: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ContentBlockStart { .. }))
        .collect();

    let block_types: Vec<ContentBlockType> = block_starts
        .iter()
        .map(|e| {
            if let StreamEvent::ContentBlockStart { block_type, .. } = e {
                *block_type
            } else {
                unreachable!()
            }
        })
        .collect();

    assert_eq!(
        block_starts.len(),
        5,
        "expected 5 per-part ContentBlockStart events (one thought, two text, two functionCall); \
         got {} — parts are being merged by category. block_types={:?}",
        block_starts.len(),
        block_types
    );

    assert_eq!(
        block_types,
        vec![Reasoning, Text, ToolUse, Text, ToolUse],
        "block order must match original part order from the stream; \
         current parser groups by category instead of preserving stream order"
    );

    // Both functionCall parts must produce ToolUse blocks with the right ids.
    let tool_use_starts: Vec<(Option<&str>, Option<&str>)> = block_starts
        .iter()
        .filter_map(|e| {
            if let StreamEvent::ContentBlockStart {
                block_type: ToolUse,
                id,
                name,
                ..
            } = e
            {
                Some((id.as_deref(), name.as_deref()))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(tool_use_starts.len(), 2, "expected 2 ToolUse blocks");
    assert_eq!(tool_use_starts[0].1, Some("read_file"), "first tool name");
    assert_eq!(tool_use_starts[1].1, Some("list_dir"), "second tool name");
    assert_eq!(
        tool_use_starts[0].0,
        Some("call_real_001"),
        "first tool must replay the real id Gemini emitted"
    );
    assert!(
        tool_use_starts[1].0.is_some_and(|id| !id.is_empty()),
        "second tool must still have an id (synthesized) for engine correlation"
    );
    assert_ne!(
        tool_use_starts[1].0,
        Some("call_real_001"),
        "second tool's id must not collide with the first's real id"
    );
}

/// Gate 2.6 contract: full pipeline (SSE → engine builder → persistence
/// round-trip → `build_contents`) must produce a Gemini `Content` object
/// byte-identical to the original.
#[tokio::test]
async fn test_pipeline_byte_identity() {
    let bytes = load_sse_bytes("multipart_turn.sse");
    let expected = load_expected_json("multipart_turn_expected.json");

    let actual = run_gemini_test_pipeline(bytes, "gemini-3-pro-preview").await;

    assert_eq!(
        actual, expected,
        "replayed Content must byte-match originally streamed Content"
    );
}

/// Gate 2.5 contract: `functionResponse.id` is emitted iff the matching
/// `functionCall.id` was real. The fixture has one real-id call and one
/// synthesized-id call; on a turn-2 request that includes tool results for
/// both, `functionResponse_a` must include `id` and `functionResponse_b` must
/// omit it.
#[tokio::test]
async fn test_function_response_id_symmetry() {
    use zdx_types::{ToolResult, ToolResultContent};

    // Replicates the relevant slice of `multipart_turn_expected.json`: an
    // assistant turn with one real-id functionCall and one synthesized-id
    // functionCall, followed by a user message carrying tool results for both.
    let messages = vec![
        ChatMessage::user("kick off the work"),
        ChatMessage {
            role: "assistant".to_string(),
            phase: None,
            content: MessageContent::Blocks(vec![
                ChatContentBlock::ToolUse {
                    id: "call_real_001".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "README.md"}),
                    id_origin: IdOrigin::Real,
                    replay: None,
                },
                ChatContentBlock::ToolUse {
                    id: "call_synth_local".to_string(),
                    name: "list_dir".to_string(),
                    input: serde_json::json!({"path": "src"}),
                    id_origin: IdOrigin::Synthesized,
                    replay: None,
                },
            ]),
        },
        ChatMessage {
            role: "user".to_string(),
            phase: None,
            content: MessageContent::Blocks(vec![
                ChatContentBlock::ToolResult(ToolResult {
                    tool_use_id: "call_real_001".to_string(),
                    content: ToolResultContent::Text("# zdx\n".to_string()),
                    is_error: false,
                }),
                ChatContentBlock::ToolResult(ToolResult {
                    tool_use_id: "call_synth_local".to_string(),
                    content: ToolResultContent::Text("a.rs\nb.rs\n".to_string()),
                    is_error: false,
                }),
            ]),
        },
    ];

    let contents = build_contents(&messages, "gemini-3-pro-preview");

    // contents: [user("kick off"), assistant(2x functionCall), user(2x functionResponse)].
    let assistant_parts = contents[1]["parts"].as_array().expect("assistant parts");
    assert_eq!(assistant_parts.len(), 2);
    assert_eq!(
        assistant_parts[0]["functionCall"]["id"], "call_real_001",
        "real id must be emitted on functionCall"
    );
    assert!(
        assistant_parts[1]["functionCall"].get("id").is_none(),
        "synthesized id must be omitted on functionCall"
    );

    let response_parts = contents[2]["parts"].as_array().expect("response parts");
    assert_eq!(response_parts.len(), 2);

    let fr_real = &response_parts[0]["functionResponse"];
    assert_eq!(fr_real["name"], "read_file");
    assert_eq!(
        fr_real["id"], "call_real_001",
        "functionResponse must echo the real id"
    );

    let fr_synth = &response_parts[1]["functionResponse"];
    assert_eq!(fr_synth["name"], "list_dir");
    assert!(
        fr_synth.get("id").is_none(),
        "functionResponse for a synthesized-id call must omit `id` (cache symmetry)"
    );
}
