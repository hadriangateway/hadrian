//! `McpExecutor` — the [`ServerExecutedTool`] that intercepts the
//! function calls produced by [`super::preprocess::rewrite_mcp_tools`]
//! and dispatches them to the right pooled MCP client.
//!
//! Lifecycle per call:
//! 1. The upstream provider emits `response.output_item.done` with a
//!    `function_call` item whose `name` matches `mcp_<label>__<tool>`.
//! 2. `detect()` parses the function name and arguments, returns one
//!    `DetectedToolCall` per match.
//! 3. `execute()` resolves `<label>` back to the requesting `McpTool`
//!    (server_url + auth + headers), emits the `mcp_call` lifecycle
//!    events (`output_item.added`, `mcp_call_arguments.delta/done`,
//!    `mcp_call.in_progress`, then `mcp_call.completed`/`.failed` plus
//!    a terminal `output_item.done` with `output` / `error` inlined on
//!    the same `mcp_call` item), and folds a `function_call_output`
//!    continuation item for the next provider request.
//! 4. The runner re-invokes `ResponsesExecutor::execute` with the
//!    continuation payload, which triggers another tools/list-cached
//!    rewrite (idempotent — same catalog → same function tools).
//!
//! Calls gated by `require_approval` are parked instead of executed.
//! [`McpExecutor::park_for_approval`] writes a row to
//! `mcp_pending_approvals` and emits the canonical `mcp_approval_request`
//! item; [`super::resume::resume_mcp_approvals`] picks the parked call
//! back up when a follow-up request carries a matching
//! `mcp_approval_response` input item.

use async_trait::async_trait;
use bytes::Bytes;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::warn;

use super::{McpCallResult, McpService, preprocess::parse_function_name};
use crate::{
    api_types::responses::{
        CreateResponsesPayload, FunctionCallOutput, McpTool, ResponsesInput, ResponsesInputItem,
        ResponsesToolDefinition,
    },
    services::server_tools::{
        DetectedToolCall, ServerExecutedTool, ToolCallResult, ToolContext, ToolError,
        ToolExecutionHandle,
    },
};

/// One MCP server's connection details, captured from a `McpTool` entry
/// on the request. Used by [`McpExecutor::resolve_binding`] to map a
/// detected function call back to the server it belongs to.
#[derive(Clone)]
struct ServerBinding {
    server_label: String,
    server_url: String,
    authorization: Option<String>,
    headers: std::collections::HashMap<String, String>,
    /// Sanitized form of `server_label` — what appears in the function
    /// name the model invokes. Computed once at executor construction
    /// to match the rewrite output.
    sanitized_label: String,
    /// Per-server approval policy, captured at request time. The
    /// executor consults this before every call to decide whether to
    /// park or run.
    require_approval: Option<crate::api_types::responses::McpRequireApproval>,
    /// Upper bound on a single `tools/call` round-trip. Resolved at
    /// construction from the tool's `call_timeout_secs` Hadrian extension,
    /// falling back to `[features.mcp].call_timeout_secs`.
    call_timeout: std::time::Duration,
}

impl ServerBinding {
    fn from_mcp(tool: &McpTool, default_call_timeout_secs: u64) -> Option<Self> {
        let server_url = tool.server_url.clone()?;
        // Synthesize a known function name with a sentinel tool to
        // extract just the sanitized label. Keeps the two sides aligned
        // even if `sanitize_label`'s rules evolve.
        let synthesized = super::synthesize_function_name(&tool.server_label, "x");
        let (sanitized_label, _) = parse_function_name(&synthesized)?;
        let timeout_secs = tool.call_timeout_secs.unwrap_or(default_call_timeout_secs);
        Some(Self {
            server_label: tool.server_label.clone(),
            server_url,
            authorization: tool.authorization.clone(),
            headers: tool.headers.clone().unwrap_or_default(),
            sanitized_label: sanitized_label.to_string(),
            require_approval: tool.require_approval.clone(),
            call_timeout: std::time::Duration::from_secs(timeout_secs),
        })
    }

    /// Decide whether a call to `tool_name` on this server should be
    /// gated by approval. Honors the spec's three shapes:
    ///
    /// - `None` → spec default is `"always"`
    ///   (`openapi/openai.openapi.json::MCPTool.require_approval`); gate.
    /// - `Mode(Never)` → never gate.
    /// - `Mode(Always)` → always gate.
    /// - `Filter { always, never }` → if `never` matches, skip; else
    ///   if `always` matches, gate; else gate by default (matches the
    ///   `Always` default OpenAI documents for the filter object form).
    ///
    /// `read_only` predicate on a filter matches only when the tool
    /// carries an explicit MCP `readOnlyHint` annotation equal to the
    /// filter value (passed in via `read_only_hint`); `None` (no
    /// annotation) matches neither `read_only: true` nor `false`.
    fn requires_approval(&self, tool_name: &str, read_only_hint: Option<bool>) -> bool {
        use crate::api_types::responses::{McpApprovalMode, McpRequireApproval};
        match &self.require_approval {
            None => true,
            Some(McpRequireApproval::Mode(McpApprovalMode::Never)) => false,
            Some(McpRequireApproval::Mode(McpApprovalMode::Always)) => true,
            Some(McpRequireApproval::Filter(f)) => {
                if let Some(never) = f.never.as_ref()
                    && filter_matches(never, tool_name, read_only_hint)
                {
                    return false;
                }
                if let Some(always) = f.always.as_ref()
                    && filter_matches(always, tool_name, read_only_hint)
                {
                    return true;
                }
                // Default for the object form: gate. OpenAI documents
                // the filter object as a way to *exempt* specific tools;
                // anything not explicitly listed keeps the default.
                true
            }
        }
    }
}

/// True iff `tool_name` (and its read-only hint) match every constraint
/// declared on the filter. Empty constraints (both fields `None`) match
/// every tool.
fn filter_matches(
    filter: &crate::api_types::responses::McpToolFilter,
    tool_name: &str,
    read_only_hint: Option<bool>,
) -> bool {
    if let Some(names) = filter.tool_names.as_ref()
        && !names.iter().any(|n| n == tool_name)
    {
        return false;
    }
    if let Some(required) = filter.read_only
        && read_only_hint != Some(required)
    {
        // Per the MCP tool filter spec, a `read_only` predicate matches
        // only tools annotated with `readOnlyHint`; an absent annotation
        // matches neither `true` nor `false`.
        return false;
    }
    true
}

/// Server-executed-tool implementation for MCP.
///
/// One executor instance per `/v1/responses` request — built in
/// `apply_streaming_pipeline` after the request payload is admitted.
pub struct McpExecutor {
    /// Pooled MCP clients + tools/list cache.
    service: McpService,
    /// All MCP tool entries originally on the request, captured before
    /// the rewrite stripped them. Each is a server we may need to call
    /// during this response.
    bindings: Vec<ServerBinding>,
    /// Server labels whose `mcp_list_tools` snapshot is already in the
    /// caller's context (via a prior `previous_response_id`). For these
    /// we skip emitting a fresh catalog so SDKs see the snapshot exactly
    /// once per conversation, matching OpenAI's behavior.
    suppress_list_tools: std::collections::HashSet<String>,
    /// Identifier of the response this executor is serving. Used to
    /// scope `mcp_pending_approvals` rows. `None` when the request
    /// isn't persisted (e.g. `store=false` with no DB), in which case
    /// approval gating degrades to a warn-and-run.
    response_id: Option<String>,
    /// Org scope for parked approvals.
    org_id: Option<uuid::Uuid>,
    /// TTL for parked approvals. Rows past this are reaped by the
    /// existing retention worker. Default 24h to give users plenty of
    /// time to decide; configurable per-deployment is a future tweak.
    approval_ttl_secs: u64,
    /// Approvals resolved by the resume path on this request, drained
    /// out of [`McpService::take_resolved_approval`] at construction.
    /// Emitted as synthesized `mcp_call` output items by
    /// [`Self::prefix_events`] so the resumed response stream carries
    /// the spec-mandated item lifecycle (`output_item.added`,
    /// `response.mcp_call.in_progress`, `response.mcp_call.completed`
    /// or `.failed`, `output_item.done`).
    resumed_approvals: Vec<super::ResolvedMcpApproval>,
    /// Hides the rewritten `mcp_<label>__<tool>` function-call plumbing
    /// from the client stream — we synthesize the spec-shaped `mcp_call`
    /// items ourselves (see [`Self::transform_event`]). Shared with the
    /// other server tools.
    suppressor: crate::services::server_tools::FunctionCallSuppressor,
}

/// Fallback per-call timeout used by [`McpExecutor::new`] and tests.
/// Mirrors `[features.mcp].call_timeout_secs`'s default; the real
/// pipeline path always passes the configured value to
/// [`McpExecutor::with_persistence`].
const DEFAULT_CALL_TIMEOUT_SECS: u64 = 300;

impl McpExecutor {
    pub fn new(service: McpService, original_payload: &CreateResponsesPayload) -> Self {
        Self::with_persistence(
            service,
            original_payload,
            None,
            None,
            DEFAULT_CALL_TIMEOUT_SECS,
        )
    }

    /// Construct an executor that can persist parked approvals. Pass
    /// `response_id` / `org_id` when both are known (DB-backed
    /// responses); otherwise use [`McpExecutor::new`] and approval
    /// gating will warn-and-run. `default_call_timeout_secs` is the
    /// deployment default applied to any `mcp` tool that doesn't set its
    /// own `call_timeout_secs`.
    pub fn with_persistence(
        service: McpService,
        original_payload: &CreateResponsesPayload,
        response_id: Option<String>,
        org_id: Option<uuid::Uuid>,
        default_call_timeout_secs: u64,
    ) -> Self {
        let bindings: Vec<ServerBinding> = original_payload
            .tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t.as_mcp())
                    .filter_map(|m| ServerBinding::from_mcp(m, default_call_timeout_secs))
                    .collect()
            })
            .unwrap_or_default();
        let suppress_list_tools = collect_inlined_list_tools_labels(original_payload);
        // Drain any approvals the resume path just resolved on this
        // request. Match by `call_id` of `function_call_output` items
        // the resume wrote into `payload.input` — that's our stable
        // join key.
        let resumed_approvals = drain_resumed_approvals(&service, org_id, original_payload);
        Self {
            service,
            bindings,
            suppress_list_tools,
            response_id,
            org_id,
            approval_ttl_secs: 86_400,
            resumed_approvals,
            suppressor: crate::services::server_tools::FunctionCallSuppressor::new(),
        }
    }

    /// `true` iff the tool's `readOnlyHint` annotation is set to true.
    /// `false` when unknown / missing — matches OpenAI's documented
    /// behavior for the `read_only` filter (matches only when the
    /// server explicitly annotated the tool as read-only). Reads the
    /// service-level `tools/list` cache, which the rewrite primed for
    /// this endpoint moments earlier.
    /// The tool's MCP `readOnlyHint` annotation, or `None` when the tool
    /// has no such annotation (or the catalog isn't cached). `None` is
    /// distinct from `Some(false)`: a `read_only` filter only matches an
    /// explicit annotation, so the absence must not be coerced to false.
    fn read_only_hint_for(&self, binding: &ServerBinding, tool_name: &str) -> Option<bool> {
        let catalog = self.service.cached_tools(
            &binding.server_url,
            binding.authorization.as_deref(),
            &binding.headers,
        )?;
        catalog
            .iter()
            .find(|t| t.name == tool_name)
            .and_then(|t| t.annotations.as_ref())
            .and_then(|a| a.get("readOnlyHint"))
            .and_then(|v| v.as_bool())
    }

    /// True when this request had at least one `mcp` tool entry the
    /// executor can serve, or at least one resumed approval to surface
    /// as an `mcp_call` item.
    pub fn has_bindings(&self) -> bool {
        !self.bindings.is_empty() || !self.resumed_approvals.is_empty()
    }

    fn resolve_binding(&self, sanitized_label: &str) -> Option<&ServerBinding> {
        self.bindings
            .iter()
            .find(|b| b.sanitized_label == sanitized_label)
    }

    /// Park a model-initiated call for human approval.
    ///
    /// Emits the canonical `mcp_approval_request` SSE item, persists
    /// the call to `mcp_pending_approvals` so a follow-up
    /// `mcp_approval_response` can find it, and synthesizes a
    /// `function_call_output` telling the model to stop and wait.
    ///
    /// **Fail-closed**: when persistence is unavailable (no DB, the
    /// request is `store=false`, or the principal has no org), the
    /// call is *not* run. Instead a synthesized `mcp_call` item with
    /// `status="failed"` and an explanatory `error` is emitted, and
    /// the continuation `function_call_output` carries the same error.
    /// Letting the call through silently would defeat the purpose of
    /// `require_approval`.
    async fn park_for_approval(
        &self,
        binding: &ServerBinding,
        call_id: &str,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<ToolExecutionHandle, ToolError> {
        let (Some(repo), Some(response_id), Some(org_id)) = (
            self.service.approvals_repo().cloned(),
            self.response_id.clone(),
            self.org_id,
        ) else {
            // Diagnose which precondition is missing so the operator/
            // caller can fix it. The order mirrors the destructuring:
            // no repo → no DB; no response_id → store=false (or no DB);
            // no org_id → anonymous request.
            let missing = if self.service.approvals_repo().is_none() {
                "no database is configured (server-side)"
            } else if self.response_id.is_none() {
                "the request was sent with `store=false` (or persistence is not \
                 wired for this response); approvals require a persisted response \
                 so the parked call can be resumed"
            } else {
                "the request has no authenticated organization scope"
            };
            let err = format!(
                "MCP `require_approval` gates this tool but the gateway cannot park \
                 the call: {missing}. Set `[features.mcp]` with a database, send \
                 `store=true`, and authenticate the request — or set \
                 `require_approval=\"never\"` if gating isn't desired."
            );
            tracing::error!(
                server_label = %binding.server_label,
                tool = %tool_name,
                error = %err,
                "MCP approval gate failing closed"
            );
            return self.synthesize_failed_call(binding, call_id, tool_name, arguments, err);
        };

        let approval_id = format!("mcpr_{}", uuid::Uuid::new_v4().simple());
        let now = crate::db::repos::truncate_to_millis(chrono::Utc::now());
        let expires_at = now + chrono::Duration::seconds(self.approval_ttl_secs as i64);

        // `arguments` was parsed from the model's JSON, so serializing it
        // back can't realistically fail; if it ever does, persisting `{}`
        // would silently drop the model's intent on resume — log loudly.
        let arguments_json = serde_json::to_string(arguments).unwrap_or_else(|e| {
            tracing::error!(
                server_label = %binding.server_label,
                tool = %tool_name,
                error = %e,
                "Failed to serialize MCP call arguments while parking for approval; storing empty"
            );
            "{}".to_string()
        });

        // Strip path / query from `server_url` before persistence —
        // matches OpenAI's "discard everything but scheme+host" rule.
        // The resume path pulls the live URL from `payload.tools` so
        // the trimmed copy is only ever used for audit / lookup keys.
        let stored_server_url = crate::routes::api::chat::trim_url_to_origin(&binding.server_url);

        repo.insert(crate::db::repos::NewMcpPendingApproval {
            id: approval_id.clone(),
            response_id,
            org_id,
            call_id: call_id.to_string(),
            server_label: binding.server_label.clone(),
            server_url: stored_server_url,
            tool_name: tool_name.to_string(),
            arguments_json: arguments_json.clone(),
            created_at: now,
            expires_at,
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("MCP approval persist failed: {e}")))?;

        let (event_tx, event_rx) = mpsc::channel::<Bytes>(8);
        let (result_tx, result_rx) =
            tokio::sync::oneshot::channel::<Result<ToolCallResult, ToolError>>();

        // Emit `mcp_approval_request` as a paired added/done item.
        // Every other Responses-API output item emits `.added` before
        // `.done`; SDKs that key off the open→close transition need
        // both edges, so match that here even though we have nothing
        // incremental to stream between them.
        let request_item = mcp_approval_request_item(
            &approval_id,
            &binding.server_label,
            tool_name,
            &arguments_json,
        );
        let _ = event_tx
            .send(sse_output_item(
                "response.output_item.added",
                request_item.clone(),
            ))
            .await;
        let _ = event_tx
            .send(sse_output_item("response.output_item.done", request_item))
            .await;

        let call_id_owned = call_id.to_string();
        let server_label_owned = binding.server_label.clone();
        let tool_name_owned = tool_name.to_string();
        // Spawn so the channel survives until the runner pulls the
        // event; the actual continuation is synthesized immediately.
        tokio::spawn(async move {
            // Tell the model to stop and wait. Without an output the
            // function_call has no result; with one, the model sees a
            // clear "do not continue" signal and the response ends in
            // an assistant turn rather than burning the iteration
            // budget on retries.
            let waiting_msg = serde_json::json!({
                "status": "pending_approval",
                "approval_request_id": approval_id,
                "server_label": server_label_owned,
                "tool": tool_name_owned,
                "message": "Call gated by `require_approval`. Stop here — the caller \
                            must submit an `mcp_approval_response` on a follow-up \
                            request to resume."
            })
            .to_string();
            let continuation = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: crate::api_types::responses::FunctionCallOutputType::FunctionCallOutput,
                id: None,
                call_id: call_id_owned.clone(),
                output: waiting_msg,
                status: None,
            });
            let _ = result_tx.send(Ok(ToolCallResult {
                call_id: call_id_owned,
                continuation_items: vec![continuation],
            }));
            drop(event_tx);
        });

        Ok(ToolExecutionHandle {
            events: Box::pin(futures_util::stream::unfold(
                event_rx,
                |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
            )),
            result: Box::pin(async move {
                result_rx.await.map_err(|_| {
                    ToolError::ExecutionFailed(
                        "MCP approval task dropped before sending result".to_string(),
                    )
                })?
            }),
        })
    }

    /// Synthesize a failed `mcp_call` item without touching the
    /// upstream — used when the approval gate fails closed because
    /// persistence isn't available. Emits the same item lifecycle as
    /// `dispatch_call` (`output_item.added` → `output_item.done` →
    /// `response.mcp_call.failed`) but skips the HTTP round-trip and
    /// fills in `error` directly. The continuation carries the same
    /// error JSON so the model sees a clean refusal.
    fn synthesize_failed_call(
        &self,
        binding: &ServerBinding,
        call_id: &str,
        tool_name: &str,
        raw_args: &Value,
        error_msg: String,
    ) -> Result<ToolExecutionHandle, ToolError> {
        let item_id = next_item_id("mcp");

        let (event_tx, event_rx) = mpsc::channel::<Bytes>(4);
        let (result_tx, result_rx) =
            tokio::sync::oneshot::channel::<Result<ToolCallResult, ToolError>>();

        let failed_item = mcp_call_item(
            &item_id,
            &binding.server_label,
            tool_name,
            raw_args,
            "failed",
            None,
            Some(error_msg.as_str()),
            None,
        );
        let added = sse_output_item("response.output_item.added", failed_item.clone());
        let done = sse_output_item("response.output_item.done", failed_item);
        let lifecycle = sse_mcp_lifecycle_event("response.mcp_call.failed", &item_id);

        let call_id_owned = call_id.to_string();
        let error_for_task = error_msg.clone();
        tokio::spawn(async move {
            // Spec order: lifecycle `failed` precedes terminal `done`.
            for ev in [added, lifecycle, done] {
                if event_tx.send(ev).await.is_err() {
                    return;
                }
            }
            let continuation = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: crate::api_types::responses::FunctionCallOutputType::FunctionCallOutput,
                id: None,
                call_id: call_id_owned.clone(),
                output: serde_json::json!({ "error": error_for_task }).to_string(),
                status: None,
            });
            let _ = result_tx.send(Ok(ToolCallResult {
                call_id: call_id_owned,
                continuation_items: vec![continuation],
            }));
            drop(event_tx);
        });

        Ok(ToolExecutionHandle {
            events: Box::pin(futures_util::stream::unfold(
                event_rx,
                |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
            )),
            result: Box::pin(async move {
                result_rx.await.map_err(|_| {
                    ToolError::ExecutionFailed(
                        "MCP fail-closed task dropped before sending result".to_string(),
                    )
                })?
            }),
        })
    }

    /// Run an MCP `tools/call` and stream the canonical `mcp_call`
    /// lifecycle events. The result is inlined onto the terminal
    /// `mcp_call` item — there is no separate `mcp_call_output` per
    /// OpenAI's spec. Used by the normal `execute()` path after the
    /// approval gate either passes or has already been resolved on a
    /// follow-up request.
    async fn dispatch_call(
        &self,
        binding: ServerBinding,
        call_id: String,
        tool_name: String,
        raw_args: Value,
        approval_request_id: Option<String>,
    ) -> Result<ToolExecutionHandle, ToolError> {
        let service = self.service.clone();

        let (event_tx, event_rx) = mpsc::channel::<Bytes>(16);
        let (result_tx, result_rx) =
            tokio::sync::oneshot::channel::<Result<ToolCallResult, ToolError>>();

        // Emit the full MCP lifecycle ahead of the actual call:
        //   response.output_item.added         (mcp_call status=in_progress)
        //   response.mcp_call_arguments.delta  (single chunk of full args)
        //   response.mcp_call_arguments.done
        //   response.mcp_call.in_progress
        // We have no incremental source for the arguments under
        // hadrian_hosted (the upstream emits the function_call
        // atomically via `output_item.done`), so the delta carries
        // the full argument JSON in one chunk.
        let call_item_id = next_item_id("mcp");
        let arguments_str = serde_json::to_string(&raw_args).unwrap_or_else(|_| "{}".to_string());
        let approval_req = approval_request_id.clone();

        // Initial item status is `calling` rather than `in_progress`:
        // under hadrian_hosted the model's function_call arrived
        // atomically from the upstream, so by the time we emit the
        // `output_item.added` we are already invoking the MCP server.
        // `MCPToolCallStatus` defines `calling` for exactly this state
        // (`openapi/openai.openapi.json::MCPToolCallStatus`); using it
        // here lets SDKs and persistence reflect that the args phase
        // is done and the network round-trip is in flight.
        let _ = event_tx
            .send(sse_output_item(
                "response.output_item.added",
                mcp_call_item(
                    &call_item_id,
                    &binding.server_label,
                    &tool_name,
                    &raw_args,
                    "calling",
                    None,
                    None,
                    approval_req.as_deref(),
                ),
            ))
            .await;
        let _ = event_tx
            .send(sse_mcp_arguments_event(
                "response.mcp_call_arguments.delta",
                &call_item_id,
                Some(&arguments_str),
            ))
            .await;
        let _ = event_tx
            .send(sse_mcp_arguments_event(
                "response.mcp_call_arguments.done",
                &call_item_id,
                Some(&arguments_str),
            ))
            .await;
        let _ = event_tx
            .send(sse_mcp_lifecycle_event(
                "response.mcp_call.in_progress",
                &call_item_id,
            ))
            .await;

        // Spawn the actual call so the executor returns its handle
        // immediately and the runner can stream the `added` events
        // while we work.
        let binding_for_task = binding.clone();
        let approval_req_for_task = approval_request_id;
        tokio::spawn(async move {
            // Bound the call: `rmcp` applies no request timeout and the
            // reqwest client it builds sets none either, so a server that
            // accepts the connection and then stalls would hang the whole
            // response. On expiry we cancel the in-flight future and
            // surface an `incomplete` call.
            let call_timeout = binding_for_task.call_timeout;
            let outcome = tokio::time::timeout(
                call_timeout,
                run_call(&service, &binding_for_task, &tool_name, raw_args.clone()),
            )
            .await;

            // OpenAI's `mcp_call.output` is a plain string. MCP servers
            // may return `text` content, `structuredContent` (a JSON
            // object), or non-text blocks (image/audio/resource). We
            // collapse to one string in priority order so the model
            // always gets _something_ readable: text first, then a
            // JSON-encoded `structuredContent`, then `{}` as a last
            // resort. Mirrors the same fallback in `resume.rs`.
            // `output` is populated only for a successful call. On any
            // failure (isError, transport/protocol error, timeout) the
            // message lives in `error` and `output` is `null` — matching
            // OpenAI's `MCPToolCall` schema (output/error are mutually
            // exclusive) and the resume path in `resume.rs`.
            let (status, output, error_text): (&str, Option<String>, Option<String>) =
                match &outcome {
                    Ok(Ok(result)) if !result.is_error => {
                        let text = collapse_result_text(result);
                        ("completed", Some(text), None)
                    }
                    Ok(Ok(result)) => {
                        let text = collapse_result_text(result);
                        let err = format!("MCP tool returned isError=true: {text}");
                        ("failed", None, Some(err))
                    }
                    Ok(Err(e)) => ("failed", None, Some(e.to_string())),
                    Err(_elapsed) => {
                        let err =
                            format!("MCP tool call timed out after {}s", call_timeout.as_secs());
                        tracing::warn!(
                            server_label = %binding_for_task.server_label,
                            tool = %tool_name,
                            timeout_secs = call_timeout.as_secs(),
                            "MCP tools/call timed out; surfacing incomplete call"
                        );
                        // The timed-out request was abandoned mid-flight
                        // without a protocol-level cancel, so the pooled
                        // session is now suspect. Evict it: the next call
                        // reconnects, and dropping the connection issues a
                        // clean rmcp cancel. See `McpService::evict_endpoint`.
                        service.evict_endpoint(
                            &binding_for_task.server_url,
                            binding_for_task.authorization.as_deref(),
                            &binding_for_task.headers,
                        );
                        // `incomplete` is the `MCPToolCallStatus` for a call
                        // that neither succeeded nor hard-failed but was cut
                        // short — exactly a timeout.
                        ("incomplete", None, Some(err))
                    }
                };

            // MCP-specific terminal lifecycle event SDKs key on. Spec
            // order: the lifecycle `completed`/`failed` precedes the
            // terminal `output_item.done`. There is no `.incomplete`
            // stream event in the spec, so a timeout rides the `.failed`
            // lifecycle event while the item itself carries the precise
            // `incomplete` status.
            let lifecycle_event = if error_text.is_some() {
                "response.mcp_call.failed"
            } else {
                "response.mcp_call.completed"
            };
            let _ = event_tx
                .send(sse_mcp_lifecycle_event(lifecycle_event, &call_item_id))
                .await;

            // Terminal `output_item.done` with `output` / `error`
            // inlined on the same `mcp_call` item — the spec has no
            // separate `mcp_call_output` type.
            if event_tx
                .send(sse_output_item(
                    "response.output_item.done",
                    mcp_call_item(
                        &call_item_id,
                        &binding_for_task.server_label,
                        &tool_name,
                        &raw_args,
                        status,
                        output.as_deref(),
                        error_text.as_deref(),
                        approval_req_for_task.as_deref(),
                    ),
                ))
                .await
                .is_err()
            {
                tracing::debug!(
                    server_label = %binding_for_task.server_label,
                    tool = %tool_name,
                    "MCP event consumer dropped before the terminal mcp_call item; \
                     the call result is still recorded for the continuation"
                );
            }

            let function_output_text = if let Some(err) = &error_text {
                serde_json::json!({"error": err}).to_string()
            } else {
                output.unwrap_or_default()
            };

            let continuation = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: crate::api_types::responses::FunctionCallOutputType::FunctionCallOutput,
                id: None,
                call_id: call_id.clone(),
                output: function_output_text,
                status: None,
            });

            let _ = result_tx.send(Ok(ToolCallResult {
                call_id,
                continuation_items: vec![continuation],
            }));
        });

        Ok(ToolExecutionHandle {
            events: Box::pin(futures_util::stream::unfold(
                event_rx,
                |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
            )),
            result: Box::pin(async move {
                result_rx.await.map_err(|_| {
                    ToolError::ExecutionFailed(
                        "MCP executor task dropped before sending result".to_string(),
                    )
                })?
            }),
        })
    }
}

#[async_trait]
impl ServerExecutedTool for McpExecutor {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn is_enabled_for(&self, payload: &CreateResponsesPayload) -> bool {
        // Enabled when the original payload has `mcp` tool entries.
        // Once the rewrite has run, those entries become function tools
        // — we still want to be enabled because the model will be
        // calling the rewritten names. So we engage iff the constructor
        // captured any bindings; the trait method's `payload` argument
        // is the original, and the rewrite happens *after* registration.
        let _ = payload;
        self.has_bindings()
    }

    fn detect(&self, event: &[u8], _ctx: &ToolContext) -> Vec<DetectedToolCall> {
        detect_in_chunk(event, &self.bindings)
    }

    /// Suppress the rewritten `function_call` plumbing from the
    /// client-facing stream. Under `hadrian_hosted` the model drives
    /// MCP via `mcp_<label>__<tool>` function tools, but the spec's
    /// output only contains `mcp_call` / `mcp_list_tools` /
    /// `mcp_approval_request` items — never the function calls. We emit
    /// the `mcp_call` items ourselves (see [`Self::execute`] /
    /// [`Self::prefix_events`]), so the underlying `function_call`
    /// `output_item.added` / `.done` and `function_call_arguments.*`
    /// events are dropped here (returned as empty bytes; the runner
    /// skips empty events). Detection and the continuation payload run
    /// off the *raw* upstream event, so suppressing the client copy
    /// doesn't break the server-tool loop.
    fn transform_event(&self, event: Bytes) -> Bytes {
        // Only the MCP-rewritten function tools (`mcp_<label>__<tool>`)
        // are ours to hide.
        self.suppressor
            .suppress(event, |name| parse_function_name(name).is_some())
    }

    fn prefix_events(&self) -> Vec<Bytes> {
        // One `mcp_list_tools` item per requesting server, populated
        // from the cache the preprocess rewrite just warmed up. Servers
        // whose catalog already appears in the caller's input (via
        // `previous_response_id`) are skipped — matches OpenAI's
        // "don't re-emit when the item is in context" behavior.
        let mut out = Vec::with_capacity(self.bindings.len() * 5);
        for binding in &self.bindings {
            if self.suppress_list_tools.contains(&binding.server_label) {
                continue;
            }
            let item_id = next_item_id("mcpl");

            // `output_item.added` placeholder — required by the spec
            // before the body and `output_item.done` events.
            let placeholder = serde_json::json!({
                "type": "mcp_list_tools",
                "id": item_id,
                "server_label": binding.server_label,
                "tools": [],
                "error": null,
            });
            out.push(sse_output_item("response.output_item.added", placeholder));
            out.push(sse_mcp_lifecycle_event(
                "response.mcp_list_tools.in_progress",
                &item_id,
            ));

            let catalog = self.service.cached_tools(
                &binding.server_url,
                binding.authorization.as_deref(),
                &binding.headers,
            );

            match catalog {
                Some(catalog) => {
                    let tools_json: Vec<Value> = catalog
                        .iter()
                        .map(|t| {
                            let mut obj = serde_json::json!({
                                "name": t.name,
                                "input_schema": t.input_schema,
                            });
                            if let Some(desc) = t.description.as_ref() {
                                obj["description"] = Value::from(desc.as_str());
                            }
                            if let Some(ann) = t.annotations.as_ref() {
                                obj["annotations"] = ann.clone();
                            }
                            obj
                        })
                        .collect();
                    let item = serde_json::json!({
                        "type": "mcp_list_tools",
                        "id": item_id,
                        "server_label": binding.server_label,
                        "tools": tools_json,
                        "error": null,
                    });
                    // Spec order: the lifecycle `completed` precedes the
                    // terminal `output_item.done`.
                    out.push(sse_mcp_lifecycle_event(
                        "response.mcp_list_tools.completed",
                        &item_id,
                    ));
                    out.push(sse_output_item("response.output_item.done", item));
                }
                None => {
                    // Cache miss — surface as the spec-shaped failed
                    // path with `error` inlined on the item. Prefer the
                    // verbatim upstream `tools/list` error when one was
                    // recorded; fall back to a generic message only when
                    // the catalog merely aged out of cache between the
                    // rewrite and stream start.
                    let error_message = self
                        .service
                        .cached_tools_error(
                            &binding.server_url,
                            binding.authorization.as_deref(),
                            &binding.headers,
                        )
                        .unwrap_or_else(|| "tools/list catalog unavailable".to_string());
                    let item = serde_json::json!({
                        "type": "mcp_list_tools",
                        "id": item_id,
                        "server_label": binding.server_label,
                        "tools": [],
                        "error": error_message,
                    });
                    out.push(sse_mcp_lifecycle_event(
                        "response.mcp_list_tools.failed",
                        &item_id,
                    ));
                    out.push(sse_output_item("response.output_item.done", item));
                }
            }
        }

        // Synthesize `mcp_call` items for any approvals the resume
        // path resolved on this request. The spec requires the
        // resumed response to carry an `mcp_call` linked to the
        // original `mcp_approval_request` via `approval_request_id`,
        // with `output` / `error` inlined. The model also gets the
        // same payload as a `function_call_output` in its input (so
        // it continues), but the `mcp_call` output item is what SDKs
        // and the persisted response history key off.
        for approval in &self.resumed_approvals {
            let item_id = next_item_id("mcp");
            // Args were serialized from a `Value` when the call was parked;
            // a parse failure means the stashed/stored row is corrupt.
            // Surface `null` args rather than fabricating, but log it.
            let raw_args: Value = serde_json::from_str(&approval.arguments_json)
                .unwrap_or_else(|e| {
                    tracing::error!(
                        approval_request_id = %approval.approval_request_id,
                        server_label = %approval.server_label,
                        tool = %approval.tool_name,
                        error = %e,
                        "Resumed MCP approval has unparseable arguments_json; emitting null arguments"
                    );
                    Value::Null
                });
            let status = if approval.error.is_some() {
                "failed"
            } else {
                "completed"
            };

            out.push(sse_output_item(
                "response.output_item.added",
                mcp_call_item(
                    &item_id,
                    &approval.server_label,
                    &approval.tool_name,
                    &raw_args,
                    // Match the live dispatch path: the args phase is done
                    // and (here) the call already executed during resume,
                    // so the initial item status is `calling`, not
                    // `in_progress`.
                    "calling",
                    None,
                    None,
                    Some(&approval.approval_request_id),
                ),
            ));
            out.push(sse_mcp_lifecycle_event(
                "response.mcp_call.in_progress",
                &item_id,
            ));
            // Spec order: lifecycle `completed`/`failed` precedes the
            // terminal `output_item.done`.
            let lifecycle = if approval.error.is_some() {
                "response.mcp_call.failed"
            } else {
                "response.mcp_call.completed"
            };
            out.push(sse_mcp_lifecycle_event(lifecycle, &item_id));
            out.push(sse_output_item(
                "response.output_item.done",
                mcp_call_item(
                    &item_id,
                    &approval.server_label,
                    &approval.tool_name,
                    &raw_args,
                    status,
                    approval.output.as_deref(),
                    approval.error.as_deref(),
                    Some(&approval.approval_request_id),
                ),
            ));
        }

        out
    }

    async fn execute(
        &self,
        call: DetectedToolCall,
        _ctx: &ToolContext,
    ) -> Result<ToolExecutionHandle, ToolError> {
        let sanitized_label = call
            .arguments
            .get("__mcp_label")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tool_name = call
            .arguments
            .get("__mcp_tool")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let raw_args = call
            .arguments
            .get("__mcp_args")
            .cloned()
            .unwrap_or(Value::Null);

        let binding = self
            .resolve_binding(&sanitized_label)
            .cloned()
            .ok_or_else(|| {
                ToolError::InvalidCall(format!(
                    "no MCP server binding for sanitized label '{sanitized_label}' \
                     (function_call was probably stale across a config change)"
                ))
            })?;

        // Approval gate. When the binding requires approval for this
        // tool, park the call instead of running it. The caller must
        // submit a matching `mcp_approval_response` on a follow-up
        // request to resume.
        let read_only_hint = self.read_only_hint_for(&binding, &tool_name);
        if binding.requires_approval(&tool_name, read_only_hint) {
            return self
                .park_for_approval(&binding, &call.call_id, &tool_name, &raw_args)
                .await;
        }

        self.dispatch_call(binding, call.call_id, tool_name, raw_args, None)
            .await
    }

    fn apply_to_continuation(
        &self,
        payload: &mut CreateResponsesPayload,
        results: &[ToolCallResult],
        is_final_iteration: bool,
    ) {
        let outputs: Vec<ResponsesInputItem> = results
            .iter()
            .flat_map(|r| r.continuation_items.clone())
            .collect();
        if outputs.is_empty() && !is_final_iteration {
            return;
        }

        if !outputs.is_empty() {
            match payload.input {
                Some(ResponsesInput::Items(ref mut items)) => items.extend(outputs),
                Some(ResponsesInput::Text(ref text)) => {
                    let text = text.clone();
                    let mut items = vec![ResponsesInputItem::EasyMessage(
                        crate::api_types::responses::EasyInputMessage {
                            type_: None,
                            role: crate::api_types::responses::EasyInputMessageRole::User,
                            content: crate::api_types::responses::EasyInputMessageContent::Text(
                                text,
                            ),
                        },
                    )];
                    items.extend(outputs);
                    payload.input = Some(ResponsesInput::Items(items));
                }
                None => {
                    payload.input = Some(ResponsesInput::Items(outputs));
                }
            }
        }

        // On the final iteration, strip every function tool that came
        // from the MCP rewrite so the model has to produce a text
        // response. Detected by the `mcp_` prefix on the function name.
        if is_final_iteration && let Some(ref mut tools) = payload.tools {
            tools.retain(|t| match t {
                ResponsesToolDefinition::Function(f) => parse_function_name(&f.name).is_none(),
                _ => true,
            });
            if tools.is_empty() {
                payload.tools = None;
            }
        }
    }
}

/// Collapse `McpCallResult` to the single string `mcp_call.output`
/// requires. OpenAI's spec types `output` as a plain string and gives
/// no guidance on non-text MCP content; we fall through in priority
/// order so the model always sees *something* instead of an empty
/// payload that would make it look like the call returned nothing:
///
/// 1. Concatenated text blocks (the common case).
/// 2. JSON-encoded `structuredContent` (richer return shape).
/// 3. JSON-encoded non-text blocks (image / audio / resource) — kept
///    verbatim from the MCP server so the model at least sees the
///    shape and can react ("the tool returned an image with this
///    mime type and base64 data"). Better than silently dropping.
/// 4. `"{}"` as a last resort.
pub(super) fn collapse_result_text(result: &McpCallResult) -> String {
    if !result.text.is_empty() {
        return result.text.clone();
    }
    if let Some(structured) = result.structured_content.as_ref() {
        return structured.to_string();
    }
    if !result.extra_content.is_empty() {
        let blocks: Vec<Value> = result
            .extra_content
            .iter()
            .map(|c| {
                serde_json::json!({
                    "type": c.kind,
                    "data": c.value,
                })
            })
            .collect();
        return Value::Array(blocks).to_string();
    }
    "{}".to_string()
}

/// Run one tool call against the remote server. Wraps service errors
/// in a friendly string for SSE surfacing.
async fn run_call(
    service: &McpService,
    binding: &ServerBinding,
    tool_name: &str,
    arguments: Value,
) -> Result<McpCallResult, super::McpClientError> {
    service
        .call_tool(
            &binding.server_url,
            binding.authorization.as_deref(),
            &binding.headers,
            tool_name,
            arguments,
        )
        .await
}

/// Scan one SSE event for `response.output_item.done` carrying a
/// `function_call` whose name matches the MCP rewrite. Returns a
/// `DetectedToolCall` per match. The runner hands us one complete SSE
/// event (terminated by a blank line) at a time via
/// [`crate::streaming::sse_buffer::SseBuffer`].
fn detect_in_chunk(chunk: &[u8], bindings: &[ServerBinding]) -> Vec<DetectedToolCall> {
    let Some(data) = extract_sse_data(chunk) else {
        return Vec::new();
    };
    let trimmed = data.trim();
    if trimmed == "[DONE]" {
        return Vec::new();
    }
    let Ok(json) = serde_json::from_str::<Value>(trimmed) else {
        return Vec::new();
    };
    if json.get("type").and_then(|t| t.as_str()) != Some("response.output_item.done") {
        return Vec::new();
    }
    let Some(item) = json.get("item") else {
        return Vec::new();
    };
    if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
        return Vec::new();
    }
    let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    let Some((sanitized_label, tool_name)) = parse_function_name(name) else {
        return Vec::new();
    };
    if !bindings
        .iter()
        .any(|b| b.sanitized_label == sanitized_label)
    {
        // Function name matches the prefix but no binding — could
        // be a stale call from a prior turn after config changed.
        // Don't emit; let the runner pass it through unchanged.
        warn!(
            tool = %name,
            "MCP-shaped function call has no matching binding; ignoring"
        );
        return Vec::new();
    }
    let Some(call_id) = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
    else {
        // No `call_id`/`id` means we can't pair the continuation
        // `function_call_output` back to this call. Fabricating a
        // placeholder would let two malformed calls collide on the join
        // key; skip instead and let the runner pass the event through.
        warn!(
            tool = %name,
            "MCP-shaped function call missing both `call_id` and `id`; ignoring (cannot pair continuation)"
        );
        return Vec::new();
    };
    let call_id = call_id.to_string();
    // Model emits `arguments` as a JSON-encoded string. Parse it
    // so the executor can serve structured args to the MCP server.
    let raw_args_str = item
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let parsed_args: Value = serde_json::from_str(raw_args_str).unwrap_or(Value::Null);

    vec![DetectedToolCall {
        tool_name: "mcp",
        call_id,
        arguments: serde_json::json!({
            "__mcp_label": sanitized_label,
            "__mcp_tool": tool_name,
            "__mcp_args": parsed_args,
        }),
    }]
}

/// Concatenate every `data:` field on an SSE event into a single
/// string, joined with `\n`. Per the [HTML SSE spec][1], one event can
/// carry multiple `data:` lines and the dispatched payload is their
/// `\n`-joined concatenation — line-by-line JSON parsing (which the
/// previous implementation did) drops valid events where a provider
/// chose to wrap a long JSON across multiple `data:` lines.
///
/// Returns `None` if the event has no `data:` fields, or if the bytes
/// aren't UTF-8.
///
/// [1]: https://html.spec.whatwg.org/multipage/server-sent-events.html#dispatchMessage
fn extract_sse_data(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        // CRLF framing: strip the trailing '\r' from the line. Bare LF
        // framing leaves the line untouched.
        let line = line.strip_suffix('\r').unwrap_or(line);
        // Empty line is the event terminator (the SseBuffer already
        // delimited on this, but a final blank line can still appear
        // inside the slice). Comments start with `:`. Skip both.
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        // Other field types (`event:`, `id:`, `retry:`) are ignored —
        // we only consume `data:` payloads.
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        // Per spec, a single leading space after the colon is stripped.
        // Any subsequent leading whitespace is preserved.
        let value = rest.strip_prefix(' ').unwrap_or(rest);
        parts.push(value);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n"))
}

/// `output_index` / `sequence_number` are emitted as `0` placeholders:
/// the pipeline always runs the `StreamRewriter`, which stamps one
/// monotonic `sequence_number` across the whole stream and remaps
/// `output_index` to a stable per-item value (see
/// `server_tools::runner::StreamRewriter::rewrite`). Computing them here
/// would be dead work — the rewriter overwrites both. The `output_index`
/// key must still be present, since the rewriter only remaps events that
/// already carry it.
fn sse_output_item(event_type: &str, item: Value) -> Bytes {
    let payload = serde_json::json!({
        "type": event_type,
        "output_index": 0,
        "sequence_number": 0,
        "item": item,
    });
    let s = serde_json::to_string(&payload).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", s))
}

/// `response.mcp_call.in_progress` / `.completed` / `.failed`. No
/// payload other than the locators — SDKs use this to fire MCP-specific
/// state transitions; the item content lives on the matching
/// `output_item.added` / `.done`.
fn sse_mcp_lifecycle_event(event_type: &str, item_id: &str) -> Bytes {
    // `output_index` / `sequence_number` are `0` placeholders the
    // `StreamRewriter` overwrites — see `sse_output_item`.
    let payload = serde_json::json!({
        "type": event_type,
        "output_index": 0,
        "sequence_number": 0,
        "item_id": item_id,
    });
    let s = serde_json::to_string(&payload).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", s))
}

/// `response.mcp_call_arguments.delta` / `.done`. Carries the
/// model-supplied arguments JSON. Under hadrian_hosted there's no
/// incremental source, so the `.delta` is a single chunk holding the
/// full string and `.done` echoes it.
fn sse_mcp_arguments_event(event_type: &str, item_id: &str, arguments: Option<&str>) -> Bytes {
    let key = if event_type.ends_with(".delta") {
        "delta"
    } else {
        "arguments"
    };
    // `output_index` / `sequence_number` are `0` placeholders the
    // `StreamRewriter` overwrites — see `sse_output_item`.
    let payload = serde_json::json!({
        "type": event_type,
        "output_index": 0,
        "sequence_number": 0,
        "item_id": item_id,
        key: arguments.unwrap_or(""),
    });
    let s = serde_json::to_string(&payload).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", s))
}

/// Build a fully-shaped `mcp_call` item. `output` / `error` live on the
/// item itself per OpenAI's `MCPToolCall` schema — there is no separate
/// `mcp_call_output` type. `approval_request_id`, when set, links back
/// to the `mcp_approval_request` that gated this call.
#[allow(clippy::too_many_arguments)]
fn mcp_call_item(
    item_id: &str,
    server_label: &str,
    tool_name: &str,
    arguments: &Value,
    status: &str,
    output: Option<&str>,
    error: Option<&str>,
    approval_request_id: Option<&str>,
) -> Value {
    // OpenAI's `MCPToolCall` always carries `output`, `error`, and
    // `approval_request_id` keys — `null` when unset, not omitted — so
    // SDKs can read them unconditionally. Mirror that exactly.
    serde_json::json!({
        "type": "mcp_call",
        "id": item_id,
        "server_label": server_label,
        "name": tool_name,
        "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
        "status": status,
        "output": output.map(Value::from).unwrap_or(Value::Null),
        "error": error.map(Value::from).unwrap_or(Value::Null),
        "approval_request_id": approval_request_id.map(Value::from).unwrap_or(Value::Null),
    })
}

/// Drain any approvals the resume path stashed for this org on this
/// request. The join key is the `call_id` of every `function_call_output`
/// in `payload.input` — those are the items the resume just wrote, and
/// each has a matching stashed `ResolvedMcpApproval`.
///
/// Returns an empty Vec when `org_id` is unknown (no auth scope) or
/// when the resume path didn't stash anything (the common case — most
/// requests aren't resumes).
fn drain_resumed_approvals(
    service: &McpService,
    org_id: Option<uuid::Uuid>,
    payload: &CreateResponsesPayload,
) -> Vec<super::ResolvedMcpApproval> {
    let Some(org_id) = org_id else {
        return Vec::new();
    };
    let Some(ResponsesInput::Items(items)) = payload.input.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        if let ResponsesInputItem::FunctionCallOutput(fco) = item
            && let Some(approval) = service.take_resolved_approval(org_id, &fco.call_id)
        {
            out.push(approval);
        }
    }
    out
}

/// Walk `payload.input` for `mcp_list_tools` items, returning the set
/// of `server_label`s already present in the caller's context. Used
/// to suppress re-emission of the catalog on follow-up turns — matches
/// OpenAI's "don't refetch when the item is already in context" rule.
fn collect_inlined_list_tools_labels(
    payload: &CreateResponsesPayload,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Some(input) = payload.input.as_ref() else {
        return out;
    };
    let ResponsesInput::Items(items) = input else {
        return out;
    };
    for item in items {
        if let ResponsesInputItem::McpListTools(it) = item {
            out.insert(it.server_label.clone());
        }
    }
    out
}

/// Synthesize a globally-unique output-item id (`mcp_`, `mcpl_`, …),
/// matching OpenAI's `<prefix>_<random hex>` scheme. A UUID rather than
/// a per-response counter so ids don't collide across responses when
/// `store=true` — and so we stay consistent with the rest of the
/// pipeline, which also uses `Uuid::new_v4().simple()` for synthesized
/// ids.
fn next_item_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Build the spec-shaped `mcp_approval_request` item. The
/// `approval_request_id` (item `id`) is what the caller echoes back
/// on the matching `mcp_approval_response` to resume.
fn mcp_approval_request_item(
    approval_id: &str,
    server_label: &str,
    tool_name: &str,
    arguments_json: &str,
) -> Value {
    serde_json::json!({
        "type": "mcp_approval_request",
        "id": approval_id,
        "server_label": server_label,
        "name": tool_name,
        "arguments": arguments_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::responses::McpToolType;

    fn mcp_with(label: &str, url: &str) -> McpTool {
        McpTool {
            type_: McpToolType::Mcp,
            server_label: label.to_string(),
            server_url: Some(url.to_string()),
            connector_id: None,
            server_description: None,
            authorization: None,
            headers: None,
            require_approval: None,
            allowed_tools: None,
            defer_loading: None,
            defer_loading_passthrough: None,
            call_timeout_secs: None,
        }
    }

    #[test]
    fn server_binding_captures_sanitized_label() {
        let b = ServerBinding::from_mcp(
            &mcp_with("My Co/Linear", "https://x"),
            DEFAULT_CALL_TIMEOUT_SECS,
        )
        .unwrap();
        assert_eq!(b.server_label, "My Co/Linear");
        assert_eq!(b.sanitized_label, "My_Co_Linear");
    }

    #[test]
    fn server_binding_resolves_call_timeout() {
        // No per-tool override → deployment default applies.
        let b = ServerBinding::from_mcp(&mcp_with("atlassian", "https://x"), 42).unwrap();
        assert_eq!(b.call_timeout, std::time::Duration::from_secs(42));

        // Per-tool `call_timeout_secs` extension wins over the default.
        let mut tool = mcp_with("atlassian", "https://x");
        tool.call_timeout_secs = Some(7);
        let b = ServerBinding::from_mcp(&tool, 42).unwrap();
        assert_eq!(b.call_timeout, std::time::Duration::from_secs(7));
    }

    #[test]
    fn detect_skips_non_mcp_function_calls() {
        let bindings = vec![
            ServerBinding::from_mcp(
                &mcp_with("atlassian", "https://x"),
                DEFAULT_CALL_TIMEOUT_SECS,
            )
            .unwrap(),
        ];
        let chunk = br#"data: {"type":"response.output_item.done","item":{"type":"function_call","name":"shell","arguments":"{}","call_id":"c1"}}"#;
        let calls = detect_in_chunk(chunk, &bindings);
        assert!(calls.is_empty());
    }

    #[test]
    fn detect_picks_up_mcp_function_calls() {
        let bindings = vec![
            ServerBinding::from_mcp(
                &mcp_with("atlassian", "https://x"),
                DEFAULT_CALL_TIMEOUT_SECS,
            )
            .unwrap(),
        ];
        let chunk = br#"data: {"type":"response.output_item.done","item":{"type":"function_call","name":"mcp_atlassian__jira_search","arguments":"{\"query\":\"bugs\"}","call_id":"c1"}}"#;
        let calls = detect_in_chunk(chunk, &bindings);
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.tool_name, "mcp");
        assert_eq!(c.call_id, "c1");
        assert_eq!(c.arguments.get("__mcp_label").unwrap(), "atlassian");
        assert_eq!(c.arguments.get("__mcp_tool").unwrap(), "jira_search");
        assert_eq!(
            c.arguments.get("__mcp_args").unwrap().get("query").unwrap(),
            "bugs"
        );
    }

    #[test]
    fn detect_ignores_unknown_label() {
        let bindings = vec![
            ServerBinding::from_mcp(
                &mcp_with("atlassian", "https://x"),
                DEFAULT_CALL_TIMEOUT_SECS,
            )
            .unwrap(),
        ];
        let chunk = br#"data: {"type":"response.output_item.done","item":{"type":"function_call","name":"mcp_notion__something","arguments":"{}","call_id":"c1"}}"#;
        let calls = detect_in_chunk(chunk, &bindings);
        assert!(calls.is_empty());
    }

    #[test]
    fn detect_handles_multiline_data_fields() {
        // Per the SSE spec, a single event can carry the JSON payload
        // across multiple `data:` lines; the actual data is the
        // `\n`-joined concatenation. The previous line-by-line parser
        // would have dropped this event.
        let bindings = vec![
            ServerBinding::from_mcp(
                &mcp_with("atlassian", "https://x"),
                DEFAULT_CALL_TIMEOUT_SECS,
            )
            .unwrap(),
        ];
        let chunk = b"data: {\"type\":\"response.output_item.done\",\"item\":{\n\
                      data: \"type\":\"function_call\",\n\
                      data: \"name\":\"mcp_atlassian__jira_search\",\n\
                      data: \"arguments\":\"{\\\"q\\\":\\\"bugs\\\"}\",\n\
                      data: \"call_id\":\"c1\"}}\n\n";
        let calls = detect_in_chunk(chunk, &bindings);
        assert_eq!(calls.len(), 1, "multi-line data: should reconstruct");
        assert_eq!(calls[0].call_id, "c1");
        assert_eq!(calls[0].arguments.get("__mcp_tool").unwrap(), "jira_search");
    }

    #[test]
    fn detect_handles_crlf_framing() {
        // Some upstream HTTP stacks emit `\r\n` line terminators. The
        // parser must strip the trailing `\r` before matching `data:`.
        let bindings = vec![
            ServerBinding::from_mcp(
                &mcp_with("atlassian", "https://x"),
                DEFAULT_CALL_TIMEOUT_SECS,
            )
            .unwrap(),
        ];
        let chunk = b"data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"name\":\"mcp_atlassian__jira_search\",\"arguments\":\"{}\",\"call_id\":\"c1\"}}\r\n\r\n";
        let calls = detect_in_chunk(chunk, &bindings);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "c1");
    }

    #[test]
    fn detect_skips_comment_and_other_fields() {
        // `:` comments and non-`data:` fields (`event:`, `id:`) must
        // be ignored — only `data:` contributes to the payload.
        let bindings = vec![
            ServerBinding::from_mcp(
                &mcp_with("atlassian", "https://x"),
                DEFAULT_CALL_TIMEOUT_SECS,
            )
            .unwrap(),
        ];
        let chunk = b": keepalive\n\
                      event: response.output_item.done\n\
                      id: 42\n\
                      data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"name\":\"mcp_atlassian__jira_search\",\"arguments\":\"{}\",\"call_id\":\"c1\"}}\n\n";
        let calls = detect_in_chunk(chunk, &bindings);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn detect_returns_empty_on_done_sentinel() {
        let bindings = vec![
            ServerBinding::from_mcp(
                &mcp_with("atlassian", "https://x"),
                DEFAULT_CALL_TIMEOUT_SECS,
            )
            .unwrap(),
        ];
        let chunk = b"data: [DONE]\n\n";
        let calls = detect_in_chunk(chunk, &bindings);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_sse_data_joins_with_newline() {
        // Verify the join character matches the SSE spec — concatenated
        // with `\n`, not `\r\n` and not empty string.
        let chunk = b"data: line1\ndata: line2\ndata: line3\n\n";
        let joined = extract_sse_data(chunk).unwrap();
        assert_eq!(joined, "line1\nline2\nline3");
    }

    #[test]
    fn extract_sse_data_preserves_subsequent_spaces() {
        // Per spec, only the *first* leading space after `data:` is
        // stripped — additional whitespace is part of the payload.
        let chunk = b"data:  with-two-leading-spaces\n\n";
        let joined = extract_sse_data(chunk).unwrap();
        assert_eq!(joined, " with-two-leading-spaces");
    }

    #[test]
    fn mcp_call_item_inlines_output_and_error() {
        let v = mcp_call_item(
            "mcp_1",
            "atlassian",
            "jira_search",
            &serde_json::json!({"q": "bugs"}),
            "completed",
            Some("{\"hits\":2}"),
            None,
            Some("mcpr_x"),
        );
        assert_eq!(v["type"], "mcp_call");
        assert_eq!(v["server_label"], "atlassian");
        let args = v["arguments"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(args).unwrap();
        assert_eq!(parsed["q"], "bugs");
        assert_eq!(v["output"], "{\"hits\":2}");
        assert_eq!(v["approval_request_id"], "mcpr_x");
        // `error` is always present per OpenAI's schema — `null`, not omitted.
        assert_eq!(v["error"], Value::Null);
    }

    #[test]
    fn binding_requires_approval_handles_every_shape() {
        use crate::api_types::responses::{
            McpApprovalFilter, McpApprovalMode, McpRequireApproval, McpToolFilter,
        };

        let mut tool = mcp_with("atlassian", "https://x");

        // No policy → spec default is `"always"`, so gate.
        tool.require_approval = None;
        let b = ServerBinding::from_mcp(&tool, DEFAULT_CALL_TIMEOUT_SECS).unwrap();
        assert!(b.requires_approval("jira_create", None));

        // Mode::Never → never.
        tool.require_approval = Some(McpRequireApproval::Mode(McpApprovalMode::Never));
        let b = ServerBinding::from_mcp(&tool, DEFAULT_CALL_TIMEOUT_SECS).unwrap();
        assert!(!b.requires_approval("jira_create", None));

        // Mode::Always → every tool.
        tool.require_approval = Some(McpRequireApproval::Mode(McpApprovalMode::Always));
        let b = ServerBinding::from_mcp(&tool, DEFAULT_CALL_TIMEOUT_SECS).unwrap();
        assert!(b.requires_approval("jira_search", None));
        assert!(b.requires_approval("jira_create", None));

        // Filter with `never` exempts a subset; everything else stays gated.
        tool.require_approval = Some(McpRequireApproval::Filter(McpApprovalFilter {
            always: None,
            never: Some(McpToolFilter {
                tool_names: Some(vec!["jira_search".into()]),
                read_only: None,
            }),
        }));
        let b = ServerBinding::from_mcp(&tool, DEFAULT_CALL_TIMEOUT_SECS).unwrap();
        assert!(!b.requires_approval("jira_search", None));
        assert!(b.requires_approval("jira_create", None));

        // `never` with `read_only: true` exempts tools whose readOnlyHint
        // is explicitly true; mutating tools still gate.
        tool.require_approval = Some(McpRequireApproval::Filter(McpApprovalFilter {
            always: None,
            never: Some(McpToolFilter {
                tool_names: None,
                read_only: Some(true),
            }),
        }));
        let b = ServerBinding::from_mcp(&tool, DEFAULT_CALL_TIMEOUT_SECS).unwrap();
        assert!(!b.requires_approval("jira_search", Some(true)));
        assert!(b.requires_approval("jira_create", Some(false)));
        // A tool with NO readOnlyHint annotation must not match a
        // `read_only: true` filter — absence is not `false`, so it gates.
        assert!(b.requires_approval("jira_unknown", None));

        // `always` explicitly gates a subset and exempts the rest via
        // implicit default — actually the spec default for the filter
        // object is "gate", so non-listed tools still gate.
        tool.require_approval = Some(McpRequireApproval::Filter(McpApprovalFilter {
            always: Some(McpToolFilter {
                tool_names: Some(vec!["jira_create".into()]),
                read_only: None,
            }),
            never: None,
        }));
        let b = ServerBinding::from_mcp(&tool, DEFAULT_CALL_TIMEOUT_SECS).unwrap();
        assert!(b.requires_approval("jira_create", None));
        // Non-listed tools fall through to the default — gate.
        assert!(b.requires_approval("jira_search", None));
    }

    #[test]
    fn collapse_result_text_prefers_text_then_structured_then_extras() {
        use super::super::McpCallContent;

        // Text wins.
        let r = McpCallResult {
            is_error: false,
            text: "hello".into(),
            structured_content: Some(serde_json::json!({"k": "v"})),
            extra_content: vec![],
        };
        assert_eq!(collapse_result_text(&r), "hello");

        // No text → structured.
        let r = McpCallResult {
            is_error: false,
            text: String::new(),
            structured_content: Some(serde_json::json!({"k": "v"})),
            extra_content: vec![],
        };
        assert_eq!(collapse_result_text(&r), r#"{"k":"v"}"#);

        // No text + no structured → JSON-encode extras (image/audio/resource)
        // so the model sees *something* rather than an empty payload.
        let r = McpCallResult {
            is_error: false,
            text: String::new(),
            structured_content: None,
            extra_content: vec![McpCallContent {
                kind: "image".into(),
                value: serde_json::json!({"mimeType": "image/png", "data": "..."}),
            }],
        };
        let s = collapse_result_text(&r);
        assert!(s.contains("\"image\""), "got: {s}");
        assert!(s.contains("image/png"), "got: {s}");

        // Nothing at all → "{}" placeholder.
        let r = McpCallResult {
            is_error: false,
            text: String::new(),
            structured_content: None,
            extra_content: vec![],
        };
        assert_eq!(collapse_result_text(&r), "{}");
    }

    #[test]
    fn drain_resumed_approvals_matches_function_call_outputs_in_input() {
        use crate::api_types::responses::{
            FunctionCallOutput, FunctionCallOutputType, ResponsesInput, ResponsesInputItem,
        };

        let svc = McpService::new();
        let org_id = uuid::Uuid::new_v4();
        let other_org = uuid::Uuid::new_v4();

        // Stash two approvals: one matching, one for a call_id not in payload.
        svc.stash_resolved_approval(
            org_id,
            super::super::ResolvedMcpApproval {
                call_id: "call_match".into(),
                approval_request_id: "mcpr_1".into(),
                server_label: "atlassian".into(),
                tool_name: "jira_search".into(),
                arguments_json: r#"{"q":"x"}"#.into(),
                output: Some("ok".into()),
                error: None,
            },
        );
        svc.stash_resolved_approval(
            org_id,
            super::super::ResolvedMcpApproval {
                call_id: "call_stale".into(),
                approval_request_id: "mcpr_2".into(),
                server_label: "atlassian".into(),
                tool_name: "jira_search".into(),
                arguments_json: "{}".into(),
                output: None,
                error: Some("err".into()),
            },
        );
        // Different org — must not leak across tenants.
        svc.stash_resolved_approval(
            other_org,
            super::super::ResolvedMcpApproval {
                call_id: "call_match".into(),
                approval_request_id: "mcpr_3".into(),
                server_label: "atlassian".into(),
                tool_name: "jira_search".into(),
                arguments_json: "{}".into(),
                output: Some("other-org".into()),
                error: None,
            },
        );

        let payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({})).unwrap();
        let mut payload = payload;
        payload.input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: FunctionCallOutputType::FunctionCallOutput,
                id: None,
                call_id: "call_match".into(),
                output: "ok".into(),
                status: None,
            }),
            ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: FunctionCallOutputType::FunctionCallOutput,
                id: None,
                call_id: "call_no_match".into(),
                output: "nope".into(),
                status: None,
            }),
        ]));

        let drained = super::drain_resumed_approvals(&svc, Some(org_id), &payload);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].call_id, "call_match");
        assert_eq!(drained[0].approval_request_id, "mcpr_1");

        // Stash for `call_match` was consumed (one-shot) and other_org's
        // entry is untouched.
        assert!(svc.take_resolved_approval(org_id, "call_match").is_none());
        assert!(svc.take_resolved_approval(org_id, "call_stale").is_some());
        assert!(
            svc.take_resolved_approval(other_org, "call_match")
                .is_some()
        );
    }

    #[test]
    fn approval_request_item_has_expected_shape() {
        let item = mcp_approval_request_item(
            "mcpr_abc",
            "atlassian",
            "jira_create",
            r#"{"summary":"bug"}"#,
        );
        assert_eq!(item["type"], "mcp_approval_request");
        assert_eq!(item["id"], "mcpr_abc");
        assert_eq!(item["server_label"], "atlassian");
        assert_eq!(item["name"], "jira_create");
        assert_eq!(item["arguments"], r#"{"summary":"bug"}"#);
    }

    #[tokio::test]
    async fn park_for_approval_fails_closed_without_persistence() {
        use futures_util::StreamExt;

        use crate::api_types::responses::{McpApprovalMode, McpRequireApproval};

        // Build an executor with no persistence: no DB on the service,
        // no response_id, no org_id. require_approval = "always" forces
        // the gate to engage. The fail-closed path should produce a
        // synthesized failed mcp_call rather than running the call.
        let mut tool = mcp_with("atlassian", "https://x");
        tool.require_approval = Some(McpRequireApproval::Mode(McpApprovalMode::Always));
        let payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "tools": [serde_json::to_value(tool).unwrap()],
        }))
        .unwrap();

        let service = McpService::new();
        let executor =
            McpExecutor::with_persistence(service, &payload, None, None, DEFAULT_CALL_TIMEOUT_SECS);

        let call = DetectedToolCall {
            tool_name: "mcp",
            call_id: "c1".into(),
            arguments: serde_json::json!({
                "__mcp_label": "atlassian",
                "__mcp_tool": "jira_create",
                "__mcp_args": {"summary": "bug"},
            }),
        };
        let ctx = ToolContext {
            original_payload: payload,
        };
        let handle = executor.execute(call, &ctx).await.expect("handle returned");

        // Drain events; expect a failed mcp_call lifecycle.
        let mut events = handle.events;
        let mut payloads: Vec<serde_json::Value> = Vec::new();
        let mut lifecycle_types: Vec<String> = Vec::new();
        while let Some(bytes) = events.next().await {
            let text = std::str::from_utf8(&bytes).unwrap();
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    let v: serde_json::Value = serde_json::from_str(rest.trim()).unwrap();
                    if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                        if t.starts_with("response.mcp_call.") {
                            lifecycle_types.push(t.to_string());
                        }
                        if t == "response.output_item.done" {
                            payloads.push(v["item"].clone());
                        }
                    }
                }
            }
        }

        // The lifecycle ends in `failed`, not `completed`.
        assert!(
            lifecycle_types
                .iter()
                .any(|t| t == "response.mcp_call.failed"),
            "expected response.mcp_call.failed lifecycle, got {:?}",
            lifecycle_types
        );
        // The terminal item has status=failed and a non-empty error
        // mentioning the gate.
        let terminal = payloads.last().expect("a terminal item was emitted");
        assert_eq!(terminal["status"], "failed");
        let err = terminal["error"].as_str().expect("error field set");
        assert!(
            err.contains("require_approval"),
            "expected the error to mention the gate, got: {err}"
        );

        // The continuation function_call_output also surfaces the error
        // so the model sees it on its next turn.
        let result = handle.result.await.expect("result resolves");
        assert_eq!(result.call_id, "c1");
        assert_eq!(result.continuation_items.len(), 1);
        let cont = &result.continuation_items[0];
        if let ResponsesInputItem::FunctionCallOutput(fco) = cont {
            assert_eq!(fco.call_id, "c1");
            let parsed: serde_json::Value = serde_json::from_str(&fco.output).unwrap();
            assert!(
                parsed.get("error").is_some(),
                "continuation should carry an error field"
            );
        } else {
            panic!("expected FunctionCallOutput continuation");
        }
    }

    fn executor() -> McpExecutor {
        let payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "tools": [{"type":"mcp","server_label":"atlassian","server_url":"https://x"}]
        }))
        .unwrap();
        McpExecutor::new(McpService::new(), &payload)
    }

    #[test]
    fn transform_suppresses_mcp_function_call_items() {
        use crate::services::server_tools::ServerExecutedTool;
        let exec = executor();
        // The rewritten function_call must not leak to the client — the
        // executor emits the spec-shaped `mcp_call` itself.
        let added = Bytes::from(
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","id":"fc_1","name":"mcp_atlassian__jira_search","arguments":""}}"#.to_string() + "\n\n",
        );
        assert!(exec.transform_event(added).is_empty());

        // Argument-streaming events for that item id are suppressed too.
        let delta = Bytes::from(
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{"}"#.to_string() + "\n\n",
        );
        assert!(exec.transform_event(delta).is_empty());

        let done = Bytes::from(
            r#"data: {"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","name":"mcp_atlassian__jira_search","arguments":"{}"}}"#.to_string() + "\n\n",
        );
        assert!(exec.transform_event(done).is_empty());
    }

    #[test]
    fn transform_passes_through_non_mcp_and_synthesized_items() {
        use crate::services::server_tools::ServerExecutedTool;
        let exec = executor();
        // A regular (non-MCP) function call is left alone.
        let other = Bytes::from(
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","id":"fc_2","name":"get_weather","arguments":""}}"#.to_string() + "\n\n",
        );
        assert_eq!(exec.transform_event(other.clone()), other);

        // The executor's own mcp_call item must pass through untouched.
        let mcp_call = Bytes::from(
            r#"data: {"type":"response.output_item.done","item":{"type":"mcp_call","id":"mcp_1","name":"t","arguments":"{}","status":"completed"}}"#.to_string() + "\n\n",
        );
        assert_eq!(exec.transform_event(mcp_call.clone()), mcp_call);

        // Arg events for an untracked item id pass through.
        let delta = Bytes::from(
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_2","delta":"{"}"#.to_string() + "\n\n",
        );
        assert_eq!(exec.transform_event(delta.clone()), delta);
    }
}
