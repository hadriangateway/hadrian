//! MCP (Model Context Protocol) tool preprocessing.
//!
//! Validate `{"type": "mcp", ...}` tool entries on a `/v1/responses`
//! payload against the operator config and the resolved provider.
//! Under `mode = passthrough_openai` the entries are forwarded
//! verbatim to OpenAI / Azure OpenAI; under `mode = hadrian_hosted`
//! the rewrite in [`super::mcp::preprocess`] turns them into per-tool
//! function tools so any provider can drive the loop. Either way,
//! invalid shapes are rejected with a clear 400.
//!
//! Caller-supplied credentials only — the `authorization` field on
//! `McpTool` is treated as opaque and never persisted by the gateway.
//! See `docs/content/docs/features/mcp.mdx`.
//!
//! Pre-admission lives in `routes/api/chat.rs::api_v1_responses`, next
//! to the existing `resolve_shell_environment` block. Failures map to
//! HTTP 400 via `ApiError`.

#![cfg(not(target_arch = "wasm32"))]

use crate::{
    api_types::responses::{
        CreateResponsesPayload, McpTool, ResponsesToolDefinition, is_known_mcp_connector_id,
    },
    config::{McpConfig, McpMode, ProviderConfig},
};

/// Errors raised by `preprocess_mcp_tools`. All map to HTTP 400 with
/// the variant's `error_code` and the `Display` form as message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpToolError {
    #[error("MCP tool requires exactly one of `server_url` or `connector_id` (server_label: {0})")]
    InvalidTarget(String),

    #[error(
        "MCP tool uses `connector_id` but `[features.mcp].allow_connector_ids` is false \
         (server_label: {0})"
    )]
    ConnectorIdNotAllowed(String),

    #[error(
        "MCP `connector_id` '{connector_id}' is not a known OpenAI connector \
         (server_label: {server_label})"
    )]
    InvalidConnectorId {
        server_label: String,
        connector_id: String,
    },

    #[error(
        "MCP tool `server_url` '{server_url}' is not in `[features.mcp].allowed_server_urls` \
         (server_label: {server_label})"
    )]
    ServerUrlNotAllowed {
        server_label: String,
        server_url: String,
    },

    #[error(
        "MCP `mode = passthrough_openai` requires an OpenAI or Azure OpenAI upstream; \
         resolved provider is {0} (server_label: {1})"
    )]
    PassthroughRequiresOpenAi(&'static str, String),

    #[error(
        "MCP `mode = hadrian_hosted` requires the gateway to be built with the `mcp` \
         cargo feature (server_label: {0})"
    )]
    HadrianHostedNotImplemented(String),

    #[error("MCP tool is present but `[features.mcp]` is not enabled (server_label: {0})")]
    Disabled(String),
}

impl McpToolError {
    /// Stable error code string surfaced to clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidTarget(_) => "mcp_invalid_target",
            Self::ConnectorIdNotAllowed(_) => "mcp_connector_id_not_allowed",
            Self::InvalidConnectorId { .. } => "mcp_invalid_connector_id",
            Self::ServerUrlNotAllowed { .. } => "mcp_server_url_not_allowed",
            Self::PassthroughRequiresOpenAi(_, _) => "mcp_passthrough_unsupported_provider",
            Self::HadrianHostedNotImplemented(_) => "mcp_hadrian_hosted_not_implemented",
            Self::Disabled(_) => "mcp_disabled",
        }
    }
}

/// Coarse provider classification used by the MCP preprocess. We only
/// need to know whether the upstream speaks OpenAI's `mcp` tool
/// natively; the concrete provider config is captured for error
/// messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProviderKind {
    OpenAi,
    AzureOpenAi,
    Anthropic,
    Bedrock,
    Vertex,
    Test,
}

impl McpProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::AzureOpenAi => "azure_openai",
            Self::Anthropic => "anthropic",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::Test => "test",
        }
    }

    /// True for the upstreams that run OpenAI's MCP loop natively.
    pub fn supports_passthrough_mcp(self) -> bool {
        matches!(self, Self::OpenAi | Self::AzureOpenAi)
    }

    /// Derive the kind from a resolved `ProviderConfig`. Stays in sync
    /// with the arms in `routes/execution.rs::ResponsesExecutor::execute`.
    pub fn from_provider(provider: &ProviderConfig) -> Self {
        match provider {
            ProviderConfig::OpenAi(_) => Self::OpenAi,
            ProviderConfig::Anthropic(_) => Self::Anthropic,
            #[cfg(feature = "provider-azure")]
            ProviderConfig::AzureOpenAi(_) => Self::AzureOpenAi,
            #[cfg(feature = "provider-bedrock")]
            ProviderConfig::Bedrock(_) => Self::Bedrock,
            #[cfg(feature = "provider-vertex")]
            ProviderConfig::Vertex(_) => Self::Vertex,
            ProviderConfig::Test(_) => Self::Test,
        }
    }
}

/// Validate every `mcp` tool entry on the payload against the operator
/// configuration and the resolved provider.
///
/// Returns the first failure encountered (bail-on-first; collecting
/// every failure into a `Vec` is a future enhancement). On success the
/// payload is untouched; the downstream rewrite in
/// [`super::mcp::preprocess`] runs only under `hadrian_hosted`.
pub fn preprocess_mcp_tools(
    payload: &CreateResponsesPayload,
    cfg: Option<&McpConfig>,
    provider: McpProviderKind,
) -> Result<(), McpToolError> {
    let Some(tools) = payload.tools.as_ref() else {
        return Ok(());
    };

    for tool in tools {
        let ResponsesToolDefinition::Mcp(mcp) = tool else {
            continue;
        };
        validate_one(mcp, cfg, provider)?;
    }

    Ok(())
}

fn validate_one(
    mcp: &McpTool,
    cfg: Option<&McpConfig>,
    provider: McpProviderKind,
) -> Result<(), McpToolError> {
    let label = mcp.server_label.clone();

    // 1. Feature gate.
    let cfg = match cfg {
        Some(c) if c.enabled => c,
        _ => return Err(McpToolError::Disabled(label)),
    };

    // 2. Target shape — exactly one of `server_url`, `connector_id`.
    if !mcp.has_exactly_one_target() {
        return Err(McpToolError::InvalidTarget(label));
    }

    // 3. Connector flag + closed-enum check.
    if let Some(ref cid) = mcp.connector_id {
        if !cfg.allow_connector_ids {
            return Err(McpToolError::ConnectorIdNotAllowed(label));
        }
        if !is_known_mcp_connector_id(cid) {
            return Err(McpToolError::InvalidConnectorId {
                server_label: label,
                connector_id: cid.clone(),
            });
        }
    }

    // 4. Server URL allowlist (only when configured).
    if let (Some(url), Some(allowed)) = (mcp.server_url.as_ref(), cfg.allowed_server_urls.as_ref())
        && !allowed.iter().any(|a| a == url)
    {
        return Err(McpToolError::ServerUrlNotAllowed {
            server_label: label,
            server_url: url.clone(),
        });
    }

    // 5. Mode → provider check.
    match cfg.mode {
        McpMode::PassthroughOpenai => {
            if !provider.supports_passthrough_mcp() {
                return Err(McpToolError::PassthroughRequiresOpenAi(
                    provider.name(),
                    label,
                ));
            }
        }
        McpMode::HadrianHosted => {
            // The hosted implementation lives in `services::mcp` and is
            // gated on the `mcp` cargo feature. When the feature is off
            // (e.g. `tiny`/`minimal` builds) the mode is still rejected.
            #[cfg(not(feature = "mcp"))]
            {
                return Err(McpToolError::HadrianHostedNotImplemented(label));
            }
            #[cfg(feature = "mcp")]
            {
                // The actual tools/list + rewrite happens in
                // `routes/execution.rs` (async), so pre-admission only
                // gates structural validity. `connector_id` is
                // unreachable under hadrian_hosted — reject early.
                if mcp.connector_id.is_some() {
                    return Err(McpToolError::ConnectorIdNotAllowed(label));
                }
                // server_url is mandatory under hadrian_hosted, and
                // `has_exactly_one_target` already guarantees one of
                // server_url/connector_id is set, so this is implicit.
                let _ = label;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::responses::{McpToolType, ResponsesToolDefinition};

    fn cfg_passthrough() -> McpConfig {
        McpConfig {
            enabled: true,
            mode: McpMode::PassthroughOpenai,
            allowed_server_urls: None,
            allow_connector_ids: false,
            ..McpConfig::default()
        }
    }

    fn cfg_hosted() -> McpConfig {
        McpConfig {
            enabled: true,
            mode: McpMode::HadrianHosted,
            allowed_server_urls: None,
            allow_connector_ids: false,
            ..McpConfig::default()
        }
    }

    fn mcp_tool(server_url: Option<&str>, connector_id: Option<&str>) -> ResponsesToolDefinition {
        ResponsesToolDefinition::Mcp(McpTool {
            type_: McpToolType::Mcp,
            server_label: "test".into(),
            server_url: server_url.map(str::to_string),
            connector_id: connector_id.map(str::to_string),
            server_description: None,
            authorization: None,
            headers: None,
            require_approval: None,
            allowed_tools: None,
            defer_loading: None,
            defer_loading_passthrough: None,
            call_timeout_secs: None,
        })
    }

    fn empty_payload() -> CreateResponsesPayload {
        serde_json::from_value(serde_json::json!({})).expect("minimal payload parses")
    }

    fn payload_with(tool: ResponsesToolDefinition) -> CreateResponsesPayload {
        let mut p = empty_payload();
        p.tools = Some(vec![tool]);
        p
    }

    #[test]
    fn no_mcp_tools_is_noop() {
        let payload = empty_payload();
        assert!(preprocess_mcp_tools(&payload, None, McpProviderKind::Anthropic).is_ok());
    }

    #[test]
    fn disabled_when_config_missing() {
        let payload = payload_with(mcp_tool(Some("https://x"), None));
        let err = preprocess_mcp_tools(&payload, None, McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_disabled");
    }

    #[test]
    fn disabled_when_config_disabled() {
        let cfg = McpConfig {
            enabled: false,
            ..cfg_passthrough()
        };
        let payload = payload_with(mcp_tool(Some("https://x"), None));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_disabled");
    }

    #[test]
    fn rejects_no_target() {
        let cfg = cfg_passthrough();
        let payload = payload_with(mcp_tool(None, None));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_invalid_target");
    }

    #[test]
    fn rejects_both_targets() {
        let cfg = cfg_passthrough();
        let payload = payload_with(mcp_tool(
            Some("https://x"),
            Some("connector_googlecalendar"),
        ));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_invalid_target");
    }

    #[test]
    fn rejects_connector_id_when_disallowed() {
        let cfg = cfg_passthrough();
        let payload = payload_with(mcp_tool(None, Some("connector_googlecalendar")));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_connector_id_not_allowed");
    }

    #[test]
    fn accepts_connector_id_when_allowed() {
        let cfg = McpConfig {
            allow_connector_ids: true,
            ..cfg_passthrough()
        };
        let payload = payload_with(mcp_tool(None, Some("connector_googlecalendar")));
        assert!(preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).is_ok());
    }

    #[test]
    fn rejects_unknown_connector_id() {
        let cfg = McpConfig {
            allow_connector_ids: true,
            ..cfg_passthrough()
        };
        let payload = payload_with(mcp_tool(None, Some("connector_made_up")));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_invalid_connector_id");
    }

    #[test]
    fn enforces_server_url_allowlist() {
        let cfg = McpConfig {
            allowed_server_urls: Some(vec!["https://allowed.example/mcp".into()]),
            ..cfg_passthrough()
        };
        let payload = payload_with(mcp_tool(Some("https://evil.example/mcp"), None));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_server_url_not_allowed");

        let ok_payload = payload_with(mcp_tool(Some("https://allowed.example/mcp"), None));
        assert!(preprocess_mcp_tools(&ok_payload, Some(&cfg), McpProviderKind::OpenAi).is_ok());
    }

    #[test]
    fn passthrough_rejects_anthropic() {
        let cfg = cfg_passthrough();
        let payload = payload_with(mcp_tool(Some("https://x"), None));
        let err =
            preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::Anthropic).unwrap_err();
        assert_eq!(err.code(), "mcp_passthrough_unsupported_provider");
    }

    #[test]
    fn passthrough_accepts_azure() {
        let cfg = cfg_passthrough();
        let payload = payload_with(mcp_tool(Some("https://x"), None));
        assert!(preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::AzureOpenAi).is_ok());
    }

    #[cfg(not(feature = "mcp"))]
    #[test]
    fn hadrian_hosted_rejects_without_feature() {
        let cfg = cfg_hosted();
        let payload = payload_with(mcp_tool(Some("https://x"), None));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_hadrian_hosted_not_implemented");
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn hadrian_hosted_admits_with_feature() {
        // The actual tools/list + rewrite happens later in
        // routes/execution.rs; pre-admission just validates structural
        // shape, so a valid `mcp` tool under hadrian_hosted passes here.
        let cfg = cfg_hosted();
        let payload = payload_with(mcp_tool(Some("https://x"), None));
        assert!(preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).is_ok());
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn hadrian_hosted_rejects_connector_id() {
        // connector_id needs OpenAI's connector registry, unreachable
        // from a self-hosted gateway. Reject at pre-admission even
        // though allow_connector_ids=true would let passthrough through.
        let cfg = McpConfig {
            allow_connector_ids: true,
            ..cfg_hosted()
        };
        let payload = payload_with(mcp_tool(None, Some("connector_googlecalendar")));
        let err = preprocess_mcp_tools(&payload, Some(&cfg), McpProviderKind::OpenAi).unwrap_err();
        assert_eq!(err.code(), "mcp_connector_id_not_allowed");
    }
}
