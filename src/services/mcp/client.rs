//! Thin Hadrian-friendly wrapper around [`rmcp`]'s Streamable-HTTP
//! client. We hide rmcp's `Cow<'static, str>` / `JsonObject` /
//! `Annotated<RawContent>` types behind plain serde JSON so the rest
//! of Hadrian doesn't need to know about the MCP SDK's internals.

use std::sync::Arc;

use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, Content, RawContent},
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use thiserror::Error;

/// One tool advertised by a remote MCP server. The shape mirrors
/// [`api_types::responses::McpListedTool`](crate::api_types::responses::McpListedTool)
/// so the executor can re-emit the catalog as an `mcp_list_tools`
/// item without re-deriving fields.
#[derive(Debug, Clone)]
pub struct McpToolMeta {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    /// Raw annotations forwarded from the server (read-only hint,
    /// idempotency, etc.). `None` if the server didn't supply any.
    pub annotations: Option<serde_json::Value>,
}

impl McpToolMeta {
    fn from_rmcp(tool: rmcp::model::Tool) -> Self {
        let input_schema =
            serde_json::to_value(tool.input_schema.as_ref()).unwrap_or(serde_json::json!({}));
        let annotations = tool
            .annotations
            .as_ref()
            .and_then(|a| serde_json::to_value(a).ok());
        Self {
            name: tool.name.into_owned(),
            description: tool.description.map(|d| d.into_owned()),
            input_schema,
            annotations,
        }
    }
}

/// Hadrian-side projection of `rmcp::model::CallToolResult`. We flatten
/// the content array down to a single text payload (the model only
/// consumes text), preserve a structured-content fallback, and surface
/// `is_error` so the executor can populate `mcp_call.error`.
#[derive(Debug, Clone)]
pub struct McpCallResult {
    /// True iff the server returned `isError = true`.
    pub is_error: bool,
    /// Text content concatenated in order. Empty when the server
    /// returned only non-text content (images, etc.).
    pub text: String,
    /// Optional `structuredContent` from the result (JSON object the
    /// server attached for richer consumers).
    pub structured_content: Option<serde_json::Value>,
    /// Non-text content blocks the server returned, kept verbatim so
    /// callers can render images / resources if they choose to.
    pub extra_content: Vec<McpCallContent>,
}

/// Non-text content block from a tool result, captured verbatim so
/// future consumers (e.g. image rendering in the chat UI) can use it
/// without going through rmcp.
#[derive(Debug, Clone)]
pub struct McpCallContent {
    /// Content kind: `"image"`, `"audio"`, `"resource"`, `"resource_link"`.
    pub kind: String,
    /// Raw JSON of the content block.
    pub value: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum McpClientError {
    #[error("MCP transport setup failed: {0}")]
    Transport(String),
    #[error("MCP handshake (initialize) failed: {0}")]
    Initialize(String),
    #[error("MCP `tools/list` failed: {0}")]
    ListTools(String),
    #[error("MCP `tools/call` ({tool}) failed: {message}")]
    CallTool { tool: String, message: String },
    #[error("MCP arguments must serialize to a JSON object, got: {0}")]
    NonObjectArguments(String),
}

/// A live MCP client session. Wraps `rmcp`'s `RunningService` so the
/// connection stays open across multiple `call_tool` invocations
/// during a single response.
///
/// One `McpClient` represents one `(server_url, authorization)` pair.
/// [`McpService`](super::service::McpService) keys a `DashMap` of these
/// (behind `Arc`) for cross-request reuse; `list_tools` / `call_tool`
/// take `&self`, so the pooled handle is shared without a mutex.
pub struct McpClient {
    service: Option<RunningService<RoleClient, ()>>,
    server_url: Arc<str>,
}

impl McpClient {
    /// Open a Streamable HTTP connection to `server_url`, send
    /// `initialize`, and return a ready-to-use client.
    ///
    /// `authorization` is sent verbatim in the `Authorization` HTTP
    /// header on every request — caller is responsible for the
    /// `Bearer ` prefix (or whatever the upstream expects).
    /// `headers` are merged into every request.
    pub async fn connect(
        server_url: impl Into<Arc<str>>,
        authorization: Option<String>,
        headers: std::collections::HashMap<String, String>,
    ) -> Result<Self, McpClientError> {
        let server_url: Arc<str> = server_url.into();

        let mut custom_headers = std::collections::HashMap::new();
        for (k, v) in headers {
            let name = http::HeaderName::try_from(k.as_str()).map_err(|e| {
                McpClientError::Transport(format!("invalid header name '{k}': {e}"))
            })?;
            let value = http::HeaderValue::try_from(v.as_str()).map_err(|e| {
                McpClientError::Transport(format!("invalid header value for '{k}': {e}"))
            })?;
            custom_headers.insert(name, value);
        }

        let mut cfg = StreamableHttpClientTransportConfig::with_uri(server_url.clone());
        if let Some(auth) = authorization {
            cfg = cfg.auth_header(strip_bearer_prefix(&auth));
        }
        cfg = cfg.custom_headers(custom_headers);

        let transport = StreamableHttpClientTransport::from_config(cfg);
        let service =
            ().serve(transport)
                .await
                .map_err(|e| McpClientError::Initialize(e.to_string()))?;

        Ok(Self {
            service: Some(service),
            server_url,
        })
    }

    /// URL of the connected MCP server.
    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    /// Fetch the full tool catalog (auto-paginated via
    /// `list_all_tools`).
    pub async fn list_tools(&self) -> Result<Vec<McpToolMeta>, McpClientError> {
        let svc = self
            .service
            .as_ref()
            .ok_or_else(|| McpClientError::ListTools("client already closed".to_string()))?;
        let tools = svc
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| McpClientError::ListTools(e.to_string()))?;
        Ok(tools.into_iter().map(McpToolMeta::from_rmcp).collect())
    }

    /// Invoke a tool by name. `arguments` must serialize to a JSON
    /// object — MCP doesn't accept arrays or scalars at the top level.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpCallResult, McpClientError> {
        let svc = self
            .service
            .as_ref()
            .ok_or_else(|| McpClientError::CallTool {
                tool: name.to_string(),
                message: "client already closed".to_string(),
            })?;

        let args_obj = match arguments {
            serde_json::Value::Null => None,
            serde_json::Value::Object(map) => Some(map),
            other => {
                return Err(McpClientError::NonObjectArguments(other.to_string()));
            }
        };

        let mut params = CallToolRequestParams::new(name.to_string());
        params.arguments = args_obj;

        let result = svc
            .peer()
            .call_tool(params)
            .await
            .map_err(|e| McpClientError::CallTool {
                tool: name.to_string(),
                message: e.to_string(),
            })?;

        Ok(flatten_call_result(result))
    }

    /// Gracefully close the connection. Idempotent — safe to call
    /// once after pool eviction or once before drop.
    pub async fn close(mut self) -> Result<(), McpClientError> {
        if let Some(svc) = self.service.take() {
            svc.cancel()
                .await
                .map_err(|e| McpClientError::Transport(e.to_string()))?;
        }
        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Some(svc) = self.service.take() {
            // RunningService logs a warning if dropped without explicit
            // close — avoid the noise for the common case (pool TTL
            // eviction can't await). Spawn a detached cancel.
            tokio::spawn(async move {
                let _ = svc.cancel().await;
            });
        }
    }
}

/// If `value` starts with `Bearer ` (case-insensitive), drop the
/// prefix. rmcp prepends `Bearer ` itself; double-prefixing would
/// produce `Authorization: Bearer Bearer …` which servers reject.
fn strip_bearer_prefix(value: &str) -> String {
    let trimmed = value.trim_start();
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") {
        trimmed[7..].trim_start().to_string()
    } else {
        trimmed.to_string()
    }
}

fn flatten_call_result(result: rmcp::model::CallToolResult) -> McpCallResult {
    let mut text_parts = Vec::new();
    let mut extra = Vec::new();

    for content in result.content {
        match flatten_content(content) {
            FlattenedContent::Text(t) => text_parts.push(t),
            FlattenedContent::Other(kind, value) => extra.push(McpCallContent { kind, value }),
        }
    }

    McpCallResult {
        is_error: result.is_error.unwrap_or(false),
        text: text_parts.join("\n"),
        structured_content: result.structured_content,
        extra_content: extra,
    }
}

enum FlattenedContent {
    Text(String),
    Other(String, serde_json::Value),
}

fn flatten_content(content: Content) -> FlattenedContent {
    match content.raw {
        RawContent::Text(t) => FlattenedContent::Text(t.text),
        RawContent::Image(img) => {
            FlattenedContent::Other("image".to_string(), to_value_or_null("image", &img))
        }
        RawContent::Audio(a) => {
            FlattenedContent::Other("audio".to_string(), to_value_or_null("audio", &a))
        }
        RawContent::Resource(r) => {
            FlattenedContent::Other("resource".to_string(), to_value_or_null("resource", &r))
        }
        RawContent::ResourceLink(r) => FlattenedContent::Other(
            "resource_link".to_string(),
            to_value_or_null("resource_link", &r),
        ),
    }
}

/// Serialize an MCP content block to JSON, logging (rather than silently
/// swallowing) a failure and falling back to `null` so one bad block
/// doesn't drop the whole result.
fn to_value_or_null<T: serde::Serialize>(kind: &str, value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|e| {
        tracing::warn!(
            content_kind = kind,
            error = %e,
            "Failed to serialize MCP {kind} content block; dropping it (null)"
        );
        serde_json::Value::Null
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_bearer_handles_case_insensitive() {
        assert_eq!(strip_bearer_prefix("Bearer xyz"), "xyz");
        assert_eq!(strip_bearer_prefix("BEARER xyz"), "xyz");
        assert_eq!(strip_bearer_prefix("bearer xyz"), "xyz");
        assert_eq!(strip_bearer_prefix("xyz"), "xyz");
        assert_eq!(strip_bearer_prefix("  Bearer  xyz"), "xyz");
    }
}
