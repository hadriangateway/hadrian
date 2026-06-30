#![allow(dead_code)]

//! OpenAI-compatible video generation API types.
//!
//! Video generation is asynchronous: creating a video returns a job in the
//! `queued` state, and the client polls for completion before downloading the
//! rendered asset. Endpoints:
//! - POST   /v1/videos                     - Create a video generation job
//! - GET    /v1/videos                      - List video jobs
//! - GET    /v1/videos/{id}                 - Retrieve a video job
//! - DELETE /v1/videos/{id}                 - Delete a video job
//! - GET    /v1/videos/{id}/content         - Download the rendered asset
//! - POST   /v1/videos/{id}/remix           - Remix an existing video
//! - POST   /v1/videos/edits                - Edit an existing video
//! - POST   /v1/videos/extensions           - Extend an existing video
//! - POST   /v1/videos/characters           - Create a character from a video
//! - GET    /v1/videos/characters/{id}      - Retrieve a character

use serde::{Deserialize, Serialize};
use validator::Validate;

/// Video generation model (for schema/documentation; requests accept any
/// `model` string so provider-prefixed ids like `openai/sora-2` route).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub enum VideoModel {
    #[serde(rename = "sora-2")]
    Sora2,
    #[serde(rename = "sora-2-pro")]
    Sora2Pro,
    #[serde(rename = "sora-2-2025-10-06")]
    Sora2_20251006,
    #[serde(rename = "sora-2-pro-2025-10-06")]
    Sora2Pro20251006,
    #[serde(rename = "sora-2-2025-12-08")]
    Sora2_20251208,
}

/// Status of a video generation job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum VideoStatus {
    /// Queued for processing.
    Queued,
    /// Generation underway.
    InProgress,
    /// Ready for download.
    Completed,
    /// Generation failed (see `error`).
    Failed,
}

impl VideoStatus {
    /// Terminal states no longer change upstream, so callers can stop polling.
    pub fn is_terminal(self) -> bool {
        matches!(self, VideoStatus::Completed | VideoStatus::Failed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VideoStatus::Queued => "queued",
            VideoStatus::InProgress => "in_progress",
            VideoStatus::Completed => "completed",
            VideoStatus::Failed => "failed",
        }
    }
}

/// Output resolution for a generated video.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub enum VideoSize {
    /// 720x1280 portrait (default).
    #[default]
    #[serde(rename = "720x1280")]
    Size720x1280,
    /// 1280x720 landscape.
    #[serde(rename = "1280x720")]
    Size1280x720,
    /// 1024x1792 portrait.
    #[serde(rename = "1024x1792")]
    Size1024x1792,
    /// 1792x1024 landscape.
    #[serde(rename = "1792x1024")]
    Size1792x1024,
}

impl VideoSize {
    pub fn as_str(self) -> &'static str {
        match self {
            VideoSize::Size720x1280 => "720x1280",
            VideoSize::Size1280x720 => "1280x720",
            VideoSize::Size1024x1792 => "1024x1792",
            VideoSize::Size1792x1024 => "1792x1024",
        }
    }
}

/// Requested video duration in seconds. Creation/edits allow 4/8/12;
/// extensions additionally allow 16/20.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub enum VideoSeconds {
    #[default]
    #[serde(rename = "4")]
    Four,
    #[serde(rename = "8")]
    Eight,
    #[serde(rename = "12")]
    Twelve,
    #[serde(rename = "16")]
    Sixteen,
    #[serde(rename = "20")]
    Twenty,
}

impl VideoSeconds {
    pub fn as_str(self) -> &'static str {
        match self {
            VideoSeconds::Four => "4",
            VideoSeconds::Eight => "8",
            VideoSeconds::Twelve => "12",
            VideoSeconds::Sixteen => "16",
            VideoSeconds::Twenty => "20",
        }
    }

    /// Numeric duration, used for per-second usage/pricing.
    pub fn as_i64(self) -> i64 {
        match self {
            VideoSeconds::Four => 4,
            VideoSeconds::Eight => 8,
            VideoSeconds::Twelve => 12,
            VideoSeconds::Sixteen => 16,
            VideoSeconds::Twenty => 20,
        }
    }
}

/// Asset variant for the content-download endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum VideoVariant {
    /// The rendered video (default).
    #[default]
    Video,
    /// A still thumbnail.
    Thumbnail,
    /// A spritesheet of frames.
    Spritesheet,
}

impl VideoVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            VideoVariant::Video => "video",
            VideoVariant::Thumbnail => "thumbnail",
            VideoVariant::Spritesheet => "spritesheet",
        }
    }
}

/// Reference image used to guide generation (image-to-video).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct InputReference {
    /// Id of a previously uploaded file to use as the reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,

    /// Fully qualified URL or base64 data URL of the reference image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

/// Create video generation request (POST /v1/videos).
#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateVideoRequest {
    /// A text description of the desired video.
    pub prompt: String,

    /// The model to use for video generation (defaults to `sora-2`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Requested duration in seconds (4, 8, or 12).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<VideoSeconds>,

    /// Output resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<VideoSize>,

    /// Optional reference image to guide generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_reference: Option<InputReference>,

    /// **Hadrian Extension:** Per-request sovereignty requirements.
    /// Merged with API key requirements (most restrictive wins). Stripped
    /// before the request is forwarded upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sovereignty_requirements: Option<crate::config::SovereigntyRequirements>,
}

/// Reference to an existing video by id (edits/extensions).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct VideoRef {
    /// Id of the source video.
    pub id: String,
}

/// Remix request (POST /v1/videos/{id}/remix).
#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct RemixVideoRequest {
    /// Updated direction for the remixed video.
    pub prompt: String,
}

/// Edit request (POST /v1/videos/edits).
#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct VideoEditRequest {
    /// Instructions describing the edit.
    pub prompt: String,

    /// The source video to edit.
    pub video: VideoRef,
}

/// Extension request (POST /v1/videos/extensions).
#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct VideoExtensionRequest {
    /// Direction for the extended segment.
    pub prompt: String,

    /// Additional duration in seconds (4, 8, 12, 16, or 20).
    pub seconds: VideoSeconds,

    /// The source video to extend.
    pub video: VideoRef,
}

/// Create-character request (POST /v1/videos/characters).
///
/// Note: this endpoint accepts multipart/form-data. The `video` field is a
/// file upload (the source clip), not a JSON field.
#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateCharacterRequest {
    /// Display name for the new character.
    pub name: String,

    /// Optional model id, used only to resolve the provider (default `sora-2`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Error details for a failed video job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct VideoError {
    /// Machine-readable error code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    /// Human-readable error message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A video generation job.
///
/// Fields beyond the core identity are optional so provider responses with
/// varying shapes still deserialize; Hadrian persists and re-serves the
/// last-known object verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Video {
    /// Unique job identifier.
    pub id: String,

    /// Object type, always "video".
    #[serde(default = "video_object")]
    pub object: String,

    /// Model used for generation.
    pub model: String,

    /// Current job status.
    pub status: VideoStatus,

    /// Approximate completion percentage (0-100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<i32>,

    /// Unix timestamp (seconds) when the job was created.
    pub created_at: i64,

    /// Unix timestamp (seconds) when the job completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,

    /// Unix timestamp (seconds) when the rendered asset expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,

    /// Original generation prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    /// Requested duration in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<String>,

    /// Output resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,

    /// Source video id when this job is a remix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remixed_from_video_id: Option<String>,

    /// Error details when `status` is `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<VideoError>,
}

fn video_object() -> String {
    "video".to_string()
}

/// Response from deleting a video job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct VideoDeleteResponse {
    /// Id of the deleted video.
    pub id: String,

    /// Object type, always "video.deleted".
    pub object: String,

    /// Whether the video was deleted.
    pub deleted: bool,
}

impl VideoDeleteResponse {
    pub fn new(id: impl Into<String>, deleted: bool) -> Self {
        Self {
            id: id.into(),
            object: "video.deleted".to_string(),
            deleted,
        }
    }
}

/// Paginated list of video jobs (OpenAI list envelope).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct VideoListResponse {
    /// Object type, always "list".
    pub object: String,

    /// The video jobs in this page.
    pub data: Vec<Video>,

    /// Id of the first item in the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,

    /// Id of the last item in the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,

    /// Whether more items exist after this page.
    pub has_more: bool,
}

/// A character created from a reference video.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Character {
    /// Unique character identifier.
    pub id: String,

    /// Object type, always "video.character".
    #[serde(default = "character_object")]
    pub object: String,

    /// Unix timestamp (seconds) when the character was created.
    pub created_at: i64,

    /// Display name.
    pub name: String,
}

fn character_object() -> String {
    "video.character".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_video_request_minimal() {
        let json = r#"{"prompt": "a cat surfing"}"#;
        let req: CreateVideoRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.prompt, "a cat surfing");
        assert!(req.model.is_none());
        assert!(req.seconds.is_none());
    }

    #[test]
    fn test_create_video_request_full_serialization() {
        let req = CreateVideoRequest {
            prompt: "a cat surfing".to_string(),
            model: Some("sora-2".to_string()),
            seconds: Some(VideoSeconds::Eight),
            size: Some(VideoSize::Size1280x720),
            input_reference: Some(InputReference {
                file_id: Some("file_123".to_string()),
                image_url: None,
            }),
            sovereignty_requirements: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"seconds\":\"8\""));
        assert!(json.contains("\"size\":\"1280x720\""));
        assert!(json.contains("\"file_id\":\"file_123\""));
        // Stripped/absent when None.
        assert!(!json.contains("sovereignty_requirements"));
    }

    #[test]
    fn test_video_object_deserialization() {
        let json = r#"{
            "id": "video_abc",
            "object": "video",
            "model": "sora-2",
            "status": "queued",
            "created_at": 1730000000,
            "seconds": "8",
            "size": "720x1280"
        }"#;
        let video: Video = serde_json::from_str(json).unwrap();
        assert_eq!(video.id, "video_abc");
        assert_eq!(video.status, VideoStatus::Queued);
        assert_eq!(video.seconds.as_deref(), Some("8"));
        assert!(!video.status.is_terminal());
    }

    #[test]
    fn test_video_object_minimal_defaults_object() {
        let json = r#"{"id":"video_x","model":"sora-2","status":"completed","created_at":1}"#;
        let video: Video = serde_json::from_str(json).unwrap();
        assert_eq!(video.object, "video");
        assert!(video.status.is_terminal());
    }

    #[test]
    fn test_video_seconds_numeric() {
        assert_eq!(VideoSeconds::Twelve.as_i64(), 12);
        assert_eq!(VideoSeconds::Twenty.as_i64(), 20);
        assert_eq!(VideoSeconds::Twelve.as_str(), "12");
    }

    #[test]
    fn test_video_status_serde() {
        assert_eq!(
            serde_json::to_string(&VideoStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
    }

    #[test]
    fn test_video_size_serde() {
        assert_eq!(
            serde_json::to_string(&VideoSize::Size1792x1024).unwrap(),
            "\"1792x1024\""
        );
        assert_eq!(VideoSize::Size720x1280.as_str(), "720x1280");
    }

    #[test]
    fn test_video_delete_response() {
        let resp = VideoDeleteResponse::new("video_abc", true);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"object\":\"video.deleted\""));
        assert!(json.contains("\"deleted\":true"));
    }

    #[test]
    fn test_video_variant_str() {
        assert_eq!(VideoVariant::Spritesheet.as_str(), "spritesheet");
        assert_eq!(VideoVariant::default(), VideoVariant::Video);
    }

    #[test]
    fn test_extension_request_requires_seconds() {
        let json = r#"{"prompt":"more","seconds":"16","video":{"id":"video_1"}}"#;
        let req: VideoExtensionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.seconds, VideoSeconds::Sixteen);
        assert_eq!(req.video.id, "video_1");
    }
}
