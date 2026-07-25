//! Post-request settlement of budget and token-rate-limit reservations.
//!
//! The API middleware reserves an estimated cost (budget) and estimated token
//! count (TPM/TPD) before a request runs. Once actual usage is known the
//! reservation must be adjusted to the real values. For non-streaming
//! responses the middleware itself settles from response headers
//! (`X-Cost-Microcents`, token headers) right after the handler returns. For
//! streaming responses those headers do not exist — the true cost is only
//! known when the stream finishes — so the middleware parks the reservations
//! in a [`SettlementCell`] on the request extensions; the streaming handler
//! claims the cell when it wraps the body in a `UsageTrackingStream`, and the
//! streaming usage logger settles with the final token counts (including the
//! partial-usage path when a client disconnects mid-stream).
//!
//! Exactly one side settles: the cell is a one-shot `take()`, so whichever
//! party still holds the [`StreamSettlement`] after the response completes
//! performs the adjustment.

use std::sync::{Arc, Mutex};

use crate::{
    cache::Cache,
    middleware::{
        layers::rate_limit::{TokenRateLimitCheckResult, adjust_token_reservation},
        util::budget::{BudgetCheckResult, adjust_budget_reservation},
    },
    observability::metrics,
};

/// Reservations taken pre-request that must be adjusted to actual usage.
pub struct StreamSettlement {
    pub cache: Arc<dyn Cache>,
    pub budget: Option<BudgetCheckResult>,
    pub token: Option<TokenRateLimitCheckResult>,
}

impl StreamSettlement {
    /// Adjust the reservations to actual usage.
    ///
    /// Pass `actual_cost_microcents = 0` / `actual_total_tokens = 0` to refund
    /// the reservations entirely (failed requests).
    pub async fn settle(&self, actual_cost_microcents: i64, actual_total_tokens: i64) {
        if let Some(budget) = &self.budget {
            let succeeded =
                adjust_budget_reservation(&self.cache, budget, actual_cost_microcents).await;
            let operation = if actual_cost_microcents == 0 {
                "refund"
            } else {
                "adjust"
            };
            metrics::record_cache_operation(
                "budget",
                operation,
                if succeeded { "success" } else { "error" },
            );
        }

        if let Some(token) = &self.token {
            let succeeded = adjust_token_reservation(&self.cache, token, actual_total_tokens).await;
            let operation = if actual_total_tokens == 0 {
                "refund"
            } else {
                "adjust"
            };
            metrics::record_cache_operation(
                "token_rate_limit",
                operation,
                if succeeded { "success" } else { "error" },
            );
        }
    }
}

/// One-shot shared cell handing a [`StreamSettlement`] from the middleware to
/// a streaming handler. Cloned into the request extensions; `take()` is
/// atomic, so exactly one of (middleware, streaming logger) settles.
#[derive(Clone)]
pub struct SettlementCell(Arc<Mutex<Option<StreamSettlement>>>);

impl SettlementCell {
    pub fn new(settlement: StreamSettlement) -> Self {
        Self(Arc::new(Mutex::new(Some(settlement))))
    }

    /// Claim the settlement. Returns `None` if the other side already took it.
    pub fn take(&self) -> Option<StreamSettlement> {
        self.0.lock().ok()?.take()
    }
}
