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
        ResponsesInput, ResponsesInputItem, ResponsesToolDefinition, WebSearchCallOutput,
        WebSearchCallOutputType, WebSearchStatus,
    },
    config::WebSearchConfig,
    observability::metrics::record_web_search,
    routes::api::tools::{WebSearchResult, execute_web_search},
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

    pub fn parse(arguments_json: &str) -> Option<Self> {
        serde_json::from_str(arguments_json).ok()
    }

    pub fn function_description() -> &'static str {
        "Search the web for current information. Use this when you need up-to-date facts, recent events, or information that may not be in your training data."
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

// ─────────────────────────────────────────────────────────────────────────────
// Detection
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a web_search tool call from a JSON value.
fn parse_web_search_tool_call(value: &Value) -> Option<WebSearchToolCall> {
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
    let args = WebSearchToolArguments::parse(arguments_str)?;

    Some(WebSearchToolCall {
        id,
        query: args.query,
    })
}

/// Detect web_search tool calls in an SSE chunk.
fn detect_web_search_in_chunk(chunk: &[u8]) -> Vec<WebSearchToolCall> {
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

fn format_web_search_call_output_event(item_id: &str) -> Option<Bytes> {
    let output = WebSearchCallOutput {
        type_: WebSearchCallOutputType::WebSearchCall,
        id: item_id.to_string(),
        status: WebSearchStatus::Completed,
    };
    let event_data = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": output,
    });
    let json_str = serde_json::to_string(&event_data).ok()?;
    Some(Bytes::from(format!("data: {}\n\n", json_str)))
}

// ─────────────────────────────────────────────────────────────────────────────
// Streaming wrapper
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// ServerExecutedTool implementation
// ─────────────────────────────────────────────────────────────────────────────

/// `ServerExecutedTool` implementation for `web_search`.
pub struct WebSearchExecutor {
    context: WebSearchContext,
    /// Hides the rewritten `web_search` function-call plumbing from the
    /// client stream; the executor emits the spec-shaped
    /// `web_search_call` items itself.
    suppressor: crate::services::server_tools::FunctionCallSuppressor,
}

impl WebSearchExecutor {
    pub fn new(context: WebSearchContext) -> Self {
        Self {
            context,
            suppressor: crate::services::server_tools::FunctionCallSuppressor::new(),
        }
    }
}

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
            .map(|tc| crate::services::server_tools::DetectedToolCall {
                tool_name: WebSearchToolArguments::FUNCTION_NAME,
                call_id: tc.id.clone(),
                arguments: serde_json::json!({
                    "id": tc.id,
                    "query": tc.query,
                }),
            })
            .collect()
    }

    async fn execute(
        &self,
        call: crate::services::server_tools::DetectedToolCall,
        _ctx: &crate::services::server_tools::ToolContext,
    ) -> Result<
        crate::services::server_tools::ToolExecutionHandle,
        crate::services::server_tools::ToolError,
    > {
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
        let start = Instant::now();
        let search_outcome = context.execute_search(&query).await;
        let duration = start.elapsed().as_secs_f64();

        let content = match search_outcome {
            Ok(results) => {
                record_web_search("success", duration, results.len() as u32);
                if let Some(out_event) = format_web_search_call_output_event(&id) {
                    let _ = event_tx.send(out_event).await;
                }
                let _ = event_tx
                    .send(format_web_search_completed_event(&id, 0))
                    .await;
                format_web_search_results(&query, &results)
            }
            Err(e) => {
                record_web_search("error", duration, 0);
                error!(
                    stage = "search_failed",
                    call_id = %id,
                    error = %e,
                    "Web search execution failed"
                );
                // Surface error text back to the model rather than dropping it.
                format!("Web search failed for query \"{}\": {}", query, e)
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
        let tc = parse_web_search_tool_call(&value).unwrap();
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.query, "rust async programming");
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
        assert_eq!(calls[0].query, "latest news");
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
        assert_eq!(calls[0].id, "call_789");
        assert_eq!(calls[0].query, "weather today");
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
}
