//! `GET`/`POST cancel`/`DELETE` handlers for stored Responses API
//! records, matching OpenAI's Responses API spec.
//!
//! Persistence happens during `POST /v1/responses` (see chat.rs); these
//! endpoints surface the resulting rows.

#![cfg(feature = "server")]

use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::ApiError;
use crate::{
    AppState,
    auth::AuthenticatedRequest,
    db::repos::ResponseRecord,
    middleware::AuthzContext,
    services::{ResponsesStore, ResponsesStoreError},
};

/// Wire-format response shape. Wraps the stored JSON output and stamps
/// gateway-controlled fields (id, status, created_at). All other
/// fields come from the persisted request_payload / output / usage so
/// the surface matches OpenAI's spec.
#[derive(Serialize)]
pub struct WireResponse {
    id: String,
    object: &'static str,
    status: &'static str,
    background: bool,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    /// **Hadrian Extension:** ownership scope of the response —
    /// `organization`, `team`, `project`, `user`, or `service_account`.
    /// Mirrors the pattern used by skills, templates, and conversations.
    owner: WireOwner,
    /// Unix timestamp in seconds, matching OpenAI's integer encoding.
    created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Value::is_null")]
    output: Value,
    #[serde(skip_serializing_if = "Value::is_null")]
    usage: Value,
    #[serde(skip_serializing_if = "Value::is_null")]
    error: Value,
    /// Container the shell-tool session for this response was attached
    /// to. Surfaced so callers can chain via
    /// `environment.type = "container_reference"` without scraping
    /// output items. `null` when the request had no shell tool, when
    /// container persistence is disabled, or before the shell tool
    /// first runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    container_id: Option<String>,
    /// Echo selected request-payload fields so the response carries the
    /// instructions, tools, etc. that originally drove it — same as
    /// OpenAI's Retrieve endpoint.
    #[serde(flatten)]
    echoed: Map<String, Value>,
}

#[derive(Serialize)]
pub struct WireOwner {
    #[serde(rename = "type")]
    type_: &'static str,
    id: String,
}

fn record_to_wire(record: &ResponseRecord) -> WireResponse {
    // Pull selected request fields back into the top-level shape so
    // clients can introspect what they sent. Anything sensitive
    // (e.g. raw secret values) is omitted because callers only ever
    // stored placeholders.
    const ECHO_FIELDS: &[&str] = &[
        "input",
        "instructions",
        "metadata",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "max_output_tokens",
        "reasoning",
        "text",
        "include",
        "store",
        "previous_response_id",
    ];
    let mut echoed = Map::new();
    if let Value::Object(obj) = &record.request_payload {
        for k in ECHO_FIELDS {
            if let Some(v) = obj.get(*k) {
                echoed.insert((*k).to_string(), v.clone());
            }
        }
    }
    WireResponse {
        id: record.id.clone(),
        object: "response",
        status: record.status.as_str(),
        background: record.background,
        model: record.model.clone(),
        provider: record.provider.clone(),
        owner: WireOwner {
            type_: record.owner_type.as_str(),
            id: record.owner_id.to_string(),
        },
        created_at: record.created_at.timestamp(),
        completed_at: record.completed_at.map(|t| t.timestamp()),
        output: record.output.clone().unwrap_or(Value::Null),
        usage: record.usage.clone().unwrap_or(Value::Null),
        error: record.error.clone().unwrap_or(Value::Null),
        container_id: record.container_id.clone(),
        echoed,
    }
}

fn resolve_store(state: &AppState) -> Result<&ResponsesStore, ApiError> {
    state.responses_store.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "responses_persistence_disabled",
            "Response persistence requires a configured database".to_string(),
        )
    })
}

/// Resolve the caller's org or return 401. Both authenticated and
/// auth-disabled flows reach this through the API middleware, which
/// injects an `AuthenticatedRequest` carrying the synthetic default
/// principal in anonymous-mode deployments. Reject when the middleware
/// produced nothing — we can't safely scope without an org.
fn require_caller_org(auth: Option<&Extension<AuthenticatedRequest>>) -> Result<Uuid, ApiError> {
    auth.and_then(|Extension(a)| {
        a.api_key()
            .and_then(|k| k.org_id)
            .or_else(|| a.principal().org_id())
    })
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "An authenticated org is required",
        )
    })
}

/// Run `authz.require_api("response", action, ...)` when authz is
/// configured. Mirrors the gate `api_v1_responses` applies for
/// `("model", "use")`, so RBAC policies that allow create-but-deny-
/// retrieve (or vice versa) can express that.
async fn enforce_authz(
    authz: Option<&Extension<AuthzContext>>,
    auth: Option<&Extension<AuthenticatedRequest>>,
    action: &str,
) -> Result<(), ApiError> {
    let Some(Extension(authz)) = authz else {
        return Ok(());
    };
    let org_id = auth.and_then(|a| {
        a.api_key()
            .and_then(|k| k.org_id.map(|id| id.to_string()))
            .or_else(|| a.identity().and_then(|i| i.org_ids.first().cloned()))
    });
    let project_id = auth.and_then(|a| {
        a.api_key()
            .and_then(|k| k.project_id.map(|id| id.to_string()))
            .or_else(|| a.identity().and_then(|i| i.project_ids.first().cloned()))
    });
    authz
        .require_api(
            "response",
            action,
            None,
            None,
            org_id.as_deref(),
            project_id.as_deref(),
        )
        .await
        .map_err(|e| ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string()))
}

/// Convert a [`ResponsesStoreError`] into an `ApiError`, logging
/// internal details server-side and returning a safe shape to the
/// caller. Mirrors the canonical `impl From<DbError> for ApiError` in
/// `src/routes/api/mod.rs` — internal-only errors get a generic
/// message; expected-shape errors get specific codes.
fn map_store_err(e: ResponsesStoreError) -> ApiError {
    match e {
        ResponsesStoreError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "response_not_found",
            "No such response",
        ),
        ResponsesStoreError::NotBackground => ApiError::new(
            StatusCode::BAD_REQUEST,
            "response_not_background",
            "Only responses created with background=true can be cancelled",
        ),
        ResponsesStoreError::Database(db_err) => {
            // Delegate to the canonical From<DbError> impl so known
            // variants (NotFound / Conflict / Validation) map to the
            // same codes everywhere, and unknown ones get logged but
            // surface a generic message.
            ApiError::from(db_err)
        }
        ResponsesStoreError::Internal(msg) => {
            tracing::error!(error = %msg, "Responses store internal error");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "An internal error occurred",
            )
        }
    }
}

/// Query parameters for `GET /v1/responses/{id}`. Mirrors OpenAI's
/// retrieve-response spec: `stream` flips to SSE mode, `starting_after`
/// resumes replay from a sequence number, `include` widens the response
/// shape, and `limit` caps batch size in poll-replay loops.
#[derive(Debug, Default, Deserialize)]
pub struct RetrieveQuery {
    /// When `true`, the response is streamed as Server-Sent Events
    /// replaying the persisted event log instead of returning a static
    /// JSON object. Matches OpenAI's `stream` field on the retrieve
    /// endpoint so SDK clients can resume a dropped subscription.
    #[serde(default)]
    pub stream: Option<bool>,
    /// Return events with `sequence_number > starting_after`. Clients
    /// pass the highest sequence number they've already seen so a
    /// reconnect resumes without duplicates. Only meaningful with
    /// `stream=true`.
    #[serde(default)]
    pub starting_after: Option<i64>,
    /// Soft cap on events returned per replay page. Default 200.
    /// Only meaningful with `stream=true`.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Pass-through of OpenAI's `include` widening field. Currently
    /// ignored — the persisted shape already returns echoed fields.
    /// Accepted so SDK calls don't 400 on the extra param.
    #[serde(default)]
    pub include: Option<Vec<String>>,
    /// Pass-through of OpenAI's `include_obfuscation` field. Accepted
    /// for parity but Hadrian doesn't add padding bytes today.
    #[serde(default)]
    pub include_obfuscation: Option<bool>,
}

/// `GET /v1/responses/{response_id}` — retrieve a stored response.
///
/// When `?stream=true` is set, returns the persisted event log as an
/// SSE stream (matching OpenAI's retrieve-with-stream behavior). The
/// `starting_after` query parameter resumes replay from a specific
/// sequence number so a client reconnecting after a disconnect picks
/// up exactly where it stopped.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/responses/{response_id}",
    tag = "responses",
    params(
        ("response_id" = String, Path, description = "ID returned by POST /v1/responses"),
        ("stream" = Option<bool>, Query, description = "Stream the event log as SSE"),
        ("starting_after" = Option<i64>, Query, description = "Resume cursor (stream mode)"),
        ("limit" = Option<i64>, Query, description = "Events per page (stream mode)"),
    ),
    responses(
        (status = 200, description = "Stored response (JSON) or SSE replay when stream=true"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Response not found", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_responses_get(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(response_id): Path<String>,
    Query(query): Query<RetrieveQuery>,
) -> Result<axum::response::Response, ApiError> {
    let store = resolve_store(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "read").await?;
    let org_id = require_caller_org(auth.as_ref())?;

    if query.stream.unwrap_or(false) {
        return stream_response_events(
            state.clone(),
            response_id,
            org_id,
            query.starting_after,
            query.limit,
        )
        .await;
    }

    let record = store
        .get(&response_id, org_id)
        .await
        .map_err(map_store_err)?;
    Ok(Json(record_to_wire(&record)).into_response())
}

/// `POST /v1/responses/{response_id}/cancel` — cancel an in-progress
/// background response. Per OpenAI's spec, returns 400 if the response
/// is not in background mode. Idempotent for already-terminal rows.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/responses/{response_id}/cancel",
    tag = "responses",
    params(("response_id" = String, Path, description = "ID of the response to cancel")),
    responses(
        (status = 200, description = "The cancelled response object"),
        (status = 400, description = "Response is not in background mode", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Response not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_responses_cancel(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(response_id): Path<String>,
) -> Result<Json<WireResponse>, ApiError> {
    let store = resolve_store(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "cancel").await?;
    let org_id = require_caller_org(auth.as_ref())?;
    let record = store
        .cancel(&response_id, org_id)
        .await
        .map_err(map_store_err)?;
    Ok(Json(record_to_wire(&record)))
}

#[derive(Serialize)]
pub struct DeleteResponse {
    pub id: String,
    pub object: &'static str,
    pub deleted: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Event log replay
// ─────────────────────────────────────────────────────────────────────────────

/// Stream the persisted event log as Server-Sent Events. While the
/// response is still in-progress the handler polls the DB every
/// 250ms for new rows; once the row reaches a terminal status and the
/// caller has caught up to `last_sequence_number`, the stream
/// terminates with a `data: [DONE]` sentinel.
///
/// Each event is emitted with the OpenAI-style named SSE form
/// (`event: <type>\ndata: <payload>\n\n`) so JS SDK callers see the
/// typed events they expect.
///
/// Used by both `GET /v1/responses/{id}?stream=true` (the
/// spec-conformant entry point) and any clients that supply the cursor
/// query parameter.
async fn stream_response_events(
    state: AppState,
    response_id: String,
    org_id: Uuid,
    starting_after: Option<i64>,
    limit: Option<i64>,
) -> Result<axum::response::Response, ApiError> {
    use bytes::Bytes;
    use http::Response as HttpResponse;

    let store = resolve_store(&state)?;
    let Some(db) = state.db.as_ref().cloned() else {
        return Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "responses_persistence_disabled",
            "Response persistence requires a configured database".to_string(),
        ));
    };

    // Verify the response exists and belongs to the caller's org.
    let _record = store
        .get(&response_id, org_id)
        .await
        .map_err(map_store_err)?;

    let starting_after = starting_after.unwrap_or(0).max(0);
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    let store_clone = state.responses_store.as_ref().cloned();
    let events_repo = db.response_events();
    let response_id_clone = response_id.clone();
    let org_id_for_task = org_id;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);

    crate::compat::spawn_detached(async move {
        const TERMINAL_EVENT_TYPES: &[&str] = &[
            "response.completed",
            "response.failed",
            "response.cancelled",
            "response.incomplete",
        ];
        let mut cursor = starting_after;
        loop {
            // Drain everything past the cursor.
            let events = match events_repo
                .list_after(&response_id_clone, cursor, limit)
                .await
            {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!(
                        response_id = %response_id_clone,
                        error = %e,
                        "Failed to list response events for SSE replay"
                    );
                    // Don't leak DB internals to the caller — terminate
                    // the stream cleanly. They can reconnect and we'll
                    // retry.
                    let _ = tx
                        .send(Err(std::io::Error::other("event log read failed")))
                        .await;
                    return;
                }
            };

            let batch_max = events.last().map(|e| e.sequence_number);
            // Event log is the truth source: if any event in this
            // batch is a terminal type, we know nothing else is coming
            // and we can [DONE] right after emitting. This sidesteps
            // the race where row.status updates before
            // last_sequence_number does (the persister commits
            // terminal events via `insert_sync` to make this happen).
            let mut saw_terminal_event = false;
            for ev in events {
                let payload_str = match serde_json::to_string(&ev.payload) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            response_id = %response_id_clone,
                            seq = ev.sequence_number,
                            error = %e,
                            "Skipping unserialisable response event"
                        );
                        continue;
                    }
                };
                if TERMINAL_EVENT_TYPES.contains(&ev.event_type.as_str()) {
                    saw_terminal_event = true;
                }
                // Named-SSE-event form matches OpenAI's Responses API
                // wire format: `event: <type>\ndata: <payload>\n\n`.
                let sse = format!("event: {}\ndata: {}\n\n", ev.event_type, payload_str);
                if tx.send(Ok(Bytes::from(sse))).await.is_err() {
                    return; // client disconnected
                }
            }

            if let Some(seq) = batch_max {
                cursor = seq;
            }

            if saw_terminal_event {
                let _ = tx.send(Ok(Bytes::from_static(b"data: [DONE]\n\n"))).await;
                return;
            }

            // No terminal event yet. Fall back to the row's status for
            // responses that never generated any events (non-streaming
            // requests persisted via `persist_non_streaming`): those
            // reach terminal status without writing to the event log,
            // so without this we'd loop forever.
            let Some(ref store) = store_clone else { return };
            let record = match store.get(&response_id_clone, org_id_for_task).await {
                Ok(r) => r,
                Err(_) => return,
            };
            if record.status.is_terminal() && record.last_sequence_number == 0 {
                let _ = tx.send(Ok(Bytes::from_static(b"data: [DONE]\n\n"))).await;
                return;
            }

            // In-progress — poll again shortly.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Ok(HttpResponse::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
        .unwrap())
}

/// `DELETE /v1/responses/{response_id}` — remove a stored response.
/// Returns the OpenAI-spec deletion confirmation shape.
#[cfg_attr(feature = "utoipa", utoipa::path(
    delete,
    path = "/api/v1/responses/{response_id}",
    tag = "responses",
    params(("response_id" = String, Path, description = "ID of the response to delete")),
    responses(
        (status = 200, description = "Deletion confirmation"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_responses_delete(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(response_id): Path<String>,
) -> Result<Json<DeleteResponse>, ApiError> {
    let store = resolve_store(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "delete").await?;
    let org_id = require_caller_org(auth.as_ref())?;
    let deleted = store
        .delete(&response_id, org_id)
        .await
        .map_err(map_store_err)?;
    Ok(Json(DeleteResponse {
        id: response_id,
        object: "response.deleted",
        deleted,
    }))
}
