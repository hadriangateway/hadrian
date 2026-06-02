//! `/v1/containers/*` endpoints for the shell-tool `/mnt/data`
//! artifact store. Spec-parity surface for OpenAI's Container resource:
//!
//! - `POST   /v1/containers`                         — create a container
//! - `GET    /v1/containers/{container_id}`          — retrieve metadata
//! - `DELETE /v1/containers/{container_id}`          — soft-delete
//! - `POST   /v1/containers/{container_id}/files`    — upload a file
//! - `GET    /v1/containers/{container_id}/files`    — list files
//! - `GET    /v1/containers/{container_id}/files/{file_id}` — file metadata
//! - `GET    /v1/containers/{container_id}/files/{file_id}/content` — raw bytes
//! - `DELETE /v1/containers/{container_id}/files/{file_id}` — remove a file
//!
//! `POST /v1/containers` creates an *empty* container row (no live VM
//! yet) so subsequent responses can attach to it via
//! `environment.type = "container_reference"`. The VM boots on first
//! shell call against the row.

#![cfg(feature = "server")]

use axum::{
    Extension, Json,
    body::Body,
    extract::{FromRequest, Path, Query, State},
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
    /// Optional display name set at creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Memory ceiling captured at creation, formatted as the
    /// OpenAI-spec string (e.g. `"512m"`, `"1g"`). Picks the largest
    /// power-of-two unit that yields a clean integer. `None` when no
    /// limit was set at creation. Matches OpenAI's `Container.memory_limit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_limit: Option<String>,
    /// **Hadrian Extension:** memory ceiling in MiB. Stable integer
    /// form for clients that prefer parsing it directly. Kept alongside
    /// `memory_limit` so existing Hadrian-aware clients don't break.
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_limit_mb: Option<i64>,
    /// `expires_after` block echoed back per OpenAI's spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_after: Option<WireExpiresAfter>,
    /// **Hadrian Extension:** network policy bound to this container
    /// at creation time (or `null` when none was set). Returned as the
    /// same JSON shape that goes into the request body.
    #[serde(skip_serializing_if = "Option::is_none")]
    network_policy: Option<serde_json::Value>,
    /// **Hadrian Extension:** skill UUIDs bound to this container.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skill_ids: Vec<String>,
    /// **Hadrian Extension:** response this container was originally
    /// provisioned for, when implicit-created from a response. `null`
    /// when the container was created via `POST /v1/containers`.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_response_id: Option<String>,
}

#[derive(Serialize)]
pub struct WireExpiresAfter {
    anchor: &'static str,
    minutes: i64,
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
/// (with `data`, `has_more`, plus optional `first_id` / `last_id` for
/// cursor pagination on endpoints that need it).
#[derive(Serialize)]
pub struct WireList<T: Serialize> {
    object: &'static str,
    data: Vec<T>,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListFilesQuery {
    /// Maximum number of files to return. Server clamps to `[1, 1000]`.
    /// Cursor pagination (`after`) is a future enhancement.
    #[serde(default)]
    limit: Option<i64>,
}

/// Query params for `GET /v1/containers`. Matches OpenAI's cursor
/// shape — `after` is an opaque `cntr_<hex>` id and pagination flows
/// newest-first.
#[derive(Debug, Deserialize)]
pub struct ListContainersQuery {
    /// Page size. Clamped to `[1, 100]` (default 20).
    #[serde(default)]
    limit: Option<i64>,
    /// Cursor: the `cntr_<hex>` id of the last item from a prior page.
    /// Unknown ids return an empty page rather than 404.
    #[serde(default)]
    after: Option<String>,
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
    let expires_after = if record.idle_ttl_secs > 0 {
        Some(WireExpiresAfter {
            anchor: "last_active_at",
            minutes: record.idle_ttl_secs / 60,
        })
    } else {
        None
    };
    let network_policy = record
        .network_policy_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    let skill_ids = record
        .skill_ids_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default();
    let memory_limit = record.memory_limit_mb.map(format_memory_limit_mb);
    WireContainer {
        id: record.id,
        object: OBJECT_CONTAINER,
        status: record.status.as_str().to_string(),
        created_at: record.created_at.timestamp(),
        last_active_at: record.last_active_at.timestamp(),
        expires_at,
        idle_ttl_secs: record.idle_ttl_secs,
        runtime: record.runtime_label,
        name: record.name,
        memory_limit,
        memory_limit_mb: record.memory_limit_mb,
        expires_after,
        network_policy,
        skill_ids,
        source_response_id: record.source_response_id,
    }
}

/// Render a MiB-denominated memory limit as the spec string form
/// (`"512m"`, `"1g"`, etc.). Picks `g` when the value is a whole number
/// of GiB, otherwise `m`. Zero or negative values produce `None`-equivalent
/// behaviour at the caller (we never pass them in).
fn format_memory_limit_mb(mb: i64) -> String {
    if mb > 0 && mb % 1024 == 0 {
        format!("{}g", mb / 1024)
    } else {
        format!("{mb}m")
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

/// Resolve the org the request operates in. Falls back to the deployment's
/// `default_org_id` (set in no-auth / anonymous dev mode) so the same
/// endpoints work locally without credentials — mirroring how the responses
/// pipeline attributes anonymous usage to the default org.
fn require_caller_org(
    auth: Option<&Extension<AuthenticatedRequest>>,
    default_org: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    auth.and_then(|Extension(a)| {
        a.api_key()
            .and_then(|k| k.org_id)
            .or_else(|| a.principal().org_id())
    })
    .or(default_org)
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

/// Surface-level validation for skill entries before they're persisted
/// onto a container row. Mirrors what `resolve_and_inject_skills` will
/// later enforce on request — we run it eagerly here so a misshaped
/// `inline` skill never makes it onto a stored row.
fn validate_skill_entry(entry: &crate::api_types::RequestSkill) -> Result<(), ApiError> {
    match entry {
        crate::api_types::RequestSkill::SkillReference(reference) => {
            // A reference is addressed by a prefixed (`skill_…`) or bare UUID,
            // or by a skill name slug. Existence is resolved at request time.
            let id_ok = reference.skill_id.parse::<crate::models::SkillId>().is_ok();
            if !id_ok && crate::models::validate_skill_name(&reference.skill_id).is_err() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_skill_id",
                    format!(
                        "skill id '{}' is not a valid skill id or name",
                        reference.skill_id
                    ),
                ));
            }
            // Version: omit, `latest`, or a positive integer.
            if let Some(v) = reference.version.as_deref()
                && v != "latest"
                && !matches!(v.parse::<i64>(), Ok(n) if n > 0)
            {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_skill_version",
                    format!("invalid skill version `{v}`; use a positive integer or `latest`"),
                ));
            }
            Ok(())
        }
        crate::api_types::RequestSkill::Inline(inline) => {
            if inline.name.trim().is_empty() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_inline_skill",
                    "inline skill `name` must be non-empty",
                ));
            }
            // The name becomes the `/skills/<name>` mount segment, written to
            // the sandbox verbatim, so it must be a path-safe slug. Mirror the
            // resolver's check so a bad name is rejected at creation rather than
            // failing on every later request.
            if crate::models::validate_skill_name(&inline.name).is_err() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_inline_skill",
                    format!("inline skill `name` '{}' is not a valid slug", inline.name),
                ));
            }
            let crate::api_types::InlineSkillSource::Base64 { media_type, data } = &inline.source;
            if media_type != "text/markdown" {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_inline_skill_media_type",
                    format!(
                        "inline skill `{}` uses media_type `{media_type}` — only `text/markdown` is supported today",
                        inline.name
                    ),
                ));
            }
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.as_bytes())
                .map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_inline_skill",
                        format!("inline skill `{}` base64: {e}", inline.name),
                    )
                })?;
            // Mirror the resolver: text/markdown payloads must be valid UTF-8,
            // so reject early rather than failing later at mount time.
            if std::str::from_utf8(&bytes).is_err() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_inline_skill",
                    format!("inline skill `{}` payload is not valid UTF-8", inline.name),
                ));
            }
            Ok(())
        }
    }
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

/// Request body for `POST /v1/containers`.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CreateContainerRequest {
    /// Optional display name (max 255 chars).
    #[serde(default)]
    pub name: Option<String>,
    /// Optional memory ceiling. Accepts OpenAI's `"1g"` / `"512m"`
    /// strings; parsed case-insensitively. Capped by the operator's
    /// `[features.server_tools.shell_limits].max_mem_limit_mb`.
    #[serde(default)]
    pub memory_limit: Option<String>,
    /// Optional idle-TTL hint. Per OpenAI's spec: `{anchor:
    /// "last_active_at", minutes: N}`. Capped by
    /// `[features.containers].max_idle_ttl_secs / 60`.
    #[serde(default)]
    pub expires_after: Option<crate::api_types::responses::ContainerExpiresAfter>,
    /// Optional network policy (same shape as
    /// `tools.shell.environment.network_policy`). OpenAI types this as
    /// a `oneOf { Disabled, Allowlist }` discriminated by `type`;
    /// Hadrian represents it as a single flat object that validates
    /// the same constraints at request resolution, so the schema is
    /// rendered as an opaque `object` to avoid spurious diffs.
    #[serde(default)]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub network_policy: Option<crate::api_types::responses::ShellNetworkPolicy>,
    /// Skills to mount whenever a session is booted against this
    /// container. Matches OpenAI's typed shape — see
    /// [`crate::api_types::RequestSkill`].
    #[serde(default)]
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<Object>))]
    pub skills: Vec<crate::api_types::RequestSkill>,
    /// Files-API ids to copy into `/mnt/data` at container creation.
    /// Spec: `CreateContainerBody.file_ids`. Each id is resolved via
    /// the Files API (same path as `input_file` parts on a response
    /// request) and the bytes are persisted into `container_files`
    /// so the model can read them on first attach.
    #[serde(default)]
    pub file_ids: Vec<String>,
}

/// `POST /v1/containers` — create an unattached container.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/containers",
    tag = "containers",
    request_body = CreateContainerRequest,
    responses(
        (status = 200, description = "The created container metadata"),
        (status = 400, description = "Request rejected", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_create(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Json(body): Json<CreateContainerRequest>,
) -> Result<Json<WireContainer>, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "write").await?;
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;

    let owner = crate::services::responses_pipeline::derive_response_owner(
        &state,
        auth.as_ref().map(|e| &e.0),
    )
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Container creation requires an authenticated owner",
        )
    })?;

    let containers_cfg = &state.config.features.containers;
    let shell_limits = &state.config.features.server_tools.shell_limits;

    // ── Memory limit (request → bytes → MiB column) ──
    let memory_limit_mb: Option<i64> = match body.memory_limit.as_deref() {
        Some(raw) => {
            let bytes =
                crate::services::shell_tool::parse_memory_limit_pub(raw).ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_memory_limit",
                        format!("invalid memory_limit '{raw}'"),
                    )
                })?;
            if let Some(cap_mb) = shell_limits.max_mem_limit_mb {
                let cap_bytes = u64::from(cap_mb) * 1024 * 1024;
                if bytes > cap_bytes {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "memory_limit_exceeds_cap",
                        format!(
                            "memory_limit {} MB exceeds operator cap {} MB",
                            bytes / (1024 * 1024),
                            cap_mb
                        ),
                    ));
                }
            }
            Some(i64::try_from(bytes / (1024 * 1024)).unwrap_or(i64::MAX))
        }
        None => None,
    };

    // ── expires_after ──
    let idle_ttl_secs = match body.expires_after.as_ref() {
        Some(exp) => {
            let max_minutes = (containers_cfg.max_idle_ttl_secs / 60) as u32;
            if exp.minutes > max_minutes {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "expires_after_exceeds_cap",
                    format!(
                        "expires_after.minutes {} exceeds operator cap of {} minutes",
                        exp.minutes, max_minutes
                    ),
                ));
            }
            i64::from(exp.minutes) * 60
        }
        None => containers_cfg.default_idle_ttl_secs as i64,
    };

    // ── network_policy: validate against operator caps (resolver
    //    accepts both inline + reference forms) and persist verbatim.
    if let Some(ref np) = body.network_policy {
        // Wrap in a minimal `ShellEnvironment::ContainerAuto` so the
        // existing resolver does the validation work.
        let env = crate::api_types::responses::ShellEnvironment::ContainerAuto(
            crate::api_types::responses::ShellContainerAuto {
                memory_limit: None,
                network_policy: Some(np.clone()),
                file_ids: None,
                skills: None,
                expires_after: None,
            },
        );
        crate::services::shell_tool::resolve_shell_environment(
            Some(&env),
            shell_limits,
            containers_cfg,
        )
        .map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "network_policy_rejected",
                e.to_string(),
            )
        })?;
    }
    let network_policy_json = body
        .network_policy
        .as_ref()
        .map(|np| serde_json::to_string(np).unwrap_or_default());

    // ── skills: validate references + inline payloads up front so an
    //    invalid skill never makes it onto the container row. We don't
    //    actually mount them here — that happens on the first response
    //    the container backs. Validation: shape, base64 decode, and
    //    media_type. Stored verbatim so per-response resolve picks up
    //    the same payload.
    if !body.skills.is_empty() {
        for entry in &body.skills {
            validate_skill_entry(entry)?;
        }
    }
    let skill_ids_json = if body.skills.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&body.skills).unwrap_or_default())
    };

    // Choose the runtime label so future requests reusing this
    // container know which runtime backend is expected. When no shell
    // runtime is configured we still let the row exist so that an
    // operator switching from `passthrough_openai` to e.g.
    // `microsandbox` later doesn't have to recreate containers.
    let runtime_label = match &state.config.features.shell {
        crate::config::ShellRuntimeConfig::None => "none",
        crate::config::ShellRuntimeConfig::PassthroughOpenAI => "passthrough_openai",
        crate::config::ShellRuntimeConfig::ClientPassthrough => "client_passthrough",
        #[cfg(feature = "runtime-microsandbox")]
        crate::config::ShellRuntimeConfig::Microsandbox(_) => "microsandbox",
        #[cfg(feature = "runtime-opensandbox")]
        crate::config::ShellRuntimeConfig::OpenSandbox(_) => "opensandbox",
    };

    let container_id = format!("cntr_{}", Uuid::new_v4().simple());
    let name = body.name.clone();
    if let Some(ref n) = name
        && n.len() > 255
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "name_too_long",
            "name must be 255 chars or fewer",
        ));
    }

    let record = svc
        .create_explicit(
            container_id.clone(),
            org_id,
            owner,
            runtime_label,
            idle_ttl_secs,
            name,
            memory_limit_mb,
            network_policy_json,
            skill_ids_json,
        )
        .await
        .map_err(map_service_err)?;

    // Spec: `CreateContainerBody.file_ids` — copy the named Files-API
    // uploads into `/mnt/data` immediately so they're present the first
    // time the container is attached. Fetch via the same Files service
    // as `input_file` parts, then persist into `container_files`.
    if !body.file_ids.is_empty() {
        use std::str::FromStr;

        use sha2::{Digest, Sha256};
        let services = state.services.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "files_unavailable",
                "Files service is unavailable; cannot resolve file_ids",
            )
        })?;
        // Scope file_id lookups to the container owner so a caller can't
        // stage another tenant's Files-API uploads. A missing owner
        // scope (service-account owner) fails closed as not-found.
        let (file_owner_type, file_owner_id) = owner.as_file_owner().ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "file_not_found",
                "file_ids are not available for this owner",
            )
        })?;
        let max_bytes = state.config.features.containers.max_bytes_per_file as usize;
        let mut to_persist: Vec<crate::services::containers::PersistFileInput> =
            Vec::with_capacity(body.file_ids.len());
        for raw in &body.file_ids {
            let file_id = crate::models::FileId::from_str(raw).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_file_id",
                    format!("file_id '{raw}' is not a valid identifier"),
                )
            })?;
            let uuid = file_id.into_inner();
            let metadata = services
                .files
                .get_for_owner(uuid, file_owner_type, file_owner_id)
                .await
                .map_err(|e| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "file_lookup_failed",
                        e.to_string(),
                    )
                })?
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "file_not_found",
                        format!("file_id '{raw}' not found"),
                    )
                })?;
            let content = services.files.get_content(uuid).await.map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "file_lookup_failed",
                    e.to_string(),
                )
            })?;
            if content.len() > max_bytes {
                return Err(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "file_too_large",
                    format!(
                        "file_id '{}' size {} exceeds operator cap of {} bytes",
                        raw,
                        content.len(),
                        max_bytes
                    ),
                ));
            }
            let bytes = bytes::Bytes::from(content);
            let hash = Sha256::digest(&bytes);
            let content_hash_hex = hex::encode(hash);
            to_persist.push(crate::services::containers::PersistFileInput {
                file_id: format!("cfile_{}", uuid::Uuid::new_v4().simple()),
                path: format!("/mnt/data/{}", metadata.filename),
                filename: metadata.filename.clone(),
                content_type: metadata.content_type.clone(),
                source: crate::api_types::responses::ContainerFileSource::User,
                content: bytes,
                content_hash_hex,
                source_response_id: None,
                source_call_id: None,
            });
        }
        let svc_for_files = svc.clone();
        svc_for_files
            .persist_files(&container_id, org_id, to_persist)
            .await
            .map_err(map_service_err)?;
    }

    Ok(Json(container_to_wire(record)))
}

/// `GET /v1/containers` — list containers in the caller's org,
/// newest-first. Matches OpenAI's cursor pagination shape.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/containers",
    tag = "containers",
    params(
        ("limit" = Option<i64>, Query, description = "Page size, clamped to 1..=100 (default 20)"),
        ("after" = Option<String>, Query, description = "Cursor: cntr_<hex> id of the last item from a prior page"),
    ),
    responses(
        (status = 200, description = "Containers in the org"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_list(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Query(params): Query<ListContainersQuery>,
) -> Result<Json<WireList<WireContainer>>, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "read").await?;
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;

    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let mut rows = svc
        .list_containers(org_id, limit + 1, params.after.as_deref())
        .await
        .map_err(map_service_err)?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let first_id = rows.first().map(|r| r.id.clone());
    let last_id = rows.last().map(|r| r.id.clone());
    let data = rows.into_iter().map(container_to_wire).collect();
    Ok(Json(WireList {
        object: OBJECT_LIST,
        data,
        has_more,
        first_id,
        last_id,
    }))
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
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;
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
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;

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
        first_id: None,
        last_id: None,
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
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;
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
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;

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
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;

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

/// JSON body for `POST /v1/containers/{container_id}/files` when
/// copying an existing Files-API upload into the container instead of
/// streaming bytes via multipart. Spec shape: `{file_id: "file_..."}`.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateContainerFileRequest {
    /// Files-API id of an already-uploaded file to copy into the
    /// container's `/mnt/data`. Must belong to the caller's org.
    pub file_id: String,
    /// Optional override for the destination path. Same rules as the
    /// multipart variant: rebased under `/mnt/data` and rejects `..`.
    #[serde(default)]
    pub path: Option<String>,
}

/// `POST /v1/containers/{container_id}/files` — upload a file into a
/// container's `/mnt/data`. Accepts either:
///
/// - `multipart/form-data` with a `file` part (required) and optional
///   `path` / `content_type` parts — streams bytes directly.
/// - `application/json` with `{file_id: "file_..."}` — copies an
///   existing Files-API upload into the container (same resolver as
///   the `file_ids` array on `POST /v1/containers`).
///
/// Matches OpenAI's upload contract.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/containers/{container_id}/files",
    tag = "containers",
    params(("container_id" = String, Path)),
    responses(
        (status = 200, description = "Uploaded file metadata"),
        (status = 400, description = "Request rejected", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Container not found", body = crate::openapi::ErrorResponse),
        (status = 413, description = "Payload too large", body = crate::openapi::ErrorResponse),
        (status = 415, description = "Unsupported Content-Type", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_file_upload(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(container_id): Path<String>,
    request: axum::http::Request<Body>,
) -> Result<Json<WireContainerFile>, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "write").await?;
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;

    // 404 the upload up front when the container isn't reachable.
    let container = svc
        .get_container(&container_id, org_id)
        .await
        .map_err(map_service_err)?;
    if !matches!(container.status, crate::db::repos::ContainerStatus::Active) {
        return Err(ApiError::new(
            StatusCode::GONE,
            "container_not_reusable",
            format!(
                "container is in status '{}' and cannot accept uploads",
                container.status.as_str()
            ),
        ));
    }

    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if content_type.starts_with("application/json") {
        let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_body",
                    format!("failed to read request body: {e}"),
                )
            })?;
        let body: CreateContainerFileRequest = serde_json::from_slice(&bytes).map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                format!("invalid JSON body: {e}"),
            )
        })?;
        let owner = crate::services::responses_pipeline::derive_response_owner(
            &state,
            auth.as_ref().map(|e| &e.0),
        )
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "owner_unresolved",
                "could not resolve an owner for this request",
            )
        })?;
        let record = upload_from_file_id(
            &state,
            svc,
            &container_id,
            org_id,
            owner,
            &body.file_id,
            body.path,
        )
        .await?;
        return Ok(Json(file_to_wire(record)));
    }
    if !content_type.starts_with("multipart/") {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
            "Content-Type must be 'multipart/form-data' or 'application/json'",
        ));
    }

    let mut multipart = axum::extract::Multipart::from_request(request, &state)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_multipart",
                format!("malformed multipart request: {e}"),
            )
        })?;

    let max_bytes = state.config.features.containers.max_bytes_per_file as usize;
    let mut filename: Option<String> = None;
    let mut content_bytes: Option<Vec<u8>> = None;
    let mut content_type_field: Option<String> = None;
    let mut path_field: Option<String> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_multipart",
            format!("malformed multipart request: {e}"),
        )
    })? {
        match field.name().unwrap_or("") {
            "file" => {
                filename = field.file_name().map(str::to_string);
                content_type_field = field.content_type().map(str::to_string);
                let mut data: Vec<u8> = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_multipart",
                        format!("failed to read file part: {e}"),
                    )
                })? {
                    if data.len() + chunk.len() > max_bytes {
                        return Err(ApiError::new(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "file_too_large",
                            format!("file size exceeds operator cap of {max_bytes} bytes"),
                        ));
                    }
                    data.extend_from_slice(&chunk);
                }
                content_bytes = Some(data);
            }
            "path" => {
                path_field = Some(field.text().await.unwrap_or_default());
            }
            "content_type" => {
                content_type_field = Some(field.text().await.unwrap_or_default());
            }
            _ => {
                // Skip unknown parts silently — OpenAI clients
                // sometimes attach `purpose` or similar markers.
                let _ = field.bytes().await;
            }
        }
    }

    let content = content_bytes.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_file_part",
            "multipart upload must include a 'file' part",
        )
    })?;
    let filename = filename.unwrap_or_else(|| "upload".to_string());
    let path = normalize_mnt_path(path_field.as_deref(), &filename)?;

    let record = svc
        .upload_file(
            &container_id,
            org_id,
            path,
            filename,
            content_type_field,
            content,
            crate::api_types::responses::ContainerFileSource::User,
            None,
            None,
        )
        .await
        .map_err(map_service_err)?;
    Ok(Json(file_to_wire(record)))
}

/// Resolve the requested destination path under `/mnt/data`. Rebases
/// anything else and rejects `..` traversal.
fn normalize_mnt_path(path_field: Option<&str>, filename: &str) -> Result<String, ApiError> {
    const MNT: &str = crate::services::container_session::MNT_DATA;
    // The relative target comes from the explicit `path` field when
    // present, otherwise from the (user-controlled) upload filename.
    // Both are untrusted, so the traversal guard applies uniformly.
    let normalised = match path_field {
        Some(p) if !p.is_empty() => p.trim_start_matches('/').to_string(),
        _ => filename.trim_start_matches('/').to_string(),
    };
    if normalised.contains("..") {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "path may not contain '..'",
        ));
    }
    if normalised.starts_with("mnt/data/") {
        Ok(format!("/{normalised}"))
    } else {
        Ok(format!("{MNT}/{normalised}"))
    }
}

/// Copy bytes from a Files-API upload into the container. Mirrors the
/// `file_ids` resolution path on `POST /v1/containers` so behaviour is
/// identical between creation-time staging and post-creation copies.
async fn upload_from_file_id(
    state: &AppState,
    svc: &ContainersService,
    container_id: &str,
    org_id: Uuid,
    owner: crate::db::repos::ResponseOwner,
    raw_file_id: &str,
    path_override: Option<String>,
) -> Result<crate::db::repos::ContainerFileRecord, ApiError> {
    use std::str::FromStr;

    let services = state.services.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "files_unavailable",
            "Files service is unavailable; cannot resolve file_id",
        )
    })?;
    let file_id = crate::models::FileId::from_str(raw_file_id).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_file_id",
            format!("file_id '{raw_file_id}' is not a valid identifier"),
        )
    })?;
    let uuid = file_id.into_inner();
    // Scope to the caller's owner so a file_id belonging to another
    // tenant resolves as not-found (no info leak).
    let (file_owner_type, file_owner_id) = owner.as_file_owner().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "file_not_found",
            format!("file_id '{raw_file_id}' not found"),
        )
    })?;
    let metadata = services
        .files
        .get_for_owner(uuid, file_owner_type, file_owner_id)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "file_lookup_failed",
                e.to_string(),
            )
        })?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "file_not_found",
                format!("file_id '{raw_file_id}' not found"),
            )
        })?;
    let content = services.files.get_content(uuid).await.map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file_lookup_failed",
            e.to_string(),
        )
    })?;
    let max_bytes = state.config.features.containers.max_bytes_per_file as usize;
    if content.len() > max_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file_too_large",
            format!(
                "file size {} exceeds operator cap of {} bytes",
                content.len(),
                max_bytes
            ),
        ));
    }
    let path = normalize_mnt_path(path_override.as_deref(), &metadata.filename)?;
    svc.upload_file(
        container_id,
        org_id,
        path,
        metadata.filename.clone(),
        metadata.content_type.clone(),
        content,
        crate::api_types::responses::ContainerFileSource::User,
        None,
        None,
    )
    .await
    .map_err(map_service_err)
}

/// `DELETE /v1/containers/{container_id}/files/{file_id}` — remove
/// a file from a container.
#[cfg_attr(feature = "utoipa", utoipa::path(
    delete,
    path = "/api/v1/containers/{container_id}/files/{file_id}",
    tag = "containers",
    params(("container_id" = String, Path), ("file_id" = String, Path)),
    responses(
        (status = 204, description = "File deleted"),
        (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Authorization denied", body = crate::openapi::ErrorResponse),
        (status = 404, description = "File not found", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Persistence disabled", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
pub async fn api_v1_containers_file_delete(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path((container_id, file_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let svc = resolve_service(&state)?;
    enforce_authz(authz.as_ref(), auth.as_ref(), "delete").await?;
    let org_id = require_caller_org(auth.as_ref(), state.default_org_id)?;
    svc.delete_file(&container_id, &file_id, org_id)
        .await
        .map_err(map_service_err)?;
    Ok(StatusCode::NO_CONTENT)
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
            name: None,
            memory_limit_mb: None,
            network_policy_json: None,
            skill_ids_json: None,
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
    fn memory_limit_string_renders_clean_units() {
        assert_eq!(format_memory_limit_mb(1024), "1g");
        assert_eq!(format_memory_limit_mb(4096), "4g");
        assert_eq!(format_memory_limit_mb(512), "512m");
        assert_eq!(format_memory_limit_mb(1500), "1500m");
    }

    #[test]
    fn wire_container_surfaces_memory_limit_string() {
        let mut r = record(crate::db::repos::ContainerStatus::Active);
        r.memory_limit_mb = Some(1024);
        let wire = container_to_wire(r);
        assert_eq!(wire.memory_limit.as_deref(), Some("1g"));
        assert_eq!(wire.memory_limit_mb, Some(1024));
    }

    #[test]
    fn normalize_mnt_path_rebases_relative_paths() {
        assert_eq!(
            normalize_mnt_path(Some("out.csv"), "fallback.txt").unwrap(),
            "/mnt/data/out.csv"
        );
        assert_eq!(
            normalize_mnt_path(Some("/mnt/data/sub/out.csv"), "fallback.txt").unwrap(),
            "/mnt/data/sub/out.csv"
        );
        assert_eq!(
            normalize_mnt_path(None, "fallback.txt").unwrap(),
            "/mnt/data/fallback.txt"
        );
    }

    #[test]
    fn normalize_mnt_path_rejects_traversal() {
        let err = normalize_mnt_path(Some("../escape.txt"), "x").unwrap_err();
        assert!(err.to_string().starts_with("invalid_path:"), "{err}");
    }

    #[test]
    fn normalize_mnt_path_rejects_traversal_in_filename_fallback() {
        // No `path` field: the untrusted upload filename must still be
        // guarded against `..` traversal.
        let err = normalize_mnt_path(None, "../../etc/passwd").unwrap_err();
        assert!(err.to_string().starts_with("invalid_path:"), "{err}");
    }

    #[test]
    fn create_container_file_request_parses_file_id_json() {
        let body: CreateContainerFileRequest =
            serde_json::from_str(r#"{"file_id":"file_abc"}"#).unwrap();
        assert_eq!(body.file_id, "file_abc");
        assert!(body.path.is_none());
    }

    #[test]
    fn create_container_file_request_parses_file_id_with_path() {
        let body: CreateContainerFileRequest =
            serde_json::from_str(r#"{"file_id":"file_abc","path":"sub/out.csv"}"#).unwrap();
        assert_eq!(body.file_id, "file_abc");
        assert_eq!(body.path.as_deref(), Some("sub/out.csv"));
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
