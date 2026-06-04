//! Web search tool interception service for the Responses API.
//!
//! Intercepts `web_search` tool calls from the LLM and executes them against
//! the configured search provider (Tavily/Exa), feeding results back into the
//! conversation transparently — following the same pattern as `file_search_tool`.

use std::time::Instant;

use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, error, info};

use crate::{
    api_types::responses::{
        CreateResponsesPayload, FunctionCallOutput, FunctionCallOutputType, FunctionTool,
        FunctionToolCall, FunctionToolCallType, ResponsesIncludable, ResponsesInput,
        ResponsesInputItem, ResponsesToolDefinition, WebSearchAction, WebSearchActionType,
        WebSearchCallOutput, WebSearchCallOutputType, WebSearchSource, WebSearchSourceType,
        WebSearchStatus,
    },
    config::WebSearchConfig,
    observability::metrics::record_web_search,
    routes::api::tools::{WebSearchResult, execute_web_search},
    services::server_tool_history::rewrite_hosted_calls_to_function_pairs,
};

// ─────────────────────────────────────────────────────────────────────────────
// Tool Arguments (function schema for the model)
// ─────────────────────────────────────────────────────────────────────────────

/// Arguments the model produces when calling the web_search function tool.
#[derive(Debug, Clone, Deserialize)]
pub struct WebSearchToolArguments {
    pub query: String,
}

impl WebSearchToolArguments {
    pub const FUNCTION_NAME: &'static str = "web_search";

    pub fn parse(arguments_json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(arguments_json)
    }

    pub fn function_description() -> &'static str {
        "Search the web for current or fast-changing information that may not be in your training data, such as recent events, news, prices, or release dates. Returns a ranked list of results with titles, URLs, and short content snippets rather than full page contents. Prefer this over answering from memory whenever the freshness of a fact matters or you're unsure it's up to date."
    }

    pub fn function_parameters_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to find relevant information on the web"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    pub fn function_tool_definition() -> Value {
        serde_json::json!({
            "type": "function",
            "name": Self::FUNCTION_NAME,
            "description": Self::function_description(),
            "parameters": Self::function_parameters_schema(),
            "strict": false,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Payload Preprocessing
// ─────────────────────────────────────────────────────────────────────────────

/// Convert all `WebSearch*` tool definitions to function tools that models can call.
///
/// After preprocessing, the model sees a standard function tool named `"web_search"`.
/// The streaming middleware intercepts calls to this function and executes them.
pub fn preprocess_web_search_tools(payload: &mut CreateResponsesPayload) {
    // Normalize any hosted `web_search_call` items echoed back in the input
    // before the tools early-return, so a continuation that no longer
    // re-declares web_search still gets its history rewritten. See
    // [`rewrite_web_search_history`].
    rewrite_web_search_history(payload);

    let Some(tools) = payload.tools.as_mut() else {
        return;
    };

    for tool in tools.iter_mut() {
        if tool.is_web_search() {
            let function_def = WebSearchToolArguments::function_tool_definition();
            *tool = ResponsesToolDefinition::Function(
                FunctionTool::from_json(function_def)
                    .expect("web_search function-tool definition is well-formed"),
            );
            debug!(
                stage = "tool_preprocessed",
                "Preprocessed web_search tool to function definition"
            );
        }
    }
}

/// Rewrite hosted `web_search_call` items echoed back in `payload.input` into
/// the `function_call` + `function_call_output` pair every provider understands.
/// Mutates in place; a no-op when no `web_search_call` items are present.
///
/// Web search is always server-executed in Hadrian: [`preprocess_web_search_tools`]
/// rewrites the `web_search` tool to a function tool for *every* provider, so the
/// model only ever emits a `web_search` function call (suppressed from the client)
/// and the provider never produces a native `web_search_call`. The shared driver
/// [`rewrite_hosted_calls_to_function_pairs`] does the expansion; see its docs and
/// `file_search_tool::rewrite_file_search_history` /
/// `mcp::preprocess::rewrite_mcp_history` for the sibling rewrites.
fn rewrite_web_search_history(payload: &mut CreateResponsesPayload) {
    rewrite_hosted_calls_to_function_pairs(payload, |item| match item {
        ResponsesInputItem::WebSearchCall(call) => Some(web_search_call_to_function_pair(call)),
        _ => None,
    });
}

/// Reconstruct the `(function_call, function_call_output)` pair for one echoed
/// `web_search_call`. The two share a `call_id` derived from the item id so the
/// provider conversion pairs them. The function arguments mirror what the model
/// originally emitted (`{"query": …}`) and the output is the retained
/// [`WebSearchCallOutput::replay_content`] — the same result text the model saw
/// when the search first ran. A missing query/content (e.g. a failed search or a
/// pre-existing row from before content retention) degrades to an empty string
/// rather than dropping the pair, so the transcript stays well-formed.
fn web_search_call_to_function_pair(
    call: &WebSearchCallOutput,
) -> (FunctionToolCall, FunctionCallOutput) {
    // Prefer the (deprecated) singular query Hadrian always writes; fall back to
    // the first of `queries` for native items that only carry the array form.
    let query = if !call.action.query.is_empty() {
        call.action.query.clone()
    } else {
        call.action.queries.first().cloned().unwrap_or_default()
    };
    let arguments = serde_json::json!({ "query": query }).to_string();
    let output_text = call.replay_content.clone().unwrap_or_default();
    let function_call = FunctionToolCall {
        type_: FunctionToolCallType::FunctionCall,
        id: call.id.clone(),
        call_id: call.id.clone(),
        name: WebSearchToolArguments::FUNCTION_NAME.to_string(),
        arguments,
        status: None,
    };
    let output = FunctionCallOutput {
        type_: FunctionCallOutputType::FunctionCallOutput,
        id: None,
        call_id: call.id.clone(),
        output: output_text,
        status: None,
    };
    (function_call, output)
}

/// Whether the request opted into source URLs via
/// `include: ["web_search_call.action.sources"]`.
fn should_include_sources(payload: &CreateResponsesPayload) -> bool {
    payload
        .include
        .as_ref()
        .map(|includes| includes.contains(&ResponsesIncludable::WebSearchCallActionSources))
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Context
// ─────────────────────────────────────────────────────────────────────────────

/// Context for web search middleware operations.
#[derive(Clone)]
pub struct WebSearchContext {
    pub http_client: reqwest::Client,
    pub config: WebSearchConfig,
    pub max_iterations: usize,
}

impl WebSearchContext {
    pub fn new(
        http_client: reqwest::Client,
        config: WebSearchConfig,
        max_iterations: usize,
    ) -> Self {
        Self {
            http_client,
            config,
            max_iterations,
        }
    }

    pub fn is_enabled(&self) -> bool {
        true // If we have a context, we're enabled
    }

    /// Execute a web search using the configured provider.
    async fn execute_search(&self, query: &str) -> Result<Vec<WebSearchResult>, String> {
        let max_results = self.config.max_results;
        execute_web_search(&self.http_client, &self.config, query, max_results)
            .await
            .map_err(|e| e.to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Detected tool call
// ─────────────────────────────────────────────────────────────────────────────

/// A detected web_search tool call from the model.
#[derive(Debug, Clone)]
struct WebSearchToolCall {
    id: String,
    query: String,
}

/// Outcome of inspecting a `function_call` item named `web_search`.
///
/// `Invalid` carries the call id and reason so the executor can synthesize
/// a `web_search_call` with status `failed` rather than dropping the call.
/// `None` from [`parse_web_search_tool_call`] means the item is not a
/// web_search call and should pass through untouched.
#[derive(Debug, Clone)]
enum WebSearchCallDetection {
    Valid(WebSearchToolCall),
    Invalid { id: String, error: String },
}

// ─────────────────────────────────────────────────────────────────────────────
// Detection
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a web_search tool call from a JSON value.
fn parse_web_search_tool_call(value: &Value) -> Option<WebSearchCallDetection> {
    let obj = value.as_object()?;

    let type_val = obj.get("type")?.as_str()?;
    if type_val != "function_call" {
        return None;
    }

    let name = obj.get("name")?.as_str()?;
    if name != WebSearchToolArguments::FUNCTION_NAME {
        return None;
    }

    let id = obj
        .get("call_id")
        .or_else(|| obj.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let arguments_str = obj.get("arguments")?.as_str()?;
    match WebSearchToolArguments::parse(arguments_str) {
        Ok(args) => Some(WebSearchCallDetection::Valid(WebSearchToolCall {
            id,
            query: args.query,
        })),
        Err(e) => Some(WebSearchCallDetection::Invalid {
            id,
            error: format!("could not parse `arguments` (expected {{\"query\": \"...\"}}): {e}"),
        }),
    }
}

/// Detect web_search tool calls in an SSE chunk.
fn detect_web_search_in_chunk(chunk: &[u8]) -> Vec<WebSearchCallDetection> {
    let Some(chunk_str) = std::str::from_utf8(chunk).ok() else {
        return Vec::new();
    };

    let mut found_calls = Vec::new();

    for line in chunk_str.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data == "[DONE]" {
                continue;
            }

            if let Ok(json) = serde_json::from_str::<Value>(data) {
                // Responses API: output array
                if let Some(output) = json.get("output").and_then(|o| o.as_array()) {
                    for item in output {
                        if let Some(tc) = parse_web_search_tool_call(item) {
                            found_calls.push(tc);
                        }
                    }
                }

                // Direct function_call
                if let Some(tc) = parse_web_search_tool_call(&json) {
                    found_calls.push(tc);
                }

                // response.output_item.done — canonical event for complete function calls.
                // Note: we intentionally skip `response.function_call_arguments.done`
                // because the Responses API emits both events for the same tool call,
                // which would cause duplicate search executions. The output_item.done
                // event contains the complete function call with the correct `call_id`.
                if json.get("type").and_then(|t| t.as_str()) == Some("response.output_item.done")
                    && let Some(item) = json.get("item")
                    && let Some(tc) = parse_web_search_tool_call(item)
                {
                    found_calls.push(tc);
                }

                // Chat completion delta format
                if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                    for choice in choices {
                        if let Some(delta) = choice.get("delta")
                            && let Some(tool_calls) =
                                delta.get("tool_calls").and_then(|t| t.as_array())
                        {
                            for tc in tool_calls {
                                if let Some(tc) = parse_web_search_tool_call(tc) {
                                    found_calls.push(tc);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    found_calls
}

// ─────────────────────────────────────────────────────────────────────────────
// Result formatting
// ─────────────────────────────────────────────────────────────────────────────

/// Format web search results as text for the model to consume.
fn format_web_search_results(query: &str, results: &[WebSearchResult]) -> String {
    let mut output = format!(
        "Web search results for \"{}\" ({} results):\n\n",
        query,
        results.len()
    );

    for (i, result) in results.iter().enumerate() {
        output.push_str(&format!("[{}] {} - {}\n", i + 1, result.title, result.url));
        output.push_str(&result.content);
        output.push_str("\n\n");
    }

    output
        .push_str("Cite sources using their URLs when referencing information from these results.");
    output
}

// ─────────────────────────────────────────────────────────────────────────────
// SSE event formatters
// ─────────────────────────────────────────────────────────────────────────────

fn format_web_search_in_progress_event(item_id: &str, output_index: usize) -> Bytes {
    let event_data = serde_json::json!({
        "type": "response.web_search_call.in_progress",
        "output_index": output_index,
        "item_id": item_id,
    });
    let json_str = serde_json::to_string(&event_data).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", json_str))
}

fn format_web_search_searching_event(item_id: &str, output_index: usize) -> Bytes {
    let event_data = serde_json::json!({
        "type": "response.web_search_call.searching",
        "output_index": output_index,
        "item_id": item_id,
    });
    let json_str = serde_json::to_string(&event_data).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", json_str))
}

fn format_web_search_completed_event(item_id: &str, output_index: usize) -> Bytes {
    let event_data = serde_json::json!({
        "type": "response.web_search_call.completed",
        "output_index": output_index,
        "item_id": item_id,
    });
    let json_str = serde_json::to_string(&event_data).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", json_str))
}

/// Wrap a completed/failed `web_search_call` item in the canonical
/// `response.output_item.done` event. The item carries the spec-shaped
/// `action` (query + optional sources) and the retained `replay_content`, so
/// the persisted output is enough to replay the search on a later turn.
fn format_web_search_call_output_event(item: &WebSearchCallOutput) -> Option<Bytes> {
    let event_data = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": item,
    });
    let json_str = serde_json::to_string(&event_data).ok()?;
    Some(Bytes::from(format!("data: {}\n\n", json_str)))
}

/// Build a self-contained handle for a `web_search` call whose arguments
/// couldn't be parsed. Emits a `web_search_call` item with status `failed`
/// (the spec's failure status) and feeds the error back as a
/// `function_call_output` so the loop continues and the model can retry.
#[cfg(feature = "server")]
fn synthesize_web_search_invalid_handle(
    call_id: &str,
    error: &str,
) -> crate::services::server_tools::ToolExecutionHandle {
    let id = call_id.to_string();
    let error_text = crate::services::server_tools::invalid_arguments_text(
        WebSearchToolArguments::FUNCTION_NAME,
        error,
    );
    // The arguments couldn't be parsed, so there's no query to record — emit the
    // spec-required `action` with an empty query (the spec keeps `action` even on
    // a `failed` call) and keep the error as `replay_content` so a later-turn
    // replay surfaces the same failure rather than an empty result.
    let failed_item = WebSearchCallOutput {
        type_: WebSearchCallOutputType::WebSearchCall,
        id: id.clone(),
        status: WebSearchStatus::Failed,
        action: WebSearchAction::default(),
        replay_content: Some(error_text.clone()),
    };
    // Reuse the canonical `output_item.done` formatter so the failed item's
    // envelope stays in lockstep with the success/failure paths.
    let mut events = vec![format_web_search_in_progress_event(&id, 0)];
    if let Some(done_event) = format_web_search_call_output_event(&failed_item) {
        events.push(done_event);
    }

    let continuation_item = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
        type_: FunctionCallOutputType::FunctionCallOutput,
        id: Some(id.clone()),
        call_id: id.clone(),
        output: error_text,
        status: None,
    });
    let result = crate::services::server_tools::ToolCallResult {
        call_id: id,
        continuation_items: vec![continuation_item],
    };

    crate::services::server_tools::ToolExecutionHandle {
        events: Box::pin(futures_util::stream::iter(events)),
        result: Box::pin(async move { Ok(result) }),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Streaming wrapper
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// ServerExecutedTool implementation
// ─────────────────────────────────────────────────────────────────────────────

/// `ServerExecutedTool` implementation for `web_search`.
#[cfg(feature = "server")]
pub struct WebSearchExecutor {
    context: WebSearchContext,
    /// Hides the rewritten `web_search` function-call plumbing from the
    /// client stream; the executor emits the spec-shaped
    /// `web_search_call` items itself.
    suppressor: crate::services::server_tools::FunctionCallSuppressor,
}

#[cfg(feature = "server")]
impl WebSearchExecutor {
    pub fn new(context: WebSearchContext) -> Self {
        Self {
            context,
            suppressor: crate::services::server_tools::FunctionCallSuppressor::new(),
        }
    }
}

#[cfg(feature = "server")]
#[async_trait::async_trait]
impl crate::services::server_tools::ServerExecutedTool for WebSearchExecutor {
    fn name(&self) -> &'static str {
        WebSearchToolArguments::FUNCTION_NAME
    }

    /// Hide the rewritten `web_search` function-call plumbing; the
    /// executor emits the spec-shaped `web_search_call` items itself.
    fn transform_event(&self, event: Bytes) -> Bytes {
        self.suppressor
            .suppress(event, |name| name == WebSearchToolArguments::FUNCTION_NAME)
    }

    fn is_enabled_for(&self, payload: &CreateResponsesPayload) -> bool {
        self.context.is_enabled()
            && payload
                .tools
                .as_ref()
                .map(|tools| {
                    tools.iter().any(|t| {
                        t.is_web_search()
                            || matches!(
                                t,
                                ResponsesToolDefinition::Function(f)
                                    if f.name == WebSearchToolArguments::FUNCTION_NAME
                            )
                    })
                })
                .unwrap_or(false)
    }

    fn detect(
        &self,
        event: &[u8],
        _ctx: &crate::services::server_tools::ToolContext,
    ) -> Vec<crate::services::server_tools::DetectedToolCall> {
        detect_web_search_in_chunk(event)
            .into_iter()
            .map(|detection| match detection {
                WebSearchCallDetection::Valid(tc) => {
                    crate::services::server_tools::DetectedToolCall::new(
                        WebSearchToolArguments::FUNCTION_NAME,
                        tc.id.clone(),
                        serde_json::json!({
                            "id": tc.id,
                            "query": tc.query,
                        }),
                    )
                }
                WebSearchCallDetection::Invalid { id, error } => {
                    crate::services::server_tools::DetectedToolCall::invalid(
                        WebSearchToolArguments::FUNCTION_NAME,
                        id,
                        error,
                    )
                }
            })
            .collect()
    }

    async fn execute(
        &self,
        call: crate::services::server_tools::DetectedToolCall,
        ctx: &crate::services::server_tools::ToolContext,
    ) -> Result<
        crate::services::server_tools::ToolExecutionHandle,
        crate::services::server_tools::ToolError,
    > {
        // The model emitted a `web_search` call we recognized but couldn't
        // parse. Surface a `web_search_call` with status `failed` and feed
        // the error back so the loop continues — never drop it.
        if let Some(error) = &call.invalid {
            return Ok(synthesize_web_search_invalid_handle(&call.call_id, error));
        }
        let query = call
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let id = call.call_id.clone();

        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<Bytes>(8);

        // in_progress + searching events
        let _ = event_tx
            .send(format_web_search_in_progress_event(&id, 0))
            .await;
        let _ = event_tx
            .send(format_web_search_searching_event(&id, 0))
            .await;

        let context = self.context.clone();
        let include_sources = should_include_sources(&ctx.original_payload);
        let start = Instant::now();
        let search_outcome = context.execute_search(&query).await;
        let duration = start.elapsed().as_secs_f64();

        // Build the spec-shaped `web_search_call` item once and emit it as the
        // canonical `output_item.done`. `replay_content` is the same result text
        // we feed the model below, retained so the search can be replayed on a
        // later turn (see `rewrite_web_search_history`).
        let content = match search_outcome {
            Ok(results) => {
                record_web_search("success", duration, results.len() as u32);
                let content = format_web_search_results(&query, &results);
                let sources = include_sources.then(|| {
                    results
                        .iter()
                        .map(|r| WebSearchSource {
                            type_: WebSearchSourceType::Url,
                            url: r.url.clone(),
                        })
                        .collect()
                });
                let item = WebSearchCallOutput {
                    type_: WebSearchCallOutputType::WebSearchCall,
                    id: id.clone(),
                    status: WebSearchStatus::Completed,
                    action: WebSearchAction {
                        type_: WebSearchActionType::Search,
                        query: query.clone(),
                        queries: vec![query.clone()],
                        sources,
                    },
                    replay_content: Some(content.clone()),
                };
                if let Some(out_event) = format_web_search_call_output_event(&item) {
                    let _ = event_tx.send(out_event).await;
                }
                let _ = event_tx
                    .send(format_web_search_completed_event(&id, 0))
                    .await;
                content
            }
            Err(e) => {
                record_web_search("error", duration, 0);
                error!(
                    stage = "search_failed",
                    call_id = %id,
                    error = %e,
                    "Web search execution failed"
                );
                // Surface error text back to the model rather than dropping it,
                // and emit a `failed` item so the call still appears in (and
                // replays from) the transcript.
                let content = format!("Web search failed for query \"{}\": {}", query, e);
                let item = WebSearchCallOutput {
                    type_: WebSearchCallOutputType::WebSearchCall,
                    id: id.clone(),
                    status: WebSearchStatus::Failed,
                    action: WebSearchAction {
                        type_: WebSearchActionType::Search,
                        query: query.clone(),
                        queries: vec![query.clone()],
                        sources: None,
                    },
                    replay_content: Some(content.clone()),
                };
                if let Some(out_event) = format_web_search_call_output_event(&item) {
                    let _ = event_tx.send(out_event).await;
                }
                content
            }
        };

        let continuation_item = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
            type_: FunctionCallOutputType::FunctionCallOutput,
            id: Some(id.clone()),
            call_id: id.clone(),
            output: content,
            status: None,
        });

        drop(event_tx);

        let result = crate::services::server_tools::ToolCallResult {
            call_id: id,
            continuation_items: vec![continuation_item],
        };

        Ok(crate::services::server_tools::ToolExecutionHandle {
            events: Box::pin(futures_util::stream::unfold(
                event_rx,
                |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
            )),
            result: Box::pin(async move { Ok(result) }),
        })
    }

    fn apply_to_continuation(
        &self,
        payload: &mut CreateResponsesPayload,
        results: &[crate::services::server_tools::ToolCallResult],
        is_final_iteration: bool,
    ) {
        let function_outputs: Vec<ResponsesInputItem> = results
            .iter()
            .flat_map(|r| r.continuation_items.clone())
            .collect();

        if function_outputs.is_empty() {
            return;
        }

        match payload.input {
            Some(ResponsesInput::Items(ref mut items)) => {
                items.extend(function_outputs);
            }
            Some(ResponsesInput::Text(ref text)) => {
                let text = text.clone();
                let mut items = vec![ResponsesInputItem::EasyMessage(
                    crate::api_types::responses::EasyInputMessage {
                        type_: None,
                        role: crate::api_types::responses::EasyInputMessageRole::User,
                        content: crate::api_types::responses::EasyInputMessageContent::Text(text),
                    },
                )];
                items.extend(function_outputs);
                payload.input = Some(ResponsesInput::Items(items));
            }
            None => {
                payload.input = Some(ResponsesInput::Items(function_outputs));
            }
        }

        // Strip web_search tool definitions on the final iteration.
        if is_final_iteration && let Some(ref mut tools) = payload.tools {
            let before = tools.len();
            tools.retain(|t| !t.is_web_search());
            tools.retain(|t| {
                if let ResponsesToolDefinition::Function(f) = t {
                    f.name != WebSearchToolArguments::FUNCTION_NAME
                } else {
                    true
                }
            });
            if tools.len() < before {
                info!(
                    stage = "tools_removed",
                    removed = before - tools.len(),
                    "Removed web_search tools on final iteration to force completion"
                );
            }
            if tools.is_empty() {
                payload.tools = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_web_search_tool_call() {
        let value = serde_json::json!({
            "type": "function_call",
            "name": "web_search",
            "call_id": "call_123",
            "arguments": "{\"query\": \"rust async programming\"}"
        });
        let WebSearchCallDetection::Valid(tc) = parse_web_search_tool_call(&value).unwrap() else {
            panic!("expected a valid web_search call");
        };
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.query, "rust async programming");
    }

    #[test]
    fn test_parse_web_search_tool_call_invalid_arguments() {
        // `query` is a number, not a string → fails to deserialize.
        let value = serde_json::json!({
            "type": "function_call",
            "name": "web_search",
            "call_id": "call_bad",
            "arguments": "{\"query\": 123}"
        });
        let WebSearchCallDetection::Invalid { id, error } =
            parse_web_search_tool_call(&value).unwrap()
        else {
            panic!("expected an invalid web_search call");
        };
        assert_eq!(id, "call_bad");
        assert!(!error.is_empty());
    }

    #[test]
    fn test_parse_web_search_tool_call_not_web_search() {
        let value = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_123",
            "arguments": "{\"query\": \"test\"}"
        });
        assert!(parse_web_search_tool_call(&value).is_none());
    }

    #[test]
    fn test_parse_web_search_tool_call_wrong_type() {
        let value = serde_json::json!({
            "type": "message",
            "name": "web_search",
        });
        assert!(parse_web_search_tool_call(&value).is_none());
    }

    #[test]
    fn test_detect_web_search_in_chunk_output_item_done() {
        let chunk = br#"data: {"type": "response.output_item.done", "item": {"type": "function_call", "name": "web_search", "call_id": "call_456", "arguments": "{\"query\": \"latest news\"}"}}

"#;
        let calls = detect_web_search_in_chunk(chunk);
        assert_eq!(calls.len(), 1);
        let WebSearchCallDetection::Valid(tc) = &calls[0] else {
            panic!("expected a valid web_search call");
        };
        assert_eq!(tc.query, "latest news");
    }

    #[test]
    fn test_detect_web_search_ignores_function_call_arguments_done() {
        // The Responses API emits both `response.function_call_arguments.done` and
        // `response.output_item.done` for the same tool call. We only detect from
        // `response.output_item.done` to avoid duplicates.
        let chunk = br#"data: {"type": "response.function_call_arguments.done", "name": "web_search", "item_id": "item_789", "arguments": "{\"query\": \"weather today\"}"}

"#;
        let calls = detect_web_search_in_chunk(chunk);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_detect_web_search_no_duplicate_across_event_types() {
        // Simulate both events arriving in the same chunk — only one detection expected.
        let chunk = b"data: {\"type\": \"response.function_call_arguments.done\", \"name\": \"web_search\", \"item_id\": \"item_789\", \"arguments\": \"{\\\"query\\\": \\\"weather today\\\"}\"}\n\ndata: {\"type\": \"response.output_item.done\", \"item\": {\"type\": \"function_call\", \"name\": \"web_search\", \"call_id\": \"call_789\", \"arguments\": \"{\\\"query\\\": \\\"weather today\\\"}\"}}\n\n";
        let calls = detect_web_search_in_chunk(chunk);
        assert_eq!(calls.len(), 1);
        let WebSearchCallDetection::Valid(tc) = &calls[0] else {
            panic!("expected a valid web_search call");
        };
        assert_eq!(tc.id, "call_789");
        assert_eq!(tc.query, "weather today");
    }

    #[test]
    fn test_detect_web_search_in_chunk_no_match() {
        let chunk = br#"data: {"type": "response.output_item.done", "item": {"type": "function_call", "name": "file_search", "call_id": "call_123", "arguments": "{\"query\": \"test\"}"}}

"#;
        let calls = detect_web_search_in_chunk(chunk);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_format_web_search_results() {
        let results = vec![
            WebSearchResult {
                title: "Example".to_string(),
                url: "https://example.com".to_string(),
                content: "Example content".to_string(),
                score: Some(0.9),
            },
            WebSearchResult {
                title: "Other".to_string(),
                url: "https://other.com".to_string(),
                content: "Other content".to_string(),
                score: None,
            },
        ];
        let output = format_web_search_results("test query", &results);
        assert!(output.contains("test query"));
        assert!(output.contains("[1] Example"));
        assert!(output.contains("[2] Other"));
        assert!(output.contains("https://example.com"));
    }

    #[test]
    fn test_preprocess_web_search_tools() {
        let json = serde_json::json!({
            "tools": [{"type": "web_search"}],
            "stream": false,
        });
        let mut payload: CreateResponsesPayload = serde_json::from_value(json).unwrap();
        preprocess_web_search_tools(&mut payload);
        let tools = payload.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert!(matches!(tools[0], ResponsesToolDefinition::Function(_)));
        if let ResponsesToolDefinition::Function(ref f) = tools[0] {
            assert_eq!(f.name, "web_search");
        }
    }

    fn web_search_call(id: &str, query: &str, content: &str) -> ResponsesInputItem {
        ResponsesInputItem::WebSearchCall(WebSearchCallOutput {
            type_: WebSearchCallOutputType::WebSearchCall,
            id: id.to_string(),
            status: WebSearchStatus::Completed,
            action: WebSearchAction {
                type_: WebSearchActionType::Search,
                query: query.to_string(),
                queries: vec![query.to_string()],
                sources: None,
            },
            replay_content: Some(content.to_string()),
        })
    }

    #[test]
    fn test_rewrite_web_search_history_expands_to_function_pair() {
        // Simulates reconstructed history from a `previous_response_id` chain:
        // a user message, a synthesized web_search_call, then a follow-up.
        let mut payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "input": [
                {"role": "user", "content": "weather?"},
                {"role": "user", "content": "tomorrow?"},
            ],
            "stream": false,
        }))
        .unwrap();
        let Some(ResponsesInput::Items(items)) = payload.input.as_mut() else {
            panic!("expected items input");
        };
        items.insert(
            1,
            web_search_call("ws_1", "weather today", "Results: sunny, 25C"),
        );
        assert_eq!(items.len(), 3);

        rewrite_web_search_history(&mut payload);

        let Some(ResponsesInput::Items(items)) = payload.input else {
            panic!("expected items input");
        };
        // The single web_search_call expands to a function_call + output pair,
        // so the two user messages plus the pair = 4 items.
        assert_eq!(items.len(), 4);
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, ResponsesInputItem::WebSearchCall(_))),
            "no web_search_call items should remain"
        );
        // The function_call carries the query; its paired output carries the
        // retained result text, sharing a call_id.
        let ResponsesInputItem::FunctionCall(ref fc) = items[1] else {
            panic!("expected a function_call at index 1, got {:?}", items[1]);
        };
        assert_eq!(fc.name, "web_search");
        assert_eq!(fc.call_id, "ws_1");
        assert!(fc.arguments.contains("weather today"));
        let ResponsesInputItem::FunctionCallOutput(ref out) = items[2] else {
            panic!(
                "expected a function_call_output at index 2, got {:?}",
                items[2]
            );
        };
        assert_eq!(out.call_id, "ws_1");
        assert_eq!(out.output, "Results: sunny, 25C");
    }

    #[test]
    fn test_preprocess_rewrites_history_without_redeclared_tools() {
        // Continuation turn: the client chained via `previous_response_id` and did
        // not re-declare the web_search tool. The reconstructed history still
        // carries a web_search_call that must be rewritten before dispatch — the
        // rewrite must run *before* the tools early-return.
        let mut payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({"stream": false})).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![web_search_call(
            "ws_1",
            "rust async",
            "Results: ...",
        )]));
        assert!(payload.tools.is_none());

        preprocess_web_search_tools(&mut payload);

        let Some(ResponsesInput::Items(items)) = payload.input else {
            panic!("expected items input");
        };
        assert_eq!(
            items.len(),
            2,
            "web_search_call must expand to a function pair"
        );
        assert!(matches!(items[0], ResponsesInputItem::FunctionCall(_)));
        assert!(matches!(
            items[1],
            ResponsesInputItem::FunctionCallOutput(_)
        ));
    }

    #[test]
    fn test_should_include_sources() {
        let with = serde_json::from_value::<CreateResponsesPayload>(serde_json::json!({
            "include": ["web_search_call.action.sources"],
            "stream": false,
        }))
        .unwrap();
        assert!(should_include_sources(&with));

        let without =
            serde_json::from_value::<CreateResponsesPayload>(serde_json::json!({"stream": false}))
                .unwrap();
        assert!(!should_include_sources(&without));
    }

    #[test]
    fn test_web_search_call_serialization_is_spec_shaped() {
        // OpenAI requires `action` on every `web_search_call` and `query` on the
        // search action. Assert both are always serialized — including the
        // default action used when arguments fail to parse — and that the modern
        // `queries` array is emitted alongside the deprecated `query`.
        let completed = serde_json::to_value(WebSearchCallOutput {
            type_: WebSearchCallOutputType::WebSearchCall,
            id: "ws_1".to_string(),
            status: WebSearchStatus::Completed,
            action: WebSearchAction {
                type_: WebSearchActionType::Search,
                query: "rust 2024".to_string(),
                queries: vec!["rust 2024".to_string()],
                sources: None,
            },
            replay_content: Some("results".to_string()),
        })
        .unwrap();
        assert_eq!(completed["action"]["type"], "search");
        assert_eq!(completed["action"]["query"], "rust 2024");
        assert_eq!(completed["action"]["queries"][0], "rust 2024");

        // The malformed-arguments path emits a default action; `action` and
        // `query` must still be present (query as an empty string).
        let failed = serde_json::to_value(WebSearchCallOutput {
            type_: WebSearchCallOutputType::WebSearchCall,
            id: "ws_2".to_string(),
            status: WebSearchStatus::Failed,
            action: WebSearchAction::default(),
            replay_content: Some("error".to_string()),
        })
        .unwrap();
        assert!(failed.get("action").is_some(), "action must be serialized");
        assert_eq!(failed["action"]["type"], "search");
        assert_eq!(failed["action"]["query"], "");
    }
}
