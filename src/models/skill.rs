//! Agent Skills, modeled on the OpenAI Skills API (immutable, versioned) with
//! a Hadrian ownership extension.
//!
//! A skill is a packaged set of instructions (SKILL.md) plus optional bundled
//! files (scripts, references, assets). The OpenAI model is immutable and
//! versioned: a skill carries a `default_version`/`latest_version` pointer and
//! the only way to change content is to publish a new version. Hadrian extends
//! the spec so every skill is owned by an organization, team, project, or user
//! — matching the ownership model used by prompt templates.
//!
//! Storage: a `skills` row (identity + version pointers) → many immutable
//! `skill_versions` → each with its own `skill_version_files`. The [`Skill`]
//! struct here is a *projection* that surfaces the default version's
//! metadata/files alongside the skill's pointers.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::{Validate, ValidationError};

/// The filename of the required main instructions file in every skill version.
pub const SKILL_MAIN_FILE: &str = "SKILL.md";

/// Owner type for skills (organization, team, project, or user).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SkillOwnerType {
    Organization,
    Team,
    Project,
    User,
}

impl SkillOwnerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillOwnerType::Organization => "organization",
            SkillOwnerType::Team => "team",
            SkillOwnerType::Project => "project",
            SkillOwnerType::User => "user",
        }
    }
}

impl std::str::FromStr for SkillOwnerType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "organization" => Ok(SkillOwnerType::Organization),
            "team" => Ok(SkillOwnerType::Team),
            "project" => Ok(SkillOwnerType::Project),
            "user" => Ok(SkillOwnerType::User),
            _ => Err(format!("Invalid skill owner type: {}", s)),
        }
    }
}

/// Validate skill `name` per https://agentskills.io/specification.md:
/// 1..=64 chars, lowercase ASCII alphanumeric or hyphen, no leading or
/// trailing hyphen, no consecutive hyphens.
pub fn validate_skill_name(name: &str) -> Result<(), ValidationError> {
    if !(1..=64).contains(&name.len()) {
        return Err(ValidationError::new("skill_name_length"));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(ValidationError::new("skill_name_hyphen_boundary"));
    }
    if name.contains("--") {
        return Err(ValidationError::new("skill_name_consecutive_hyphens"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ValidationError::new("skill_name_charset"));
    }
    Ok(())
}

/// Validate a relative skill-file path. No absolute paths, no `..` segments,
/// no empty segments, 1..=255 bytes.
pub fn validate_skill_path(path: &str) -> Result<(), ValidationError> {
    if path.is_empty() || path.len() > 255 {
        return Err(ValidationError::new("skill_path_length"));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(ValidationError::new("skill_path_absolute"));
    }
    for seg in path.split(['/', '\\']) {
        if seg.is_empty() || seg == ".." || seg == "." {
            return Err(ValidationError::new("skill_path_traversal"));
        }
    }
    Ok(())
}

/// A file bundled with a skill version. Returned in full detail by get-by-id;
/// list endpoints populate [`SkillFileManifest`] instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SkillFile {
    /// Relative path inside the skill, e.g. "SKILL.md" or "scripts/extract.py".
    pub path: String,
    /// File contents. Text-only in v1 (binary assets unsupported).
    pub content: String,
    /// Byte length of `content`.
    pub byte_size: i64,
    /// MIME type, e.g. "text/markdown".
    pub content_type: String,
    pub created_at: DateTime<Utc>,
}

/// Lightweight file entry returned by list endpoints — contents omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SkillFileManifest {
    pub path: String,
    pub byte_size: i64,
    pub content_type: String,
}

/// A skill: identity (owner + per-owner unique slug) plus version pointers,
/// projected with the **default version's** metadata and files. This is the
/// internal domain type; the `/v1` route layer maps it to an OpenAI-shaped
/// wire response.
#[derive(Debug, Clone)]
pub struct Skill {
    pub id: Uuid,
    pub owner_type: SkillOwnerType,
    pub owner_id: Uuid,
    /// Skill name (unique per owner). See [`validate_skill_name`].
    pub name: String,
    /// Default version sequence number (the public `default_version` string).
    pub default_version_seq: i64,
    /// Latest live version sequence number (the public `latest_version` string).
    pub latest_version_seq: i64,

    // ---- Projected from the default version ----
    /// Human-readable description. Used by the model to decide when to invoke.
    pub description: String,
    pub user_invocable: Option<bool>,
    pub disable_model_invocation: Option<bool>,
    pub allowed_tools: Option<Vec<String>>,
    pub argument_hint: Option<String>,
    pub source_url: Option<String>,
    pub source_ref: Option<String>,
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
    /// Default version's total file size (bytes).
    pub total_bytes: i64,
    /// Default version's full file contents. Populated by get-by-id endpoints.
    pub files: Vec<SkillFile>,
    /// Default version's file summary (no contents). Populated by list endpoints.
    pub files_manifest: Vec<SkillFileManifest>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An immutable version of a skill.
#[derive(Debug, Clone)]
pub struct SkillVersion {
    pub id: Uuid,
    pub skill_id: Uuid,
    /// Version sequence number (the public `version` string, e.g. "1").
    pub version_seq: i64,
    /// Snapshot of the skill slug at creation time.
    pub name: String,
    pub description: String,
    pub user_invocable: Option<bool>,
    pub disable_model_invocation: Option<bool>,
    pub allowed_tools: Option<Vec<String>>,
    pub argument_hint: Option<String>,
    pub source_url: Option<String>,
    pub source_ref: Option<String>,
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
    pub total_bytes: i64,
    /// Full file contents. Populated by get-version endpoints.
    pub files: Vec<SkillFile>,
    /// File summary (no contents). Populated by list-versions endpoints.
    pub files_manifest: Vec<SkillFileManifest>,
    pub created_at: DateTime<Utc>,
}

/// Owner specification for creating a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillOwner {
    Organization { organization_id: Uuid },
    Team { team_id: Uuid },
    Project { project_id: Uuid },
    User { user_id: Uuid },
}

impl SkillOwner {
    pub fn owner_type(&self) -> SkillOwnerType {
        match self {
            SkillOwner::Organization { .. } => SkillOwnerType::Organization,
            SkillOwner::Team { .. } => SkillOwnerType::Team,
            SkillOwner::Project { .. } => SkillOwnerType::Project,
            SkillOwner::User { .. } => SkillOwnerType::User,
        }
    }

    pub fn owner_id(&self) -> Uuid {
        match self {
            SkillOwner::Organization { organization_id } => *organization_id,
            SkillOwner::Team { team_id } => *team_id,
            SkillOwner::Project { project_id } => *project_id,
            SkillOwner::User { user_id } => *user_id,
        }
    }
}

/// A single file in a create/version request.
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct SkillFileInput {
    #[validate(custom(function = "validate_skill_path"))]
    pub path: String,
    #[validate(length(min = 1))]
    pub content: String,
    /// Optional MIME type. If omitted, the service sniffs it from the path
    /// extension.
    #[validate(length(max = 127))]
    pub content_type: Option<String>,
}

/// Service-layer input to create a new skill (and its first version). The
/// `/v1` route layer normalizes the JSON / multipart / zip request body into
/// this fully-resolved form (owner derived from the API key when omitted;
/// name/description sniffed from SKILL.md frontmatter when not supplied).
/// `files` must contain exactly one entry with `path == "SKILL.md"`.
#[derive(Debug, Clone, Validate)]
pub struct CreateSkill {
    pub owner: SkillOwner,
    #[validate(custom(function = "validate_skill_name"))]
    pub name: String,
    #[validate(length(min = 1, max = 1024))]
    pub description: String,
    #[validate(length(min = 1), nested)]
    pub files: Vec<SkillFileInput>,

    pub user_invocable: Option<bool>,
    pub disable_model_invocation: Option<bool>,
    pub allowed_tools: Option<Vec<String>>,
    #[validate(length(max = 255))]
    pub argument_hint: Option<String>,
    #[validate(length(max = 2048))]
    pub source_url: Option<String>,
    #[validate(length(max = 255))]
    pub source_ref: Option<String>,
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,
}

/// Service-layer input to publish a new immutable version of an existing skill.
/// The version's `name` is taken from the skill slug (immutable). `files` must
/// contain exactly one entry with `path == "SKILL.md"`.
#[derive(Debug, Clone, Validate)]
pub struct CreateSkillVersion {
    #[validate(length(min = 1), nested)]
    pub files: Vec<SkillFileInput>,
    #[validate(length(min = 1, max = 1024))]
    pub description: String,

    pub user_invocable: Option<bool>,
    pub disable_model_invocation: Option<bool>,
    pub allowed_tools: Option<Vec<String>>,
    #[validate(length(max = 255))]
    pub argument_hint: Option<String>,
    #[validate(length(max = 2048))]
    pub source_url: Option<String>,
    #[validate(length(max = 255))]
    pub source_ref: Option<String>,
    pub frontmatter_extra: Option<HashMap<String, serde_json::Value>>,

    /// Whether this version becomes the skill's default.
    pub make_default: bool,
}

/// How a `skill_reference` addresses a skill: by id (prefixed `skill_…` or bare
/// UUID) or by its name slug (e.g. `openai-spreadsheets`).
#[derive(Debug, Clone)]
pub enum SkillRef {
    Id(Uuid),
    Name(String),
}

/// Which version of a skill to resolve for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSelector {
    /// The skill's default version (omitted `version`).
    Default,
    /// The newest version (`version = "latest"`).
    Latest,
    /// A specific version sequence number.
    Exact(i64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_name_accepts_valid_examples() {
        for name in [
            "pdf-processing",
            "data-analysis",
            "code-review",
            "a",
            "abc123",
        ] {
            assert!(
                validate_skill_name(name).is_ok(),
                "expected {name:?} to be valid"
            );
        }
    }

    #[test]
    fn skill_name_rejects_bad_examples() {
        for name in [
            "",
            "PDF-Processing",
            "-pdf",
            "pdf-",
            "pdf--processing",
            "pdf_processing",
            "pdf processing",
            &"x".repeat(65),
        ] {
            assert!(
                validate_skill_name(name).is_err(),
                "expected {name:?} to be invalid"
            );
        }
    }

    #[test]
    fn skill_path_accepts_valid_examples() {
        for path in [
            "SKILL.md",
            "scripts/extract.py",
            "references/REFERENCE.md",
            "assets/template.txt",
            "a/b/c/d.txt",
        ] {
            assert!(
                validate_skill_path(path).is_ok(),
                "expected {path:?} to be valid"
            );
        }
    }

    #[test]
    fn skill_path_rejects_bad_examples() {
        for path in [
            "",
            "/absolute/path.md",
            "\\windows\\style.md",
            "../escape.md",
            "ok/../escape.md",
            "./SKILL.md",
            "scripts/./helper.py",
            "double//slash.md",
            &"x".repeat(256),
        ] {
            assert!(
                validate_skill_path(path).is_err(),
                "expected {path:?} to be invalid"
            );
        }
    }

    #[test]
    fn skill_owner_type_roundtrips() {
        for ot in [
            SkillOwnerType::Organization,
            SkillOwnerType::Team,
            SkillOwnerType::Project,
            SkillOwnerType::User,
        ] {
            assert_eq!(ot.as_str().parse::<SkillOwnerType>().unwrap(), ot);
        }
    }

    #[test]
    fn skill_owner_extracts_type_and_id() {
        let org = Uuid::new_v4();
        let owner = SkillOwner::Organization {
            organization_id: org,
        };
        assert_eq!(owner.owner_type(), SkillOwnerType::Organization);
        assert_eq!(owner.owner_id(), org);
    }
}
