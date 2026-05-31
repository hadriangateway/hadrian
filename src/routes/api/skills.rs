//! OpenAI-compatible `/v1/skills` endpoints, with Hadrian extensions
//! (org/team/project/user ownership and JSON file arrays alongside the spec's
//! zip/multipart upload). Skills are immutable and versioned; the only way to
//! change content is to publish a new version.
//!
//! Server-only: uploads parse multipart/zip and downloads emit zip via the
//! `skill_zip` helper (which pulls in the `zip` crate).

use std::collections::HashMap;

use axum::{
    Extension, Json,
    extract::{FromRequest, Multipart, Path, Query, Request, State},
    response::{IntoResponse, Response},
};
use http::{StatusCode, header};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    ApiError, SortOrder, check_resource_access_optional, extract_identity_memberships, get_services,
};
use crate::{
    AppState,
    auth::AuthenticatedRequest,
    db::{
        ListParams,
        repos::{Cursor, CursorDirection},
    },
    middleware::AuthzContext,
    models::{
        AuditActorType, CreateAuditLog, CreateSkill, CreateSkillVersion, SKILL_MAIN_FILE, Skill,
        SkillFile, SkillFileInput, SkillFileManifest, SkillId, SkillOwner, SkillOwnerType,
        SkillVersion, SkillVersionId, VectorStoreOwnerType, validate_skill_path,
    },
    services::{Services, skill_zip},
};

// ===========================================================================
// Wire response types (OpenAI shape + Hadrian extension fields)
// ===========================================================================

/// A skill (OpenAI `SkillResource` + Hadrian extensions). Timestamps are unix
/// seconds; the projection reflects the **default** version's metadata/files.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SkillResource {
    /// Unique identifier (`skill_<uuid>`).
    pub id: String,
    /// Object type, always `skill`.
    pub object: String,
    pub name: String,
    pub description: String,
    /// Unix timestamp (seconds) for when the skill was created.
    pub created_at: i64,
    /// Default version number.
    pub default_version: String,
    /// Latest version number.
    pub latest_version: String,
    /// **Hadrian Extension:** owner kind (organization/team/project/user).
    pub owner_type: String,
    /// **Hadrian Extension:** owner id.
    pub owner_id: String,
    /// **Hadrian Extension:** default version's total file size in bytes.
    pub total_bytes: i64,
    /// **Hadrian Extension:** default version's files (get-by-id only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SkillFile>,
    /// **Hadrian Extension:** default version's file summary (list only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files_manifest: Vec<SkillFileManifest>,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — hide from the user `/` menu.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_invocable: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — block model auto-invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_model_invocation: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter — tools the skill may use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// **Hadrian Extension:** SKILL.md frontmatter — argument hint for autocomplete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// **Hadrian Extension:** origin URL when imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// **Hadrian Extension:** git ref when imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// **Hadrian Extension:** unknown/forward-compat frontmatter keys, preserved verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
}

/// An immutable skill version (OpenAI `SkillVersionResource` + extensions).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SkillVersionResource {
    /// Object type, always `skill.version`.
    pub object: String,
    /// Unique identifier (`skillver_<uuid>`).
    pub id: String,
    /// Identifier of the parent skill (`skill_<uuid>`).
    pub skill_id: String,
    /// Version number for this skill.
    pub version: String,
    /// Unix timestamp (seconds) for when the version was created.
    pub created_at: i64,
    pub name: String,
    pub description: String,
    /// **Hadrian Extension:** total file size in bytes.
    pub total_bytes: i64,
    /// **Hadrian Extension:** files (get-version only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SkillFile>,
    /// **Hadrian Extension:** file summary (list only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files_manifest: Vec<SkillFileManifest>,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — hide from the user `/` menu.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_invocable: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — block model auto-invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_model_invocation: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter — tools the skill may use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// **Hadrian Extension:** SKILL.md frontmatter — argument hint for autocomplete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// **Hadrian Extension:** origin URL when imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// **Hadrian Extension:** git ref when imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// **Hadrian Extension:** unknown/forward-compat frontmatter keys, preserved verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
}

/// Paginated list of skills.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SkillListResource {
    pub object: String,
    pub data: Vec<SkillResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,
    pub has_more: bool,
}

/// Paginated list of skill versions.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SkillVersionListResource {
    pub object: String,
    pub data: Vec<SkillVersionResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,
    pub has_more: bool,
}

/// Response for a deleted skill.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DeletedSkillResource {
    pub id: String,
    pub object: String,
    pub deleted: bool,
}

/// Response for a deleted skill version.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DeletedSkillVersionResource {
    pub id: String,
    pub object: String,
    pub deleted: bool,
    pub version: String,
}

// ===========================================================================
// Request body / query types
// ===========================================================================

/// Create-skill request body (`application/json`). **Hadrian Extension:** the
/// spec uploads binary `files[]` / a zip via multipart; this JSON variant takes
/// a `{path, content}` file array plus optional owner/name/frontmatter fields.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateSkillBody {
    /// **Hadrian Extension:** owner; defaults to the API key's scope when omitted.
    #[serde(default)]
    pub owner: Option<SkillOwner>,
    /// **Hadrian Extension:** skill name slug; sniffed from SKILL.md frontmatter when omitted.
    #[serde(default)]
    pub name: Option<String>,
    /// **Hadrian Extension:** description; sniffed from SKILL.md frontmatter when omitted.
    #[serde(default)]
    pub description: Option<String>,
    /// **Hadrian Extension:** skill files as `{path, content}` objects (the spec
    /// uploads binary `files[]` / a zip via multipart instead).
    #[serde(default)]
    pub files: Vec<SkillFileInput>,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — hide from the user `/` menu.
    #[serde(default)]
    pub user_invocable: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — block model auto-invocation.
    #[serde(default)]
    pub disable_model_invocation: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter — tools the skill may use.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// **Hadrian Extension:** SKILL.md frontmatter — argument hint for autocomplete.
    #[serde(default)]
    pub argument_hint: Option<String>,
    /// **Hadrian Extension:** origin URL when imported.
    #[serde(default)]
    pub source_url: Option<String>,
    /// **Hadrian Extension:** git ref when imported.
    #[serde(default)]
    pub source_ref: Option<String>,
    /// **Hadrian Extension:** unknown/forward-compat frontmatter keys, preserved verbatim.
    #[serde(default)]
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
}

/// Create-version request body (`application/json`).
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateSkillVersionBody {
    /// **Hadrian Extension:** skill files as `{path, content}` objects (the spec
    /// uploads binary `files[]` / a zip via multipart instead).
    #[serde(default)]
    pub files: Vec<SkillFileInput>,
    /// **Hadrian Extension:** description; sniffed from SKILL.md frontmatter when omitted.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether to set this version as the default.
    #[serde(default)]
    pub default: bool,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — hide from the user `/` menu.
    #[serde(default)]
    pub user_invocable: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter flag — block model auto-invocation.
    #[serde(default)]
    pub disable_model_invocation: Option<bool>,
    /// **Hadrian Extension:** SKILL.md frontmatter — tools the skill may use.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// **Hadrian Extension:** SKILL.md frontmatter — argument hint for autocomplete.
    #[serde(default)]
    pub argument_hint: Option<String>,
    /// **Hadrian Extension:** origin URL when imported.
    #[serde(default)]
    pub source_url: Option<String>,
    /// **Hadrian Extension:** git ref when imported.
    #[serde(default)]
    pub source_ref: Option<String>,
    /// **Hadrian Extension:** unknown/forward-compat frontmatter keys, preserved verbatim.
    #[serde(default)]
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
}

/// Update the default version pointer for a skill.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SetDefaultSkillVersionBody {
    /// The skill version number to set as default.
    pub default_version: String,
}

/// Query parameters for listing skills.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema, utoipa::IntoParams))]
pub struct ListSkillsQuery {
    /// **Hadrian Extension:** owner kind for single-owner listing.
    pub owner_type: Option<String>,
    /// **Hadrian Extension:** owner id for single-owner listing.
    pub owner_id: Option<Uuid>,
    #[cfg_attr(feature = "utoipa", param(minimum = 1, maximum = 100))]
    pub limit: Option<i64>,
    #[serde(default)]
    pub order: Option<SortOrder>,
    pub after: Option<String>,
    /// **Hadrian Extension:** cursor for backward pagination (id of the first
    /// item from a previous page).
    pub before: Option<String>,
}

/// Query parameters for listing skill versions.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema, utoipa::IntoParams))]
pub struct ListSkillVersionsQuery {
    #[cfg_attr(feature = "utoipa", param(minimum = 1, maximum = 100))]
    pub limit: Option<i64>,
    #[serde(default)]
    pub order: Option<SortOrder>,
    pub after: Option<String>,
    /// **Hadrian Extension:** cursor for backward pagination (id of the first
    /// item from a previous page).
    pub before: Option<String>,
}

// ===========================================================================
// Conversions / helpers
// ===========================================================================

fn skill_to_wire(s: Skill) -> SkillResource {
    SkillResource {
        id: SkillId::new(s.id).to_string(),
        object: "skill".to_string(),
        name: s.name,
        description: s.description,
        created_at: s.created_at.timestamp(),
        default_version: s.default_version_seq.to_string(),
        latest_version: s.latest_version_seq.to_string(),
        owner_type: s.owner_type.as_str().to_string(),
        owner_id: s.owner_id.to_string(),
        total_bytes: s.total_bytes,
        files: s.files,
        files_manifest: s.files_manifest,
        user_invocable: s.user_invocable,
        disable_model_invocation: s.disable_model_invocation,
        allowed_tools: s.allowed_tools,
        argument_hint: s.argument_hint,
        source_url: s.source_url,
        source_ref: s.source_ref,
        frontmatter_extra: s.frontmatter_extra,
    }
}

fn version_to_wire(v: SkillVersion) -> SkillVersionResource {
    SkillVersionResource {
        object: "skill.version".to_string(),
        id: SkillVersionId::new(v.id).to_string(),
        skill_id: SkillId::new(v.skill_id).to_string(),
        version: v.version_seq.to_string(),
        created_at: v.created_at.timestamp(),
        name: v.name,
        description: v.description,
        total_bytes: v.total_bytes,
        files: v.files,
        files_manifest: v.files_manifest,
        user_invocable: v.user_invocable,
        disable_model_invocation: v.disable_model_invocation,
        allowed_tools: v.allowed_tools,
        argument_hint: v.argument_hint,
        source_url: v.source_url,
        source_ref: v.source_ref,
        frontmatter_extra: v.frontmatter_extra,
    }
}

fn to_vs_owner_type(t: SkillOwnerType) -> VectorStoreOwnerType {
    match t {
        SkillOwnerType::Organization => VectorStoreOwnerType::Organization,
        SkillOwnerType::Team => VectorStoreOwnerType::Team,
        SkillOwnerType::Project => VectorStoreOwnerType::Project,
        SkillOwnerType::User => VectorStoreOwnerType::User,
    }
}

/// Derive a default owner from the API key's scope (project → org → user).
fn derive_skill_owner(auth: Option<&AuthenticatedRequest>) -> Option<SkillOwner> {
    let auth = auth?;
    if let Some(k) = auth.api_key() {
        if let Some(project_id) = k.project_id {
            return Some(SkillOwner::Project { project_id });
        }
        if let Some(organization_id) = k.org_id {
            return Some(SkillOwner::Organization { organization_id });
        }
        if let Some(user_id) = k.user_id {
            return Some(SkillOwner::User { user_id });
        }
    }
    auth.user_id().map(|user_id| SkillOwner::User { user_id })
}

/// Audit-log org/project context for a skill owner.
fn audit_context(owner_type: SkillOwnerType, owner_id: Uuid) -> (Option<Uuid>, Option<Uuid>) {
    match owner_type {
        SkillOwnerType::Organization => (Some(owner_id), None),
        SkillOwnerType::Project => (None, Some(owner_id)),
        SkillOwnerType::Team | SkillOwnerType::User => (None, None),
    }
}

async fn enforce_authz(
    authz: Option<&AuthzContext>,
    auth: Option<&AuthenticatedRequest>,
    action: &str,
) -> Result<(), ApiError> {
    if let Some(authz) = authz {
        let org_id = auth.and_then(|a| a.api_key().and_then(|k| k.org_id.map(|id| id.to_string())));
        let project_id = auth.and_then(|a| {
            a.api_key()
                .and_then(|k| k.project_id.map(|id| id.to_string()))
        });
        authz
            .require_api(
                "skill",
                action,
                None,
                None,
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn audit_skill(
    services: &Services,
    auth: Option<&AuthenticatedRequest>,
    action: &str,
    resource_id: Uuid,
    owner_type: SkillOwnerType,
    owner_id: Uuid,
    details: serde_json::Value,
) {
    let api_key_id = auth.and_then(|a| a.api_key().map(|k| k.key.id));
    let (mut org_id, project_id) = audit_context(owner_type, owner_id);
    // Team/user-owned skills carry no org via the owner; fall back to the API
    // key's org so the audit row stays org-attributable.
    if org_id.is_none() {
        org_id = auth.and_then(|a| a.api_key()).and_then(|k| k.org_id);
    }
    let _ = services
        .audit_logs
        .create(CreateAuditLog {
            actor_type: api_key_id
                .map(|_| AuditActorType::ApiKey)
                .unwrap_or(AuditActorType::System),
            actor_id: api_key_id,
            action: action.to_string(),
            resource_type: "skill".to_string(),
            resource_id,
            org_id,
            project_id,
            details,
            ip_address: None,
            user_agent: None,
        })
        .await;
}

/// Parsed skill request body, normalized from JSON or multipart.
#[derive(Default)]
struct ParsedSkillBody {
    owner: Option<SkillOwner>,
    name: Option<String>,
    description: Option<String>,
    user_invocable: Option<bool>,
    disable_model_invocation: Option<bool>,
    allowed_tools: Option<Vec<String>>,
    argument_hint: Option<String>,
    source_url: Option<String>,
    source_ref: Option<String>,
    frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
    files: Vec<SkillFileInput>,
    make_default: bool,
}

impl ParsedSkillBody {
    /// Fill missing name/description/flags from the SKILL.md frontmatter.
    fn merge_frontmatter(&mut self) {
        let Some(main) = self.files.iter().find(|f| f.path == SKILL_MAIN_FILE) else {
            return;
        };
        let fm = skill_zip::parse_skill_frontmatter(&main.content);
        self.name = self.name.take().or(fm.name);
        self.description = self.description.take().or(fm.description);
        self.user_invocable = self.user_invocable.or(fm.user_invocable);
        self.disable_model_invocation = self
            .disable_model_invocation
            .or(fm.disable_model_invocation);
        self.allowed_tools = self.allowed_tools.take().or(fm.allowed_tools);
        self.argument_hint = self.argument_hint.take().or(fm.argument_hint);
    }
}

fn json_err(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        format!("invalid JSON body: {e}"),
    )
}

fn content_type_of(request: &Request) -> String {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Collect skill files (+ scalar fields) from a multipart upload. A single zip
/// part is unpacked as a directory bundle; otherwise each part is one file.
async fn collect_multipart(
    state: &AppState,
    request: Request,
    max_bytes: u64,
    max_files: usize,
) -> Result<ParsedSkillBody, ApiError> {
    let mut multipart = Multipart::from_request(request, state).await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_multipart",
            format!("malformed multipart request: {e}"),
        )
    })?;

    let mut owner_type: Option<String> = None;
    let mut owner_id: Option<Uuid> = None;
    let mut parsed = ParsedSkillBody::default();
    let mut raw_files: Vec<(String, Option<String>, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_multipart",
            format!("malformed multipart request: {e}"),
        )
    })? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "files" | "file" => {
                let filename = field.file_name().map(str::to_string).unwrap_or_default();
                let ct = field.content_type().map(str::to_string);
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| {
                        ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "file_read_error",
                            format!("failed to read file part: {e}"),
                        )
                    })?
                    .to_vec();
                total = total.saturating_add(bytes.len() as u64);
                if max_bytes > 0 && total > max_bytes {
                    return Err(ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "skill_too_large",
                        format!("skill bundle exceeds the maximum size of {max_bytes} bytes"),
                    ));
                }
                raw_files.push((filename, ct, bytes));
            }
            "owner_type" => owner_type = Some(field.text().await.unwrap_or_default()),
            "owner_id" => {
                let v = field.text().await.unwrap_or_default();
                owner_id = Some(Uuid::parse_str(&v).map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_owner_id",
                        format!("invalid owner_id: {v}"),
                    )
                })?);
            }
            "name" => parsed.name = Some(field.text().await.unwrap_or_default()),
            "description" => parsed.description = Some(field.text().await.unwrap_or_default()),
            "default" => {
                let v = field.text().await.unwrap_or_default();
                parsed.make_default = matches!(v.to_ascii_lowercase().as_str(), "true" | "1");
            }
            _ => {}
        }
    }

    // Resolve owner from scalar fields if both present.
    if let (Some(ot), Some(oid)) = (owner_type.as_deref(), owner_id) {
        parsed.owner = Some(owner_from_parts(ot, oid)?);
    }

    parsed.files = files_from_multipart(raw_files, max_bytes, max_files)?;
    Ok(parsed)
}

/// Turn collected multipart parts into skill files: a lone zip part is
/// unpacked; otherwise each part is a directory file (path = its filename).
fn files_from_multipart(
    raw: Vec<(String, Option<String>, Vec<u8>)>,
    max_bytes: u64,
    max_files: usize,
) -> Result<Vec<SkillFileInput>, ApiError> {
    let is_zip = |name: &str, ct: &Option<String>| {
        name.to_ascii_lowercase().ends_with(".zip")
            || matches!(
                ct.as_deref(),
                Some("application/zip") | Some("application/x-zip-compressed")
            )
    };

    if raw.len() == 1 && is_zip(&raw[0].0, &raw[0].1) {
        return skill_zip::unpack_zip_to_files(&raw[0].2, max_bytes, max_files).map_err(zip_err);
    }

    // Directory upload: the zip branch enforces the cap inside the unpacker, so
    // bound the part count here.
    check_file_count(raw.len(), max_files)?;
    let mut files = Vec::with_capacity(raw.len());
    for (name, ct, bytes) in raw {
        if name.is_empty() {
            continue;
        }
        validate_skill_path(&name).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_path",
                format!("invalid file path in bundle: {name}"),
            )
        })?;
        let content = String::from_utf8(bytes).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "binary_not_supported",
                format!("file '{name}' is not valid UTF-8 text (binary files are unsupported)"),
            )
        })?;
        files.push(SkillFileInput {
            path: name,
            content,
            content_type: ct,
        });
    }
    Ok(files)
}

/// Reject bundles whose file count exceeds the per-skill cap. Enforced on
/// every upload path (JSON, multipart directory, zip) so none can flood the
/// `skill_version_files` table with many tiny rows while staying under the
/// byte budget.
fn check_file_count(count: usize, max_files: usize) -> Result<(), ApiError> {
    if count > max_files {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_files",
            format!("skill bundle exceeds the maximum of {max_files} files"),
        ));
    }
    Ok(())
}

fn owner_from_parts(owner_type: &str, owner_id: Uuid) -> Result<SkillOwner, ApiError> {
    match owner_type {
        "organization" => Ok(SkillOwner::Organization {
            organization_id: owner_id,
        }),
        "team" => Ok(SkillOwner::Team { team_id: owner_id }),
        "project" => Ok(SkillOwner::Project {
            project_id: owner_id,
        }),
        "user" => Ok(SkillOwner::User { user_id: owner_id }),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_owner_type",
            "owner_type must be one of: organization, team, project, user",
        )),
    }
}

fn zip_err(e: skill_zip::SkillZipError) -> ApiError {
    use skill_zip::SkillZipError::*;
    let (status, code) = match e {
        TooLarge(_) | TooManyFiles(_) => (StatusCode::PAYLOAD_TOO_LARGE, "skill_too_large"),
        NotUtf8(_) => (StatusCode::BAD_REQUEST, "binary_not_supported"),
        InvalidPath(_) => (StatusCode::BAD_REQUEST, "invalid_path"),
        _ => (StatusCode::BAD_REQUEST, "invalid_skill_bundle"),
    };
    ApiError::new(status, code, e.to_string())
}

/// Parse the create-skill body from JSON or multipart.
async fn parse_create_body(
    state: &AppState,
    request: Request,
) -> Result<ParsedSkillBody, ApiError> {
    let limits = &state.config.limits.resource_limits;
    let max_bytes = limits.max_skill_bytes as u64;
    let max_files = 500;
    let ct = content_type_of(&request);
    if ct.starts_with("application/json") {
        let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .map_err(json_err)?;
        let body: CreateSkillBody = serde_json::from_slice(&bytes).map_err(json_err)?;
        check_file_count(body.files.len(), max_files)?;
        Ok(ParsedSkillBody {
            owner: body.owner,
            name: body.name,
            description: body.description,
            user_invocable: body.user_invocable,
            disable_model_invocation: body.disable_model_invocation,
            allowed_tools: body.allowed_tools,
            argument_hint: body.argument_hint,
            source_url: body.source_url,
            source_ref: body.source_ref,
            frontmatter_extra: body.frontmatter_extra,
            files: body.files,
            make_default: false,
        })
    } else if ct.starts_with("multipart/") {
        collect_multipart(state, request, max_bytes, max_files).await
    } else {
        Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
            "Content-Type must be 'application/json' or 'multipart/form-data'",
        ))
    }
}

/// Parse the create-version body from JSON or multipart.
async fn parse_version_body(
    state: &AppState,
    request: Request,
) -> Result<ParsedSkillBody, ApiError> {
    let limits = &state.config.limits.resource_limits;
    let max_bytes = limits.max_skill_bytes as u64;
    let max_files = 500;
    let ct = content_type_of(&request);
    if ct.starts_with("application/json") {
        let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .map_err(json_err)?;
        let body: CreateSkillVersionBody = serde_json::from_slice(&bytes).map_err(json_err)?;
        check_file_count(body.files.len(), max_files)?;
        Ok(ParsedSkillBody {
            description: body.description,
            user_invocable: body.user_invocable,
            disable_model_invocation: body.disable_model_invocation,
            allowed_tools: body.allowed_tools,
            argument_hint: body.argument_hint,
            source_url: body.source_url,
            source_ref: body.source_ref,
            frontmatter_extra: body.frontmatter_extra,
            files: body.files,
            make_default: body.default,
            ..Default::default()
        })
    } else if ct.starts_with("multipart/") {
        collect_multipart(state, request, max_bytes, max_files).await
    } else {
        Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
            "Content-Type must be 'application/json' or 'multipart/form-data'",
        ))
    }
}

fn parse_skill_id(raw: &str) -> Result<Uuid, ApiError> {
    raw.parse::<SkillId>()
        .map(|id| id.into_inner())
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_skill_id",
                format!("invalid skill id: {raw}"),
            )
        })
}

fn parse_version_seq(raw: &str) -> Result<i64, ApiError> {
    raw.parse::<i64>().ok().filter(|n| *n > 0).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_version",
            format!("invalid skill version: {raw}"),
        )
    })
}

fn zip_response(name: &str, version: &str, files: &[SkillFile]) -> Result<Response, ApiError> {
    let bytes = skill_zip::pack_files_to_zip(files).map_err(zip_err)?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}-{version}.zip\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

// ===========================================================================
// Handlers
// ===========================================================================

/// Create a new skill (and its first version).
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/skills",
    tag = "skills",
    operation_id = "skill_create",
    request_body = CreateSkillBody,
    responses(
        (status = 201, description = "Skill created", body = SkillResource),
        (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponse),
        (status = 409, description = "Skill name already exists or limit reached", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz, request))]
pub async fn api_v1_skills_create(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    request: Request,
) -> Result<(StatusCode, Json<SkillResource>), ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "create",
    )
    .await?;
    let services = get_services(&state)?;

    let mut parsed = parse_create_body(&state, request).await?;
    parsed.merge_frontmatter();

    let owner = parsed
        .owner
        .clone()
        .or_else(|| derive_skill_owner(auth.as_ref().map(|e| &e.0)))
        .or_else(|| {
            state
                .default_org_id
                .map(|organization_id| SkillOwner::Organization { organization_id })
        })
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "owner_required",
                "could not resolve an owner; provide `owner` or authenticate",
            )
        })?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(owner.owner_type()),
        owner.owner_id(),
    )?;

    let name = parsed.name.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_name",
            "skill name is required (provide it or include it in SKILL.md frontmatter)",
        )
    })?;
    let description = parsed
        .description
        .clone()
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "missing_description",
                "skill description is required (provide it or include it in SKILL.md frontmatter)",
            )
        })?;

    // Per-owner skill count limit.
    let max = state.config.limits.resource_limits.max_skills_per_owner;
    if max > 0 {
        let count = services
            .skills
            .count_skills_by_owner(owner.owner_type(), owner.owner_id(), false)
            .await?;
        if count >= max as i64 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "limit_reached",
                format!("Owner has reached the maximum number of skills ({max})"),
            ));
        }
    }

    let input = CreateSkill {
        owner: owner.clone(),
        name,
        description,
        files: parsed.files,
        user_invocable: parsed.user_invocable,
        disable_model_invocation: parsed.disable_model_invocation,
        allowed_tools: parsed.allowed_tools,
        argument_hint: parsed.argument_hint,
        source_url: parsed.source_url,
        source_ref: parsed.source_ref,
        frontmatter_extra: parsed.frontmatter_extra,
    };

    let skill = services.skills.create_skill(input).await?;
    audit_skill(
        services,
        auth.as_ref().map(|e| &e.0),
        "skill.create",
        skill.id,
        owner.owner_type(),
        owner.owner_id(),
        serde_json::json!({ "name": skill.name, "version": "1" }),
    )
    .await;

    Ok((StatusCode::CREATED, Json(skill_to_wire(skill))))
}

/// List skills for the current project (or a specific owner / accessible set).
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/skills",
    tag = "skills",
    operation_id = "skill_list",
    params(ListSkillsQuery),
    responses(
        (status = 200, description = "List of skills", body = SkillListResource),
        (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_list(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Query(query): Query<ListSkillsQuery>,
) -> Result<Json<SkillListResource>, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "list",
    )
    .await?;
    let services = get_services(&state)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);

    let (cursor, direction) = match (&query.after, &query.before) {
        (Some(after), _) => {
            let id = parse_skill_id(after)?;
            let rec = services.skills.get_skill(id).await?.ok_or_else(|| {
                ApiError::new(StatusCode::BAD_REQUEST, "invalid_cursor", "unknown cursor")
            })?;
            (
                Some(Cursor::new(rec.created_at, rec.id)),
                CursorDirection::Forward,
            )
        }
        (None, Some(before)) => {
            let id = parse_skill_id(before)?;
            let rec = services.skills.get_skill(id).await?.ok_or_else(|| {
                ApiError::new(StatusCode::BAD_REQUEST, "invalid_cursor", "unknown cursor")
            })?;
            (
                Some(Cursor::new(rec.created_at, rec.id)),
                CursorDirection::Backward,
            )
        }
        (None, None) => (None, CursorDirection::Forward),
    };

    let params = ListParams {
        limit: Some(limit),
        cursor,
        direction,
        sort_order: query.order.unwrap_or_default().into(),
        ..Default::default()
    };

    let result = match (query.owner_type.as_deref(), query.owner_id) {
        (Some(ot), Some(oid)) => {
            let owner_type: SkillOwnerType = ot.parse().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_owner_type",
                    "owner_type must be one of: organization, team, project, user",
                )
            })?;
            check_resource_access_optional(
                auth.as_ref().map(|e| &e.0),
                to_vs_owner_type(owner_type),
                oid,
            )?;
            services
                .skills
                .list_skills_by_owner(owner_type, oid, params)
                .await?
        }
        (None, None) => match auth.as_ref() {
            None => {
                // Open-access mode: scope to the default org if configured.
                match state.default_org_id {
                    Some(org) => services.skills.list_skills_by_org(org, params).await?,
                    None => crate::db::repos::ListResult::new(
                        vec![],
                        false,
                        crate::db::repos::PageCursors::default(),
                    ),
                }
            }
            Some(auth_ext) => {
                let (user_id, org_ids, team_ids, project_ids) =
                    extract_identity_memberships(Some(&auth_ext.0))?;
                services
                    .skills
                    .list_skills_accessible(user_id, &org_ids, &team_ids, &project_ids, params)
                    .await?
            }
        },
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_parameters",
                "Both owner_type and owner_id must be provided together, or both omitted",
            ));
        }
    };

    let first_id = result.items.first().map(|s| SkillId::new(s.id).to_string());
    let last_id = result.items.last().map(|s| SkillId::new(s.id).to_string());
    Ok(Json(SkillListResource {
        object: "list".to_string(),
        data: result.items.into_iter().map(skill_to_wire).collect(),
        first_id,
        last_id,
        has_more: result.has_more,
    }))
}

/// Get a skill by id.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/skills/{skill_id}",
    tag = "skills",
    operation_id = "skill_get",
    params(("skill_id" = String, Path, description = "Skill id")),
    responses(
        (status = 200, description = "Skill", body = SkillResource),
        (status = 404, description = "Skill not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_get(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillResource>, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "read",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;
    let skill = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(skill.owner_type),
        skill.owner_id,
    )?;
    Ok(Json(skill_to_wire(skill)))
}

/// Set the default version pointer for a skill.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/skills/{skill_id}",
    tag = "skills",
    operation_id = "skill_set_default_version",
    params(("skill_id" = String, Path, description = "Skill id")),
    request_body = SetDefaultSkillVersionBody,
    responses(
        (status = 200, description = "Skill", body = SkillResource),
        (status = 400, description = "Unknown version", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Skill not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz, body))]
pub async fn api_v1_skills_set_default(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(skill_id): Path<String>,
    Json(body): Json<SetDefaultSkillVersionBody>,
) -> Result<Json<SkillResource>, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "update",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;

    // Ownership check on the existing skill.
    let existing = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(existing.owner_type),
        existing.owner_id,
    )?;

    let seq = parse_version_seq(&body.default_version)?;
    let skill = services.skills.set_default_version(id, seq).await?;
    audit_skill(
        services,
        auth.as_ref().map(|e| &e.0),
        "skill.set_default_version",
        skill.id,
        skill.owner_type,
        skill.owner_id,
        serde_json::json!({ "default_version": body.default_version }),
    )
    .await;
    Ok(Json(skill_to_wire(skill)))
}

/// Delete a skill.
#[cfg_attr(feature = "utoipa", utoipa::path(
    delete,
    path = "/api/v1/skills/{skill_id}",
    tag = "skills",
    operation_id = "skill_delete",
    params(("skill_id" = String, Path, description = "Skill id")),
    responses(
        (status = 200, description = "Skill deleted", body = DeletedSkillResource),
        (status = 404, description = "Skill not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_delete(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(skill_id): Path<String>,
) -> Result<Json<DeletedSkillResource>, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "delete",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;

    let existing = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(existing.owner_type),
        existing.owner_id,
    )?;

    services.skills.delete_skill(id).await?;
    audit_skill(
        services,
        auth.as_ref().map(|e| &e.0),
        "skill.delete",
        id,
        existing.owner_type,
        existing.owner_id,
        serde_json::json!({ "name": existing.name }),
    )
    .await;

    Ok(Json(DeletedSkillResource {
        id: SkillId::new(id).to_string(),
        object: "skill.deleted".to_string(),
        deleted: true,
    }))
}

/// Download the default version of a skill as a zip bundle.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/skills/{skill_id}/content",
    tag = "skills",
    operation_id = "skill_get_content",
    params(("skill_id" = String, Path, description = "Skill id")),
    responses(
        (status = 200, description = "Skill zip bundle", content_type = "application/zip"),
        (status = 404, description = "Skill not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_get_content(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(skill_id): Path<String>,
) -> Result<Response, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "read",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;
    let skill = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(skill.owner_type),
        skill.owner_id,
    )?;
    zip_response(
        &skill.name,
        &skill.default_version_seq.to_string(),
        &skill.files,
    )
}

/// Create a new immutable version of a skill.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/skills/{skill_id}/versions",
    tag = "skills",
    operation_id = "skill_create_version",
    params(("skill_id" = String, Path, description = "Skill id")),
    request_body = CreateSkillVersionBody,
    responses(
        (status = 201, description = "Version created", body = SkillVersionResource),
        (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Skill not found", body = crate::openapi::ErrorResponse),
        (status = 409, description = "Version limit reached", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz, request))]
pub async fn api_v1_skills_create_version(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(skill_id): Path<String>,
    request: Request,
) -> Result<(StatusCode, Json<SkillVersionResource>), ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "update",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;

    // Ownership check on the existing skill.
    let existing = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(existing.owner_type),
        existing.owner_id,
    )?;

    let mut parsed = parse_version_body(&state, request).await?;
    parsed.merge_frontmatter();
    let description = parsed
        .description
        .clone()
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "missing_description",
                "version description is required (provide it or include it in SKILL.md frontmatter)",
            )
        })?;

    // Per-skill version limit.
    let max = state
        .config
        .limits
        .resource_limits
        .max_skill_versions_per_skill;
    if max > 0 {
        let count = services.skills.count_versions(id, false).await?;
        if count >= max as i64 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "limit_reached",
                format!("Skill has reached the maximum number of versions ({max})"),
            ));
        }
    }

    let input = CreateSkillVersion {
        files: parsed.files,
        description,
        user_invocable: parsed.user_invocable,
        disable_model_invocation: parsed.disable_model_invocation,
        allowed_tools: parsed.allowed_tools,
        argument_hint: parsed.argument_hint,
        source_url: parsed.source_url,
        source_ref: parsed.source_ref,
        frontmatter_extra: parsed.frontmatter_extra,
        make_default: parsed.make_default,
    };
    let version = services.skills.create_version(id, input).await?;
    audit_skill(
        services,
        auth.as_ref().map(|e| &e.0),
        "skill.create_version",
        id,
        existing.owner_type,
        existing.owner_id,
        serde_json::json!({ "version": version.version_seq.to_string(), "default": parsed.make_default }),
    )
    .await;

    Ok((StatusCode::CREATED, Json(version_to_wire(version))))
}

/// List a skill's versions.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/skills/{skill_id}/versions",
    tag = "skills",
    operation_id = "skill_list_versions",
    params(("skill_id" = String, Path, description = "Skill id"), ListSkillVersionsQuery),
    responses(
        (status = 200, description = "List of versions", body = SkillVersionListResource),
        (status = 404, description = "Skill not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_list_versions(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(skill_id): Path<String>,
    Query(query): Query<ListSkillVersionsQuery>,
) -> Result<Json<SkillVersionListResource>, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "read",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;

    let existing = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(existing.owner_type),
        existing.owner_id,
    )?;

    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let (cursor, direction) = match (&query.after, &query.before) {
        (Some(after), _) => {
            let vid = after.parse::<SkillVersionId>().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_cursor",
                    "invalid 'after' cursor",
                )
            })?;
            let rec = services
                .skills
                .get_version_by_id(vid.into_inner())
                .await?
                .filter(|r| r.skill_id == id)
                .ok_or_else(|| {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_cursor", "unknown cursor")
                })?;
            (
                Some(Cursor::new(rec.created_at, rec.id)),
                CursorDirection::Forward,
            )
        }
        (None, Some(before)) => {
            let vid = before.parse::<SkillVersionId>().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_cursor",
                    "invalid 'before' cursor",
                )
            })?;
            let rec = services
                .skills
                .get_version_by_id(vid.into_inner())
                .await?
                .filter(|r| r.skill_id == id)
                .ok_or_else(|| {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_cursor", "unknown cursor")
                })?;
            (
                Some(Cursor::new(rec.created_at, rec.id)),
                CursorDirection::Backward,
            )
        }
        (None, None) => (None, CursorDirection::Forward),
    };

    let params = ListParams {
        limit: Some(limit),
        cursor,
        direction,
        sort_order: query.order.unwrap_or_default().into(),
        ..Default::default()
    };
    let result = services.skills.list_versions(id, params).await?;
    let first_id = result
        .items
        .first()
        .map(|v| SkillVersionId::new(v.id).to_string());
    let last_id = result
        .items
        .last()
        .map(|v| SkillVersionId::new(v.id).to_string());
    Ok(Json(SkillVersionListResource {
        object: "list".to_string(),
        data: result.items.into_iter().map(version_to_wire).collect(),
        first_id,
        last_id,
        has_more: result.has_more,
    }))
}

/// Get a specific skill version.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/skills/{skill_id}/versions/{version}",
    tag = "skills",
    operation_id = "skill_get_version",
    params(
        ("skill_id" = String, Path, description = "Skill id"),
        ("version" = String, Path, description = "Version number"),
    ),
    responses(
        (status = 200, description = "Version", body = SkillVersionResource),
        (status = 404, description = "Not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_get_version(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path((skill_id, version)): Path<(String, String)>,
) -> Result<Json<SkillVersionResource>, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "read",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;

    let existing = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(existing.owner_type),
        existing.owner_id,
    )?;

    let seq = parse_version_seq(&version)?;
    let version =
        services.skills.get_version(id, seq).await?.ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "Version not found")
        })?;
    Ok(Json(version_to_wire(version)))
}

/// Delete a skill version.
#[cfg_attr(feature = "utoipa", utoipa::path(
    delete,
    path = "/api/v1/skills/{skill_id}/versions/{version}",
    tag = "skills",
    operation_id = "skill_delete_version",
    params(
        ("skill_id" = String, Path, description = "Skill id"),
        ("version" = String, Path, description = "Version number"),
    ),
    responses(
        (status = 200, description = "Version deleted", body = DeletedSkillVersionResource),
        (status = 404, description = "Not found", body = crate::openapi::ErrorResponse),
        (status = 409, description = "Cannot delete the default version", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_delete_version(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path((skill_id, version)): Path<(String, String)>,
) -> Result<Json<DeletedSkillVersionResource>, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "update",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;

    let existing = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(existing.owner_type),
        existing.owner_id,
    )?;

    let seq = parse_version_seq(&version)?;
    services.skills.delete_version(id, seq).await?;
    audit_skill(
        services,
        auth.as_ref().map(|e| &e.0),
        "skill.delete_version",
        id,
        existing.owner_type,
        existing.owner_id,
        serde_json::json!({ "version": version }),
    )
    .await;

    Ok(Json(DeletedSkillVersionResource {
        id: SkillId::new(id).to_string(),
        object: "skill.version.deleted".to_string(),
        deleted: true,
        version,
    }))
}

/// Download a specific skill version as a zip bundle.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/skills/{skill_id}/versions/{version}/content",
    tag = "skills",
    operation_id = "skill_get_version_content",
    params(
        ("skill_id" = String, Path, description = "Skill id"),
        ("version" = String, Path, description = "Version number"),
    ),
    responses(
        (status = 200, description = "Version zip bundle", content_type = "application/zip"),
        (status = 404, description = "Not found", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(skip(state, auth, authz))]
pub async fn api_v1_skills_get_version_content(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path((skill_id, version)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    enforce_authz(
        authz.as_ref().map(|e| &e.0),
        auth.as_ref().map(|e| &e.0),
        "read",
    )
    .await?;
    let services = get_services(&state)?;
    let id = parse_skill_id(&skill_id)?;

    let existing = services
        .skills
        .get_skill(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "Skill not found"))?;
    check_resource_access_optional(
        auth.as_ref().map(|e| &e.0),
        to_vs_owner_type(existing.owner_type),
        existing.owner_id,
    )?;

    let seq = parse_version_seq(&version)?;
    let v =
        services.skills.get_version(id, seq).await?.ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "Version not found")
        })?;
    zip_response(&v.name, &v.version_seq.to_string(), &v.files)
}
