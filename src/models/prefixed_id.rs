//! Prefixed ID types for OpenAI API compatibility.
//!
//! OpenAI uses prefixed string IDs like `vs_abc123` for vector stores and `file-abc123` for files.
//! This module provides types that serialize UUIDs with these prefixes for API responses while
//! accepting both prefixed and plain UUIDs as input for backward compatibility.
//!
//! # Prefixes
//!
//! | Resource | Prefix | Example |
//! |----------|--------|---------|
//! | Vector Store | `vs_` | `vs_550e8400-e29b-41d4-a716-446655440000` |
//! | File | `file-` | `file-550e8400-e29b-41d4-a716-446655440000` |
//! | Vector Store File | `file-` | `file-550e8400-e29b-41d4-a716-446655440000` |
//! | File Batch | `vsfb_` | `vsfb_550e8400-e29b-41d4-a716-446655440000` |
//! | Chunk | `chunk_` | `chunk_550e8400-e29b-41d4-a716-446655440000` |

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

/// A UUID that serializes with a prefix for OpenAI API compatibility.
///
/// Serializes as `{prefix}{uuid}` (e.g., `vs_550e8400-e29b-41d4-a716-446655440000`).
/// Deserializes from either prefixed or plain UUID strings for backward compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrefixedUuid {
    uuid: Uuid,
    prefix: &'static str,
}

impl PrefixedUuid {
    /// Create a new prefixed UUID.
    pub fn new(uuid: Uuid, prefix: &'static str) -> Self {
        Self { uuid, prefix }
    }

    /// Get the inner UUID.
    pub fn into_inner(self) -> Uuid {
        self.uuid
    }

    /// Get the inner UUID by reference.
    pub fn as_uuid(&self) -> &Uuid {
        &self.uuid
    }

    /// Parse a string that may or may not have the prefix.
    pub fn parse(s: &str, prefix: &'static str) -> Result<Self, PrefixedIdError> {
        let uuid_str = s.strip_prefix(prefix).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self { uuid, prefix })
    }
}

impl fmt::Display for PrefixedUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.prefix, self.uuid)
    }
}

impl From<PrefixedUuid> for Uuid {
    fn from(id: PrefixedUuid) -> Self {
        id.uuid
    }
}

/// Error type for prefixed ID parsing.
#[derive(Debug, thiserror::Error)]
pub enum PrefixedIdError {
    #[error("invalid UUID in '{input}': {source}")]
    InvalidUuid {
        input: String,
        #[source]
        source: uuid::Error,
    },
}

// =============================================================================
// Vector Store ID (prefix: "vs_")
// =============================================================================

/// A vector store ID that serializes with `vs_` prefix.
///
/// # Examples
///
/// ```
/// use uuid::Uuid;
/// use hadrian::models::VectorStoreId;
///
/// let uuid = Uuid::new_v4();
/// let id = VectorStoreId::from(uuid);
/// assert!(id.to_string().starts_with("vs_"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(value_type = String, example = "vs_550e8400-e29b-41d4-a716-446655440000"))]
pub struct VectorStoreId(Uuid);

impl VectorStoreId {
    pub const PREFIX: &'static str = "vs_";

    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn into_inner(self) -> Uuid {
        self.0
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl From<Uuid> for VectorStoreId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<VectorStoreId> for Uuid {
    fn from(id: VectorStoreId) -> Self {
        id.0
    }
}

impl fmt::Display for VectorStoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.0)
    }
}

impl FromStr for VectorStoreId {
    type Err = PrefixedIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_str = s.strip_prefix(Self::PREFIX).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self(uuid))
    }
}

impl Serialize for VectorStoreId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for VectorStoreId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

// =============================================================================
// File ID (prefix: "file-")
// =============================================================================

/// A file ID that serializes with `file-` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(value_type = String, example = "file-550e8400-e29b-41d4-a716-446655440000"))]
pub struct FileId(Uuid);

impl FileId {
    pub const PREFIX: &'static str = "file-";

    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn into_inner(self) -> Uuid {
        self.0
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl From<Uuid> for FileId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<FileId> for Uuid {
    fn from(id: FileId) -> Self {
        id.0
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.0)
    }
}

impl FromStr for FileId {
    type Err = PrefixedIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_str = s.strip_prefix(Self::PREFIX).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self(uuid))
    }
}

impl Serialize for FileId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

// =============================================================================
// Vector Store File ID (prefix: "file-")
// =============================================================================

/// A vector store file (link) ID that serializes with `file-` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(value_type = String, example = "file-550e8400-e29b-41d4-a716-446655440000"))]
pub struct VectorStoreFileId(Uuid);

impl VectorStoreFileId {
    pub const PREFIX: &'static str = "file-";

    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn into_inner(self) -> Uuid {
        self.0
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl From<Uuid> for VectorStoreFileId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<VectorStoreFileId> for Uuid {
    fn from(id: VectorStoreFileId) -> Self {
        id.0
    }
}

impl fmt::Display for VectorStoreFileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.0)
    }
}

impl FromStr for VectorStoreFileId {
    type Err = PrefixedIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_str = s.strip_prefix(Self::PREFIX).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self(uuid))
    }
}

impl Serialize for VectorStoreFileId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for VectorStoreFileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

// =============================================================================
// File Batch ID (prefix: "vsfb_")
// =============================================================================

/// A file batch ID that serializes with `vsfb_` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(value_type = String, example = "vsfb_550e8400-e29b-41d4-a716-446655440000"))]
pub struct FileBatchId(Uuid);

impl FileBatchId {
    pub const PREFIX: &'static str = "vsfb_";

    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn into_inner(self) -> Uuid {
        self.0
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl From<Uuid> for FileBatchId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<FileBatchId> for Uuid {
    fn from(id: FileBatchId) -> Self {
        id.0
    }
}

impl fmt::Display for FileBatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.0)
    }
}

impl FromStr for FileBatchId {
    type Err = PrefixedIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_str = s.strip_prefix(Self::PREFIX).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self(uuid))
    }
}

impl Serialize for FileBatchId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FileBatchId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

// =============================================================================
// Chunk ID (prefix: "chunk_")
// =============================================================================

/// A chunk ID that serializes with `chunk_` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(value_type = String, example = "chunk_550e8400-e29b-41d4-a716-446655440000"))]
pub struct ChunkId(Uuid);

impl ChunkId {
    pub const PREFIX: &'static str = "chunk_";

    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn into_inner(self) -> Uuid {
        self.0
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl From<Uuid> for ChunkId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<ChunkId> for Uuid {
    fn from(id: ChunkId) -> Self {
        id.0
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.0)
    }
}

impl FromStr for ChunkId {
    type Err = PrefixedIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_str = s.strip_prefix(Self::PREFIX).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self(uuid))
    }
}

impl Serialize for ChunkId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ChunkId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

// =============================================================================
// Skill ID (prefix: "skill_")
// =============================================================================

/// A skill ID that serializes with `skill_` prefix (OpenAI Skills API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(value_type = String, example = "skill_550e8400-e29b-41d4-a716-446655440000"))]
pub struct SkillId(Uuid);

impl SkillId {
    pub const PREFIX: &'static str = "skill_";

    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn into_inner(self) -> Uuid {
        self.0
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl From<Uuid> for SkillId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<SkillId> for Uuid {
    fn from(id: SkillId) -> Self {
        id.0
    }
}

impl fmt::Display for SkillId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.0)
    }
}

impl FromStr for SkillId {
    type Err = PrefixedIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_str = s.strip_prefix(Self::PREFIX).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self(uuid))
    }
}

impl Serialize for SkillId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SkillId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

// =============================================================================
// Skill Version ID (prefix: "skillver_")
// =============================================================================

/// A skill version ID that serializes with `skillver_` prefix (OpenAI Skills API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(value_type = String, example = "skillver_550e8400-e29b-41d4-a716-446655440000"))]
pub struct SkillVersionId(Uuid);

impl SkillVersionId {
    pub const PREFIX: &'static str = "skillver_";

    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn into_inner(self) -> Uuid {
        self.0
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl From<Uuid> for SkillVersionId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<SkillVersionId> for Uuid {
    fn from(id: SkillVersionId) -> Self {
        id.0
    }
}

impl fmt::Display for SkillVersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.0)
    }
}

impl FromStr for SkillVersionId {
    type Err = PrefixedIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_str = s.strip_prefix(Self::PREFIX).unwrap_or(s);
        let uuid = Uuid::parse_str(uuid_str).map_err(|e| PrefixedIdError::InvalidUuid {
            input: s.to_string(),
            source: e,
        })?;
        Ok(Self(uuid))
    }
}

impl Serialize for SkillVersionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SkillVersionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

// =============================================================================
// Serde Helper Modules
// =============================================================================
//
// These modules allow using `#[serde(with = "vector_store_id_serde")]` on Uuid fields
// to automatically serialize/deserialize with prefixes while keeping the internal
// representation as Uuid.

/// Serde module for vector store IDs (`vs_` prefix).
///
/// # Example
///
/// ```ignore
/// use uuid::Uuid;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct MyStruct {
///     #[serde(with = "crate::models::vector_store_id_serde")]
///     id: Uuid,
/// }
/// ```
pub mod vector_store_id_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use uuid::Uuid;

    use super::VectorStoreId;

    pub fn serialize<S>(uuid: &Uuid, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&VectorStoreId::from(*uuid).to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
    where
        D: Deserializer<'de>,
    {
        let id = VectorStoreId::deserialize(deserializer)?;
        Ok(id.into_inner())
    }
}

/// Serde module for file IDs (`file-` prefix).
pub mod file_id_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use uuid::Uuid;

    use super::FileId;

    pub fn serialize<S>(uuid: &Uuid, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&FileId::from(*uuid).to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
    where
        D: Deserializer<'de>,
    {
        let id = FileId::deserialize(deserializer)?;
        Ok(id.into_inner())
    }
}
/// Serde module for chunk IDs (`chunk_` prefix).
pub mod chunk_id_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use uuid::Uuid;

    use super::ChunkId;

    pub fn serialize<S>(uuid: &Uuid, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&ChunkId::from(*uuid).to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
    where
        D: Deserializer<'de>,
    {
        let id = ChunkId::deserialize(deserializer)?;
        Ok(id.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_store_id_serialization() {
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let id = VectorStoreId::new(uuid);

        // Test display
        assert_eq!(id.to_string(), "vs_550e8400-e29b-41d4-a716-446655440000");

        // Test JSON serialization
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"vs_550e8400-e29b-41d4-a716-446655440000\"");
    }

    #[test]
    fn test_vector_store_id_deserialization_with_prefix() {
        let id: VectorStoreId =
            serde_json::from_str("\"vs_550e8400-e29b-41d4-a716-446655440000\"").unwrap();
        assert_eq!(
            id.into_inner(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
    }

    #[test]
    fn test_vector_store_id_deserialization_without_prefix() {
        // Should accept plain UUIDs for backward compatibility
        let id: VectorStoreId =
            serde_json::from_str("\"550e8400-e29b-41d4-a716-446655440000\"").unwrap();
        assert_eq!(
            id.into_inner(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
    }

    #[test]
    fn test_file_id_serialization() {
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let id = FileId::new(uuid);

        assert_eq!(id.to_string(), "file-550e8400-e29b-41d4-a716-446655440000");

        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"file-550e8400-e29b-41d4-a716-446655440000\"");
    }

    #[test]
    fn test_file_id_deserialization() {
        // With prefix
        let id: FileId =
            serde_json::from_str("\"file-550e8400-e29b-41d4-a716-446655440000\"").unwrap();
        assert_eq!(
            id.into_inner(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );

        // Without prefix (backward compatibility)
        let id: FileId = serde_json::from_str("\"550e8400-e29b-41d4-a716-446655440000\"").unwrap();
        assert_eq!(
            id.into_inner(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
    }

    #[test]
    fn test_vector_store_file_id_serialization() {
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let id = VectorStoreFileId::new(uuid);

        assert_eq!(id.to_string(), "file-550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_file_batch_id_serialization() {
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let id = FileBatchId::new(uuid);

        assert_eq!(id.to_string(), "vsfb_550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_chunk_id_serialization() {
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let id = ChunkId::new(uuid);

        assert_eq!(id.to_string(), "chunk_550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_from_str() {
        // With prefix
        let id: VectorStoreId = "vs_550e8400-e29b-41d4-a716-446655440000".parse().unwrap();
        assert_eq!(
            id.into_inner(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );

        // Without prefix
        let id: VectorStoreId = "550e8400-e29b-41d4-a716-446655440000".parse().unwrap();
        assert_eq!(
            id.into_inner(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
    }

    #[test]
    fn test_invalid_uuid() {
        let result: Result<VectorStoreId, _> = "vs_invalid".parse();
        assert!(result.is_err());

        let result: Result<VectorStoreId, _> = "invalid".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_uuid_conversion() {
        let uuid = Uuid::new_v4();
        let id = VectorStoreId::from(uuid);
        let back: Uuid = id.into();
        assert_eq!(uuid, back);
    }
}
