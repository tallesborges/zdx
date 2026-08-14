use std::collections::{HashMap, VecDeque};

use super::thread_persistence::ThreadEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadTimingReport {
    pub turns: Vec<TurnTiming>,
    pub has_unavailable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnTiming {
    pub requests: Vec<RequestTiming>,
    pub tools: Vec<ToolTiming>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTiming {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub duration_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTiming {
    pub name: String,
    pub ok: Option<bool>,
    pub duration_ms: Option<u64>,
}

#[must_use]
pub fn inspect_thread_timings(events: &[ThreadEvent]) -> ThreadTimingReport {
    let mut turns = Vec::new();
    let mut pending: HashMap<String, VecDeque<usize>> = HashMap::new();
    let mut has_unavailable = false;

    for event in events {
        if matches!(event, ThreadEvent::Message { role, .. } if role == "user") {
            turns.push(TurnTiming {
                requests: Vec::new(),
                tools: Vec::new(),
            });
            pending.clear();
            continue;
        }

        let Some(turn) = turns.last_mut() else {
            continue;
        };
        match event {
            ThreadEvent::Usage {
                output_tokens,
                model,
                provider,
                duration_ms,
                ttft_ms,
                ..
            } if *output_tokens > 0 || duration_ms.is_some() || ttft_ms.is_some() => {
                has_unavailable |= duration_ms.is_none() || ttft_ms.is_none();
                turn.requests.push(RequestTiming {
                    model: model.clone(),
                    provider: provider.clone(),
                    duration_ms: *duration_ms,
                    ttft_ms: *ttft_ms,
                });
            }
            ThreadEvent::ToolUse { id, name, .. } => {
                let index = turn.tools.len();
                turn.tools.push(ToolTiming {
                    name: name.clone(),
                    ok: None,
                    duration_ms: None,
                });
                pending.entry(id.clone()).or_default().push_back(index);
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                ok,
                duration_ms,
                ..
            } => {
                let index = pending.get_mut(tool_use_id).and_then(VecDeque::pop_front);
                if let Some(index) = index {
                    let tool = &mut turn.tools[index];
                    tool.ok = Some(*ok);
                    tool.duration_ms = *duration_ms;
                    has_unavailable |= duration_ms.is_none();
                } else {
                    has_unavailable = true;
                }
            }
            _ => {}
        }
    }

    for turn in &turns {
        has_unavailable |= turn.tools.iter().any(|tool| tool.ok.is_none());
    }

    ThreadTimingReport {
        turns,
        has_unavailable,
    }
}

#[must_use]
pub fn format_thread_timing_report(report: &ThreadTimingReport) -> Vec<String> {
    let mut lines =
        vec!["Client-observed timings; TTFT is time to first streamed content.".to_string()];

    if report.turns.is_empty() {
        lines.push("No user turns found.".to_string());
        return lines;
    }

    for (turn_index, turn) in report.turns.iter().enumerate() {
        if turn_index > 0 {
            lines.push(String::new());
        }
        lines.push(format!("Turn {}", turn_index + 1));

        if turn.requests.is_empty() {
            lines.push("  Model requests: none recorded".to_string());
        } else {
            lines.push("  Model requests".to_string());
            for (index, request) in turn.requests.iter().enumerate() {
                let identity = match (&request.provider, &request.model) {
                    (Some(provider), Some(model)) => format!("{provider}:{model}"),
                    (None, Some(model)) => model.clone(),
                    _ => "unknown model".to_string(),
                };
                lines.push(format!(
                    "    {}. {} · duration {} · TTFT {}",
                    index + 1,
                    identity,
                    format_optional_duration(request.duration_ms),
                    format_optional_duration(request.ttft_ms),
                ));
            }
        }

        if turn.tools.is_empty() {
            lines.push("  Tools: none".to_string());
        } else {
            lines.push("  Tools".to_string());
            for tool in &turn.tools {
                let status = match tool.ok {
                    Some(true) => "ok",
                    Some(false) => "failed",
                    None => "incomplete",
                };
                lines.push(format!(
                    "    {} · {status} · duration {}",
                    tool.name,
                    format_optional_duration(tool.duration_ms),
                ));
            }
        }

        lines.push(format_aggregate(
            "Recorded successful request time",
            turn.requests.iter().map(|request| request.duration_ms),
        ));
        lines.push(format_aggregate(
            "Tool work (sum, not wall time)",
            turn.tools.iter().map(|tool| tool.duration_ms),
        ));
    }

    if report.has_unavailable {
        lines.push(String::new());
        lines.push(
            "Unavailable timings come from legacy or incomplete thread events; no estimates are shown."
                .to_string(),
        );
    }
    lines
}

fn format_aggregate(label: &str, values: impl Iterator<Item = Option<u64>>) -> String {
    let values: Vec<_> = values.collect();
    if values.is_empty() {
        return format!("  {label}: —");
    }
    let measured = values.iter().filter(|value| value.is_some()).count();
    if measured != values.len() {
        return format!(
            "  {label}: unavailable ({measured}/{} measured)",
            values.len()
        );
    }
    let total = values
        .into_iter()
        .flatten()
        .fold(0_u64, u64::saturating_add);
    format!("  {label}: {}", format_duration(total))
}

fn format_optional_duration(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_string(), format_duration)
}

fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", ms as f64 / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn groups_requests_and_repeated_tool_ids_within_turns() {
        let events = vec![
            ThreadEvent::user_message("first"),
            ThreadEvent::usage(
                super::super::thread_persistence::Usage::new(10, 2, 0, 0),
                Some("m".to_string()),
                Some("p".to_string()),
                Some(100),
                Some(20),
            ),
            ThreadEvent::tool_use("same", "read", json!({})),
            ThreadEvent::ToolResult {
                tool_use_id: "same".to_string(),
                output: json!({}),
                ok: true,
                duration_ms: Some(30),
                ts: "2026-01-01T00:00:00Z".to_string(),
            },
            ThreadEvent::user_message("second"),
            ThreadEvent::tool_use("same", "bash", json!({})),
            ThreadEvent::ToolResult {
                tool_use_id: "same".to_string(),
                output: json!({}),
                ok: false,
                duration_ms: Some(40),
                ts: "2026-01-01T00:00:01Z".to_string(),
            },
        ];

        let report = inspect_thread_timings(&events);
        assert_eq!(report.turns.len(), 2);
        assert_eq!(report.turns[0].requests[0].duration_ms, Some(100));
        assert_eq!(report.turns[0].tools[0].duration_ms, Some(30));
        assert_eq!(report.turns[1].tools[0].name, "bash");
        assert_eq!(report.turns[1].tools[0].duration_ms, Some(40));
        assert!(!report.has_unavailable);
    }

    #[test]
    fn incomplete_tool_makes_sum_unavailable() {
        let events = vec![
            ThreadEvent::user_message("hello"),
            ThreadEvent::tool_use("a", "read", json!({})),
            ThreadEvent::tool_result("a", json!({}), true),
        ];
        let lines = format_thread_timing_report(&inspect_thread_timings(&events));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Tool work") && line.contains("0/1 measured"))
        );
    }
}
