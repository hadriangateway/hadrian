//! Read-only `/v1/containers/*` endpoints for the shell-tool
//! `/mnt/data` artifact store.
//!
//! Phase 3 ships GET routes only:
//!
//! - `GET /v1/containers/{container_id}` — container metadata
//! - `GET /v1/containers/{container_id}/files` — list files in the container
//! - `GET /v1/containers/{container_id}/files/{file_id}` — file metadata
//! - `GET /v1/containers/{container_id}/files/{file_id}/content` — raw bytes
//!
//! `POST` and `DELETE` routes are reserved for Phase 4 (cross-response
//! reuse + lifecycle management).

#![cfg(feature = "server")]

use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ApiError;
use crate::{
    AppState,
    auth::AuthenticatedRequest,
    db::repos::{ContainerFileRecord, ContainerRecord},
    middleware::AuthzContext,
    services::containers::{ContainersService, ContainersServiceError},
};

const OBJECT_CONTAINER: &str = "container";
const OBJECT_CONTAINER_FILE: &str = "container.file";
const OBJECT_LIST: &str = "list";

/// Wire shape for a container resource. Matches OpenAI's
/// `Container` object so generic clients work without modification.
#[derive(Serialize)]
pub struct WireContainer {
    id: String,
    object: &'static str,
    /// `active` | `expired` | `deleted`.
    status: String,
    /// Unix timestamp (seconds).
    created_at: i64,
    /// Unix timestamp of the last successful activity.
    last_active_at: i64,
    /// Unix timestamp when this container will expire (or did expire,
    /// for terminal statuses). For `active` containers this is a
    /// forward-looking estimate computed as
    /// `last_active_at + idle_ttl_secs`; every shell call rolls
    /// `last_active_at` forward, so the field moves with activity. For
    /// `expired` / `deleted` containers it's the exact moment the row
    /// transitioned. Always present.
    ///
    /// **Hadrian Extension:** OpenAI's `Container` object surfaces only
    /// `created_at`; the proactive expiry estimate is a Hadrian addition
    /// so clients can plan reuse without polling. The field name and
    /// type match OpenAI's other expiring resources (e.g. Files API).
    expires_at: i64,
    /// **Hadrian Extension:** idle TTL applied to this container, in
    /// seconds. Stable for the row's lifetime; clients can recompute
    /// `expires_at` themselves from `last_active_at + idle_ttl_secs`.
    idle_ttl_secs: i64,
    /// **Hadrian Extension:** runtime that backed the session.
    runtime: String,
    /// **Hadrian Extension:** response this container was originally
    /// provisioned for. Always set for Phase 3 (containers can only
    /// be created via a response); Phase 4's manual create will leave
    /// it null.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_response_id: Option<String>,
}

/// Wire shape for one `container.file` row. Matches OpenAI's response
/// to `POST /v1/containers/{id}/files` and `GET .../{file_id}`.
#[derive(Serialize)]
pub struct WireContainerFile {
    id: String,
    object: &'static str,
    container_id: String,
    /// Absolute path inside the container, always under `/mnt/data/`.
    path: String,
    /// **Hadrian Extension:** the display name (basename of `path`).
    filename: String,
    /// Size of the file in bytes.
    bytes: i64,
    /// `user` (staged from an `input_file` part) or `assistant` (written
    /// by the model during a shell command).
    source: String,
    /// Best-effort MIME type.
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    /// Unix timestamp (seconds).
    created_at: i64,
}

/// Wrapper for list endpoints. Matches OpenAI's `list` object shape
/// (with `data`, `has_more`, plus extension cursors for forward/back
/// pagination — Phase 4 will populate the cursors).
#[derive(Serialize)]
pub struct WireList<T: Serialize> {
    object: &'static str,
    data: Vec<T>,
    has_more: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListFilesQuery {
    /// Maximum number of files to return. Server clamps to `[1, 1000]`.
    /// Phase 3 has no cursor pagination yet — Phase 4 will add `after`.
    #[serde(default)]
    limit: Option<i64>,
}

fn container_to_wire(record: ContainerRecord) -> WireContainer {
    // For active containers compute a forward-looking expiry from the
    // last_active_at + idle_ttl. The reaper job uses the same formula
    // to decide when to flip the row to `expired`; surfacing it lets
    // clients reuse a container right before it would have lapsed
    // without having to poll. Terminal statuses use the persisted
    // moment of transition.
    let expires_at = record
        .expires_at
        .map(|t| t.timestamp())
        .unwrap_or_else(|| record.last_active_at.timestamp() + record.idle_ttl_secs);
    WireContainer {
        id: record.id,
        object: OBJECT_CONTAINER,
        status: record.status.as_str().to_string(),
        created_at: record.created_at.timestamp(),
        last_active_at: record.last_active_at.timestamp(),
        expires_at,
        idle_ttl_secs: record.idle_ttl_secs,
        runtime: record.runtime_label,
        source_response_id: record.source_response_id,
    }
}

fn file_to_wire(record: ContainerFileRecord) -> WireContainerFile {
    WireContainerFile {
        id: record.id,
        object: OBJECT_CONTAINER_FILE,
        container_id: record.container_id,
        path: record.path,
        filename: record.filename,
        bytes: record.size_bytes,
        source: record.source.as_str().to_string(),
        content_type: record.content_type,
        created_at: record.created_at.timestamp(),
    }
}

fn resolve_service(state: &AppState) -> Result<&ContainersService, ApiError> {
    state.containers_service.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "containers_persistence_disabled",
            "Container persistence requires a configured database".to_string(),
        )
    })
}

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
            "container",
            action,
            None,
            None,
            org_id.as_deref(),
            project_id.as_deref(),
        )
        .await
        .map_err(|e| ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string()))
}

fn map_service_err(e: ContainersServiceError) -> ApiError {
    match e {
        ContainersServiceError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "container_not_found",
            "No such container or container file",
        ),
        ContainersServiceError::Expired(_) => ApiError::new(
            StatusCode::GONE,
            "container_expired",
            "Container has expired and cannot be reused",
        ),
        ContainersServiceError::ContentUnavailable(msg) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file_content_unavailable",
            msg,
        ),
        ContainersServiceError::Db(msg) => {
            tracing::error!(error = %msg, "Containers service DB error");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "An internal error occurred",
            )
        }
    }
}

/// `GET /v1/containers/{container_id}` — retrieve a container.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/containers/{container_id}",
    tag = "containers",
    params(("container_id" = String, Path, description = "Container ID (`cntr_<hex>`)")),
    responses(
        (status = 200, description = "The container metadata"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Container not found", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_get(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(container_id): Path<String>,
) -> Result<Json<WireContainer>, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "read").await?;
    let org_id = require_caller_org(auth.as_ref())?;
    let record = svc
        .get_container(&container_id, org_id)
        .await
        .map_err(map_service_err)?;
    Ok(Json(container_to_wire(record)))
}

/// `GET /v1/containers/{container_id}/files` — list files.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/containers/{container_id}/files",
    tag = "containers",
    params(
        ("container_id" = String, Path, description = "Container ID"),
        ("limit" = Option<i64>, Query, description = "Page size, clamped to 1..=1000"),
    ),
    responses(
        (status = 200, description = "Files in the container"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Container not found", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_list_files(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(container_id): Path<String>,
    Query(params): Query<ListFilesQuery>,
) -> Result<Json<WireList<WireContainerFile>>, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "read").await?;
    let org_id = require_caller_org(auth.as_ref())?;

    // 404 the listing when the container itself isn't reachable from
    // this org. Without this an enumerator could distinguish "no
    // container" from "container exists but is empty".
    svc.get_container(&container_id, org_id)
        .await
        .map_err(map_service_err)?;

    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    // Fetch limit+1 to compute has_more, then truncate.
    let mut rows = svc
        .list_files(&container_id, org_id, limit + 1)
        .await
        .map_err(map_service_err)?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let data = rows.into_iter().map(file_to_wire).collect();
    Ok(Json(WireList {
        object: OBJECT_LIST,
        data,
        has_more,
    }))
}

/// `GET /v1/containers/{container_id}/files/{file_id}` — file metadata.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/containers/{container_id}/files/{file_id}",
    tag = "containers",
    params(
        ("container_id" = String, Path),
        ("file_id" = String, Path),
    ),
    responses(
        (status = 200, description = "The container file metadata"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "File not found", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_file_get(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path((container_id, file_id)): Path<(String, String)>,
) -> Result<Json<WireContainerFile>, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "read").await?;
    let org_id = require_caller_org(auth.as_ref())?;
    let record = svc
        .get_file(&file_id, org_id)
        .await
        .map_err(map_service_err)?;
    if record.container_id != container_id {
        // Don't leak the existence of a file in a different container —
        // return the same 404 as a non-existent id.
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "container_not_found",
            "No such container or container file",
        ));
    }
    Ok(Json(file_to_wire(record)))
}

/// `DELETE /v1/containers/{container_id}` — soft-delete a container.
///
/// Sets `status='deleted'` and evicts the matching in-memory session
/// from the registry so the VM is torn down on the next Arc drop.
/// The underlying `container_files` rows cascade-delete with the
/// container row.
#[cfg_attr(feature = "utoipa", utoipa::path(
    delete,
    path = "/api/v1/containers/{container_id}",
    tag = "containers",
    params(("container_id" = String, Path)),
    responses(
        (status = 204, description = "Container deleted"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Container not found", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_delete(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(container_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "delete").await?;
    let org_id = require_caller_org(auth.as_ref())?;

    // Mark the row deleted first so a concurrent reuse attempt fails
    // the active-status check before we evict the session. The
    // service returns 404 when the row isn't there or belongs to a
    // different org.
    let patch = crate::db::repos::ContainerPatch {
        status: Some(crate::db::repos::ContainerStatus::Deleted),
        expires_at: Some(chrono::Utc::now()),
        ..Default::default()
    };
    let updated = svc
        .update_within_org(&container_id, org_id, patch)
        .await
        .map_err(map_service_err)?;
    if updated.is_none() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "container_not_found",
            "No such container",
        ));
    }

    // Evict from the in-memory registry. Dropping the Arc lets
    // ContainerSession::drop run, which detaches a terminate task.
    state.container_session_registry.remove(&container_id);

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/containers/{container_id}/files/{file_id}/content` — raw bytes.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/containers/{container_id}/files/{file_id}/content",
    tag = "containers",
    params(
        ("container_id" = String, Path),
        ("file_id" = String, Path),
    ),
    responses(
        (status = 200, description = "Raw file bytes; Content-Type comes from the row"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "File not found", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_file_content(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path((container_id, file_id)): Path<(String, String)>,
) -> Result<Response<Body>, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "read").await?;
    let org_id = require_caller_org(auth.as_ref())?;

    // Two-step fetch so we get the content-type + container check from
    // the row before reading bytes — keeps the 404 path cheap.
    let record = svc
        .get_file(&file_id, org_id)
        .await
        .map_err(map_service_err)?;
    if record.container_id != container_id {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "container_not_found",
            "No such container or container file",
        ));
    }

    let bytes = svc
        .read_content(&file_id, org_id)
        .await
        .map_err(map_service_err)?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        record
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream")
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );
    if let Ok(disp) = content_disposition_attachment(&record.filename).parse() {
        headers.insert(header::CONTENT_DISPOSITION, disp);
    }
    headers.insert(header::CONTENT_LENGTH, bytes.len().into());

    let mut response = Response::new(Body::from(bytes));
    *response.headers_mut() = headers;
    Ok(response)
}

/// Build an RFC 6266 `Content-Disposition: attachment` header value
/// that survives any byte sequence in `filename`.
///
/// Emits both an ASCII-safe quoted form (with `"` / `\` / controls /
/// non-ASCII swapped for `_`) and an RFC 5987 `filename*=UTF-8''…`
/// percent-encoded form. Modern clients prefer the latter and round-
/// trip Unicode losslessly; legacy clients fall back to the quoted
/// form. Either way the header parses — no silent drop when the model
/// produces a filename with `"` or control chars.
fn content_disposition_attachment(filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if matches!(c, ' '..='~') && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = percent_encode_rfc5987(filename);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// RFC 5987 `value-chars` percent-encoder. Encodes everything that
/// isn't in the `attr-char` set so the result can land in an
/// `ext-value` like `filename*=UTF-8''…`.
fn percent_encode_rfc5987(s: &str) -> String {
    // attr-char = ALPHA / DIGIT / one of "!#$&+-.^_`|~"
    fn is_attr_char(b: u8) -> bool {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            )
    }
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if is_attr_char(*byte) {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::db::repos::ResponseOwnerType;

    fn record(status: crate::db::repos::ContainerStatus) -> ContainerRecord {
        ContainerRecord {
            id: "cntr_test".into(),
            org_id: Uuid::nil(),
            owner_type: ResponseOwnerType::User,
            owner_id: Uuid::nil(),
            status,
            runtime_label: "microsandbox".into(),
            source_response_id: None,
            idle_ttl_secs: 1200,
            last_active_at: chrono::Utc.timestamp_opt(1_000_000, 0).unwrap(),
            created_at: chrono::Utc.timestamp_opt(999_000, 0).unwrap(),
            expires_at: None,
        }
    }

    #[test]
    fn active_container_projects_expiry_from_last_active() {
        let wire = container_to_wire(record(crate::db::repos::ContainerStatus::Active));
        assert_eq!(wire.last_active_at, 1_000_000);
        assert_eq!(wire.idle_ttl_secs, 1200);
        assert_eq!(wire.expires_at, 1_001_200);
    }

    #[test]
    fn terminal_container_uses_persisted_expiry() {
        let mut r = record(crate::db::repos::ContainerStatus::Expired);
        r.expires_at = Some(chrono::Utc.timestamp_opt(1_000_500, 0).unwrap());
        let wire = container_to_wire(r);
        assert_eq!(wire.expires_at, 1_000_500);
    }

    #[test]
    fn content_disposition_escapes_quotes_and_backslashes() {
        let h = content_disposition_attachment("evil\"name.txt");
        // ASCII fallback strips the offending quote.
        assert!(h.contains("filename=\"evil_name.txt\""));
        // Ext form percent-encodes it.
        assert!(h.contains("filename*=UTF-8''evil%22name.txt"));
    }

    #[test]
    fn content_disposition_handles_crlf_and_controls() {
        let h = content_disposition_attachment("a\r\nInjected: yes\nb");
        // No bare CR/LF survives in the header value — header parsers
        // reject those, which was the bug we're fixing.
        assert!(!h.contains('\r'));
        assert!(!h.contains('\n'));
        // And the encoded form preserves the original bytes for clients
        // that decode it.
        assert!(h.contains("%0D%0A"));
    }

    #[test]
    fn content_disposition_preserves_unicode_in_ext_form() {
        let h = content_disposition_attachment("café.csv");
        // ASCII fallback degrades one underscore per non-ASCII char;
        // ext form percent-encodes the UTF-8 bytes losslessly.
        assert!(h.contains("filename=\"caf_.csv\""), "got: {h}");
        assert!(h.contains("filename*=UTF-8''caf%C3%A9.csv"), "got: {h}");
    }

    #[test]
    fn content_disposition_passes_through_plain_ascii() {
        let h = content_disposition_attachment("report.csv");
        assert!(h.contains("filename=\"report.csv\""));
        assert!(h.contains("filename*=UTF-8''report.csv"));
    }
}
