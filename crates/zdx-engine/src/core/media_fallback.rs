//! Image fallback for models that do not accept image input.
//!
//! Some chat models (e.g. `deepseek:deepseek-v4-flash`) have no vision path:
//! sending image bytes either errors or is silently dropped by the provider.
//! Instead of shipping bytes the model cannot read, the engine swaps every
//! image block for a text note pointing at `zdx ask-media`, which the model can
//! call with a question it writes itself for whatever it actually needs.

use zdx_types::{ToolResultBlock, ToolResultContent};

use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};

const IMAGE_NOTE: &str = "[Image omitted: the active model cannot read images. To inspect it, run `zdx ask-media <path> -p \"<question>\"` (see the `ask-media` skill) with a question written for what you need from this image. The file path is in the surrounding text or tool output.]";

/// Replaces every image block with [`IMAGE_NOTE`], in user content and in tool
/// results, and returns how many images were replaced.
///
/// Idempotent: notes are plain text, so a second pass replaces nothing.
pub fn replace_images_with_ask_media_notes(messages: &mut [ChatMessage]) -> usize {
    let mut replaced = 0;
    for message in messages {
        let MessageContent::Blocks(blocks) = &mut message.content else {
            continue;
        };
        for block in blocks.iter_mut() {
            match block {
                ChatContentBlock::Image { .. } => {
                    *block = ChatContentBlock::text(IMAGE_NOTE);
                    replaced += 1;
                }
                ChatContentBlock::ToolResult(result) => {
                    replaced += replace_tool_result_images(&mut result.content);
                }
                _ => {}
            }
        }
    }
    replaced
}

/// Drops image blocks from a tool result and folds the note into its text.
///
/// The note is merged into the existing text block rather than pushed as a
/// second one: providers that flatten tool results keep only the first text
/// block, so an appended block would never reach the model.
fn replace_tool_result_images(content: &mut ToolResultContent) -> usize {
    let ToolResultContent::Blocks(blocks) = content else {
        return 0;
    };

    let before = blocks.len();
    blocks.retain(|block| !matches!(block, ToolResultBlock::Image { .. }));
    let replaced = before - blocks.len();
    if replaced == 0 {
        return 0;
    }

    match blocks.iter_mut().find_map(|block| match block {
        ToolResultBlock::Text { text } => Some(text),
        ToolResultBlock::Image { .. } => None,
    }) {
        Some(text) => {
            text.push_str("\n\n");
            text.push_str(IMAGE_NOTE);
        }
        None => blocks.push(ToolResultBlock::Text {
            text: IMAGE_NOTE.to_string(),
        }),
    }

    replaced
}

#[cfg(test)]
mod tests {
    use zdx_types::ToolResult;

    use super::*;

    fn image_block() -> ChatContentBlock {
        ChatContentBlock::Image {
            mime_type: "image/png".to_string(),
            data: "Zm9v".to_string(),
        }
    }

    #[test]
    fn replaces_user_image_blocks_with_a_note() {
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            phase: None,
            content: MessageContent::Blocks(vec![
                ChatContentBlock::text("Image attachment saved at /tmp/a.png."),
                image_block(),
            ]),
        }];

        assert_eq!(replace_images_with_ask_media_notes(&mut messages), 1);

        let MessageContent::Blocks(blocks) = &messages[0].content else {
            panic!("expected blocks");
        };
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, ChatContentBlock::Image { .. }))
        );
        assert!(matches!(
            &blocks[1],
            ChatContentBlock::Text { text, .. } if text.contains("zdx ask-media")
        ));
    }

    #[test]
    fn folds_tool_result_images_into_the_existing_text_block() {
        let mut messages = vec![ChatMessage::tool_results(vec![ToolResult {
            tool_use_id: "1".to_string(),
            content: ToolResultContent::Blocks(vec![
                ToolResultBlock::Text {
                    text: "{\"file_path\":\"/tmp/a.png\"}".to_string(),
                },
                ToolResultBlock::Image {
                    mime_type: "image/png".to_string(),
                    data: "Zm9v".to_string(),
                },
            ]),
            is_error: false,
        }])];

        assert_eq!(replace_images_with_ask_media_notes(&mut messages), 1);

        let MessageContent::Blocks(blocks) = &messages[0].content else {
            panic!("expected blocks");
        };
        let ChatContentBlock::ToolResult(result) = &blocks[0] else {
            panic!("expected tool result");
        };
        let ToolResultContent::Blocks(result_blocks) = &result.content else {
            panic!("expected result blocks");
        };
        assert_eq!(result_blocks.len(), 1);
        let text = result.content.as_text().unwrap();
        assert!(text.starts_with("{\"file_path\""));
        assert!(text.contains("zdx ask-media"));
    }

    #[test]
    fn is_idempotent() {
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            phase: None,
            content: MessageContent::Blocks(vec![image_block()]),
        }];

        assert_eq!(replace_images_with_ask_media_notes(&mut messages), 1);
        assert_eq!(replace_images_with_ask_media_notes(&mut messages), 0);
    }
}
