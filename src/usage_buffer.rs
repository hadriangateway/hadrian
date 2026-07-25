//! Async usage log buffering for high-throughput scenarios.
//!
//! This module provides a buffer that collects usage log entries and flushes them
//! to configured sinks (database, OTLP, etc.) in batches.
//!
//! ## Features
//! - Configurable buffer size (default: 1000 entries)
//! - Configurable flush interval (default: 1 second)
//! - Async-safe with proper shutdown handling
//! - Multiple sink support (database + OTLP simultaneously)
//! - Graceful shutdown flushes remaining entries
//! - **Lock-free push**: Uses crossbeam channel for contention-free writes
//!
//! ## Performance
//! At high request rates, batching reduces write pressure:
//! - 100K requests/sec → ~100 batch operations/sec (1000x reduction)
//! - Lock-free MPSC channel eliminates mutex contention on push()
//!
//! ## Availability
//! `UsageLogBuffer` requires the `concurrency` feature (crossbeam channels).
//! Without it (e.g. WASM builds), only `UsageBufferConfig` is available and
//! usage entries are written directly to the database without buffering.

use std::time::Duration;

/// Configuration for the usage log buffer.
#[derive(Debug, Clone)]
pub struct UsageBufferConfig {
    /// Maximum number of entries to buffer before flushing.
    /// Default: 1000
    pub max_size: usize,
    /// Maximum time to wait before flushing the buffer.
    /// Default: 1 second
    pub flush_interval: Duration,
    /// Maximum pending entries before dropping new entries.
    /// When the sink is slow or unavailable, prevents unbounded memory growth.
    /// Default: 10,000 (10x max_size)
    pub max_pending_entries: usize,
}

impl Default for UsageBufferConfig {
    fn default() -> Self {
        Self {
            max_size: 1000,
            flush_interval: Duration::from_secs(1),
            max_pending_entries: 10_000,
        }
    }
}

impl From<&crate::config::UsageBufferConfig> for UsageBufferConfig {
    fn from(config: &crate::config::UsageBufferConfig) -> Self {
        Self {
            max_size: config.max_size,
            flush_interval: Duration::from_millis(config.flush_interval_ms),
            max_pending_entries: config.max_pending_entries,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UsageLogBuffer — requires `concurrency` feature (crossbeam channels)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "concurrency")]
mod buffer {
    use std::sync::Arc;

    use chrono::Utc;
    use crossbeam_channel::{Receiver, Sender, TrySendError};
    use uuid::Uuid;

    use super::UsageBufferConfig;
    use crate::{
        events::{EventBus, ServerEvent},
        models::UsageLogEntry,
        usage_sink::UsageSink,
    };

    /// Async buffer for usage log entries.
    ///
    /// Entries are collected and flushed to configured sinks in batches.
    /// The buffer flushes when:
    /// - The buffer reaches `max_size` entries
    /// - The `flush_interval` timer expires
    /// - `shutdown()` is called (during graceful shutdown)
    ///
    /// Uses a lock-free MPSC channel for push operations, eliminating mutex
    /// contention under high load. If the channel is full (exceeds `max_pending_entries`),
    /// new entries are dropped to prevent OOM.
    pub struct UsageLogBuffer {
        /// Lock-free sender for push operations.
        sender: Sender<UsageLogEntry>,
        /// Receiver for the background worker (only used by start_worker).
        receiver: Receiver<UsageLogEntry>,
        config: UsageBufferConfig,
        /// Flag to signal shutdown.
        shutdown: Arc<std::sync::atomic::AtomicBool>,
        /// Optional event bus for publishing usage events.
        event_bus: Option<Arc<EventBus>>,
        /// Count of entries dropped due to buffer overflow.
        dropped_count: std::sync::atomic::AtomicU64,
    }

    impl UsageLogBuffer {
        /// Create a new usage log buffer with the given configuration.
        pub fn new(config: UsageBufferConfig) -> Self {
            // Use max_pending_entries as channel capacity, or a reasonable default if 0
            let capacity = if config.max_pending_entries > 0 {
                config.max_pending_entries
            } else {
                // Unbounded is risky; use a large but bounded capacity
                1_000_000
            };
            let (sender, receiver) = crossbeam_channel::bounded(capacity);

            Self {
                sender,
                receiver,
                config,
                shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                event_bus: None,
                dropped_count: std::sync::atomic::AtomicU64::new(0),
            }
        }

        /// Create a new usage log buffer with EventBus for real-time notifications.
        pub fn with_event_bus(config: UsageBufferConfig, event_bus: Arc<EventBus>) -> Self {
            let capacity = if config.max_pending_entries > 0 {
                config.max_pending_entries
            } else {
                1_000_000
            };
            let (sender, receiver) = crossbeam_channel::bounded(capacity);

            Self {
                sender,
                receiver,
                config,
                shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                event_bus: Some(event_bus),
                dropped_count: std::sync::atomic::AtomicU64::new(0),
            }
        }

        /// Add a usage entry to the buffer.
        ///
        /// This is a **lock-free** operation using a crossbeam channel.
        /// Multiple threads can call this concurrently without contention.
        ///
        /// If the channel has exceeded `max_pending_entries`, the entry is dropped.
        pub fn push(&self, entry: UsageLogEntry) {
            match self.sender.try_send(entry) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    #[cfg(feature = "prometheus")]
                    metrics::counter!("hadrian_usage_buffer_entries_dropped_total").increment(1);
                    let count = self
                        .dropped_count
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if count == 0 || count.is_multiple_of(10) {
                        tracing::warn!(
                            dropped_count = count + 1,
                            max_pending = self.config.max_pending_entries,
                            "Usage buffer overflow: dropping entries (sink may be slow/unavailable)"
                        );
                    }
                }
                Err(TrySendError::Disconnected(_)) => {
                    // Channel closed - worker has shut down, silently drop
                }
            }
        }

        /// Get the count of entries dropped due to buffer overflow.
        pub fn dropped_count(&self) -> u64 {
            self.dropped_count
                .load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Start the background flush worker.
        ///
        /// This spawns a task that periodically flushes the buffer to all
        /// configured sinks. The worker will run until `shutdown()` is called.
        pub fn start_worker(
            self: &Arc<Self>,
            sink: Arc<dyn UsageSink>,
        ) -> tokio::task::JoinHandle<()> {
            let buffer = Arc::clone(self);
            let flush_interval = self.config.flush_interval;
            let max_batch_size = self.config.max_size;

            tokio::spawn(async move {
                let mut batch = Vec::with_capacity(max_batch_size);

                loop {
                    // Drain available entries up to batch size
                    buffer.drain_entries(&mut batch, max_batch_size);

                    // If we have entries, flush them
                    if !batch.is_empty() {
                        buffer.flush_batch(&sink, &mut batch).await;
                    }

                    // Check for shutdown
                    if buffer.shutdown.load(std::sync::atomic::Ordering::Acquire) {
                        // Final drain and flush before exiting
                        buffer.drain_all(&mut batch);
                        if !batch.is_empty() {
                            buffer.flush_batch(&sink, &mut batch).await;
                        }
                        let total_dropped = buffer
                            .dropped_count
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if total_dropped > 0 {
                            tracing::warn!(
                                total_dropped,
                                "Usage buffer dropped entries during lifetime"
                            );
                        }
                        tracing::info!("Usage log buffer worker shutting down");
                        break;
                    }

                    // Wait for flush interval or shutdown
                    tokio::time::sleep(flush_interval).await;
                }
            })
        }

        /// Drain entries from the channel into the batch vector.
        fn drain_entries(&self, batch: &mut Vec<UsageLogEntry>, max_size: usize) {
            while batch.len() < max_size {
                match self.receiver.try_recv() {
                    Ok(entry) => batch.push(entry),
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        }

        /// Drain all remaining entries from the channel.
        fn drain_all(&self, batch: &mut Vec<UsageLogEntry>) {
            while let Ok(entry) = self.receiver.try_recv() {
                batch.push(entry);
            }
        }

        /// Signal the worker to shut down.
        pub fn shutdown(&self) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Release);
        }

        /// Flush a batch of entries to the sink.
        async fn flush_batch(&self, sink: &Arc<dyn UsageSink>, batch: &mut Vec<UsageLogEntry>) {
            let entry_count = batch.len();
            tracing::debug!(count = entry_count, "Flushing usage log buffer");

            // Publish usage events to WebSocket subscribers before writing to sink
            if let Some(event_bus) = &self.event_bus {
                for entry in batch.iter() {
                    event_bus.publish(ServerEvent::UsageRecorded {
                        request_id: Uuid::parse_str(&entry.request_id)
                            .unwrap_or_else(|_| Uuid::nil()),
                        timestamp: Utc::now(),
                        model: entry.model.clone(),
                        provider: entry.provider.clone(),
                        input_tokens: entry.input_tokens,
                        output_tokens: entry.output_tokens,
                        cost_microcents: entry.cost_microcents,
                        user_id: entry.user_id,
                        org_id: entry.org_id,
                        project_id: entry.project_id,
                        team_id: entry.team_id,
                        service_account_id: entry.service_account_id,
                    });
                }
            }

            // Write to sink(s)
            match sink.write_batch(batch).await {
                Ok(written) => {
                    tracing::debug!(
                        written = written,
                        total = entry_count,
                        "Usage log flush successful"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        count = entry_count,
                        "Usage log flush failed"
                    );
                }
            }

            batch.clear();
        }

        /// Get the current number of buffered entries.
        #[allow(dead_code)] // Used in tests; public API for buffer introspection
        pub fn len(&self) -> usize {
            self.receiver.len()
        }

        /// Check if the buffer is empty.
        #[allow(dead_code)] // Used in tests; public API for buffer introspection
        pub fn is_empty(&self) -> bool {
            self.receiver.is_empty()
        }
    }

    #[cfg(test)]
    mod tests {
        use std::time::Duration;

        use chrono::Utc;
        use uuid::Uuid;

        use super::*;

        fn make_test_entry() -> UsageLogEntry {
            UsageLogEntry {
                request_id: Uuid::new_v4().to_string(),
                client_request_id: None,
                api_key_id: Some(Uuid::new_v4()),
                user_id: None,
                org_id: None,
                project_id: None,
                team_id: None,
                service_account_id: None,
                model: "test-model".to_string(),
                provider: "test-provider".to_string(),
                input_tokens: 100,
                output_tokens: 50,
                cost_microcents: Some(1000),
                http_referer: None,
                request_at: Utc::now(),
                streamed: false,
                cached_tokens: 0,
                reasoning_tokens: 0,
                finish_reason: Some("stop".to_string()),
                latency_ms: Some(100),
                cancelled: false,
                status_code: Some(200),
                pricing_source: crate::pricing::CostPricingSource::None,
                image_count: None,
                audio_seconds: None,
                character_count: None,
                provider_source: None,
                record_type: "model".to_string(),
                tool_name: None,
                tool_query: None,
                tool_url: None,
                tool_bytes_fetched: None,
                tool_results_count: None,
                tool_runtime_seconds: None,
                tool_exit_code: None,
            }
        }

        #[test]
        fn test_buffer_push_and_len() {
            let buffer = UsageLogBuffer::new(UsageBufferConfig::default());

            assert!(buffer.is_empty());
            assert_eq!(buffer.len(), 0);

            buffer.push(make_test_entry());
            assert_eq!(buffer.len(), 1);

            buffer.push(make_test_entry());
            assert_eq!(buffer.len(), 2);
        }

        #[test]
        fn test_buffer_config_defaults() {
            let config = UsageBufferConfig::default();
            assert_eq!(config.max_size, 1000);
            assert_eq!(config.flush_interval, Duration::from_secs(1));
            assert_eq!(config.max_pending_entries, 10_000);
        }

        #[test]
        fn test_buffer_with_custom_config() {
            let config = UsageBufferConfig {
                max_size: 100,
                flush_interval: Duration::from_millis(500),
                max_pending_entries: 1000,
            };
            let buffer = UsageLogBuffer::new(config);

            // Push entries up to the limit
            for _ in 0..99 {
                buffer.push(make_test_entry());
            }
            assert_eq!(buffer.len(), 99);
        }

        #[test]
        fn test_buffer_overflow_drops_new_entries() {
            let config = UsageBufferConfig {
                max_size: 10,
                flush_interval: Duration::from_secs(60), // Long interval so no auto-flush
                max_pending_entries: 5,                  // Small limit for testing
            };
            let buffer = UsageLogBuffer::new(config);

            // Push 5 entries (reaches channel capacity)
            for _ in 0..5 {
                buffer.push(make_test_entry());
            }
            assert_eq!(buffer.len(), 5);
            assert_eq!(buffer.dropped_count(), 0);

            // Push one more - should be dropped (channel full)
            buffer.push(make_test_entry());
            assert_eq!(buffer.len(), 5); // Still 5 (new entry dropped)
            assert_eq!(buffer.dropped_count(), 1);

            // Push 3 more - all should be dropped
            for _ in 0..3 {
                buffer.push(make_test_entry());
            }
            assert_eq!(buffer.len(), 5); // Still capped at 5
            assert_eq!(buffer.dropped_count(), 4);
        }

        #[test]
        fn test_buffer_large_capacity_when_zero() {
            let config = UsageBufferConfig {
                max_size: 100,
                flush_interval: Duration::from_secs(60),
                max_pending_entries: 0, // Uses large default capacity
            };
            let buffer = UsageLogBuffer::new(config);

            // Push many entries - should not drop any (large capacity)
            for _ in 0..200 {
                buffer.push(make_test_entry());
            }
            assert_eq!(buffer.len(), 200);
            assert_eq!(buffer.dropped_count(), 0);
        }

        #[test]
        fn test_drain_entries() {
            let config = UsageBufferConfig {
                max_size: 10,
                flush_interval: Duration::from_secs(60),
                max_pending_entries: 100,
            };
            let buffer = UsageLogBuffer::new(config);

            // Push 15 entries
            for _ in 0..15 {
                buffer.push(make_test_entry());
            }
            assert_eq!(buffer.len(), 15);

            // Drain up to 10 (max_size)
            let mut batch = Vec::new();
            buffer.drain_entries(&mut batch, 10);
            assert_eq!(batch.len(), 10);
            assert_eq!(buffer.len(), 5); // 5 remaining

            // Drain the rest
            batch.clear();
            buffer.drain_entries(&mut batch, 10);
            assert_eq!(batch.len(), 5);
            assert_eq!(buffer.len(), 0);
        }
    }
}

#[cfg(feature = "concurrency")]
pub use buffer::UsageLogBuffer;
