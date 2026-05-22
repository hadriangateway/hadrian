//! Hadrian's hosted MCP (Model Context Protocol) integration.
//!
//! The `mcp` tool on a `/v1/responses` request can run in two modes:
//!
//! - `passthrough_openai` — the tool entry is forwarded verbatim to
//!   OpenAI/Azure, which runs the MCP client loop. Hadrian does no
//!   MCP-protocol work; see
//!   [`services::mcp_tool`](crate::services::mcp_tool).
//! - `hadrian_hosted` — this module. Hadrian opens a Streamable HTTP
//!   connection to the remote MCP server, lists the advertised tools,
//!   and rewrites the `mcp` tool into N per-tool function tools so any
//!   provider can drive the loop.
//!
//! Implementation is layered on top of the official [`rmcp`] crate —
//! it owns the transport, JSON-RPC envelope, and `Mcp-Session-Id`
//! propagation. This module owns only the Hadrian-specific glue: the
//! client pool, the function-tool rewrite, the `ServerExecutedTool`
//! executor, and the approval gate.
//!
//! Wired in when the `mcp` cargo feature is enabled.

#![cfg(feature = "mcp")]
#![cfg(not(target_arch = "wasm32"))]

pub mod client;
pub mod executor;
pub mod preprocess;
pub mod resume;
pub mod service;
pub mod tool_search;

pub use client::{McpCallContent, McpCallResult, McpClient, McpClientError, McpToolMeta};
pub use executor::McpExecutor;
pub use preprocess::{
    McpRewriteError, parse_function_name, rewrite_mcp_tools, synthesize_function_name,
};
pub use resume::{McpResumeError, resume_mcp_approvals};
pub use service::{McpEndpointKey, McpService, ResolvedMcpApproval};
pub use tool_search::{TOOL_SEARCH_FUNCTION_NAME, ToolSearchExecutor};
