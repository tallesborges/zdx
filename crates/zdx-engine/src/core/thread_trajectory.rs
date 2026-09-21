use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use super::thread_persistence::ThreadEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectorySpanKind {
    Input,
    Model,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryLane {
    Input,
    Model,
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryStatus {
    Ok,
    Failed,
    Running,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingQuality {
    Exact,
    Duration,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnTimingQuality {
    Exact,
    Partial,
    Legacy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrajectoryInput {
    pub sequence: usize,
    pub ts: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrajectorySpan {
    pub key: String,
    pub kind: TrajectorySpanKind,
    pub lane: TrajectoryLane,
    pub turn: usize,
    pub sequence: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_sequence: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    pub label: String,
    pub detail: String,
    pub status: TrajectoryStatus,
    pub timing: TimingQuality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
    /// Client-recorded duration. This remains distinct from `wall_ms`: tool
    /// boundaries include scheduling around the actual tool execution timer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Width of explicit wall-clock boundaries. Used for placement, interval
    /// unions, and overlap arithmetic. Live spans carry a provisional value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    pub track: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Live-only bounded input. Persisted payloads stay in the thread activity
    /// response and are joined by source sequence in the Mini App.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_input: Option<Value>,
    /// Live-only bounded output tail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_output_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrajectoryBottleneck {
    pub key: String,
    pub kind: TrajectorySpanKind,
    pub turn: usize,
    pub label: String,
    pub detail: String,
    pub status: TrajectoryStatus,
    pub timing: TimingQuality,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrajectoryTurn {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<TrajectoryInput>,
    pub spans: Vec<TrajectorySpan>,
    pub timing: TurnTimingQuality,
    pub measured: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_start_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_end_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Union of exact model/tool intervals observed in this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_wall_ms: Option<u64>,
    /// Sum of exact interval widths. This is wall-clock span work, not the sum
    /// of independently recorded request/tool execution durations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_work_ms: Option<u64>,
    /// `span_work_ms - active_wall_ms`: excess concurrent span work. With
    /// three-way concurrency this exceeds the time during which any overlap
    /// existed, so it is not labelled "overlap duration".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrent_work_ms: Option<u64>,
    pub model_tracks: usize,
    pub tool_tracks: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadTrajectoryReport {
    pub source_event_count: usize,
    pub observed_at: String,
    pub turns: Vec<TrajectoryTurn>,
    pub bottlenecks: Vec<TrajectoryBottleneck>,
    pub measured: usize,
    pub total: usize,
    pub exact: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_wall_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_work_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrent_work_ms: Option<u64>,
    pub has_unavailable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveToolSpan {
    pub sequence: usize,
    pub id: String,
    pub name: String,
    pub summary: String,
    pub input: Value,
    pub output_tail: String,
    pub started_at: String,
}

struct MutableTurn {
    index: usize,
    input: Option<TrajectoryInput>,
    spans: Vec<TrajectorySpan>,
    request_count: usize,
}

#[derive(Default)]
struct PendingUsage {
    sequence: usize,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    model: Option<String>,
    provider: Option<String>,
    duration_ms: Option<u64>,
    ttft_ms: Option<u64>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

impl PendingUsage {
    fn has_input(&self) -> bool {
        self.input_tokens > 0 || self.cache_read_tokens > 0 || self.cache_write_tokens > 0
    }

    fn merge(&mut self, sequence: usize, event: &ThreadEvent) {
        let ThreadEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            model,
            provider,
            duration_ms,
            ttft_ms,
            started_at,
            completed_at,
            ..
        } = event
        else {
            return;
        };
        if self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_read_tokens == 0
            && self.cache_write_tokens == 0
        {
            self.sequence = sequence;
        }
        self.input_tokens = self.input_tokens.saturating_add(*input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(*output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(*cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(*cache_write_tokens);
        if model.is_some() {
            self.model.clone_from(model);
        }
        if provider.is_some() {
            self.provider.clone_from(provider);
        }
        if duration_ms.is_some() {
            self.duration_ms = *duration_ms;
        }
        if ttft_ms.is_some() {
            self.ttft_ms = *ttft_ms;
        }
        if started_at.is_some() {
            self.started_at.clone_from(started_at);
        }
        if completed_at.is_some() {
            self.completed_at.clone_from(completed_at);
        }
    }
}

#[must_use]
pub fn inspect_thread_trajectory(events: &[ThreadEvent]) -> ThreadTrajectoryReport {
    inspect_thread_trajectory_with_live(events, &[], Utc::now())
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn inspect_thread_trajectory_with_live(
    events: &[ThreadEvent],
    live_tools: &[LiveToolSpan],
    observed_at: DateTime<Utc>,
) -> ThreadTrajectoryReport {
    let mut turns = Vec::new();
    let mut current = None;
    let mut pending_usage = None;
    let mut pending_tools: HashMap<String, VecDeque<usize>> = HashMap::new();
    let mut unmatched_result = false;

    for (sequence, event) in events.iter().enumerate() {
        if let ThreadEvent::Message { role, text, ts, .. } = event
            && role == "user"
        {
            flush_pending_usage(&mut turns, &mut current, &mut pending_usage);
            let index = turns.len() + 1;
            let start_ms = precise_event_ms(ts);
            let input = TrajectoryInput {
                sequence,
                ts: ts.clone(),
                text: text.clone(),
            };
            turns.push(MutableTurn {
                index,
                input: Some(input),
                spans: vec![TrajectorySpan {
                    key: format!("input:{sequence}"),
                    kind: TrajectorySpanKind::Input,
                    lane: TrajectoryLane::Input,
                    turn: index,
                    sequence,
                    result_sequence: None,
                    tool_use_id: None,
                    label: "Input".to_string(),
                    detail: compact(text, 96),
                    status: TrajectoryStatus::Ok,
                    timing: if start_ms.is_some() {
                        TimingQuality::Exact
                    } else {
                        TimingQuality::Unavailable
                    },
                    started_at: Some(ts.clone()),
                    completed_at: None,
                    start_ms,
                    end_ms: start_ms,
                    duration_ms: Some(0),
                    wall_ms: Some(0),
                    track: 0,
                    model: None,
                    provider: None,
                    ttft_ms: None,
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    live_input: None,
                    live_output_tail: None,
                }],
                request_count: 0,
            });
            current = Some(turns.len() - 1);
            pending_tools.clear();
            continue;
        }

        match event {
            ThreadEvent::Usage {
                input_tokens,
                cache_read_tokens,
                cache_write_tokens,
                duration_ms,
                ttft_ms,
                started_at,
                completed_at,
                ..
            } => {
                let terminal = duration_ms.is_some()
                    || ttft_ms.is_some()
                    || (started_at.is_some() && completed_at.is_some());
                let input_bearing =
                    *input_tokens > 0 || *cache_read_tokens > 0 || *cache_write_tokens > 0;
                if !terminal
                    && input_bearing
                    && pending_usage.as_ref().is_some_and(PendingUsage::has_input)
                {
                    flush_pending_usage(&mut turns, &mut current, &mut pending_usage);
                }
                let pending = pending_usage.get_or_insert_with(PendingUsage::default);
                pending.merge(sequence, event);
                if terminal {
                    flush_pending_usage(&mut turns, &mut current, &mut pending_usage);
                }
            }
            ThreadEvent::ToolUse {
                id, name, input, ..
            } => {
                flush_pending_usage(&mut turns, &mut current, &mut pending_usage);
                let turn_index = ensure_turn(&mut turns, &mut current);
                let turn = &mut turns[turn_index];
                let span_index = turn.spans.len();
                turn.spans.push(TrajectorySpan {
                    key: format!("tool:{}:{sequence}:{id}", turn.index),
                    kind: TrajectorySpanKind::Tool,
                    lane: TrajectoryLane::Tools,
                    turn: turn.index,
                    sequence,
                    result_sequence: None,
                    tool_use_id: Some(id.clone()),
                    label: name.to_ascii_lowercase(),
                    detail: tool_detail(name, input),
                    status: TrajectoryStatus::Unknown,
                    timing: TimingQuality::Unavailable,
                    started_at: None,
                    completed_at: None,
                    start_ms: None,
                    end_ms: None,
                    duration_ms: None,
                    wall_ms: None,
                    track: 0,
                    model: None,
                    provider: None,
                    ttft_ms: None,
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    live_input: None,
                    live_output_tail: None,
                });
                pending_tools
                    .entry(id.clone())
                    .or_default()
                    .push_back(span_index);
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                ok,
                duration_ms,
                started_at,
                completed_at,
                ..
            } => {
                let turn_index = ensure_turn(&mut turns, &mut current);
                let queued = pending_tools
                    .get_mut(tool_use_id)
                    .and_then(VecDeque::pop_front);
                if pending_tools
                    .get(tool_use_id)
                    .is_some_and(VecDeque::is_empty)
                {
                    pending_tools.remove(tool_use_id);
                }
                if let Some(span_index) = queued {
                    let span = &mut turns[turn_index].spans[span_index];
                    span.result_sequence = Some(sequence);
                    span.status = if *ok {
                        TrajectoryStatus::Ok
                    } else {
                        TrajectoryStatus::Failed
                    };
                    apply_timing(
                        span,
                        *duration_ms,
                        started_at.as_deref(),
                        completed_at.as_deref(),
                    );
                } else {
                    unmatched_result = true;
                    let turn = &mut turns[turn_index];
                    let mut span = TrajectorySpan {
                        key: format!("tool-result:{}:{sequence}", turn.index),
                        kind: TrajectorySpanKind::Tool,
                        lane: TrajectoryLane::Tools,
                        turn: turn.index,
                        sequence,
                        result_sequence: Some(sequence),
                        tool_use_id: Some(tool_use_id.clone()),
                        label: "tool result".to_string(),
                        detail: tool_use_id.clone(),
                        status: if *ok {
                            TrajectoryStatus::Ok
                        } else {
                            TrajectoryStatus::Failed
                        },
                        timing: TimingQuality::Unavailable,
                        started_at: None,
                        completed_at: None,
                        start_ms: None,
                        end_ms: None,
                        duration_ms: None,
                        wall_ms: None,
                        track: 0,
                        model: None,
                        provider: None,
                        ttft_ms: None,
                        input_tokens: None,
                        output_tokens: None,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        live_input: None,
                        live_output_tail: None,
                    };
                    apply_timing(
                        &mut span,
                        *duration_ms,
                        started_at.as_deref(),
                        completed_at.as_deref(),
                    );
                    turn.spans.push(span);
                }
            }
            _ => {}
        }
    }
    flush_pending_usage(&mut turns, &mut current, &mut pending_usage);

    if !live_tools.is_empty() {
        let turn_index = ensure_turn(&mut turns, &mut current);
        for live in live_tools {
            let existing = turns[turn_index].spans.iter().rposition(|span| {
                span.kind == TrajectorySpanKind::Tool
                    && span.tool_use_id.as_deref() == Some(live.id.as_str())
                    && span.status == TrajectoryStatus::Unknown
            });
            let start_ms = timestamp_ms(&live.started_at);
            let end_ms = start_ms.map(|start| observed_at.timestamp_millis().max(start));
            let wall_ms = interval_width(start_ms, end_ms);
            let live_span = TrajectorySpan {
                key: existing.map_or_else(
                    || {
                        format!(
                            "tool:{}:{}:{}",
                            turns[turn_index].index, live.sequence, live.id
                        )
                    },
                    |span_index| turns[turn_index].spans[span_index].key.clone(),
                ),
                kind: TrajectorySpanKind::Tool,
                lane: TrajectoryLane::Tools,
                turn: turns[turn_index].index,
                sequence: existing.map_or(live.sequence, |span_index| {
                    turns[turn_index].spans[span_index].sequence
                }),
                result_sequence: None,
                tool_use_id: Some(live.id.clone()),
                label: live.name.to_ascii_lowercase(),
                detail: if live.summary.trim().is_empty() {
                    tool_detail(&live.name, &live.input)
                } else {
                    compact(&live.summary, 64)
                },
                status: TrajectoryStatus::Running,
                timing: if start_ms.is_some() {
                    TimingQuality::Exact
                } else {
                    TimingQuality::Unavailable
                },
                started_at: Some(live.started_at.clone()),
                completed_at: None,
                start_ms,
                end_ms,
                duration_ms: None,
                wall_ms,
                track: 0,
                model: None,
                provider: None,
                ttft_ms: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                live_input: Some(live.input.clone()),
                live_output_tail: Some(live.output_tail.clone()),
            };
            if let Some(span_index) = existing {
                turns[turn_index].spans[span_index] = live_span;
            } else {
                turns[turn_index].spans.push(live_span);
            }
        }
    }

    let turns: Vec<_> = turns.into_iter().map(finalize_turn).collect();
    let spans = turns
        .iter()
        .flat_map(|turn| turn.spans.iter())
        .filter(|span| span.kind != TrajectorySpanKind::Input);
    let mut bottlenecks: Vec<_> = spans
        .clone()
        .filter_map(|span| {
            span.duration_ms
                .or(span.wall_ms)
                .map(|duration_ms| TrajectoryBottleneck {
                    key: span.key.clone(),
                    kind: span.kind,
                    turn: span.turn,
                    label: span.label.clone(),
                    detail: span.detail.clone(),
                    status: span.status,
                    timing: span.timing,
                    duration_ms,
                })
        })
        .collect();
    bottlenecks.sort_by(|a, b| {
        b.duration_ms
            .cmp(&a.duration_ms)
            .then_with(|| a.turn.cmp(&b.turn))
            .then_with(|| a.key.cmp(&b.key))
    });

    let total = spans.clone().count();
    let measured = spans
        .clone()
        .filter(|span| span.duration_ms.is_some() || span.wall_ms.is_some())
        .count();
    let exact = spans
        .clone()
        .filter(|span| span.timing == TimingQuality::Exact)
        .count();
    let active_wall_ms = sum_optional(turns.iter().map(|turn| turn.active_wall_ms));
    let span_work_ms = sum_optional(turns.iter().map(|turn| turn.span_work_ms));
    let concurrent_work_ms = match (active_wall_ms, span_work_ms) {
        (Some(active), Some(work)) => Some(work.saturating_sub(active)),
        _ => None,
    };

    ThreadTrajectoryReport {
        source_event_count: events.len(),
        observed_at: observed_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        turns,
        bottlenecks,
        measured,
        total,
        exact,
        active_wall_ms,
        span_work_ms,
        concurrent_work_ms,
        has_unavailable: unmatched_result || measured < total,
    }
}

fn ensure_turn(turns: &mut Vec<MutableTurn>, current: &mut Option<usize>) -> usize {
    if let Some(index) = *current {
        return index;
    }
    turns.push(MutableTurn {
        index: turns.len() + 1,
        input: None,
        spans: Vec::new(),
        request_count: 0,
    });
    let index = turns.len() - 1;
    *current = Some(index);
    index
}

fn flush_pending_usage(
    turns: &mut Vec<MutableTurn>,
    current: &mut Option<usize>,
    pending: &mut Option<PendingUsage>,
) {
    let Some(pending) = pending.take() else {
        return;
    };
    let turn_index = ensure_turn(turns, current);
    let turn = &mut turns[turn_index];
    turn.request_count += 1;
    let detail = match (&pending.provider, &pending.model) {
        (Some(provider), Some(model)) => format!("{provider}:{model}"),
        (None, Some(model)) => model.clone(),
        _ => "model request".to_string(),
    };
    let mut span = TrajectorySpan {
        key: format!("model:{}:{}", turn.index, pending.sequence),
        kind: TrajectorySpanKind::Model,
        lane: TrajectoryLane::Model,
        turn: turn.index,
        sequence: pending.sequence,
        result_sequence: None,
        tool_use_id: None,
        label: format!("Request {}", turn.request_count),
        detail,
        status: TrajectoryStatus::Ok,
        timing: TimingQuality::Unavailable,
        started_at: None,
        completed_at: None,
        start_ms: None,
        end_ms: None,
        duration_ms: None,
        wall_ms: None,
        track: 0,
        model: pending.model,
        provider: pending.provider,
        ttft_ms: pending.ttft_ms,
        input_tokens: Some(pending.input_tokens),
        output_tokens: Some(pending.output_tokens),
        cache_read_tokens: Some(pending.cache_read_tokens),
        cache_write_tokens: Some(pending.cache_write_tokens),
        live_input: None,
        live_output_tail: None,
    };
    apply_timing(
        &mut span,
        pending.duration_ms,
        pending.started_at.as_deref(),
        pending.completed_at.as_deref(),
    );
    if span.timing == TimingQuality::Unavailable {
        span.status = TrajectoryStatus::Unknown;
    }
    turn.spans.push(span);
}

fn apply_timing(
    span: &mut TrajectorySpan,
    duration_ms: Option<u64>,
    started_at: Option<&str>,
    completed_at: Option<&str>,
) {
    span.duration_ms = duration_ms;
    span.started_at = started_at.map(str::to_string);
    span.completed_at = completed_at.map(str::to_string);
    let start_ms = started_at.and_then(timestamp_ms);
    let end_ms = completed_at.and_then(timestamp_ms);
    span.start_ms = start_ms;
    span.end_ms = end_ms;
    span.wall_ms = interval_width(start_ms, end_ms);
    span.timing = if span.wall_ms.is_some() {
        TimingQuality::Exact
    } else if duration_ms.is_some() {
        TimingQuality::Duration
    } else {
        TimingQuality::Unavailable
    };
}

fn finalize_turn(mut turn: MutableTurn) -> TrajectoryTurn {
    let exact_indices: Vec<_> = turn
        .spans
        .iter()
        .enumerate()
        .filter(|(_, span)| {
            span.kind != TrajectorySpanKind::Input && span.timing == TimingQuality::Exact
        })
        .map(|(index, _)| index)
        .collect();
    let model_tracks = pack_tracks(&mut turn.spans, &exact_indices, TrajectoryLane::Model);
    let tool_tracks = pack_tracks(&mut turn.spans, &exact_indices, TrajectoryLane::Tools);
    let candidates: Vec<_> = turn
        .spans
        .iter()
        .filter(|span| span.kind != TrajectorySpanKind::Input)
        .collect();
    let measured = candidates
        .iter()
        .filter(|span| span.duration_ms.is_some() || span.wall_ms.is_some())
        .count();
    let total = candidates.len();
    let exact = candidates
        .iter()
        .filter(|span| span.timing == TimingQuality::Exact)
        .count();
    let timing = if total > 0 && exact == total {
        TurnTimingQuality::Exact
    } else if exact > 0 {
        TurnTimingQuality::Partial
    } else {
        TurnTimingQuality::Legacy
    };

    let mut starts: Vec<i64> = exact_indices
        .iter()
        .filter_map(|index| turn.spans[*index].start_ms)
        .collect();
    if let Some(input_start) = turn
        .spans
        .iter()
        .find(|span| span.kind == TrajectorySpanKind::Input)
        .and_then(|span| span.start_ms)
    {
        starts.push(input_start);
    }
    let ends: Vec<i64> = exact_indices
        .iter()
        .filter_map(|index| turn.spans[*index].end_ms)
        .collect();
    let domain_start_ms = starts.into_iter().min();
    let domain_end_ms = ends.into_iter().max().or(domain_start_ms);
    let elapsed_ms = interval_width(domain_start_ms, domain_end_ms);
    let active_wall_ms = union_duration(
        exact_indices
            .iter()
            .filter_map(|index| interval(&turn.spans[*index])),
    );
    let span_work_ms = sum_optional(exact_indices.iter().map(|index| turn.spans[*index].wall_ms));
    let concurrent_work_ms = match (active_wall_ms, span_work_ms) {
        (Some(active), Some(work)) => Some(work.saturating_sub(active)),
        _ => None,
    };

    TrajectoryTurn {
        index: turn.index,
        input: turn.input,
        spans: turn.spans,
        timing,
        measured,
        total,
        domain_start_ms,
        domain_end_ms,
        elapsed_ms,
        active_wall_ms,
        span_work_ms,
        concurrent_work_ms,
        model_tracks,
        tool_tracks,
    }
}

fn pack_tracks(
    spans: &mut [TrajectorySpan],
    exact_indices: &[usize],
    lane: TrajectoryLane,
) -> usize {
    let mut indices: Vec<_> = exact_indices
        .iter()
        .copied()
        .filter(|index| spans[*index].lane == lane)
        .collect();
    indices.sort_by_key(|index| (spans[*index].start_ms, spans[*index].sequence));
    let mut ends = Vec::new();
    for index in indices {
        let start = spans[index].start_ms.unwrap_or_default();
        let end = spans[index].end_ms.unwrap_or(start);
        let track = ends.iter().position(|previous| *previous <= start);
        let track = track.unwrap_or_else(|| {
            ends.push(end);
            ends.len() - 1
        });
        ends[track] = end;
        spans[index].track = track;
    }
    ends.len().max(1)
}

fn interval(span: &TrajectorySpan) -> Option<(i64, i64)> {
    let start = span.start_ms?;
    let end = span.end_ms?;
    (end >= start).then_some((start, end))
}

fn union_duration(intervals: impl Iterator<Item = (i64, i64)>) -> Option<u64> {
    let mut intervals: Vec<_> = intervals.collect();
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_unstable_by_key(|(start, _)| *start);
    let (mut start, mut end) = intervals[0];
    let mut total = 0_u64;
    for (next_start, next_end) in intervals.into_iter().skip(1) {
        if next_start <= end {
            end = end.max(next_end);
        } else {
            total = total.saturating_add(u64::try_from(end - start).unwrap_or_default());
            start = next_start;
            end = next_end;
        }
    }
    Some(total.saturating_add(u64::try_from(end - start).unwrap_or_default()))
}

fn sum_optional(values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    let values: Vec<_> = values.collect();
    if values.is_empty() || values.iter().all(Option::is_none) {
        return None;
    }
    Some(
        values
            .into_iter()
            .flatten()
            .fold(0_u64, u64::saturating_add),
    )
}

fn timestamp_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn precise_event_ms(value: &str) -> Option<i64> {
    value
        .split_once('T')
        .is_some_and(|(_, time)| time.contains('.'))
        .then(|| timestamp_ms(value))
        .flatten()
}

fn interval_width(start: Option<i64>, end: Option<i64>) -> Option<u64> {
    let (start, end) = (start?, end?);
    (end >= start)
        .then(|| u64::try_from(end - start).ok())
        .flatten()
}

fn tool_detail(name: &str, input: &Value) -> String {
    let detail = zdx_types::tool_command_text(&name.to_ascii_lowercase(), input);
    if detail.trim().is_empty() {
        name.to_ascii_lowercase()
    } else {
        compact(&detail, 64)
    }
}

fn compact(value: &str, max: usize) -> String {
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut truncated: String = flat.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn format_thread_trajectory_report(report: &ThreadTrajectoryReport) -> Vec<String> {
    let mut lines = vec![
        "Client-observed trajectory; exact spans use recorded wall-clock boundaries.".to_string(),
        format!(
            "Coverage: {}/{} durations measured · {} with exact boundaries",
            report.measured, report.total, report.exact
        ),
        format!(
            "Observed active wall: {}",
            format_optional_duration(report.active_wall_ms)
        ),
        format!(
            "Observed span work: {}",
            format_optional_duration(report.span_work_ms)
        ),
        format!(
            "Concurrent span work: {}",
            format_optional_duration(report.concurrent_work_ms)
        ),
    ];

    if report.bottlenecks.is_empty() {
        lines.push("Bottlenecks: none measured".to_string());
    } else {
        lines.push("Bottlenecks".to_string());
        for (index, span) in report.bottlenecks.iter().take(5).enumerate() {
            lines.push(format!(
                "  {}. {} · turn {} · {} · {}",
                index + 1,
                span.label,
                span.turn,
                timing_quality_label(span.timing),
                format_duration(span.duration_ms),
            ));
        }
    }

    if report.turns.is_empty() {
        lines.push("No user turns found.".to_string());
        return lines;
    }

    for turn in &report.turns {
        lines.push(String::new());
        lines.push(format!(
            "Turn {} · {}",
            turn.index,
            turn_quality_label(turn.timing)
        ));
        let requests: Vec<_> = turn
            .spans
            .iter()
            .filter(|span| span.kind == TrajectorySpanKind::Model)
            .collect();
        let tools: Vec<_> = turn
            .spans
            .iter()
            .filter(|span| span.kind == TrajectorySpanKind::Tool)
            .collect();

        if requests.is_empty() {
            lines.push("  Model requests: none recorded".to_string());
        } else {
            lines.push("  Model requests".to_string());
            for (index, request) in requests.iter().enumerate() {
                lines.push(format!(
                    "    {}. {} · duration {} · TTFT {} · {}",
                    index + 1,
                    request.detail,
                    format_optional_duration(request.duration_ms),
                    format_optional_duration(request.ttft_ms),
                    timing_quality_label(request.timing),
                ));
            }
        }

        if tools.is_empty() {
            lines.push("  Tools: none".to_string());
        } else {
            lines.push("  Tools".to_string());
            for tool in &tools {
                lines.push(format!(
                    "    {} · {} · duration {} · {}",
                    tool.label,
                    status_label(tool.status),
                    format_optional_duration(tool.duration_ms.or(tool.wall_ms)),
                    timing_quality_label(tool.timing),
                ));
            }
        }

        lines.push(format_aggregate(
            "Recorded successful request time",
            requests.iter().map(|span| span.duration_ms),
        ));
        lines.push(format_aggregate(
            "Tool work (sum, not wall time)",
            tools.iter().map(|span| span.duration_ms),
        ));
        lines.push(format!(
            "  Observed active wall: {}",
            format_optional_duration(turn.active_wall_ms)
        ));
        lines.push(format!(
            "  Concurrent span work: {}",
            format_optional_duration(turn.concurrent_work_ms)
        ));
    }

    if report.has_unavailable {
        lines.push(String::new());
        lines.push(
            "Unavailable timings come from legacy or incomplete thread events; no boundaries are inferred."
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

fn timing_quality_label(quality: TimingQuality) -> &'static str {
    match quality {
        TimingQuality::Exact => "exact span",
        TimingQuality::Duration => "duration only",
        TimingQuality::Unavailable => "unavailable",
    }
}

fn turn_quality_label(quality: TurnTimingQuality) -> &'static str {
    match quality {
        TurnTimingQuality::Exact => "exact",
        TurnTimingQuality::Partial => "partial",
        TurnTimingQuality::Legacy => "legacy",
    }
}

fn status_label(status: TrajectoryStatus) -> &'static str {
    match status {
        TrajectoryStatus::Ok => "ok",
        TrajectoryStatus::Failed => "failed",
        TrajectoryStatus::Running => "running",
        TrajectoryStatus::Unknown => "incomplete",
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    fn user(ts: &str) -> ThreadEvent {
        ThreadEvent::Message {
            role: "user".to_string(),
            text: "inspect".to_string(),
            phase: None,
            context: None,
            context_key: None,
            replay: None,
            ts: ts.to_string(),
        }
    }

    fn usage(
        duration_ms: Option<u64>,
        started_at: Option<&str>,
        completed_at: Option<&str>,
    ) -> ThreadEvent {
        ThreadEvent::Usage {
            input_tokens: 10,
            output_tokens: 2,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            model: Some("model-a".to_string()),
            provider: Some("provider-a".to_string()),
            duration_ms,
            ttft_ms: Some(20),
            started_at: started_at.map(str::to_string),
            completed_at: completed_at.map(str::to_string),
            ts: completed_at.unwrap_or("2026-01-01T00:00:01Z").to_string(),
        }
    }

    #[test]
    fn preserves_parallel_overlap_and_packs_tool_tracks() {
        let events = vec![
            user("2026-01-01T10:00:00.000Z"),
            usage(
                Some(1_000),
                Some("2026-01-01T10:00:00.100Z"),
                Some("2026-01-01T10:00:01.100Z"),
            ),
            ThreadEvent::tool_use("a", "read", json!({"file_path": "a.txt"})),
            ThreadEvent::tool_use("b", "read", json!({"file_path": "b.txt"})),
            ThreadEvent::ToolResult {
                tool_use_id: "a".to_string(),
                output: json!({}),
                ok: true,
                duration_ms: Some(1_800),
                started_at: Some("2026-01-01T10:00:01.100Z".to_string()),
                completed_at: Some("2026-01-01T10:00:03.100Z".to_string()),
                ts: "2026-01-01T10:00:03.100Z".to_string(),
            },
            ThreadEvent::ToolResult {
                tool_use_id: "b".to_string(),
                output: json!({}),
                ok: true,
                duration_ms: Some(900),
                started_at: Some("2026-01-01T10:00:01.100Z".to_string()),
                completed_at: Some("2026-01-01T10:00:02.100Z".to_string()),
                ts: "2026-01-01T10:00:02.100Z".to_string(),
            },
            usage(
                Some(500),
                Some("2026-01-01T10:00:03.100Z"),
                Some("2026-01-01T10:00:03.600Z"),
            ),
        ];

        let report = inspect_thread_trajectory_with_live(
            &events,
            &[],
            Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 4).unwrap(),
        );
        let turn = &report.turns[0];
        assert_eq!(turn.timing, TurnTimingQuality::Exact);
        assert_eq!(turn.tool_tracks, 2);
        assert_eq!(turn.active_wall_ms, Some(3_500));
        assert_eq!(turn.span_work_ms, Some(4_500));
        assert_eq!(turn.concurrent_work_ms, Some(1_000));
        assert_eq!(report.bottlenecks[0].label, "read");
        assert_eq!(report.bottlenecks[0].duration_ms, 1_800);
    }

    #[test]
    fn keeps_recorded_duration_distinct_from_boundary_width() {
        let events = vec![
            user("2026-01-01T10:00:00.000Z"),
            ThreadEvent::tool_use("a", "bash", json!({"command": "run checks"})),
            ThreadEvent::ToolResult {
                tool_use_id: "a".to_string(),
                output: json!({}),
                ok: true,
                duration_ms: Some(700),
                started_at: Some("2026-01-01T10:00:01.000Z".to_string()),
                completed_at: Some("2026-01-01T10:00:02.000Z".to_string()),
                ts: "2026-01-01T10:00:02.000Z".to_string(),
            },
        ];
        let report = inspect_thread_trajectory(&events);
        let span = &report.turns[0].spans[1];
        assert_eq!(span.duration_ms, Some(700));
        assert_eq!(span.wall_ms, Some(1_000));
        assert_eq!(report.bottlenecks[0].duration_ms, 700);
    }

    #[test]
    fn ranks_legacy_duration_without_inventing_boundaries() {
        let events = vec![
            user("2025-01-01T10:00:00Z"),
            ThreadEvent::tool_use("a", "bash", json!({"command": "run checks"})),
            ThreadEvent::ToolResult {
                tool_use_id: "a".to_string(),
                output: json!({}),
                ok: true,
                duration_ms: Some(8_000),
                started_at: None,
                completed_at: None,
                ts: "2025-01-01T10:00:09Z".to_string(),
            },
        ];
        let report = inspect_thread_trajectory(&events);
        assert_eq!(report.turns[0].timing, TurnTimingQuality::Legacy);
        assert_eq!(report.turns[0].active_wall_ms, None);
        assert_eq!(report.bottlenecks[0].duration_ms, 8_000);
        assert_eq!(report.bottlenecks[0].timing, TimingQuality::Duration);
    }

    #[test]
    fn folds_untimed_usage_fragment_into_terminal_request() {
        let events = vec![
            user("2026-01-01T10:00:00.000Z"),
            ThreadEvent::Usage {
                input_tokens: 2,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 34_308,
                model: Some("model-a".to_string()),
                provider: Some("provider-a".to_string()),
                duration_ms: None,
                ttft_ms: None,
                started_at: None,
                completed_at: None,
                ts: "2026-01-01T10:00:00.100Z".to_string(),
            },
            ThreadEvent::Usage {
                input_tokens: 0,
                output_tokens: 322,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                model: Some("model-a".to_string()),
                provider: Some("provider-a".to_string()),
                duration_ms: Some(5_166),
                ttft_ms: Some(4_417),
                started_at: Some("2026-01-01T10:00:00.100Z".to_string()),
                completed_at: Some("2026-01-01T10:00:05.266Z".to_string()),
                ts: "2026-01-01T10:00:05.266Z".to_string(),
            },
        ];
        let report = inspect_thread_trajectory(&events);
        let requests: Vec<_> = report.turns[0]
            .spans
            .iter()
            .filter(|span| span.kind == TrajectorySpanKind::Model)
            .collect();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].input_tokens, Some(2));
        assert_eq!(requests[0].output_tokens, Some(322));
        assert_eq!(requests[0].duration_ms, Some(5_166));
    }

    #[test]
    fn repeated_ids_pair_fifo_and_orphans_remain_visible() {
        let events = vec![
            user("2026-01-01T10:00:00.000Z"),
            ThreadEvent::tool_use("same", "read", json!({"file_path": "a.txt"})),
            ThreadEvent::tool_use("same", "read", json!({"file_path": "b.txt"})),
            ThreadEvent::ToolResult {
                tool_use_id: "same".to_string(),
                output: json!({}),
                ok: true,
                duration_ms: Some(10),
                started_at: None,
                completed_at: None,
                ts: "2026-01-01T10:00:01Z".to_string(),
            },
            ThreadEvent::ToolResult {
                tool_use_id: "same".to_string(),
                output: json!({}),
                ok: false,
                duration_ms: Some(20),
                started_at: None,
                completed_at: None,
                ts: "2026-01-01T10:00:02Z".to_string(),
            },
            ThreadEvent::tool_result("orphan", json!({}), true),
        ];
        let report = inspect_thread_trajectory(&events);
        let tools: Vec<_> = report.turns[0]
            .spans
            .iter()
            .filter(|span| span.kind == TrajectorySpanKind::Tool)
            .collect();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].duration_ms, Some(10));
        assert_eq!(tools[1].duration_ms, Some(20));
        assert_eq!(tools[2].label, "tool result");
        assert!(report.has_unavailable);
    }

    #[test]
    fn live_span_is_provisional_and_uses_one_observation_time() {
        let events = vec![user("2026-01-01T10:00:00.000Z")];
        let live = vec![LiveToolSpan {
            sequence: 1,
            id: "live".to_string(),
            name: "bash".to_string(),
            summary: "cargo test".to_string(),
            input: json!({"command": "cargo test"}),
            output_tail: "checking".to_string(),
            started_at: "2026-01-01T10:00:01.000Z".to_string(),
        }];
        let report = inspect_thread_trajectory_with_live(
            &events,
            &live,
            Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 3).unwrap(),
        );
        let span = &report.turns[0].spans[1];
        assert_eq!(span.status, TrajectoryStatus::Running);
        assert_eq!(span.completed_at, None);
        assert_eq!(span.duration_ms, None);
        assert_eq!(span.wall_ms, Some(2_000));
        assert_eq!(report.bottlenecks[0].duration_ms, 2_000);
    }

    #[test]
    fn incomplete_tool_makes_aggregate_unavailable() {
        let events = vec![
            user("2026-01-01T10:00:00Z"),
            ThreadEvent::tool_use("a", "read", json!({})),
            ThreadEvent::tool_result("a", json!({}), true),
        ];
        let report = inspect_thread_trajectory(&events);
        let lines = format_thread_trajectory_report(&report);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Tool work") && line.contains("0/1 measured"))
        );
    }
}
