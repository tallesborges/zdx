//! Rich messages (Bot API 10.1) flattened to plain text.
//!
//! A rich message carries no `text`: its words live in `rich_message.blocks`.
//! Blocks and inline text stay `serde_json::Value` on purpose — Telegram keeps
//! adding block and text types, and an unknown one must degrade to its nested
//! text instead of failing the whole update or dropping what the user wrote.

use serde::Deserialize;
use serde_json::{Map, Value};

/// Keys that carry user-visible content in any rich block or rich text object.
/// The generic fallback walks only these, so file ids, URLs, and layout hints
/// never leak into the prompt.
const CONTENT_KEYS: [&str; 10] = [
    "text",
    "summary",
    "caption",
    "credit",
    "blocks",
    "items",
    "cells",
    "buttons",
    "label",
    "expression",
];

/// Rich formatted message content (`Message.rich_message`).
#[derive(Debug, Deserialize)]
pub struct RichMessage {
    #[serde(default)]
    blocks: Vec<Value>,
}

impl RichMessage {
    /// Markdown-ish plain text of the message, or `None` when it has no text.
    pub fn to_text(&self) -> Option<String> {
        let rendered = self
            .blocks
            .iter()
            .filter_map(render_block)
            .collect::<Vec<_>>()
            .join("\n\n");
        let trimmed = rendered.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }
}

fn render_block(block: &Value) -> Option<String> {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let rendered = match block_type {
        "paragraph" | "footer" => rich_text_at(block, "text"),
        "heading" => render_heading(block),
        "pre" => render_pre(block),
        "divider" => "---".to_string(),
        "anchor" => String::new(),
        "list" => render_list(block),
        "blockquote" => render_quote(&render_nested_blocks(block), &rich_text_at(block, "credit")),
        "expandable_blockquote" | "pullquote" => {
            render_quote(&rich_text_at(block, "text"), &rich_text_at(block, "credit"))
        }
        "details" => join_lines(&[rich_text_at(block, "summary"), render_nested_blocks(block)]),
        "table" => render_table(block),
        "collage" | "slideshow" => {
            join_lines(&[render_nested_blocks(block), rich_text_at(block, "caption")])
        }
        "animation" | "audio" | "document" | "map" | "photo" | "video" | "voice_note" => {
            render_media(block_type, block)
        }
        _ => nested_content(block),
    };
    (!rendered.trim().is_empty()).then(|| rendered.trim_end().to_string())
}

fn render_heading(block: &Value) -> String {
    let text = rich_text_at(block, "text");
    if text.trim().is_empty() {
        return String::new();
    }
    let size = block
        .get("size")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 6);
    let hashes = "#".repeat(usize::try_from(size).unwrap_or(1));
    format!("{hashes} {text}")
}

fn render_pre(block: &Value) -> String {
    let text = rich_text_at(block, "text");
    if text.trim().is_empty() {
        return String::new();
    }
    let language = block
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("```{language}\n{text}\n```")
}

fn render_list(block: &Value) -> String {
    let Some(items) = block.get("items").and_then(Value::as_array) else {
        return nested_content(block);
    };
    items
        .iter()
        .map(render_list_item)
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_list_item(item: &Value) -> String {
    let content = render_nested_blocks(item);
    if content.trim().is_empty() {
        return String::new();
    }
    let marker = if flag_at(item, "has_checkbox") {
        if flag_at(item, "is_checked") {
            "- [x]".to_string()
        } else {
            "- [ ]".to_string()
        }
    } else {
        let label = item
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if label.is_empty() {
            "-".to_string()
        } else {
            label.to_string()
        }
    };

    let mut lines = content.lines();
    let first = lines.next().unwrap_or_default();
    let mut rendered = format!("{marker} {first}");
    for line in lines {
        rendered.push_str("\n  ");
        rendered.push_str(line);
    }
    rendered
}

fn render_quote(content: &str, credit: &str) -> String {
    if content.trim().is_empty() {
        return String::new();
    }
    let mut quoted = content
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    if !credit.trim().is_empty() {
        quoted.push_str("\n> — ");
        quoted.push_str(credit);
    }
    quoted
}

fn render_table(block: &Value) -> String {
    let Some(rows) = block.get("cells").and_then(Value::as_array) else {
        return nested_content(block);
    };
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(cells) = row.as_array() else {
            continue;
        };
        if cells.is_empty() {
            continue;
        }
        let rendered = cells
            .iter()
            .map(|cell| rich_text_at(cell, "text").replace('\n', " "))
            .collect::<Vec<_>>();
        lines.push(format!("| {} |", rendered.join(" | ")));
        if index == 0 && cells.iter().any(|cell| flag_at(cell, "is_header")) {
            lines.push(format!("| {} |", vec!["---"; rendered.len()].join(" | ")));
        }
    }
    join_lines(&[lines.join("\n"), rich_text_at(block, "caption")])
}

/// Media inside a rich message is not downloaded as an attachment, so it is
/// named instead of dropped: a caption alone would hide that media was sent.
fn render_media(block_type: &str, block: &Value) -> String {
    let label = block_type.replace('_', " ");
    let caption = rich_text_at(block, "caption");
    if caption.trim().is_empty() {
        format!("[{label}]")
    } else {
        format!("[{label}] {caption}")
    }
}

fn render_nested_blocks(value: &Value) -> String {
    let Some(blocks) = value.get("blocks").and_then(Value::as_array) else {
        return String::new();
    };
    blocks
        .iter()
        .filter_map(render_block)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text of an unknown block type: whatever content its known keys hold.
fn nested_content(value: &Value) -> String {
    let mut parts = Vec::new();
    collect_content(value, &mut parts);
    parts.join("\n")
}

fn collect_content(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if !text.trim().is_empty() {
                out.push(text.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_content(item, out);
            }
        }
        Value::Object(map) => {
            for key in CONTENT_KEYS {
                if let Some(child) = map.get(key) {
                    collect_content(child, out);
                }
            }
        }
        _ => {}
    }
}

fn rich_text_at(value: &Value, key: &str) -> String {
    value.get(key).map(render_rich_text).unwrap_or_default()
}

fn flag_at(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn join_lines(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|part| !part.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// `RichText` is a string, an array of rich texts, or an object wrapping more
/// rich text with a style. Styles carry no meaning for the agent, so only the
/// words survive — except where the payload *is* the content (LaTeX, custom
/// emoji fallback, link targets).
fn render_rich_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(render_rich_text).collect(),
        Value::Object(map) => match map.get("type").and_then(Value::as_str).unwrap_or_default() {
            "mathematical_expression" => string_at(map, "expression"),
            "custom_emoji" => string_at(map, "alternative_text"),
            "anchor" => String::new(),
            "url" => render_link(map),
            _ => map.get("text").map(render_rich_text).unwrap_or_default(),
        },
        _ => String::new(),
    }
}

fn render_link(map: &Map<String, Value>) -> String {
    let text = map.get("text").map(render_rich_text).unwrap_or_default();
    let url = string_at(map, "url");
    if url.is_empty() || text.trim() == url {
        text
    } else {
        format!("{text} ({url})")
    }
}

fn string_at(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rich(blocks: &Value) -> RichMessage {
        serde_json::from_value(serde_json::json!({ "blocks": blocks })).expect("rich message")
    }

    #[test]
    fn renders_paragraph_and_list() {
        let message = rich(&serde_json::json!([
            { "type": "paragraph", "text": "Agosto:" },
            { "type": "list", "items": [
                { "label": "•", "blocks": [{ "type": "paragraph", "text": "Item one: 460,00" }] },
                { "label": "•", "blocks": [{ "type": "paragraph", "text": "Item two: 296,00" }] },
            ]},
        ]));

        assert_eq!(
            message.to_text().as_deref(),
            Some("Agosto:\n\n• Item one: 460,00\n• Item two: 296,00")
        );
    }

    #[test]
    fn renders_styled_and_nested_rich_text() {
        let message = rich(&serde_json::json!([
            { "type": "paragraph", "text": [
                "total ",
                { "type": "bold", "text": [{ "type": "italic", "text": "1.552,00" }] },
                { "type": "url", "text": "receipt", "url": "https://example.com/r" },
            ]},
        ]));

        assert_eq!(
            message.to_text().as_deref(),
            Some("total 1.552,00receipt (https://example.com/r)")
        );
    }

    #[test]
    fn renders_headings_code_quotes_and_tasks() {
        let message = rich(&serde_json::json!([
            { "type": "heading", "text": "Report", "size": 2 },
            { "type": "pre", "text": "cargo test", "language": "bash" },
            { "type": "blockquote", "blocks": [{ "type": "paragraph", "text": "quoted" }], "credit": "author" },
            { "type": "list", "items": [
                { "has_checkbox": true, "is_checked": true, "blocks": [{ "type": "paragraph", "text": "done" }] },
                { "has_checkbox": true, "blocks": [{ "type": "paragraph", "text": "todo" }] },
            ]},
            { "type": "divider" },
        ]));

        assert_eq!(
            message.to_text().as_deref(),
            Some(
                "## Report\n\n```bash\ncargo test\n```\n\n> quoted\n> — author\n\n- [x] done\n- [ ] todo\n\n---"
            )
        );
    }

    #[test]
    fn renders_table_with_header_row() {
        let message = rich(&serde_json::json!([
            { "type": "table", "cells": [
                [{ "text": "Month", "is_header": true }, { "text": "Total", "is_header": true }],
                [{ "text": "August" }, { "text": "1.552,00" }],
            ], "caption": "Expenses" },
        ]));

        assert_eq!(
            message.to_text().as_deref(),
            Some("| Month | Total |\n| --- | --- |\n| August | 1.552,00 |\nExpenses")
        );
    }

    #[test]
    fn names_media_blocks_instead_of_dropping_them() {
        let message = rich(&serde_json::json!([
            { "type": "photo", "photo": [{ "file_id": "abc" }], "caption": { "text": "the receipt" } },
            { "type": "voice_note", "voice_note": { "file_id": "def" } },
        ]));

        assert_eq!(
            message.to_text().as_deref(),
            Some("[photo] the receipt\n\n[voice note]")
        );
    }

    #[test]
    fn keeps_text_of_unknown_block_and_text_types() {
        let message = rich(&serde_json::json!([
            { "type": "future_block", "text": "still readable", "blocks": [
                { "type": "paragraph", "text": "nested too" },
            ]},
            { "type": "paragraph", "text": { "type": "future_style", "text": "styled" } },
        ]));

        assert_eq!(
            message.to_text().as_deref(),
            Some("still readable\nnested too\n\nstyled")
        );
    }

    #[test]
    fn empty_content_is_none() {
        assert!(rich(&serde_json::json!([])).to_text().is_none());
        assert!(
            rich(&serde_json::json!([{ "type": "anchor", "name": "top" }]))
                .to_text()
                .is_none()
        );
    }
}
