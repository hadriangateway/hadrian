//! Webhook delivery for Responses API terminal-state transitions.
//!
//! When `[features.responses.webhook]` is configured, a `POST` is
//! fired at the URL each time a stored response transitions to a
//! terminal status (`completed`, `failed`, `cancelled`, `incomplete`).
//! The body is a small JSON envelope; the receiver fetches the full
//! object via `GET /v1/responses/{id}` for any further detail.
//!
//! Flow control:
//! - `enqueue` pushes to a bounded mpsc; full → drop with a
//!   `webhook_dropped_total` counter increment.
//! - A single drainer task pulls events and acquires a slot from a
//!   `Semaphore` (capacity `max_concurrent_deliveries`) before
//!   spawning the actual HTTP call. Slow targets back-pressure the
//!   drainer instead of unboundedly spawning tasks.
//! - Each delivery retries 3× with exponential backoff. Permanent
//!   failure pushes a `DlqEntry` (when a DLQ is configured) so
//!   operators can replay later instead of losing the event.

#![cfg(not(target_arch = "wasm32"))]

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::Serialize;
use sha2::Sha256;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, error, info, warn};

use crate::{
    config::ResponsesWebhookConfig,
    db::repos::ResponseStatus,
    dlq::{DeadLetterQueue, DlqEntry},
};

type HmacSha256 = Hmac<Sha256>;

/// Header carrying the HMAC signature when `signing_secret` is set.
/// Format: `t=<unix-seconds>,v1=<hex-sha256>`. The signed payload is
/// `"<unix>.<body>"` so a captured request can't be replayed against a
/// receiver that enforces timestamp freshness.
const SIGNATURE_HEADER: &str = "X-Hadrian-Signature";

/// Entry-type marker used when pushing failed webhook deliveries to
/// the DLQ. Surfaced in `/admin/v1/dlq` filters.
const DLQ_ENTRY_TYPE: &str = "responses_webhook";

/// Payload sent to the configured webhook endpoint.
///
/// Mirrors OpenAI's webhook event shape so existing handlers work
/// with minimal porting: `type` distinguishes which terminal state
/// fired, `data.id` carries the response id for follow-up fetches.
#[derive(Debug, Serialize, Clone)]
pub struct WebhookEvent {
    /// e.g. `"response.completed"`, `"response.failed"`,
    /// `"response.cancelled"`, `"response.incomplete"`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// ISO-8601 timestamp.
    pub created_at: DateTime<Utc>,
    pub data: WebhookEventData,
}

#[derive(Debug, Serialize, Clone)]
pub struct WebhookEventData {
    pub id: String,
    pub status: &'static str,
    pub background: bool,
}

/// Dispatcher held in AppState. Cheap to clone (everything inside is
/// already `Arc`-friendly).
#[derive(Clone)]
pub struct ResponsesWebhookDispatcher {
    tx: mpsc::Sender<WebhookEvent>,
    /// Held on the dispatcher (not just the drainer) so `enqueue` can
    /// divert events to the DLQ when the bounded channel is full,
    /// instead of silently dropping them.
    dlq: Option<Arc<dyn DeadLetterQueue>>,
    /// Webhook target URL — copied here so the overflow path can
    /// label DLQ entries without reading the full config.
    target_url: String,
}

impl ResponsesWebhookDispatcher {
    /// Construct a dispatcher and spawn its drainer. `dlq` is
    /// optional; when present, permanently-failed deliveries land
    /// there for operator replay.
    pub fn spawn(
        config: ResponsesWebhookConfig,
        http: Client,
        dlq: Option<Arc<dyn DeadLetterQueue>>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(config.retry_queue_capacity.max(1));
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_deliveries.max(1)));
        let target_url = config.url.clone();
        let shared = Arc::new(DispatcherInner {
            config,
            http,
            dlq: dlq.clone(),
        });
        crate::compat::spawn_detached(drain_events(rx, semaphore, shared));
        Self {
            tx,
            dlq,
            target_url,
        }
    }

    /// Non-blocking enqueue. When the retry queue is full, the event
    /// is diverted to the DLQ (if configured) so a wedged target
    /// can't cause terminal-state notifications to vanish silently.
    /// Without a DLQ, an overflow logs and drops as a last resort.
    pub fn enqueue(&self, response_id: String, status: ResponseStatus, background: bool) {
        let Some(event_type) = terminal_event_name(status) else {
            // Non-terminal status — nothing to deliver.
            return;
        };
        let event = WebhookEvent {
            event_type: event_type.to_string(),
            created_at: Utc::now(),
            data: WebhookEventData {
                id: response_id,
                status: status.as_str(),
                background,
            },
        };
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(ev)) => {
                self.route_overflow_to_dlq(ev);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Drainer exited; we're shutting down.
            }
        }
    }

    /// Divert an overflow event to the DLQ off the hot path. Uses a
    /// detached task because `enqueue` is sync; the caller (the
    /// `responses_store` update path) doesn't pay for DLQ writes.
    fn route_overflow_to_dlq(&self, ev: WebhookEvent) {
        let Some(ref dlq) = self.dlq else {
            warn!(
                response_id = %ev.data.id,
                event_type = %ev.event_type,
                "Webhook retry queue full and no DLQ configured; dropping event"
            );
            return;
        };
        let dlq = dlq.clone();
        let target_url = self.target_url.clone();
        crate::compat::spawn_detached(async move {
            let payload = match serde_json::to_string(&ev) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        error = %e,
                        response_id = %ev.data.id,
                        event_type = %ev.event_type,
                        "Failed to serialise overflow webhook event for DLQ; event lost"
                    );
                    return;
                }
            };
            let entry = DlqEntry::new(
                DLQ_ENTRY_TYPE,
                payload,
                format!("webhook retry queue full; deferred to DLQ ({target_url})"),
            )
            .with_metadata("response_id", ev.data.id.clone())
            .with_metadata("event_type", ev.event_type.clone())
            .with_metadata("reason", "queue_full".to_string());
            match dlq.push(entry).await {
                Ok(_) => info!(
                    response_id = %ev.data.id,
                    event_type = %ev.event_type,
                    "Webhook retry queue full; overflow event routed to DLQ"
                ),
                Err(e) => error!(
                    response_id = %ev.data.id,
                    event_type = %ev.event_type,
                    error = %e,
                    "Failed to push overflow webhook event to DLQ; event lost"
                ),
            }
        });
    }
}

struct DispatcherInner {
    config: ResponsesWebhookConfig,
    http: Client,
    dlq: Option<Arc<dyn DeadLetterQueue>>,
}

async fn drain_events(
    mut rx: mpsc::Receiver<WebhookEvent>,
    semaphore: Arc<Semaphore>,
    shared: Arc<DispatcherInner>,
) {
    while let Some(event) = rx.recv().await {
        // Block here until a slot is free. Back-pressures the channel
        // so a slow target can't fan out unbounded in-flight requests.
        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            return; // semaphore closed
        };
        let shared = shared.clone();
        crate::compat::spawn_detached(async move {
            let _permit = permit; // released when this task ends
            deliver_or_dlq(&shared, event).await;
        });
    }
}

async fn deliver_or_dlq(shared: &DispatcherInner, event: WebhookEvent) {
    if deliver_with_retry(shared, &event).await {
        return;
    }
    // Permanent failure: route to DLQ if available so the event can
    // be replayed later. Without a DLQ we log and drop — the
    // persisted `responses` row still carries the same data.
    let Some(ref dlq) = shared.dlq else {
        info!(
            response_id = %event.data.id,
            event_type = %event.event_type,
            "Webhook delivery permanently failed; no DLQ configured, dropping"
        );
        return;
    };
    let payload = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to serialise webhook event for DLQ");
            return;
        }
    };
    let entry = DlqEntry::new(
        DLQ_ENTRY_TYPE,
        payload,
        format!(
            "permanent delivery failure to {} after retries",
            shared.config.url
        ),
    )
    .with_metadata("response_id", event.data.id.clone())
    .with_metadata("event_type", event.event_type.clone());
    if let Err(e) = dlq.push(entry).await {
        warn!(
            response_id = %event.data.id,
            event_type = %event.event_type,
            error = %e,
            "Failed to push webhook delivery to DLQ"
        );
    } else {
        info!(
            response_id = %event.data.id,
            event_type = %event.event_type,
            "Webhook delivery permanently failed; routed to DLQ"
        );
    }
}

fn terminal_event_name(status: ResponseStatus) -> Option<&'static str> {
    match status {
        ResponseStatus::Completed => Some("response.completed"),
        ResponseStatus::Failed => Some("response.failed"),
        ResponseStatus::Cancelled => Some("response.cancelled"),
        ResponseStatus::Incomplete => Some("response.incomplete"),
        ResponseStatus::Queued | ResponseStatus::InProgress => None,
    }
}

/// Returns true on success, false after exhausting retries.
async fn deliver_with_retry(shared: &DispatcherInner, event: &WebhookEvent) -> bool {
    let body = match serde_json::to_vec(event) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "Webhook serialization failed; dropping");
            return false;
        }
    };

    const BACKOFFS_MS: [u64; 3] = [250, 1_000, 4_000];
    for (attempt, backoff) in BACKOFFS_MS.iter().enumerate() {
        // Recompute the signature per attempt so the `t=` timestamp
        // stays fresh — a retry an hour later shouldn't fail receiver
        // freshness checks for a stale stamp from the first try.
        let signature = shared
            .config
            .signing_secret
            .as_deref()
            .map(|secret| sign_payload(secret, &body, Utc::now()));
        let mut req = shared
            .http
            .post(&shared.config.url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "hadrian-responses-webhook/1")
            .timeout(Duration::from_secs(shared.config.timeout_secs))
            .body(body.clone());
        if let Some(ref token) = shared.config.bearer_token {
            req = req.bearer_auth(token);
        }
        if let Some(ref sig) = signature {
            req = req.header(SIGNATURE_HEADER, sig);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!(
                    response_id = %event.data.id,
                    event_type = %event.event_type,
                    attempt = attempt + 1,
                    status = resp.status().as_u16(),
                    "Webhook delivered"
                );
                return true;
            }
            Ok(resp) => {
                warn!(
                    response_id = %event.data.id,
                    event_type = %event.event_type,
                    attempt = attempt + 1,
                    status = resp.status().as_u16(),
                    "Webhook responded non-2xx; retrying"
                );
            }
            Err(e) => {
                warn!(
                    response_id = %event.data.id,
                    event_type = %event.event_type,
                    attempt = attempt + 1,
                    error = %e,
                    "Webhook delivery failed; retrying"
                );
            }
        }
        if attempt + 1 < BACKOFFS_MS.len() {
            tokio::time::sleep(Duration::from_millis(*backoff)).await;
        }
    }
    false
}

/// Compute the `X-Hadrian-Signature` header value for a body.
///
/// Signs the payload `"<unix>.<body>"` with HMAC-SHA256 keyed by the
/// configured secret. The leading timestamp prevents a captured
/// request from being replayed against a receiver that enforces a
/// freshness window (the receiver re-signs with its own copy of
/// `body` and the timestamp from the header, then compares).
fn sign_payload(secret: &str, body: &[u8], now: DateTime<Utc>) -> String {
    let ts = now.timestamp();
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC-SHA256 accepts any key length");
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let digest = mac.finalize().into_bytes();
    format!("t={ts},v1={}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_format_and_verifiability() {
        let secret = "shh";
        let body = br#"{"type":"response.completed","data":{"id":"resp_abc"}}"#;
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let header = sign_payload(secret, body, now);

        // Format: t=<unix>,v1=<64-hex>
        let (t_part, v_part) = header.split_once(',').expect("comma-separated header");
        assert_eq!(t_part, "t=1700000000");
        let hex_sig = v_part.strip_prefix("v1=").expect("v1= prefix");
        assert_eq!(hex_sig.len(), 64);

        // Verifiable: recomputing with the same secret + body + ts
        // reproduces the digest byte-for-byte.
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(b"1700000000");
        mac.update(b".");
        mac.update(body);
        assert_eq!(hex::encode(mac.finalize().into_bytes()), hex_sig);
    }

    #[test]
    fn signature_changes_when_body_changes() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let a = sign_payload("shh", b"a", now);
        let b = sign_payload("shh", b"b", now);
        assert_ne!(a, b);
    }

    #[test]
    fn signature_changes_when_timestamp_changes() {
        let body = b"payload";
        let t1 = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let t2 = DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap();
        assert_ne!(sign_payload("shh", body, t1), sign_payload("shh", body, t2));
    }
}
