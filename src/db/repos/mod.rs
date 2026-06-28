mod api_keys;
mod audit_logs;
mod containers;
mod conversations;
pub mod cursor;
#[cfg(feature = "sso")]
mod domain_verifications;
mod files;
#[cfg(feature = "mcp")]
mod mcp_pending_approvals;
mod model_pricing;
mod oauth_authorization_codes;
mod org_rbac_policies;
#[cfg(feature = "sso")]
mod org_sso_configs;
mod organizations;
mod projects;
mod providers;
mod response_events;
mod responses;
#[cfg(feature = "sso")]
mod scim_configs;
#[cfg(feature = "sso")]
mod scim_group_mappings;
#[cfg(feature = "sso")]
mod scim_user_mappings;
mod service_accounts;
mod skills;
#[cfg(feature = "sso")]
mod sso_group_mappings;
mod teams;
mod templates;
mod usage;
mod users;
mod vector_stores;
mod videos;

pub use api_keys::*;
pub use audit_logs::*;
use chrono::NaiveDate;
pub use containers::*;
pub use conversations::*;
pub use cursor::*;
#[cfg(feature = "sso")]
pub use domain_verifications::*;
pub use files::*;
#[cfg(feature = "mcp")]
pub use mcp_pending_approvals::*;
pub use model_pricing::*;
pub use oauth_authorization_codes::*;
pub use org_rbac_policies::*;
#[cfg(feature = "sso")]
pub use org_sso_configs::*;
pub use organizations::*;
pub use projects::*;
pub use providers::*;
pub use response_events::*;
pub use responses::*;
#[cfg(feature = "sso")]
pub use scim_configs::*;
#[cfg(feature = "sso")]
pub use scim_group_mappings::*;
#[cfg(feature = "sso")]
pub use scim_user_mappings::*;
pub use service_accounts::*;
pub use skills::*;
#[cfg(feature = "sso")]
pub use sso_group_mappings::*;
pub use teams::*;
pub use templates::*;
pub use usage::*;
pub use users::*;
pub use vector_stores::*;
pub use videos::*;

/// Sort order for list queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    /// Ascending order (oldest first)
    Asc,
    /// Descending order (newest first)
    #[default]
    Desc,
}

impl SortOrder {
    /// Get the SQL ORDER BY direction string.
    pub fn as_sql(&self) -> &'static str {
        match self {
            SortOrder::Asc => "ASC",
            SortOrder::Desc => "DESC",
        }
    }

    /// Get the opposite sort order.
    pub fn opposite(&self) -> Self {
        match self {
            SortOrder::Asc => SortOrder::Desc,
            SortOrder::Desc => SortOrder::Asc,
        }
    }

    /// Compute SQL comparison operator and ORDER BY direction for cursor-based pagination.
    ///
    /// Returns (comparison_operator, order_direction, should_reverse_results).
    ///
    /// The logic depends on both sort order and pagination direction:
    /// - Desc + Forward: items with (ts, id) < cursor, ORDER BY DESC
    /// - Desc + Backward: items with (ts, id) > cursor, ORDER BY ASC, then reverse
    /// - Asc + Forward: items with (ts, id) > cursor, ORDER BY ASC
    /// - Asc + Backward: items with (ts, id) < cursor, ORDER BY DESC, then reverse
    pub fn cursor_query_params(
        &self,
        direction: CursorDirection,
    ) -> (&'static str, &'static str, bool) {
        match (self, direction) {
            // Desc (newest first) + Forward: get older items (lower timestamps)
            (SortOrder::Desc, CursorDirection::Forward) => ("<", "DESC", false),
            // Desc + Backward: get newer items (higher timestamps), then reverse
            (SortOrder::Desc, CursorDirection::Backward) => (">", "ASC", true),
            // Asc (oldest first) + Forward: get newer items (higher timestamps)
            (SortOrder::Asc, CursorDirection::Forward) => (">", "ASC", false),
            // Asc + Backward: get older items (lower timestamps), then reverse
            (SortOrder::Asc, CursorDirection::Backward) => ("<", "DESC", true),
        }
    }
}

/// Pagination and listing parameters using cursor-based pagination.
///
/// Cursor-based pagination provides stable, performant pagination for large datasets.
/// Use `limit`, `cursor`, and `direction` to navigate through results.
#[derive(Debug, Clone, Default)]
pub struct ListParams {
    /// Maximum number of records to return.
    pub limit: Option<i64>,
    /// Cursor for keyset pagination. When provided, results start from this position.
    pub cursor: Option<Cursor>,
    /// Direction for cursor-based pagination.
    pub direction: CursorDirection,
    /// Sort order for results (asc = oldest first, desc = newest first).
    pub sort_order: SortOrder,
    /// Include soft-deleted records in results.
    pub include_deleted: bool,
}

/// Hard upper bound on `ListParams.limit`. A client passing a giant value
/// would otherwise scan an entire table and DoS the gateway. Every list
/// endpoint that materialises rows must clamp through `ListParams::clamp`
/// before passing the params to a repo.
pub const MAX_LIST_LIMIT: i64 = 1000;

impl ListParams {
    /// Clamp `limit` to `[1, MAX_LIST_LIMIT]`, leaving `None` as `None`.
    /// Idempotent — safe to call multiple times.
    pub fn clamp(mut self) -> Self {
        if let Some(limit) = self.limit {
            self.limit = Some(limit.clamp(1, MAX_LIST_LIMIT));
        }
        self
    }
}

/// Result of a paginated list query.
///
/// Contains items and pagination metadata for cursor-based pagination.
#[derive(Debug, Clone)]
pub struct ListResult<T> {
    /// The items returned for this page.
    pub items: Vec<T>,
    /// Whether there are more items after this page.
    pub has_more: bool,
    /// Cursors for navigating to next/previous pages.
    pub cursors: PageCursors,
}

impl<T> ListResult<T> {
    /// Create a new list result.
    pub fn new(items: Vec<T>, has_more: bool, cursors: PageCursors) -> Self {
        Self {
            items,
            has_more,
            cursors,
        }
    }
}

/// Date range for queries
#[derive(Debug, Clone)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}
