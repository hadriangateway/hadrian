//! Unified provider error handling.
//!
//! This module provides a consistent way to translate provider-specific errors
//! into OpenAI-compatible error responses. All providers should use these types
//! and functions to ensure consistent error handling across the gateway.

use axum::{body::Body, response::Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};

/// OpenAI-compatible error types.
///
/// These map to the `type` field in OpenAI's error response format.
/// See: https://platform.openai.com/docs/guides/error-codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiErrorType {
    /// Invalid parameters, malformed request, model not found, validation errors.
    InvalidRequest,
    /// Invalid API key, unauthorized access, permission denied.
    Authentication,
    /// Rate limit exceeded, quota exceeded, too many requests.
    RateLimit,
    /// Internal server error, timeout, service unavailable.
    Server,
    /// Catch-all for other provider-specific errors.
    Api,
}

impl OpenAiErrorType {
    /// Returns the OpenAI error type string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request_error",
            Self::Authentication => "authentication_error",
            Self::RateLimit => "rate_limit_error",
            Self::Server => "server_error",
            Self::Api => "api_error",
        }
    }
}

impl std::fmt::Display for OpenAiErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Provider error information extracted from a provider's error response.
#[derive(Debug, Clone)]
pub struct ProviderErrorInfo {
    /// The OpenAI-compatible error type.
    pub error_type: OpenAiErrorType,
    /// Human-readable error message.
    pub message: String,
    /// Provider-specific error code (will be lowercased in response).
    pub code: String,
}

impl ProviderErrorInfo {
    /// Create a new provider error info.
    pub fn new(
        error_type: OpenAiErrorType,
        message: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            error_type,
            message: message.into(),
            code: code.into(),
        }
    }

    /// Create an invalid request error.
    #[cfg(test)]
    pub fn invalid_request(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::new(OpenAiErrorType::InvalidRequest, message, code)
    }

    /// Create an authentication error.
    #[cfg(test)]
    pub fn authentication(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::new(OpenAiErrorType::Authentication, message, code)
    }

    /// Create a rate limit error.
    #[cfg(test)]
    pub fn rate_limit(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::new(OpenAiErrorType::RateLimit, message, code)
    }

    /// Create a server error.
    #[cfg(test)]
    pub fn server(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::new(OpenAiErrorType::Server, message, code)
    }

    /// Create a generic API error.
    #[cfg(test)]
    pub fn api(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::new(OpenAiErrorType::Api, message, code)
    }
}

/// OpenAI-compatible error response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiErrorResponse {
    pub error: OpenAiErrorBody,
}

/// OpenAI-compatible error body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
}

/// Build an OpenAI-compatible error response from provider error info.
///
/// This function creates a consistent error response format that matches
/// OpenAI's error schema across all providers.
pub fn build_provider_error_response(
    status: StatusCode,
    error_info: ProviderErrorInfo,
) -> Result<Response, super::ProviderError> {
    let response_body = OpenAiErrorResponse {
        error: OpenAiErrorBody {
            message: error_info.message,
            error_type: error_info.error_type.as_str().to_string(),
            code: error_info.code.to_lowercase(),
        },
    };

    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&response_body).unwrap_or_default(),
        ))?)
}

/// Trait for parsing provider-specific error responses.
///
/// Implement this trait for each provider to extract error information
/// from their specific error format.
pub trait ProviderErrorParser {
    /// Parse a provider's error response and extract error information.
    ///
    /// # Arguments
    /// * `status` - The HTTP status code from the provider
    /// * `headers` - Response headers (some providers include error info in headers)
    /// * `body` - The response body bytes
    ///
    /// # Returns
    /// Provider error information suitable for building an OpenAI-compatible response.
    fn parse_error(status: StatusCode, headers: &http::HeaderMap, body: &[u8])
    -> ProviderErrorInfo;
}

#[cfg(feature = "provider-bedrock")]
/// AWS Bedrock error parser.
///
/// Bedrock uses the `x-amzn-errortype` header and a simple `{"message": "..."}` body.
pub struct BedrockErrorParser;

#[cfg(feature = "provider-bedrock")]
impl ProviderErrorParser for BedrockErrorParser {
    fn parse_error(
        _status: StatusCode,
        headers: &http::HeaderMap,
        body: &[u8],
    ) -> ProviderErrorInfo {
        // Extract error type from x-amzn-errortype header (e.g., "ValidationException:http://...")
        let error_type_header = headers
            .get("x-amzn-errortype")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(':').next().unwrap_or(s))
            .unwrap_or("bedrock_error");

        // Map AWS error types to OpenAI error types
        let error_type = match error_type_header {
            "ValidationException" => OpenAiErrorType::InvalidRequest,
            "AccessDeniedException" | "UnrecognizedClientException" => {
                OpenAiErrorType::Authentication
            }
            "ThrottlingException" | "ServiceQuotaExceededException" => OpenAiErrorType::RateLimit,
            "ModelNotReadyException" | "ModelTimeoutException" => OpenAiErrorType::Server,
            "InternalServerException" | "ServiceUnavailableException" => OpenAiErrorType::Server,
            "ResourceNotFoundException" | "ModelNotFoundException" => {
                OpenAiErrorType::InvalidRequest
            }
            _ => OpenAiErrorType::Api,
        };

        // Parse the Bedrock error body
        let bedrock_error: serde_json::Value =
            serde_json::from_slice(body).unwrap_or_else(|_| serde_json::json!({}));

        let message = bedrock_error["message"]
            .as_str()
            .unwrap_or("Unknown Bedrock error")
            .to_string();

        ProviderErrorInfo::new(error_type, message, error_type_header)
    }
}

#[cfg(feature = "provider-vertex")]
/// Google Vertex AI / Gemini error parser.
///
/// Vertex uses a JSON body with `{"error": {"status": "...", "message": "..."}}`.
pub struct VertexErrorParser;

#[cfg(feature = "provider-vertex")]
impl ProviderErrorParser for VertexErrorParser {
    fn parse_error(
        _status: StatusCode,
        _headers: &http::HeaderMap,
        body: &[u8],
    ) -> ProviderErrorInfo {
        // Parse the Vertex error body
        let vertex_error: serde_json::Value =
            serde_json::from_slice(body).unwrap_or_else(|_| serde_json::json!({}));

        let error_obj = &vertex_error["error"];
        let vertex_status = error_obj["status"].as_str().unwrap_or("UNKNOWN");
        let message = error_obj["message"]
            .as_str()
            .unwrap_or("Unknown Vertex AI error")
            .to_string();

        // Map Vertex status to OpenAI error type
        let error_type = match vertex_status {
            "NOT_FOUND" => OpenAiErrorType::InvalidRequest,
            "INVALID_ARGUMENT" | "FAILED_PRECONDITION" => OpenAiErrorType::InvalidRequest,
            "UNAUTHENTICATED" | "PERMISSION_DENIED" => OpenAiErrorType::Authentication,
            "RESOURCE_EXHAUSTED" => OpenAiErrorType::RateLimit,
            "INTERNAL" | "UNAVAILABLE" | "DEADLINE_EXCEEDED" => OpenAiErrorType::Server,
            _ => OpenAiErrorType::Api,
        };

        ProviderErrorInfo::new(error_type, message, vertex_status)
    }
}

/// Anthropic error parser.
///
/// Anthropic uses a JSON body with `{"type": "error", "error": {"type": "...", "message": "..."}}`.
pub struct AnthropicErrorParser;

impl ProviderErrorParser for AnthropicErrorParser {
    fn parse_error(
        _status: StatusCode,
        _headers: &http::HeaderMap,
        body: &[u8],
    ) -> ProviderErrorInfo {
        // Parse the Anthropic error body
        let anthropic_error: serde_json::Value =
            serde_json::from_slice(body).unwrap_or_else(|_| serde_json::json!({}));

        let error_obj = &anthropic_error["error"];
        let anthropic_type = error_obj["type"].as_str().unwrap_or("api_error");
        let message = error_obj["message"]
            .as_str()
            .unwrap_or("Unknown Anthropic error")
            .to_string();

        // Map Anthropic error types to OpenAI error types
        // See: https://docs.anthropic.com/en/api/errors
        let error_type = match anthropic_type {
            "invalid_request_error" => OpenAiErrorType::InvalidRequest,
            "authentication_error" => OpenAiErrorType::Authentication,
            "permission_error" => OpenAiErrorType::Authentication,
            "not_found_error" => OpenAiErrorType::InvalidRequest,
            "rate_limit_error" => OpenAiErrorType::RateLimit,
            "overloaded_error" => OpenAiErrorType::Server,
            "api_error" => OpenAiErrorType::Server,
            _ => OpenAiErrorType::Api,
        };

        ProviderErrorInfo::new(error_type, message, anthropic_type)
    }
}

/// OpenAI-compatible provider error parser.
///
/// Handles the standard OpenAI shape `{"error": {"message": "...", "type": "...",
/// "code": "..."}}` plus aggregator variants: OpenRouter uses a numeric `code`
/// and attaches top-level account fields (e.g. `user_id`) that identify the
/// gateway operator's upstream account and must never be forwarded to clients.
/// Only the message is preserved; everything else is re-derived.
pub struct OpenAiErrorParser;

impl ProviderErrorParser for OpenAiErrorParser {
    fn parse_error(
        status: StatusCode,
        _headers: &http::HeaderMap,
        body: &[u8],
    ) -> ProviderErrorInfo {
        let upstream_error: serde_json::Value =
            serde_json::from_slice(body).unwrap_or_else(|_| serde_json::json!({}));

        let error_obj = &upstream_error["error"];
        let upstream_type = error_obj["type"].as_str();
        let message = error_obj["message"]
            .as_str()
            .unwrap_or("Unknown upstream provider error")
            .to_string();

        let error_type = if let Some(t) = upstream_type {
            match t {
                "invalid_request_error" => OpenAiErrorType::InvalidRequest,
                "authentication_error" => OpenAiErrorType::Authentication,
                "rate_limit_error" => OpenAiErrorType::RateLimit,
                "server_error" => OpenAiErrorType::Server,
                _ => OpenAiErrorType::Api,
            }
        } else {
            match status.as_u16() {
                400 | 404 | 422 => OpenAiErrorType::InvalidRequest,
                401 | 403 => OpenAiErrorType::Authentication,
                429 => OpenAiErrorType::RateLimit,
                500..=599 => OpenAiErrorType::Server,
                _ => OpenAiErrorType::Api,
            }
        };

        // OpenRouter sends `code` as a bare HTTP status number; fall back to a
        // status-derived code so the field is always a meaningful string.
        let code = error_obj["code"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| {
                status
                    .canonical_reason()
                    .unwrap_or("upstream_error")
                    .to_lowercase()
                    .replace(' ', "_")
            });

        ProviderErrorInfo::new(error_type, message, code)
    }
}

#[cfg(feature = "provider-azure")]
/// Azure OpenAI error parser.
///
/// Azure OpenAI uses the same format as OpenAI: `{"error": {"message": "...", "type": "...", "code": "..."}}`.
pub struct AzureOpenAiErrorParser;

#[cfg(feature = "provider-azure")]
impl ProviderErrorParser for AzureOpenAiErrorParser {
    fn parse_error(
        status: StatusCode,
        _headers: &http::HeaderMap,
        body: &[u8],
    ) -> ProviderErrorInfo {
        // Parse the Azure OpenAI error body (same format as OpenAI)
        let azure_error: serde_json::Value =
            serde_json::from_slice(body).unwrap_or_else(|_| serde_json::json!({}));

        let error_obj = &azure_error["error"];
        let azure_type = error_obj["type"].as_str();
        let azure_code = error_obj["code"].as_str().unwrap_or("unknown");
        let message = error_obj["message"]
            .as_str()
            .unwrap_or("Unknown Azure OpenAI error")
            .to_string();

        // Azure may include type from OpenAI format, or we infer from status/code
        let error_type = if let Some(t) = azure_type {
            match t {
                "invalid_request_error" => OpenAiErrorType::InvalidRequest,
                "authentication_error" => OpenAiErrorType::Authentication,
                "rate_limit_error" => OpenAiErrorType::RateLimit,
                "server_error" => OpenAiErrorType::Server,
                _ => OpenAiErrorType::Api,
            }
        } else {
            // Infer from status code if type not present
            match status.as_u16() {
                400 | 404 | 422 => OpenAiErrorType::InvalidRequest,
                401 | 403 => OpenAiErrorType::Authentication,
                429 => OpenAiErrorType::RateLimit,
                500..=599 => OpenAiErrorType::Server,
                _ => OpenAiErrorType::Api,
            }
        };

        ProviderErrorInfo::new(error_type, message, azure_code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_error_type_display() {
        assert_eq!(
            OpenAiErrorType::InvalidRequest.as_str(),
            "invalid_request_error"
        );
        assert_eq!(
            OpenAiErrorType::Authentication.as_str(),
            "authentication_error"
        );
        assert_eq!(OpenAiErrorType::RateLimit.as_str(), "rate_limit_error");
        assert_eq!(OpenAiErrorType::Server.as_str(), "server_error");
        assert_eq!(OpenAiErrorType::Api.as_str(), "api_error");
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_validation() {
        let headers = {
            let mut h = http::HeaderMap::new();
            h.insert("x-amzn-errortype", "ValidationException".parse().unwrap());
            h
        };
        let body = br#"{"message": "Invalid model ID"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::BAD_REQUEST, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.message, "Invalid model ID");
        assert_eq!(info.code, "ValidationException");
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_auth() {
        let headers = {
            let mut h = http::HeaderMap::new();
            h.insert("x-amzn-errortype", "AccessDeniedException".parse().unwrap());
            h
        };
        let body = br#"{"message": "Access denied"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::FORBIDDEN, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_rate_limit() {
        let headers = {
            let mut h = http::HeaderMap::new();
            h.insert("x-amzn-errortype", "ThrottlingException".parse().unwrap());
            h
        };
        let body = br#"{"message": "Rate exceeded"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::TOO_MANY_REQUESTS, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::RateLimit);
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser() {
        let body = br#"{"error": {"status": "INVALID_ARGUMENT", "message": "Bad request"}}"#;

        let info =
            VertexErrorParser::parse_error(StatusCode::BAD_REQUEST, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.message, "Bad request");
        assert_eq!(info.code, "INVALID_ARGUMENT");
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_auth() {
        let body = br#"{"error": {"status": "PERMISSION_DENIED", "message": "Forbidden"}}"#;

        let info =
            VertexErrorParser::parse_error(StatusCode::FORBIDDEN, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
    }

    #[test]
    fn test_anthropic_error_parser() {
        let body = br#"{"type": "error", "error": {"type": "invalid_request_error", "message": "Invalid request"}}"#;

        let info = AnthropicErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.message, "Invalid request");
    }

    #[test]
    fn test_anthropic_error_parser_rate_limit() {
        let body = br#"{"type": "error", "error": {"type": "rate_limit_error", "message": "Too many requests"}}"#;

        let info = AnthropicErrorParser::parse_error(
            StatusCode::TOO_MANY_REQUESTS,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::RateLimit);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_with_type() {
        let body = br#"{"error": {"type": "invalid_request_error", "code": "InvalidModel", "message": "Model not found"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::NOT_FOUND,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.message, "Model not found");
        assert_eq!(info.code, "InvalidModel");
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_from_status() {
        let body = br#"{"error": {"code": "RateLimitExceeded", "message": "Rate limit hit"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::TOO_MANY_REQUESTS,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::RateLimit);
    }

    #[test]
    fn test_build_provider_error_response() {
        let info = ProviderErrorInfo::invalid_request("Invalid model", "ValidationException");
        let response = build_provider_error_response(StatusCode::BAD_REQUEST, info).unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    // ========================================================================
    // Response Body Structure Tests
    // ========================================================================

    #[tokio::test]
    async fn test_build_provider_error_response_json_structure() {
        let info = ProviderErrorInfo::new(
            OpenAiErrorType::InvalidRequest,
            "Model not found",
            "ModelNotFoundException",
        );
        let response = build_provider_error_response(StatusCode::NOT_FOUND, info).unwrap();

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: OpenAiErrorResponse = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(parsed.error.message, "Model not found");
        assert_eq!(parsed.error.error_type, "invalid_request_error");
        assert_eq!(parsed.error.code, "modelnotfoundexception"); // lowercased
    }

    #[tokio::test]
    async fn test_build_provider_error_response_code_lowercasing() {
        // Verify that codes with mixed case are lowercased in the response
        let test_cases = [
            ("ValidationException", "validationexception"),
            ("INVALID_ARGUMENT", "invalid_argument"),
            ("RateLimitExceeded", "ratelimitexceeded"),
            ("already_lowercase", "already_lowercase"),
            ("MixedCASE_With_UNDERSCORES", "mixedcase_with_underscores"),
        ];

        for (input_code, expected_code) in test_cases {
            let info = ProviderErrorInfo::api("test message", input_code);
            let response = build_provider_error_response(StatusCode::BAD_REQUEST, info).unwrap();

            let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let parsed: OpenAiErrorResponse = serde_json::from_slice(&body_bytes).unwrap();

            assert_eq!(
                parsed.error.code, expected_code,
                "Code '{}' should be lowercased to '{}'",
                input_code, expected_code
            );
        }
    }

    // ========================================================================
    // Bedrock Parser - Complete Error Type Coverage
    // ========================================================================

    #[cfg(feature = "provider-bedrock")]
    fn bedrock_headers(error_type: &str) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        h.insert("x-amzn-errortype", error_type.parse().unwrap());
        h
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_unrecognized_client() {
        let headers = bedrock_headers("UnrecognizedClientException");
        let body = br#"{"message": "Unrecognized client"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::UNAUTHORIZED, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
        assert_eq!(info.code, "UnrecognizedClientException");
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_service_quota() {
        let headers = bedrock_headers("ServiceQuotaExceededException");
        let body = br#"{"message": "Quota exceeded"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::TOO_MANY_REQUESTS, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::RateLimit);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_model_not_ready() {
        let headers = bedrock_headers("ModelNotReadyException");
        let body = br#"{"message": "Model is not ready"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::SERVICE_UNAVAILABLE, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_model_timeout() {
        let headers = bedrock_headers("ModelTimeoutException");
        let body = br#"{"message": "Model timed out"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::GATEWAY_TIMEOUT, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_internal_server() {
        let headers = bedrock_headers("InternalServerException");
        let body = br#"{"message": "Internal error"}"#;

        let info =
            BedrockErrorParser::parse_error(StatusCode::INTERNAL_SERVER_ERROR, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_service_unavailable() {
        let headers = bedrock_headers("ServiceUnavailableException");
        let body = br#"{"message": "Service unavailable"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::SERVICE_UNAVAILABLE, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_resource_not_found() {
        let headers = bedrock_headers("ResourceNotFoundException");
        let body = br#"{"message": "Resource not found"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::NOT_FOUND, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_model_not_found() {
        let headers = bedrock_headers("ModelNotFoundException");
        let body = br#"{"message": "Model not found"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::NOT_FOUND, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_unknown_type() {
        let headers = bedrock_headers("SomeNewException");
        let body = br#"{"message": "Unknown error"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::BAD_REQUEST, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Api);
        assert_eq!(info.code, "SomeNewException");
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_header_with_url_suffix() {
        // AWS sometimes includes URL suffix in the header: "ValidationException:http://..."
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-amzn-errortype",
            "ValidationException:http://internal.amazonaws.com/doc/2023-09-30/"
                .parse()
                .unwrap(),
        );
        let body = br#"{"message": "Invalid"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::BAD_REQUEST, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.code, "ValidationException");
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_missing_header() {
        let headers = http::HeaderMap::new();
        let body = br#"{"message": "Error without header"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::BAD_REQUEST, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::Api);
        assert_eq!(info.code, "bedrock_error");
    }

    // ========================================================================
    // Vertex Parser - Complete Error Type Coverage
    // ========================================================================

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_not_found() {
        let body = br#"{"error": {"status": "NOT_FOUND", "message": "Model not found"}}"#;

        let info =
            VertexErrorParser::parse_error(StatusCode::NOT_FOUND, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.code, "NOT_FOUND");
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_failed_precondition() {
        let body =
            br#"{"error": {"status": "FAILED_PRECONDITION", "message": "Precondition failed"}}"#;

        let info =
            VertexErrorParser::parse_error(StatusCode::BAD_REQUEST, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_unauthenticated() {
        let body = br#"{"error": {"status": "UNAUTHENTICATED", "message": "Invalid credentials"}}"#;

        let info =
            VertexErrorParser::parse_error(StatusCode::UNAUTHORIZED, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_resource_exhausted() {
        let body = br#"{"error": {"status": "RESOURCE_EXHAUSTED", "message": "Quota exceeded"}}"#;

        let info = VertexErrorParser::parse_error(
            StatusCode::TOO_MANY_REQUESTS,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::RateLimit);
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_internal() {
        let body = br#"{"error": {"status": "INTERNAL", "message": "Internal error"}}"#;

        let info = VertexErrorParser::parse_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_unavailable() {
        let body = br#"{"error": {"status": "UNAVAILABLE", "message": "Service unavailable"}}"#;

        let info = VertexErrorParser::parse_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_deadline_exceeded() {
        let body = br#"{"error": {"status": "DEADLINE_EXCEEDED", "message": "Request timed out"}}"#;

        let info = VertexErrorParser::parse_error(
            StatusCode::GATEWAY_TIMEOUT,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_unknown_status() {
        let body = br#"{"error": {"status": "SOME_NEW_STATUS", "message": "Unknown"}}"#;

        let info =
            VertexErrorParser::parse_error(StatusCode::BAD_REQUEST, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::Api);
        assert_eq!(info.code, "SOME_NEW_STATUS");
    }

    // ========================================================================
    // Anthropic Parser - Complete Error Type Coverage
    // ========================================================================

    #[test]
    fn test_anthropic_error_parser_authentication() {
        let body = br#"{"type": "error", "error": {"type": "authentication_error", "message": "Invalid API key"}}"#;

        let info = AnthropicErrorParser::parse_error(
            StatusCode::UNAUTHORIZED,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
        assert_eq!(info.code, "authentication_error");
    }

    #[test]
    fn test_anthropic_error_parser_permission() {
        let body = br#"{"type": "error", "error": {"type": "permission_error", "message": "Permission denied"}}"#;

        let info =
            AnthropicErrorParser::parse_error(StatusCode::FORBIDDEN, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
        assert_eq!(info.code, "permission_error");
    }

    #[test]
    fn test_anthropic_error_parser_not_found() {
        let body = br#"{"type": "error", "error": {"type": "not_found_error", "message": "Resource not found"}}"#;

        let info =
            AnthropicErrorParser::parse_error(StatusCode::NOT_FOUND, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.code, "not_found_error");
    }

    #[test]
    fn test_anthropic_error_parser_overloaded() {
        let body = br#"{"type": "error", "error": {"type": "overloaded_error", "message": "API is overloaded"}}"#;

        let info = AnthropicErrorParser::parse_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
        assert_eq!(info.code, "overloaded_error");
    }

    #[test]
    fn test_anthropic_error_parser_api_error() {
        let body =
            br#"{"type": "error", "error": {"type": "api_error", "message": "Internal error"}}"#;

        let info = AnthropicErrorParser::parse_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
        assert_eq!(info.code, "api_error");
    }

    #[test]
    fn test_anthropic_error_parser_unknown_type() {
        let body =
            br#"{"type": "error", "error": {"type": "new_error_type", "message": "New error"}}"#;

        let info = AnthropicErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Api);
        assert_eq!(info.code, "new_error_type");
    }

    // ========================================================================
    // Azure Parser - Complete Error Type Coverage
    // ========================================================================

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_authentication_type() {
        let body = br#"{"error": {"type": "authentication_error", "code": "Unauthorized", "message": "Invalid key"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::UNAUTHORIZED,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_rate_limit_type() {
        let body =
            br#"{"error": {"type": "rate_limit_error", "code": "429", "message": "Rate limited"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::TOO_MANY_REQUESTS,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::RateLimit);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_server_type() {
        let body = br#"{"error": {"type": "server_error", "code": "InternalError", "message": "Server error"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_unknown_type_string() {
        let body =
            br#"{"error": {"type": "some_new_type", "code": "NewCode", "message": "New error"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Api);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_400() {
        let body = br#"{"error": {"code": "BadRequest", "message": "Bad request"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_401() {
        let body = br#"{"error": {"code": "Unauthorized", "message": "Unauthorized"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::UNAUTHORIZED,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_403() {
        let body = br#"{"error": {"code": "Forbidden", "message": "Forbidden"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::FORBIDDEN,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Authentication);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_422() {
        let body = br#"{"error": {"code": "UnprocessableEntity", "message": "Invalid params"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_500() {
        let body = br#"{"error": {"code": "InternalError", "message": "Internal error"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_503() {
        let body =
            br#"{"error": {"code": "ServiceUnavailable", "message": "Service unavailable"}}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_infer_unknown_status() {
        let body = br#"{"error": {"code": "Unknown", "message": "Unknown error"}}"#;

        // 418 I'm a teapot - not a standard error code
        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::IM_A_TEAPOT,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Api);
    }

    // ========================================================================
    // Edge Cases - Malformed Input Handling
    // ========================================================================

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_malformed_json() {
        let headers = bedrock_headers("ValidationException");
        let body = b"not valid json";

        let info = BedrockErrorParser::parse_error(StatusCode::BAD_REQUEST, &headers, body);
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest);
        assert_eq!(info.message, "Unknown Bedrock error");
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_empty_body() {
        let headers = bedrock_headers("ValidationException");
        let body = b"";

        let info = BedrockErrorParser::parse_error(StatusCode::BAD_REQUEST, &headers, body);
        assert_eq!(info.message, "Unknown Bedrock error");
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_bedrock_error_parser_missing_message_field() {
        let headers = bedrock_headers("ValidationException");
        let body = br#"{"error": "some error"}"#;

        let info = BedrockErrorParser::parse_error(StatusCode::BAD_REQUEST, &headers, body);
        assert_eq!(info.message, "Unknown Bedrock error");
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_malformed_json() {
        let body = b"not valid json";

        let info =
            VertexErrorParser::parse_error(StatusCode::BAD_REQUEST, &http::HeaderMap::new(), body);
        assert_eq!(info.error_type, OpenAiErrorType::Api);
        assert_eq!(info.message, "Unknown Vertex AI error");
        assert_eq!(info.code, "UNKNOWN");
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_empty_body() {
        let body = b"";

        let info =
            VertexErrorParser::parse_error(StatusCode::BAD_REQUEST, &http::HeaderMap::new(), body);
        assert_eq!(info.message, "Unknown Vertex AI error");
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_error_parser_missing_error_object() {
        let body = br#"{"status": "ERROR"}"#;

        let info =
            VertexErrorParser::parse_error(StatusCode::BAD_REQUEST, &http::HeaderMap::new(), body);
        assert_eq!(info.code, "UNKNOWN");
    }

    #[test]
    fn test_anthropic_error_parser_malformed_json() {
        let body = b"not valid json";

        let info = AnthropicErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server); // Falls back to api_error mapping
        assert_eq!(info.message, "Unknown Anthropic error");
    }

    #[test]
    fn test_anthropic_error_parser_empty_body() {
        let body = b"";

        let info = AnthropicErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.message, "Unknown Anthropic error");
    }

    #[test]
    fn test_anthropic_error_parser_missing_error_object() {
        let body = br#"{"type": "error"}"#;

        let info = AnthropicErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.code, "api_error"); // Default when type is missing
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_malformed_json() {
        let body = b"not valid json";

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::InvalidRequest); // Inferred from 400
        assert_eq!(info.message, "Unknown Azure OpenAI error");
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_empty_body() {
        let body = b"";

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.error_type, OpenAiErrorType::Server);
        assert_eq!(info.code, "unknown");
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_error_parser_missing_error_object() {
        let body = br#"{"message": "error outside error object"}"#;

        let info = AzureOpenAiErrorParser::parse_error(
            StatusCode::BAD_REQUEST,
            &http::HeaderMap::new(),
            body,
        );
        assert_eq!(info.code, "unknown");
    }

    // ========================================================================
    // Cross-Provider Consistency Tests
    // ========================================================================

    /// Verifies that equivalent error scenarios across all providers produce
    /// the same OpenAI error type, ensuring consistent client experience.
    #[cfg(all(
        feature = "provider-bedrock",
        feature = "provider-vertex",
        feature = "provider-azure"
    ))]
    mod cross_provider_consistency {
        use super::*;

        #[test]
        fn test_invalid_request_consistency() {
            // All providers should map validation/bad request errors to InvalidRequest
            let bedrock = BedrockErrorParser::parse_error(
                StatusCode::BAD_REQUEST,
                &bedrock_headers("ValidationException"),
                br#"{"message": "Invalid"}"#,
            );
            let vertex = VertexErrorParser::parse_error(
                StatusCode::BAD_REQUEST,
                &http::HeaderMap::new(),
                br#"{"error": {"status": "INVALID_ARGUMENT", "message": "Invalid"}}"#,
            );
            let anthropic = AnthropicErrorParser::parse_error(
                StatusCode::BAD_REQUEST,
                &http::HeaderMap::new(),
                br#"{"type": "error", "error": {"type": "invalid_request_error", "message": "Invalid"}}"#,
            );
            let azure = AzureOpenAiErrorParser::parse_error(
                StatusCode::BAD_REQUEST,
                &http::HeaderMap::new(),
                br#"{"error": {"type": "invalid_request_error", "code": "invalid", "message": "Invalid"}}"#,
            );

            assert_eq!(bedrock.error_type, OpenAiErrorType::InvalidRequest);
            assert_eq!(vertex.error_type, OpenAiErrorType::InvalidRequest);
            assert_eq!(anthropic.error_type, OpenAiErrorType::InvalidRequest);
            assert_eq!(azure.error_type, OpenAiErrorType::InvalidRequest);
        }

        #[test]
        fn test_authentication_consistency() {
            // All providers should map auth errors to Authentication
            let bedrock = BedrockErrorParser::parse_error(
                StatusCode::FORBIDDEN,
                &bedrock_headers("AccessDeniedException"),
                br#"{"message": "Access denied"}"#,
            );
            let vertex = VertexErrorParser::parse_error(
                StatusCode::FORBIDDEN,
                &http::HeaderMap::new(),
                br#"{"error": {"status": "PERMISSION_DENIED", "message": "Denied"}}"#,
            );
            let anthropic = AnthropicErrorParser::parse_error(
                StatusCode::UNAUTHORIZED,
                &http::HeaderMap::new(),
                br#"{"type": "error", "error": {"type": "authentication_error", "message": "Invalid key"}}"#,
            );
            let azure = AzureOpenAiErrorParser::parse_error(
                StatusCode::UNAUTHORIZED,
                &http::HeaderMap::new(),
                br#"{"error": {"code": "Unauthorized", "message": "Unauthorized"}}"#,
            );

            assert_eq!(bedrock.error_type, OpenAiErrorType::Authentication);
            assert_eq!(vertex.error_type, OpenAiErrorType::Authentication);
            assert_eq!(anthropic.error_type, OpenAiErrorType::Authentication);
            assert_eq!(azure.error_type, OpenAiErrorType::Authentication);
        }

        #[test]
        fn test_rate_limit_consistency() {
            // All providers should map rate limit errors to RateLimit
            let bedrock = BedrockErrorParser::parse_error(
                StatusCode::TOO_MANY_REQUESTS,
                &bedrock_headers("ThrottlingException"),
                br#"{"message": "Throttled"}"#,
            );
            let vertex = VertexErrorParser::parse_error(
                StatusCode::TOO_MANY_REQUESTS,
                &http::HeaderMap::new(),
                br#"{"error": {"status": "RESOURCE_EXHAUSTED", "message": "Quota exceeded"}}"#,
            );
            let anthropic = AnthropicErrorParser::parse_error(
                StatusCode::TOO_MANY_REQUESTS,
                &http::HeaderMap::new(),
                br#"{"type": "error", "error": {"type": "rate_limit_error", "message": "Rate limited"}}"#,
            );
            let azure = AzureOpenAiErrorParser::parse_error(
                StatusCode::TOO_MANY_REQUESTS,
                &http::HeaderMap::new(),
                br#"{"error": {"code": "RateLimitExceeded", "message": "Rate limited"}}"#,
            );

            assert_eq!(bedrock.error_type, OpenAiErrorType::RateLimit);
            assert_eq!(vertex.error_type, OpenAiErrorType::RateLimit);
            assert_eq!(anthropic.error_type, OpenAiErrorType::RateLimit);
            assert_eq!(azure.error_type, OpenAiErrorType::RateLimit);
        }

        #[test]
        fn test_server_error_consistency() {
            // All providers should map server/internal errors to Server
            let bedrock = BedrockErrorParser::parse_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &bedrock_headers("InternalServerException"),
                br#"{"message": "Internal error"}"#,
            );
            let vertex = VertexErrorParser::parse_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &http::HeaderMap::new(),
                br#"{"error": {"status": "INTERNAL", "message": "Internal error"}}"#,
            );
            let anthropic = AnthropicErrorParser::parse_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &http::HeaderMap::new(),
                br#"{"type": "error", "error": {"type": "api_error", "message": "Internal error"}}"#,
            );
            let azure = AzureOpenAiErrorParser::parse_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &http::HeaderMap::new(),
                br#"{"error": {"code": "InternalError", "message": "Internal error"}}"#,
            );

            assert_eq!(bedrock.error_type, OpenAiErrorType::Server);
            assert_eq!(vertex.error_type, OpenAiErrorType::Server);
            assert_eq!(anthropic.error_type, OpenAiErrorType::Server);
            assert_eq!(azure.error_type, OpenAiErrorType::Server);
        }

        #[test]
        fn test_model_not_found_consistency() {
            // All providers should map model/resource not found to InvalidRequest
            let bedrock = BedrockErrorParser::parse_error(
                StatusCode::NOT_FOUND,
                &bedrock_headers("ModelNotFoundException"),
                br#"{"message": "Model not found"}"#,
            );
            let vertex = VertexErrorParser::parse_error(
                StatusCode::NOT_FOUND,
                &http::HeaderMap::new(),
                br#"{"error": {"status": "NOT_FOUND", "message": "Model not found"}}"#,
            );
            let anthropic = AnthropicErrorParser::parse_error(
                StatusCode::NOT_FOUND,
                &http::HeaderMap::new(),
                br#"{"type": "error", "error": {"type": "not_found_error", "message": "Model not found"}}"#,
            );
            let azure = AzureOpenAiErrorParser::parse_error(
                StatusCode::NOT_FOUND,
                &http::HeaderMap::new(),
                br#"{"error": {"code": "DeploymentNotFound", "message": "Model not found"}}"#,
            );

            assert_eq!(bedrock.error_type, OpenAiErrorType::InvalidRequest);
            assert_eq!(vertex.error_type, OpenAiErrorType::InvalidRequest);
            assert_eq!(anthropic.error_type, OpenAiErrorType::InvalidRequest);
            assert_eq!(azure.error_type, OpenAiErrorType::InvalidRequest);
        }
    }

    // ========================================================================
    // ProviderErrorInfo Helper Tests
    // ========================================================================

    #[test]
    fn test_provider_error_info_constructors() {
        let invalid = ProviderErrorInfo::invalid_request("msg", "code");
        assert_eq!(invalid.error_type, OpenAiErrorType::InvalidRequest);

        let auth = ProviderErrorInfo::authentication("msg", "code");
        assert_eq!(auth.error_type, OpenAiErrorType::Authentication);

        let rate = ProviderErrorInfo::rate_limit("msg", "code");
        assert_eq!(rate.error_type, OpenAiErrorType::RateLimit);

        let server = ProviderErrorInfo::server("msg", "code");
        assert_eq!(server.error_type, OpenAiErrorType::Server);

        let api = ProviderErrorInfo::api("msg", "code");
        assert_eq!(api.error_type, OpenAiErrorType::Api);
    }

    #[test]
    fn test_openai_error_type_display_trait() {
        assert_eq!(
            format!("{}", OpenAiErrorType::InvalidRequest),
            "invalid_request_error"
        );
        assert_eq!(
            format!("{}", OpenAiErrorType::Authentication),
            "authentication_error"
        );
        assert_eq!(
            format!("{}", OpenAiErrorType::RateLimit),
            "rate_limit_error"
        );
        assert_eq!(format!("{}", OpenAiErrorType::Server), "server_error");
        assert_eq!(format!("{}", OpenAiErrorType::Api), "api_error");
    }
}
