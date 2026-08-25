use std::fmt::Write as _;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use super::event::ThreadEvent;
use super::storage::truncate_str;

/// Returns a shortened thread ID for display.
pub fn short_thread_id(id: &str) -> String {
    if id.len() > 8 {
        format!("{}…", &id[..8])
    } else {
        id.to_string()
    }
}

/// Returns the title if present, otherwise the short-ID fallback.
pub(crate) fn display_title_or_short_id(title: Option<&str>, id: &str) -> String {
    title.map_or_else(|| short_thread_id(id), str::to_string)
}

/// Formats a `SystemTime` as a simple date/time string (YYYY-MM-DD HH:MM).
pub fn format_timestamp(time: SystemTime) -> Option<String> {
    let datetime: DateTime<Utc> = time.into();
    Some(datetime.format("%Y-%m-%d %H:%M").to_string())
}

/// Formats a `SystemTime` as a short relative age (e.g., "2m ago", "3h ago", "5d ago").
pub fn format_timestamp_relative(time: SystemTime) -> Option<String> {
    let datetime: DateTime<Utc> = time.into();
    let now = Utc::now();
    let seconds = now.signed_duration_since(datetime).num_seconds().max(0);

    let mins = seconds / 60;
    if mins < 1 {
        return Some("just now".to_string());
    }
    if mins < 60 {
        return Some(format!("{mins}m ago"));
    }

    let hours = mins / 60;
    if hours < 24 {
        return Some(format!("{hours}h ago"));
    }

    let days = hours / 24;
    if days < 7 {
        return Some(format!("{days}d ago"));
    }

    let weeks = days / 7;
    if weeks < 5 {
        return Some(format!("{weeks}w ago"));
    }

    let months = days / 30;
    if months < 12 {
        return Some(format!("{months}mo ago"));
    }

    let years = days / 365;
    Some(format!("{years}y ago"))
}

/// Total byte budget for a rendered transcript.
///
/// Transcripts are consumed by 1M-token models (`read_thread`, handoff, tldr),
/// so the budget exists to avoid blowing the context window, not to keep the
/// text small. Measured against the real thread corpus this leaves ~99.9% of
/// threads rendered verbatim; only threads built from a single oversized tool
/// result or several days of continuous work degrade.
const MAX_TRANSCRIPT_BYTES: usize = 2_800_000;

/// Allowance for one tool result or reasoning block once the whole transcript
/// no longer fits the budget.
const DEGRADED_BLOCK_BYTES: usize = 2_000;

/// Bytes kept from the head of a degraded block. The remainder comes from the
/// tail, where exit status and test summaries live.
const DEGRADED_HEAD_BYTES: usize = 600;

/// Smallest remaining budget still worth spending on a shortened block.
const MIN_DEGRADED_BYTES: usize = 200;

/// One rendered event.
struct Block {
    text: String,
    /// Tool results, tool arguments, and reasoning may be shortened under
    /// budget pressure. Messages and markers are kept whole or not at all.
    compressible: bool,
}

/// Returns the last `max_bytes` of `s`, snapped to a UTF-8 boundary.
fn tail_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Shortens a block to `budget` bytes, keeping both ends.
///
/// Head-only truncation is the wrong shape for tool output: a command echo is
/// rarely the evidence, while the pass/fail line at the end usually is.
fn head_and_tail(text: &str, budget: usize) -> String {
    const ELLIPSIS: &str = "\n… truncated …\n";
    if text.len() <= budget {
        return text.to_string();
    }
    if budget <= ELLIPSIS.len() + MIN_DEGRADED_BYTES {
        return String::new();
    }
    let content = budget - ELLIPSIS.len();
    let head = DEGRADED_HEAD_BYTES.min(content / 2);
    let tail = content - head;
    format!(
        "{}{ELLIPSIS}{}",
        truncate_str(text, head),
        tail_str(text, tail)
    )
}

/// Assembles blocks within `budget`.
///
/// Under budget the transcript is emitted verbatim. Over it, recent activity is
/// kept intact and older tool output is shortened or dropped, because the newest
/// events carry the evidence a caller is usually asking about.
fn fit_to_budget(blocks: Vec<Block>, budget: usize) -> String {
    let total: usize = blocks.iter().map(|b| b.text.len()).sum();
    if total <= budget {
        return blocks.into_iter().map(|b| b.text).collect();
    }

    let mut kept: Vec<String> = Vec::with_capacity(blocks.len());
    let mut used = 0usize;
    let mut omitted = 0usize;

    for block in blocks.into_iter().rev() {
        let remaining = budget.saturating_sub(used);
        if block.text.len() <= remaining {
            used += block.text.len();
            kept.push(block.text);
            continue;
        }
        if block.compressible {
            let shortened = head_and_tail(&block.text, DEGRADED_BLOCK_BYTES.min(remaining));
            if !shortened.is_empty() {
                used += shortened.len();
                kept.push(shortened);
                continue;
            }
        }
        omitted += 1;
    }

    kept.reverse();
    let mut output = String::with_capacity(used + 256);
    if omitted > 0 {
        writeln!(
            output,
            "### Transcript truncated\n{omitted} events omitted and older tool output shortened to fit the size budget. Recent activity is kept in full.\n"
        )
        .expect("write");
    }
    for text in kept {
        output.push_str(&text);
    }
    output
}

/// Renders one event, or `None` when it contributes no transcript text.
fn render_event(event: &ThreadEvent) -> Option<Block> {
    let mut text = String::new();
    match event {
        ThreadEvent::Meta { schema_version, .. } => {
            writeln!(text, "### Thread (schema v{schema_version})").expect("write");
            text.push('\n');
        }
        ThreadEvent::Message {
            role, text: body, ..
        } => {
            let role_label = match role.as_str() {
                "user" => "You",
                "assistant" => "Assistant",
                _ => role,
            };
            writeln!(text, "### {role_label}").expect("write");
            text.push_str(body);
            text.push_str("\n\n");
        }
        ThreadEvent::Reasoning { text: body, .. } => {
            let content = body.as_ref()?;
            text.push_str("### Thinking\n");
            text.push_str(content);
            text.push_str("\n\n");
            return Some(Block {
                text,
                compressible: true,
            });
        }
        ThreadEvent::ToolUse { name, input, .. } => {
            writeln!(text, "### Tool: {name}").expect("write");
            writeln!(
                text,
                "```json\n{}\n```\n",
                serde_json::to_string_pretty(input).unwrap_or_default()
            )
            .expect("write");
            return Some(Block {
                text,
                compressible: true,
            });
        }
        ThreadEvent::ToolResult {
            ok, output: out, ..
        } => {
            let status = if *ok { "✓" } else { "✗" };
            writeln!(text, "### Result {status}").expect("write");
            writeln!(
                text,
                "```json\n{}\n```\n",
                serde_json::to_string_pretty(out).unwrap_or_default()
            )
            .expect("write");
            return Some(Block {
                text,
                compressible: true,
            });
        }
        ThreadEvent::Interrupted { .. } => {
            text.push_str("### Interrupted\n\n");
        }
        ThreadEvent::Notice { message, .. } => {
            writeln!(text, "### Notice\n⚠ {message}\n").expect("write");
        }
        ThreadEvent::Usage { .. } => return None,
    }
    Some(Block {
        text,
        compressible: false,
    })
}

/// Formats a thread transcript in a human-readable format.
pub fn format_transcript(events: &[ThreadEvent]) -> String {
    let mut blocks: Vec<Block> = Vec::with_capacity(events.len());
    let mut models_used: Vec<String> = Vec::new();

    for event in events {
        if let ThreadEvent::Usage {
            model, provider, ..
        } = event
        {
            if let Some(label) = usage_model_label(model.as_deref(), provider.as_deref())
                && !models_used.contains(&label)
            {
                models_used.push(label);
            }
            continue;
        }
        if let Some(block) = render_event(event) {
            blocks.push(block);
        }
    }

    let mut output = fit_to_budget(blocks, MAX_TRANSCRIPT_BYTES);

    if !models_used.is_empty() {
        writeln!(output, "### Models used\n{}\n", models_used.join(", ")).expect("write");
    }

    output.trim_end().to_string()
}

/// Builds a display label for a usage event's model/provider attribution.
/// Returns `provider:model`, or just the model or provider when only one is
/// known, or `None` when neither is present.
fn usage_model_label(model: Option<&str>, provider: Option<&str>) -> Option<String> {
    match (
        provider.filter(|p| !p.is_empty()),
        model.filter(|m| !m.is_empty()),
    ) {
        (Some(provider), Some(model)) => Some(format!("{provider}:{model}")),
        (None, Some(model)) => Some(model.to_string()),
        (Some(provider), None) => Some(provider.to_string()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn block(text: &str, compressible: bool) -> Block {
        Block {
            text: text.to_string(),
            compressible,
        }
    }

    #[test]
    fn under_budget_is_verbatim() {
        let blocks = vec![block("aaa", false), block("bbb", true)];
        assert_eq!(fit_to_budget(blocks, 1000), "aaabbb");
    }

    #[test]
    fn head_and_tail_keeps_both_ends() {
        let text = format!("START{}END", "x".repeat(50_000));
        let out = head_and_tail(&text, 2_000);

        assert!(out.len() < text.len());
        assert!(out.starts_with("START"), "head lost: {}", &out[..20]);
        assert!(out.ends_with("END"), "tail lost — this is the evidence end");
        assert!(out.contains("… truncated …"));
    }

    #[test]
    fn over_budget_keeps_newest_intact_and_shortens_older() {
        let old = format!("OLDSTART{}OLDEND", "x".repeat(40_000));
        let recent = "RECENT-EVIDENCE".to_string();
        let blocks = vec![block(&old, true), block(&recent, true)];

        let out = fit_to_budget(blocks, 5_000);

        assert!(out.contains("RECENT-EVIDENCE"), "newest must survive whole");
        assert!(out.contains("OLDSTART"), "older block keeps its head");
        assert!(out.contains("OLDEND"), "older block keeps its tail");
        assert!(out.len() <= 5_000);
    }

    #[test]
    fn incompressible_blocks_that_do_not_fit_are_reported() {
        let huge = "y".repeat(10_000);
        let blocks = vec![block(&huge, false), block("recent", false)];

        let out = fit_to_budget(blocks, 1_000);

        assert!(out.contains("recent"));
        assert!(!out.contains(&huge));
        assert!(
            out.contains("1 events omitted"),
            "omission must be visible: {out}"
        );
    }

    #[test]
    fn tail_str_snaps_to_char_boundary() {
        let text = "aaaé";
        // 5 bytes total; a cut at 2 bytes would land inside the 2-byte 'é'.
        assert_eq!(tail_str(text, 2), "é");
    }
}
