//! Hadrian-side tool search for deferred MCP tools (`defer_loading`).
//!
//! Under `hadrian_hosted`, when a request marks an `mcp` tool entry with
//! `defer_loading: true` (and does not opt into native passthrough), the
//! rewrite keeps that server's catalog out of the prompt and exposes a
//! single `tool_search` function tool instead. The model calls it with a
//! query; [`ToolSearchExecutor`] ranks the retained catalog locally
//! ([`ranker`]), emits spec-shaped `tool_search_call` / `tool_search_output`
//! items, and injects the matched per-tool function definitions into the
//! continuation so the model can actually call them next turn.
//!
//! This makes `defer_loading` work behind every provider — not just the
//! ones with native tool search — mirroring OpenAI's wire contract.

pub mod ranker;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use bytes::Bytes;
pub use ranker::{
    HybridRanker, LexicalRanker, RankError, RankedTool, SemanticRanker, ToolSearchRanker,
};
use serde_json::Value;

use super::{McpService, McpToolMeta, preprocess};
use crate::{
    api_types::responses::{
        CreateResponsesPayload, FunctionCallOutput, FunctionCallOutputType, McpTool,
        ResponsesInputItem, ResponsesToolChoice, ResponsesToolDefinition, ToolSearchRankerKind,
    },
    cache::EmbeddingService,
    config::ToolSearchConfig,
    services::server_tools::{
        DetectedToolCall, ServerExecutedTool, ToolCallResult, ToolContext, ToolError,
        ToolExecutionHandle,
    },
};

/// Name of the synthetic function tool the rewrite injects for deferred
/// MCP servers. The model calls it to discover tools; [`ToolSearchExecutor`]
/// intercepts the call. Distinct from the `mcp_<label>__<tool>` shape so
/// detection is unambiguous.
pub const TOOL_SEARCH_FUNCTION_NAME: &str = "tool_search";

/// The [`ServerExecutedTool`] that answers `tool_search` calls locally:
/// ranks the deferred MCP catalog, emits `tool_search_call` /
/// `tool_search_output` items, and injects the matched per-tool function
/// definitions into the continuation so the model can call them.
///
/// Constructed once per request from the *original* payload (before the
/// rewrite strips `mcp` entries), alongside [`super::McpExecutor`].
pub struct ToolSearchExecutor {
    service: McpService,
    /// The deferred-default MCP servers, captured before rewrite.
    deferred: Vec<McpTool>,
    /// Ranking strategy resolved at construction from config + per-request
    /// override + embedding availability.
    ranker: Arc<dyn ToolSearchRanker>,
    max_results: usize,
    score_threshold: f64,
    output_index: AtomicU64,
    sequence_number: AtomicU64,
    /// Function tools discovered by `execute`, awaiting injection into the
    /// continuation payload by `apply_to_continuation`.
    pending_tools: Mutex<Vec<ResponsesToolDefinition>>,
    suppressor: crate::services::server_tools::FunctionCallSuppressor,
}

impl ToolSearchExecutor {
    /// Build the executor. Returns one even when no server is deferred;
    /// [`Self::is_enabled_for`] gates registration.
    pub fn new(
        service: McpService,
        original_payload: &CreateResponsesPayload,
        cfg: &ToolSearchConfig,
        embeddings: Option<Arc<EmbeddingService>>,
    ) -> Self {
        let forced = mcp_choice_label(original_payload);
        let deferred: Vec<McpTool> = original_payload
            .tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t.as_mcp())
                    .filter(|m| {
                        m.defer_loading == Some(true)
                            && m.defer_loading_passthrough != Some(true)
                            && forced.as_deref() != Some(m.server_label.as_str())
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        // Per-request ranker override (a caller-supplied `tool_search`
        // tool entry) wins over the deployment default.
        let request_override = original_payload
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find_map(|t| t.as_tool_search()))
            .and_then(|ts| ts.ranker);
        let effective = request_override.unwrap_or(cfg.ranker);
        let ranker = build_ranker(effective, embeddings, &service, cfg.rrf_k);

        Self {
            service,
            deferred,
            ranker,
            max_results: cfg.max_results,
            score_threshold: cfg.score_threshold,
            output_index: AtomicU64::new(0),
            sequence_number: AtomicU64::new(0),
            pending_tools: Mutex::new(Vec::new()),
            suppressor: crate::services::server_tools::FunctionCallSuppressor::new(),
        }
    }

    /// True iff at least one server is deferred via Hadrian-side search.
    pub fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    fn next_seq(&self) -> u64 {
        self.sequence_number.fetch_add(1, Ordering::Relaxed)
    }

    /// Collect the searchable catalog: every allowed, valid tool from the
    /// deferred servers, optionally narrowed to `server_label`. Returns
    /// parallel vectors of metadata (for the ranker) and the owning
    /// `McpTool` index (for rendering the function definition).
    fn candidates(&self, server_label: Option<&str>) -> (Vec<McpToolMeta>, Vec<usize>) {
        let mut metas = Vec::new();
        let mut owners = Vec::new();
        for (idx, mcp) in self.deferred.iter().enumerate() {
            if let Some(want) = server_label
                && want != mcp.server_label
            {
                continue;
            }
            let Some(server_url) = mcp.server_url.as_deref() else {
                continue;
            };
            let headers = mcp.headers.clone().unwrap_or_default();
            let Some(catalog) =
                self.service
                    .cached_tools(server_url, mcp.authorization.as_deref(), &headers)
            else {
                continue;
            };
            for meta in catalog.iter() {
                if preprocess::is_allowed(meta, mcp.allowed_tools.as_ref())
                    && preprocess::is_valid_tool_name(&meta.name)
                {
                    metas.push(meta.clone());
                    owners.push(idx);
                }
            }
        }
        (metas, owners)
    }
}

/// The `server_label` an `mcp` `tool_choice` pins, if any.
fn mcp_choice_label(payload: &CreateResponsesPayload) -> Option<String> {
    match payload.tool_choice.as_ref()? {
        ResponsesToolChoice::Mcp(c) => Some(c.server_label.clone()),
        _ => None,
    }
}

/// Select a ranker from the effective strategy and embedding
/// availability. `hybrid`/`semantic` fall back to lexical (with a warning)
/// when no embedding provider resolved — the request still succeeds.
fn build_ranker(
    kind: ToolSearchRankerKind,
    embeddings: Option<Arc<EmbeddingService>>,
    service: &McpService,
    rrf_k: u32,
) -> Arc<dyn ToolSearchRanker> {
    match (kind, embeddings) {
        (ToolSearchRankerKind::Lexical, _) => Arc::new(LexicalRanker),
        (ToolSearchRankerKind::Semantic, Some(e)) => {
            Arc::new(SemanticRanker::new(e, service.clone()))
        }
        (ToolSearchRankerKind::Hybrid, Some(e)) => Arc::new(HybridRanker::new(
            SemanticRanker::new(e, service.clone()),
            rrf_k,
        )),
        (kind, None) => {
            tracing::warn!(
                ?kind,
                "tool search requested {kind:?} ranking but no embedding provider is \
                 configured; falling back to lexical ranking"
            );
            Arc::new(LexicalRanker)
        }
    }
}

#[async_trait]
impl ServerExecutedTool for ToolSearchExecutor {
    fn name(&self) -> &'static str {
        TOOL_SEARCH_FUNCTION_NAME
    }

    fn is_enabled_for(&self, _payload: &CreateResponsesPayload) -> bool {
        self.has_deferred()
    }

    fn detect(&self, event: &[u8], _ctx: &ToolContext) -> Vec<DetectedToolCall> {
        let Some(data) = sse_data(event) else {
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
        if item.get("name").and_then(|v| v.as_str()) != Some(TOOL_SEARCH_FUNCTION_NAME) {
            return Vec::new();
        }
        let call_id = match item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(|v| v.as_str())
        {
            Some(id) => id.to_string(),
            None => {
                // A `tool_search` function-call event with neither
                // `call_id` nor `id` can't be paired to a continuation;
                // fabricating a placeholder would let two such calls
                // collide on the join key. Skip it.
                tracing::warn!(
                    "tool_search function-call event missing both `call_id` and `id`; \
                     ignoring (continuation cannot be paired)"
                );
                return Vec::new();
            }
        };
        let args_str = item
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let arguments: Value = serde_json::from_str(args_str).unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                arguments = %args_str,
                "tool_search call arguments are not valid JSON; treating as empty"
            );
            Value::Null
        });
        vec![DetectedToolCall {
            tool_name: TOOL_SEARCH_FUNCTION_NAME,
            call_id,
            arguments,
        }]
    }

    async fn execute(
        &self,
        call: DetectedToolCall,
        _ctx: &ToolContext,
    ) -> Result<ToolExecutionHandle, ToolError> {
        let query = call
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let server_label = call
            .arguments
            .get("server_label")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let (metas, owners) = self.candidates(server_label.as_deref());

        // Rank, then apply the score floor and result cap. The floor only
        // applies to rankers whose scores are normalized to `0.0..=1.0`
        // (lexical/semantic); hybrid RRF scores are ranking-only, so
        // applying a `0..1` threshold to them would drop every result.
        let mut ranked = self
            .ranker
            .rank(&query, &metas)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        if self.ranker.scores_are_normalized() {
            ranked.retain(|r| r.score >= self.score_threshold);
        }
        ranked.truncate(self.max_results);

        // Render the matched tools as their `mcp_<label>__<tool>` function
        // definitions: one copy for the `tool_search_output` item, one for
        // injection into the continuation payload.
        let mut tool_defs_json: Vec<Value> = Vec::with_capacity(ranked.len());
        let mut summary: Vec<Value> = Vec::with_capacity(ranked.len());
        let mut injected: Vec<ResponsesToolDefinition> = Vec::with_capacity(ranked.len());
        for r in &ranked {
            let meta = &metas[r.index];
            let mcp = &self.deferred[owners[r.index]];
            // A discovered tool we can't render (server-controlled schema
            // that won't build, or won't serialize) would land as a null
            // hole in `tool_search_output.tools`; skip it entirely (call +
            // injection) and log rather than emit a malformed item.
            let def = match preprocess::build_function_tool(mcp, meta, false) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(
                        tool = %meta.name,
                        server_label = %mcp.server_label,
                        error = %e,
                        "Failed to build discovered MCP tool definition; omitting from tool_search_output"
                    );
                    continue;
                }
            };
            let def_json = match serde_json::to_value(&def) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        tool = %meta.name,
                        server_label = %mcp.server_label,
                        error = %e,
                        "Failed to serialize discovered MCP tool definition; omitting from tool_search_output"
                    );
                    continue;
                }
            };
            tool_defs_json.push(def_json);
            summary.push(serde_json::json!({
                "name": preprocess::synthesize_function_name(&mcp.server_label, &meta.name),
                "description": meta.description.clone().unwrap_or_default(),
            }));
            injected.push(def);
        }

        if let Ok(mut pending) = self.pending_tools.lock() {
            pending.extend(injected);
        }

        // Emit the spec-shaped lifecycle: a `tool_search_call` then a
        // `tool_search_output` carrying the loaded definitions.
        let call_index = self.output_index.fetch_add(1, Ordering::Relaxed);
        let out_index = self.output_index.fetch_add(1, Ordering::Relaxed);
        let call_item_id = next_item_id("ts");
        let out_item_id = next_item_id("tso");
        // Per the tool-search spec, server-executed search reports
        // `call_id: null` (a client-executed search would echo the id the
        // caller assigned). The model's underlying `tool_search`
        // function-call id is still used for the provider continuation
        // below — it's just not surfaced on these spec items.
        let call_item = |status: &str| {
            serde_json::json!({
                "type": "tool_search_call",
                "id": call_item_id,
                "call_id": Value::Null,
                "execution": "server",
                "arguments": call.arguments,
                "status": status,
            })
        };
        let output_item = |status: &str| {
            serde_json::json!({
                "type": "tool_search_output",
                "id": out_item_id,
                "call_id": Value::Null,
                "execution": "server",
                "tools": tool_defs_json,
                "status": status,
            })
        };
        let events = vec![
            sse_output_item(
                "response.output_item.added",
                call_index,
                self.next_seq(),
                call_item("in_progress"),
            ),
            sse_output_item(
                "response.output_item.done",
                call_index,
                self.next_seq(),
                call_item("completed"),
            ),
            sse_output_item(
                "response.output_item.added",
                out_index,
                self.next_seq(),
                output_item("in_progress"),
            ),
            sse_output_item(
                "response.output_item.done",
                out_index,
                self.next_seq(),
                output_item("completed"),
            ),
        ];

        // The function-call output the model reads next turn: the list of
        // tools the search surfaced (now also callable via injection).
        let output_text = serde_json::json!({ "tools": summary }).to_string();
        let continuation = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
            type_: FunctionCallOutputType::FunctionCallOutput,
            id: None,
            call_id: call.call_id.clone(),
            output: output_text,
            status: None,
        });
        let result = ToolCallResult {
            call_id: call.call_id,
            continuation_items: vec![continuation],
        };

        Ok(ToolExecutionHandle {
            events: Box::pin(futures_util::stream::iter(events)),
            result: Box::pin(async move { Ok(result) }),
        })
    }

    fn apply_to_continuation(
        &self,
        payload: &mut CreateResponsesPayload,
        results: &[ToolCallResult],
        is_final_iteration: bool,
    ) {
        // Append the tool_search function-call outputs.
        let outputs: Vec<ResponsesInputItem> = results
            .iter()
            .flat_map(|r| r.continuation_items.clone())
            .collect();
        if !outputs.is_empty() {
            use crate::api_types::responses::ResponsesInput;
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
                None => payload.input = Some(ResponsesInput::Items(outputs)),
            }
        }

        // Inject the discovered function definitions so the model can call
        // them on its next turn. They persist on the continuation payload;
        // the per-iteration rewrite leaves Function tools untouched. Dedup
        // against names already present (a tool found by an earlier search).
        let discovered = self
            .pending_tools
            .lock()
            .map(|mut p| std::mem::take(&mut *p))
            .unwrap_or_default();
        if !discovered.is_empty() {
            let tools = payload.tools.get_or_insert_with(Vec::new);
            let existing: std::collections::HashSet<String> = tools
                .iter()
                .filter_map(|t| match t {
                    ResponsesToolDefinition::Function(f) => Some(f.name.clone()),
                    _ => None,
                })
                .collect();
            for def in discovered {
                if let ResponsesToolDefinition::Function(f) = &def
                    && existing.contains(&f.name)
                {
                    continue;
                }
                tools.push(def);
            }
        }

        // On the final iteration, drop the `tool_search` tool so the model
        // must produce a text answer. (The MCP executor strips the
        // discovered `mcp_<label>__<tool>` functions in its own pass.)
        if is_final_iteration && let Some(ref mut tools) = payload.tools {
            tools.retain(|t| !t.is_tool_search());
            tools.retain(|t| match t {
                ResponsesToolDefinition::Function(f) => f.name != TOOL_SEARCH_FUNCTION_NAME,
                _ => true,
            });
            if tools.is_empty() {
                payload.tools = None;
            }
        }
    }

    fn transform_event(&self, event: Bytes) -> Bytes {
        // Hide the raw `tool_search` function-call plumbing; the
        // spec-shaped `tool_search_call` / `_output` items we emit replace it.
        self.suppressor
            .suppress(event, |name| name == TOOL_SEARCH_FUNCTION_NAME)
    }
}

/// Concatenate the `data:` fields of one SSE event. Mirrors the helper in
/// [`super::executor`] (kept local to avoid widening its visibility).
fn sse_data(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            parts.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn sse_output_item(
    event_type: &str,
    output_index: u64,
    sequence_number: u64,
    item: Value,
) -> Bytes {
    let payload = serde_json::json!({
        "type": event_type,
        "output_index": output_index,
        "sequence_number": sequence_number,
        "item": item,
    });
    let body = serde_json::to_string(&payload).unwrap_or_else(|e| {
        tracing::error!(
            event_type,
            error = %e,
            "Failed to serialize tool_search SSE event; emitting empty frame"
        );
        String::new()
    });
    Bytes::from(format!("data: {body}\n\n"))
}

fn next_item_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;
    use crate::api_types::responses::ResponsesInput;

    const URL: &str = "http://127.0.0.1:1/mcp";

    fn meta(name: &str, desc: &str) -> McpToolMeta {
        McpToolMeta {
            name: name.to_string(),
            description: Some(desc.to_string()),
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        }
    }

    /// Service with a primed deferred catalog, plus an executor over a
    /// single deferred server. Lexical ranking (no embeddings).
    fn executor_with_catalog() -> ToolSearchExecutor {
        let service = McpService::new();
        service.prime_tools_cache(
            URL,
            None,
            &std::collections::HashMap::new(),
            vec![
                meta("jira_search", "Search Jira issues by query"),
                meta("confluence_create_page", "Create a Confluence page"),
            ],
        );
        let payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "tools": [{
                "type": "mcp",
                "server_label": "atlassian",
                "server_url": URL,
                "defer_loading": true,
            }]
        }))
        .unwrap();
        ToolSearchExecutor::new(service, &payload, &ToolSearchConfig::default(), None)
    }

    fn tool_search_call_event(call_id: &str, args: serde_json::Value) -> Vec<u8> {
        let ev = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "id": "fc_1",
                "call_id": call_id,
                "name": TOOL_SEARCH_FUNCTION_NAME,
                "arguments": args.to_string(),
            }
        });
        format!("data: {}\n\n", ev).into_bytes()
    }

    fn ctx() -> ToolContext {
        ToolContext {
            original_payload: serde_json::from_value(serde_json::json!({})).unwrap(),
        }
    }

    #[test]
    fn enabled_only_with_deferred_server() {
        let exec = executor_with_catalog();
        assert!(exec.has_deferred());
        assert!(exec.is_enabled_for(&serde_json::from_value(serde_json::json!({})).unwrap()));
    }

    #[test]
    fn detect_matches_tool_search_function_call() {
        let exec = executor_with_catalog();
        let ev = tool_search_call_event("call_1", serde_json::json!({"query": "jira"}));
        let calls = exec.detect(&ev, &ctx());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, TOOL_SEARCH_FUNCTION_NAME);
        assert_eq!(calls[0].call_id, "call_1");
        assert_eq!(calls[0].arguments["query"], "jira");
    }

    #[test]
    fn detect_ignores_other_function_calls() {
        let exec = executor_with_catalog();
        let ev = serde_json::json!({
            "type": "response.output_item.done",
            "item": {"type": "function_call", "id": "x", "call_id": "c", "name": "mcp_atlassian__jira_search", "arguments": "{}"}
        });
        let calls = exec.detect(format!("data: {ev}\n\n").as_bytes(), &ctx());
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn execute_emits_call_and_output_items_and_stashes_defs() {
        let exec = executor_with_catalog();
        let call = DetectedToolCall {
            tool_name: TOOL_SEARCH_FUNCTION_NAME,
            call_id: "call_1".to_string(),
            arguments: serde_json::json!({"query": "search jira issues"}),
        };
        let handle = exec.execute(call, &ctx()).await.expect("execute");
        let events: Vec<Bytes> = handle.events.collect().await;
        let joined: String = events
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(joined.contains("\"type\":\"tool_search_call\""));
        assert!(joined.contains("\"type\":\"tool_search_output\""));
        // The jira tool should have matched and been rendered.
        assert!(joined.contains("mcp_atlassian__jira_search"));

        // Result carries a function_call_output for the call.
        let result = handle.result.await.expect("result");
        assert_eq!(result.call_id, "call_1");
        assert_eq!(result.continuation_items.len(), 1);

        // The discovered function def is stashed for injection.
        assert!(!exec.pending_tools.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn apply_to_continuation_injects_discovered_tools_and_output() {
        let exec = executor_with_catalog();
        let call = DetectedToolCall {
            tool_name: TOOL_SEARCH_FUNCTION_NAME,
            call_id: "call_1".to_string(),
            arguments: serde_json::json!({"query": "search jira"}),
        };
        let handle = exec.execute(call, &ctx()).await.unwrap();
        let _ = handle.events.collect::<Vec<_>>().await;
        let result = handle.result.await.unwrap();

        let mut payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "input": [],
            "tools": [{"type": "mcp", "server_label": "atlassian", "server_url": URL, "defer_loading": true}]
        }))
        .unwrap();
        exec.apply_to_continuation(&mut payload, std::slice::from_ref(&result), false);

        let tools = payload.tools.as_ref().unwrap();
        // Discovered function tool injected.
        assert!(tools.iter().any(|t| matches!(t, ResponsesToolDefinition::Function(f) if f.name == "mcp_atlassian__jira_search")));
        // function_call_output appended to input.
        if let Some(ResponsesInput::Items(items)) = &payload.input {
            assert!(items.iter().any(
                |i| matches!(i, ResponsesInputItem::FunctionCallOutput(o) if o.call_id == "call_1")
            ));
        } else {
            panic!("expected items input");
        }
    }

    #[tokio::test]
    async fn final_iteration_strips_tool_search_tool() {
        let exec = executor_with_catalog();
        let mut payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "input": [],
            "tools": [
                {"type": "tool_search"},
                {"type": "function", "name": "mcp_atlassian__jira_search", "parameters": {"type": "object"}}
            ]
        }))
        .unwrap();
        exec.apply_to_continuation(&mut payload, &[], true);
        // tool_search tool removed; the mcp_ function remains (the MCP
        // executor strips those in its own pass).
        let tools = payload.tools.unwrap();
        assert!(!tools.iter().any(|t| t.is_tool_search()));
        assert!(tools.iter().any(|t| matches!(t, ResponsesToolDefinition::Function(f) if f.name == "mcp_atlassian__jira_search")));
    }

    #[test]
    fn build_ranker_falls_back_to_lexical_without_embeddings() {
        // hybrid default + no embeddings → lexical (no panic, returns a ranker).
        let service = McpService::new();
        let r = build_ranker(ToolSearchRankerKind::Hybrid, None, &service, 60);
        // Smoke: it ranks without embeddings.
        let _ = r;
    }
}
