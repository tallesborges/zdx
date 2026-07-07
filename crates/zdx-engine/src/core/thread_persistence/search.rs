use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;

use super::event::ThreadEvent;
use super::format::display_title_or_short_id;
use super::storage::{ThreadSummary, list_threads, load_thread_events, truncate_str};
use crate::config::paths::threads_dir;

/// Options for thread search.
#[derive(Debug, Clone)]
pub struct ThreadSearchOptions {
    pub query: Option<String>,
    pub date: Option<NaiveDate>,
    pub date_start: Option<NaiveDate>,
    pub date_end: Option<NaiveDate>,
    pub limit: usize,
    /// Thread ID to exclude from results (e.g. the current active thread).
    pub exclude_thread_id: Option<String>,
}

impl Default for ThreadSearchOptions {
    fn default() -> Self {
        Self {
            query: None,
            date: None,
            date_start: None,
            date_end: None,
            limit: 20,
            exclude_thread_id: None,
        }
    }
}

/// A thread search match.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadSearchResult {
    pub thread_id: String,
    pub title: Option<String>,
    pub root_path: Option<String>,
    pub activity_at: Option<String>,
    pub preview: String,
}

impl ThreadSearchResult {
    /// Returns a display-friendly title (or short ID fallback).
    pub fn display_title(&self) -> String {
        display_title_or_short_id(self.title.as_deref(), &self.thread_id)
    }
}

/// Options for searching tool calls across saved threads.
#[derive(Debug, Clone)]
pub struct ThreadToolSearchOptions {
    pub tool_name: Option<String>,
    pub failed_only: bool,
    pub date: Option<NaiveDate>,
    pub date_start: Option<NaiveDate>,
    pub date_end: Option<NaiveDate>,
    pub limit: usize,
}

impl Default for ThreadToolSearchOptions {
    fn default() -> Self {
        Self {
            tool_name: None,
            failed_only: false,
            date: None,
            date_start: None,
            date_end: None,
            limit: 20,
        }
    }
}

/// A tool call found in saved threads.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadToolMatch {
    pub thread_id: String,
    pub title: Option<String>,
    pub tool_use_id: String,
    pub tool_name: String,
    pub tool_ts: Option<String>,
    pub status: String,
    pub args_summary: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl ThreadToolMatch {
    /// Returns a display-friendly title (or short ID fallback).
    pub fn display_title(&self) -> String {
        display_title_or_short_id(self.title.as_deref(), &self.thread_id)
    }
}

#[derive(Debug, Clone)]
struct PendingToolUse {
    tool_use_id: String,
    tool_name: String,
    tool_ts: String,
    args_summary: String,
}

const SEARCH_PREVIEW_MAX_BYTES: usize = 200;
const TOOL_ARGS_SUMMARY_MAX_BYTES: usize = 160;

/// Searches threads by optional query and/or date filters.
///
/// Results are ordered by recency (`list_threads()` already returns
/// newest-first). When a query is provided the grep pre-filter narrows
/// candidates; we collect up to `limit` matches (early termination).
///
/// # Errors
/// Returns an error if thread listing fails.
pub fn search_threads(options: &ThreadSearchOptions) -> Result<Vec<ThreadSearchResult>> {
    let normalized_query = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(String::from);
    let limit = options.limit.max(1);

    // Build a grep matcher once for the whole search. Used to pre-filter raw
    // files before expensive JSON deserialisation.
    let grep_matcher = normalized_query.as_deref().and_then(build_grep_matcher);
    let mut grep_searcher = grep_searcher::Searcher::new();

    let mut results: Vec<ThreadSearchResult> = Vec::new();

    // list_threads() is already sorted by modified time (newest first).
    for summary in list_threads()? {
        // Skip the current active thread if one is specified.
        if let Some(ref excluded) = options.exclude_thread_id
            && &summary.id == excluded
        {
            continue;
        }

        // Fast grep pre-filter: check title first (already in memory from
        // list_threads), then scan the raw JSONL file for query terms.
        // Threads that can't possibly match are skipped before deserialising.
        if let Some(ref matcher) = grep_matcher {
            let title_contains_any_word = normalized_query.as_deref().is_some_and(|q| {
                let title_lower = summary
                    .title
                    .as_deref()
                    .map(str::to_lowercase)
                    .unwrap_or_default();
                q.split_whitespace()
                    .any(|word| title_lower.contains(&word.to_lowercase()))
            });

            if !title_contains_any_word {
                let thread_path = threads_dir().join(format!("{}.jsonl", &summary.id));
                if !grep_file_has_match(&mut grep_searcher, matcher, &thread_path) {
                    continue;
                }
            }
        }

        // When date filters are active, derive activity_at from event
        // timestamps (accurate); otherwise use the cheap file modified time.
        let activity_at = if has_date_filters(options) {
            let events = load_thread_events(&summary.id).unwrap_or_default();
            latest_event_timestamp(&events).or_else(|| summary.modified.map(DateTime::<Utc>::from))
        } else {
            summary.modified.map(DateTime::<Utc>::from)
        };

        if !matches_thread_date_filters(activity_at.as_ref(), options) {
            continue;
        }

        // Build preview from first assistant message (loads events only for
        // matched threads that survived the filter).
        let preview = build_thread_preview_simple(&summary);

        let result = ThreadSearchResult {
            thread_id: summary.id,
            title: summary.title,
            root_path: summary.root_path,
            activity_at: activity_at
                .as_ref()
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            preview,
        };
        results.push(result);

        // Early termination — list is already sorted by recency.
        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}

/// Searches tool calls across saved threads.
///
/// Results are ordered by tool-call timestamp descending.
///
/// # Errors
/// Returns an error if thread listing fails.
pub fn search_thread_tools(options: &ThreadToolSearchOptions) -> Result<Vec<ThreadToolMatch>> {
    let limit = options.limit.max(1);
    let normalized_tool = options
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase);

    let mut matches = Vec::new();

    for summary in list_threads()? {
        let events = load_thread_events(&summary.id).unwrap_or_default();
        let mut pending: HashMap<String, PendingToolUse> = HashMap::new();

        for event in events {
            match event {
                ThreadEvent::ToolUse {
                    id,
                    name,
                    input,
                    ts,
                    ..
                } => {
                    if !matches_tool_name_filter(&name, normalized_tool.as_deref()) {
                        continue;
                    }
                    if !matches_tool_date_filters(Some(&ts), options) {
                        continue;
                    }

                    pending.insert(
                        id.clone(),
                        PendingToolUse {
                            tool_use_id: id,
                            tool_name: name,
                            tool_ts: ts,
                            args_summary: summarize_tool_args(&input),
                        },
                    );
                }
                ThreadEvent::ToolResult {
                    tool_use_id,
                    output,
                    ok,
                    ..
                } => {
                    let Some(tool_use) = pending.remove(&tool_use_id) else {
                        continue;
                    };
                    if options.failed_only && ok {
                        continue;
                    }

                    let (error_code, error_message) = extract_tool_error(&output);
                    matches.push(ThreadToolMatch {
                        thread_id: summary.id.clone(),
                        title: summary.title.clone(),
                        tool_use_id: tool_use.tool_use_id,
                        tool_name: tool_use.tool_name,
                        tool_ts: Some(tool_use.tool_ts),
                        status: if ok {
                            "ok".to_string()
                        } else {
                            "failed".to_string()
                        },
                        args_summary: tool_use.args_summary,
                        error_code,
                        error_message,
                    });
                }
                _ => {}
            }
        }

        if !options.failed_only {
            for tool_use in pending.into_values() {
                matches.push(ThreadToolMatch {
                    thread_id: summary.id.clone(),
                    title: summary.title.clone(),
                    tool_use_id: tool_use.tool_use_id,
                    tool_name: tool_use.tool_name,
                    tool_ts: Some(tool_use.tool_ts),
                    status: "pending".to_string(),
                    args_summary: tool_use.args_summary,
                    error_code: None,
                    error_message: None,
                });
            }
        }
    }

    matches.sort_by(|a, b| b.tool_ts.cmp(&a.tool_ts));
    if matches.len() > limit {
        matches.truncate(limit);
    }

    Ok(matches)
}

fn has_date_filters(options: &ThreadSearchOptions) -> bool {
    options.date.is_some() || options.date_start.is_some() || options.date_end.is_some()
}

fn has_tool_date_filters(options: &ThreadToolSearchOptions) -> bool {
    options.date.is_some() || options.date_start.is_some() || options.date_end.is_some()
}

fn matches_tool_name_filter(tool_name: &str, expected: Option<&str>) -> bool {
    expected.is_none_or(|expected| tool_name.eq_ignore_ascii_case(expected))
}

fn matches_tool_date_filters(raw_ts: Option<&str>, options: &ThreadToolSearchOptions) -> bool {
    if !has_tool_date_filters(options) {
        return true;
    }

    let Some(raw_ts) = raw_ts else {
        return false;
    };

    let Ok(activity_at) = DateTime::parse_from_rfc3339(raw_ts) else {
        return false;
    };
    let activity_date = activity_at.with_timezone(&Utc).date_naive();

    if let Some(date) = options.date
        && activity_date != date
    {
        return false;
    }
    if let Some(start) = options.date_start
        && activity_date < start
    {
        return false;
    }
    if let Some(end) = options.date_end
        && activity_date > end
    {
        return false;
    }

    true
}

fn summarize_tool_args(input: &Value) -> String {
    if let Some(command) = input.get("command").and_then(Value::as_str) {
        return truncate_with_ellipsis(command, TOOL_ARGS_SUMMARY_MAX_BYTES);
    }

    let compact = serde_json::to_string(input).unwrap_or_default();
    if compact.is_empty() {
        "{}".to_string()
    } else {
        truncate_with_ellipsis(&compact, TOOL_ARGS_SUMMARY_MAX_BYTES)
    }
}

fn truncate_with_ellipsis(value: &str, max_bytes: usize) -> String {
    if value.len() > max_bytes {
        format!("{}...", truncate_str(value, max_bytes))
    } else {
        value.to_string()
    }
}

fn extract_tool_error(output: &Value) -> (Option<String>, Option<String>) {
    let error = output.get("error");
    let code = error
        .and_then(|err| err.get("code"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let message = error
        .and_then(|err| err.get("message"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    (code, message)
}

/// Returns the latest RFC-3339 timestamp found across all events in a thread.
fn latest_event_timestamp(events: &[ThreadEvent]) -> Option<DateTime<Utc>> {
    events
        .iter()
        .filter_map(|event| {
            let ts = match event {
                ThreadEvent::Meta { ts, .. }
                | ThreadEvent::Message { ts, .. }
                | ThreadEvent::ToolUse { ts, .. }
                | ThreadEvent::ToolResult { ts, .. }
                | ThreadEvent::Interrupted { ts, .. }
                | ThreadEvent::Reasoning { ts, .. }
                | ThreadEvent::Usage { ts, .. }
                | ThreadEvent::Notice { ts, .. } => ts,
            };
            DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        })
        .max()
}

/// Builds a case-insensitive grep matcher from query words (compiled once,
/// reused across all thread files). Uses `fixed_strings` mode so words are
/// matched literally without regex escaping.
fn build_grep_matcher(query: &str) -> Option<grep_regex::RegexMatcher> {
    let words: Vec<&str> = query.split_whitespace().filter(|w| !w.is_empty()).collect();

    if words.is_empty() {
        return None;
    }

    grep_regex::RegexMatcherBuilder::new()
        .case_insensitive(true)
        .fixed_strings(true)
        .build_literals(&words)
        .ok()
}

/// Returns `true` if the file at `path` contains at least one line matching the
/// pre-compiled matcher. Uses grep-searcher for fast, SIMD-optimised scanning
/// over raw bytes — much cheaper than deserialising every JSONL event.
fn grep_file_has_match(
    searcher: &mut grep_searcher::Searcher,
    matcher: &grep_regex::RegexMatcher,
    path: &Path,
) -> bool {
    use grep_searcher::sinks::Bytes;

    let mut found = false;
    let _ = searcher.search_path(
        matcher,
        path,
        Bytes(|_line_num, _bytes| {
            found = true;
            Ok(false) // stop on first match
        }),
    );

    found
}

fn matches_thread_date_filters(
    activity_at: Option<&DateTime<Utc>>,
    options: &ThreadSearchOptions,
) -> bool {
    if !has_date_filters(options) {
        return true;
    }

    let Some(activity_at) = activity_at else {
        return false;
    };
    let activity_date = activity_at.date_naive();

    if let Some(date) = options.date
        && activity_date != date
    {
        return false;
    }
    if let Some(start) = options.date_start
        && activity_date < start
    {
        return false;
    }
    if let Some(end) = options.date_end
        && activity_date > end
    {
        return false;
    }

    true
}

/// Returns a short preview string for a thread: first assistant message, or title fallback.
fn build_thread_preview_simple(summary: &ThreadSummary) -> String {
    let events = load_thread_events(&summary.id).unwrap_or_default();

    for event in &events {
        if let ThreadEvent::Message { role, text, .. } = event
            && role == "assistant"
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return truncate_preview(trimmed);
            }
        }
    }

    summary
        .title
        .as_deref()
        .map(truncate_preview)
        .unwrap_or_default()
}

fn truncate_preview(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.len() > SEARCH_PREVIEW_MAX_BYTES {
        format!("{}...", truncate_str(trimmed, SEARCH_PREVIEW_MAX_BYTES))
    } else {
        trimmed.to_string()
    }
}
