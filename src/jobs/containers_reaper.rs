//! Idle-container reaper for the shell-tool `/mnt/data` workspace.
//!
//! Every pass under the cluster-wide leader lock:
//! 1. Mark `containers` rows whose `last_active_at + idle_ttl_secs`
//!    has elapsed as `expired`.
//! 2. Evict the corresponding entries from the in-memory
//!    [`ContainerSessionRegistry`] so their `ContainerSession::drop`
//!    detaches a terminate task and the underlying VM is torn down.
//!
//! On non-leader replicas the registry eviction step still runs — each
//! replica only knows about its own sessions, so per-replica eviction
//! is required to actually free VMs even when a different replica
//! flipped the DB row.

use std::{sync::Arc, time::Duration as StdDuration};

use chrono::Utc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::{
    jobs::leader_lock::{self, LeadershipOutcome, keys},
    services::{container_session::ContainerSessionRegistry, containers::ContainersService},
};

/// Run the reaper loop until `shutdown` fires.
pub async fn start_containers_reaper_worker(
    containers: Arc<ContainersService>,
    registry: Arc<ContainerSessionRegistry>,
    db: Arc<crate::db::DbPool>,
    interval: StdDuration,
    shutdown: CancellationToken,
) {
    tracing::info!(
        interval_secs = interval.as_secs(),
        "Starting containers reaper worker"
    );

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("Containers reaper worker received shutdown signal");
                return;
            }
            _ = sleep(interval) => {}
        }

        // Only the leader flips DB rows; other replicas still need to
        // sweep their local registry of in-memory sessions for any
        // containers a previous leader pass already expired.
        let leader_guard = match leader_lock::try_acquire(&db, keys::CONTAINERS_REAPER).await {
            LeadershipOutcome::Leader(g) => Some(Some(g)),
            LeadershipOutcome::NotLeader => Some(None),
            LeadershipOutcome::NoCoordination => None,
        };
        let is_leader = !matches!(leader_guard, Some(None));

        if is_leader {
            let now = Utc::now();
            match containers.mark_expired_idle(now).await {
                Ok(expired_ids) if !expired_ids.is_empty() => {
                    tracing::info!(count = expired_ids.len(), "Reaped idle containers");
                    for id in &expired_ids {
                        if registry.remove(id).is_some() {
                            tracing::debug!(
                                container_id = %id,
                                "Evicted idle container session from registry"
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "Containers reaper pass failed");
                }
            }
        }

        drop(leader_guard);
    }
}
