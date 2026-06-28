//! Retention + reaper worker for the `responses` table.
//!
//! Each pass under the cluster-wide leader lock:
//! 1. **Reap**: mark `status='in_progress'` rows older than
//!    `max_in_progress_secs` as Failed with `code='worker_lost'`. A
//!    worker that died mid-execution would otherwise leave the row
//!    stuck forever (`claim_queued` only picks `queued`).
//! 2. **Prune**: delete rows whose `retention_expires_at` is past.
//! 3. **MCP approvals sweep** (when the `mcp` feature is enabled):
//!    delete `mcp_pending_approvals` rows past their `expires_at`. The
//!    claim path already gates on `expires_at > now`, so stale rows are
//!    never executable — this sweep just stops them accumulating
//!    forever when a gated call is never resumed.
//!
//! The reap stamps a fresh `retention_expires_at` so the prune
//! picks reaped rows up on a future cycle, not the current one
//! (gives external observers time to see the terminal state).

use std::{sync::Arc, time::Duration as StdDuration};

use chrono::Utc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::{
    jobs::leader_lock::{self, LeadershipOutcome, keys},
    services::ResponsesStore,
};

/// Loop until `shutdown` is cancelled. Runs under `tokio::spawn`. Each
/// pass tries to acquire the cluster-wide leader lock first; non-leader
/// replicas skip the prune so only one replica writes per interval.
pub async fn start_responses_retention_worker(
    store: Arc<ResponsesStore>,
    db: Arc<crate::db::DbPool>,
    cleanup_interval: StdDuration,
    max_in_progress: StdDuration,
    shutdown: CancellationToken,
) {
    tracing::info!(
        interval_secs = cleanup_interval.as_secs(),
        max_in_progress_secs = max_in_progress.as_secs(),
        "Starting responses retention worker"
    );

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("Responses retention worker received shutdown signal");
                return;
            }
            _ = sleep(cleanup_interval) => {}
        }

        let _guard = match leader_lock::try_acquire(&db, keys::RESPONSES_RETENTION).await {
            LeadershipOutcome::Leader(g) => Some(g),
            LeadershipOutcome::NotLeader => {
                tracing::trace!("responses_retention: not leader, skipping");
                continue;
            }
            LeadershipOutcome::NoCoordination => None,
        };

        match store.reap_stuck(max_in_progress).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(reaped = n, "Reaped stuck in_progress response rows"),
            Err(e) => tracing::warn!(error = %e, "Responses reaper pass failed"),
        }

        match store.prune_expired(Utc::now()).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(deleted = n, "Pruned expired response rows"),
            Err(e) => tracing::warn!(error = %e, "Responses retention pass failed"),
        }

        // Prune expired video-job mapping rows. Proxy-on-read means the
        // upstream provider owns the asset lifecycle; this only bounds the
        // local routing-map storage. Shares the leader lock above.
        match db.videos().delete_expired(Utc::now()).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(deleted = n, "Pruned expired video rows"),
            Err(e) => tracing::warn!(error = %e, "Video retention pass failed"),
        }

        // Sweep parked MCP approvals past their TTL. Runs under the same
        // leader lock so only one replica writes; the claim path already
        // refuses expired rows, so this only bounds storage growth.
        #[cfg(feature = "mcp")]
        match db.mcp_pending_approvals().delete_expired(Utc::now()).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(deleted = n, "Swept expired MCP pending-approval rows"),
            Err(e) => tracing::warn!(error = %e, "MCP pending-approvals sweep failed"),
        }
    }
}
