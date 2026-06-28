pub mod sse_buffer;

use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::Value;
pub use sse_buffer::SseBuffer;
#[cfg(feature = "server")]
use tokio::sync::mpsc;
use tokio::time::Sleep;
#[cfg(feature = "server")]
use tokio_util::task::TaskTracker;

use crate::{db::DbPool, models::UsageLogEntry, observability::metrics, pricing::PricingConfig};

/// Default capacity for the usage-drain channel.
///
/// Each pending job holds two `Arc`s, so memory pressure is small. The cap is
/// here to bound the worst case if the drainer falls behind — under normal
/// operation it stays empty.
#[cfg(feature = "server")]
pub const USAGE_DRAIN_CAPACITY: usize = 4096;

/// A handle to the usage-drain background task.
///
/// `UsageTrackingStream::Drop` runs synchronously and is not guaranteed to be
/// called from within a Tokio runtime context (clients can disconnect on a
/// thread that's tearing down, or the future can be cancelled in
/// `poll_cancel`). Spawning a task directly from `Drop` therefore risks a
/// `there is no reactor running` panic and also unbounded fan-out under heavy
/// disconnect storms.
///
/// Instead, drops push a job into a bounded mpsc channel; a single drainer
/// task spawned at startup (owned by the existing `TaskTracker` so graceful
/// shutdown awaits it) pulls jobs and runs `UsageLogger::log_usage` from
/// inside the runtime where spawning is safe.
#[cfg(feature = "server")]
#[derive(Clone)]
pub struct UsageDrainHandle {
    tx: mpsc::Sender<UsageDrainJob>,
}

#[cfg(feature = "server")]
struct UsageDrainJob {
    logger: Arc<UsageLogger>,
    tokens: Arc<TokenAccumulator>,
}

#[cfg(feature = "server")]
impl UsageDrainHandle {
    /// Spawn the drainer task and return a clonable handle for sending jobs.
    pub fn spawn(task_tracker: &TaskTracker, capacity: usize) -> Self {
        let (tx, mut rx) = mpsc::channel::<UsageDrainJob>(capacity);
        task_tracker.spawn(async move {
            while let Some(job) = rx.recv().await {
                job.logger.log_usage(&job.tokens).await;
            }
            tracing::debug!("Usage drain channel closed; drainer exiting");
        });
        Self { tx }
    }

    /// Sync-send a usage log job. Safe to call from any thread/context,
    /// including `Drop`. Drops the job (with a warning) if the channel is
    /// full or closed — this is preferable to panicking from a destructor.
    fn try_log(&self, logger: Arc<UsageLogger>, tokens: Arc<TokenAccumulator>) {
        if let Err(err) = self.tx.try_send(UsageDrainJob { logger, tokens }) {
            tracing::warn!(
                error = %err,
                "Usage drain channel rejected job; partial usage will not be recorded"
            );
        }
    }
}

/// Sentinel value indicating an optional field is not set
const NONE_SENTINEL: i64 = i64::MIN;

/// Multiplier to convert dollars to nano-dollars for atomic storage
const NANODOLLARS_MULTIPLIER: f64 = 1_000_000_000.0;

// ============================================================================
// Idle Timeout Stream
// ============================================================================

/// Error returned when a streaming response times out.
#[derive(Debug, thiserror::Error)]
#[error("streaming idle timeout: no chunk received within {0:?}")]
pub struct IdleTimeoutError(Duration);

/// A stream wrapper that enforces an idle timeout between chunks.
///
/// If no chunk is yielded from the inner stream within the specified timeout,
/// the stream returns an error and terminates. This protects against:
///
/// - Stalled upstream providers that stop sending data
/// - Connection pool exhaustion from hung streaming requests
/// - Slow client attacks (when combined with proper TCP settings)
///
/// The timeout resets after each successful chunk, so long-running streams
/// that are actively producing data will not timeout.
pub struct IdleTimeoutStream<S> {
    /// `None` once the stream has terminated, dropping the inner stream so any
    /// upstream resources (sockets, channels) are released immediately.
    inner: Option<S>,
    timeout: Duration,
    /// Sleep future for the current timeout period.
    /// Pinned because Sleep requires pinning.
    sleep: Pin<Box<Sleep>>,
}

impl<S> IdleTimeoutStream<S>
where
    S: Stream + Unpin,
{
    /// Create a new IdleTimeoutStream wrapping the inner stream.
    ///
    /// If `timeout` is zero, the wrapper is effectively a no-op pass-through.
    pub fn new(inner: S, timeout: Duration) -> Self {
        Self {
            inner: Some(inner),
            timeout,
            sleep: Box::pin(tokio::time::sleep(timeout)),
        }
    }

    /// Check if idle timeout is enabled (non-zero duration).
    fn timeout_enabled(&self) -> bool {
        !self.timeout.is_zero()
    }
}

impl<S, T, E> Stream for IdleTimeoutStream<S>
where
    S: Stream<Item = Result<T, E>> + Unpin,
    E: From<io::Error>,
{
    type Item = Result<T, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.inner.is_none() {
            return Poll::Ready(None);
        }

        // If timeout is disabled (zero), just pass through
        if !self.timeout_enabled() {
            return Pin::new(self.inner.as_mut().expect("checked above")).poll_next(cx);
        }

        // Poll the inner stream first
        let inner = self.inner.as_mut().expect("checked above");
        match Pin::new(inner).poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => {
                // Got a chunk - reset the timeout
                let new_deadline = tokio::time::Instant::now() + self.timeout;
                self.sleep.as_mut().reset(new_deadline);
                Poll::Ready(Some(Ok(item)))
            }
            Poll::Ready(Some(Err(e))) => {
                self.inner = None;
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.inner = None;
                Poll::Ready(None)
            }
            Poll::Pending => {
                // Stream is waiting for data - check if we've timed out
                match self.sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        // Timeout elapsed - drop the inner stream so its
                        // socket/connection releases instead of lingering.
                        self.inner = None;
                        tracing::warn!(
                            timeout_secs = self.timeout.as_secs(),
                            "Streaming response idle timeout - terminating stalled stream"
                        );
                        metrics::record_gateway_error("streaming", "idle_timeout", None);
                        let err =
                            io::Error::new(io::ErrorKind::TimedOut, IdleTimeoutError(self.timeout));
                        Poll::Ready(Some(Err(err.into())))
                    }
                    Poll::Pending => {
                        // Still waiting for either data or timeout
                        Poll::Pending
                    }
                }
            }
        }
    }
}

// ============================================================================
// SSE Parsing
// ============================================================================

/// Parser for Server-Sent Events (SSE) format used by OpenAI streaming
pub struct SseParser;

impl SseParser {
    /// Parse an SSE chunk and extract token data
    /// OpenAI sends chunks like: data: {"choices":[{"delta":{"content":"hello"}}]}
    pub fn parse_chunk(chunk: &[u8]) -> Option<SseChunk> {
        let chunk_str = std::str::from_utf8(chunk).ok()?;

        // SSE format: "data: {json}\n\n"
        for line in chunk_str.lines() {
            if let Some(json_str) = line.strip_prefix("data: ") {
                // Check for done signal
                if json_str.trim() == "[DONE]" {
                    return Some(SseChunk::Done);
                }

                // Parse JSON
                if let Ok(json) = serde_json::from_str::<Value>(json_str) {
                    // Extract usage if present (sent in final chunk by OpenAI/OpenRouter)
                    // Check both root level and nested in response object (response.completed format)
                    let usage = json
                        .get("usage")
                        .or_else(|| json.get("response").and_then(|r| r.get("usage")));

                    if let Some(usage) = usage {
                        let prompt_tokens = usage
                            .get("prompt_tokens")
                            .or_else(|| usage.get("input_tokens"))
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let completion_tokens = usage
                            .get("completion_tokens")
                            .or_else(|| usage.get("output_tokens"))
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);

                        // Extract provider-reported cost (OpenRouter format)
                        let cost_dollars = usage.get("cost").and_then(|v| v.as_f64());

                        // Extract cached tokens from input_tokens_details
                        let cached_tokens = usage
                            .get("input_tokens_details")
                            .and_then(|d| d.get("cached_tokens"))
                            .and_then(|v| v.as_i64());

                        // Extract reasoning tokens from output_tokens_details
                        let reasoning_tokens = usage
                            .get("output_tokens_details")
                            .and_then(|d| d.get("reasoning_tokens"))
                            .and_then(|v| v.as_i64());

                        // Extract finish_reason from choices[0].finish_reason or response.status
                        let finish_reason = json
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("finish_reason"))
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .or_else(|| {
                                // Try response.status for Responses API format
                                json.get("response")
                                    .and_then(|r| r.get("status"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| match s {
                                        "completed" => "stop".to_string(),
                                        other => other.to_string(),
                                    })
                            });

                        return Some(SseChunk::Usage {
                            prompt_tokens,
                            completion_tokens,
                            cost_dollars,
                            cached_tokens,
                            reasoning_tokens,
                            finish_reason,
                        });
                    }

                    // Count tokens in delta content
                    // This is approximate - real token counting requires tokenizer
                    if let Some(content) = json
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|choice| choice.get("delta"))
                        .and_then(|delta| delta.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        // Rough approximation: 1 token ≈ 4 characters.
                        // Use chars() instead of len() so multibyte content
                        // (CJK, emoji) isn't over-counted as a token-per-byte.
                        let estimated_tokens = (content.chars().count() as i64 + 3) / 4;
                        return Some(SseChunk::Delta {
                            tokens: estimated_tokens,
                        });
                    }
                }
            }
        }

        None
    }
}

#[derive(Debug, Clone)]
pub enum SseChunk {
    /// Delta content with estimated token count
    Delta { tokens: i64 },
    /// Final usage data from provider
    Usage {
        prompt_tokens: i64,
        completion_tokens: i64,
        /// Provider-reported cost in dollars (e.g., from OpenRouter)
        cost_dollars: Option<f64>,
        /// Cached tokens (if reported)
        cached_tokens: Option<i64>,
        /// Reasoning tokens (if reported)
        reasoning_tokens: Option<i64>,
        /// How the generation ended (stop, length, etc.)
        finish_reason: Option<String>,
    },
    /// Stream done marker
    Done,
}

/// Inject a `cost` field (in dollars) into an SSE chunk's usage JSON object.
///
/// Handles both formats:
/// - Chat Completions: `data: {"usage":{"prompt_tokens":...}}`
/// - Responses API: `data: {"type":"response.completed","response":{"usage":{"input_tokens":...}}}`
fn inject_cost_into_sse_chunk(chunk: &[u8], cost_dollars: f64) -> Bytes {
    let chunk_str = match std::str::from_utf8(chunk) {
        Ok(s) => s,
        Err(_) => return Bytes::copy_from_slice(chunk),
    };

    let mut output = String::with_capacity(chunk_str.len() + 32);
    for raw in chunk_str.split_inclusive('\n') {
        let (line, terminator) = match raw.strip_suffix('\n') {
            Some(without) => (without, "\n"),
            None => (raw, ""),
        };
        if let Some(json_str) = line.strip_prefix("data: ") {
            if let Ok(mut json) = serde_json::from_str::<Value>(json_str) {
                // Try root-level usage (Chat Completions format)
                let injected =
                    if let Some(usage) = json.get_mut("usage").and_then(|u| u.as_object_mut()) {
                        usage.insert("cost".to_string(), Value::from(cost_dollars));
                        true
                    } else {
                        false
                    };

                // Try nested response.usage (Responses API format)
                if !injected
                    && let Some(usage) = json
                        .get_mut("response")
                        .and_then(|r| r.get_mut("usage"))
                        .and_then(|u| u.as_object_mut())
                {
                    usage.insert("cost".to_string(), Value::from(cost_dollars));
                }

                output.push_str("data: ");
                output.push_str(
                    &serde_json::to_string(&json).unwrap_or_else(|_| json_str.to_string()),
                );
            } else {
                output.push_str(line);
            }
        } else {
            output.push_str(line);
        }
        output.push_str(terminator);
    }

    Bytes::from(output)
}

/// Accumulator for token counts across a stream.
/// Uses atomic types to allow lock-free updates from the stream poll context.
#[derive(Debug)]
pub struct TokenAccumulator {
    input_tokens: AtomicI64,
    output_tokens: AtomicI64,
    estimated_output: AtomicI64,
    usage_received: AtomicBool,
    /// Stored with NONE_SENTINEL for None
    cached_tokens: AtomicI64,
    /// Stored with NONE_SENTINEL for None
    reasoning_tokens: AtomicI64,
    /// Provider-reported cost stored as nano-dollars (dollars * 1e9).
    /// Uses NONE_SENTINEL for None.
    provider_cost_nanodollars: AtomicI64,
    /// How the generation ended (stop, length, etc.)
    finish_reason: crate::compat::Mutex<Option<String>>,
}

impl Default for TokenAccumulator {
    fn default() -> Self {
        Self {
            input_tokens: AtomicI64::new(0),
            output_tokens: AtomicI64::new(0),
            estimated_output: AtomicI64::new(0),
            usage_received: AtomicBool::new(false),
            cached_tokens: AtomicI64::new(NONE_SENTINEL),
            reasoning_tokens: AtomicI64::new(NONE_SENTINEL),
            provider_cost_nanodollars: AtomicI64::new(NONE_SENTINEL),
            finish_reason: crate::compat::Mutex::new(None),
        }
    }
}

impl TokenAccumulator {
    /// Add to the estimated output token count (from delta chunks)
    pub fn add_estimated_output(&self, count: i64) {
        self.estimated_output.fetch_add(count, Ordering::Relaxed);
    }

    /// Set the official usage data from the provider's final chunk
    pub fn set_usage(
        &self,
        prompt_tokens: i64,
        completion_tokens: i64,
        cost_dollars: Option<f64>,
        cached_tokens: Option<i64>,
        reasoning_tokens: Option<i64>,
        finish_reason: Option<String>,
    ) {
        self.input_tokens.store(prompt_tokens, Ordering::Relaxed);
        self.output_tokens
            .store(completion_tokens, Ordering::Relaxed);

        if let Some(cost) = cost_dollars {
            self.provider_cost_nanodollars
                .store((cost * NANODOLLARS_MULTIPLIER) as i64, Ordering::Relaxed);
        }
        if let Some(cached) = cached_tokens {
            self.cached_tokens.store(cached, Ordering::Relaxed);
        }
        if let Some(reasoning) = reasoning_tokens {
            self.reasoning_tokens.store(reasoning, Ordering::Relaxed);
        }
        if finish_reason.is_some() {
            *self.finish_reason.lock() = finish_reason;
        }

        // Set usage_received last with Release ordering to ensure all other
        // stores are visible when this flag is observed as true
        self.usage_received.store(true, Ordering::Release);
    }

    /// Get the input token count
    pub fn input_tokens(&self) -> i64 {
        self.input_tokens.load(Ordering::Relaxed)
    }

    /// Get the output token count
    pub fn output_tokens(&self) -> i64 {
        self.output_tokens.load(Ordering::Relaxed)
    }

    /// Get the estimated output token count (from delta chunks)
    pub fn estimated_output(&self) -> i64 {
        self.estimated_output.load(Ordering::Relaxed)
    }

    /// Check if official usage data was received from the provider
    pub fn usage_received(&self) -> bool {
        // Use Acquire ordering to synchronize with the Release in set_usage
        self.usage_received.load(Ordering::Acquire)
    }

    /// Get the cached token count if available
    pub fn cached_tokens(&self) -> Option<i64> {
        let value = self.cached_tokens.load(Ordering::Relaxed);
        if value == NONE_SENTINEL {
            None
        } else {
            Some(value)
        }
    }

    /// Get the reasoning token count if available
    pub fn reasoning_tokens(&self) -> Option<i64> {
        let value = self.reasoning_tokens.load(Ordering::Relaxed);
        if value == NONE_SENTINEL {
            None
        } else {
            Some(value)
        }
    }

    /// Get the provider-reported cost in dollars if available
    pub fn provider_cost_dollars(&self) -> Option<f64> {
        let value = self.provider_cost_nanodollars.load(Ordering::Relaxed);
        if value == NONE_SENTINEL {
            None
        } else {
            Some(value as f64 / NANODOLLARS_MULTIPLIER)
        }
    }

    /// Get the finish reason if available
    pub fn finish_reason(&self) -> Option<String> {
        self.finish_reason.lock().clone()
    }
}

/// Wrapper around a streaming response that tracks token usage and streaming metrics.
/// Implements Stream to pass through chunks while counting tokens.
pub struct UsageTrackingStream<S> {
    inner: S,
    accumulated_tokens: Arc<TokenAccumulator>,
    usage_logger: Arc<UsageLogger>,
    stream_ended: bool,
    #[cfg(feature = "server")]
    usage_drain: UsageDrainHandle,
    /// Streaming metrics tracking
    streaming_metrics: Arc<StreamingMetrics>,
}

/// Tracks streaming metrics for observability
#[derive(Debug)]
pub struct StreamingMetrics {
    /// Provider name for metric labels
    provider: String,
    /// Model name for metric labels
    model: String,
    /// When the stream started
    start_time: Instant,
    /// When the first chunk was received (stored as nanos since start)
    first_chunk_nanos: AtomicU64,
    /// Total chunks received
    chunk_count: AtomicU64,
    /// Whether the first chunk has been received
    first_chunk_received: AtomicBool,
    /// Whether metrics have been reported (to detect cancellation on drop)
    reported: AtomicBool,
}

/// Sentinel value indicating first chunk time is not set
const FIRST_CHUNK_NOT_SET: u64 = u64::MAX;

impl StreamingMetrics {
    fn new(provider: String, model: String) -> Self {
        Self {
            provider,
            model,
            start_time: Instant::now(),
            first_chunk_nanos: AtomicU64::new(FIRST_CHUNK_NOT_SET),
            chunk_count: AtomicU64::new(0),
            first_chunk_received: AtomicBool::new(false),
            reported: AtomicBool::new(false),
        }
    }

    /// Record a chunk arrival
    fn record_chunk(&self) {
        // Increment chunk count
        self.chunk_count.fetch_add(1, Ordering::Relaxed);

        // Record first chunk time if not already set
        if !self.first_chunk_received.swap(true, Ordering::AcqRel) {
            let elapsed_nanos = self.start_time.elapsed().as_nanos() as u64;
            self.first_chunk_nanos
                .store(elapsed_nanos, Ordering::Relaxed);
        }
    }

    /// Get time to first chunk in seconds, if first chunk was received
    fn time_to_first_chunk_secs(&self) -> Option<f64> {
        let nanos = self.first_chunk_nanos.load(Ordering::Relaxed);
        if nanos == FIRST_CHUNK_NOT_SET {
            None
        } else {
            Some(nanos as f64 / 1_000_000_000.0)
        }
    }

    /// Get total duration since stream start
    fn total_duration_secs(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Get total chunk count
    fn chunk_count(&self) -> u64 {
        self.chunk_count.load(Ordering::Relaxed)
    }

    /// Report final streaming metrics
    fn report(&self, outcome: &str) {
        // Only report once
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }

        metrics::record_streaming_response(
            &self.provider,
            &self.model,
            self.chunk_count(),
            self.time_to_first_chunk_secs(),
            self.total_duration_secs(),
            outcome,
        );
    }
}

impl Drop for StreamingMetrics {
    fn drop(&mut self) {
        // If metrics weren't reported before drop, the stream was cancelled
        // (e.g., client disconnected, request timeout)
        if !*self.reported.get_mut() {
            metrics::record_streaming_response(
                &self.provider,
                &self.model,
                self.chunk_count(),
                self.time_to_first_chunk_secs(),
                self.total_duration_secs(),
                "cancelled",
            );
        }
    }
}

/// Separate struct to handle logging after stream completes
/// This avoids lifetime issues with the stream itself
pub struct UsageLogger {
    db: Arc<DbPool>,
    pricing: Arc<PricingConfig>,
    usage_entry: UsageLogEntry,
    provider: String,
    model: String,
    #[cfg(feature = "server")]
    task_tracker: TaskTracker,
}

impl UsageLogger {
    pub fn new(
        db: Arc<DbPool>,
        pricing: Arc<PricingConfig>,
        usage_entry: UsageLogEntry,
        provider: String,
        model: String,
        #[cfg(feature = "server")] task_tracker: TaskTracker,
    ) -> Self {
        Self {
            db,
            pricing,
            usage_entry,
            provider,
            model,
            #[cfg(feature = "server")]
            task_tracker,
        }
    }

    /// Calculate cost in dollars for the given token counts.
    /// Returns `None` if pricing data is unavailable.
    pub fn calculate_cost_dollars(&self, usage: &SseChunk) -> Option<f64> {
        if let SseChunk::Usage {
            prompt_tokens,
            completion_tokens,
            cost_dollars: provider_cost,
            cached_tokens,
            reasoning_tokens,
            ..
        } = usage
        {
            let calculated = self.pricing.calculate_cost_detailed(
                &self.provider,
                &self.model,
                &crate::pricing::TokenUsage {
                    input_tokens: *prompt_tokens,
                    output_tokens: *completion_tokens,
                    cached_tokens: *cached_tokens,
                    reasoning_tokens: *reasoning_tokens,
                    image_count: None,
                    image_size: None,
                    image_quality: None,
                    audio_seconds: None,
                    character_count: None,
                    video_seconds: None,
                },
            );
            let (cost_microcents, _pricing_source) =
                self.pricing.resolve_cost(*provider_cost, calculated);
            cost_microcents.map(crate::pricing::microcents_to_dollars)
        } else {
            None
        }
    }

    /// Log usage to database based on accumulated tokens
    pub async fn log_usage(&self, tokens: &TokenAccumulator) {
        // Use official usage if received, otherwise use estimates
        let (input_tokens, output_tokens) = if tokens.usage_received() {
            (tokens.input_tokens(), tokens.output_tokens())
        } else {
            // Fall back to estimates
            tracing::warn!(
                "Streaming usage logged without official token counts - using estimates"
            );
            (0, tokens.estimated_output())
        };

        // Calculate cost based on configured pricing
        let calculated_cost = self.pricing.calculate_cost_detailed(
            &self.provider,
            &self.model,
            &crate::pricing::TokenUsage {
                input_tokens,
                output_tokens,
                cached_tokens: tokens.cached_tokens(),
                reasoning_tokens: tokens.reasoning_tokens(),
                image_count: None,
                image_size: None,
                image_quality: None,
                audio_seconds: None,
                character_count: None,
                video_seconds: None,
            },
        );

        // Resolve cost based on cost_source preference (provider-reported vs calculated)
        let (cost_microcents, pricing_source) = self
            .pricing
            .resolve_cost(tokens.provider_cost_dollars(), calculated_cost);

        if let Some(cost) = tokens.provider_cost_dollars() {
            tracing::debug!(
                "Using provider-reported cost: ${:.6} -> {} microcents",
                cost,
                cost_microcents.unwrap_or(0)
            );
        }

        // Update usage entry
        // Use saturating casts to prevent overflow with extremely large token counts
        let mut entry = self.usage_entry.clone();
        entry.input_tokens = saturate_i64_to_i32(input_tokens);
        entry.output_tokens = saturate_i64_to_i32(output_tokens);
        entry.cost_microcents = cost_microcents;
        entry.pricing_source = pricing_source;
        entry.cached_tokens = saturate_i64_to_i32(tokens.cached_tokens().unwrap_or(0));
        entry.reasoning_tokens = saturate_i64_to_i32(tokens.reasoning_tokens().unwrap_or(0));
        entry.finish_reason = tokens.finish_reason();

        // Log to database with retry logic, using task_tracker to ensure completion on shutdown
        let db = self.db.clone();
        #[cfg(feature = "server")]
        self.task_tracker.spawn(async move {
            for attempt in 0..3 {
                match db.usage().log(entry.clone()).await {
                    Ok(_) => {
                        tracing::debug!(
                            "Logged streaming usage: input={}, output={}, cost_microcents={:?}",
                            entry.input_tokens,
                            entry.output_tokens,
                            entry.cost_microcents
                        );
                        break;
                    }
                    Err(e) if attempt == 2 => {
                        tracing::error!(
                            "Failed to log streaming usage after 3 attempts: {}. Entry: {:?}",
                            e,
                            entry
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to log streaming usage (attempt {}): {}. Retrying...",
                            attempt + 1,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(
                            100 * 2_u64.pow(attempt),
                        ))
                        .await;
                    }
                }
            }
        });
    }
}

impl<S> UsageTrackingStream<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        stream: S,
        db: Arc<DbPool>,
        pricing: Arc<PricingConfig>,
        usage_entry: UsageLogEntry,
        provider: String,
        model: String,
        #[cfg(feature = "server")] task_tracker: TaskTracker,
        #[cfg(feature = "server")] usage_drain: UsageDrainHandle,
    ) -> Self {
        let logger = Arc::new(UsageLogger::new(
            db,
            pricing,
            usage_entry,
            provider.clone(),
            model.clone(),
            #[cfg(feature = "server")]
            task_tracker.clone(),
        ));

        Self {
            inner: stream,
            accumulated_tokens: Arc::new(TokenAccumulator::default()),
            usage_logger: logger,
            stream_ended: false,
            #[cfg(feature = "server")]
            usage_drain,
            streaming_metrics: Arc::new(StreamingMetrics::new(provider, model)),
        }
    }
}

impl<S> Stream for UsageTrackingStream<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let inner = Pin::new(&mut self.inner);
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                // Record chunk arrival for streaming metrics
                self.streaming_metrics.record_chunk();

                // Parse chunk for token data and update atomically.
                // Using atomic operations ensures updates are never skipped.
                let mut chunk = chunk;
                if let Some(sse_chunk) = SseParser::parse_chunk(&chunk) {
                    match sse_chunk {
                        SseChunk::Delta { tokens: count } => {
                            self.accumulated_tokens.add_estimated_output(count);
                        }
                        ref usage @ SseChunk::Usage {
                            prompt_tokens,
                            completion_tokens,
                            cost_dollars,
                            cached_tokens,
                            reasoning_tokens,
                            ref finish_reason,
                        } => {
                            self.accumulated_tokens.set_usage(
                                prompt_tokens,
                                completion_tokens,
                                cost_dollars,
                                cached_tokens,
                                reasoning_tokens,
                                finish_reason.clone(),
                            );

                            // Inject calculated cost into the SSE chunk
                            if let Some(cost) = self.usage_logger.calculate_cost_dollars(usage) {
                                chunk = inject_cost_into_sse_chunk(&chunk, cost);
                            }
                        }
                        SseChunk::Done => {
                            // Stream complete marker
                        }
                    }
                }

                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(None) => {
                // Stream ended normally - log usage and report metrics
                if !self.stream_ended {
                    self.stream_ended = true;
                    self.streaming_metrics.report("completed");
                    #[cfg(feature = "server")]
                    self.usage_drain
                        .try_log(self.usage_logger.clone(), self.accumulated_tokens.clone());
                }

                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => {
                // Error in stream - still try to log what we have
                if !self.stream_ended {
                    self.stream_ended = true;
                    self.streaming_metrics.report("error");
                    #[cfg(feature = "server")]
                    {
                        tracing::warn!("Stream ended with error, logging partial usage");
                        self.usage_drain
                            .try_log(self.usage_logger.clone(), self.accumulated_tokens.clone());
                    }
                }

                Poll::Ready(Some(Err(e)))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Drop for UsageTrackingStream<S> {
    fn drop(&mut self) {
        // If stream is dropped without completing, log whatever usage we have.
        // This ensures partial usage is tracked even on:
        // - Client disconnect
        // - Idle timeout
        // - Request cancellation
        //
        // This is important for budget enforcement - without this, an attacker
        // could consume tokens without them being recorded by dropping connections.
        //
        // Drop runs synchronously and is not guaranteed to be inside a Tokio
        // runtime context, so we hand the job to the bounded usage-drain
        // channel instead of spawning a task here directly.
        if !self.stream_ended {
            self.stream_ended = true;
            self.streaming_metrics.report("dropped");
            #[cfg(feature = "server")]
            {
                tracing::warn!(
                    "Stream dropped without completing - logging partial usage for budget accuracy"
                );
                self.usage_drain
                    .try_log(self.usage_logger.clone(), self.accumulated_tokens.clone());
            }
        }
    }
}

/// Saturate an i64 value to fit in an i32.
///
/// Returns `i32::MAX` if the value exceeds the i32 range,
/// `i32::MIN` if below, or the value as i32 otherwise.
#[inline]
fn saturate_i64_to_i32(value: i64) -> i32 {
    if value > i32::MAX as i64 {
        i32::MAX
    } else if value < i32::MIN as i64 {
        i32::MIN
    } else {
        value as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_delta() {
        let chunk = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello world\"}}]}\n\n";
        let result = SseParser::parse_chunk(chunk);

        match result {
            Some(SseChunk::Delta { tokens }) => {
                // "Hello world" = 11 chars ≈ 3 tokens (11/4 rounded up)
                assert!((2..=4).contains(&tokens));
            }
            _ => panic!("Expected Delta chunk"),
        }
    }

    #[test]
    fn test_parse_sse_usage() {
        let chunk = b"data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50}}\n\n";
        let result = SseParser::parse_chunk(chunk);

        match result {
            Some(SseChunk::Usage {
                prompt_tokens,
                completion_tokens,
                cost_dollars,
                cached_tokens,
                reasoning_tokens,
                finish_reason,
            }) => {
                assert_eq!(prompt_tokens, 100);
                assert_eq!(completion_tokens, 50);
                assert!(cost_dollars.is_none());
                assert!(cached_tokens.is_none());
                assert!(reasoning_tokens.is_none());
                assert!(finish_reason.is_none());
            }
            _ => panic!("Expected Usage chunk"),
        }
    }

    #[test]
    fn test_parse_sse_usage_with_cost() {
        // OpenRouter format with cost and token details
        let chunk = br#"data: {"usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19,"cost":0.0000014,"input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}}"#;
        let result = SseParser::parse_chunk(chunk);

        match result {
            Some(SseChunk::Usage {
                prompt_tokens,
                completion_tokens,
                cost_dollars,
                cached_tokens,
                reasoning_tokens,
                finish_reason,
            }) => {
                assert_eq!(prompt_tokens, 12);
                assert_eq!(completion_tokens, 7);
                assert!((cost_dollars.unwrap() - 0.0000014).abs() < 1e-10);
                assert_eq!(cached_tokens, Some(0));
                assert_eq!(reasoning_tokens, Some(0));
                assert!(finish_reason.is_none());
            }
            _ => panic!("Expected Usage chunk"),
        }
    }

    #[test]
    fn test_parse_sse_usage_openrouter_streaming() {
        // OpenRouter streaming response.completed format
        let chunk = br#"data: {"type":"response.completed","response":{"usage":{"input_tokens":187,"output_tokens":57,"total_tokens":244,"cost":0.0000236,"input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":47}},"status":"completed"}}"#;
        let result = SseParser::parse_chunk(chunk);

        match result {
            Some(SseChunk::Usage {
                prompt_tokens,
                completion_tokens,
                cost_dollars,
                cached_tokens,
                reasoning_tokens,
                finish_reason,
            }) => {
                assert_eq!(prompt_tokens, 187);
                assert_eq!(completion_tokens, 57);
                assert!((cost_dollars.unwrap() - 0.0000236).abs() < 1e-10);
                assert_eq!(cached_tokens, Some(0));
                assert_eq!(reasoning_tokens, Some(47));
                // "completed" is mapped to "stop"
                assert_eq!(finish_reason, Some("stop".to_string()));
            }
            _ => panic!("Expected Usage chunk"),
        }
    }

    #[test]
    fn test_parse_sse_usage_with_finish_reason() {
        // OpenAI format with finish_reason in choices
        let chunk = br#"data: {"choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":50}}"#;
        let result = SseParser::parse_chunk(chunk);

        match result {
            Some(SseChunk::Usage {
                prompt_tokens,
                completion_tokens,
                finish_reason,
                ..
            }) => {
                assert_eq!(prompt_tokens, 100);
                assert_eq!(completion_tokens, 50);
                assert_eq!(finish_reason, Some("stop".to_string()));
            }
            _ => panic!("Expected Usage chunk"),
        }
    }

    #[test]
    fn test_parse_sse_delta_multibyte_content() {
        // Four CJK chars = 12 bytes. len()/4 would estimate 3 tokens;
        // chars().count()/4 estimates 1.
        let chunk = r#"data: {"choices":[{"delta":{"content":"日本語😀"}}]}"#;
        let result = SseParser::parse_chunk(chunk.as_bytes());
        match result {
            Some(SseChunk::Delta { tokens }) => {
                assert_eq!(
                    tokens, 1,
                    "4 chars should estimate to 1 token, got {tokens}"
                );
            }
            _ => panic!("Expected Delta chunk"),
        }
    }

    #[test]
    fn test_inject_cost_preserves_double_newline_terminator() {
        let chunk = b"data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n";
        let injected = inject_cost_into_sse_chunk(chunk, 0.0042);
        let s = std::str::from_utf8(&injected).unwrap();
        assert!(s.ends_with("\n\n"), "must preserve SSE event terminator");
        assert!(!s.ends_with("\n\n\n"), "must not add extra newline");
        assert!(s.contains("\"cost\":0.0042"));
    }

    #[test]
    fn test_inject_cost_no_trailing_newline() {
        let chunk = b"data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}";
        let injected = inject_cost_into_sse_chunk(chunk, 0.0042);
        let s = std::str::from_utf8(&injected).unwrap();
        assert!(!s.ends_with('\n'), "must preserve absent terminator");
        assert!(s.contains("\"cost\":0.0042"));
    }

    #[test]
    fn test_parse_sse_done() {
        let chunk = b"data: [DONE]\n\n";
        let result = SseParser::parse_chunk(chunk);

        assert!(matches!(result, Some(SseChunk::Done)));
    }

    #[test]
    fn test_parse_invalid_sse() {
        let chunk = b"invalid data";
        let result = SseParser::parse_chunk(chunk);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_empty_delta() {
        let chunk = b"data: {\"choices\":[{\"delta\":{}}]}\n\n";
        let result = SseParser::parse_chunk(chunk);
        // Should be None since no content in delta
        assert!(result.is_none());
    }

    #[test]
    fn test_token_accumulator() {
        let acc = TokenAccumulator::default();
        assert_eq!(acc.input_tokens(), 0);
        assert_eq!(acc.output_tokens(), 0);
        assert_eq!(acc.estimated_output(), 0);
        assert!(!acc.usage_received());
        assert!(acc.cached_tokens().is_none());
        assert!(acc.reasoning_tokens().is_none());
        assert!(acc.provider_cost_dollars().is_none());
        assert!(acc.finish_reason().is_none());

        // Simulate delta chunks (atomic add)
        acc.add_estimated_output(10);
        acc.add_estimated_output(5);
        assert_eq!(acc.estimated_output(), 15);

        // Simulate official usage
        acc.set_usage(
            100,
            50,
            Some(0.0001),
            Some(10),
            Some(5),
            Some("stop".to_string()),
        );
        assert_eq!(acc.input_tokens(), 100);
        assert_eq!(acc.output_tokens(), 50);
        assert!(acc.usage_received());
        assert_eq!(acc.cached_tokens(), Some(10));
        assert_eq!(acc.reasoning_tokens(), Some(5));
        assert_eq!(acc.finish_reason(), Some("stop".to_string()));
        // Check cost conversion (allow small floating point error)
        let cost = acc.provider_cost_dollars().unwrap();
        assert!((cost - 0.0001).abs() < 1e-12);
    }

    #[test]
    fn test_token_accumulator_concurrent_updates() {
        use std::{sync::Arc, thread};

        let acc = Arc::new(TokenAccumulator::default());
        let mut handles = vec![];

        // Spawn multiple threads to add estimated output concurrently
        for _ in 0..10 {
            let acc_clone = acc.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    acc_clone.add_estimated_output(1);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All 1000 increments should be counted
        assert_eq!(acc.estimated_output(), 1000);
    }

    // ============================================================================
    // Numeric Saturation Tests
    // ============================================================================

    #[test]
    fn test_saturate_i64_to_i32() {
        // Normal values pass through
        assert_eq!(saturate_i64_to_i32(0), 0);
        assert_eq!(saturate_i64_to_i32(1_000_000), 1_000_000);
        assert_eq!(saturate_i64_to_i32(-1_000_000), -1_000_000);

        // i32 boundaries pass through
        assert_eq!(saturate_i64_to_i32(i32::MAX as i64), i32::MAX);
        assert_eq!(saturate_i64_to_i32(i32::MIN as i64), i32::MIN);

        // Values beyond i32::MAX saturate
        assert_eq!(saturate_i64_to_i32(i32::MAX as i64 + 1), i32::MAX);
        assert_eq!(saturate_i64_to_i32(i64::MAX), i32::MAX);

        // Values below i32::MIN saturate
        assert_eq!(saturate_i64_to_i32(i32::MIN as i64 - 1), i32::MIN);
        assert_eq!(saturate_i64_to_i32(i64::MIN), i32::MIN);

        // Test token-realistic values that exceed i32::MAX
        // 3 billion tokens would overflow i32 (max is ~2.1 billion)
        assert_eq!(saturate_i64_to_i32(3_000_000_000), i32::MAX);
    }

    // ============================================================================
    // StreamingMetrics Tests
    // ============================================================================

    #[test]
    fn test_streaming_metrics_new() {
        let metrics = StreamingMetrics::new("anthropic".to_string(), "claude-3".to_string());

        assert_eq!(metrics.provider, "anthropic");
        assert_eq!(metrics.model, "claude-3");
        assert_eq!(metrics.chunk_count(), 0);
        assert!(metrics.time_to_first_chunk_secs().is_none());
        assert!(!metrics.reported.load(Ordering::Relaxed));
    }

    #[test]
    fn test_streaming_metrics_record_chunk() {
        let metrics = StreamingMetrics::new("openai".to_string(), "gpt-4".to_string());

        // First chunk should record TTFC
        metrics.record_chunk();
        assert_eq!(metrics.chunk_count(), 1);
        assert!(metrics.time_to_first_chunk_secs().is_some());

        let ttfc = metrics.time_to_first_chunk_secs().unwrap();
        assert!(ttfc >= 0.0, "TTFC should be non-negative");

        // Subsequent chunks should not change TTFC
        std::thread::sleep(std::time::Duration::from_millis(1));
        metrics.record_chunk();
        assert_eq!(metrics.chunk_count(), 2);

        let ttfc_after = metrics.time_to_first_chunk_secs().unwrap();
        assert!(
            (ttfc - ttfc_after).abs() < 1e-9,
            "TTFC should not change after first chunk"
        );

        // More chunks
        metrics.record_chunk();
        metrics.record_chunk();
        assert_eq!(metrics.chunk_count(), 4);
    }

    #[test]
    fn test_streaming_metrics_total_duration() {
        let metrics = StreamingMetrics::new("bedrock".to_string(), "titan".to_string());

        let duration_before = metrics.total_duration_secs();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let duration_after = metrics.total_duration_secs();

        assert!(
            duration_after > duration_before,
            "Duration should increase over time"
        );
        assert!(
            duration_after >= 0.01,
            "Duration should be at least 10ms after sleep"
        );
    }

    #[test]
    fn test_streaming_metrics_report_only_once() {
        let metrics = StreamingMetrics::new("vertex".to_string(), "gemini".to_string());

        // First report should succeed
        assert!(!metrics.reported.load(Ordering::Relaxed));
        metrics.report("completed");
        assert!(metrics.reported.load(Ordering::Relaxed));

        // Subsequent reports should be no-ops
        metrics.report("error");
        metrics.report("cancelled");
        // If it tried to report again, it would have panicked or caused issues
        // The fact that we get here means it correctly skipped subsequent reports
    }

    #[test]
    fn test_streaming_metrics_concurrent_chunk_recording() {
        use std::{sync::Arc, thread};

        let metrics = Arc::new(StreamingMetrics::new(
            "openai".to_string(),
            "gpt-4".to_string(),
        ));
        let mut handles = vec![];

        // Spawn multiple threads to record chunks concurrently
        for _ in 0..10 {
            let metrics_clone = metrics.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    metrics_clone.record_chunk();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All 1000 chunks should be counted
        assert_eq!(metrics.chunk_count(), 1000);
        // TTFC should have been recorded (by whichever thread won)
        assert!(metrics.time_to_first_chunk_secs().is_some());
    }

    #[test]
    fn test_streaming_metrics_drop_reports_cancelled() {
        // When StreamingMetrics is dropped without report() being called,
        // it should report as "cancelled" in the Drop impl.
        // We can't easily test the actual metrics call, but we can verify
        // the logic works by checking the reported flag behavior.

        let metrics = StreamingMetrics::new("anthropic".to_string(), "claude-3".to_string());
        metrics.record_chunk();
        metrics.record_chunk();

        // Don't call report() - let it drop
        // The Drop impl will call report("cancelled")
        // We verify this works by ensuring no panic occurs
        drop(metrics);

        // If we get here without panic, the Drop impl worked correctly
    }

    #[test]
    fn test_streaming_metrics_drop_after_report_no_double_report() {
        // When report() is called before drop, Drop should not report again
        let metrics = StreamingMetrics::new("openai".to_string(), "gpt-4".to_string());
        metrics.record_chunk();

        metrics.report("completed");
        assert!(metrics.reported.load(Ordering::Relaxed));

        // Drop should see reported=true and skip
        drop(metrics);
        // If we get here without issues, the guard worked
    }

    // ============================================================================
    // IdleTimeoutStream Tests
    // ============================================================================

    use futures_util::stream;

    #[tokio::test]
    async fn test_idle_timeout_stream_passes_through_data() {
        use futures_util::StreamExt;

        // Create a stream that yields items immediately
        let items = vec![
            Ok::<_, io::Error>(Bytes::from("chunk1")),
            Ok(Bytes::from("chunk2")),
        ];
        let inner = stream::iter(items);

        // Wrap with a 1-second timeout (should never trigger since items are immediate)
        let mut timeout_stream = IdleTimeoutStream::new(inner, Duration::from_secs(1));

        // Collect results
        let chunk1 = timeout_stream.next().await;
        assert!(matches!(chunk1, Some(Ok(ref b)) if b == &Bytes::from("chunk1")));

        let chunk2 = timeout_stream.next().await;
        assert!(matches!(chunk2, Some(Ok(ref b)) if b == &Bytes::from("chunk2")));

        let end = timeout_stream.next().await;
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn test_idle_timeout_stream_zero_timeout_disabled() {
        use futures_util::StreamExt;

        // Create a pending stream (would hang forever without timeout)
        let inner = stream::pending::<Result<Bytes, io::Error>>();

        // Zero timeout should disable the timeout mechanism
        let mut timeout_stream = IdleTimeoutStream::new(inner, Duration::ZERO);

        // Use tokio::time::timeout to ensure we don't actually block forever
        let result = tokio::time::timeout(Duration::from_millis(50), timeout_stream.next()).await;

        // Should timeout waiting (Err) since the stream is pending and timeout is disabled
        assert!(result.is_err(), "Expected tokio timeout, got {:?}", result);
    }

    #[tokio::test]
    async fn test_idle_timeout_stream_times_out_on_stalled_stream() {
        use futures_util::StreamExt;

        // Create a pending stream that never yields
        let inner = stream::pending::<Result<Bytes, io::Error>>();

        // Use a short timeout
        let mut timeout_stream = IdleTimeoutStream::new(inner, Duration::from_millis(50));

        // Should timeout and return an error
        let result = timeout_stream.next().await;

        match result {
            Some(Err(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::TimedOut);
                assert!(e.to_string().contains("idle timeout"));
            }
            other => panic!("Expected timeout error, got {:?}", other),
        }

        // After timeout, stream should be terminated
        let end = timeout_stream.next().await;
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn test_idle_timeout_stream_resets_on_each_chunk() {
        use futures_util::StreamExt;

        // Create a stream with delays between chunks
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, io::Error>>(10);
        let inner = tokio_stream::wrappers::ReceiverStream::new(rx);

        // Timeout is 100ms
        let mut timeout_stream = IdleTimeoutStream::new(inner, Duration::from_millis(100));

        // Spawn a task that sends chunks with 50ms delays (less than timeout)
        let sender = tokio::spawn(async move {
            tx.send(Ok(Bytes::from("chunk1"))).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(Ok(Bytes::from("chunk2"))).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(Ok(Bytes::from("chunk3"))).await.unwrap();
            // Don't send more - stream will close
        });

        // All chunks should be received without timeout
        let chunk1 = timeout_stream.next().await;
        assert!(matches!(chunk1, Some(Ok(ref b)) if b == &Bytes::from("chunk1")));

        let chunk2 = timeout_stream.next().await;
        assert!(matches!(chunk2, Some(Ok(ref b)) if b == &Bytes::from("chunk2")));

        let chunk3 = timeout_stream.next().await;
        assert!(matches!(chunk3, Some(Ok(ref b)) if b == &Bytes::from("chunk3")));

        // Stream should end normally
        let end = timeout_stream.next().await;
        assert!(end.is_none());

        sender.await.unwrap();
    }

    #[tokio::test]
    async fn test_idle_timeout_stream_propagates_errors() {
        use futures_util::StreamExt;

        // Create a stream that yields an error using iter (which is Unpin)
        let items: Vec<Result<Bytes, io::Error>> = vec![Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "connection lost",
        ))];
        let inner = stream::iter(items);

        let mut timeout_stream = IdleTimeoutStream::new(inner, Duration::from_secs(1));

        let result = timeout_stream.next().await;
        match result {
            Some(Err(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
            }
            other => panic!("Expected broken pipe error, got {:?}", other),
        }

        // Stream should be terminated after error
        let end = timeout_stream.next().await;
        assert!(end.is_none());
    }
}
