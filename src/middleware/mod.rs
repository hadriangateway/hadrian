//! Axum middleware layers for the Hadrian gateway.
//!
//! # Middleware Pipeline
//!
//! Middleware is applied in layers, with different middleware for different route groups:
//!
//! ## Global (all routes)
//! - [`request_id_middleware`] — Assigns a unique request ID to each request
//! - [`security_headers_middleware`] — Adds security response headers (CSP, HSTS, etc.)
//!
//! ## API routes (`/v1/*`)
//! Applied via [`get_api_routes()`](crate::routes::api::get_api_routes) in this order:
//! 1. [`rate_limit_middleware`] — IP-based rate limiting (rejects early before auth overhead)
//! 2. [`api_middleware`] — Authentication, budget enforcement, usage tracking
//! 3. [`api_authz_middleware`] — CEL-based authorization policy evaluation
//!
//! ## Admin routes (`/admin/v1/*`)
//! - [`admin_auth_middleware`] — Admin authentication (OIDC/cookie/API key)
//! - [`authz_middleware`] — System-level CEL policy evaluation
//!
//! ## Unprotected admin routes (login, session info)
//! - [`permissive_authz_middleware`] — Injects allow-all authz context

// ── Types extracted by middleware (used by route handlers via Extension<T>) ────
// Always available on all targets (including WASM).
mod types;
pub use types::{AdminAuth, AuthzContext, ClientInfo, RequestId};

// ── True middleware (Axum middleware layers) — server only ───────────────────
#[cfg(feature = "server")]
mod layers;

// ── Internal utilities (budget, scope, usage helpers for combined middleware) ──
#[cfg(feature = "server")]
pub(crate) mod util;

// Budget/token reservation handoff between the API middleware and streaming
// usage logging (see `util::settlement`).
// ── Middleware layer exports — server only ───────────────────────────────────
#[cfg(feature = "sso")]
pub use layers::admin::strip_reserved_roles;
#[cfg(feature = "sso")]
pub use layers::rate_limit::extract_client_ip_from_parts;
#[cfg(feature = "server")]
pub use layers::{
    admin::admin_auth_middleware,
    api::api_middleware,
    authz::{AuthzResponse, api_authz_middleware, authz_middleware, permissive_authz_middleware},
    rate_limit::{discover_rate_limit_middleware, rate_limit_middleware},
    request_id::request_id_middleware,
    security_headers::security_headers_middleware,
};
#[cfg(feature = "server")]
pub use util::settlement::{SettlementCell, StreamSettlement};
