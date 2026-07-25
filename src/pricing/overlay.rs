//! In-memory overlay of admin-managed model pricing rows.
//!
//! The hot cost path (`PricingConfig::calculate_cost_detailed*`) is fully
//! synchronous — it runs inside streaming usage loggers and response
//! rewriting with no async context — so DB lookups cannot happen there.
//! This registry mirrors the billing-relevant `model_pricing` table rows in
//! memory (same pattern as `catalog::ModelCatalogRegistry`): loaded at
//! startup, reloaded after every admin mutation, and periodically refreshed
//! by `jobs::start_model_pricing_overlay_refresh_worker` so multi-replica
//! deployments converge.
//!
//! Scoping: only global (`owner_type IS NULL`) and organization-scoped rows
//! participate in runtime billing. Team-, project-, and user-scoped rows are
//! CRUD-managed but skipped on load: the synchronous cost path only carries
//! the organization id, and applying a narrower scope without its id would
//! risk billing one tenant with another tenant's rates. An organization's
//! rows are never applied to another organization or to requests without
//! org attribution.

use std::{collections::HashMap, sync::Arc};

use uuid::Uuid;

use super::ModelPricing;
use crate::{
    compat::RwLock,
    models::{DbModelPricing, PricingOwner},
};

/// Map key: (org scope, provider, model). `None` org = global row.
type OverlayKey = (Option<Uuid>, String, String);

/// Thread-safe, shared registry of admin-managed pricing overrides.
///
/// Cloning is cheap and every clone shares the same underlying data, so the
/// same handle can live inside `PricingConfig` (for lookups) and be refreshed
/// from admin routes and background jobs.
#[derive(Clone, Default)]
pub struct ModelPricingOverlay {
    inner: Arc<RwLock<HashMap<OverlayKey, ModelPricing>>>,
}

impl ModelPricingOverlay {
    /// Create a new empty overlay.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the overlay contents from DB rows.
    ///
    /// Rows with team/project/user owners are skipped (see module docs).
    /// Returns the number of entries applied.
    pub fn load_from_rows(&self, rows: &[DbModelPricing]) -> usize {
        let mut data = HashMap::with_capacity(rows.len());
        let mut skipped = 0usize;
        for row in rows {
            let scope = match &row.owner {
                PricingOwner::Global => None,
                PricingOwner::Organization { org_id } => Some(*org_id),
                PricingOwner::Team { .. }
                | PricingOwner::Project { .. }
                | PricingOwner::User { .. } => {
                    skipped += 1;
                    continue;
                }
            };
            data.insert(
                (scope, row.provider.clone(), row.model.clone()),
                row.to_model_pricing(),
            );
        }
        if skipped > 0 {
            tracing::debug!(
                skipped,
                "Skipped team/project/user-scoped model pricing rows; only global and \
                 organization scopes apply to runtime billing"
            );
        }
        let count = data.len();
        *self.inner.write() = data;
        count
    }

    /// Look up admin pricing for a provider/model.
    ///
    /// Precedence: the caller's organization row (when `org_id` is known),
    /// then the global row. Passing `None` applies global rows only —
    /// org-scoped rows are never applied outside their organization.
    pub fn lookup(
        &self,
        provider: &str,
        model: &str,
        org_id: Option<Uuid>,
    ) -> Option<ModelPricing> {
        let inner = self.inner.read();
        if let Some(org) = org_id
            && let Some(pricing) = inner.get(&(Some(org), provider.to_string(), model.to_string()))
        {
            return Some(pricing.clone());
        }
        inner
            .get(&(None, provider.to_string(), model.to_string()))
            .cloned()
    }

    /// Number of entries currently loaded.
    pub fn entry_count(&self) -> usize {
        self.inner.read().len()
    }
}

impl std::fmt::Debug for ModelPricingOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelPricingOverlay")
            .field("entry_count", &self.entry_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PricingSource;

    fn row(owner: PricingOwner, provider: &str, model: &str, input: i64) -> DbModelPricing {
        DbModelPricing {
            id: Uuid::new_v4(),
            owner,
            provider: provider.to_string(),
            model: model.to_string(),
            input_per_1m_tokens: input,
            output_per_1m_tokens: input * 2,
            per_image: None,
            per_request: None,
            cached_input_per_1m_tokens: None,
            cache_write_per_1m_tokens: None,
            reasoning_per_1m_tokens: None,
            per_second: None,
            per_1m_characters: None,
            source: PricingSource::Manual,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_load_skips_unsupported_scopes() {
        let overlay = ModelPricingOverlay::new();
        let rows = vec![
            row(PricingOwner::Global, "openai", "gpt-4o", 100),
            row(
                PricingOwner::Organization {
                    org_id: Uuid::new_v4(),
                },
                "openai",
                "gpt-4o",
                200,
            ),
            row(
                PricingOwner::Team {
                    team_id: Uuid::new_v4(),
                },
                "openai",
                "gpt-4o",
                300,
            ),
            row(
                PricingOwner::Project {
                    project_id: Uuid::new_v4(),
                },
                "openai",
                "gpt-4o",
                400,
            ),
            row(
                PricingOwner::User {
                    user_id: Uuid::new_v4(),
                },
                "openai",
                "gpt-4o",
                500,
            ),
        ];

        // Only global + org rows are applied
        assert_eq!(overlay.load_from_rows(&rows), 2);
        assert_eq!(overlay.entry_count(), 2);
    }

    #[test]
    fn test_lookup_org_precedence_and_isolation() {
        let org_a = Uuid::new_v4();
        let org_b = Uuid::new_v4();
        let overlay = ModelPricingOverlay::new();
        overlay.load_from_rows(&[
            row(PricingOwner::Global, "openai", "gpt-4o", 100),
            row(
                PricingOwner::Organization { org_id: org_a },
                "openai",
                "gpt-4o",
                200,
            ),
        ]);

        // Org A gets its own row
        let a = overlay.lookup("openai", "gpt-4o", Some(org_a)).unwrap();
        assert_eq!(a.input_per_1m_tokens, 200);

        // Org B falls back to the global row, never org A's
        let b = overlay.lookup("openai", "gpt-4o", Some(org_b)).unwrap();
        assert_eq!(b.input_per_1m_tokens, 100);

        // No org context: global row only
        let none = overlay.lookup("openai", "gpt-4o", None).unwrap();
        assert_eq!(none.input_per_1m_tokens, 100);

        // Unknown model: no match
        assert!(overlay.lookup("openai", "unknown", Some(org_a)).is_none());
    }

    #[test]
    fn test_org_row_never_applies_without_org() {
        let org_a = Uuid::new_v4();
        let overlay = ModelPricingOverlay::new();
        overlay.load_from_rows(&[row(
            PricingOwner::Organization { org_id: org_a },
            "openai",
            "gpt-4o",
            200,
        )]);

        assert!(overlay.lookup("openai", "gpt-4o", None).is_none());
        assert!(
            overlay
                .lookup("openai", "gpt-4o", Some(Uuid::new_v4()))
                .is_none()
        );
        assert!(overlay.lookup("openai", "gpt-4o", Some(org_a)).is_some());
    }

    #[test]
    fn test_load_replaces_existing_data() {
        let overlay = ModelPricingOverlay::new();
        overlay.load_from_rows(&[row(PricingOwner::Global, "openai", "gpt-4o", 100)]);
        assert_eq!(overlay.entry_count(), 1);

        // Reload with an empty set clears everything (e.g. all rows deleted)
        overlay.load_from_rows(&[]);
        assert_eq!(overlay.entry_count(), 0);
        assert!(overlay.lookup("openai", "gpt-4o", None).is_none());
    }
}
