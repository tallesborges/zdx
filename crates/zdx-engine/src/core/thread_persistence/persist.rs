use tokio::task::JoinHandle;

use super::event::{ThreadEvent, Usage};
use super::replay::emit_message_events;
use super::storage::Thread;
use crate::core::agent::AgentEventRx;

/// Spawns a thread persistence task that consumes events from a channel.
///
/// The task owns the `Thread` and persists relevant events until the channel closes.
/// Returns a `JoinHandle` that resolves when all events have been persisted.
///
/// Only tool-related and interrupt events are persisted via this task.
/// User and assistant messages are handled separately by the chat/agent modules.
///
/// # Example
///
/// ```ignore
/// let thread = Thread::new_with_root(Path::new("."))?;
/// let (tx, rx) = agent::create_event_channel();
/// let persist_handle = spawn_thread_persist_task(thread, rx);
///
/// // ... send events to tx ...
/// drop(tx); // Close channel
///
/// persist_handle.await.unwrap(); // Wait for persistence to finish
/// ```
pub fn spawn_thread_persist_task(mut thread: Thread, mut rx: AgentEventRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut usage_persistor = UsagePersistor::new();
        while let Some(event) = rx.recv().await {
            for thread_event in usage_persistor.handle_event(&event) {
                // Best-effort persistence - log errors but don't panic
                if let Err(e) = thread.append(&thread_event) {
                    tracing::warn!(%e, "Failed to persist thread event");
                }
            }
        }

        for thread_event in usage_persistor.finish() {
            if let Err(e) = thread.append(&thread_event) {
                tracing::warn!(%e, "Failed to persist thread event");
            }
        }
    })
}

#[derive(Debug, Default)]
pub(crate) struct UsagePersistor {
    pending: Option<Usage>,
    /// Model/provider from the most recent `UsageUpdate`, attached to every
    /// emitted usage event (including the trailing `finish()` flush and
    /// output-only usage) so attribution survives across flush boundaries.
    /// `None` until the first attributed usage arrives.
    current_model: Option<String>,
    current_provider: Option<String>,
    /// Index into `messages` already flushed to disk for this run.
    /// Combined with the run-entry `prior_message_count` cursor (carried on
    /// every `TurnCheckpoint`/`TurnFinished`), this makes flushes idempotent
    /// across repeated checkpoints — `start = max(prior_count, last_persisted)`.
    last_persisted_index: usize,
}

impl UsagePersistor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn handle_event(&mut self, event: &crate::core::events::AgentEvent) -> Vec<ThreadEvent> {
        use crate::core::events::AgentEvent;

        let mut events = Vec::new();

        match event {
            AgentEvent::UsageUpdate {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model,
                provider,
                duration_ms,
                ttft_ms,
            } => {
                self.current_model = (!model.is_empty()).then(|| model.clone());
                self.current_provider = (!provider.is_empty()).then(|| provider.clone());

                if *input_tokens > 0
                    || *cache_read_input_tokens > 0
                    || *cache_creation_input_tokens > 0
                {
                    self.flush_pending(&mut events);
                    self.pending = Some(Usage::new(
                        *input_tokens,
                        0,
                        *cache_read_input_tokens,
                        *cache_creation_input_tokens,
                    ));
                }

                if *output_tokens > 0 {
                    if let Some(mut usage) = self.pending.take() {
                        usage.output += *output_tokens;
                        events.push(self.usage_event(usage, *duration_ms, *ttft_ms));
                    } else {
                        events.push(self.usage_event(
                            Usage::new(0, *output_tokens, 0, 0),
                            *duration_ms,
                            *ttft_ms,
                        ));
                    }
                }
            }
            AgentEvent::TurnCheckpoint {
                messages,
                prior_message_count,
            } => {
                self.flush_pending(&mut events);
                self.flush_messages(messages, *prior_message_count, &mut events);
            }
            AgentEvent::TurnFinished {
                messages,
                prior_message_count,
                ..
            } => {
                self.flush_pending(&mut events);
                self.flush_messages(messages, *prior_message_count, &mut events);
                if let Some(thread_event) = ThreadEvent::from_agent(event) {
                    events.push(thread_event);
                }
            }
            _ => {
                if let Some(thread_event) = ThreadEvent::from_agent(event) {
                    events.push(thread_event);
                }
            }
        }

        events
    }

    /// Walks the new turn-suffix of `messages` (from `max(prior_count,
    /// last_persisted_index)` to the end) and emits ordered `ThreadEvent`s
    /// for each block of each `ChatMessage`. Idempotent across repeated
    /// checkpoints because `last_persisted_index` advances monotonically.
    /// Interrupted turns may produce a final snapshot shorter than an earlier
    /// checkpoint; in that case there is nothing new to flush.
    pub(crate) fn flush_messages(
        &mut self,
        full: &[crate::providers::ChatMessage],
        prior_count: usize,
        events: &mut Vec<ThreadEvent>,
    ) {
        let start = std::cmp::max(prior_count, self.last_persisted_index);
        if start >= full.len() {
            return;
        }

        for msg in &full[start..] {
            emit_message_events(msg, events);
        }

        self.last_persisted_index = full.len();
    }

    fn flush_pending(&mut self, events: &mut Vec<ThreadEvent>) {
        if let Some(usage) = self.pending.take()
            && usage.total() > 0
        {
            events.push(self.usage_event(usage, None, None));
        }
    }

    /// Builds a usage `ThreadEvent`, attaching the current model/provider and
    /// optional per-request latency (present only on terminal usage events).
    fn usage_event(
        &self,
        usage: Usage,
        duration_ms: Option<u64>,
        ttft_ms: Option<u64>,
    ) -> ThreadEvent {
        ThreadEvent::usage(
            usage,
            self.current_model.clone(),
            self.current_provider.clone(),
            duration_ms,
            ttft_ms,
        )
    }

    fn finish(&mut self) -> Vec<ThreadEvent> {
        let mut events = Vec::new();
        self.flush_pending(&mut events);
        events
    }
}
