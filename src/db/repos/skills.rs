use async_trait::async_trait;
use uuid::Uuid;

use super::{ListParams, ListResult};
use crate::{
    db::error::DbResult,
    models::{
        CreateSkill, CreateSkillVersion, Skill, SkillOwnerType, SkillRef, SkillVersion,
        VersionSelector,
    },
};

/// Sniff a MIME type from a skill file path's extension. Shared by the
/// sqlite and postgres repos so the mapping never drifts. Falls back to
/// `text/plain` for unknown extensions.
pub fn sniff_skill_content_type(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    match lower.rsplit_once('.').map(|(_, ext)| ext) {
        Some("md") | Some("markdown") => "text/markdown",
        Some("py") => "text/x-python",
        Some("js") | Some("mjs") | Some("cjs") => "text/javascript",
        Some("ts") => "text/typescript",
        Some("sh") | Some("bash") => "text/x-shellscript",
        Some("json") => "application/json",
        Some("yaml") | Some("yml") => "application/yaml",
        Some("toml") => "application/toml",
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("csv") => "text/csv",
        Some("txt") | None => "text/plain",
        _ => "text/plain",
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SkillRepo: Send + Sync {
    // ====================================================================
    // Skill lifecycle
    // ====================================================================

    /// Create a new skill and its first immutable version (`version_seq = 1`).
    /// `input.files` is stored verbatim — callers (service layer) must have
    /// already enforced the spec invariants (SKILL.md present, paths valid,
    /// total-size limit).
    async fn create_skill(&self, input: CreateSkill) -> DbResult<Skill>;

    /// Get a skill projected with its default version's metadata and files.
    async fn get_skill(&self, id: Uuid) -> DbResult<Option<Skill>>;

    /// Get a skill by ID, scoped to a specific organization. Verifies the skill
    /// is reachable within the org via its owner relationship.
    async fn get_skill_and_org(&self, id: Uuid, org_id: Uuid) -> DbResult<Option<Skill>>;

    /// List skills by owner. Results populate the default version's
    /// `files_manifest` (not `files`).
    async fn list_skills_by_owner(
        &self,
        owner_type: SkillOwnerType,
        owner_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<Skill>>;

    /// List all skills reachable within an organization (org/team/project/user
    /// scopes that belong to the org).
    async fn list_skills_by_org(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<Skill>>;

    /// List skills accessible to a principal based on their memberships.
    async fn list_skills_accessible(
        &self,
        user_id: Option<Uuid>,
        org_ids: &[Uuid],
        team_ids: &[Uuid],
        project_ids: &[Uuid],
        params: ListParams,
    ) -> DbResult<ListResult<Skill>>;

    /// Count skills by owner.
    async fn count_skills_by_owner(
        &self,
        owner_type: SkillOwnerType,
        owner_id: Uuid,
        include_deleted: bool,
    ) -> DbResult<i64>;

    /// Repoint the skill's default version. Errors if the target version does
    /// not exist or is deleted.
    async fn set_default_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<Skill>;

    /// Soft-delete a skill (and, by cascade semantics, hide its versions).
    async fn delete_skill(&self, id: Uuid) -> DbResult<()>;

    // ====================================================================
    // Version lifecycle
    // ====================================================================

    /// Publish a new immutable version of a skill. Advances `latest_version`
    /// (and `default_version` when `input.make_default`) atomically.
    async fn create_version(
        &self,
        skill_id: Uuid,
        input: CreateSkillVersion,
    ) -> DbResult<SkillVersion>;

    /// Get a specific version (by sequence number) with its files.
    async fn get_version(&self, skill_id: Uuid, version_seq: i64)
    -> DbResult<Option<SkillVersion>>;

    /// Get a version by its own ID (no files). Used to resolve list cursors.
    async fn get_version_by_id(&self, version_id: Uuid) -> DbResult<Option<SkillVersion>>;

    /// List a skill's versions (newest first by default). Populates each
    /// version's `files_manifest`.
    async fn list_versions(
        &self,
        skill_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<SkillVersion>>;

    /// Count a skill's live versions.
    async fn count_versions(&self, skill_id: Uuid, include_deleted: bool) -> DbResult<i64>;

    /// Soft-delete a version. Errors if it is the default version; recomputes
    /// `latest_version` when the deleted version was the latest.
    async fn delete_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<()>;

    // ====================================================================
    // Runtime resolution
    // ====================================================================

    /// Resolve a `skill_reference` (by id or name slug) + version selector to a
    /// concrete version (with files), scoped to the caller's organization.
    async fn resolve_version_for_reference(
        &self,
        skill_ref: SkillRef,
        version: VersionSelector,
        org_id: Uuid,
    ) -> DbResult<Option<SkillVersion>>;
}
