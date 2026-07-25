use std::sync::Arc;

use uuid::Uuid;

use crate::{
    db::{
        DbPool, DbResult,
        repos::{ListParams, ListResult},
    },
    models::{CreateModelPricing, DbModelPricing, PricingOwner, UpdateModelPricing},
};

/// Service layer for model pricing operations
#[derive(Clone)]
pub struct ModelPricingService {
    db: Arc<DbPool>,
}

impl ModelPricingService {
    pub fn new(db: Arc<DbPool>) -> Self {
        Self { db }
    }

    /// Create a new model pricing entry
    pub async fn create(&self, input: CreateModelPricing) -> DbResult<DbModelPricing> {
        self.db.model_pricing().create(input).await
    }

    /// Get pricing by ID
    pub async fn get_by_id(&self, id: Uuid) -> DbResult<Option<DbModelPricing>> {
        self.db.model_pricing().get_by_id(id).await
    }

    /// Get pricing by provider and model for a specific owner
    pub async fn get_by_provider_model(
        &self,
        owner: &PricingOwner,
        provider: &str,
        model: &str,
    ) -> DbResult<Option<DbModelPricing>> {
        self.db
            .model_pricing()
            .get_by_provider_model(owner, provider, model)
            .await
    }

    /// Get effective pricing for a provider/model with hierarchical lookup
    /// Searches: user → project → org → global
    pub async fn get_effective_pricing(
        &self,
        provider: &str,
        model: &str,
        user_id: Option<Uuid>,
        project_id: Option<Uuid>,
        org_id: Option<Uuid>,
    ) -> DbResult<Option<DbModelPricing>> {
        self.db
            .model_pricing()
            .get_effective_pricing(provider, model, user_id, project_id, org_id)
            .await
    }

    /// List pricing for an organization
    pub async fn list_by_org(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        self.db.model_pricing().list_by_org(org_id, params).await
    }

    /// Count pricing for an organization
    pub async fn count_by_org(&self, org_id: Uuid) -> DbResult<i64> {
        self.db.model_pricing().count_by_org(org_id).await
    }

    /// List pricing for a project
    pub async fn list_by_project(
        &self,
        project_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        self.db
            .model_pricing()
            .list_by_project(project_id, params)
            .await
    }

    /// Count pricing for a project
    pub async fn count_by_project(&self, project_id: Uuid) -> DbResult<i64> {
        self.db.model_pricing().count_by_project(project_id).await
    }

    /// List pricing for a user
    pub async fn list_by_user(
        &self,
        user_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        self.db.model_pricing().list_by_user(user_id, params).await
    }

    /// Count pricing for a user
    pub async fn count_by_user(&self, user_id: Uuid) -> DbResult<i64> {
        self.db.model_pricing().count_by_user(user_id).await
    }

    /// List global pricing
    pub async fn list_global(&self, params: ListParams) -> DbResult<ListResult<DbModelPricing>> {
        self.db.model_pricing().list_global(params).await
    }

    /// Count global pricing
    pub async fn count_global(&self) -> DbResult<i64> {
        self.db.model_pricing().count_global().await
    }

    /// List pricing by provider
    pub async fn list_by_provider(
        &self,
        provider: &str,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        self.db
            .model_pricing()
            .list_by_provider(provider, params)
            .await
    }

    /// Count pricing by provider
    pub async fn count_by_provider(&self, provider: &str) -> DbResult<i64> {
        self.db.model_pricing().count_by_provider(provider).await
    }

    /// Update pricing
    pub async fn update(&self, id: Uuid, input: UpdateModelPricing) -> DbResult<DbModelPricing> {
        self.db.model_pricing().update(id, input).await
    }

    /// Delete pricing
    pub async fn delete(&self, id: Uuid) -> DbResult<()> {
        self.db.model_pricing().delete(id).await
    }

    /// Upsert pricing (create or update)
    pub async fn upsert(&self, input: CreateModelPricing) -> DbResult<DbModelPricing> {
        self.db.model_pricing().upsert(input).await
    }

    /// Bulk upsert pricing entries (e.g., for OpenRouter API sync)
    pub async fn bulk_upsert(&self, entries: Vec<CreateModelPricing>) -> DbResult<usize> {
        self.db.model_pricing().bulk_upsert(entries).await
    }

    /// Reload the in-memory billing overlay from the DB (global and
    /// organization-scoped rows only; see `pricing::ModelPricingOverlay`).
    /// Returns the number of entries applied.
    pub async fn refresh_overlay(
        &self,
        overlay: &crate::pricing::ModelPricingOverlay,
    ) -> DbResult<usize> {
        let rows = self.db.model_pricing().list_billing_overlay_rows().await?;
        Ok(overlay.load_from_rows(&rows))
    }
}
