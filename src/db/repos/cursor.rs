//! Cursor-based pagination support for efficient, consistent pagination.
//!
//! This module provides cursor-based pagination as an alternative to offset-based
//! pagination. Cursor pagination offers several advantages:
//!
//! - **Performance**: O(1) lookup vs O(n) for offset-based pagination
//! - **Consistency**: Stable results even when data changes between requests
//! - **Efficiency**: Uses indexed columns for seeking
//!
//! # Usage
//!
//! ```rust,ignore
//! use crate::db::repos::{Cursor, CursorDirection, ListParams};
//!
//! // First page (no cursor)
//! let params = ListParams { limit: Some(20), ..Default::default() };
//! let (items, cursors) = repo.list_with_cursor(params).await?;
//!
//! // Next page (use next_cursor from response)
//! let params = ListParams {
//!     limit: Some(20),
//!     cursor: cursors.next,
//!     direction: CursorDirection::Forward,
//!     ..Default::default()
//! };
//! ```
//!
//! # Timestamp Precision
//!
//! **Important**: Cursors encode timestamps as milliseconds. If entities store timestamps
//! with higher precision (e.g., nanoseconds from `Utc::now()`), the cursor's decoded
//! timestamp won't match the stored value, causing comparison issues in SQLite
//! (which stores DateTime as TEXT).
//!
//! To avoid this, truncate timestamps to milliseconds when creating entities:
//!
//! ```rust,ignore
//! use crate::db::repos::truncate_to_millis;
//!
//! let created_at = truncate_to_millis(Utc::now());
//! ```

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Error type for cursor operations.
#[derive(Debug, Error)]
pub enum CursorError {
    #[error("invalid cursor format")]
    InvalidFormat,
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid timestamp in cursor")]
    InvalidTimestamp,
    #[error("invalid UUID in cursor")]
    InvalidUuid,
}

/// A cursor for keyset pagination, encoding a position in an ordered result set.
///
/// The cursor encodes both `created_at` timestamp and `id` to provide a unique,
/// stable ordering even when multiple records have the same timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The timestamp component of the cursor position.
    pub created_at: DateTime<Utc>,
    /// The UUID component of the cursor position.
    pub id: Uuid,
}

impl Cursor {
    /// Create a new cursor from a timestamp and ID.
    pub fn new(created_at: DateTime<Utc>, id: Uuid) -> Self {
        Self { created_at, id }
    }

    /// Encode the cursor as a URL-safe base64 string.
    ///
    /// Format: `{timestamp_millis}:{uuid}` encoded as base64.
    pub fn encode(&self) -> String {
        let raw = format!("{}:{}", self.created_at.timestamp_millis(), self.id);
        URL_SAFE_NO_PAD.encode(raw.as_bytes())
    }

    /// Decode a cursor from a base64 string.
    pub fn decode(encoded: &str) -> Result<Self, CursorError> {
        let bytes = URL_SAFE_NO_PAD.decode(encoded)?;
        let raw = String::from_utf8(bytes).map_err(|_| CursorError::InvalidFormat)?;

        // Format: {timestamp}:{uuid}
        // UUIDs use hyphens not colons, so ':' cleanly separates the two parts.
        let (timestamp_str, uuid_str) = raw.split_once(':').ok_or(CursorError::InvalidFormat)?;

        let timestamp_millis: i64 = timestamp_str
            .parse()
            .map_err(|_| CursorError::InvalidTimestamp)?;

        let created_at = DateTime::from_timestamp_millis(timestamp_millis)
            .ok_or(CursorError::InvalidTimestamp)?;

        let id = Uuid::parse_str(uuid_str).map_err(|_| CursorError::InvalidUuid)?;

        Ok(Self { created_at, id })
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.encode())
    }
}

impl Serialize for Cursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.encode())
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Cursor::decode(&s).map_err(serde::de::Error::custom)
    }
}

/// Direction for cursor-based pagination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorDirection {
    /// Fetch items after the cursor (newer items in descending order).
    #[default]
    Forward,
    /// Fetch items before the cursor (older items in descending order).
    Backward,
}

/// Cursors for navigating paginated results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageCursors {
    /// Cursor for the next page (if more items exist).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
    /// Cursor for the previous page (if not on first page).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<Cursor>,
}

impl PageCursors {
    /// Create cursors from a list of items.
    ///
    /// Items are expected to be in the order they will be returned to the user
    /// (typically descending by created_at).
    ///
    /// # Arguments
    /// * `items` - The items returned for this page
    /// * `has_more` - Whether there are more items after this page
    /// * `direction` - The direction of pagination
    /// * `cursor` - The cursor used for this request (if any)
    /// * `get_cursor` - Function to extract cursor components from an item
    pub fn from_items<T, F>(
        items: &[T],
        has_more: bool,
        direction: CursorDirection,
        cursor: Option<&Cursor>,
        get_cursor: F,
    ) -> Self
    where
        F: Fn(&T) -> Cursor,
    {
        if items.is_empty() {
            return Self::default();
        }

        let first = get_cursor(&items[0]);
        let last = get_cursor(&items[items.len() - 1]);

        match direction {
            CursorDirection::Forward => Self {
                // Next cursor: position of last item if there are more
                next: if has_more { Some(last) } else { None },
                // Prev cursor: position of first item if we're not on the first page
                prev: cursor.map(|_| first),
            },
            CursorDirection::Backward => Self {
                // When going backward, next is the first item's position
                next: cursor.map(|_| first),
                // Prev is the last item's position if there are more
                prev: if has_more { Some(last) } else { None },
            },
        }
    }
}

#[cfg(any(
    feature = "database-sqlite",
    feature = "database-postgres",
    feature = "database-wasm-sqlite"
))]
/// Create a cursor from a row's created_at and id fields.
///
/// Convenience function for use in database queries.
pub fn cursor_from_row(created_at: DateTime<Utc>, id: Uuid) -> Cursor {
    Cursor::new(created_at, id)
}

/// Truncate a DateTime to millisecond precision.
///
/// This is important for cursor-based pagination because cursors encode timestamps
/// as milliseconds. Without truncation, the cursor's timestamp (ms precision) won't
/// match the stored timestamp (ns precision), causing string comparison issues in SQLite.
///
/// # Example
///
/// ```rust,ignore
/// use chrono::Utc;
/// use crate::db::repos::truncate_to_millis;
///
/// // When creating entities that will use cursor pagination:
/// let created_at = truncate_to_millis(Utc::now());
/// ```
pub fn truncate_to_millis(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(dt.timestamp_millis()).unwrap_or(dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_encode_decode_roundtrip() {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let cursor = Cursor::new(now, id);

        let encoded = cursor.encode();
        let decoded = Cursor::decode(&encoded).unwrap();

        // Compare milliseconds since encode uses millis precision
        assert_eq!(
            cursor.created_at.timestamp_millis(),
            decoded.created_at.timestamp_millis()
        );
        assert_eq!(cursor.id, decoded.id);
    }

    #[test]
    fn test_cursor_encode_is_url_safe() {
        let cursor = Cursor::new(Utc::now(), Uuid::new_v4());
        let encoded = cursor.encode();

        // URL-safe base64 should only contain alphanumeric, dash, underscore
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn test_cursor_decode_invalid_base64() {
        let result = Cursor::decode("not valid base64!!!");
        assert!(matches!(result, Err(CursorError::Base64(_))));
    }

    #[test]
    fn test_cursor_decode_invalid_format() {
        // Valid base64 but missing colon separator
        let encoded = URL_SAFE_NO_PAD.encode(b"invalid_format");
        let result = Cursor::decode(&encoded);
        assert!(matches!(result, Err(CursorError::InvalidFormat)));
    }

    #[test]
    fn test_cursor_decode_invalid_timestamp() {
        // Valid format but non-numeric timestamp
        let encoded = URL_SAFE_NO_PAD.encode(b"not_a_number:00000000-0000-0000-0000-000000000000");
        let result = Cursor::decode(&encoded);
        assert!(matches!(result, Err(CursorError::InvalidTimestamp)));
    }

    #[test]
    fn test_cursor_decode_invalid_uuid() {
        // Valid format but invalid UUID
        let encoded = URL_SAFE_NO_PAD.encode(b"1234567890:not-a-uuid");
        let result = Cursor::decode(&encoded);
        assert!(matches!(result, Err(CursorError::InvalidUuid)));
    }

    #[test]
    fn test_cursor_serde_roundtrip() {
        let cursor = Cursor::new(Utc::now(), Uuid::new_v4());
        let json = serde_json::to_string(&cursor).unwrap();
        let decoded: Cursor = serde_json::from_str(&json).unwrap();

        assert_eq!(
            cursor.created_at.timestamp_millis(),
            decoded.created_at.timestamp_millis()
        );
        assert_eq!(cursor.id, decoded.id);
    }

    #[test]
    fn test_cursor_direction_default() {
        assert_eq!(CursorDirection::default(), CursorDirection::Forward);
    }

    #[test]
    fn test_cursor_direction_serde() {
        let forward: CursorDirection = serde_json::from_str("\"forward\"").unwrap();
        let backward: CursorDirection = serde_json::from_str("\"backward\"").unwrap();

        assert_eq!(forward, CursorDirection::Forward);
        assert_eq!(backward, CursorDirection::Backward);
    }

    #[test]
    fn test_page_cursors_empty_items() {
        let cursors = PageCursors::from_items::<(), _>(
            &[],
            false,
            CursorDirection::Forward,
            None,
            |_| unreachable!(),
        );
        assert!(cursors.next.is_none());
        assert!(cursors.prev.is_none());
    }

    #[test]
    fn test_page_cursors_first_page_with_more() {
        let items = vec![(Utc::now(), Uuid::new_v4()), (Utc::now(), Uuid::new_v4())];

        let cursors = PageCursors::from_items(
            &items,
            true, // has_more
            CursorDirection::Forward,
            None, // no cursor = first page
            |(created_at, id)| Cursor::new(*created_at, *id),
        );

        // First page with more items: next cursor, no prev cursor
        assert!(cursors.next.is_some());
        assert!(cursors.prev.is_none());
    }

    #[test]
    fn test_page_cursors_middle_page() {
        let items = vec![(Utc::now(), Uuid::new_v4()), (Utc::now(), Uuid::new_v4())];
        let prev_cursor = Cursor::new(Utc::now(), Uuid::new_v4());

        let cursors = PageCursors::from_items(
            &items,
            true, // has_more
            CursorDirection::Forward,
            Some(&prev_cursor),
            |(created_at, id)| Cursor::new(*created_at, *id),
        );

        // Middle page: both next and prev cursors
        assert!(cursors.next.is_some());
        assert!(cursors.prev.is_some());
    }

    #[test]
    fn test_page_cursors_last_page() {
        let items = vec![(Utc::now(), Uuid::new_v4()), (Utc::now(), Uuid::new_v4())];
        let prev_cursor = Cursor::new(Utc::now(), Uuid::new_v4());

        let cursors = PageCursors::from_items(
            &items,
            false, // no more items
            CursorDirection::Forward,
            Some(&prev_cursor),
            |(created_at, id)| Cursor::new(*created_at, *id),
        );

        // Last page: no next cursor, has prev cursor
        assert!(cursors.next.is_none());
        assert!(cursors.prev.is_some());
    }

    #[cfg(any(feature = "database-sqlite", feature = "database-postgres"))]
    #[test]
    fn test_cursor_from_row() {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let cursor = cursor_from_row(now, id);

        assert_eq!(cursor.created_at, now);
        assert_eq!(cursor.id, id);
    }
}
