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

/// Formats a thread transcript in a human-readable format.
pub fn format_transcript(events: &[ThreadEvent]) -> String {
    let mut output = String::new();
    let mut models_used: Vec<String> = Vec::new();

    for event in events {
        match event {
            ThreadEvent::Meta { schema_version, .. } => {
                writeln!(output, "### Thread (schema v{schema_version})").expect("write");
                output.push('\n');
            }
            ThreadEvent::Message { role, text, .. } => {
                let role_label = match role.as_str() {
                    "user" => "You",
                    "assistant" => "Assistant",
                    _ => role,
                };
                writeln!(output, "### {role_label}").expect("write");
                output.push_str(text);
                output.push_str("\n\n");
            }
            ThreadEvent::Reasoning { text, .. } => {
                if let Some(content) = text {
                    output.push_str("### Thinking\n");
                    if content.len() > 500 {
                        output.push_str(truncate_str(content, 500));
                        output.push_str("...");
                    } else {
                        output.push_str(content);
                    }
                    output.push_str("\n\n");
                }
            }
            ThreadEvent::ToolUse { name, input, .. } => {
                writeln!(output, "### Tool: {name}").expect("write");
                writeln!(
                    output,
                    "```json\n{}\n```\n",
                    serde_json::to_string_pretty(input).unwrap_or_default()
                )
                .expect("write");
            }
            ThreadEvent::ToolResult {
                ok, output: out, ..
            } => {
                let status = if *ok { "✓" } else { "✗" };
                writeln!(output, "### Result {status}").expect("write");
                // Truncate long outputs for display
                let out_str = serde_json::to_string_pretty(out).unwrap_or_default();
                if out_str.len() > 500 {
                    writeln!(output, "```json\n{}...\n```\n", truncate_str(&out_str, 500))
                        .expect("write");
                } else {
                    writeln!(output, "```json\n{out_str}\n```\n").expect("write");
                }
            }
            ThreadEvent::Interrupted { .. } => {
                output.push_str("### Interrupted\n\n");
            }
            ThreadEvent::Notice { message, .. } => {
                writeln!(output, "### Notice\n⚠ {message}\n").expect("write");
            }
            ThreadEvent::Usage {
                model, provider, ..
            } => {
                if let Some(label) = usage_model_label(model.as_deref(), provider.as_deref())
                    && !models_used.contains(&label)
                {
                    models_used.push(label);
                }
            }
        }
    }

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
