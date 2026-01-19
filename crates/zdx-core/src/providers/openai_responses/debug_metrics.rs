//! Debug metrics wrapper for SSE stream instrumentation.
//!
//! When the `ZDX_DEBUG_STREAM` environment variable is set to a file path,
//! this wrapper tracks timing metrics and writes them to the specified file
//! when the stream completes.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::Stream;
use serde::Serialize;

use crate::providers::StreamEvent;

/// How the stream ended.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum CompletionState {
    /// MessageCompleted event received.
    Completed,
    /// Stream ended without MessageCompleted.
    EndedWithoutCompleted,
    /// Stream returned an error.
    Error,
    /// Stream was dropped (cancelled/timeout).
    Dropped,
}

/// Gap duration buckets for pattern analysis.
#[derive(Debug, Default, Clone, Serialize)]
pub struct GapBuckets {
    /// Gaps < 50ms
    pub under_50ms: usize,
    /// Gaps 50-200ms
    pub ms_50_200: usize,
    /// Gaps 200ms-1s
    pub ms_200_1000: usize,
    /// Gaps 1-5s
    pub s_1_5: usize,
    /// Gaps > 5s
    pub over_5s: usize,
}

impl GapBuckets {
    fn record(&mut self, gap: Duration) {
        let ms = gap.as_millis();
        if ms < 50 {
            self.under_50ms += 1;
        } else if ms < 200 {
            self.ms_50_200 += 1;
        } else if ms < 1000 {
            self.ms_200_1000 += 1;
        } else if ms < 5000 {
            self.s_1_5 += 1;
        } else {
            self.over_5s += 1;
        }
    }
}

/// Metrics collected during stream processing.
#[derive(Debug)]
pub struct StreamMetrics {
    /// Time when the stream was created (request started).
    pub stream_start: Instant,
    /// Time to receive the first SSE event.
    pub t_first_event: Option<Duration>,
    /// Time to first assistant output (TextDelta or ContentBlockStart with text).
    pub t_first_output: Option<Duration>,
    /// Time to first TextDelta event specifically.
    pub t_first_text_delta: Option<Duration>,
    /// Time to MessageCompleted event.
    pub t_message_completed: Option<Duration>,
    /// Largest gap between consecutive events (includes consumer delay).
    pub largest_gap: Duration,
    /// Event after which largest gap occurred.
    pub largest_gap_after_event: Option<String>,
    /// Largest gap between Ready events only (server/network pacing).
    pub largest_ready_gap: Duration,
    /// Event after which largest ready gap occurred.
    pub largest_ready_gap_after_event: Option<String>,
    /// Gap buckets for event gaps (pattern analysis).
    pub gap_buckets: GapBuckets,
    /// Total event count.
    pub event_count: usize,
    /// Event counts by type.
    pub event_counts: HashMap<String, usize>,
    /// Time of last event received.
    last_event_time: Option<Instant>,
    /// Time of last Ready(Some(_)) return.
    last_ready_time: Option<Instant>,
    /// Model name (extracted from MessageStart).
    pub model: Option<String>,

    // === Backpressure / poll metrics ===
    /// Time of first poll_next call.
    pub t_first_poll: Option<Duration>,
    /// Total number of poll_next calls.
    pub poll_count: usize,
    /// Number of Poll::Pending returns.
    pub pending_count: usize,
    /// Largest gap between consecutive poll_next calls (consumer/UI stalls).
    pub largest_poll_gap: Duration,
    /// Time of last poll_next call.
    last_poll_time: Option<Instant>,
    /// Current consecutive Pending streak.
    current_pending_streak: usize,
    /// Maximum consecutive Pending streak.
    pub max_pending_streak: usize,

    // === Text throughput metrics ===
    /// Total bytes received via TextDelta (note: bytes, not chars).
    pub text_delta_bytes: usize,
    /// Number of TextDelta events.
    pub text_delta_count: usize,
    /// Time to reach 100 bytes.
    pub t_100_bytes: Option<Duration>,
    /// Time to reach 1000 bytes.
    pub t_1k_bytes: Option<Duration>,

    // === Completion state ===
    /// How the stream ended.
    pub completion_state: CompletionState,
}

impl StreamMetrics {
    fn new() -> Self {
        Self {
            stream_start: Instant::now(),
            t_first_event: None,
            t_first_output: None,
            t_first_text_delta: None,
            t_message_completed: None,
            largest_gap: Duration::ZERO,
            largest_gap_after_event: None,
            largest_ready_gap: Duration::ZERO,
            largest_ready_gap_after_event: None,
            gap_buckets: GapBuckets::default(),
            event_count: 0,
            event_counts: HashMap::new(),
            last_event_time: None,
            last_ready_time: None,
            model: None,
            // Backpressure metrics
            t_first_poll: None,
            poll_count: 0,
            pending_count: 0,
            largest_poll_gap: Duration::ZERO,
            last_poll_time: None,
            current_pending_streak: 0,
            max_pending_streak: 0,
            // Text throughput metrics
            text_delta_bytes: 0,
            text_delta_count: 0,
            t_100_bytes: None,
            t_1k_bytes: None,
            // Completion state
            completion_state: CompletionState::Dropped,
        }
    }

    /// Record a poll_next call (for backpressure detection).
    fn record_poll(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.stream_start);

        if self.t_first_poll.is_none() {
            self.t_first_poll = Some(elapsed);
        }

        self.poll_count += 1;

        if let Some(last_poll) = self.last_poll_time {
            let gap = now.duration_since(last_poll);
            if gap > self.largest_poll_gap {
                self.largest_poll_gap = gap;
            }
        }
        self.last_poll_time = Some(now);
    }

    /// Record a Poll::Pending return.
    fn record_pending(&mut self) {
        self.pending_count += 1;
        self.current_pending_streak += 1;
        if self.current_pending_streak > self.max_pending_streak {
            self.max_pending_streak = self.current_pending_streak;
        }
    }

    /// Record a Poll::Ready return (resets pending streak).
    fn record_ready(&mut self) {
        self.current_pending_streak = 0;
    }

    fn record_event(&mut self, event: &StreamEvent) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.stream_start);

        // Track first event timing
        if self.t_first_event.is_none() {
            self.t_first_event = Some(elapsed);
        }

        // Track gap between events (includes consumer delay)
        if let Some(last_time) = self.last_event_time {
            let gap = now.duration_since(last_time);
            self.gap_buckets.record(gap);
            if gap > self.largest_gap {
                self.largest_gap = gap;
                self.largest_gap_after_event = Some(format!(
                    "event #{} ({})",
                    self.event_count,
                    Self::event_type_name(event)
                ));
            }
        }
        self.last_event_time = Some(now);

        // Track ready gap (server/network pacing only)
        if let Some(last_ready) = self.last_ready_time {
            let ready_gap = now.duration_since(last_ready);
            if ready_gap > self.largest_ready_gap {
                self.largest_ready_gap = ready_gap;
                self.largest_ready_gap_after_event = Some(format!(
                    "event #{} ({})",
                    self.event_count,
                    Self::event_type_name(event)
                ));
            }
        }
        self.last_ready_time = Some(now);

        // Count events
        self.event_count += 1;
        let type_name = Self::event_type_name(event);
        *self.event_counts.entry(type_name.to_string()).or_insert(0) += 1;

        // Track specific event timings
        match event {
            StreamEvent::TextDelta { text, .. } => {
                // Track text throughput (bytes, not chars)
                self.text_delta_count += 1;
                let prev_bytes = self.text_delta_bytes;
                self.text_delta_bytes += text.len();

                // Track byte milestones
                if prev_bytes < 100 && self.text_delta_bytes >= 100 {
                    self.t_100_bytes = Some(elapsed);
                }
                if prev_bytes < 1000 && self.text_delta_bytes >= 1000 {
                    self.t_1k_bytes = Some(elapsed);
                }

                if self.t_first_text_delta.is_none() {
                    self.t_first_text_delta = Some(elapsed);
                }
                if self.t_first_output.is_none() {
                    self.t_first_output = Some(elapsed);
                }
            }
            StreamEvent::ContentBlockStart {
                block_type: crate::providers::ContentBlockType::Text,
                ..
            } => {
                if self.t_first_output.is_none() {
                    self.t_first_output = Some(elapsed);
                }
            }
            StreamEvent::MessageStart { model, .. } => {
                self.model = Some(model.clone());
            }
            StreamEvent::MessageCompleted => {
                self.t_message_completed = Some(elapsed);
                self.completion_state = CompletionState::Completed;
            }
            StreamEvent::Error { .. } => {
                self.completion_state = CompletionState::Error;
            }
            _ => {}
        }
    }

    fn event_type_name(event: &StreamEvent) -> &'static str {
        match event {
            StreamEvent::Ping => "Ping",
            StreamEvent::MessageStart { .. } => "MessageStart",
            StreamEvent::MessageDelta { .. } => "MessageDelta",
            StreamEvent::MessageCompleted => "MessageCompleted",
            StreamEvent::ContentBlockStart { .. } => "ContentBlockStart",
            StreamEvent::ContentBlockCompleted { .. } => "ContentBlockCompleted",
            StreamEvent::TextDelta { .. } => "TextDelta",
            StreamEvent::InputJsonDelta { .. } => "InputJsonDelta",
            StreamEvent::ReasoningDelta { .. } => "ReasoningDelta",
            StreamEvent::ReasoningSignatureDelta { .. } => "ReasoningSignatureDelta",
            StreamEvent::ReasoningCompleted { .. } => "ReasoningCompleted",
            StreamEvent::Error { .. } => "Error",
        }
    }

    fn format_duration(d: Duration) -> String {
        if d.as_secs() > 0 {
            format!("{:.2}s", d.as_secs_f64())
        } else {
            format!("{}ms", d.as_millis())
        }
    }

    fn write_to_file(&self, path: &str) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;

        let total_duration = self.stream_start.elapsed();
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");

        writeln!(
            file,
            "\n============================================================"
        )?;
        writeln!(file, "=== Stream Metrics @ {} ===", timestamp)?;
        writeln!(
            file,
            "============================================================"
        )?;

        if let Some(model) = &self.model {
            writeln!(file, "Model: {}", model)?;
        }
        writeln!(file, "Completion: {:?}", self.completion_state)?;

        writeln!(file, "\n--- Timing ---")?;
        writeln!(
            file,
            "Total duration:     {}",
            Self::format_duration(total_duration)
        )?;
        if let Some(t) = self.t_first_event {
            writeln!(file, "First event:        {}", Self::format_duration(t))?;
        }
        if let Some(t) = self.t_first_output {
            writeln!(file, "First output:       {}", Self::format_duration(t))?;
        }
        if let Some(t) = self.t_first_text_delta {
            writeln!(file, "First text delta:   {}", Self::format_duration(t))?;
        }
        if let Some(t) = self.t_message_completed {
            writeln!(file, "Message completed:  {}", Self::format_duration(t))?;
        }

        // Gap analysis (server vs consumer)
        writeln!(file, "\n--- Gap Analysis ---")?;
        writeln!(
            file,
            "Largest event gap:  {} (includes consumer delay)",
            Self::format_duration(self.largest_gap)
        )?;
        if let Some(ref after) = self.largest_gap_after_event {
            writeln!(file, "  (after {})", after)?;
        }
        writeln!(
            file,
            "Largest ready gap:  {} (server/network only)",
            Self::format_duration(self.largest_ready_gap)
        )?;
        if let Some(ref after) = self.largest_ready_gap_after_event {
            writeln!(file, "  (after {})", after)?;
        }

        // Gap buckets
        writeln!(file, "\n--- Gap Buckets ---")?;
        writeln!(file, "  <50ms:      {}", self.gap_buckets.under_50ms)?;
        writeln!(file, "  50-200ms:   {}", self.gap_buckets.ms_50_200)?;
        writeln!(file, "  200ms-1s:   {}", self.gap_buckets.ms_200_1000)?;
        writeln!(file, "  1-5s:       {}", self.gap_buckets.s_1_5)?;
        writeln!(file, "  >5s:        {}", self.gap_buckets.over_5s)?;

        // Text throughput milestones
        writeln!(file, "\n--- Text Throughput ---")?;
        writeln!(file, "TextDelta count:    {}", self.text_delta_count)?;
        writeln!(file, "Total bytes:        {}", self.text_delta_bytes)?;
        if let Some(t) = self.t_100_bytes {
            writeln!(file, "Time to 100 bytes:  {}", Self::format_duration(t))?;
        }
        if let Some(t) = self.t_1k_bytes {
            writeln!(file, "Time to 1k bytes:   {}", Self::format_duration(t))?;
        }
        if total_duration.as_secs_f64() > 0.0 {
            let bytes_per_sec = self.text_delta_bytes as f64 / total_duration.as_secs_f64();
            writeln!(file, "Bytes/second:       {:.1}", bytes_per_sec)?;
        }

        // Backpressure / poll metrics
        writeln!(file, "\n--- Poll Metrics (backpressure) ---")?;
        if let Some(t) = self.t_first_poll {
            writeln!(file, "First poll:         {}", Self::format_duration(t))?;
        }
        writeln!(file, "Poll count:         {}", self.poll_count)?;
        writeln!(file, "Pending count:      {}", self.pending_count)?;
        writeln!(file, "Max pending streak: {}", self.max_pending_streak)?;
        writeln!(
            file,
            "Largest poll gap:   {}",
            Self::format_duration(self.largest_poll_gap)
        )?;
        if self.poll_count > 0 && self.pending_count > 0 {
            let ready_ratio =
                (self.poll_count - self.pending_count) as f64 / self.poll_count as f64;
            writeln!(file, "Ready ratio:        {:.1}%", ready_ratio * 100.0)?;
        }

        writeln!(file, "\n--- Event Counts ({} total) ---", self.event_count)?;
        let mut counts: Vec<_> = self.event_counts.iter().collect();
        counts.sort_by(|a, b| b.1.cmp(a.1));
        for (event_type, count) in counts {
            writeln!(file, "  {:<25} {}", event_type, count)?;
        }

        writeln!(file)?;
        Ok(())
    }

    /// Write metrics as a single JSONL line for easy diffing/plotting.
    fn write_jsonl(&self, path: &str) -> std::io::Result<()> {
        let jsonl_path = format!("{}.jsonl", path.trim_end_matches(".jsonl"));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jsonl_path)?;

        let total_duration = self.stream_start.elapsed();
        let timestamp = chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%.3f")
            .to_string();

        // Clone fields that would be moved by serde_json::json!
        let model = self.model.clone();
        let event_counts = self.event_counts.clone();
        let largest_gap_after = self.largest_gap_after_event.clone();
        let largest_ready_gap_after = self.largest_ready_gap_after_event.clone();
        let gap_buckets = self.gap_buckets.clone();

        let record = serde_json::json!({
            "timestamp": timestamp,
            "model": model,
            "completion_state": format!("{:?}", self.completion_state),
            "timing": {
                "total_ms": total_duration.as_millis(),
                "first_event_ms": self.t_first_event.map(|d| d.as_millis()),
                "first_output_ms": self.t_first_output.map(|d| d.as_millis()),
                "first_text_delta_ms": self.t_first_text_delta.map(|d| d.as_millis()),
                "message_completed_ms": self.t_message_completed.map(|d| d.as_millis()),
            },
            "gaps": {
                "largest_gap_ms": self.largest_gap.as_millis(),
                "largest_gap_after": largest_gap_after,
                "largest_ready_gap_ms": self.largest_ready_gap.as_millis(),
                "largest_ready_gap_after": largest_ready_gap_after,
                "buckets": gap_buckets,
            },
            "text_throughput": {
                "delta_count": self.text_delta_count,
                "total_bytes": self.text_delta_bytes,
                "t_100_bytes_ms": self.t_100_bytes.map(|d| d.as_millis()),
                "t_1k_bytes_ms": self.t_1k_bytes.map(|d| d.as_millis()),
                "bytes_per_second": if total_duration.as_secs_f64() > 0.0 {
                    Some(self.text_delta_bytes as f64 / total_duration.as_secs_f64())
                } else {
                    None
                },
            },
            "backpressure": {
                "first_poll_ms": self.t_first_poll.map(|d| d.as_millis()),
                "poll_count": self.poll_count,
                "pending_count": self.pending_count,
                "max_pending_streak": self.max_pending_streak,
                "largest_poll_gap_ms": self.largest_poll_gap.as_millis(),
            },
            "events": {
                "total": self.event_count,
                "by_type": event_counts,
            }
        });

        writeln!(
            file,
            "{}",
            serde_json::to_string(&record).unwrap_or_default()
        )?;
        Ok(())
    }
}

/// Wrapper stream that tracks timing metrics for debugging.
pub struct MetricsStream<S> {
    inner: S,
    metrics: StreamMetrics,
    output_path: String,
    completed: bool,
}

impl<S> MetricsStream<S> {
    pub fn new(inner: S, output_path: String) -> Self {
        Self {
            inner,
            metrics: StreamMetrics::new(),
            output_path,
            completed: false,
        }
    }

    /// Write both human-readable and JSONL metrics.
    fn write_metrics(&self) {
        if let Err(e) = self.metrics.write_to_file(&self.output_path) {
            eprintln!("Failed to write stream metrics: {}", e);
        }
        if let Err(e) = self.metrics.write_jsonl(&self.output_path) {
            eprintln!("Failed to write stream metrics JSONL: {}", e);
        }
    }
}

impl<S> Drop for MetricsStream<S> {
    fn drop(&mut self) {
        // Skip writing during panic unwinding to avoid nested panics
        if std::thread::panicking() {
            return;
        }

        if !self.completed {
            // Stream was dropped before completion (possibly cancelled)
            // completion_state defaults to Dropped, so just write metrics
            self.write_metrics();
        }
    }
}

impl<S> Stream for MetricsStream<S>
where
    S: Stream<Item = Result<StreamEvent>> + Unpin,
{
    type Item = Result<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Record poll timing for backpressure detection
        self.metrics.record_poll();

        let inner = Pin::new(&mut self.inner);
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                self.metrics.record_ready();
                self.metrics.record_event(&event);

                // Check for stream completion
                if matches!(event, StreamEvent::MessageCompleted) {
                    self.completed = true;
                    self.write_metrics();
                }

                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(e))) => {
                self.metrics.record_ready();
                // Record error and write metrics
                self.metrics.completion_state = CompletionState::Error;
                self.completed = true;
                self.write_metrics();
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.metrics.record_ready();
                // Stream ended without MessageCompleted
                if !self.completed {
                    if !matches!(self.metrics.completion_state, CompletionState::Completed) {
                        self.metrics.completion_state = CompletionState::EndedWithoutCompleted;
                    }
                    self.completed = true;
                    self.write_metrics();
                }
                Poll::Ready(None)
            }
            Poll::Pending => {
                self.metrics.record_pending();
                Poll::Pending
            }
        }
    }
}

/// Returns the debug stream output path if `ZDX_DEBUG_STREAM` is set.
pub fn debug_stream_path() -> Option<String> {
    std::env::var("ZDX_DEBUG_STREAM")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Wraps a stream with metrics tracking if `ZDX_DEBUG_STREAM` is set.
/// Otherwise returns the stream unchanged.
pub fn maybe_wrap_with_metrics<S>(
    stream: S,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>
where
    S: Stream<Item = Result<StreamEvent>> + Send + Unpin + 'static,
{
    if let Some(path) = debug_stream_path() {
        Box::pin(MetricsStream::new(stream, path))
    } else {
        Box::pin(stream)
    }
}
