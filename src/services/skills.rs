use std::{collections::HashSet, sync::Arc};

use uuid::Uuid;
use validator::Validate;

use crate::{
    db::{DbError, DbPool, DbResult, ListParams, repos::ListResult},
    models::{
        CreateSkill, CreateSkillVersion, SKILL_MAIN_FILE, Skill, SkillFileInput, SkillOwnerType,
        SkillRef, SkillVersion, VersionSelector,
    },
};

/// Service layer for skill operations. Enforces spec invariants on top of the
/// raw repo:
/// - Exactly one file must have path == "SKILL.md".
/// - No duplicate paths within a skill version.
/// - Total file size must not exceed the configured `max_skill_bytes` limit.
///
/// Skills are immutable and versioned: content changes are published as new
/// versions; `set_default_version` repoints which version is served by default.
#[derive(Clone)]
pub struct SkillService {
    db: Arc<DbPool>,
    max_skill_bytes: u32,
}

impl SkillService {
    pub fn new(db: Arc<DbPool>, max_skill_bytes: u32) -> Self {
        Self {
            db,
            max_skill_bytes,
        }
    }

    fn validate_files(&self, files: &[SkillFileInput]) -> DbResult<()> {
        if files.is_empty() {
            return Err(DbError::Validation(
                "Skill must contain at least one file".into(),
            ));
        }

        let main_count = files.iter().filter(|f| f.path == SKILL_MAIN_FILE).count();
        if main_count == 0 {
            return Err(DbError::Validation(format!(
                "Skill must contain a `{}` file",
                SKILL_MAIN_FILE
            )));
        }
        if main_count > 1 {
            return Err(DbError::Validation(format!(
                "Skill must not contain more than one `{}` file",
                SKILL_MAIN_FILE
            )));
        }

        let mut seen: HashSet<&str> = HashSet::with_capacity(files.len());
        for file in files {
            if !seen.insert(file.path.as_str()) {
                return Err(DbError::Validation(format!(
                    "Duplicate file path in skill: {}",
                    file.path
                )));
            }
        }

        // Byte-size limit is configured at runtime; skip the check when set to 0
        // (meaning "unlimited", matching the convention used by other resource
        // limits in ResourceLimits).
        if self.max_skill_bytes > 0 {
            let total: u64 = files.iter().map(|f| f.content.len() as u64).sum();
            if total > self.max_skill_bytes as u64 {
                return Err(DbError::Validation(format!(
                    "Skill files total {} bytes, exceeding the configured limit of {} bytes",
                    total, self.max_skill_bytes
                )));
            }
        }

        Ok(())
    }

    // ====================================================================
    // Skill lifecycle
    // ====================================================================

    /// Create a new skill (and its first version) after enforcing invariants.
    pub async fn create_skill(&self, input: CreateSkill) -> DbResult<Skill> {
        input
            .validate()
            .map_err(|e| DbError::Validation(e.to_string()))?;
        self.validate_files(&input.files)?;
        self.db.skills().create_skill(input).await
    }

    pub async fn get_skill(&self, id: Uuid) -> DbResult<Option<Skill>> {
        self.db.skills().get_skill(id).await
    }

    pub async fn get_skill_and_org(&self, id: Uuid, org_id: Uuid) -> DbResult<Option<Skill>> {
        self.db.skills().get_skill_and_org(id, org_id).await
    }

    pub async fn list_skills_by_owner(
        &self,
        owner_type: SkillOwnerType,
        owner_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<Skill>> {
        self.db
            .skills()
            .list_skills_by_owner(owner_type, owner_id, params)
            .await
    }

    pub async fn list_skills_by_org(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<Skill>> {
        self.db.skills().list_skills_by_org(org_id, params).await
    }

    pub async fn list_skills_accessible(
        &self,
        user_id: Option<Uuid>,
        org_ids: &[Uuid],
        team_ids: &[Uuid],
        project_ids: &[Uuid],
        params: ListParams,
    ) -> DbResult<ListResult<Skill>> {
        self.db
            .skills()
            .list_skills_accessible(user_id, org_ids, team_ids, project_ids, params)
            .await
    }

    pub async fn count_skills_by_owner(
        &self,
        owner_type: SkillOwnerType,
        owner_id: Uuid,
        include_deleted: bool,
    ) -> DbResult<i64> {
        self.db
            .skills()
            .count_skills_by_owner(owner_type, owner_id, include_deleted)
            .await
    }

    /// Repoint the skill's default version (parsed from the `default_version`
    /// string by the caller).
    pub async fn set_default_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<Skill> {
        self.db
            .skills()
            .set_default_version(skill_id, version_seq)
            .await
    }

    pub async fn delete_skill(&self, id: Uuid) -> DbResult<()> {
        self.db.skills().delete_skill(id).await
    }

    // ====================================================================
    // Version lifecycle
    // ====================================================================

    /// Publish a new immutable version after enforcing file invariants.
    pub async fn create_version(
        &self,
        skill_id: Uuid,
        input: CreateSkillVersion,
    ) -> DbResult<SkillVersion> {
        input
            .validate()
            .map_err(|e| DbError::Validation(e.to_string()))?;
        self.validate_files(&input.files)?;
        self.db.skills().create_version(skill_id, input).await
    }

    pub async fn get_version(
        &self,
        skill_id: Uuid,
        version_seq: i64,
    ) -> DbResult<Option<SkillVersion>> {
        self.db.skills().get_version(skill_id, version_seq).await
    }

    pub async fn get_version_by_id(&self, version_id: Uuid) -> DbResult<Option<SkillVersion>> {
        self.db.skills().get_version_by_id(version_id).await
    }

    pub async fn list_versions(
        &self,
        skill_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<SkillVersion>> {
        self.db.skills().list_versions(skill_id, params).await
    }

    pub async fn count_versions(&self, skill_id: Uuid, include_deleted: bool) -> DbResult<i64> {
        self.db
            .skills()
            .count_versions(skill_id, include_deleted)
            .await
    }

    pub async fn delete_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<()> {
        self.db.skills().delete_version(skill_id, version_seq).await
    }

    // ====================================================================
    // Runtime resolution
    // ====================================================================

    /// Resolve a `skill_reference` (by id or name) + version selector to a
    /// concrete version with files, scoped to the caller's organization.
    pub async fn resolve_version_for_reference(
        &self,
        skill_ref: SkillRef,
        version: VersionSelector,
        org_id: Uuid,
    ) -> DbResult<Option<SkillVersion>> {
        self.db
            .skills()
            .resolve_version_for_reference(skill_ref, version, org_id)
            .await
    }
}
