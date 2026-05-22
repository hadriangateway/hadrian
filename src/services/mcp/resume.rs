//! Resume parked MCP tool calls.
//!
//! When a caller sends a follow-up `/v1/responses` request containing
//! `mcp_approval_response` input items, this module:
//!
//! 1. Atomically *claims* each parked approval by `approval_request_id`
//!    (scoped to the caller's org) with a `DELETE … RETURNING`. Doing
//!    the delete up front, before the upstream call, makes resume
//!    exactly-once: of two concurrent resumes of the same id only the
//!    one that wins the delete gets the row and proceeds.
//! 2. On `approve: true` — pulls the caller's bearer / headers off the
//!    matching `mcp` tool entry on the current request, invokes the
//!    original `tools/call` via [`McpService`], and folds the result
//!    back as a reconstructed `function_call` + `function_call_output`
//!    pair sharing the parked `call_id`. The original `function_call`
//!    was suppressed from the client when the gate parked it, so
//!    emitting both keeps the call/result coherent on providers that
//!    translate to native pairwise formats. The model on its next turn
//!    sees the result and continues.
//! 3. On `approve: false` — synthesizes the same pair with a
//!    `{ "error": "<reason>" }` JSON output. The model sees the refusal
//!    and can replan. **Does not require the `mcp` tool block** —
//!    refusals don't need to call the upstream.
//!
//! Approvals that can't be claimed (already consumed, expired, wrong
//! org, never parked) are dropped from `input` with a warning so the
//! request can proceed — the alternative (rejecting the whole request)
//! would leave the caller unable to recover from stale UI state. The
//! tradeoff of claiming before calling is that a transient upstream
//! failure consumes the approval; that is preferable to risking a
//! double-executed side-effecting tool.
//!
//! ### Why the bearer comes from `payload.tools`, not the parked row
//!
//! The parked row deliberately stores `server_label` / `server_url` /
//! tool name / arguments — but **not** the caller's `authorization`
//! header. That keeps OAuth tokens out of the database. Resumption
//! pulls the bearer back off the live request's `tools[]` entry that
//! matches the parked `server_label`, so the caller continues to
//! "own" the credential exactly as they do on every other request.

use std::collections::HashMap;

use uuid::Uuid;

use super::{McpService, preprocess::synthesize_function_name};
use crate::{
    api_types::responses::{
        CreateResponsesPayload, FunctionCallOutput, FunctionCallOutputType, FunctionToolCall,
        FunctionToolCallType, McpApprovalResponseItem, ResponsesInput, ResponsesInputItem,
        ResponsesToolDefinition,
    },
    db::repos::McpPendingApprovalsRepo,
};

/// Failures surfaced by [`resume_mcp_approvals`].
///
/// `CallFailed` / `Repo` map to HTTP 502 (the upstream MCP server or
/// the approvals table failed). `MissingToolBinding` maps to HTTP 400
/// — the caller asked us to approve a call but didn't include the
/// matching `mcp` tool entry on the request, so we have no bearer to
/// forward. Lookup misses on the approvals table itself are NOT
/// errors (warn + drop, so stale UI state doesn't brick the request).
#[derive(Debug, thiserror::Error)]
pub enum McpResumeError {
    #[error("MCP resume failed for tool '{tool}' on '{server_label}': {message}")]
    CallFailed {
        server_label: String,
        tool: String,
        message: String,
    },
    #[error("MCP approvals repo error: {0}")]
    Repo(String),
    #[error(
        "MCP approval response approves a call to server '{server_label}' but the request \
         does not include a matching `mcp` tool entry. Re-send the original `tools` block on \
         the resume request so the gateway has credentials for the upstream call. \
         (approval_request_id: {approval_request_id})"
    )]
    MissingToolBinding {
        server_label: String,
        approval_request_id: String,
    },
    #[error(
        "MCP approval response for server '{server_label}' targets a different server origin \
         than the call that was approved. The approval was issued for '{approved_origin}' but \
         the resume request's `mcp` tool entry points at '{request_origin}'. Re-send the \
         original `server_url` so the approved call runs against the host it was approved for. \
         (approval_request_id: {approval_request_id})"
    )]
    ServerOriginMismatch {
        server_label: String,
        approved_origin: String,
        request_origin: String,
        approval_request_id: String,
    },
}

impl McpResumeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CallFailed { .. } => "mcp_resume_call_failed",
            Self::Repo(_) => "mcp_resume_repo_error",
            Self::MissingToolBinding { .. } => "mcp_resume_missing_tool_binding",
            Self::ServerOriginMismatch { .. } => "mcp_resume_server_origin_mismatch",
        }
    }

    /// True when this error reflects a malformed caller request (HTTP
    /// 400) rather than an upstream / gateway failure (HTTP 502). The
    /// route handler uses this to pick the right status.
    pub fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::MissingToolBinding { .. } | Self::ServerOriginMismatch { .. }
        )
    }
}

/// Authorization, headers, and the full `server_url` extracted from
/// one `mcp` tool entry on the live request. Owned (not borrowed)
/// because we need to release the immutable borrow on `payload.tools`
/// before re-acquiring a mutable borrow on `payload.input` to rewrite
/// it. The extra clone of the bearer / URL happens once per request —
/// negligible.
///
/// `server_url` is sourced from the live request rather than the
/// parked row because we deliberately trim the stored URL to
/// scheme+host (matches OpenAI's discard-after-request behaviour).
#[derive(Debug, Clone)]
struct ToolBinding {
    authorization: Option<String>,
    headers: HashMap<String, String>,
    server_url: Option<String>,
    /// Upper bound on the resumed `tools/call`, resolved from the tool's
    /// `call_timeout_secs` extension or the deployment default. Same
    /// rationale as the streaming dispatch path: rmcp/reqwest apply no
    /// timeout, so an unresponsive server would otherwise hang the
    /// resume request indefinitely.
    call_timeout: std::time::Duration,
}

/// Process every `mcp_approval_response` item in `payload.input`.
///
/// Mutates `payload.input` in place: each `McpApprovalResponse` is
/// replaced by a `FunctionCallOutput` carrying the call's result (on
/// approve) or refusal (on deny). Caller is the `org_id` whose
/// approvals table we look in.
pub async fn resume_mcp_approvals(
    payload: &mut CreateResponsesPayload,
    mcp_service: &McpService,
    org_id: Uuid,
    default_call_timeout_secs: u64,
) -> Result<(), McpResumeError> {
    // Quick exit: no approvals → no work, no allocation.
    let needs_resume = payload
        .input
        .as_ref()
        .map(|i| match i {
            ResponsesInput::Items(items) => items
                .iter()
                .any(|x| matches!(x, ResponsesInputItem::McpApprovalResponse(_))),
            ResponsesInput::Text(_) => false,
        })
        .unwrap_or(false);
    if !needs_resume {
        return Ok(());
    }

    let Some(repo) = mcp_service.approvals_repo().cloned() else {
        tracing::warn!(
            "MCP approval response items present but approvals repo is unavailable; \
             dropping the items so the request can proceed"
        );
        if let Some(ResponsesInput::Items(items)) = payload.input.as_mut() {
            items.retain(|i| !matches!(i, ResponsesInputItem::McpApprovalResponse(_)));
        }
        return Ok(());
    };

    // Build the server_label → ToolBinding lookup from payload.tools
    // BEFORE mutating input. This is the credential the caller sent
    // on THIS request — same lifecycle as any other authenticated
    // call, never persisted by the gateway.
    let bindings = collect_tool_bindings(payload, default_call_timeout_secs);

    let Some(ResponsesInput::Items(items)) = payload.input.as_mut() else {
        return Ok(());
    };

    let mut rewritten: Vec<ResponsesInputItem> = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        match item {
            ResponsesInputItem::McpApprovalResponse(resp) => {
                if let Some((call, output)) =
                    resolve_approval(&resp, mcp_service, repo.as_ref(), org_id, &bindings).await?
                {
                    // Emit the call and its output as a self-contained pair.
                    // The model's original `function_call` was suppressed from
                    // the client when the gate parked it, so the resumed
                    // request carries no matching call for this output —
                    // providers that translate to native pairwise formats
                    // (Anthropic/Bedrock/Vertex) would drop an orphan
                    // `function_call_output`. Reconstructing the pair keeps the
                    // approved call and its result coherent behind any provider.
                    rewritten.push(ResponsesInputItem::FunctionCall(call));
                    rewritten.push(ResponsesInputItem::FunctionCallOutput(output));
                }
                // None means the approval wasn't found / already
                // consumed — drop silently after the warn so the model
                // doesn't see a dangling response.
            }
            other => rewritten.push(other),
        }
    }
    *items = rewritten;
    Ok(())
}

/// Walk `payload.tools` once and index every `mcp` entry by its
/// `server_label`. Returns owned data so the caller can drop the
/// borrow on `payload` before rewriting `payload.input`.
fn collect_tool_bindings(
    payload: &CreateResponsesPayload,
    default_call_timeout_secs: u64,
) -> HashMap<String, ToolBinding> {
    let Some(tools) = payload.tools.as_ref() else {
        return HashMap::new();
    };
    let mut out = HashMap::with_capacity(tools.len());
    for tool in tools {
        if let ResponsesToolDefinition::Mcp(mcp) = tool {
            let timeout_secs = mcp.call_timeout_secs.unwrap_or(default_call_timeout_secs);
            out.insert(
                mcp.server_label.clone(),
                ToolBinding {
                    authorization: mcp.authorization.clone(),
                    headers: mcp.headers.clone().unwrap_or_default(),
                    server_url: mcp.server_url.clone(),
                    call_timeout: std::time::Duration::from_secs(timeout_secs),
                },
            );
        }
    }
    out
}

/// Look up + resolve one approval. Returns the reconstructed
/// `(function_call, function_call_output)` pair to fold back into input,
/// or `None` when the approval doesn't match any parked row.
async fn resolve_approval(
    resp: &McpApprovalResponseItem,
    mcp_service: &McpService,
    repo: &dyn McpPendingApprovalsRepo,
    org_id: Uuid,
    bindings: &HashMap<String, ToolBinding>,
) -> Result<Option<(FunctionToolCall, FunctionCallOutput)>, McpResumeError> {
    // Claim the row atomically (DELETE … RETURNING). Doing the delete up
    // front — before the upstream call — is what makes resume exactly-once:
    // two concurrent resumes of the same `approval_request_id` race on the
    // delete, and only the winner gets `Some(row)` and proceeds to execute.
    // The loser (and any replay of an already-consumed/expired id) sees
    // `None` and is dropped. The tradeoff is that a transient upstream
    // failure after the claim consumes the approval; we prefer that over
    // risking a double-executed side-effecting tool.
    let parked = repo
        .take_by_id_and_org(&resp.approval_request_id, org_id, chrono::Utc::now())
        .await
        .map_err(|e| McpResumeError::Repo(e.to_string()))?;

    let Some(row) = parked else {
        tracing::warn!(
            approval_request_id = %resp.approval_request_id,
            "MCP approval response references an unknown (or already-consumed) parked call; ignoring"
        );
        return Ok(None);
    };

    // Resolve to (function_call_output text, mcp_call output, mcp_call error).
    // `mcp_call.output` and `.error` are mutually exclusive on the
    // synthesized output item the executor will emit; the
    // `function_call_output` text is what the model actually consumes
    // on its next turn.
    let (output_text, mcp_output, mcp_error) = if resp.approve {
        // Resume requires a live tool binding to recover the caller's
        // bearer AND the full `server_url` — we deliberately don't
        // persist auth tokens or the URL path/query in the approvals
        // table. A missing binding is a malformed request (caller
        // forgot to re-send `tools[]`); refuse with 400.
        let binding =
            bindings
                .get(&row.server_label)
                .ok_or_else(|| McpResumeError::MissingToolBinding {
                    server_label: row.server_label.clone(),
                    approval_request_id: resp.approval_request_id.clone(),
                })?;
        let server_url =
            binding
                .server_url
                .as_deref()
                .ok_or_else(|| McpResumeError::MissingToolBinding {
                    server_label: row.server_label.clone(),
                    approval_request_id: resp.approval_request_id.clone(),
                })?;

        // Bind the approval to the server it was approved against.
        // The parked row stores the origin (scheme://host[:port]) the
        // gate emitted the `mcp_approval_request` for; we recompute the
        // origin of the live request's `server_url` and reject a
        // mismatch. Without this, a caller holding a valid
        // `approval_request_id` could resume the approved (tool, args)
        // against a *different* host (same `server_label`), redirecting
        // a human-approved side-effecting call — and forwarding that
        // entry's bearer — to an attacker-chosen origin.
        let request_origin = crate::routes::api::chat::trim_url_to_origin(server_url);
        if request_origin != row.server_url {
            tracing::warn!(
                approval_request_id = %resp.approval_request_id,
                server_label = %row.server_label,
                approved_origin = %row.server_url,
                request_origin = %request_origin,
                "MCP approval response targets a different server origin than the approved call; refusing"
            );
            return Err(McpResumeError::ServerOriginMismatch {
                server_label: row.server_label.clone(),
                approved_origin: row.server_url.clone(),
                request_origin,
                approval_request_id: resp.approval_request_id.clone(),
            });
        }

        // The arguments were serialized from a `Value` at park time, so
        // a parse failure here means the stored row is corrupt. Don't
        // silently call the tool with `null` args — fail loud.
        let arguments: serde_json::Value =
            serde_json::from_str(&row.arguments_json).map_err(|e| {
                tracing::error!(
                    approval_request_id = %resp.approval_request_id,
                    server_label = %row.server_label,
                    tool = %row.tool_name,
                    error = %e,
                    "Parked MCP approval has corrupt arguments_json; refusing to resume"
                );
                McpResumeError::CallFailed {
                    server_label: row.server_label.clone(),
                    tool: row.tool_name.clone(),
                    message: format!("corrupt stored arguments: {e}"),
                }
            })?;
        // Bound the call the same way the streaming dispatch path
        // does: rmcp/reqwest apply no timeout, so an unresponsive
        // server would hang the resume request. On expiry, surface a
        // timeout failure to the model.
        let call = mcp_service.call_tool(
            server_url,
            binding.authorization.as_deref(),
            &binding.headers,
            &row.tool_name,
            arguments,
        );
        let result = match tokio::time::timeout(binding.call_timeout, call).await {
            Ok(inner) => inner.map_err(|e| McpResumeError::CallFailed {
                server_label: row.server_label.clone(),
                tool: row.tool_name.clone(),
                message: e.to_string(),
            })?,
            Err(_elapsed) => {
                tracing::warn!(
                    approval_request_id = %resp.approval_request_id,
                    server_label = %row.server_label,
                    tool = %row.tool_name,
                    timeout_secs = binding.call_timeout.as_secs(),
                    "Resumed MCP tools/call timed out"
                );
                // Evict the pooled connection: the timed-out request was
                // abandoned mid-flight without a protocol-level cancel,
                // so the rmcp session is suspect and must not be reused.
                mcp_service.evict_endpoint(
                    server_url,
                    binding.authorization.as_deref(),
                    &binding.headers,
                );
                return Err(McpResumeError::CallFailed {
                    server_label: row.server_label.clone(),
                    tool: row.tool_name.clone(),
                    message: format!(
                        "MCP tool call timed out after {}s",
                        binding.call_timeout.as_secs()
                    ),
                });
            }
        };

        let result_text = super::executor::collapse_result_text(&result);
        if result.is_error {
            let err = format!("MCP tool returned isError=true: {result_text}");
            (
                serde_json::json!({"error": result_text}).to_string(),
                None,
                Some(err),
            )
        } else {
            (result_text.clone(), Some(result_text), None)
        }
    } else {
        // Refusals don't hit the upstream — no binding needed. Lets
        // callers cleanly deny without re-sending the tools block.
        let detail = resp
            .reason
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("User refused the call.");
        let refusal = format!("refused: {detail}");
        (
            serde_json::json!({ "error": refusal.clone() }).to_string(),
            None,
            Some(refusal),
        )
    };

    // No delete here: the row was already claimed (and removed) by the
    // `take_by_id_and_org` above.

    // Hand the resolved approval to the executor so it can synthesize
    // an `mcp_call` output item on the resumed response stream — the
    // spec-mandated item carrying `output` / `error` and the link back
    // to the original `approval_request_id`.
    mcp_service.stash_resolved_approval(
        org_id,
        super::ResolvedMcpApproval {
            call_id: row.call_id.clone(),
            approval_request_id: resp.approval_request_id.clone(),
            server_label: row.server_label.clone(),
            tool_name: row.tool_name.clone(),
            arguments_json: row.arguments_json.clone(),
            output: mcp_output,
            error: mcp_error,
        },
    );

    // Reconstruct the assistant's `function_call` so the output below has a
    // matching call to anchor to (`synthesize_function_name` yields the same
    // `mcp_<label>__<tool>` name the rewrite exposed). The original was
    // suppressed from the client at park time, so it isn't in the resumed
    // input. `id` and `call_id` share the parked `call_id` — unique per
    // response and never colliding with a live function-call id.
    let function_call = FunctionToolCall {
        type_: FunctionToolCallType::FunctionCall,
        id: row.call_id.clone(),
        call_id: row.call_id.clone(),
        name: synthesize_function_name(&row.server_label, &row.tool_name),
        arguments: row.arguments_json.clone(),
        status: None,
    };
    let output = FunctionCallOutput {
        type_: FunctionCallOutputType::FunctionCallOutput,
        id: None,
        call_id: row.call_id,
        output: output_text,
        status: None,
    };
    Ok(Some((function_call, output)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::responses::{McpTool, McpToolType};

    fn mcp_tool_def(label: &str, auth: Option<&str>) -> ResponsesToolDefinition {
        ResponsesToolDefinition::Mcp(McpTool {
            type_: McpToolType::Mcp,
            server_label: label.to_string(),
            server_url: Some("https://x".to_string()),
            connector_id: None,
            server_description: None,
            authorization: auth.map(str::to_string),
            headers: None,
            require_approval: None,
            allowed_tools: None,
            defer_loading: None,
            defer_loading_passthrough: None,
            call_timeout_secs: None,
        })
    }

    fn payload_with_tools(tools: Vec<ResponsesToolDefinition>) -> CreateResponsesPayload {
        let mut p: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({})).expect("minimal payload");
        p.tools = Some(tools);
        p
    }

    #[test]
    fn collect_tool_bindings_indexes_by_server_label() {
        let payload = payload_with_tools(vec![
            mcp_tool_def("atlassian", Some("Bearer X")),
            mcp_tool_def("notion", Some("Bearer Y")),
            // Non-mcp tools (here represented as a function tool)
            // should be ignored by the indexer.
            ResponsesToolDefinition::Function(
                crate::api_types::responses::FunctionTool::from_json(
                    serde_json::json!({"type":"function","name":"foo"}),
                )
                .unwrap(),
            ),
        ]);
        let map = collect_tool_bindings(&payload, 300);
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("atlassian").unwrap().authorization.as_deref(),
            Some("Bearer X")
        );
        assert_eq!(
            map.get("notion").unwrap().authorization.as_deref(),
            Some("Bearer Y")
        );
        assert!(!map.contains_key("github"));
    }

    #[test]
    fn collect_tool_bindings_handles_no_tools() {
        let payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({})).unwrap();
        let map = collect_tool_bindings(&payload, 300);
        assert!(map.is_empty());
    }

    #[test]
    fn collect_tool_bindings_captures_missing_authorization() {
        // Callers without a bearer (e.g. anonymous MCP servers) still
        // get a binding, just with `authorization: None`.
        let payload = payload_with_tools(vec![mcp_tool_def("anon", None)]);
        let map = collect_tool_bindings(&payload, 300);
        assert!(map.get("anon").unwrap().authorization.is_none());
    }

    #[test]
    fn missing_tool_binding_is_client_error() {
        let e = McpResumeError::MissingToolBinding {
            server_label: "atlassian".into(),
            approval_request_id: "mcpr_x".into(),
        };
        assert!(e.is_client_error());
        assert_eq!(e.code(), "mcp_resume_missing_tool_binding");
    }

    #[test]
    fn call_failed_is_not_client_error() {
        let e = McpResumeError::CallFailed {
            server_label: "atlassian".into(),
            tool: "jira_search".into(),
            message: "503".into(),
        };
        assert!(!e.is_client_error());
        assert_eq!(e.code(), "mcp_resume_call_failed");
    }

    /// Repo holding a single parked approval; `take_by_id_and_org`
    /// returns it once (and `None` thereafter, mirroring the real
    /// claim-and-delete semantics).
    struct OneShotRepo {
        row: std::sync::Mutex<Option<crate::db::repos::McpPendingApproval>>,
    }

    #[async_trait::async_trait]
    impl McpPendingApprovalsRepo for OneShotRepo {
        async fn insert(
            &self,
            _row: crate::db::repos::NewMcpPendingApproval,
        ) -> crate::db::DbResult<()> {
            Ok(())
        }

        async fn take_by_id_and_org(
            &self,
            id: &str,
            org_id: Uuid,
            _now: chrono::DateTime<chrono::Utc>,
        ) -> crate::db::DbResult<Option<crate::db::repos::McpPendingApproval>> {
            let mut guard = self.row.lock().unwrap();
            match guard.as_ref() {
                Some(r) if r.id == id && r.org_id == org_id => Ok(guard.take()),
                _ => Ok(None),
            }
        }

        async fn delete_expired(
            &self,
            _cutoff: chrono::DateTime<chrono::Utc>,
        ) -> crate::db::DbResult<u64> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn refusal_reconstructs_self_contained_function_pair() {
        // A `mcp_approval_response` with `approve: false` is rewritten into
        // a paired `function_call` + `function_call_output` so non-OpenAI
        // providers (which drop an orphan `function_call_output`) still see
        // the call and its refusal. Refusals don't hit the upstream, so no
        // tool binding / network is needed.
        use crate::api_types::responses::{McpApprovalResponseItem, McpApprovalResponseItemType};

        let org_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        let repo = std::sync::Arc::new(OneShotRepo {
            row: std::sync::Mutex::new(Some(crate::db::repos::McpPendingApproval {
                id: "mcpr_x".into(),
                response_id: "resp_1".into(),
                org_id,
                call_id: "fc_1".into(),
                server_label: "atlassian".into(),
                server_url: "https://mcp.atlassian.com".into(),
                tool_name: "jira_search".into(),
                arguments_json: r#"{"q":"bug"}"#.into(),
                created_at: now,
                expires_at: now + chrono::Duration::seconds(600),
            })),
        });
        let service = McpService::with_approvals_repo(
            Some(repo as std::sync::Arc<dyn McpPendingApprovalsRepo>),
            Default::default(),
        );

        let mut payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({})).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::McpApprovalResponse(McpApprovalResponseItem {
                type_: McpApprovalResponseItemType::McpApprovalResponse,
                approval_request_id: "mcpr_x".into(),
                approve: false,
                reason: Some("not now".into()),
            }),
        ]));

        resume_mcp_approvals(&mut payload, &service, org_id, 300)
            .await
            .unwrap();

        let Some(ResponsesInput::Items(items)) = payload.input.as_ref() else {
            panic!("expected items input");
        };
        assert_eq!(items.len(), 2, "approval response → call + output pair");
        match &items[0] {
            ResponsesInputItem::FunctionCall(fc) => {
                assert_eq!(fc.name, "mcp_atlassian__jira_search");
                assert_eq!(fc.call_id, "fc_1");
                assert_eq!(fc.arguments, r#"{"q":"bug"}"#);
            }
            other => panic!("expected function_call, got {other:?}"),
        }
        match &items[1] {
            ResponsesInputItem::FunctionCallOutput(out) => {
                assert_eq!(out.call_id, "fc_1");
                assert!(out.output.contains("refused"));
                assert!(out.output.contains("not now"));
            }
            other => panic!("expected function_call_output, got {other:?}"),
        }
    }
}
