//! Background job infrastructure for the AI Gateway.
//!
//! This module provides background workers for periodic maintenance tasks:
//!
//! - **Vector Store Cleanup**: Removes soft-deleted vector stores, their chunks,
//!   and orphaned files after a configurable delay.
//! - **Container Cleanup**: Hard-deletes `expired` / `deleted` containers (and
//!   their captured `container_files`) after a configurable delay.
//! - **Provider Health Checks**: Periodically checks provider availability and
//!   publishes health status changes to the EventBus.
//!
//! Jobs follow a consistent pattern:
//! 1. Configuration in `config/features.rs` or provider config
//! 2. Worker function that runs in a loop with configurable interval
//! 3. Run function that performs a single pass
//! 4. Structured result type for tracking state
//! 5. Metrics/events for monitoring operations
//!
//! # Example
//!
//! ```toml
//! [features.vector_store_cleanup]
//! enabled = true
//! interval_secs = 300
//! cleanup_delay_secs = 3600
//!
//! [providers.my-openai.health_check]
//! enabled = true
//! mode = "reachability"
//! interval_secs = 60
//! ```

#[cfg(feature = "server")]
mod background_responses;
#[cfg(feature = "server")]
mod containers_cleanup;
#[cfg(feature = "server")]
mod containers_reaper;
mod leader_lock;
mod model_catalog_sync;
#[cfg(feature = "server")]
mod model_pricing_refresh;
mod oauth_code_cleanup;
mod provider_health_check;
#[cfg(feature = "server")]
mod responses_cancel_poller;
#[cfg(feature = "server")]
mod responses_retention;
mod vector_store_cleanup;

#[cfg(feature = "server")]
pub use background_responses::start_background_response_worker;
#[cfg(feature = "server")]
pub use containers_cleanup::start_containers_cleanup_worker;
#[cfg(feature = "server")]
pub use containers_reaper::start_containers_reaper_worker;
pub use model_catalog_sync::start_model_catalog_sync_worker;
#[cfg(feature = "server")]
pub use model_pricing_refresh::start_model_pricing_overlay_refresh_worker;
pub use oauth_code_cleanup::start_oauth_code_cleanup_worker;
pub use provider_health_check::{
    ProviderHealthChecker, ProviderHealthState, ProviderHealthStateRegistry,
};
#[cfg(feature = "server")]
pub use responses_cancel_poller::start_responses_cancel_poller;
#[cfg(feature = "server")]
pub use responses_retention::start_responses_retention_worker;
pub use vector_store_cleanup::start_vector_store_cleanup_worker;
