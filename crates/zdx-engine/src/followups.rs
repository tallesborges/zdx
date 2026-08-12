//! Shared parsing for end-of-turn `<followups>` suggestion blocks.
//!
//! The model may append a `<followups>` block to its final reply listing
//! optional next-step suggestions. Each surface (Telegram bot, TUI) strips the
//! block from the visible reply and renders the suggestions in its own way;
//! only the pure parsing lives here.

const FOLLOWUPS_OPEN: &str = "<followups>";
const FOLLOWUPS_CLOSE: &str = "</followups>";
const FOLLOWUP_OPEN: &str = "<followup>";
const FOLLOWUP_CLOSE: &str = "</followup>";

/// Maximum suggestions kept from a reply.
pub const MAX_FOLLOWUPS: usize = 4;

/// Extracts `<followups>` blocks from the final reply text.
///
/// Returns the cleaned text (block removed) and the list of suggestions
/// (capped at [`MAX_FOLLOWUPS`], deduplicated, empty items dropped). An
/// unterminated block is left intact in the cleaned text.
#[must_use]
pub fn extract_followups(input: &str) -> (String, Vec<String>) {
    let mut cleaned = String::new();
    let mut items = Vec::new();
    let mut cursor = 0;

    while let Some(start_rel) = input[cursor..].find(FOLLOWUPS_OPEN) {
        let start = cursor + start_rel;
        cleaned.push_str(&input[cursor..start]);

        let content_start = start + FOLLOWUPS_OPEN.len();
        let Some(close_rel) = input[content_start..].find(FOLLOWUPS_CLOSE) else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let content_end = content_start + close_rel;
        collect_followup_items(&input[content_start..content_end], &mut items);
        cursor = content_end + FOLLOWUPS_CLOSE.len();
    }

    if cursor < input.len() {
        cleaned.push_str(&input[cursor..]);
    }

    items.truncate(MAX_FOLLOWUPS);
    (cleaned, items)
}

fn collect_followup_items(block: &str, items: &mut Vec<String>) {
    let mut cursor = 0;
    while let Some(start_rel) = block[cursor..].find(FOLLOWUP_OPEN) {
        let content_start = cursor + start_rel + FOLLOWUP_OPEN.len();
        let tail = &block[content_start..];
        let close_rel = tail.find(FOLLOWUP_CLOSE);
        let next_open_rel = tail.find(FOLLOWUP_OPEN);

        if let Some(next_open) = next_open_rel
            && close_rel.is_none_or(|close| next_open < close)
        {
            collect_plain_item(&tail[..next_open], items);
            cursor = content_start + next_open;
            continue;
        }

        if let Some(close) = close_rel {
            collect_plain_item(&tail[..close], items);
            cursor = content_start + close + FOLLOWUP_CLOSE.len();
        } else {
            collect_plain_item(tail, items);
            break;
        }
    }
}

fn collect_plain_item(candidate: &str, items: &mut Vec<String>) {
    let plain_end = candidate.find(['<', '>']).unwrap_or(candidate.len());
    let item = candidate[..plain_end].trim();
    if !item.is_empty() && !items.iter().any(|existing| existing == item) {
        items.push(item.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_block_and_cleans_text() {
        let input = "Reply text.\n\n<followups><followup>Run tests</followup><followup>Commit it</followup></followups>";
        let (cleaned, items) = extract_followups(input);
        assert_eq!(cleaned.trim(), "Reply text.");
        assert_eq!(
            items,
            vec!["Run tests".to_string(), "Commit it".to_string()]
        );
    }

    #[test]
    fn ignores_text_without_followups() {
        let (cleaned, items) = extract_followups("Just a reply.");
        assert_eq!(cleaned, "Just a reply.");
        assert!(items.is_empty());
    }

    #[test]
    fn dedupes_and_caps_items() {
        let input = "<followups>\
            <followup>A</followup><followup>A</followup>\
            <followup>B</followup><followup>C</followup>\
            <followup>D</followup><followup>E</followup>\
            </followups>";
        let (_, items) = extract_followups(input);
        assert_eq!(items, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn keeps_unclosed_block_as_text() {
        let input = "Reply. <followups><followup>A</followup>";
        let (cleaned, items) = extract_followups(input);
        assert_eq!(cleaned, input);
        assert!(items.is_empty());
    }

    #[test]
    fn recovers_item_with_misspelled_closing_tag() {
        let input = concat!(
            "Reply. <followups>",
            "<followup>Remove dim-tickets</footup>",
            "<followup>Fully retire it</followup>",
            "<followup>Stop the stack</followup>",
            "</followups>"
        );
        let (cleaned, items) = extract_followups(input);
        assert_eq!(cleaned.trim(), "Reply.");
        assert_eq!(
            items,
            vec!["Remove dim-tickets", "Fully retire it", "Stop the stack"]
        );
    }

    #[test]
    fn missing_close_does_not_consume_next_valid_item() {
        let input = concat!(
            "<followups>",
            "<followup>First",
            "<followup>Second</followup>",
            "</followups>"
        );
        let (_, items) = extract_followups(input);
        assert_eq!(items, vec!["First", "Second"]);
    }

    #[test]
    fn strips_tag_fragments_from_recovered_labels() {
        let input = concat!(
            "<followups>",
            "<followup>Safe label</footup><garbage>",
            "<followup>Later item</followup>",
            "</followups>"
        );
        let (_, items) = extract_followups(input);
        assert_eq!(items, vec!["Safe label", "Later item"]);
        assert!(items.iter().all(|item| !item.contains(['<', '>'])));
    }
}
