//! Display projection of one tool call.
//!
//! Turns a call's name, input, and raw output envelope into what a reader
//! needs: the command or target once, the real output by kind, an outcome,
//! short status facts, and warnings. Surfaces render this instead of digging
//! through the envelope; the raw input and output stay with the caller for
//! debugging.

use serde::Serialize;
use serde_json::Value;

use crate::events::ToolOutput;
use crate::tools::tool_command_text;

/// Longest text a display view carries per field.
pub const MAX_VIEW_TEXT_BYTES: usize = 64 * 1024;
/// Most list entries (matches, paths, diff lines) a display view carries.
pub const MAX_VIEW_ENTRIES: usize = 2000;

/// How much output a view may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewLimits {
    pub text_bytes: usize,
    pub entries: usize,
}

impl ViewLimits {
    /// For a surface that keeps the raw output elsewhere (the Mini App sends
    /// it next to the view). Longer output is cut with a note.
    pub const DISPLAY: Self = Self {
        text_bytes: MAX_VIEW_TEXT_BYTES,
        entries: MAX_VIEW_ENTRIES,
    };
    /// Everything, for a surface whose view is the only way to read the
    /// output (the terminal tool detail).
    pub const FULL: Self = Self {
        text_bytes: usize::MAX,
        entries: usize::MAX,
    };
}
/// Unchanged lines kept on each side of an edit's changed block.
const DIFF_CONTEXT_LINES: usize = 3;

/// What a reader sees for one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolView {
    pub outcome: ToolOutcome,
    /// The command or target, once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<ToolHead>,
    pub body: ToolBody,
    /// Short facts for the status line, in reading order.
    pub status: Vec<ToolFact>,
    /// One short fact for the collapsed row, when one helps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<ToolFact>,
    /// Warnings the reader must not miss: cut output, partial results,
    /// timeouts.
    pub notes: Vec<String>,
    /// Plain details too long for the status line, such as log paths.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub info: Vec<String>,
    /// Thread a delegated call ran in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// No output yet.
    Pending,
    Ok,
    /// The tool failed, or a command exited non-zero.
    Failed,
    TimedOut,
    Canceled,
}

/// One short fact, such as `exit 0` or `lines 1-40 of 225`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolFact {
    pub text: String,
    /// Marks a fact the reader has to act on (failure, timeout).
    pub bad: bool,
    pub kind: FactKind,
}

/// What a fact is about, so a surface that already shows a call's outcome or
/// timing (the trajectory span sheet) can leave those facts out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// How the call ended: `exit 0`, `timed out`, `failed`, `in background`.
    Outcome,
    /// The call's own duration.
    Duration,
    /// Anything else: sizes, ranges, counts, ids.
    Detail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolHead {
    /// A shell command. `label` is the `bash` call's `description` (short
    /// prose), when the call carried one; collapsed rows show it, the
    /// expanded head still shows `command`.
    Shell {
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        command: String,
    },
    Path {
        path: String,
    },
    Search {
        pattern: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    Prompt {
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        prompt: String,
    },
    Text {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolBody {
    /// Nothing beyond the head (no output yet).
    Empty,
    /// Process output; empty `streams` means the command printed nothing.
    Streams {
        streams: Vec<OutputStream>,
    },
    /// File content; `first_line` numbers the first line.
    Code {
        text: String,
        first_line: u64,
    },
    Matches {
        files: Vec<MatchFile>,
    },
    Paths {
        entries: Vec<PathEntry>,
    },
    Diff {
        lines: Vec<DiffLine>,
        additions: usize,
        deletions: usize,
    },
    Markdown {
        text: String,
    },
    Error {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },
    Text {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputStream {
    /// `stdout` or `stderr`.
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchFile {
    pub path: String,
    pub lines: Vec<MatchLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchLine {
    pub line: u64,
    pub text: String,
    /// A context line around a match, not a match.
    #[serde(skip_serializing_if = "is_false")]
    pub context: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathEntry {
    pub path: String,
    pub dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffOp {
    Add,
    Del,
    Ctx,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    pub op: DiffOp,
    pub text: String,
}

impl ToolView {
    /// Line counts of an edit or write, for a row summary.
    #[must_use]
    pub fn diff_stats(&self) -> Option<(usize, usize)> {
        match &self.body {
            ToolBody::Diff {
                additions,
                deletions,
                ..
            } => Some((*additions, *deletions)),
            _ => None,
        }
    }
}

/// A finished call's result, borrowed from either output shape.
#[derive(Clone, Copy)]
enum CallResult<'a> {
    Success(&'a Value),
    Failure {
        code: &'a str,
        message: &'a str,
        details: Option<&'a str>,
    },
    /// Not an envelope (older or synthetic results).
    Raw(&'a Value),
}

static NULL: Value = Value::Null;

/// Builds the view of one call from its raw result envelope (`{ok, data}` or
/// `{ok: false, error}`); `output` is `None` while the call runs.
#[must_use]
pub fn tool_view(
    name: &str,
    input: &Value,
    output: Option<&Value>,
    duration_ms: Option<u64>,
    limits: ViewLimits,
) -> ToolView {
    let result = output.map(|out| match out.get("ok").and_then(Value::as_bool) {
        Some(true) => CallResult::Success(out.get("data").unwrap_or(&NULL)),
        Some(false) => {
            let error = out.get("error").unwrap_or(&NULL);
            let text = |key| error.get(key).and_then(Value::as_str);
            CallResult::Failure {
                code: text("code").unwrap_or("unknown"),
                message: text("message").unwrap_or_default(),
                details: text("details"),
            }
        }
        None => CallResult::Raw(out),
    });
    build(name, input, result, duration_ms, limits)
}

/// Same as [`tool_view`], from a typed [`ToolOutput`] (no envelope copy).
#[must_use]
pub fn tool_output_view(
    name: &str,
    input: &Value,
    output: Option<&ToolOutput>,
    duration_ms: Option<u64>,
    limits: ViewLimits,
) -> ToolView {
    let result = output.map(|out| match out {
        ToolOutput::Success { data, .. } => CallResult::Success(data),
        ToolOutput::Failure { error, .. } => CallResult::Failure {
            code: &error.code,
            message: &error.message,
            details: error.details.as_deref(),
        },
        ToolOutput::Canceled { message } => CallResult::Failure {
            code: "canceled",
            message,
            details: None,
        },
    });
    build(name, input, result, duration_ms, limits)
}

fn build(
    name: &str,
    input: &Value,
    result: Option<CallResult<'_>>,
    duration_ms: Option<u64>,
    limits: ViewLimits,
) -> ToolView {
    let name = name.to_ascii_lowercase();
    let mut b = Builder::new(limits);
    let head = head_for(&name, input, &mut b);

    match result {
        None => {
            b.outcome = ToolOutcome::Pending;
            if let Some(body) = input_body(&name, input, None, &mut b) {
                b.body = body;
            }
        }
        Some(CallResult::Failure {
            code,
            message,
            details,
        }) => failure(code, message, details, &mut b),
        Some(CallResult::Success(data)) => {
            b.outcome = ToolOutcome::Ok;
            success(&name, input, data, &mut b);
        }
        Some(CallResult::Raw(out)) => {
            b.outcome = ToolOutcome::Ok;
            b.body = ToolBody::Text {
                text: b.cap(&display_json(out)),
            };
        }
    }

    if let Some(ms) = duration_ms {
        b.fact(FactKind::Duration, format_duration(ms), false);
    }
    if b.cut {
        b.notes.push(format!(
            "Shown in part: at most {} per text and {} rows here.",
            format_bytes(limits.text_bytes as u64),
            limits.entries
        ));
    }

    ToolView {
        outcome: b.outcome,
        head,
        body: b.body,
        status: b.status,
        hint: b.hint,
        notes: b.notes,
        info: b.info,
        thread_id: b.thread_id,
    }
}

struct Builder {
    outcome: ToolOutcome,
    body: ToolBody,
    status: Vec<ToolFact>,
    hint: Option<ToolFact>,
    notes: Vec<String>,
    info: Vec<String>,
    thread_id: Option<String>,
    limits: ViewLimits,
    cut: bool,
}

impl Builder {
    fn new(limits: ViewLimits) -> Self {
        Self {
            outcome: ToolOutcome::Pending,
            body: ToolBody::Empty,
            status: Vec::new(),
            hint: None,
            notes: Vec::new(),
            info: Vec::new(),
            thread_id: None,
            limits,
            cut: false,
        }
    }

    fn fact(&mut self, kind: FactKind, text: impl Into<String>, bad: bool) {
        self.status.push(ToolFact {
            text: text.into(),
            bad,
            kind,
        });
    }

    fn hint(&mut self, text: impl Into<String>, bad: bool) {
        self.hint = Some(ToolFact {
            text: text.into(),
            bad,
            kind: FactKind::Detail,
        });
    }

    fn cap(&mut self, text: &str) -> String {
        if text.len() <= self.limits.text_bytes {
            return text.to_string();
        }
        self.cut = true;
        let mut end = self.limits.text_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    }

    fn cap_entries<T>(&mut self, mut items: Vec<T>) -> Vec<T> {
        if items.len() > self.limits.entries {
            self.cut = true;
            items.truncate(self.limits.entries);
        }
        items
    }

    fn tool_warning(&mut self, data: &Value) {
        if let Some(warning) = str_field(data, "warning") {
            self.notes.push(warning.to_string());
        }
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    match value.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(item)) if !item.trim().is_empty() => vec![item.clone()],
        _ => Vec::new(),
    }
}

fn display_json(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// `62ms`, `1.4s`, `2m 5s`.
#[must_use]
pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let secs = ms / 1000;
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// `81 B`, `4.2 KB`, `1.3 MB`.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Splits a leading `cd <dir> &&` off a shell command, so the directory
/// shows once. Only when every part of the rest runs in that directory: a
/// top-level `;`, `||`, `&`, or newline in the rest can run after a failed
/// `cd`, so those commands stay whole.
fn split_cd(command: &str) -> (Option<String>, String) {
    let trimmed = command.trim();
    let whole = || (None, trimmed.to_string());
    let Some(rest) = trimmed.strip_prefix("cd ") else {
        return whole();
    };
    let rest = rest.trim_start();
    let (dir, after) = if let Some(quote @ ('"' | '\'')) = rest.chars().next() {
        match rest[1..].find(quote) {
            Some(end) => (&rest[1..=end], &rest[end + 2..]),
            None => return whole(),
        }
    } else {
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|'))
            .unwrap_or(rest.len());
        (&rest[..end], &rest[end..])
    };
    let Some(command) = after.trim_start().strip_prefix("&&").map(str::trim) else {
        return whole();
    };
    if dir.is_empty() || dir == "-" || command.is_empty() || !runs_only_after_success(command) {
        return whole();
    }
    (Some(dir.to_string()), command.to_string())
}

/// True when `command` has no top-level `;`, `||`, background `&`, or
/// newline: then all of it is skipped when the `cd` before it fails.
fn runs_only_after_success(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == b'\\' && q == b'"' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\\' => i += 1,
                b'\'' | b'"' => quote = Some(c),
                b'|' if bytes.get(i + 1) == Some(&b'|') => return false,
                b'&' if bytes.get(i + 1) == Some(&b'&') => i += 1,
                // `>&` and `&>` are redirections, not background jobs.
                b'&' if (i > 0 && bytes[i - 1] == b'>') || bytes.get(i + 1) == Some(&b'>') => {}
                b';' | b'\n' | b'&' => return false,
                _ => {}
            },
        }
        i += 1;
    }
    true
}

fn head_for(name: &str, input: &Value, b: &mut Builder) -> Option<ToolHead> {
    match name {
        "bash" => {
            let (cwd, command) = split_cd(str_field(input, "command")?);
            // The model writes a short label (3-8 words) alongside the
            // command; surfaces prefer it for collapsed rows. Collapse and
            // bound it so one row stays one line.
            let label = str_field(input, "description").map(|label| {
                let flat = label.split_whitespace().collect::<Vec<_>>().join(" ");
                if flat.chars().count() > 120 {
                    let mut truncated: String = flat.chars().take(119).collect();
                    truncated.push('…');
                    truncated
                } else {
                    flat
                }
            });
            Some(ToolHead::Shell {
                cwd,
                label,
                command: b.cap(&command),
            })
        }
        "read" | "write" | "edit" => {
            let path = str_field(input, "file_path").or_else(|| str_field(input, "path"))?;
            Some(ToolHead::Path {
                path: path.to_string(),
            })
        }
        "grep" | "glob" => {
            let pattern = string_list(input, "pattern").join(", ");
            if pattern.is_empty() {
                return None;
            }
            let mut scope = string_list(input, "paths");
            if let Some(path) = str_field(input, "path") {
                scope.push(path.to_string());
            }
            Some(ToolHead::Search {
                pattern,
                scope: (!scope.is_empty()).then(|| scope.join(" ")),
            })
        }
        "invoke_subagent" => Some(ToolHead::Prompt {
            agent: str_field(input, "subagent").map(str::to_string),
            prompt: b.cap(str_field(input, "prompt")?),
        }),
        _ => {
            let text = tool_command_text(name, input);
            (!text.is_empty()).then(|| ToolHead::Text { text: b.cap(&text) })
        }
    }
}

/// Bodies known from the input alone, so a running edit already shows its
/// change. `created` is a write's result (`None` while it runs).
fn input_body(
    name: &str,
    input: &Value,
    created: Option<bool>,
    b: &mut Builder,
) -> Option<ToolBody> {
    let text = |key| input.get(key).and_then(Value::as_str);
    match name {
        "edit" => {
            let (old, new) = edit_lines(
                text("old_string").unwrap_or_default(),
                text("new_string").unwrap_or_default(),
            );
            Some(diff_body(&old, &new, b))
        }
        // Only a new file has a known "before"; an overwrite has no old
        // content here, so it shows what was written, not a diff.
        "write" if created == Some(true) => {
            let content: Vec<&str> = text("content")?.split_inclusive('\n').collect();
            Some(diff_body(&[], &content, b))
        }
        "write" => Some(ToolBody::Code {
            text: b.cap(text("content")?),
            first_line: 1,
        }),
        _ => None,
    }
}

fn failure(code: &str, message: &str, details: Option<&str>, b: &mut Builder) {
    let code = code.to_string();
    let message = b.cap(message);
    let details = details.filter(|d| !d.trim().is_empty()).map(|d| b.cap(d));
    if code == "canceled" {
        b.outcome = ToolOutcome::Canceled;
        b.fact(FactKind::Outcome, "canceled", true);
        b.hint("canceled", true);
    } else {
        b.outcome = ToolOutcome::Failed;
        b.fact(FactKind::Outcome, "failed", true);
        b.hint(code.clone(), true);
    }
    b.body = ToolBody::Error {
        code,
        message,
        details,
    };
}

fn success(name: &str, input: &Value, data: &Value, b: &mut Builder) {
    let has_streams = ["stdout", "stderr"]
        .iter()
        .any(|key| data.get(*key).is_some_and(Value::is_string));
    match name {
        "bash" => streams(data, b),
        "read" if data.get("content").and_then(Value::as_str).is_some() => read(data, b),
        "grep" => grep(data, b),
        "glob" => glob(data, b),
        "edit" => {
            if let Some(body) = input_body(name, input, None, b) {
                b.body = body;
            }
            if let Some(count) = u64_field(data, "replacements") {
                b.fact(
                    FactKind::Detail,
                    plural(count as usize, "replacement", "replacements"),
                    false,
                );
            }
        }
        "write" => {
            if let Some(body) = input_body(name, input, Some(flag(data, "created")), b) {
                b.body = body;
            }
            b.fact(
                FactKind::Detail,
                if flag(data, "created") {
                    "created"
                } else {
                    "overwritten"
                },
                false,
            );
            if let Some(bytes) = u64_field(data, "bytes") {
                b.fact(FactKind::Detail, format_bytes(bytes), false);
            }
        }
        "invoke_subagent"
            if data.get("bg_id").is_none()
                && data.get("result").and_then(Value::as_str).is_some() =>
        {
            let result = data.get("result").and_then(Value::as_str).unwrap_or("");
            b.body = ToolBody::Markdown {
                text: b.cap(result),
            };
            b.thread_id = str_field(data, "child_thread_id").map(str::to_string);
        }
        // Any other process output (`background_output`, `skill_exec`, a
        // result whose call is unknown) reads like a command's.
        _ if has_streams => streams(data, b),
        _ => {
            b.body = ToolBody::Text {
                text: b.cap(&display_json(data)),
            };
            b.tool_warning(data);
        }
    }
}

/// Process output: stdout and stderr verbatim (surfaces drop the final
/// newline when they print), then exit code, timeout, background state.
fn streams(data: &Value, b: &mut Builder) {
    let mut streams = Vec::new();
    let mut lines = 0;
    for name in ["stdout", "stderr"] {
        let text = data.get(name).and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let text = b.cap(text);
        lines += text.lines().count();
        streams.push(OutputStream {
            name: name.to_string(),
            text,
        });
    }
    b.body = ToolBody::Streams { streams };

    let truncated = flag(data, "stdout_truncated") || flag(data, "stderr_truncated");
    // A command started in the background (`background: true` has no stream
    // text at all) or moved there after its foreground bound (`backgrounded`).
    let in_background =
        flag(data, "backgrounded") || (data.get("bg_id").is_some() && data.get("stdout").is_none());
    if in_background {
        background(data, b);
    } else if flag(data, "timed_out") {
        b.outcome = ToolOutcome::TimedOut;
        b.fact(FactKind::Outcome, "timed out", true);
        b.hint("timed out", true);
        b.notes
            .push("The command timed out and was stopped. Its output is partial.".to_string());
    } else {
        // `background_output` reports whether the job is still running.
        if let Some(status) = str_field(data, "status") {
            b.fact(FactKind::Detail, status.to_string(), false);
        }
        match data.get("exit_code").and_then(Value::as_i64) {
            Some(0) => b.fact(FactKind::Outcome, "exit 0", false),
            Some(code) => {
                b.outcome = ToolOutcome::Failed;
                b.fact(FactKind::Outcome, format!("exit {code}"), true);
                b.hint(format!("exit {code}"), true);
            }
            None => {}
        }
        if b.hint.is_none() {
            let mut hint = if lines == 0 {
                "no output".to_string()
            } else {
                plural(lines, "line", "lines")
            };
            if truncated {
                hint.push_str(" · cut");
            }
            b.hint(hint, false);
        }
    }

    // A background job's byte counts cover only what came before the move,
    // so they would read as a final size.
    let total = u64_field(data, "stdout_total_bytes").unwrap_or(0)
        + u64_field(data, "stderr_total_bytes").unwrap_or(0);
    if !in_background
        && (data.get("stdout_total_bytes").is_some() || data.get("stderr_total_bytes").is_some())
    {
        b.fact(FactKind::Detail, format_bytes(total), false);
    }

    for name in ["stdout", "stderr"] {
        if !flag(data, &format!("{name}_truncated")) {
            continue;
        }
        let size = format_bytes(u64_field(data, &format!("{name}_total_bytes")).unwrap_or(0));
        let note = match str_field(data, &format!("{name}_file")) {
            Some(file) => format!("Only the start of {size} {name} is kept. Full {name}: {file}"),
            None => format!("Only the start of {size} {name} is kept."),
        };
        b.notes.push(note);
    }
}

/// A job running in the background: its id, pid, logs, and anything that
/// went wrong while handing it off.
fn background(data: &Value, b: &mut Builder) {
    b.fact(FactKind::Outcome, "in background", false);
    if let Some(id) = str_field(data, "bg_id") {
        b.fact(FactKind::Detail, id.to_string(), false);
    }
    if let Some(pid) = u64_field(data, "pid") {
        b.fact(FactKind::Detail, format!("pid {pid}"), false);
    }
    b.hint("in background", false);
    if let Some(secs) = u64_field(data, "elapsed_secs") {
        b.info.push(format!(
            "Still running after {secs}s, so it was moved to the background."
        ));
    }
    let logs: Vec<String> = ["stdout", "stderr"]
        .iter()
        .filter_map(|name| {
            str_field(data, &format!("{name}_log")).map(|path| format!("{name} {path}"))
        })
        .collect();
    if !logs.is_empty() {
        b.info.push(format!("Logs: {}", logs.join(", ")));
    }
    if flag(data, "tracking_failed") {
        b.notes.push(
            "It may be missing from the background registry, so background_output and \
             background_kill may not find it. It keeps running."
                .to_string(),
        );
    }
    if flag(data, "logging_failed") {
        b.notes.push(
            "Its output logs could not be opened, so later output is not captured.".to_string(),
        );
    }
}

fn read(data: &Value, b: &mut Builder) {
    let content = data.get("content").and_then(Value::as_str).unwrap_or("");
    let first_line = u64_field(data, "offset").unwrap_or(1).max(1);
    let shown = u64_field(data, "lines_shown").unwrap_or(content.lines().count() as u64);
    let total = u64_field(data, "total_lines").unwrap_or(shown);
    let range = if shown == 0 && first_line > total {
        format!(
            "line {first_line} is past the end ({})",
            plural(total as usize, "line", "lines")
        )
    } else if first_line > 1 || shown < total {
        let last = (first_line + shown).saturating_sub(1);
        format!("lines {first_line}-{last} of {total}")
    } else {
        plural(total as usize, "line", "lines")
    };
    b.body = ToolBody::Code {
        text: b.cap(content),
        first_line,
    };
    b.fact(FactKind::Detail, range.clone(), false);
    b.hint(range, false);
    // The range is already in the status; only a cut line needs a note.
    if flag(data, "byte_limited") {
        b.tool_warning(data);
    }
}

fn grep(data: &Value, b: &mut Builder) {
    let more = if flag(data, "truncated") {
        " · more"
    } else {
        ""
    };
    if let Some(matches) = data.get("matches").and_then(Value::as_array) {
        let files = match_files(matches, b);
        let count = u64_field(data, "total_matches").unwrap_or(matches.len() as u64) as usize;
        // Every file in the result, not only those that fit the display.
        let file_count = matches
            .iter()
            .filter_map(|m| str_field(m, "file"))
            .collect::<std::collections::HashSet<_>>()
            .len();
        b.fact(
            FactKind::Detail,
            format!(
                "{} in {}",
                plural(count, "match", "matches"),
                plural(file_count, "file", "files")
            ),
            false,
        );
        b.hint(
            format!("{}{more}", plural(count, "match", "matches")),
            false,
        );
        b.body = ToolBody::Matches { files };
    } else if data.get("files").is_some() {
        let files = string_list(data, "files");
        let count = files.len();
        let entries = files
            .into_iter()
            .map(|path| PathEntry { path, dir: false })
            .collect();
        b.body = ToolBody::Paths {
            entries: b.cap_entries(entries),
        };
        b.fact(FactKind::Detail, plural(count, "file", "files"), false);
        b.hint(format!("{}{more}", plural(count, "file", "files")), false);
    } else if data.get("values").is_some() {
        let values = string_list(data, "values");
        let count = values.len();
        b.body = ToolBody::Text {
            text: b.cap(&values.join("\n")),
        };
        b.fact(FactKind::Detail, plural(count, "value", "values"), false);
        b.hint(format!("{}{more}", plural(count, "value", "values")), false);
    } else {
        b.body = ToolBody::Text {
            text: b.cap(&display_json(data)),
        };
    }
    b.tool_warning(data);
}

/// Matches grouped by file, with their context lines, within the row budget.
fn match_files(matches: &[Value], b: &mut Builder) -> Vec<MatchFile> {
    let mut files: Vec<MatchFile> = Vec::new();
    // Context lines are rows too, and the grep tool's byte cap does not
    // bound them (an empty line costs 0 bytes), so the budget counts rows.
    let mut rows_left = b.limits.entries;
    for m in matches {
        if rows_left == 0 {
            b.cut = true;
            break;
        }
        let path = str_field(m, "file").unwrap_or_default();
        let index = if let Some(index) = files.iter().position(|f| f.path == path) {
            index
        } else {
            files.push(MatchFile {
                path: path.to_string(),
                lines: Vec::new(),
            });
            files.len() - 1
        };
        let at = u64_field(m, "line_number").unwrap_or(0);
        let before = string_list(m, "context_before");
        let after = string_list(m, "context_after");
        let first = at.saturating_sub(before.len() as u64);
        let rows = before
            .into_iter()
            .enumerate()
            .map(|(i, text)| (first + i as u64, text, true))
            .chain(std::iter::once((
                at,
                m.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                false,
            )))
            .chain(
                after
                    .into_iter()
                    .enumerate()
                    .map(|(i, text)| (at + 1 + i as u64, text, true)),
            );
        for (line, text, context) in rows {
            if rows_left == 0 {
                b.cut = true;
                break;
            }
            if push_match_line(&mut files[index].lines, line, b.cap(&text), context) {
                rows_left -= 1;
            }
        }
    }
    files.retain(|file| !file.lines.is_empty());
    files
}

/// Adds a line in order and returns whether it added a row. Context from
/// neighbouring matches can overlap, so a line already shown is skipped, and
/// a match upgrades its own context row.
fn push_match_line(lines: &mut Vec<MatchLine>, line: u64, text: String, context: bool) -> bool {
    // Rows arrive in line order, so only the tail can hold this line; stop
    // there instead of scanning every row (a big result has ~22k).
    let existing = lines
        .iter_mut()
        .rev()
        .take_while(|l| l.line >= line)
        .find(|l| l.line == line);
    if let Some(existing) = existing {
        if !context {
            existing.context = false;
            existing.text = text;
        }
        return false;
    }
    lines.push(MatchLine {
        line,
        text,
        context,
    });
    true
}

fn glob(data: &Value, b: &mut Builder) {
    let dirs = string_list(data, "directories");
    let files = string_list(data, "files");
    let mut summary = plural(files.len(), "file", "files");
    if !dirs.is_empty() {
        summary = format!("{summary}, {}", plural(dirs.len(), "dir", "dirs"));
    }
    let entries = dirs
        .into_iter()
        .map(|path| PathEntry { path, dir: true })
        .chain(files.into_iter().map(|path| PathEntry { path, dir: false }))
        .collect();
    b.body = ToolBody::Paths {
        entries: b.cap_entries(entries),
    };
    let more = if flag(data, "truncated") {
        " · more"
    } else {
        ""
    };
    b.fact(FactKind::Detail, summary.clone(), false);
    b.hint(format!("{summary}{more}"), false);
    b.tool_warning(data);
}

/// Lines of an edit's strings. They are fragments inside a file, so a line
/// without its newline is still a line, and a newline both strings end with
/// is shared, not an extra empty line.
fn edit_lines<'a>(old: &'a str, new: &'a str) -> (Vec<&'a str>, Vec<&'a str>) {
    let (old, new) = match (old.strip_suffix('\n'), new.strip_suffix('\n')) {
        (Some(old), Some(new)) => (old, new),
        _ => (old, new),
    };
    (old.split('\n').collect(), new.split('\n').collect())
}

/// Line diff of one replaced block: common leading and trailing lines are
/// context, everything between is removed then added. An edit replaces one
/// contiguous string, so this is exact for its line counts. Rows drop line
/// terminators for display; the diff uses the full strings, and only its
/// rows are capped.
fn diff_body(old: &[&str], new: &[&str], b: &mut Builder) -> ToolBody {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let removed = &old[prefix..old.len() - suffix];
    let added = &new[prefix..new.len() - suffix];
    let tail = old.len() - suffix;

    let rows = old[prefix.saturating_sub(DIFF_CONTEXT_LINES)..prefix]
        .iter()
        .map(|text| (DiffOp::Ctx, *text))
        .chain(removed.iter().map(|text| (DiffOp::Del, *text)))
        .chain(added.iter().map(|text| (DiffOp::Add, *text)))
        .chain(
            old[tail..(tail + DIFF_CONTEXT_LINES).min(old.len())]
                .iter()
                .map(|text| (DiffOp::Ctx, *text)),
        );
    let mut lines = Vec::new();
    for (op, text) in rows {
        if lines.len() == b.limits.entries {
            b.cut = true;
            break;
        }
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        lines.push(DiffLine {
            op,
            text: b.cap(text),
        });
    }

    ToolBody::Diff {
        lines,
        additions: added.len(),
        deletions: removed.len(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn facts(view: &ToolView) -> Vec<&str> {
        view.status.iter().map(|f| f.text.as_str()).collect()
    }

    #[test]
    fn bash_head_carries_the_description_label() {
        let input = json!({"command": "cargo test", "description": "Run the test suite"});
        let view = tool_view("bash", &input, None, None, ViewLimits::DISPLAY);
        assert_eq!(
            view.head,
            Some(ToolHead::Shell {
                cwd: None,
                label: Some("Run the test suite".into()),
                command: "cargo test".into()
            })
        );

        // Old calls without a description (and a blank one) carry no label.
        let view = tool_view(
            "bash",
            &json!({"command": "ls"}),
            None,
            None,
            ViewLimits::DISPLAY,
        );
        assert!(matches!(
            view.head,
            Some(ToolHead::Shell { label: None, .. })
        ));
        let view = tool_view(
            "bash",
            &json!({"command": "ls", "description": "   "}),
            None,
            None,
            ViewLimits::DISPLAY,
        );
        assert!(matches!(
            view.head,
            Some(ToolHead::Shell { label: None, .. })
        ));
    }

    #[test]
    fn bash_shows_stdout_not_the_envelope() {
        let input = json!({"command": "cd /repo/app && grep -n setsid run.sh"});
        let output = json!({"ok": true, "data": {
            "exit_code": 0, "stderr": "", "stderr_total_bytes": 0, "stderr_truncated": false,
            "stdout": "54:  setsid sh \"$SELF\" &\n", "stdout_total_bytes": 81,
            "stdout_truncated": false, "timed_out": false
        }});
        let view = tool_view("bash", &input, Some(&output), Some(62), ViewLimits::DISPLAY);

        assert_eq!(view.outcome, ToolOutcome::Ok);
        assert_eq!(
            view.head,
            Some(ToolHead::Shell {
                cwd: Some("/repo/app".into()),
                label: None,
                command: "grep -n setsid run.sh".into()
            })
        );
        assert_eq!(
            view.body,
            ToolBody::Streams {
                streams: vec![OutputStream {
                    name: "stdout".into(),
                    text: "54:  setsid sh \"$SELF\" &\n".into()
                }]
            }
        );
        assert_eq!(facts(&view), ["exit 0", "81 B", "62ms"]);
        assert_eq!(view.hint.unwrap().text, "1 line");
        assert!(view.notes.is_empty());
    }

    #[test]
    fn bash_timeout_is_its_own_outcome() {
        let output = json!({"ok": true, "data": {
            "exit_code": -1, "timed_out": true, "stdout": "partial\n", "stderr": "",
            "stdout_total_bytes": 8, "stderr_total_bytes": 0
        }});
        let view = tool_view(
            "bash",
            &json!({"command": "sleep 99"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );

        assert_eq!(view.outcome, ToolOutcome::TimedOut);
        assert_eq!(
            view.hint,
            Some(ToolFact {
                text: "timed out".into(),
                bad: true,
                kind: FactKind::Detail
            })
        );
        assert!(!facts(&view).iter().any(|f| f.starts_with("exit")));
        assert_eq!(view.notes.len(), 1);
    }

    #[test]
    fn bash_failure_and_truncation_are_reported() {
        let output = json!({"ok": true, "data": {
            "exit_code": 101, "timed_out": false, "stdout": "", "stderr": "error: boom\n",
            "stdout_truncated": true, "stdout_total_bytes": 2_000_000,
            "stdout_file": "/tmp/out.log", "stderr_total_bytes": 12
        }});
        let view = tool_view(
            "bash",
            &json!({"command": "cargo test"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );

        assert_eq!(view.outcome, ToolOutcome::Failed);
        assert_eq!(view.hint.as_ref().unwrap().text, "exit 101");
        assert!(view.status[0].bad);
        assert_eq!(
            view.notes,
            ["Only the start of 1.9 MB stdout is kept. Full stdout: /tmp/out.log"]
        );
    }

    #[test]
    fn one_line_edit_counts_one_removed_and_one_added() {
        let input = json!({"file_path": "src/a.rs", "old_string": "a\nlet x = 1;\nb", "new_string": "a\nlet x = 2;\nb"});
        let output = json!({"ok": true, "data": {"replacements": 1}});
        let view = tool_view("edit", &input, Some(&output), None, ViewLimits::DISPLAY);

        assert_eq!(view.diff_stats(), Some((1, 1)));
        let ops: Vec<DiffOp> = match &view.body {
            ToolBody::Diff { lines, .. } => lines.iter().map(|l| l.op).collect(),
            other => panic!("unexpected body {other:?}"),
        };
        assert_eq!(ops, [DiffOp::Ctx, DiffOp::Del, DiffOp::Add, DiffOp::Ctx]);
        assert_eq!(facts(&view), ["1 replacement"]);
    }

    #[test]
    fn running_edit_already_has_its_diff() {
        let input = json!({"file_path": "a", "old_string": "x", "new_string": "x\ny"});
        let view = tool_view("edit", &input, None, None, ViewLimits::DISPLAY);
        assert_eq!(view.outcome, ToolOutcome::Pending);
        assert_eq!(view.diff_stats(), Some((1, 0)));
    }

    #[test]
    fn failure_envelope_becomes_an_error_body() {
        let output =
            json!({"ok": false, "error": {"code": "not_found", "message": "No such file"}});
        let view = tool_view(
            "read",
            &json!({"file_path": "x"}),
            Some(&output),
            Some(5),
            ViewLimits::DISPLAY,
        );

        assert_eq!(view.outcome, ToolOutcome::Failed);
        assert_eq!(
            view.body,
            ToolBody::Error {
                code: "not_found".into(),
                message: "No such file".into(),
                details: None
            }
        );
        assert_eq!(facts(&view), ["failed", "5ms"]);

        let canceled =
            json!({"ok": false, "error": {"code": "canceled", "message": "Interrupted"}});
        let view = tool_view(
            "bash",
            &json!({}),
            Some(&canceled),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(view.outcome, ToolOutcome::Canceled);
    }

    #[test]
    fn read_shows_content_with_its_range() {
        let output = json!({"ok": true, "data": {
            "content": "x\ny\n", "offset": 70, "lines_shown": 2, "total_lines": 225, "truncated": true,
            "byte_limited": true, "warning": "A long line was cut."
        }});
        let view = tool_view(
            "read",
            &json!({"file_path": "a.rs"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(
            view.body,
            ToolBody::Code {
                text: "x\ny\n".into(),
                first_line: 70
            }
        );
        assert_eq!(facts(&view), ["lines 70-71 of 225"]);
        assert_eq!(view.notes, ["A long line was cut."]);
    }

    #[test]
    fn grep_groups_matches_by_file() {
        let output = json!({"ok": true, "data": {
            "matches": [
                {"file": "a.rs", "line_number": 1, "text": "foo"},
                {"file": "b.rs", "line_number": 9, "text": "foo()"},
                {"file": "a.rs", "line_number": 4, "text": "foo bar"}
            ],
            "total_matches": 3, "truncated": true
        }});
        let view = tool_view(
            "grep",
            &json!({"pattern": "foo"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        let ToolBody::Matches { files } = &view.body else {
            panic!("unexpected body {:?}", view.body);
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].lines.len(), 2);
        assert_eq!(facts(&view), ["3 matches in 2 files"]);
        assert_eq!(view.hint.unwrap().text, "3 matches · more");
    }

    #[test]
    fn grep_context_lines_are_marked_and_not_repeated() {
        let output = json!({"ok": true, "data": {
            "matches": [
                {"file": "a.rs", "line_number": 3, "text": "m1", "context_before": ["c1", "c2"], "context_after": ["c4"]},
                {"file": "a.rs", "line_number": 4, "text": "m2", "context_after": ["c5"]}
            ],
            "total_matches": 2, "truncated": false
        }});
        let view = tool_view(
            "grep",
            &json!({"pattern": "m"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        let ToolBody::Matches { files } = &view.body else {
            panic!("unexpected body {:?}", view.body);
        };
        let rows: Vec<(u64, &str, bool)> = files[0]
            .lines
            .iter()
            .map(|l| (l.line, l.text.as_str(), l.context))
            .collect();
        assert_eq!(
            rows,
            [
                (1, "c1", true),
                (2, "c2", true),
                (3, "m1", false),
                (4, "m2", false),
                (5, "c5", true)
            ]
        );
    }

    #[test]
    fn typed_output_matches_the_envelope() {
        let input = json!({"command": "ls"});
        let typed = ToolOutput::success(json!({"stdout": "a\n", "exit_code": 0}));
        let envelope = serde_json::to_value(&typed).unwrap();
        assert_eq!(
            tool_output_view("bash", &input, Some(&typed), None, ViewLimits::DISPLAY),
            tool_view("bash", &input, Some(&envelope), None, ViewLimits::DISPLAY)
        );

        let canceled = ToolOutput::canceled("Interrupted");
        let view = tool_output_view("bash", &input, Some(&canceled), None, ViewLimits::DISPLAY);
        assert_eq!(view.outcome, ToolOutcome::Canceled);
    }

    #[test]
    fn subagent_answer_is_markdown_with_its_thread() {
        let output = json!({"ok": true, "data": {"result": "Done.", "child_thread_id": "t-1"}});
        let input = json!({"subagent": "explorer", "prompt": "Look"});
        let view = tool_view(
            "Invoke_Subagent",
            &input,
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(
            view.body,
            ToolBody::Markdown {
                text: "Done.".into()
            }
        );
        assert_eq!(view.thread_id.as_deref(), Some("t-1"));
    }

    #[test]
    fn long_output_is_cut_with_a_note() {
        let big = "x".repeat(MAX_VIEW_TEXT_BYTES + 10);
        let output = json!({"ok": true, "data": {"stdout": big, "exit_code": 0}});
        let view = tool_view(
            "bash",
            &json!({"command": "yes"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        let ToolBody::Streams { streams } = &view.body else {
            panic!();
        };
        assert_eq!(streams[0].text.len(), MAX_VIEW_TEXT_BYTES);
        assert_eq!(view.notes.len(), 1);
    }

    #[test]
    fn full_limits_keep_output_past_the_display_cap() {
        let big = format!("{}tail", "x".repeat(MAX_VIEW_TEXT_BYTES + 10));
        let output = ToolOutput::success(json!({"stdout": big, "exit_code": 0}));
        let input = json!({"command": "yes"});

        let view = tool_output_view("bash", &input, Some(&output), None, ViewLimits::FULL);
        let ToolBody::Streams { streams } = &view.body else {
            panic!("unexpected body {:?}", view.body);
        };
        assert_eq!(streams[0].text, big);
        assert!(view.notes.is_empty());

        let many: Vec<String> = (0..MAX_VIEW_ENTRIES + 5).map(|i| format!("f{i}")).collect();
        let output = ToolOutput::success(json!({"files": many, "directories": []}));
        let view = tool_output_view(
            "glob",
            &json!({"pattern": "*"}),
            Some(&output),
            None,
            ViewLimits::FULL,
        );
        let ToolBody::Paths { entries } = &view.body else {
            panic!("unexpected body {:?}", view.body);
        };
        assert_eq!(entries.len(), MAX_VIEW_ENTRIES + 5);
    }

    fn body_of(view: &ToolView) -> &ToolBody {
        &view.body
    }

    #[test]
    fn any_process_output_reads_like_a_command() {
        // `background_output` and results whose call is unknown carry streams too.
        let output = json!({"ok": true, "data": {
            "bg_id": "bg-1", "status": "exited", "exit_code": 1,
            "stdout": "hello\n", "stderr": "boom\n"
        }});
        for name in ["background_output", ""] {
            let view = tool_view(name, &json!({}), Some(&output), None, ViewLimits::DISPLAY);
            assert_eq!(view.outcome, ToolOutcome::Failed, "{name}");
            assert_eq!(facts(&view), ["exited", "exit 1"], "{name}");
            let ToolBody::Streams { streams } = body_of(&view) else {
                panic!("{name}: unexpected body {:?}", view.body);
            };
            assert_eq!(streams.len(), 2);
        }
    }

    #[test]
    fn degraded_background_handoff_is_reported() {
        let output = json!({"ok": true, "data": {
            "backgrounded": true, "bg_id": "bg-2", "pid": 41, "status": "running",
            "exit_code": null, "timed_out": false, "stdout": "", "stderr": "",
            "tracking_failed": true, "logging_failed": true, "message": "moved"
        }});
        let view = tool_view(
            "bash",
            &json!({"command": "sleep 9"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(view.hint.as_ref().unwrap().text, "in background");
        assert_eq!(view.notes.len(), 2, "{:?}", view.notes);
        assert!(facts(&view).contains(&"pid 41"), "{:?}", view.status);
        assert!(view.notes[0].contains("registry"), "{:?}", view.notes);
        assert!(view.notes[1].contains("not captured"), "{:?}", view.notes);
    }

    #[test]
    fn write_is_a_diff_only_when_it_created_the_file() {
        let input = json!({"file_path": "a.txt", "content": "hello\n"});
        let created = json!({"ok": true, "data": {"created": true, "bytes": 6}});
        let view = tool_view("write", &input, Some(&created), None, ViewLimits::DISPLAY);
        assert_eq!(
            view.diff_stats(),
            Some((1, 0)),
            "one line, no phantom empty line"
        );

        let overwritten = json!({"ok": true, "data": {"created": false, "bytes": 6}});
        let view = tool_view(
            "write",
            &input,
            Some(&overwritten),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(view.diff_stats(), None);
        assert_eq!(
            view.body,
            ToolBody::Code {
                text: "hello\n".into(),
                first_line: 1
            }
        );
        let pending = tool_view("write", &input, None, None, ViewLimits::DISPLAY);
        assert_eq!(pending.diff_stats(), None, "creation unknown while running");

        let newline_only = json!({"file_path": "b.txt", "content": "\n"});
        let view = tool_view(
            "write",
            &newline_only,
            Some(&created),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(view.diff_stats(), Some((1, 0)));
    }

    #[test]
    fn diff_lines_drop_terminators_and_shared_final_newline() {
        let input = json!({"file_path": "a", "old_string": "a\r\nx = 1\r\n", "new_string": "a\r\nx = 2\r\n"});
        let view = tool_view("edit", &input, None, None, ViewLimits::DISPLAY);
        let ToolBody::Diff {
            lines,
            additions,
            deletions,
        } = body_of(&view)
        else {
            panic!("unexpected body {:?}", view.body);
        };
        assert_eq!((*additions, *deletions), (1, 1));
        let rows: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(rows, ["a", "x = 1", "x = 2"]);
    }

    #[test]
    fn stream_text_keeps_blank_lines() {
        let output = json!({"ok": true, "data": {"stdout": "a\n\n", "exit_code": 0}});
        let view = tool_view(
            "bash",
            &json!({"command": "x"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        let ToolBody::Streams { streams } = body_of(&view) else {
            panic!();
        };
        assert_eq!(streams[0].text, "a\n\n");
        assert_eq!(view.hint.unwrap().text, "2 lines");
    }

    #[test]
    fn long_edit_is_diffed_before_it_is_cut() {
        let prefix = "x\n".repeat(MAX_VIEW_TEXT_BYTES / 2 + 10);
        let input = json!({
            "file_path": "a",
            "old_string": format!("{prefix}old"),
            "new_string": format!("{prefix}new")
        });
        let view = tool_view("edit", &input, None, None, ViewLimits::DISPLAY);
        assert_eq!(view.diff_stats(), Some((1, 1)));
    }

    #[test]
    fn long_heads_and_errors_are_capped_for_display() {
        let big = "p".repeat(MAX_VIEW_TEXT_BYTES + 1);
        let input = json!({"subagent": "explorer", "prompt": big});
        let view = tool_view("invoke_subagent", &input, None, None, ViewLimits::DISPLAY);
        let Some(ToolHead::Prompt { prompt, .. }) = &view.head else {
            panic!();
        };
        assert_eq!(prompt.len(), MAX_VIEW_TEXT_BYTES);
        assert_eq!(view.notes.len(), 1);

        let failure = json!({"ok": false, "error": {"code": "x", "message": big}});
        let view = tool_view(
            "read",
            &json!({}),
            Some(&failure),
            None,
            ViewLimits::DISPLAY,
        );
        let ToolBody::Error { message, .. } = body_of(&view) else {
            panic!();
        };
        assert_eq!(message.len(), MAX_VIEW_TEXT_BYTES);

        let view = tool_view("invoke_subagent", &input, None, None, ViewLimits::FULL);
        assert!(view.notes.is_empty());
    }

    #[test]
    fn grep_row_budget_counts_context_lines() {
        // The grep tool's byte cap ignores empty context lines, so 2000
        // matches can carry ~22k rows (measured with the real tool).
        let matches: Vec<Value> = (0..2000u64)
            .map(|i| {
                json!({"file": "f", "line_number": i * 11 + 6, "text": "x",
                       "context_before": ["", "", "", "", ""], "context_after": ["", "", "", "", ""]})
            })
            .collect();
        let output = json!({"ok": true, "data": {"matches": matches, "total_matches": 2000, "truncated": false}});
        let rows = |limits| {
            let view = tool_view(
                "grep",
                &json!({"pattern": "x"}),
                Some(&output),
                None,
                limits,
            );
            let ToolBody::Matches { files } = &view.body else {
                panic!();
            };
            (
                files.iter().map(|f| f.lines.len()).sum::<usize>(),
                view.notes.len(),
            )
        };
        assert_eq!(rows(ViewLimits::DISPLAY), (MAX_VIEW_ENTRIES, 1));
        assert_eq!(rows(ViewLimits::FULL), (22_000, 0));
    }

    #[test]
    fn split_cd_keeps_commands_that_can_run_after_a_failed_cd() {
        for command in [
            "cd /x && a || b",
            "cd /x && a; b",
            "cd /x && a & b",
            "cd /x && a\nb",
            "cd /x || exit 1",
            "cd - && ls",
        ] {
            assert_eq!(split_cd(command), (None, command.into()), "{command:?}");
        }
        for (command, rest) in [
            ("cd /x && a && b | c", "a && b | c"),
            ("cd /x && make 2>&1 | tail", "make 2>&1 | tail"),
            ("cd /x && echo 'a; b || c'", "echo 'a; b || c'"),
            ("cd /x && cargo build &> log", "cargo build &> log"),
        ] {
            assert_eq!(
                split_cd(command),
                (Some("/x".into()), rest.into()),
                "{command:?}"
            );
        }
    }

    fn info_and_facts(view: &ToolView) -> (Vec<&str>, Vec<&str>) {
        (facts(view), view.info.iter().map(String::as_str).collect())
    }

    #[test]
    fn explicit_background_start_keeps_its_job_details() {
        let output = json!({"ok": true, "data": {
            "bg_id": "bg-1", "pid": 5, "status": "running",
            "stdout_log": "/l/o", "stderr_log": "/l/e",
            "message": "Started background process bg-1 (pid 5)."
        }});
        let view = tool_view(
            "bash",
            &json!({"command": "just ci", "background": true}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(view.outcome, ToolOutcome::Ok);
        assert_eq!(view.hint.as_ref().unwrap().text, "in background");
        let (facts, info) = info_and_facts(&view);
        assert_eq!(facts, ["in background", "bg-1", "pid 5"]);
        assert_eq!(info, ["Logs: stdout /l/o, stderr /l/e"]);
        assert_eq!(view.status[0].kind, FactKind::Outcome);
    }

    #[test]
    fn handoff_to_background_shows_its_move_and_logs_without_a_size() {
        let output = json!({"ok": true, "data": {
            "backgrounded": true, "bg_id": "bg-2", "elapsed_secs": 300, "exit_code": null,
            "pid": 7, "pgid": 7, "status": "running", "stdout": "", "stderr": "",
            "stdout_log": "/l/o", "stderr_log": "/l/e",
            "stdout_total_bytes": 0, "stderr_total_bytes": 0,
            "stdout_truncated": false, "stderr_truncated": false, "timed_out": false,
            "message": "Command did not complete within its 300s foreground bound"
        }});
        let view = tool_view(
            "bash",
            &json!({"command": "x"}),
            Some(&output),
            None,
            ViewLimits::FULL,
        );
        let (facts, info) = info_and_facts(&view);
        assert_eq!(facts, ["in background", "bg-2", "pid 7"]);
        assert_eq!(
            info,
            [
                "Still running after 300s, so it was moved to the background.",
                "Logs: stdout /l/o, stderr /l/e"
            ]
        );
        assert!(view.notes.is_empty());
    }

    #[test]
    fn read_past_the_end_says_so() {
        let output = json!({"ok": true, "data": {
            "content": "", "offset": 100, "lines_shown": 0, "total_lines": 3, "truncated": false
        }});
        let view = tool_view(
            "read",
            &json!({"file_path": "a"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        assert_eq!(facts(&view), ["line 100 is past the end (3 lines)"]);
    }

    #[test]
    fn grep_counts_every_file_even_when_rows_are_cut() {
        let matches: Vec<Value> = (0..30u64)
            .map(|i| {
                json!({"file": format!("f{i}"), "line_number": 1, "text": "x",
                       "context_after": vec![""; 100]})
            })
            .collect();
        let output = json!({"ok": true, "data": {"matches": matches, "total_matches": 30, "truncated": false}});
        let view = tool_view(
            "grep",
            &json!({"pattern": "x"}),
            Some(&output),
            None,
            ViewLimits::DISPLAY,
        );
        let ToolBody::Matches { files } = &view.body else {
            panic!();
        };
        assert!(files.len() < 30, "the display is cut");
        assert_eq!(facts(&view), ["30 matches in 30 files"]);
    }

    #[test]
    fn facts_say_what_they_are_about() {
        let output = json!({"ok": true, "data": {
            "stdout": "a\n", "stderr": "", "exit_code": -1, "timed_out": true,
            "stdout_total_bytes": 2, "stderr_total_bytes": 0
        }});
        let view = tool_view(
            "bash",
            &json!({"command": "x"}),
            Some(&output),
            Some(5),
            ViewLimits::DISPLAY,
        );
        let kinds: Vec<FactKind> = view.status.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [FactKind::Outcome, FactKind::Detail, FactKind::Duration]
        );
    }

    #[test]
    fn split_cd_handles_quotes_and_plain_commands() {
        assert_eq!(
            split_cd("cd \"/a b\" && ls"),
            (Some("/a b".into()), "ls".into())
        );
        assert_eq!(split_cd("cd /a; ls"), (None, "cd /a; ls".into()));
        assert_eq!(split_cd("cd /a"), (None, "cd /a".into()));
        assert_eq!(split_cd("ls -la"), (None, "ls -la".into()));
    }
}
