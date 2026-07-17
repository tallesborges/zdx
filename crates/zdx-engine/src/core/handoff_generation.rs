//! Handoff context generation for an existing thread using an LLM subagent.
//!
//! Loads the thread's persisted events, formats them as a transcript, and asks
//! a subagent (configured via `Config::handoff_model`) to produce a condensed
//! context prompt for continuing the work in a fresh thread. Shared by the TUI
//! `/handoff` command and the Telegram bot.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio_util::sync::CancellationToken;

use crate::config::ThinkingLevel;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent_with_cancel};
use crate::core::thread_persistence as tp;
use crate::prompts::HANDOFF_PROMPT_TEMPLATE;
use crate::zdx_context::build_zdx_context;

/// Timeout for handoff generation subagent (2 minutes).
const HANDOFF_TIMEOUT_SECS: u64 = 120;

/// One ancestor in a handoff lineage: `(thread_id, display_title)`.
type LineageEntry = (String, String);

/// Walks `handoff_from` from `source_thread_id` up to the root, returning the
/// chain as `(id, display_title)` starting at the source thread.
///
/// The lookup is built once from `list_all_threads()`. The walk stops when a
/// thread has no `handoff_from` or its parent isn't found. Falls back to a
/// single source entry when thread metadata can't be read.
fn collect_lineage(source_thread_id: &str) -> Vec<LineageEntry> {
    let Ok(threads) = tp::list_all_threads() else {
        return vec![(source_thread_id.to_string(), source_thread_id.to_string())];
    };
    let by_id: HashMap<String, _> = threads.into_iter().map(|t| (t.id.clone(), t)).collect();

    let mut chain: Vec<LineageEntry> = Vec::new();
    let mut current: Option<&str> = Some(source_thread_id);
    while let Some(id) = current {
        let Some(summary) = by_id.get(id) else {
            if chain.is_empty() {
                chain.push((id.to_string(), id.to_string()));
            }
            break;
        };
        chain.push((summary.id.clone(), summary.display_title()));
        current = summary.handoff_from.as_deref();
    }
    chain
}

/// Formats one lineage entry as `id "title"`.
fn format_lineage_entry(entry: &LineageEntry) -> String {
    let (id, title) = entry;
    format!("{id} \"{title}\"")
}

/// Builds the parenthetical note pointing at the source thread and its lineage.
///
/// With no ancestors it keeps the original short wording; with ancestors it
/// renders the full chain so the new assistant can `read_thread` any of them.
fn build_lineage_note(lineage: &[LineageEntry], message_empty: bool) -> String {
    let Some(source) = lineage.first() else {
        return String::new();
    };
    if lineage.len() <= 1 {
        let id = &source.0;
        if message_empty {
            format!("(Continuing from thread {id} — call read_thread for full context.)")
        } else {
            format!(
                "(Continuing from thread {id} — call read_thread for anything below that's missing.)"
            )
        }
    } else {
        let chain = lineage
            .iter()
            .map(format_lineage_entry)
            .collect::<Vec<_>>()
            .join(" ← ");
        let source_str = format_lineage_entry(source);
        format!(
            "(Continuing from {source_str}. Lineage: {chain}. Call read_thread on any thread ID above for missing context.)"
        )
    }
}

/// Prefix shown at the beginning of generated handoff output.
///
/// The user's literal next-chat message leads (so the new assistant sees the
/// user's own words first, exactly as typed), followed by a short parenthetical
/// pointing at the source thread and its full ancestor lineage. The
/// LLM-generated context block is appended after this prefix.
fn build_handoff_prefix(lineage: &[LineageEntry], next_message: &str) -> String {
    let trimmed = next_message.trim();
    let note = build_lineage_note(lineage, trimmed.is_empty());
    if trimmed.is_empty() {
        note
    } else {
        format!("{trimmed}\n\n{note}")
    }
}

/// Builds the prompt for handoff generation.
fn build_handoff_prompt(thread_content: &str, next_message: &str, zdx_context: &str) -> String {
    HANDOFF_PROMPT_TEMPLATE
        .replace("{{ZDX_CONTEXT}}", zdx_context)
        .replace("{{THREAD_CONTENT}}", thread_content)
        .replace("{{NEXT_MESSAGE}}", next_message)
}

/// Generates a handoff prompt for `thread_id`, ready to seed a new thread.
///
/// The result leads with the user's `next_message` (verbatim, when non-empty)
/// and a lineage note pointing at the source thread, followed by the
/// LLM-generated context block.
///
/// # Errors
/// Returns an error when the thread cannot be loaded, is empty, or the
/// subagent fails / times out / is cancelled.
pub async fn generate_handoff(
    thread_id: &str,
    next_message: &str,
    handoff_model: &str,
    root: &Path,
    cancel: Option<CancellationToken>,
) -> Result<String> {
    let events = tp::load_thread_events(thread_id)
        .with_context(|| format!("load thread '{thread_id}' for handoff"))?;
    ensure!(!events.is_empty(), "Thread '{thread_id}' is empty");
    let thread_content = tp::format_transcript(&events);

    let generation_prompt =
        build_handoff_prompt(&thread_content, next_message, &build_zdx_context(root));
    let lineage = collect_lineage(thread_id);
    let handoff_prefix = build_handoff_prefix(&lineage, next_message);

    let (model, thinking) = crate::models::split_model_thinking(handoff_model);
    let options = ExecSubagentOptions {
        model: Some(model.to_string()),
        system_prompt: None,
        thinking_level: Some(thinking.unwrap_or(ThinkingLevel::Low)),
        no_tools: true,
        no_system_prompt: true,
        tools_override: None,
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: Some(Duration::from_secs(HANDOFF_TIMEOUT_SECS)),
        thread_origin_kind: Some("helper:handoff".to_string()),
        ..Default::default()
    };

    let generated_prompt =
        run_exec_subagent_with_cancel(root, &generation_prompt, &options, cancel, None).await?;
    Ok(format!("{handoff_prefix}\n\n{generated_prompt}"))
}

#[cfg(test)]
mod tests {
    use super::{LineageEntry, build_handoff_prefix};

    fn entry(id: &str, title: &str) -> LineageEntry {
        (id.to_string(), title.to_string())
    }

    #[test]
    fn handoff_prefix_mentions_thread_and_read_thread_tool() {
        let lineage = vec![entry("thread-123", "Ship feature")];
        let prefix = build_handoff_prefix(&lineage, "ship the new feature");
        assert!(prefix.contains("thread-123"));
        assert!(prefix.contains("read_thread"));
    }

    #[test]
    fn handoff_prefix_leads_with_next_message_verbatim() {
        let msg = "now lets streamline the comments";
        let lineage = vec![entry("thread-xyz", "Streamline comments")];
        let prefix = build_handoff_prefix(&lineage, msg);
        assert!(prefix.starts_with(msg), "user message must lead the prefix");
        assert!(
            !prefix.contains("My goal:"),
            "prefix must not relabel the user's message as a goal"
        );
    }

    #[test]
    fn handoff_prefix_handles_empty_next_message() {
        let lineage = vec![entry("thread-abc", "Some work")];
        let prefix = build_handoff_prefix(&lineage, "   ");
        assert!(prefix.contains("thread-abc"));
        assert!(prefix.contains("read_thread"));
        // Empty case has no leading user-text section, just the parenthetical.
        assert!(prefix.starts_with('('));
    }

    #[test]
    fn handoff_prefix_single_thread_keeps_short_wording() {
        let lineage = vec![entry("thread-solo", "Solo work")];
        let prefix = build_handoff_prefix(&lineage, "keep going");
        // No ancestor chain is rendered when the source has no parents.
        assert!(
            !prefix.contains("Lineage:"),
            "single-thread handoff should not render a lineage chain"
        );
        assert!(!prefix.contains('←'));
    }

    #[test]
    fn handoff_prefix_renders_full_lineage_in_order() {
        // Source first, then ancestors up to the root: D ← C ← B ← A.
        let lineage = vec![
            entry("thread-d", "Deploy"),
            entry("thread-c", "Cleanup"),
            entry("thread-b", "Build"),
            entry("thread-a", "Analyze"),
        ];
        let prefix = build_handoff_prefix(&lineage, "keep going");

        assert!(prefix.starts_with("keep going"), "message must lead");
        assert!(prefix.contains("Lineage:"));
        assert!(prefix.contains('←'));
        assert!(prefix.contains("read_thread"));

        for (id, title) in [
            ("thread-d", "Deploy"),
            ("thread-c", "Cleanup"),
            ("thread-b", "Build"),
            ("thread-a", "Analyze"),
        ] {
            assert!(prefix.contains(id), "missing ancestor id {id}");
            assert!(prefix.contains(title), "missing ancestor title {title}");
        }

        // Ancestors appear source-first, oldest-last.
        let pos = |needle: &str| prefix.find(needle).unwrap();
        assert!(
            pos("thread-d") < pos("thread-c")
                && pos("thread-c") < pos("thread-b")
                && pos("thread-b") < pos("thread-a"),
            "lineage must render in order D ← C ← B ← A"
        );
    }
}
