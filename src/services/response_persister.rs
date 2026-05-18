//! Persistence wrapper for `/v1/responses` streams.
//!
//! Sits at the very end of the streaming pipeline (after the tool
//! loop runner). Reads the SSE events flowing to the client, looks
//! for the terminal `response.completed` / `response.failed` /
//! `response.incomplete` event, captures the full `response` object,
//! and persists it via [`ResponsesStore::update`]. The body is
//! forwarded byte-for-byte — the persister is non-destructive.
//!
//! Cancellation: a watch receiver tied to the response row is polled
//! in parallel with the body stream. When it flips, the wrapper
//! terminates forwarding immediately and marks the row Cancelled.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use axum::body::Body;
use bytes::Bytes;
use chrono::Utc;
use futures_util::StreamExt;
use http::Response;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    db::repos::{NewResponseEvent, ResponseCompletion, ResponseStatus},
    services::{CancelSignal, ResponseEventBuffer, ResponsesStore},
    streaming::SseBuffer,
};

/// Wrap a streaming Responses-API HTTP response so the final state
/// gets persisted to the `responses` table when the stream terminates.
///
/// Returns the same response shape, with body replaced by a stream
/// that mirrors the original.
///
/// `initial_sequence_number` is the row's `last_sequence_number` at
/// attach time. The persister increments from there so re-attaches
/// (background retries, hypothetical resume) continue the sequence
/// instead of restarting at 0 and colliding on the (response_id,
/// sequence_number) primary key.
pub fn wrap_streaming_with_persistence(
    response: Response<Body>,
    store: Arc<ResponsesStore>,
    response_id: String,
    org_id: Uuid,
    initial_sequence_number: i64,
    mut cancel_rx: CancelSignal,
    event_buffer: Option<Arc<ResponseEventBuffer>>,
) -> Response<Body> {
    let (parts, body) = response.into_parts();
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    crate::compat::spawn_detached(async move {
        let mut body_stream = body.into_data_stream();
        let mut sse_buffer = SseBuffer::new();
        let mut final_response_object: Option<Value> = None;
        let mut terminal_status: Option<ResponseStatus> = None;
        let mut terminal_event_persisted = false;
        let mut sequence_number: i64 = initial_sequence_number;
        let mut cancelled = false;
        // Lazily filled when the first terminal event is detected so
        // we can inject `container_id` into the response payload (the
        // upstream provider doesn't know about Hadrian containers).
        // Fetched once per response; `None` after the lookup means no
        // shell-tool session was attached.
        let mut container_id_for_event: Option<Option<String>> = None;

        loop {
            tokio::select! {
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        cancelled = true;
                        warn!(
                            stage = "persist_cancelled",
                            response_id = %response_id,
                            "Cancel signal tripped; finalising stream"
                        );
                        break;
                    }
                }
                chunk = body_stream.next() => {
                    let Some(chunk_result) = chunk else { break };
                    let chunk = match chunk_result {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx.send(Err(std::io::Error::other(e))).await;
                            return;
                        }
                    };
                    sse_buffer.extend(&chunk);
                    for event in sse_buffer.extract_complete_events() {
                        let is_terminal = inspect_terminal_event(&event);

                        // On the first terminal event, look up the
                        // response record once and cache its
                        // container_id so we can stamp it into both the
                        // forwarded SSE event and the persisted event
                        // payload. Upstream providers don't know about
                        // Hadrian-hosted containers, so without this
                        // injection clients only see container_id on
                        // the GET retrieve path.
                        if is_terminal.is_some() && container_id_for_event.is_none() {
                            let lookup = store
                                .get(&response_id, org_id)
                                .await
                                .ok()
                                .and_then(|r| r.container_id);
                            container_id_for_event = Some(lookup);
                        }
                        let event = if is_terminal.is_some()
                            && let Some(Some(ref cid)) = container_id_for_event
                        {
                            inject_container_id_into_event(&event, cid).unwrap_or(event)
                        } else {
                            event
                        };
                        if final_response_object.is_none()
                            && let Some((resp_obj, status)) = inspect_terminal_event(&event)
                        {
                            final_response_object = Some(resp_obj);
                            terminal_status = Some(status);
                        }

                        // Append to the event log if a buffer is wired
                        // up. Terminal events go through `insert_sync`
                        // so the row's status update later in this
                        // function happens-after the event commit —
                        // closes the race where ?stream=true readers
                        // see `status=Completed` before the terminal
                        // event reaches the log. Non-terminal events
                        // ride the buffered fast path.
                        if let Some(ref buf) = event_buffer {
                            sequence_number += 1;
                            let (event_type, payload) = parse_event_for_log(&event);
                            let new_event = NewResponseEvent {
                                response_id: response_id.clone(),
                                sequence_number,
                                event_type,
                                payload,
                                created_at: Utc::now(),
                            };
                            if is_terminal.is_some() {
                                if let Err(e) = buf.insert_sync(new_event).await {
                                    warn!(
                                        error = %e,
                                        response_id = %response_id,
                                        "Failed to commit terminal response event; \
                                         readers may see status=terminal without the event"
                                    );
                                } else {
                                    terminal_event_persisted = true;
                                }
                            } else {
                                buf.push(new_event);
                            }
                        }

                        if tx.send(Ok(event)).await.is_err() {
                            // Client gone; we still want to record state
                            // for the GET endpoint, so finish the persist
                            // step below.
                            break;
                        }
                    }
                }
            }
        }

        // Flush any trailing partial bytes. On cancel, append a
        // synthetic `response.cancelled` event so polling clients
        // observe the terminal status in the event log too.
        if !sse_buffer.is_empty() {
            let _ = tx.send(Ok(sse_buffer.take_remaining())).await;
        }

        // Persist the captured state. If we never saw a terminal
        // event, mark as incomplete — the stream ended without a
        // `response.completed`. Cancel takes precedence regardless of
        // what events we saw.
        let (output, usage, error_field, status) = if cancelled {
            (None, None, None, ResponseStatus::Cancelled)
        } else {
            match final_response_object {
                Some(resp) => {
                    let status = terminal_status.unwrap_or(ResponseStatus::Completed);
                    (
                        resp.get("output").cloned(),
                        resp.get("usage").cloned(),
                        resp.get("error").cloned().filter(|v| !v.is_null()),
                        status,
                    )
                }
                None => {
                    debug!(
                        stage = "persist_no_terminal_event",
                        response_id = %response_id,
                        "Stream ended without a terminal response event; marking incomplete"
                    );
                    (None, None, None, ResponseStatus::Incomplete)
                }
            }
        };

        // If the row is closing out terminally but no terminal event
        // landed in the log yet (cancelled mid-stream, or the upstream
        // closed without one), synthesise one and commit it
        // synchronously. ?stream=true readers can then detect the
        // terminal event from the log alone, matching the
        // "event-log-as-truth" contract.
        if let Some(ref buf) = event_buffer
            && status.is_terminal()
            && !terminal_event_persisted
        {
            sequence_number += 1;
            let synth_type = match status {
                ResponseStatus::Completed => "response.completed",
                ResponseStatus::Failed => "response.failed",
                ResponseStatus::Cancelled => "response.cancelled",
                ResponseStatus::Incomplete => "response.incomplete",
                _ => "response.completed",
            };
            // Resolve container_id once for the synthetic event too.
            // Re-read if we never hit a terminal event during the
            // stream (so `container_id_for_event` is still `None`).
            let container_id_synth: Option<String> = match container_id_for_event.take() {
                Some(cid) => cid,
                None => store
                    .get(&response_id, org_id)
                    .await
                    .ok()
                    .and_then(|r| r.container_id),
            };
            let mut response_obj = serde_json::json!({
                "id": response_id.clone(),
                "status": status.as_str(),
            });
            if let Some(ref cid) = container_id_synth {
                response_obj["container_id"] = Value::String(cid.clone());
            }
            let payload = serde_json::json!({
                "type": synth_type,
                "response": response_obj,
            });
            let synth_event = NewResponseEvent {
                response_id: response_id.clone(),
                sequence_number,
                event_type: synth_type.to_string(),
                payload,
                created_at: Utc::now(),
            };
            if let Err(e) = buf.insert_sync(synth_event).await {
                warn!(
                    error = %e,
                    response_id = %response_id,
                    "Failed to commit synthetic terminal event"
                );
            }
        }

        if let Err(e) = store
            .update_within_org(
                &response_id,
                org_id,
                ResponseCompletion {
                    status: Some(status),
                    completed_at: Some(Utc::now()),
                    output,
                    usage,
                    error: error_field,
                    ..Default::default()
                },
            )
            .await
        {
            error!(
                error = %e,
                response_id = %response_id,
                "Failed to persist final response state"
            );
        } else {
            info!(
                stage = "persist_complete",
                response_id = %response_id,
                status = ?status,
                "Persisted final response state"
            );
        }
    });

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Response::from_parts(parts, Body::from_stream(stream))
}

/// Rewrite a terminal SSE event's `data:` payload to stamp
/// `response.container_id`. Upstream providers don't emit this for
/// Hadrian-hosted containers, so we inject it on the way out so the
/// streaming surface matches what `GET /v1/responses/{id}` returns.
///
/// Returns `None` when the event has no parseable `data:` JSON line,
/// or when the payload already carries a matching container_id (so
/// passthrough_openai responses round-trip unchanged).
fn inject_container_id_into_event(event: &[u8], container_id: &str) -> Option<Bytes> {
    let s = std::str::from_utf8(event).ok()?;
    let mut out = String::with_capacity(event.len() + container_id.len() + 24);
    let mut mutated = false;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(data) = trimmed.strip_prefix("data:") {
            let data = data.trim_start();
            if !data.is_empty()
                && data != "[DONE]"
                && let Ok(mut json) = serde_json::from_str::<Value>(data)
                && let Some(resp) = json.get_mut("response").and_then(|r| r.as_object_mut())
            {
                let existing = resp
                    .get("container_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if existing.as_deref() != Some(container_id) {
                    resp.insert(
                        "container_id".to_string(),
                        Value::String(container_id.to_string()),
                    );
                    out.push_str("data: ");
                    out.push_str(&serde_json::to_string(&json).ok()?);
                    // Preserve the original frame's line terminator.
                    if line.ends_with("\r\n") {
                        out.push_str("\r\n");
                    } else if line.ends_with('\n') {
                        out.push('\n');
                    }
                    mutated = true;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    if mutated {
        Some(Bytes::from(out))
    } else {
        None
    }
}

/// Recognise an SSE event that terminates the response and carries the
/// full final object. The Responses API emits one of:
///   data: {"type":"response.completed","response":{...}}
///   data: {"type":"response.failed","response":{...}}
///   data: {"type":"response.incomplete","response":{...}}
fn inspect_terminal_event(event: &[u8]) -> Option<(Value, ResponseStatus)> {
    let s = std::str::from_utf8(event).ok()?;
    // Named SSE events arrive as `event: <type>\ndata: <json>\n\n` —
    // we must skip the `event:` (and any future header) lines instead
    // of short-circuiting on the first non-`data:` line. A `?` on
    // `strip_prefix` here would mean a single `event:` header in a
    // multi-line frame silently drops the trailing terminal payload.
    for line in s.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let Some(event_type) = json.get("type").and_then(|t| t.as_str()) else {
            continue;
        };
        let status = match event_type {
            "response.completed" => ResponseStatus::Completed,
            "response.failed" => ResponseStatus::Failed,
            "response.incomplete" => ResponseStatus::Incomplete,
            _ => continue,
        };
        let response = json.get("response").cloned().unwrap_or(Value::Null);
        return Some((response, status));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_terminal_event_with_event_header_prefix() {
        // Named-SSE form: `event: response.completed\ndata: {...}\n\n`.
        // A previous bug short-circuited on the first non-`data:` line
        // (the `event:` header) and missed the terminal data entirely.
        let raw = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_abc\",\"status\":\"completed\"}}\n\n";
        let result = inspect_terminal_event(raw);
        assert!(
            result.is_some(),
            "should detect terminal event past header line"
        );
        let (resp, status) = result.unwrap();
        assert_eq!(status, ResponseStatus::Completed);
        assert_eq!(resp.get("id").and_then(|v| v.as_str()), Some("resp_abc"));
    }

    #[test]
    fn detects_terminal_event_data_only_form() {
        // Plain Responses-API form without an `event:` line.
        let raw = b"data: {\"type\":\"response.failed\",\"response\":{\"error\":\"boom\"}}\n\n";
        let result = inspect_terminal_event(raw);
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, ResponseStatus::Failed);
    }

    #[test]
    fn ignores_non_terminal_events() {
        let raw = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n";
        assert!(inspect_terminal_event(raw).is_none());
    }

    #[test]
    fn ignores_done_sentinel() {
        let raw = b"data: [DONE]\n\n";
        assert!(inspect_terminal_event(raw).is_none());
    }

    #[test]
    fn injects_container_id_into_terminal_event() {
        let raw = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_abc\",\"status\":\"completed\"}}\n\n";
        let out = inject_container_id_into_event(raw, "cntr_xyz").expect("should mutate");
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("\"container_id\":\"cntr_xyz\""), "got: {s}");
        // Header line preserved verbatim.
        assert!(s.starts_with("event: response.completed\n"));
    }

    #[test]
    fn injection_skipped_when_already_present() {
        let raw = b"data: {\"type\":\"response.completed\",\"response\":{\"container_id\":\"cntr_xyz\"}}\n\n";
        assert!(inject_container_id_into_event(raw, "cntr_xyz").is_none());
    }
}

/// Parse one SSE event into (`event_type`, `payload`) for the event
/// log. Best-effort: malformed events get `event_type = "unknown"`
/// and the raw bytes as a JSON string so replay never loses data.
fn parse_event_for_log(event: &[u8]) -> (String, Value) {
    let Ok(s) = std::str::from_utf8(event) else {
        // Non-UTF-8 bytes get base64-encoded so the audit log is
        // human-pasteable (Debug-format of `bytes::Bytes` is a hex
        // array that's hard to recover from).
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(event);
        return (
            "unknown_binary".to_string(),
            serde_json::json!({ "base64": encoded }),
        );
    };
    for line in s.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            return ("done".to_string(), Value::Null);
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let event_type = json
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        return (event_type, json);
    }
    ("unknown".to_string(), Value::String(s.to_string()))
}

/// Persist a non-streaming final response by reading and reparsing the
/// JSON body. Returns the bytes so the caller can also send them to
/// the client / cache. On parse failure the response is recorded as
/// `Failed` and the bytes are still forwarded.
pub async fn persist_non_streaming(
    store: &ResponsesStore,
    response_id: &str,
    org_id: Uuid,
    body_bytes: &[u8],
    http_status: u16,
) {
    let status = if (200..300).contains(&http_status) {
        ResponseStatus::Completed
    } else {
        ResponseStatus::Failed
    };
    let parsed: Result<Value, _> = serde_json::from_slice(body_bytes);
    let (output, usage, error_field) = match parsed {
        Ok(v) => (
            v.get("output").cloned(),
            v.get("usage").cloned(),
            v.get("error").cloned().filter(|v| !v.is_null()),
        ),
        Err(_) => (None, None, None),
    };
    if let Err(e) = store
        .update_within_org(
            response_id,
            org_id,
            ResponseCompletion {
                status: Some(status),
                completed_at: Some(Utc::now()),
                output,
                usage,
                error: error_field,
                ..Default::default()
            },
        )
        .await
    {
        error!(error = %e, response_id, "Failed to persist non-streaming response");
    }
}
