use async_trait::async_trait;
use uuid::Uuid;

use super::{ListParams, ListResult};
use crate::{
    db::error::DbResult,
    models::{CreateModelPricing, DbModelPricing, PricingOwner, UpdateModelPricing},
};

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ModelPricingRepo: Send + Sync {
    /// Create a new model pricing entry
    async fn create(&self, input: CreateModelPricing) -> DbResult<DbModelPricing>;

    /// Get pricing by ID
    async fn get_by_id(&self, id: Uuid) -> DbResult<Option<DbModelPricing>>;

    /// Get pricing for a specific provider/model within an owner scope
    async fn get_by_provider_model(
        &self,
        owner: &PricingOwner,
        provider: &str,
        model: &str,
    ) -> DbResult<Option<DbModelPricing>>;

    /// Get pricing for a provider/model, searching up the hierarchy:
    /// user -> project -> organization -> global
    /// Returns the most specific pricing found
    async fn get_effective_pricing(
        &self,
        provider: &str,
        model: &str,
        user_id: Option<Uuid>,
        project_id: Option<Uuid>,
        org_id: Option<Uuid>,
    ) -> DbResult<Option<DbModelPricing>>;

    /// List all pricing for an organization
    async fn list_by_org(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>>;

    /// Count all pricing for an organization
    async fn count_by_org(&self, org_id: Uuid) -> DbResult<i64>;

    /// List all pricing for a project
    async fn list_by_project(
        &self,
        project_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>>;

    /// Count all pricing for a project
    async fn count_by_project(&self, project_id: Uuid) -> DbResult<i64>;

    /// List all pricing for a user
    async fn list_by_user(
        &self,
        user_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>>;

    /// Count all pricing for a user
    async fn count_by_user(&self, user_id: Uuid) -> DbResult<i64>;

    /// List all global pricing
    async fn list_global(&self, params: ListParams) -> DbResult<ListResult<DbModelPricing>>;

    /// Count all global pricing
    async fn count_global(&self) -> DbResult<i64>;

    /// List all pricing for a specific provider (across all scopes)
    async fn list_by_provider(
        &self,
        provider: &str,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>>;

    /// Count all pricing for a specific provider (across all scopes)
    async fn count_by_provider(&self, provider: &str) -> DbResult<i64>;

    /// Update a pricing entry
    async fn update(&self, id: Uuid, input: UpdateModelPricing) -> DbResult<DbModelPricing>;

    /// Delete a pricing entry
    async fn delete(&self, id: Uuid) -> DbResult<()>;

    /// List every global and organization-scoped pricing row for the
    /// in-memory billing overlay (`pricing::ModelPricingOverlay`).
    /// Team/project/user rows are excluded — the runtime cost path only
    /// scopes by organization. Unpaginated: the table is admin-curated
    /// and small.
    async fn list_billing_overlay_rows(&self) -> DbResult<Vec<DbModelPricing>>;

    /// Upsert pricing (create or update based on owner/provider/model)
    async fn upsert(&self, input: CreateModelPricing) -> DbResult<DbModelPricing>;

    /// Bulk upsert pricing entries for a provider (e.g., from OpenRouter API)
    async fn bulk_upsert(&self, entries: Vec<CreateModelPricing>) -> DbResult<usize>;
}
