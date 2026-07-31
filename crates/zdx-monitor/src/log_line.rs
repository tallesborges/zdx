//! Parsing and filtering of `tracing` compact-format log lines.
//!
//! Shared by the Logs tab renderer (`ui`) and the Logs filter state (`app`).

/// Log level, ordered so `>=` expresses a minimum-severity filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn from_token(token: &str) -> Option<Self> {
        match token {
            "TRACE" => Some(Self::Trace),
            "DEBUG" => Some(Self::Debug),
            "INFO" => Some(Self::Info),
            "WARN" => Some(Self::Warn),
            "ERROR" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Minimum-severity filter for the Logs tab, cycled with `l`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LevelFilter {
    #[default]
    All,
    Debug,
    Info,
    Warn,
    Error,
}

impl LevelFilter {
    /// Cycle ALL → DEBUG+ → INFO+ → WARN+ → ERROR → ALL.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Debug,
            Self::Debug => Self::Info,
            Self::Info => Self::Warn,
            Self::Warn => Self::Error,
            Self::Error => Self::All,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Debug => "DEBUG+",
            Self::Info => "INFO+",
            Self::Warn => "WARN+",
            Self::Error => "ERROR",
        }
    }

    /// Lowest level this filter admits, or `None` when it admits everything.
    fn min_level(self) -> Option<LogLevel> {
        match self {
            Self::All => None,
            Self::Debug => Some(LogLevel::Debug),
            Self::Info => Some(LogLevel::Info),
            Self::Warn => Some(LogLevel::Warn),
            Self::Error => Some(LogLevel::Error),
        }
    }

    /// Whether a line at `level` passes this filter. Lines with no parseable
    /// level (panics, raw stderr spill) always pass so nothing goes missing.
    #[must_use]
    pub fn accepts(self, level: Option<LogLevel>) -> bool {
        match (self.min_level(), level) {
            (None, _) | (_, None) => true,
            (Some(min), Some(level)) => level >= min,
        }
    }
}

/// Components of a `tracing` compact-format log line.
///
/// The compact format is
/// `<timestamp> <LEVEL> <span1>:<span2>: <target>: <message> <span_fields>`,
/// where the span scope is a single whitespace token of colon-terminated span
/// names and is absent when the event was emitted outside any span.
pub struct LogParts<'a> {
    pub timestamp: &'a str,
    pub level: &'a str,
    /// Colon-terminated span scope (e.g. `run_turn:execute_tool:`), or empty.
    pub spans: &'a str,
    pub target: &'a str,
    pub message: &'a str,
    pub structured: bool,
}

impl LogParts<'_> {
    #[must_use]
    pub fn level_enum(&self) -> Option<LogLevel> {
        LogLevel::from_token(self.level)
    }
}

/// Split off the leading whitespace-delimited token, returning it and the
/// remainder (already trimmed of the separating whitespace).
fn next_token(input: &str) -> (&str, &str) {
    let input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    let (token, rest) = input.split_at(end);
    (token, rest.trim_start())
}

/// Whether a token looks like a multi-segment Rust module path
/// (`zdx_engine::core::agent:`). The `::` requirement is what tells a target
/// apart from the first word of a message, since both can end in `:` and both
/// can otherwise be plain identifiers (`failed:`). The tradeoff is that a
/// crate-root target (`zdx_cli:`) emitted from inside a span is read as the
/// target rather than as a span scope — the safer of the two misreads, since
/// mistaking a message word for the target would corrupt most lines.
fn is_module_path(token: &str) -> bool {
    let body = token.strip_suffix(':').unwrap_or(token);
    body.contains("::")
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':')
}

/// Split a compact log line into its components, tolerating an optional span
/// scope between the level and the target. Falls back to `structured = false`
/// when the line doesn't match the compact shape.
#[must_use]
pub fn parse_log_line(line: &str) -> LogParts<'_> {
    let (timestamp, rest) = next_token(line);
    let (level, rest) = next_token(rest);
    let (third, after_third) = next_token(rest);

    // Span names never contain `::`, so a third token that does is already the
    // target. Otherwise a module-path-shaped fourth token means the third was
    // the span scope.
    let (fourth, after_fourth) = next_token(after_third);
    let (spans, target, message) =
        if !third.contains("::") && fourth.ends_with(':') && is_module_path(fourth) {
            (third, fourth, after_fourth)
        } else {
            ("", third, after_third)
        };

    let structured = LogLevel::from_token(level).is_some() && target.ends_with(':');

    LogParts {
        timestamp,
        level,
        spans,
        target,
        message,
        structured,
    }
}

/// Whether a raw log line passes the level filter, an optional target prefix
/// filter, and a case-insensitive substring query. An empty query and a `None`
/// target match everything.
#[must_use]
pub fn line_matches(
    line: &str,
    level_filter: LevelFilter,
    query_lower: &str,
    target_filter: Option<&str>,
) -> bool {
    let parts = parse_log_line(line);
    if !level_filter.accepts(parts.level_enum()) {
        return false;
    }
    if let Some(prefix) = target_filter
        && !target_matches(parts.target, prefix)
    {
        return false;
    }
    query_lower.is_empty() || line.to_lowercase().contains(query_lower)
}

/// Target of a line without its trailing `:`, or `""` for unstructured lines.
#[must_use]
pub fn line_target(line: &str) -> &str {
    let parts = parse_log_line(line);
    if parts.structured {
        parts.target.strip_suffix(':').unwrap_or(parts.target)
    } else {
        ""
    }
}

/// Whether a target token matches a filter prefix, so `zdx_engine` selects
/// `zdx_engine::core::agent`. Matches on module-path segment boundaries, so
/// `zdx_engine` does not select a hypothetical `zdx_engine_extra`.
fn target_matches(target_token: &str, prefix: &str) -> bool {
    let target = target_token.strip_suffix(':').unwrap_or(target_token);
    if target == prefix {
        return true;
    }
    target
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_compact_line() {
        let parts = parse_log_line("2026-07-31T10:00:00.123456Z  INFO zdx_bot::bot: Accepted");
        assert!(parts.structured);
        assert_eq!(parts.level, "INFO");
        assert_eq!(parts.spans, "");
        assert_eq!(parts.target, "zdx_bot::bot:");
        assert_eq!(parts.message, "Accepted");
    }

    #[test]
    fn parses_single_span_prefix() {
        let parts = parse_log_line(
            "2026-07-31T10:00:00.123456Z DEBUG run_turn_inner: zdx_engine::core::agent: Turn start thread=abc",
        );
        assert!(parts.structured);
        assert_eq!(parts.spans, "run_turn_inner:");
        assert_eq!(parts.target, "zdx_engine::core::agent:");
        assert_eq!(parts.message, "Turn start thread=abc");
    }

    #[test]
    fn parses_nested_span_prefix() {
        let parts = parse_log_line(
            "2026-07-31T10:00:00Z  WARN run_turn_inner:execute_tool: zdx_engine::tools: Tool failed code=not_found",
        );
        assert!(parts.structured);
        assert_eq!(parts.spans, "run_turn_inner:execute_tool:");
        assert_eq!(parts.target, "zdx_engine::tools:");
        assert_eq!(parts.message, "Tool failed code=not_found");
    }

    #[test]
    fn message_starting_with_colon_word_is_not_mistaken_for_target() {
        let parts = parse_log_line("2026-07-31T10:00:00Z ERROR zdx_cli: failed: bad input");
        assert!(parts.structured);
        assert_eq!(parts.spans, "");
        assert_eq!(parts.target, "zdx_cli:");
        assert_eq!(parts.message, "failed: bad input");
    }

    #[test]
    fn span_scope_with_multi_segment_target_wins_over_message_word() {
        let parts =
            parse_log_line("2026-07-31T10:00:00Z  WARN execute_tool: zdx_engine::tools: failed:");
        assert_eq!(parts.spans, "execute_tool:");
        assert_eq!(parts.target, "zdx_engine::tools:");
        assert_eq!(parts.message, "failed:");
    }

    #[test]
    fn non_tracing_line_is_unstructured() {
        let parts = parse_log_line("thread 'main' panicked at src/main.rs:10");
        assert!(!parts.structured);
    }

    #[test]
    fn level_filter_cycles_and_accepts() {
        assert_eq!(LevelFilter::All.next(), LevelFilter::Debug);
        assert_eq!(LevelFilter::Error.next(), LevelFilter::default());

        let warn = LevelFilter::Warn;
        assert!(warn.accepts(Some(LogLevel::Error)));
        assert!(warn.accepts(Some(LogLevel::Warn)));
        assert!(!warn.accepts(Some(LogLevel::Info)));
        // Unparseable lines are never hidden.
        assert!(warn.accepts(None));
    }

    #[test]
    fn line_matches_combines_level_and_query() {
        let line = "2026-07-31T10:00:00Z  WARN zdx_engine::tools: Tool failed tool=Bash";
        assert!(line_matches(line, LevelFilter::All, "", None));
        assert!(line_matches(line, LevelFilter::Warn, "bash", None));
        assert!(!line_matches(line, LevelFilter::Error, "", None));
        assert!(!line_matches(line, LevelFilter::All, "glob", None));
    }

    #[test]
    fn target_filter_matches_on_segment_boundaries() {
        let line = "2026-07-31T10:00:00Z  WARN zdx_engine::tools: Tool failed";
        assert!(line_matches(line, LevelFilter::All, "", Some("zdx_engine")));
        assert!(line_matches(
            line,
            LevelFilter::All,
            "",
            Some("zdx_engine::tools")
        ));
        assert!(!line_matches(line, LevelFilter::All, "", Some("zdx_bot")));
        // Prefix must end on a `::` boundary, not mid-segment.
        assert!(!line_matches(line, LevelFilter::All, "", Some("zdx_eng")));
    }

    #[test]
    fn line_target_strips_colon_and_ignores_unstructured() {
        assert_eq!(
            line_target("2026-07-31T10:00:00Z  INFO zdx_bot::bot: hi"),
            "zdx_bot::bot"
        );
        assert_eq!(
            line_target("2026-07-31T10:00:00Z DEBUG execute_tool: zdx_engine::tools: hi"),
            "zdx_engine::tools"
        );
        assert_eq!(line_target("thread 'main' panicked"), "");
    }
}
