//! API key scope enforcement.
//!
//! Maps request paths to required scopes for access control.

use crate::models::ApiKeyScope;

/// Determine the required scope for a given request path.
///
/// Returns `None` for paths that don't require scope enforcement
/// (e.g., health checks, OpenAPI docs).
///
/// # Scope Mappings
///
/// | Scope | Endpoints |
/// |-------|-----------|
/// | `chat` | `/v1/chat/completions`, `/v1/responses` |
/// | `completions` | `/v1/completions` |
/// | `embeddings` | `/v1/embeddings` |
/// | `images` | `/v1/images/*` |
/// | `videos` | `/v1/videos/*` |
/// | `audio` | `/v1/audio/*` |
/// | `files` | `/v1/files/*`, `/v1/vector_stores/*` |
/// | `models` | `/v1/models/*` |
/// | `admin` | `/admin/*` |
pub fn required_scope_for_path(path: &str) -> Option<ApiKeyScope> {
    // Strip query parameters if present
    let path = path.split('?').next().unwrap_or(path);

    // Chat endpoints
    if path.starts_with("/v1/chat/completions") || path.starts_with("/api/v1/chat/completions") {
        return Some(ApiKeyScope::Chat);
    }
    if path.starts_with("/v1/responses") || path.starts_with("/api/v1/responses") {
        return Some(ApiKeyScope::Chat);
    }

    // Completions endpoint (legacy)
    if path == "/v1/completions" || path == "/api/v1/completions" {
        return Some(ApiKeyScope::Completions);
    }

    // Embeddings endpoint
    if path.starts_with("/v1/embeddings") || path.starts_with("/api/v1/embeddings") {
        return Some(ApiKeyScope::Embeddings);
    }

    // Images endpoints
    if path.starts_with("/v1/images") || path.starts_with("/api/v1/images") {
        return Some(ApiKeyScope::Images);
    }

    // Videos endpoints
    if path.starts_with("/v1/videos") || path.starts_with("/api/v1/videos") {
        return Some(ApiKeyScope::Videos);
    }

    // Audio endpoints
    if path.starts_with("/v1/audio") || path.starts_with("/api/v1/audio") {
        return Some(ApiKeyScope::Audio);
    }

    // Files and vector stores endpoints
    if path.starts_with("/v1/files") || path.starts_with("/api/v1/files") {
        return Some(ApiKeyScope::Files);
    }
    if path.starts_with("/v1/vector_stores") || path.starts_with("/api/v1/vector_stores") {
        return Some(ApiKeyScope::Files);
    }

    // Models endpoint
    if path.starts_with("/v1/models") || path.starts_with("/api/v1/models") {
        return Some(ApiKeyScope::Models);
    }

    // Admin endpoints
    if path.starts_with("/admin") {
        return Some(ApiKeyScope::Admin);
    }

    // No scope required for other paths (health, docs, etc.)
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_scope() {
        assert_eq!(
            required_scope_for_path("/v1/chat/completions"),
            Some(ApiKeyScope::Chat)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/chat/completions"),
            Some(ApiKeyScope::Chat)
        );
        assert_eq!(
            required_scope_for_path("/v1/responses"),
            Some(ApiKeyScope::Chat)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/responses"),
            Some(ApiKeyScope::Chat)
        );
    }

    #[test]
    fn test_completions_scope() {
        assert_eq!(
            required_scope_for_path("/v1/completions"),
            Some(ApiKeyScope::Completions)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/completions"),
            Some(ApiKeyScope::Completions)
        );
    }

    #[test]
    fn test_embeddings_scope() {
        assert_eq!(
            required_scope_for_path("/v1/embeddings"),
            Some(ApiKeyScope::Embeddings)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/embeddings"),
            Some(ApiKeyScope::Embeddings)
        );
    }

    #[test]
    fn test_images_scope() {
        assert_eq!(
            required_scope_for_path("/v1/images/generations"),
            Some(ApiKeyScope::Images)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/images/edits"),
            Some(ApiKeyScope::Images)
        );
        assert_eq!(
            required_scope_for_path("/v1/images/variations"),
            Some(ApiKeyScope::Images)
        );
    }

    #[test]
    fn test_videos_scope() {
        assert_eq!(
            required_scope_for_path("/v1/videos"),
            Some(ApiKeyScope::Videos)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/videos/video_abc/content"),
            Some(ApiKeyScope::Videos)
        );
        assert_eq!(
            required_scope_for_path("/v1/videos/characters"),
            Some(ApiKeyScope::Videos)
        );
    }

    #[test]
    fn test_audio_scope() {
        assert_eq!(
            required_scope_for_path("/v1/audio/speech"),
            Some(ApiKeyScope::Audio)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/audio/transcriptions"),
            Some(ApiKeyScope::Audio)
        );
        assert_eq!(
            required_scope_for_path("/v1/audio/translations"),
            Some(ApiKeyScope::Audio)
        );
    }

    #[test]
    fn test_files_scope() {
        assert_eq!(
            required_scope_for_path("/v1/files"),
            Some(ApiKeyScope::Files)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/files/file-123"),
            Some(ApiKeyScope::Files)
        );
        assert_eq!(
            required_scope_for_path("/v1/vector_stores"),
            Some(ApiKeyScope::Files)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/vector_stores/vs-123/files"),
            Some(ApiKeyScope::Files)
        );
    }

    #[test]
    fn test_models_scope() {
        assert_eq!(
            required_scope_for_path("/v1/models"),
            Some(ApiKeyScope::Models)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/models"),
            Some(ApiKeyScope::Models)
        );
        // Future-proofing: model-specific endpoints
        assert_eq!(
            required_scope_for_path("/v1/models/gpt-4o"),
            Some(ApiKeyScope::Models)
        );
        assert_eq!(
            required_scope_for_path("/api/v1/models/claude-3-opus"),
            Some(ApiKeyScope::Models)
        );
    }

    #[test]
    fn test_admin_scope() {
        assert_eq!(
            required_scope_for_path("/admin/v1/organizations"),
            Some(ApiKeyScope::Admin)
        );
        assert_eq!(
            required_scope_for_path("/admin/v1/api-keys"),
            Some(ApiKeyScope::Admin)
        );
        assert_eq!(
            required_scope_for_path("/admin/v1/users/123"),
            Some(ApiKeyScope::Admin)
        );
    }

    #[test]
    fn test_no_scope_required() {
        assert_eq!(required_scope_for_path("/health"), None);
        assert_eq!(required_scope_for_path("/"), None);
        assert_eq!(required_scope_for_path("/docs"), None);
        assert_eq!(required_scope_for_path("/api/docs"), None);
    }

    #[test]
    fn test_query_params_stripped() {
        assert_eq!(
            required_scope_for_path("/v1/chat/completions?foo=bar"),
            Some(ApiKeyScope::Chat)
        );
        assert_eq!(
            required_scope_for_path("/v1/models?filter=gpt"),
            Some(ApiKeyScope::Models)
        );
    }
}
