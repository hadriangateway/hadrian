use axum::{Extension, Json, extract::State};
use axum_valid::Valid;
use chrono::Utc;
use futures_util::StreamExt;
use http::StatusCode;
use serde::{Deserialize, Serialize};
use validator::Validate;

use super::ApiError;
use crate::{
    AppState,
    auth::AuthenticatedRequest,
    config::WebSearchProvider,
    middleware::AuthzContext,
    models::UsageLogEntry,
    pricing::CostPricingSource,
    validation::url::{UrlValidationOptions, validate_base_url_opts},
};

// ─────────────────────────────────────────────────────────────────────────────
// Web Search
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebSearchRequest {
    #[validate(length(min = 1, max = 2000))]
    pub query: String,
    #[validate(range(min = 1, max = 100))]
    pub max_results: Option<usize>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebSearchResponse {
    pub results: Vec<WebSearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// Tavily API request — no Debug derive to avoid leaking `api_key` in logs.
#[derive(Serialize)]
struct TavilySearchRequest {
    query: String,
    max_results: usize,
    api_key: String,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResponse {
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
    score: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ExaSearchRequest {
    query: String,
    num_results: usize,
    contents: ExaContents,
}

#[derive(Debug, Serialize)]
struct ExaContents {
    text: ExaTextOptions,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaTextOptions {
    /// Maximum characters of text to return per result.
    /// Omit to return full text (Exa default).
    #[serde(skip_serializing_if = "Option::is_none")]
    max_characters: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ExaSearchResponse {
    results: Vec<ExaResult>,
}

#[derive(Debug, Deserialize)]
struct ExaResult {
    title: Option<String>,
    url: String,
    text: Option<String>,
    score: Option<f64>,
}

/// Execute a web search against the configured provider (Tavily or Exa).
///
/// This is the core search logic, shared by both the REST endpoint and the
/// server-side web_search tool middleware.
pub async fn execute_web_search(
    client: &reqwest::Client,
    config: &crate::config::WebSearchConfig,
    query: &str,
    max_results: usize,
) -> Result<Vec<WebSearchResult>, WebSearchError> {
    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let max_chars = config.max_content_chars;

    let truncate = |s: String| -> String {
        if max_chars == 0 || s.len() <= max_chars {
            return s;
        }
        let end = s.floor_char_boundary(max_chars);
        format!("{}…[truncated]", &s[..end])
    };

    match config.provider {
        WebSearchProvider::Tavily => {
            let req = TavilySearchRequest {
                query: query.to_string(),
                max_results,
                api_key: config.api_key.clone(),
            };
            let resp = client
                .post("https://api.tavily.com/search")
                .timeout(timeout)
                .json(&req)
                .send()
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "Tavily search request failed");
                    WebSearchError::ProviderRequestFailed
                })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                tracing::error!(status = %status, body = %body, "Tavily API error");
                return Err(WebSearchError::ProviderError);
            }

            let tavily: TavilySearchResponse = resp.json().await.map_err(|e| {
                tracing::error!(error = %e, "Failed to parse Tavily response");
                WebSearchError::InvalidResponse
            })?;

            Ok(tavily
                .results
                .into_iter()
                .map(|r| WebSearchResult {
                    title: r.title,
                    url: r.url,
                    content: truncate(r.content),
                    score: r.score,
                })
                .collect())
        }
        WebSearchProvider::Exa => {
            let req = ExaSearchRequest {
                query: query.to_string(),
                num_results: max_results,
                contents: ExaContents {
                    text: ExaTextOptions {
                        max_characters: if max_chars > 0 { Some(max_chars) } else { None },
                    },
                },
            };
            let resp = client
                .post("https://api.exa.ai/search")
                .timeout(timeout)
                .header("x-api-key", &config.api_key)
                .json(&req)
                .send()
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "Exa search request failed");
                    WebSearchError::ProviderRequestFailed
                })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                tracing::error!(status = %status, body = %body, "Exa API error");
                return Err(WebSearchError::ProviderError);
            }

            let exa: ExaSearchResponse = resp.json().await.map_err(|e| {
                tracing::error!(error = %e, "Failed to parse Exa response");
                WebSearchError::InvalidResponse
            })?;

            Ok(exa
                .results
                .into_iter()
                .map(|r| WebSearchResult {
                    title: r.title.unwrap_or_default(),
                    url: r.url,
                    content: r.text.unwrap_or_default(),
                    score: r.score,
                })
                .collect())
        }
    }
}

/// Errors from the core web search execution.
#[derive(Debug)]
pub enum WebSearchError {
    ProviderRequestFailed,
    ProviderError,
    InvalidResponse,
}

impl std::fmt::Display for WebSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderRequestFailed => write!(f, "Search provider request failed"),
            Self::ProviderError => write!(f, "Search provider returned an error"),
            Self::InvalidResponse => write!(f, "Invalid search provider response"),
        }
    }
}

/// Search the web
///
/// Performs a web search using the configured search provider (Tavily or Exa).
///
/// **Hadrian Extension:** This endpoint is not part of the OpenAI API specification.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/tools/web-search",
    tag = "Tools",
    request_body = WebSearchRequest,
    responses(
        (status = 200, description = "Search results", body = WebSearchResponse),
        (status = 400, description = "Bad request"),
        (status = 404, description = "Web search not configured"),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.tools.web_search", skip(state, auth, authz))]
pub async fn web_search(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<WebSearchRequest>>,
) -> Result<Json<WebSearchResponse>, ApiError> {
    // Authz check
    if let Some(Extension(ref authz)) = authz {
        let org_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.org_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.org_ids.first().cloned()))
        });
        let project_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.project_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.project_ids.first().cloned()))
        });
        authz
            .require_api(
                "tool",
                "execute",
                Some("web_search"),
                None,
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }

    let config = state.config.features.web_search.as_ref().ok_or_else(|| {
        ApiError::new(
            http::StatusCode::NOT_FOUND,
            "feature_not_configured",
            "Web search is not configured",
        )
    })?;

    let max_results = payload
        .max_results
        .unwrap_or(config.max_results)
        .min(config.max_results);

    let results = execute_web_search(&state.http_client, config, &payload.query, max_results)
        .await
        .map_err(|e| {
            ApiError::new(
                http::StatusCode::BAD_GATEWAY,
                "search_provider_error",
                e.to_string(),
            )
        })?;

    let results_count = results.len() as i32;
    let provider_name = match config.provider {
        WebSearchProvider::Tavily => "tavily",
        WebSearchProvider::Exa => "exa",
    };

    // Log tool usage with identity
    #[cfg(feature = "concurrency")]
    if let Some(ref usage_buffer) = state.usage_buffer {
        let (api_key_id, user_id, org_id, project_id, team_id, service_account_id) =
            extract_identity(&auth, &state);
        usage_buffer.push(UsageLogEntry {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id,
            user_id,
            org_id,
            project_id,
            team_id,
            service_account_id,
            model: "web-search".to_string(),
            provider: provider_name.to_string(),
            http_referer: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_microcents: Some(config.cost_microcents_per_request),
            request_at: Utc::now(),
            streamed: false,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: None,
            latency_ms: None,
            cancelled: false,
            status_code: Some(200),
            pricing_source: CostPricingSource::ProviderConfig,
            image_count: None,
            audio_seconds: None,
            character_count: None,
            provider_source: None,
            record_type: "tool".to_string(),
            tool_name: Some("web_search".to_string()),
            tool_query: Some(payload.query),
            tool_url: None,
            tool_bytes_fetched: None,
            tool_results_count: Some(results_count),
            tool_runtime_seconds: None,
            tool_exit_code: None,
        });
    }

    Ok(Json(WebSearchResponse { results }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Web Fetch
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebFetchRequest {
    #[validate(length(min = 1, max = 2083))]
    pub url: String,
    #[validate(range(min = 1, max = 10_485_760))]
    pub max_length: Option<usize>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebFetchResponse {
    pub url: String,
    pub content_type: Option<String>,
    pub content: String,
    pub content_length: usize,
}

/// Fetch a web page
///
/// Fetches a URL and returns its content, optionally stripping HTML tags.
///
/// **Hadrian Extension:** This endpoint is not part of the OpenAI API specification.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/tools/web-fetch",
    tag = "Tools",
    request_body = WebFetchRequest,
    responses(
        (status = 200, description = "Fetched content", body = WebFetchResponse),
        (status = 400, description = "Bad request or blocked URL"),
        (status = 404, description = "Web fetch not configured"),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.tools.web_fetch", skip(state, auth, authz))]
pub async fn web_fetch(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<WebFetchRequest>>,
) -> Result<Json<WebFetchResponse>, ApiError> {
    // Authz check
    if let Some(Extension(ref authz)) = authz {
        let org_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.org_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.org_ids.first().cloned()))
        });
        let project_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.project_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.project_ids.first().cloned()))
        });
        authz
            .require_api(
                "tool",
                "execute",
                Some("web_fetch"),
                None,
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }

    let config = state.config.features.web_fetch.as_ref().ok_or_else(|| {
        ApiError::new(
            http::StatusCode::NOT_FOUND,
            "feature_not_configured",
            "Web fetch is not configured",
        )
    })?;

    if !config.enabled {
        return Err(ApiError::new(
            http::StatusCode::NOT_FOUND,
            "feature_disabled",
            "Web fetch is disabled",
        ));
    }

    // SSRF protection — validate URL and capture resolved IPs to pin the request
    let validated = validate_base_url_opts(
        &payload.url,
        UrlValidationOptions {
            allow_loopback: state.config.server.allow_loopback_urls,
            allow_private: state.config.server.allow_private_urls,
        },
    )
    .map_err(|e| {
        tracing::warn!(url = %payload.url, error = %e, "URL validation failed");
        ApiError::new(
            http::StatusCode::BAD_REQUEST,
            "invalid_url",
            "Invalid or blocked URL",
        )
    })?;

    let max_bytes = payload
        .max_length
        .unwrap_or(config.max_response_bytes)
        .min(config.max_response_bytes);
    let timeout = std::time::Duration::from_secs(config.timeout_secs);

    // Build a one-shot client pinned to the validated IPs to prevent DNS rebinding.
    // redirect/resolve/connect_timeout are unavailable in WASM reqwest.
    let builder = reqwest::Client::builder();
    #[cfg(not(target_arch = "wasm32"))]
    let mut builder = builder
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(config.timeout_secs));
    #[cfg(not(target_arch = "wasm32"))]
    for addr in &validated.addrs {
        builder = builder.resolve(&validated.host, *addr);
    }
    let client = builder.build().map_err(|e| {
        tracing::error!(error = %e, "Failed to build pinned HTTP client");
        ApiError::new(
            http::StatusCode::INTERNAL_SERVER_ERROR,
            "client_error",
            "Failed to build HTTP client",
        )
    })?;

    let resp = client
        .get(&payload.url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, url = %payload.url, "Web fetch request failed");
            ApiError::new(
                http::StatusCode::BAD_GATEWAY,
                "fetch_failed",
                "Failed to fetch URL",
            )
        })?;

    // Reject redirects — the validated URL must be the final destination
    if resp.status().is_redirection() {
        tracing::warn!(
            url = %payload.url,
            status = %resp.status(),
            "URL returned a redirect, which is blocked for SSRF protection"
        );
        return Err(ApiError::new(
            http::StatusCode::BAD_REQUEST,
            "redirect_blocked",
            "URL returned a redirect, which is not allowed",
        ));
    }

    if !resp.status().is_success() {
        let status_code = resp.status().as_u16();
        return Err(ApiError::new(
            http::StatusCode::BAD_GATEWAY,
            "upstream_error",
            format!("URL returned status {status_code}"),
        ));
    }

    let content_type = resp
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Check content type
    if !config.allowed_content_types.is_empty() {
        match content_type {
            Some(ref ct) => {
                let ct_lower = ct.to_lowercase();
                let allowed = config
                    .allowed_content_types
                    .iter()
                    .any(|allowed| ct_lower.starts_with(allowed));
                if !allowed {
                    return Err(ApiError::new(
                        http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                        "unsupported_content_type",
                        format!("Content type '{ct}' is not allowed"),
                    ));
                }
            }
            None => {
                return Err(ApiError::new(
                    http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "unsupported_content_type",
                    "Response has no Content-Type header",
                ));
            }
        }
    }

    // Stream body with byte limit — stop reading once we have enough
    let mut buf = Vec::with_capacity(max_bytes.min(65_536));
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            tracing::error!(error = %e, "Failed to read response body");
            ApiError::new(
                http::StatusCode::BAD_GATEWAY,
                "fetch_failed",
                "Failed to read response body",
            )
        })?;
        buf.extend_from_slice(&chunk);
        if buf.len() >= max_bytes {
            buf.truncate(max_bytes);
            break;
        }
    }

    // UTF-8 safe truncation
    let lossy = String::from_utf8_lossy(&buf);
    let text = if lossy.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !lossy.is_char_boundary(end) {
            end -= 1;
        }
        &lossy[..end]
    } else {
        &lossy
    };
    let bytes_fetched = text.len() as i64;

    // Convert HTML to readable text
    let is_html = content_type
        .as_deref()
        .is_some_and(|ct| ct.starts_with("text/html"));

    let content = if is_html {
        html_to_text(text).await
    } else {
        text.to_string()
    };

    let content_length = content.len();

    // Log tool usage with identity
    #[cfg(feature = "concurrency")]
    if let Some(ref usage_buffer) = state.usage_buffer {
        let (api_key_id, user_id, org_id, project_id, team_id, service_account_id) =
            extract_identity(&auth, &state);
        usage_buffer.push(UsageLogEntry {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id,
            user_id,
            org_id,
            project_id,
            team_id,
            service_account_id,
            model: "web-fetch".to_string(),
            provider: "reqwest".to_string(),
            http_referer: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_microcents: Some(config.cost_microcents_per_request),
            request_at: Utc::now(),
            streamed: false,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: None,
            latency_ms: None,
            cancelled: false,
            status_code: Some(200),
            pricing_source: CostPricingSource::ProviderConfig,
            image_count: None,
            audio_seconds: None,
            character_count: None,
            provider_source: None,
            record_type: "tool".to_string(),
            tool_name: Some("web_fetch".to_string()),
            tool_query: None,
            tool_url: Some(payload.url.clone()),
            tool_bytes_fetched: Some(bytes_fetched),
            tool_results_count: None,
            tool_runtime_seconds: None,
            tool_exit_code: None,
        });
    }

    Ok(Json(WebFetchResponse {
        url: payload.url,
        content_type,
        content,
        content_length,
    }))
}

/// Identity fields extracted from auth context for usage logging.
type IdentityFields = (
    Option<uuid::Uuid>, // api_key_id
    Option<uuid::Uuid>, // user_id
    Option<uuid::Uuid>, // org_id
    Option<uuid::Uuid>, // project_id
    Option<uuid::Uuid>, // team_id
    Option<uuid::Uuid>, // service_account_id
);

/// Extract identity fields from the auth context for usage logging.
fn extract_identity(
    auth: &Option<Extension<AuthenticatedRequest>>,
    state: &AppState,
) -> IdentityFields {
    if let Some(Extension(auth)) = auth {
        let api_key = auth.api_key();
        (
            api_key.map(|k| k.key.id),
            auth.user_id(),
            api_key
                .and_then(|k| k.org_id)
                .or_else(|| auth.principal().org_id()),
            api_key.and_then(|k| k.project_id),
            api_key.and_then(|k| k.team_id),
            api_key.and_then(|k| k.service_account_id),
        )
    } else {
        (
            None,
            state.default_user_id,
            state.default_org_id,
            None,
            None,
            None,
        )
    }
}

/// Convert HTML to readable text.
///
/// When the `document-extraction-full` feature is enabled, uses xberg to
/// convert HTML to well-formatted markdown. Otherwise, falls back to a basic
/// tag-stripping approach.
async fn html_to_text(html: &str) -> String {
    #[cfg(feature = "document-extraction-full")]
    {
        let config = xberg::ExtractionConfig {
            output_format: xberg::OutputFormat::Markdown,
            use_cache: false,
            ..Default::default()
        };
        let input = xberg::ExtractInput::from_bytes(html.as_bytes(), "text/html", None);
        match xberg::extract(input, &config).await {
            Ok(result) => {
                if let Some(document) = result.results.into_iter().next() {
                    return document.content;
                }
                tracing::debug!(
                    "xberg HTML conversion produced no results, falling back to tag stripping"
                );
            }
            Err(e) => {
                tracing::debug!(error = %e, "xberg HTML conversion failed, falling back to tag stripping");
            }
        }
    }

    strip_html_tags(html)
}

/// Basic HTML tag stripping — removes tags and decodes common entities.
///
/// Operates entirely on the lowercased string for tag detection to avoid
/// byte-index misalignment between original and lowercased text.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;

    let lower = html.to_lowercase();
    let mut orig_chars = html.chars();

    for (byte_idx, _) in lower.char_indices() {
        let ch = match orig_chars.next() {
            Some(c) => c,
            None => break,
        };

        if !in_tag && !in_script && !in_style {
            if lower[byte_idx..].starts_with("<script") && is_tag_boundary(&lower, byte_idx + 7) {
                in_script = true;
                in_tag = true;
            } else if lower[byte_idx..].starts_with("<style")
                && is_tag_boundary(&lower, byte_idx + 6)
            {
                in_style = true;
                in_tag = true;
            } else if ch == '<' {
                in_tag = true;
            } else {
                result.push(ch);
            }
        } else if in_script {
            if lower[byte_idx..].starts_with("</script>") {
                in_script = false;
                in_tag = true;
            }
        } else if in_style {
            if lower[byte_idx..].starts_with("</style>") {
                in_style = false;
                in_tag = true;
            }
        } else if in_tag && ch == '>' {
            in_tag = false;
            result.push(' ');
        }
    }

    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        // Collapse whitespace
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns true if position `pos` in `s` is at a tag boundary character
/// (end of string, '>', ' ', '\t', '\n', '/').
fn is_tag_boundary(s: &str, pos: usize) -> bool {
    match s.as_bytes().get(pos) {
        None => true, // end of string
        Some(b) => matches!(b, b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_basic_tags() {
        assert_eq!(strip_html_tags("<p>Hello</p>"), "Hello");
    }

    #[test]
    fn test_strip_nested_tags() {
        assert_eq!(
            strip_html_tags("<div><p>Hello <b>World</b></p></div>"),
            "Hello World"
        );
    }

    #[test]
    fn test_strip_script_content() {
        assert_eq!(
            strip_html_tags("<p>Before</p><script>alert('xss')</script><p>After</p>"),
            "Before After"
        );
    }

    #[test]
    fn test_strip_style_content() {
        assert_eq!(
            strip_html_tags("<p>Before</p><style>body { color: red; }</style><p>After</p>"),
            "Before After"
        );
    }

    #[test]
    fn test_strip_script_with_attributes() {
        assert_eq!(
            strip_html_tags(r#"<script type="text/javascript">var x = 1;</script>text"#),
            "text"
        );
    }

    #[test]
    fn test_scripting_tag_not_stripped() {
        // <scripting> should NOT be treated as <script>
        assert_eq!(strip_html_tags("<scripting>visible</scripting>"), "visible");
    }

    #[test]
    fn test_self_closing_tags() {
        assert_eq!(strip_html_tags("Hello<br/>World"), "Hello World");
    }

    #[test]
    fn test_multibyte_utf8() {
        assert_eq!(strip_html_tags("<p>こんにちは</p>"), "こんにちは");
    }

    #[test]
    fn test_multibyte_utf8_with_script() {
        assert_eq!(
            strip_html_tags("<p>日本語</p><script>var x = '🎉';</script><p>テスト</p>"),
            "日本語 テスト"
        );
    }

    #[test]
    fn test_entity_decoding() {
        assert_eq!(
            strip_html_tags("&amp; &lt; &gt; &quot; &#39; &nbsp;"),
            "& < > \" '"
        );
    }

    #[test]
    fn test_whitespace_collapsing() {
        assert_eq!(strip_html_tags("<p>  Hello   World  </p>"), "Hello World");
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(strip_html_tags(""), "");
    }

    #[test]
    fn test_no_tags() {
        assert_eq!(strip_html_tags("plain text"), "plain text");
    }

    #[test]
    fn test_case_insensitive_script() {
        assert_eq!(
            strip_html_tags("<SCRIPT>alert(1)</SCRIPT>visible"),
            "visible"
        );
    }

    #[test]
    fn test_style_with_newlines() {
        assert_eq!(
            strip_html_tags("<style>\n.foo {\n  color: red;\n}\n</style>content"),
            "content"
        );
    }
}
