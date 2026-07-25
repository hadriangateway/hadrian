//! Periodic refresh of the in-memory admin model-pricing overlay.
//!
//! Admin mutations refresh the overlay immediately on the replica that
//! handled the request (see `routes::admin::model_pricing`); this worker is
//! the convergence path for other replicas and for out-of-band DB changes
//! (restores, manual SQL). A refresh is one small SELECT over an
//! admin-curated table, so it runs on every replica at a fixed interval with
//! no leader election and no config knob.

use std::time::Duration;

use crate::{pricing::ModelPricingOverlay, services::ModelPricingService};

/// Fixed refresh interval. Mutation-driven refresh is the primary mechanism;
/// this only bounds cross-replica staleness of admin pricing changes.
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Starts the model-pricing overlay refresh worker as a background task.
///
/// Runs indefinitely until the task is cancelled. Errors are logged and the
/// existing overlay data is kept, mirroring `start_model_catalog_sync_worker`.
pub async fn start_model_pricing_overlay_refresh_worker(
    service: ModelPricingService,
    overlay: ModelPricingOverlay,
) {
    tracing::info!(
        interval_secs = REFRESH_INTERVAL.as_secs(),
        "Starting model pricing overlay refresh worker"
    );

    loop {
        tokio::time::sleep(REFRESH_INTERVAL).await;

        match service.refresh_overlay(&overlay).await {
            Ok(count) => {
                tracing::debug!(entry_count = count, "Model pricing overlay refreshed");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Model pricing overlay refresh failed, keeping existing data"
                );
            }
        }
    }
}
