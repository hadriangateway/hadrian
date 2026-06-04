//! File search tool interception service for the Responses API.
//!
//! This service intercepts `file_search` tool calls from the LLM and executes
//! them against the local vector store, feeding results back into the conversation
//! without exposing the search process to the client.
//!
//! # Architecture
//!
//! ```text
//! Client → Gateway → Provider
//!                      │ file_search tool call
//!                      ▼
//!                   ┌─────────────┐
//!                   │ Gateway     │ ← intercepts
//!                   │ (executes   │
//!                   │  search)    │
//!                   └─────────────┘
//!                      │ tool result
//!                      ▼
//!                   Provider → final response → Client
//! ```
//!
//! # Usage
//!
//! The middleware is applied to streaming responses from the Responses API.
//! When a `file_search` tool call is detected, the middleware:
//!
//! 1. Pauses the stream to the client
//! 2. Executes the search against configured vector stores
//! 3. Formats results as a tool response
//! 4. Continues the conversation with the provider
//! 5. Streams the final response to the client

use std::{collections::HashMap, sync::Arc, time::Instant};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::{
    api_types::responses::{
        CreateResponsesPayload, FileSearchCallOutput, FileSearchCallOutputType,
        FileSearchComparisonFilter, FileSearchCompoundFilter, FileSearchFilter,
        FileSearchFilterComparison, FileSearchFilterLogicalType, FileSearchResultItem,
        FileSearchTool, FunctionCallOutput, FunctionCallOutputType, FunctionTool, FunctionToolCall,
        FunctionToolCallType, ResponsesAnnotation, ResponsesIncludable, ResponsesInput,
        ResponsesInputItem, ResponsesToolDefinition, WebSearchStatus,
    },
    auth::AuthenticatedRequest,
    config::FileSearchConfig,
    models::{
        AttributeFilter, ComparisonFilter, ComparisonOperator, CompoundFilter, FilterValue,
        LogicalOperator,
    },
    observability::{metrics::record_file_search, otel_span_error, otel_span_ok},
    services::{
        FileSearchRequest, FileSearchResponse, FileSearchService,
        server_tool_history::rewrite_hosted_calls_to_function_pairs,
    },
};

// ─────────────────────────────────────────────────────────────────────────────
// File Search Tool Arguments Schema
// ─────────────────────────────────────────────────────────────────────────────

/// Arguments schema for the `file_search` function tool.
///
/// When Hadrian converts a `file_search` tool into a function callable by the LLM,
/// this schema defines the expected arguments. The model generates these arguments
/// as a JSON string, which Hadrian parses to execute the search.
///
/// # OpenAI Compatibility
///
/// This schema is designed to be compatible with OpenAI's file_search tool behavior:
/// - `query`: The natural language search query (required)
/// - `max_num_results`: Limit results returned (optional, 1-50)
/// - `filters`: Attribute filters matching the tool definition format (optional)
/// - `score_threshold`: Minimum relevance score for results (optional, 0.0-1.0)
///
/// # Example
///
/// ```json
/// {
///   "query": "What is the return policy?",
///   "max_num_results": 5,
///   "score_threshold": 0.7,
///   "filters": {
///     "type": "eq",
///     "key": "category",
///     "value": "policy"
///   }
/// }
/// ```
///
/// # JSON Schema Generation
///
/// Use [`FileSearchToolArguments::function_parameters_schema()`] to get the schema
/// for injection into tool definitions. This produces a clean schema optimized for
/// LLM function calling compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchToolArguments {
    /// The natural language search query to find relevant content.
    ///
    /// This should be a clear, descriptive query that captures what information
    /// the user is looking for. The query will be used for semantic search
    /// across the configured vector stores.
    pub query: String,

    /// Maximum number of results to return.
    ///
    /// Must be between 1 and 50 inclusive. If not specified, defaults to the
    /// value configured in the file_search tool definition or server config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_num_results: Option<u32>,

    /// Minimum relevance score threshold for results.
    ///
    /// Results with scores below this threshold will be excluded.
    /// Must be between 0.0 and 1.0 inclusive, where 1.0 is a perfect match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_threshold: Option<f64>,

    /// Attribute filters to narrow down search results.
    ///
    /// Filters are applied before semantic search to limit which files/chunks
    /// are considered. The filter structure matches the OpenAI file_search
    /// filter format (comparison filters and compound filters with and/or).
    ///
    /// If specified in both the tool definition and the function call arguments,
    /// the function call filters take precedence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<FileSearchFilter>,

    /// Ranking options to control result scoring and reranking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking_options: Option<crate::models::FileSearchRankingOptions>,
}

impl FileSearchToolArguments {
    /// Generate a simplified JSON Schema suitable for LLM function calling.
    ///
    /// This produces a cleaner schema without the extra metadata that schemars
    /// includes (like `$schema`, `title`, `definitions`), making it more
    /// compatible with various LLM providers.
    pub fn function_parameters_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The natural language search query to find relevant content in the knowledge base"
                },
                "max_num_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (1-50)",
                    "minimum": 1,
                    "maximum": 50
                },
                "score_threshold": {
                    "type": "number",
                    "description": "Minimum relevance score threshold (0.0-1.0). Results below this score are excluded.",
                    "minimum": 0.0,
                    "maximum": 1.0
                },
                "filters": {
                    "type": "object",
                    "description": "Attribute filters to narrow search results. Use comparison filters (eq, ne, gt, gte, lt, lte) or compound filters (and, or)."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    /// Parse arguments from a JSON string (as received from model output).
    ///
    /// Returns `Err` if parsing fails or if required fields are missing; the
    /// caller turns that into a spec-shaped failure rather than dropping the
    /// call.
    pub fn parse(arguments_json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(arguments_json)
    }

    /// Generate a complete OpenAI-compatible function tool definition.
    ///
    /// This is the full tool definition that should be injected into the tools array
    /// when sending requests to LLM providers. It tells the model that a `file_search`
    /// function is available and what arguments it accepts.
    ///
    /// # Example Output
    ///
    /// ```json
    /// {
    ///   "type": "function",
    ///   "name": "file_search",
    ///   "description": "Search for relevant information...",
    ///   "parameters": { ... }
    /// }
    /// ```
    pub fn function_tool_definition() -> Value {
        serde_json::json!({
            "type": "function",
            "name": "file_search",
            "description": "Search the knowledge base attached to this conversation (the user's uploaded files and configured vector stores) using semantic search. Returns the most relevant passages with relevance scores, not whole documents, so synthesize your answer from these snippets. Use this for questions about the user's own documents or domain data. Don't use it for general knowledge you already have; for current web information, use a web search tool if one is available.",
            "parameters": Self::function_parameters_schema()
        })
    }

    /// Get the function name used for file_search tool calls.
    pub const FUNCTION_NAME: &'static str = "file_search";

    /// Get the function description for file_search.
    pub fn function_description() -> &'static str {
        "Search the knowledge base attached to this conversation (the user's uploaded files and configured vector stores) using semantic search. Returns the most relevant passages with relevance scores, not whole documents, so synthesize your answer from these snippets. Use this for questions about the user's own documents or domain data. Don't use it for general knowledge you already have; for current web information, use a web search tool if one is available."
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Payload Preprocessing
// ─────────────────────────────────────────────────────────────────────────────

/// Preprocess a Responses API payload to convert `file_search` tools to function tools.
///
/// This is necessary for providers that don't natively understand the `file_search` tool type
/// (like OpenAI-compatible APIs, Bedrock, etc.). The function converts `file_search` tools
/// to standard function tools with the appropriate schema, so the model knows what arguments
/// to generate when calling the tool.
///
/// The middleware will intercept the resulting `file_search` function calls and execute
/// them locally.
///
/// # Example
///
/// ```ignore
/// let mut payload = CreateResponsesPayload { ... };
/// preprocess_file_search_tools(&mut payload);
/// // payload.tools now contains function tools instead of file_search tools
/// ```
pub fn preprocess_file_search_tools(payload: &mut CreateResponsesPayload) {
    // Rewrite any hosted `file_search_call` items echoed back in the input
    // before the tools early-return, so a continuation that no longer
    // re-declares file_search still gets its history rewritten. See
    // [`rewrite_file_search_history`].
    rewrite_file_search_history(payload);

    let Some(tools) = payload.tools.as_mut() else {
        return;
    };

    for tool in tools.iter_mut() {
        if matches!(tool, ResponsesToolDefinition::FileSearch(_)) {
            // Convert file_search to a function tool
            let function_def = FileSearchToolArguments::function_tool_definition();
            *tool = ResponsesToolDefinition::Function(
                FunctionTool::from_json(function_def)
                    .expect("file_search function-tool definition is well-formed"),
            );
            debug!(
                stage = "tool_preprocessed",
                "Preprocessed file_search tool to function definition for OpenAI-compatible provider"
            );
        }
    }
}

/// Rewrite hosted `file_search_call` items echoed back in `payload.input` into
/// the `function_call` + `function_call_output` pair every provider understands.
///
/// File search is server-executed exactly like `web_search`:
/// [`preprocess_file_search_tools`] rewrites the `file_search` tool to a function
/// tool, so the provider never produces a native `file_search_call`. The shared
/// driver [`rewrite_hosted_calls_to_function_pairs`] does the expansion, replaying
/// the retained [`FileSearchCallOutput::replay_content`] as the tool output so the
/// model keeps the retrieved chunks in later-turn context. See its docs and
/// `web_search_tool::rewrite_web_search_history` for the sibling rewrite.
///
/// RAG chunks are larger than web snippets, so re-injecting them every turn costs
/// more tokens than web search does — the tradeoff for keeping multi-turn file
/// search coherent rather than dropping the evidence the model already cited.
fn rewrite_file_search_history(payload: &mut CreateResponsesPayload) {
    rewrite_hosted_calls_to_function_pairs(payload, |item| match item {
        ResponsesInputItem::FileSearchCall(call) => Some(file_search_call_to_function_pair(call)),
        _ => None,
    });
}

/// Reconstruct the `(function_call, function_call_output)` pair for one echoed
/// `file_search_call`. The two share a `call_id` derived from the item id so the
/// provider conversion pairs them. The function arguments mirror what the model
/// originally emitted (`{"query": …}`, taken from the first of `queries`) and the
/// output is the retained [`FileSearchCallOutput::replay_content`] — the same
/// retrieval text the model saw when the search first ran. A missing query/content
/// (e.g. a failed search or a row from before content retention) degrades to an
/// empty string rather than dropping the pair, so the transcript stays well-formed.
fn file_search_call_to_function_pair(
    call: &FileSearchCallOutput,
) -> (FunctionToolCall, FunctionCallOutput) {
    let query = call.queries.first().cloned().unwrap_or_default();
    let arguments = serde_json::json!({ "query": query }).to_string();
    let output_text = call.replay_content.clone().unwrap_or_default();
    let function_call = FunctionToolCall {
        type_: FunctionToolCallType::FunctionCall,
        id: call.id.clone(),
        call_id: call.id.clone(),
        name: FileSearchToolArguments::FUNCTION_NAME.to_string(),
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

/// Check if a payload contains any file_search tools.
#[allow(dead_code)] // Utility for future use
pub fn has_file_search_tools(payload: &CreateResponsesPayload) -> bool {
    payload
        .tools
        .as_ref()
        .map(|tools| tools.iter().any(|t| t.is_file_search()))
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Filter Conversion (API types → Service types)
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a `FileSearchFilter` (API type) to an `AttributeFilter` (service type).
///
/// This conversion is needed because the API uses OpenAI-compatible filter types
/// from `api_types::responses`, while the service layer uses internal filter types
/// from `models::attribute_filter`.
///
/// # Example
///
/// ```ignore
/// let api_filter = FileSearchFilter::Comparison(FileSearchComparisonFilter {
///     type_: FileSearchFilterComparison::Eq,
///     key: "author".to_string(),
///     value: json!("John"),
/// });
///
/// let service_filter = convert_file_search_filter(&api_filter);
/// // service_filter is now AttributeFilter::Comparison(...)
/// ```
pub fn convert_file_search_filter(filter: &FileSearchFilter) -> AttributeFilter {
    match filter {
        FileSearchFilter::Comparison(c) => {
            AttributeFilter::Comparison(convert_comparison_filter(c))
        }
        FileSearchFilter::Compound(c) => AttributeFilter::Compound(convert_compound_filter(c)),
    }
}

/// Convert a `FileSearchComparisonFilter` to a `ComparisonFilter`.
fn convert_comparison_filter(filter: &FileSearchComparisonFilter) -> ComparisonFilter {
    ComparisonFilter {
        operator: convert_comparison_operator(filter.type_),
        key: filter.key.clone(),
        value: convert_json_value_to_filter_value(&filter.value),
    }
}

/// Convert a `FileSearchCompoundFilter` to a `CompoundFilter`.
fn convert_compound_filter(filter: &FileSearchCompoundFilter) -> CompoundFilter {
    CompoundFilter {
        operator: convert_logical_operator(filter.type_),
        filters: filter
            .filters
            .iter()
            .map(convert_file_search_filter)
            .collect(),
    }
}

/// Convert `FileSearchFilterComparison` to `ComparisonOperator`.
fn convert_comparison_operator(op: FileSearchFilterComparison) -> ComparisonOperator {
    match op {
        FileSearchFilterComparison::Eq => ComparisonOperator::Eq,
        FileSearchFilterComparison::Ne => ComparisonOperator::Ne,
        FileSearchFilterComparison::Gt => ComparisonOperator::Gt,
        FileSearchFilterComparison::Gte => ComparisonOperator::Gte,
        FileSearchFilterComparison::Lt => ComparisonOperator::Lt,
        FileSearchFilterComparison::Lte => ComparisonOperator::Lte,
    }
}

/// Convert `FileSearchFilterLogicalType` to `LogicalOperator`.
fn convert_logical_operator(op: FileSearchFilterLogicalType) -> LogicalOperator {
    match op {
        FileSearchFilterLogicalType::And => LogicalOperator::And,
        FileSearchFilterLogicalType::Or => LogicalOperator::Or,
    }
}

/// Convert a `serde_json::Value` to a `FilterValue`.
///
/// Handles string, number, boolean, and array values. Objects and null
/// are converted to their JSON string representation as a fallback.
fn convert_json_value_to_filter_value(value: &serde_json::Value) -> FilterValue {
    match value {
        serde_json::Value::String(s) => FilterValue::String(s.clone()),
        serde_json::Value::Number(n) => FilterValue::Number(n.as_f64().unwrap_or_default()),
        serde_json::Value::Bool(b) => FilterValue::Boolean(*b),
        serde_json::Value::Array(arr) => {
            FilterValue::Array(
                arr.iter()
                    .filter_map(|v| match v {
                        serde_json::Value::String(s) => {
                            Some(crate::models::FilterValueItem::String(s.clone()))
                        }
                        serde_json::Value::Number(n) => Some(
                            crate::models::FilterValueItem::Number(n.as_f64().unwrap_or_default()),
                        ),
                        _ => None, // Skip non-string/number items
                    })
                    .collect(),
            )
        }
        // For null and objects, convert to string representation as fallback
        _ => FilterValue::String(value.to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Authentication Context
// ─────────────────────────────────────────────────────────────────────────────

/// Authentication context for file search access control.
///
/// This captures all relevant authentication information needed to verify
/// access to vector stores during file_search tool execution. It mirrors
/// the access checks performed in route handlers (`check_resource_access`).
///
/// # Access Control Rules
///
/// - **User-owned resources**: `user_id` must match the owner
/// - **Org-owned resources**: `org_id` matches OR `identity_org_ids` contains the owner
/// - **Project-owned resources**: `project_id` matches OR `identity_project_ids` contains the owner
#[derive(Debug, Clone, Default)]
pub struct FileSearchAuthContext {
    /// User ID from API key or identity
    pub user_id: Option<Uuid>,
    /// Organization ID from API key (direct ownership)
    pub org_id: Option<Uuid>,
    /// Project ID from API key (direct ownership)
    pub project_id: Option<Uuid>,
    /// Organization IDs from OAuth/OIDC identity claims
    pub identity_org_ids: Vec<String>,
    /// Project IDs from OAuth/OIDC identity claims
    pub identity_project_ids: Vec<String>,
}

impl FileSearchAuthContext {
    /// Create an auth context from an AuthenticatedRequest.
    pub fn from_auth(auth: &AuthenticatedRequest) -> Self {
        Self {
            user_id: auth.user_id(),
            org_id: auth.org_id(),
            project_id: auth.project_id(),
            identity_org_ids: auth
                .identity()
                .map(|i| i.org_ids.clone())
                .unwrap_or_default(),
            identity_project_ids: auth
                .identity()
                .map(|i| i.project_ids.clone())
                .unwrap_or_default(),
        }
    }

    /// Create an auth context from an optional AuthenticatedRequest.
    ///
    /// Returns a default (empty) context if auth is None, which will
    /// allow access when no authentication is configured.
    pub fn from_auth_optional(auth: Option<&AuthenticatedRequest>) -> Option<Self> {
        auth.map(Self::from_auth)
    }
}

/// Errors that can occur during file search middleware processing.
#[derive(Debug, Error)]
#[allow(dead_code)] // Variants will be used as implementation grows
pub enum FileSearchMiddlewareError {
    /// File search service is not configured.
    #[error("File search is not configured")]
    NotConfigured,

    /// Search operation failed.
    #[error("Search failed: {0}")]
    SearchFailed(String),

    /// Stream processing error.
    #[error("Stream error: {0}")]
    StreamError(String),

    /// Tool call parsing error.
    #[error("Failed to parse tool call: {0}")]
    ParseError(String),

    /// Maximum iterations exceeded.
    #[error("Maximum file_search iterations exceeded ({0})")]
    MaxIterationsExceeded(usize),

    /// Timeout during search.
    #[error("Search timed out after {0} seconds")]
    Timeout(u64),

    /// Provider continuation request failed.
    #[error("Provider error: {0}")]
    ProviderError(String),

    /// No provider callback configured.
    #[error("Provider callback not configured for multi-turn execution")]
    NoProviderCallback,
}

/// A detected file_search tool call from the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchToolCall {
    /// The tool call ID for the response.
    pub id: String,
    /// The search query.
    pub query: String,
    /// Vector store IDs to search (from the tool definition).
    pub vector_store_ids: Vec<String>,
    /// Maximum number of results (optional).
    pub max_num_results: Option<usize>,
    /// Minimum relevance score threshold (optional, 0.0-1.0).
    pub score_threshold: Option<f64>,
    /// Attribute filters to narrow search results (optional).
    pub filters: Option<FileSearchFilter>,
    /// Ranking options to control result scoring and reranking (optional).
    pub ranking_options: Option<crate::models::FileSearchRankingOptions>,
}

impl FileSearchToolCall {
    /// Generate a cache key for deduplication within a single request.
    ///
    /// The key uniquely identifies the search parameters (excluding the tool call ID,
    /// which changes per call even for identical queries). This allows caching results
    /// for identical queries within a single multi-turn conversation.
    ///
    /// The key is a JSON string of the relevant fields, which is simple and debuggable.
    /// For high-frequency use, consider switching to a hash-based approach.
    pub fn cache_key(&self) -> String {
        // Sort vector_store_ids for consistent ordering
        let mut sorted_ids = self.vector_store_ids.clone();
        sorted_ids.sort();

        // Serialize the relevant fields to JSON for a consistent key
        // Using serde_json ensures proper handling of Option types and nested structures
        serde_json::json!({
            "query": self.query,
            "vector_store_ids": sorted_ids,
            "max_num_results": self.max_num_results,
            "score_threshold": self.score_threshold,
            "filters": self.filters,
        })
        .to_string()
    }
}

/// Result of executing a file search tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchToolResult {
    /// The tool call ID this result corresponds to.
    pub tool_call_id: String,
    /// The search results as formatted content.
    pub content: String,
    /// Number of results returned.
    pub result_count: usize,
    /// Vector stores that were searched.
    pub vector_stores_searched: usize,
    /// The raw search response for building file_search_call output.
    #[serde(skip)]
    pub raw_response: Option<FileSearchResponse>,
}

/// Tracks file references from search results for citation annotation.
///
/// When the model generates a response with citation markers like `[Source 1]`,
/// this tracker provides the file information needed to create `FileCitation` annotations.
#[derive(Debug, Clone, Default)]
pub struct CitationTracker {
    /// Maps source number (1-indexed) to file information.
    sources: HashMap<usize, SourceInfo>,
}

/// Information about a source file from search results.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub file_id: Uuid,
    pub filename: String,
}

impl CitationTracker {
    /// Create a new citation tracker.
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
        }
    }

    /// Add sources from a file search response.
    ///
    /// Sources are numbered starting from 1, matching the format used in
    /// `format_search_results()`.
    pub fn add_from_response(&mut self, response: &FileSearchResponse) {
        for (i, result) in response.results.iter().enumerate() {
            let source_num = i + 1;
            self.sources.insert(
                source_num,
                SourceInfo {
                    file_id: result.file_id,
                    filename: result
                        .filename
                        .clone()
                        .unwrap_or_else(|| result.file_id.to_string()),
                },
            );
        }
    }

    /// Get source info by number.
    pub fn get(&self, source_num: usize) -> Option<&SourceInfo> {
        self.sources.get(&source_num)
    }

    /// Check if the tracker has any sources.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Parse citation markers from text and generate FileCitation annotations.
    ///
    /// Scans the text for patterns like `[Source 1]`, `[Source 2]`, etc. and creates
    /// `FileCitation` annotations with the file information from the tracker.
    ///
    /// Returns a list of annotations sorted by their position in the text.
    pub fn parse_citations(&self, text: &str) -> Vec<ResponsesAnnotation> {
        use regex::Regex;

        let mut annotations = Vec::new();

        // Match patterns like [Source 1], [Source 2], etc.
        // Also match variations: [source 1], [SOURCE 1], [Source1]
        let re = Regex::new(r"\[(?i)source\s*(\d+)\]").expect("Invalid regex");

        for cap in re.captures_iter(text) {
            if let (Some(full_match), Some(num_match)) = (cap.get(0), cap.get(1))
                && let Ok(source_num) = num_match.as_str().parse::<usize>()
                && let Some(source_info) = self.get(source_num)
            {
                // The index is the byte position where the citation marker starts
                let index = full_match.start() as u64;

                annotations.push(ResponsesAnnotation::FileCitation {
                    file_id: source_info.file_id.to_string(),
                    filename: source_info.filename.clone(),
                    index,
                });
            }
        }

        // Sort by index (position in text)
        annotations.sort_by_key(|a| match a {
            ResponsesAnnotation::FileCitation { index, .. } => *index,
            ResponsesAnnotation::UrlCitation { start_index, .. } => *start_index,
            ResponsesAnnotation::FilePath { index, .. } => *index,
            ResponsesAnnotation::ContainerFileCitation { start_index, .. } => *start_index,
        });

        annotations
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SSE Frame Buffer
// ─────────────────────────────────────────────────────────────────────────────

/// Context for file search middleware operations.
#[derive(Clone)]
pub struct FileSearchContext {
    /// The file search service for executing searches.
    pub service: Arc<FileSearchService>,
    /// Configuration for file search behavior.
    pub config: FileSearchConfig,
    /// Authentication context for access control (optional).
    /// When None, access control is bypassed (for deployments without auth).
    pub auth: Option<FileSearchAuthContext>,
    /// The file_search tool definitions from the request.
    pub tool_definitions: Vec<FileSearchTool>,
    /// The original request payload (used to build continuation requests).
    pub original_payload: CreateResponsesPayload,
}

impl FileSearchContext {
    /// Create a new file search context.
    pub fn new(
        service: Arc<FileSearchService>,
        config: FileSearchConfig,
        auth: Option<FileSearchAuthContext>,
        tool_definitions: Vec<FileSearchTool>,
        original_payload: CreateResponsesPayload,
    ) -> Self {
        Self {
            service,
            config,
            auth,
            tool_definitions,
            original_payload,
        }
    }

    /// Check if file search is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled && !self.tool_definitions.is_empty()
    }

    /// Get vector store IDs from the first file_search tool definition.
    pub fn get_vector_store_ids(&self) -> Vec<String> {
        self.tool_definitions
            .first()
            .map(|t| t.vector_store_ids.clone())
            .unwrap_or_default()
    }

    /// Execute a file search based on a tool call.
    #[instrument(skip(self), fields(
        tool_call_id = %tool_call.id,
        query = %tool_call.query,
        vector_store_ids = ?tool_call.vector_store_ids,
    ))]
    pub async fn execute_search(
        &self,
        tool_call: &FileSearchToolCall,
    ) -> Result<FileSearchToolResult, FileSearchMiddlewareError> {
        let start = Instant::now();
        let vector_stores_count = tool_call.vector_store_ids.len() as u32;

        info!(
            stage = "search_executing",
            tool_call_id = %tool_call.id,
            query = %tool_call.query,
            max_num_results = ?tool_call.max_num_results,
            score_threshold = ?tool_call.score_threshold,
            has_filters = tool_call.filters.is_some(),
            "Executing file search"
        );

        // Parse vector store IDs to UUIDs
        let vector_store_ids: Vec<Uuid> = tool_call
            .vector_store_ids
            .iter()
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect();

        if vector_store_ids.is_empty() {
            let duration_ms = start.elapsed().as_millis() as u64;
            record_file_search("error", start.elapsed().as_secs_f64(), 0, 0, false);
            error!(
                stage = "search_completed",
                tool_call_id = %tool_call.id,
                status = "error",
                duration_ms = duration_ms,
                "No valid vector store IDs provided"
            );
            otel_span_error!("No valid vector store IDs provided");
            return Err(FileSearchMiddlewareError::SearchFailed(
                "No valid vector store IDs provided".to_string(),
            ));
        }

        // Build the search request
        let max_results = tool_call
            .max_num_results
            .unwrap_or(self.config.max_results_per_search);

        // Use tool call's score_threshold if provided, otherwise fall back to config
        let threshold = tool_call
            .score_threshold
            .unwrap_or(self.config.score_threshold);

        // Convert API filter type to service filter type if provided
        let filters = tool_call.filters.as_ref().map(convert_file_search_filter);

        let request = FileSearchRequest {
            query: tool_call.query.clone(),
            vector_store_ids,
            max_results: Some(max_results),
            threshold: Some(threshold),
            file_ids: None,
            filters,
            ranking_options: tool_call.ranking_options.clone(),
        };

        // Execute the search with timeout.
        // Access control is enforced by the FileSearchAuth passed to the service,
        // which carries the caller's identity for ownership/membership checks.
        let search_future = self.service.search(request, self.auth.clone());
        let search_result = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.timeout_secs),
            search_future,
        )
        .await;

        let result = match search_result {
            Err(_) => {
                // Timeout
                let duration_ms = start.elapsed().as_millis() as u64;
                record_file_search(
                    "timeout",
                    start.elapsed().as_secs_f64(),
                    0,
                    vector_stores_count,
                    false,
                );
                warn!(
                    stage = "search_completed",
                    tool_call_id = %tool_call.id,
                    status = "timeout",
                    duration_ms = duration_ms,
                    timeout_secs = self.config.timeout_secs,
                    "File search timed out"
                );
                otel_span_error!("File search timed out");
                return Err(FileSearchMiddlewareError::Timeout(self.config.timeout_secs));
            }
            Ok(Err(e)) => {
                // Search failed
                let duration_ms = start.elapsed().as_millis() as u64;
                record_file_search(
                    "error",
                    start.elapsed().as_secs_f64(),
                    0,
                    vector_stores_count,
                    false,
                );
                error!(
                    stage = "search_completed",
                    tool_call_id = %tool_call.id,
                    status = "error",
                    duration_ms = duration_ms,
                    error = %e,
                    "File search failed"
                );
                otel_span_error!("File search failed: {}", e);
                return Err(FileSearchMiddlewareError::SearchFailed(e.to_string()));
            }
            Ok(Ok(result)) => result,
        };

        // Format results for the model with truncation to prevent context overflow
        let content = format_search_results_truncated(&result, self.config.max_search_result_chars);
        let result_count = result.results.len();
        let vector_stores_searched = result.vector_stores_searched;

        // Determine status based on whether we got results
        let status = if result_count == 0 {
            "no_results"
        } else {
            "success"
        };
        let duration_ms = start.elapsed().as_millis() as u64;
        record_file_search(
            status,
            start.elapsed().as_secs_f64(),
            result_count as u32,
            vector_stores_searched as u32,
            false,
        );

        info!(
            stage = "search_completed",
            tool_call_id = %tool_call.id,
            result_count = result_count,
            vector_stores_searched = vector_stores_searched,
            duration_ms = duration_ms,
            cache_hit = false,
            status = status,
            "File search completed"
        );

        otel_span_ok!();
        Ok(FileSearchToolResult {
            tool_call_id: tool_call.id.clone(),
            content,
            result_count,
            vector_stores_searched,
            raw_response: Some(result),
        })
    }
}

/// Format search results into a string suitable for the model (no truncation).
///
/// Convenience wrapper around `format_search_results_truncated` for tests
/// and cases where truncation is not needed.
#[cfg(test)]
fn format_search_results(response: &FileSearchResponse) -> String {
    format_search_results_truncated(response, usize::MAX)
}

/// Format search results with truncation to prevent context overflow.
///
/// Similar to `format_search_results`, but limits total output to `max_chars`.
/// Results are included in full - if adding a result would exceed the limit,
/// it is excluded entirely. A truncation notice is added if results were omitted.
///
/// # Arguments
/// * `response` - The search response to format
/// * `max_chars` - Maximum total characters in the output (0 for unlimited)
fn format_search_results_truncated(response: &FileSearchResponse, max_chars: usize) -> String {
    if response.results.is_empty() {
        return format!(
            "No results found for query: \"{}\".\nSearched {} vector store(s).",
            response.query, response.vector_stores_searched
        );
    }

    // If max_chars is 0 or usize::MAX, treat as unlimited
    let unlimited = max_chars == 0 || max_chars == usize::MAX;

    let header = format!(
        "Search results for query: \"{}\" ({} results from {} vector store(s)):\n\n",
        response.query,
        response.results.len(),
        response.vector_stores_searched
    );

    let citation_guidance =
        "When citing these sources, reference them as [Source N] where N is the source number.\n";

    let truncation_notice =
        "\n[... additional results truncated to prevent context overflow ...]\n";

    let mut output = header;
    let mut truncate_at: Option<usize> = None;

    for (i, result) in response.results.iter().enumerate() {
        let source_num = i + 1;

        // Format the source reference with file_id for citation tracking
        let source_ref = if let Some(filename) = &result.filename {
            format!(
                "[Source {}: {} (file_id: {})]",
                source_num, filename, result.file_id
            )
        } else {
            format!("[Source {}: file_id: {}]", source_num, result.file_id)
        };

        let result_text = format!(
            "--- {} (relevance: {:.1}%) ---\n{}\n\n",
            source_ref,
            result.score * 100.0,
            result.content
        );

        // Check if adding this result would exceed the limit
        if !unlimited {
            let potential_size = output.len()
                + result_text.len()
                + citation_guidance.len()
                + if i + 1 < response.results.len() {
                    truncation_notice.len()
                } else {
                    0
                };

            if potential_size > max_chars {
                truncate_at = Some(i);
                warn!(
                    stage = "results_truncated",
                    query = %response.query,
                    total_results = response.results.len(),
                    included_results = i,
                    max_chars = max_chars,
                    "Truncating file search results to prevent context overflow"
                );
                break;
            }
        }

        output.push_str(&result_text);
    }

    if truncate_at.is_some() {
        output.push_str(truncation_notice);
    }

    output.push_str(citation_guidance);

    output
}

/// Check if the request's `include` parameter contains `file_search_call.results`.
fn should_include_results(payload: &CreateResponsesPayload) -> bool {
    payload
        .include
        .as_ref()
        .map(|includes| includes.contains(&ResponsesIncludable::FileSearchCallResults))
        .unwrap_or(false)
}

/// Build a `file_search_call` output item from the search results.
///
/// This creates the output item that OpenAI returns when the model invokes
/// the file_search tool. When `include_results` is true, the detailed search
/// results are included in the response. `replay_content` — the formatted
/// retrieval text fed to the model — is always retained (independent of
/// `include_results`) so the call can be replayed on a later turn (see
/// [`rewrite_file_search_history`]).
fn build_file_search_call_output(
    tool_call_id: &str,
    query: &str,
    response: &FileSearchResponse,
    include_results: bool,
    replay_content: &str,
) -> FileSearchCallOutput {
    let results = if include_results {
        Some(
            response
                .results
                .iter()
                .map(|r| {
                    // Convert serde_json::Value metadata to HashMap if it's an object
                    let attributes = r.metadata.as_ref().and_then(|v| {
                        v.as_object()
                            .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    });

                    FileSearchResultItem {
                        file_id: r.file_id.to_string(),
                        filename: r.filename.clone().unwrap_or_else(|| "unknown".to_string()),
                        score: r.score,
                        attributes,
                        text: r.content.clone(),
                    }
                })
                .collect(),
        )
    } else {
        None
    };

    FileSearchCallOutput {
        type_: FileSearchCallOutputType::FileSearchCall,
        id: tool_call_id.to_string(),
        queries: vec![query.to_string()],
        status: WebSearchStatus::Completed,
        results,
        replay_content: Some(replay_content.to_string()),
    }
}

/// Format a `file_search_call` output item as an SSE event.
///
/// The format matches OpenAI's Responses API streaming format where each
/// output item is sent as an `response.output_item.done` event.
fn format_file_search_call_sse_event(output: &FileSearchCallOutput) -> Bytes {
    // Create the SSE event data
    // OpenAI sends output items as part of the response stream with type "response.output_item.done"
    let event_data = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": output,
    });

    // `FileSearchCallOutput` is a plain serde struct with no float/non-string
    // map keys, so serialization cannot fail; mirror the other formatters'
    // `unwrap_or_default()` so this always yields a terminal frame and never
    // strands the client stream.
    let json_str = serde_json::to_string(&event_data).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", json_str))
}

/// Build a self-contained handle for a `file_search` call whose arguments
/// couldn't be parsed. Emits a `file_search_call` item with status `failed`
/// (the spec's failure status) and feeds the error back as a
/// `function_call_output` so the loop continues and the model can retry.
#[cfg(not(target_arch = "wasm32"))]
fn synthesize_file_search_invalid_handle(
    call_id: &str,
    error: &str,
) -> crate::services::server_tools::ToolExecutionHandle {
    let id = call_id.to_string();
    let error_text = crate::services::server_tools::invalid_arguments_text("file_search", error);
    // The arguments couldn't be parsed, so there's no query to record; keep the
    // error as `replay_content` so a later-turn replay surfaces the same failure
    // rather than an empty retrieval.
    let failed_item = FileSearchCallOutput {
        type_: FileSearchCallOutputType::FileSearchCall,
        id: id.clone(),
        queries: Vec::new(),
        status: WebSearchStatus::Failed,
        results: None,
        replay_content: Some(error_text.clone()),
    };
    let events = vec![
        format_file_search_in_progress_event(&id, 0),
        format_file_search_call_sse_event(&failed_item),
    ];

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

/// Format a `response.file_search_call.in_progress` SSE event.
///
/// This event is emitted when a file_search tool call is detected and about to be executed.
/// It allows clients to show visual feedback while the search is in progress.
fn format_file_search_in_progress_event(item_id: &str, output_index: usize) -> Bytes {
    let event_data = serde_json::json!({
        "type": "response.file_search_call.in_progress",
        "output_index": output_index,
        "item_id": item_id,
    });
    let json_str = serde_json::to_string(&event_data).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", json_str))
}

/// Format a `response.file_search_call.searching` SSE event.
///
/// This event is emitted while the file search is actively executing.
fn format_file_search_searching_event(item_id: &str, output_index: usize) -> Bytes {
    let event_data = serde_json::json!({
        "type": "response.file_search_call.searching",
        "output_index": output_index,
        "item_id": item_id,
    });
    let json_str = serde_json::to_string(&event_data).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", json_str))
}

/// Format a `response.file_search_call.completed` SSE event.
///
/// This event is emitted when the file search completes successfully.
fn format_file_search_completed_event(item_id: &str, output_index: usize) -> Bytes {
    let event_data = serde_json::json!({
        "type": "response.file_search_call.completed",
        "output_index": output_index,
        "item_id": item_id,
    });
    let json_str = serde_json::to_string(&event_data).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", json_str))
}

/// Inject citation annotations into SSE stream chunks.
///
/// This function processes SSE events and adds `FileCitation` annotations
/// to `response.content_part.done` events based on citation markers found
/// in the text.
///
/// Returns the modified chunk with annotations injected, or the original
/// chunk if no modifications were needed.
fn inject_citation_annotations(chunk: &[u8], tracker: &CitationTracker) -> Bytes {
    if tracker.is_empty() {
        return Bytes::copy_from_slice(chunk);
    }

    let Ok(chunk_str) = std::str::from_utf8(chunk) else {
        return Bytes::copy_from_slice(chunk);
    };

    let mut output = String::new();

    for line in chunk_str.split_inclusive('\n') {
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data == "[DONE]" || data.is_empty() {
                output.push_str(line);
                continue;
            }

            // Try to parse and potentially modify the event
            if let Ok(mut json) = serde_json::from_str::<Value>(data) {
                let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

                // Handle response.content_part.done events
                if event_type == "response.content_part.done"
                    && let Some(part) = json.get_mut("part")
                    && let Some(part_obj) = part.as_object_mut()
                    && part_obj.get("type").and_then(|t| t.as_str()) == Some("output_text")
                    && let Some(text) = part_obj.get("text").and_then(|t| t.as_str())
                {
                    // Parse citations from the text
                    let annotations = tracker.parse_citations(text);

                    if !annotations.is_empty() {
                        // Serialize annotations
                        let annotations_json =
                            serde_json::to_value(&annotations).unwrap_or(serde_json::json!([]));

                        // Update the annotations field
                        part_obj.insert("annotations".to_string(), annotations_json);

                        debug!(
                            stage = "annotations_injected",
                            annotation_count = annotations.len(),
                            "Injected citation annotations into response.content_part.done"
                        );
                    }
                }

                // Re-serialize and format as SSE
                if let Ok(json_str) = serde_json::to_string(&json) {
                    output.push_str("data: ");
                    output.push_str(&json_str);
                    output.push_str("\n\n");
                    continue;
                }
            }
        }

        // If we couldn't modify the line, just pass it through
        output.push_str(line);
    }

    Bytes::from(output)
}

/// Parse a file_search tool call from a JSON value.
///
/// Expected format (from model response):
/// ```json
/// {
///   "type": "function_call",
///   "name": "file_search",
///   "call_id": "call_xyz",
///   "arguments": "{\"query\": \"search query\", \"max_num_results\": 5, \"score_threshold\": 0.7, \"filters\": {...}}"
/// }
/// ```
///
/// All fields except `query` are optional. This function uses [`FileSearchToolArguments::parse()`]
/// to deserialize the arguments, ensuring consistency with the schema sent to the model.
/// Outcome of inspecting a `function_call` item named `file_search`.
///
/// `Invalid` carries the call id and reason so the executor can synthesize
/// a `file_search_call` with status `failed` rather than dropping the call
/// (which would strand the loop) or aborting the whole turn. `None` means
/// the item is not a file_search call and should pass through untouched.
#[derive(Debug, Clone)]
pub enum FileSearchCallDetection {
    Valid(Box<FileSearchToolCall>),
    Invalid { id: String, error: String },
}

pub fn parse_file_search_tool_call(
    value: &Value,
    vector_store_ids: &[String],
) -> Option<FileSearchCallDetection> {
    // Check if this is a function call
    let obj = value.as_object()?;

    // Check type
    let type_val = obj.get("type")?.as_str()?;
    if type_val != "function_call" {
        return None;
    }

    // Check name
    let name = obj.get("name")?.as_str()?;
    if name != "file_search" {
        return None;
    }

    // Get call ID
    let id = obj
        .get("call_id")
        .or_else(|| obj.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Parse arguments using the schema-defined struct
    let arguments_str = obj.get("arguments")?.as_str()?;
    match FileSearchToolArguments::parse(arguments_str) {
        Ok(args) => Some(FileSearchCallDetection::Valid(Box::new(
            FileSearchToolCall {
                id,
                query: args.query,
                vector_store_ids: vector_store_ids.to_vec(),
                max_num_results: args.max_num_results.map(|v| v as usize),
                score_threshold: args.score_threshold,
                filters: args.filters,
                ranking_options: args.ranking_options,
            },
        ))),
        Err(e) => Some(FileSearchCallDetection::Invalid {
            id,
            error: format!("could not parse `arguments` (expected {{\"query\": \"...\"}}): {e}"),
        }),
    }
}

/// Check if a streaming chunk contains file_search tool calls.
///
/// For SSE streams, we need to parse the data field and check for tool calls.
/// Returns all file_search tool calls found in the chunk.
pub fn detect_file_search_in_chunk(
    chunk: &[u8],
    vector_store_ids: &[String],
) -> Vec<FileSearchCallDetection> {
    let Some(chunk_str) = std::str::from_utf8(chunk).ok() else {
        return Vec::new();
    };

    let mut found_calls = Vec::new();

    // Handle SSE format - look for "data:" lines
    for line in chunk_str.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data == "[DONE]" {
                continue;
            }

            // Try to parse as JSON
            if let Ok(json) = serde_json::from_str::<Value>(data) {
                // Check for tool calls in various response formats

                // OpenAI-style responses API output
                if let Some(output) = json.get("output").and_then(|o| o.as_array()) {
                    for item in output {
                        if let Some(tool_call) = parse_file_search_tool_call(item, vector_store_ids)
                        {
                            found_calls.push(tool_call);
                        }
                    }
                }

                // Check for function_call directly in the response
                if let Some(tool_call) = parse_file_search_tool_call(&json, vector_store_ids) {
                    found_calls.push(tool_call);
                }

                // Check for response.function_call_arguments.done events (Responses API streaming)
                // Format: {"type": "response.function_call_arguments.done", "name": "file_search",
                //          "item_id": "...", "arguments": "{...}"}
                if json.get("type").and_then(|t| t.as_str())
                    == Some("response.function_call_arguments.done")
                    && json.get("name").and_then(|n| n.as_str()) == Some("file_search")
                {
                    let id = json
                        .get("item_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    if let Some(arguments_str) = json.get("arguments").and_then(|a| a.as_str()) {
                        match FileSearchToolArguments::parse(arguments_str) {
                            Ok(args) => found_calls.push(FileSearchCallDetection::Valid(Box::new(
                                FileSearchToolCall {
                                    id,
                                    query: args.query,
                                    vector_store_ids: vector_store_ids.to_vec(),
                                    max_num_results: args.max_num_results.map(|v| v as usize),
                                    score_threshold: args.score_threshold,
                                    filters: args.filters,
                                    ranking_options: args.ranking_options,
                                },
                            ))),
                            Err(e) => found_calls.push(FileSearchCallDetection::Invalid {
                                id,
                                error: format!(
                                    "could not parse `arguments` (expected {{\"query\": \"...\"}}): {e}"
                                ),
                            }),
                        }
                    }
                }

                // Check for response.output_item.done events (Responses API streaming)
                // Format: {"type": "response.output_item.done", "item": {"type": "function_call", ...}}
                if json.get("type").and_then(|t| t.as_str()) == Some("response.output_item.done")
                    && let Some(item) = json.get("item")
                    && let Some(tool_call) = parse_file_search_tool_call(item, vector_store_ids)
                {
                    found_calls.push(tool_call);
                }

                // Check delta for streaming responses
                if let Some(delta) = json.get("delta")
                    && let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array())
                {
                    for tc in tool_calls {
                        if let Some(tool_call) = parse_file_search_tool_call(tc, vector_store_ids) {
                            found_calls.push(tool_call);
                        }
                    }
                }

                // Check choices array for chat completion format
                if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                    for choice in choices {
                        if let Some(delta) = choice.get("delta")
                            && let Some(tool_calls) =
                                delta.get("tool_calls").and_then(|t| t.as_array())
                        {
                            for tc in tool_calls {
                                if let Some(tool_call) =
                                    parse_file_search_tool_call(tc, vector_store_ids)
                                {
                                    found_calls.push(tool_call);
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

/// Check a non-streaming response for file_search tool calls.
///
/// Returns the detected tool calls and whether the response requires
/// file search execution.
#[allow(dead_code)] // Will be used for non-streaming responses
pub fn check_response_for_file_search(
    body: &[u8],
    vector_store_ids: &[String],
) -> Vec<FileSearchToolCall> {
    let Ok(json) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };

    let mut tool_calls = Vec::new();

    // Check output array (Responses API format)
    if let Some(output) = json.get("output").and_then(|o| o.as_array()) {
        for item in output {
            if let Some(FileSearchCallDetection::Valid(tool_call)) =
                parse_file_search_tool_call(item, vector_store_ids)
            {
                tool_calls.push(*tool_call);
            }
        }
    }

    // Check choices array (Chat Completions format)
    if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            if let Some(message) = choice.get("message")
                && let Some(tc_array) = message.get("tool_calls").and_then(|t| t.as_array())
            {
                for tc in tc_array {
                    if let Some(FileSearchCallDetection::Valid(tool_call)) =
                        parse_file_search_tool_call(tc, vector_store_ids)
                    {
                        tool_calls.push(*tool_call);
                    }
                }
            }
        }
    }

    tool_calls
}

/// Format a tool result as JSON for sending back to the provider.
#[allow(dead_code)] // Will be used for non-streaming responses
pub fn format_tool_result_json(result: &FileSearchToolResult) -> Value {
    serde_json::json!({
        "role": "tool",
        "tool_call_id": result.tool_call_id,
        "content": result.content
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// ServerExecutedTool implementation
// ─────────────────────────────────────────────────────────────────────────────

/// `ServerExecutedTool` implementation for `file_search`.
///
/// Wraps a `FileSearchContext` plus per-request shared state (query cache and
/// citation tracker) so the runner can dispatch concurrent calls against a
/// single instance while preserving the original wrapper's behaviour.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileSearchExecutor {
    context: FileSearchContext,
    /// Citation tracker shared across all calls for this request, used by
    /// `transform_event` to inject `FileCitation` annotations into events
    /// the model emits after a search has completed.
    citation_tracker: std::sync::Mutex<CitationTracker>,
    /// Query cache deduplicates identical searches within one request.
    query_cache: tokio::sync::Mutex<HashMap<String, FileSearchToolResult>>,
    /// Hides the rewritten `file_search` function-call plumbing from the
    /// client stream; the executor emits the spec-shaped
    /// `file_search_call` items itself.
    suppressor: crate::services::server_tools::FunctionCallSuppressor,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileSearchExecutor {
    pub fn new(context: FileSearchContext) -> Self {
        Self {
            context,
            citation_tracker: std::sync::Mutex::new(CitationTracker::new()),
            query_cache: tokio::sync::Mutex::new(HashMap::new()),
            suppressor: crate::services::server_tools::FunctionCallSuppressor::new(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl crate::services::server_tools::ServerExecutedTool for FileSearchExecutor {
    fn name(&self) -> &'static str {
        "file_search"
    }

    fn is_enabled_for(&self, _payload: &CreateResponsesPayload) -> bool {
        self.context.is_enabled()
    }

    fn detect(
        &self,
        event: &[u8],
        _ctx: &crate::services::server_tools::ToolContext,
    ) -> Vec<crate::services::server_tools::DetectedToolCall> {
        let vector_store_ids = self.context.get_vector_store_ids();
        detect_file_search_in_chunk(event, &vector_store_ids)
            .into_iter()
            .map(|detection| match detection {
                FileSearchCallDetection::Valid(tc) => {
                    crate::services::server_tools::DetectedToolCall::new(
                        "file_search",
                        tc.id.clone(),
                        serde_json::to_value(&*tc).unwrap_or(Value::Null),
                    )
                }
                FileSearchCallDetection::Invalid { id, error } => {
                    crate::services::server_tools::DetectedToolCall::invalid(
                        "file_search",
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
        // The model emitted a `file_search` call we recognized but couldn't
        // parse. Surface a `file_search_call` with status `failed` and feed
        // the error back so the loop continues — never drop it or abort.
        if let Some(error) = &call.invalid {
            return Ok(synthesize_file_search_invalid_handle(&call.call_id, error));
        }
        let tool_call: FileSearchToolCall =
            serde_json::from_value(call.arguments).map_err(|e| {
                crate::services::server_tools::ToolError::InvalidCall(format!(
                    "could not deserialize FileSearchToolCall: {e}"
                ))
            })?;
        let include_results = should_include_results(&ctx.original_payload);
        let cache_key = tool_call.cache_key();
        let context = self.context.clone();
        let call_id = call.call_id.clone();

        // Channel carries the progress/output events to the runner.
        let (event_tx, event_rx) = mpsc::channel::<Bytes>(8);

        // Future producing the final ToolCallResult.
        let cache_handle = &self.query_cache;
        let tracker_handle = &self.citation_tracker;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<
            Result<
                crate::services::server_tools::ToolCallResult,
                crate::services::server_tools::ToolError,
            >,
        >();

        // Clone the mutex references into the spawned task.
        // SAFETY: Mutex<T> is Send+Sync when T is — but we need owned handles.
        // Move via Arcs is the standard pattern; since the executor owns the
        // mutexes, callers must keep the executor alive while execute() is
        // running. The runner holds `Arc<dyn ServerExecutedTool>`, so this
        // is naturally upheld.
        // We work around by accessing the mutex via the spawned closure
        // borrowing the executor — but that needs lifetime tricks. Instead
        // we'll just clone what we need synchronously before spawning.
        let cached_result = {
            let cache = cache_handle.lock().await;
            cache.get(&cache_key).cloned()
        };

        // Shared citation tracker: capture by Arc<Mutex<...>> for the result
        // task. We need an Arc not a borrow.
        // Since CitationTracker is per-instance, we'll do the update
        // synchronously after the result is ready, before completing.

        // To avoid lifetime complications, we collect the work synchronously
        // here and only stream the events through a small async task.
        let (in_progress_evt, searching_evt) = (
            format_file_search_in_progress_event(&tool_call.id, 0),
            format_file_search_searching_event(&tool_call.id, 0),
        );

        // Send in_progress + searching synchronously into the channel.
        let _ = event_tx.send(in_progress_evt).await;
        let _ = event_tx.send(searching_evt).await;

        // Perform the search (or use cache).
        let search_result = if let Some(cached) = cached_result {
            info!(
                stage = "search_completed",
                tool_call_id = %tool_call.id,
                query = %tool_call.query,
                cache_hit = true,
                "Returning cached file_search result"
            );
            FileSearchToolResult {
                tool_call_id: tool_call.id.clone(),
                ..cached
            }
        } else {
            match context.execute_search(&tool_call).await {
                Ok(r) => {
                    let mut cache = cache_handle.lock().await;
                    cache.insert(cache_key, r.clone());
                    r
                }
                Err(e) => {
                    let _ = result_tx.send(Err(
                        crate::services::server_tools::ToolError::ExecutionFailed(e.to_string()),
                    ));
                    // Drop event_tx so the events stream completes.
                    drop(event_tx);
                    return Ok(crate::services::server_tools::ToolExecutionHandle {
                        events: Box::pin(futures_util::stream::unfold(
                            event_rx,
                            |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
                        )),
                        result: Box::pin(async move {
                            result_rx.await.map_err(|_| {
                                crate::services::server_tools::ToolError::ExecutionFailed(
                                    "result channel closed".into(),
                                )
                            })?
                        }),
                    });
                }
            }
        };

        // Update citation tracker.
        if let Some(ref raw) = search_result.raw_response {
            if let Ok(mut tracker) = tracker_handle.lock() {
                tracker.add_from_response(raw);
                debug!(
                    stage = "citations_tracked",
                    tool_call_id = %tool_call.id,
                    source_count = raw.results.len(),
                    "Added search results to citation tracker"
                );
            }

            // Emit the file_search_call output_item.done event. `search_result.content`
            // is the formatted retrieval text fed to the model below; retain it as
            // `replay_content` so the call replays on a later turn.
            let call_output = build_file_search_call_output(
                &tool_call.id,
                &tool_call.query,
                raw,
                include_results,
                &search_result.content,
            );
            let _ = event_tx
                .send(format_file_search_call_sse_event(&call_output))
                .await;
        }

        // Emit the completed event.
        let completed_evt = format_file_search_completed_event(&tool_call.id, 0);
        let _ = event_tx.send(completed_evt).await;

        // Build the continuation item (FunctionCallOutput) for the next turn.
        let cont_item = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
            type_: FunctionCallOutputType::FunctionCallOutput,
            id: Some(tool_call.id.clone()),
            call_id: tool_call.id.clone(),
            output: search_result.content.clone(),
            status: None,
        });

        let _ = result_tx.send(Ok(crate::services::server_tools::ToolCallResult {
            call_id,
            continuation_items: vec![cont_item],
        }));

        drop(event_tx);

        Ok(crate::services::server_tools::ToolExecutionHandle {
            events: Box::pin(futures_util::stream::unfold(
                event_rx,
                |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
            )),
            result: Box::pin(async move {
                result_rx.await.map_err(|_| {
                    crate::services::server_tools::ToolError::ExecutionFailed(
                        "result channel closed".into(),
                    )
                })?
            }),
        })
    }

    fn apply_to_continuation(
        &self,
        payload: &mut CreateResponsesPayload,
        results: &[crate::services::server_tools::ToolCallResult],
        is_final_iteration: bool,
    ) {
        // Append all function-call outputs to the input.
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

        // On the final iteration, strip file_search tool definitions so the
        // model is forced to produce a text response instead of looping.
        if is_final_iteration && let Some(ref mut tools) = payload.tools {
            let before = tools.len();
            tools.retain(|t| !t.is_file_search());
            // Preprocessing rewrites the spec `file_search` tool into a
            // `Function{name:"file_search"}` form, which `is_file_search()`
            // does not match — strip that form too, otherwise the model can
            // keep calling file_search past `max_iterations`.
            tools.retain(|t| {
                if let ResponsesToolDefinition::Function(f) = t {
                    f.name != FileSearchToolArguments::FUNCTION_NAME
                } else {
                    true
                }
            });
            if tools.len() < before {
                info!(
                    stage = "tools_removed",
                    removed = before - tools.len(),
                    "Removed file_search tools on final iteration to force completion"
                );
            }
            if tools.is_empty() {
                payload.tools = None;
            }
        }
    }

    fn transform_event(&self, event: Bytes) -> Bytes {
        // Hide the rewritten `file_search` function-call plumbing first;
        // the executor emits the spec-shaped `file_search_call` items.
        let event = self
            .suppressor
            .suppress(event, |name| name == "file_search");
        if event.is_empty() {
            return event;
        }
        let Ok(tracker) = self.citation_tracker.lock() else {
            return event;
        };
        if tracker.is_empty() {
            return event;
        }
        inject_citation_annotations(&event, &tracker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::SseBuffer;

    /// Unwrap a detection to its valid call, panicking if it's invalid —
    /// keeps the detection-shape tests terse.
    fn expect_valid(detection: FileSearchCallDetection) -> FileSearchToolCall {
        match detection {
            FileSearchCallDetection::Valid(tc) => *tc,
            FileSearchCallDetection::Invalid { error, .. } => {
                panic!("expected a valid file_search call, got invalid: {error}")
            }
        }
    }

    /// Like [`detect_file_search_in_chunk`] but unwraps every detection to a
    /// valid call for the happy-path detection tests.
    fn detect_valid(chunk: &[u8], vector_store_ids: &[String]) -> Vec<FileSearchToolCall> {
        detect_file_search_in_chunk(chunk, vector_store_ids)
            .into_iter()
            .map(expect_valid)
            .collect()
    }

    #[test]
    fn test_parse_file_search_tool_call() {
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_123",
            "arguments": "{\"query\": \"revenue growth in Q3\"}"
        });

        let vector_store_ids = vec!["vs_abc123".to_string()];
        let result = parse_file_search_tool_call(&json, &vector_store_ids);

        assert!(result.is_some());
        let tool_call = expect_valid(result.unwrap());
        assert_eq!(tool_call.id, "call_123");
        assert_eq!(tool_call.query, "revenue growth in Q3");
        assert_eq!(tool_call.vector_store_ids, vector_store_ids);
    }

    #[test]
    fn test_parse_file_search_tool_call_not_file_search() {
        let json = serde_json::json!({
            "type": "function_call",
            "name": "get_weather",
            "call_id": "call_456",
            "arguments": "{\"location\": \"San Francisco\"}"
        });

        let result = parse_file_search_tool_call(&json, &["vs_abc".to_string()]);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_file_search_tool_call_invalid_arguments() {
        // `query` is a number, not a string → fails to deserialize.
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_bad",
            "arguments": "{\"query\": 123}"
        });
        let FileSearchCallDetection::Invalid { id, error } =
            parse_file_search_tool_call(&json, &["vs_abc".to_string()]).unwrap()
        else {
            panic!("expected an invalid file_search call");
        };
        assert_eq!(id, "call_bad");
        assert!(!error.is_empty());
    }

    #[test]
    fn test_parse_file_search_tool_call_with_max_results() {
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_789",
            "arguments": "{\"query\": \"annual report\", \"max_num_results\": 5}"
        });

        let vector_store_ids = vec!["vs_abc".to_string()];
        let result = expect_valid(parse_file_search_tool_call(&json, &vector_store_ids).unwrap());

        assert_eq!(result.max_num_results, Some(5));
    }

    #[test]
    fn test_parse_file_search_tool_call_with_score_threshold() {
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_threshold",
            "arguments": "{\"query\": \"policy document\", \"score_threshold\": 0.85}"
        });

        let vector_store_ids = vec!["vs_abc".to_string()];
        let result = expect_valid(parse_file_search_tool_call(&json, &vector_store_ids).unwrap());

        assert_eq!(result.query, "policy document");
        assert_eq!(result.score_threshold, Some(0.85));
        assert!(result.filters.is_none());
    }

    #[test]
    fn test_parse_file_search_tool_call_with_comparison_filter() {
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_filter",
            "arguments": "{\"query\": \"budget report\", \"filters\": {\"type\": \"eq\", \"key\": \"department\", \"value\": \"finance\"}}"
        });

        let vector_store_ids = vec!["vs_abc".to_string()];
        let result = expect_valid(parse_file_search_tool_call(&json, &vector_store_ids).unwrap());

        assert_eq!(result.query, "budget report");
        assert!(result.filters.is_some());

        // Verify it's a comparison filter
        match result.filters.unwrap() {
            FileSearchFilter::Comparison(f) => {
                assert_eq!(f.key, "department");
                assert_eq!(f.value, serde_json::json!("finance"));
            }
            _ => panic!("Expected comparison filter"),
        }
    }

    #[test]
    fn test_parse_file_search_tool_call_with_compound_filter() {
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_compound",
            "arguments": "{\"query\": \"meeting notes\", \"filters\": {\"type\": \"and\", \"filters\": [{\"type\": \"eq\", \"key\": \"year\", \"value\": 2024}, {\"type\": \"eq\", \"key\": \"status\", \"value\": \"approved\"}]}}"
        });

        let vector_store_ids = vec!["vs_abc".to_string()];
        let result = expect_valid(parse_file_search_tool_call(&json, &vector_store_ids).unwrap());

        assert_eq!(result.query, "meeting notes");
        assert!(result.filters.is_some());

        // Verify it's a compound filter
        match result.filters.unwrap() {
            FileSearchFilter::Compound(f) => {
                assert_eq!(f.filters.len(), 2);
            }
            _ => panic!("Expected compound filter"),
        }
    }

    #[test]
    fn test_parse_file_search_tool_call_with_all_arguments() {
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_full",
            "arguments": "{\"query\": \"quarterly earnings\", \"max_num_results\": 10, \"score_threshold\": 0.75, \"filters\": {\"type\": \"eq\", \"key\": \"quarter\", \"value\": \"Q3\"}}"
        });

        let vector_store_ids = vec!["vs_abc".to_string(), "vs_def".to_string()];
        let result = expect_valid(parse_file_search_tool_call(&json, &vector_store_ids).unwrap());

        assert_eq!(result.id, "call_full");
        assert_eq!(result.query, "quarterly earnings");
        assert_eq!(result.vector_store_ids, vector_store_ids);
        assert_eq!(result.max_num_results, Some(10));
        assert_eq!(result.score_threshold, Some(0.75));
        assert!(result.filters.is_some());
    }

    #[test]
    fn test_parse_file_search_tool_call_backward_compatible_query_only() {
        // Ensure backward compatibility: calls with only query should still work
        let json = serde_json::json!({
            "type": "function_call",
            "name": "file_search",
            "call_id": "call_simple",
            "arguments": "{\"query\": \"simple search\"}"
        });

        let vector_store_ids = vec!["vs_abc".to_string()];
        let result = expect_valid(parse_file_search_tool_call(&json, &vector_store_ids).unwrap());

        assert_eq!(result.query, "simple search");
        assert!(result.max_num_results.is_none());
        assert!(result.score_threshold.is_none());
        assert!(result.filters.is_none());
    }

    #[test]
    fn test_detect_file_search_in_sse_chunk() {
        let chunk = b"data: {\"output\": [{\"type\": \"function_call\", \"name\": \"file_search\", \"call_id\": \"call_abc\", \"arguments\": \"{\\\"query\\\": \\\"test query\\\"}\"}]}\n\n";

        let vector_store_ids = vec!["vs_test".to_string()];
        let results = detect_valid(chunk, &vector_store_ids);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "call_abc");
        assert_eq!(results[0].query, "test query");
    }

    #[test]
    fn test_detect_file_search_in_sse_chunk_no_match() {
        let chunk =
            b"data: {\"output\": [{\"type\": \"message\", \"content\": \"Hello world\"}]}\n\n";

        let results = detect_file_search_in_chunk(chunk, &["vs_test".to_string()]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_detect_file_search_done_message() {
        let chunk = b"data: [DONE]\n\n";

        let results = detect_file_search_in_chunk(chunk, &["vs_test".to_string()]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_detect_file_search_function_call_arguments_done() {
        // Test response.function_call_arguments.done event format from Responses API streaming
        let chunk = br#"data: {"type": "response.function_call_arguments.done", "item_id": "fc_abc123", "name": "file_search", "output_index": 0, "arguments": "{\"query\": \"budget report\", \"max_num_results\": 5}", "sequence_number": 10}

"#;

        let vector_store_ids = vec!["vs_test".to_string()];
        let results = detect_valid(chunk, &vector_store_ids);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "fc_abc123");
        assert_eq!(results[0].query, "budget report");
        assert_eq!(results[0].max_num_results, Some(5));
        assert_eq!(results[0].vector_store_ids, vec!["vs_test".to_string()]);
    }

    #[test]
    fn test_detect_file_search_function_call_arguments_done_with_filters() {
        // Test with score_threshold and filters
        let chunk = br#"data: {"type": "response.function_call_arguments.done", "item_id": "fc_xyz", "name": "file_search", "output_index": 1, "arguments": "{\"query\": \"quarterly sales\", \"score_threshold\": 0.7}", "sequence_number": 5}

"#;

        let results = detect_valid(chunk, &["vs_123".to_string()]);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].query, "quarterly sales");
        assert_eq!(results[0].score_threshold, Some(0.7));
    }

    #[test]
    fn test_detect_file_search_function_call_arguments_done_wrong_name() {
        // Should not match when function name is not "file_search"
        let chunk = br#"data: {"type": "response.function_call_arguments.done", "item_id": "fc_abc", "name": "get_weather", "output_index": 0, "arguments": "{\"location\": \"NYC\"}", "sequence_number": 1}

"#;

        let results = detect_file_search_in_chunk(chunk, &["vs_test".to_string()]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_detect_file_search_output_item_done() {
        // Test response.output_item.done event format from Responses API streaming
        let chunk = br#"data: {"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call", "id": "fc_item123", "call_id": "call_456", "name": "file_search", "arguments": "{\"query\": \"project status\"}", "status": "completed"}, "sequence_number": 15}

"#;

        let vector_store_ids = vec!["vs_prod".to_string()];
        let results = detect_valid(chunk, &vector_store_ids);

        assert_eq!(results.len(), 1);
        // parse_file_search_tool_call prefers call_id over id
        assert_eq!(results[0].id, "call_456");
        assert_eq!(results[0].query, "project status");
        assert_eq!(results[0].vector_store_ids, vec!["vs_prod".to_string()]);
    }

    #[test]
    fn test_detect_file_search_output_item_done_message_type() {
        // Should not match when item type is "message" (not function_call)
        let chunk = br#"data: {"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "id": "msg_123", "role": "assistant", "content": [{"type": "output_text", "text": "Hello!"}], "status": "completed"}, "sequence_number": 5}

"#;

        let results = detect_file_search_in_chunk(chunk, &["vs_test".to_string()]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_detect_file_search_output_item_done_wrong_function() {
        // Should not match when function name is not "file_search"
        let chunk = br#"data: {"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "id": "fc_other", "call_id": "call_other", "name": "get_current_time", "arguments": "{}", "status": "completed"}, "sequence_number": 8}

"#;

        let results = detect_file_search_in_chunk(chunk, &["vs_test".to_string()]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_detect_multiple_file_search_in_output() {
        // Test detecting multiple file_search calls in the same output array
        let chunk = br#"data: {"output": [{"type": "function_call", "name": "file_search", "call_id": "call_1", "arguments": "{\"query\": \"Q1 revenue\"}"}, {"type": "function_call", "name": "file_search", "call_id": "call_2", "arguments": "{\"query\": \"Q2 expenses\"}"}]}

"#;

        let vector_store_ids = vec!["vs_finance".to_string()];
        let results = detect_valid(chunk, &vector_store_ids);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "call_1");
        assert_eq!(results[0].query, "Q1 revenue");
        assert_eq!(results[1].id, "call_2");
        assert_eq!(results[1].query, "Q2 expenses");
    }

    #[test]
    fn test_detect_multiple_file_search_mixed_with_other_tools() {
        // Test detecting file_search calls when mixed with other tool types
        let chunk = br#"data: {"output": [{"type": "function_call", "name": "get_weather", "call_id": "weather_1", "arguments": "{\"location\": \"NYC\"}"}, {"type": "function_call", "name": "file_search", "call_id": "search_1", "arguments": "{\"query\": \"weather data\"}"}, {"type": "message", "content": "Processing..."}, {"type": "function_call", "name": "file_search", "call_id": "search_2", "arguments": "{\"query\": \"climate report\"}"}]}

"#;

        let vector_store_ids = vec!["vs_data".to_string()];
        let results = detect_valid(chunk, &vector_store_ids);

        // Should only detect the 2 file_search calls, not get_weather or message
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "search_1");
        assert_eq!(results[0].query, "weather data");
        assert_eq!(results[1].id, "search_2");
        assert_eq!(results[1].query, "climate report");
    }

    #[test]
    fn test_check_response_for_file_search() {
        let body = serde_json::json!({
            "output": [
                {
                    "type": "function_call",
                    "name": "file_search",
                    "call_id": "call_xyz",
                    "arguments": "{\"query\": \"budget analysis\"}"
                }
            ]
        });

        let vector_store_ids = vec!["vs_abc".to_string()];
        let results =
            check_response_for_file_search(body.to_string().as_bytes(), &vector_store_ids);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].query, "budget analysis");
    }

    #[test]
    fn test_format_tool_result_json() {
        let result = FileSearchToolResult {
            tool_call_id: "call_123".to_string(),
            content: "Search results...".to_string(),
            result_count: 3,
            vector_stores_searched: 1,
            raw_response: None,
        };

        let json = format_tool_result_json(&result);

        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_123");
        assert_eq!(json["content"], "Search results...");
    }

    #[test]
    fn test_format_search_results_empty() {
        use crate::services::FileSearchResponse;

        let response = FileSearchResponse {
            results: vec![],
            query: "test query".to_string(),
            vector_stores_searched: 2,
        };

        let formatted = format_search_results(&response);

        assert!(formatted.contains("No results found"));
        assert!(formatted.contains("test query"));
        assert!(formatted.contains("2 vector store(s)"));
    }

    #[test]
    fn test_format_search_results_with_results() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "This is the content of the chunk.".to_string(),
                score: 0.95,
                filename: Some("report.pdf".to_string()),
                metadata: None,
            }],
            query: "annual report".to_string(),
            vector_stores_searched: 1,
        };

        let formatted = format_search_results(&response);

        // Check query and result count
        assert!(formatted.contains("annual report"));
        assert!(formatted.contains("1 results"));

        // Check source formatting with filename and file_id
        assert!(formatted.contains("[Source 1: report.pdf"));
        assert!(formatted.contains(&file_id.to_string()));

        // Check relevance percentage (95.0%)
        assert!(formatted.contains("95.0%"));

        // Check content
        assert!(formatted.contains("This is the content"));

        // Check citation guidance
        assert!(formatted.contains("When citing these sources"));
    }

    #[test]
    fn test_format_search_results_without_filename() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Content without filename.".to_string(),
                score: 0.80,
                filename: None,
                metadata: None,
            }],
            query: "test query".to_string(),
            vector_stores_searched: 1,
        };

        let formatted = format_search_results(&response);

        // Check source formatting without filename
        assert!(formatted.contains("[Source 1: file_id:"));
        assert!(formatted.contains(&file_id.to_string()));
        assert!(formatted.contains("80.0%"));
    }

    #[test]
    fn test_build_file_search_call_output_without_results() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Test content".to_string(),
                score: 0.85,
                filename: Some("test.pdf".to_string()),
                metadata: None,
            }],
            query: "test query".to_string(),
            vector_stores_searched: 1,
        };

        let output =
            build_file_search_call_output("call_123", "test query", &response, false, "formatted");

        assert_eq!(output.id, "call_123");
        assert_eq!(output.queries, vec!["test query"]);
        assert_eq!(output.status, WebSearchStatus::Completed);
        assert!(output.results.is_none()); // Results not included
        // replay_content is retained even when results are not included.
        assert_eq!(output.replay_content.as_deref(), Some("formatted"));
    }

    #[test]
    fn test_build_file_search_call_output_with_results() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Test content".to_string(),
                score: 0.85,
                filename: Some("test.pdf".to_string()),
                metadata: Some(serde_json::json!({"author": "Test Author"})),
            }],
            query: "test query".to_string(),
            vector_stores_searched: 1,
        };

        let output =
            build_file_search_call_output("call_456", "test query", &response, true, "formatted");

        assert_eq!(output.id, "call_456");
        assert_eq!(output.queries, vec!["test query"]);
        assert_eq!(output.status, WebSearchStatus::Completed);
        assert!(output.results.is_some());

        let results = output.results.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_id, file_id.to_string());
        assert_eq!(results[0].filename, "test.pdf");
        assert_eq!(results[0].score, 0.85);
        assert!(results[0].attributes.is_some());
        // Flat `text` per OpenAI's Responses API schema (not a `content` array).
        assert_eq!(results[0].text, "Test content");

        // Lock the spec-shaped wire form: `text` is a flat string and there is
        // no `content` array.
        let wire = serde_json::to_value(&results[0]).unwrap();
        assert_eq!(wire["text"], "Test content");
        assert!(
            wire.get("content").is_none(),
            "no `content` array on results"
        );
    }

    #[test]
    fn test_rewrite_file_search_history_expands_to_function_pair() {
        // Continuation turn: a synthesized file_search_call comes back between two
        // user messages and must expand to a function_call + function_call_output
        // pair, replaying the retained chunk text — even with no tools re-declared
        // (the rewrite must run before the tools early-return).
        let mut payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "input": [
                {"role": "user", "content": "find the policy"},
                {"role": "user", "content": "and the appendix?"},
            ],
            "stream": false,
        }))
        .unwrap();
        let file_search_call = ResponsesInputItem::FileSearchCall(FileSearchCallOutput {
            type_: FileSearchCallOutputType::FileSearchCall,
            id: "fs_1".to_string(),
            queries: vec!["policy".to_string()],
            status: WebSearchStatus::Completed,
            results: None,
            replay_content: Some("Retrieved: the policy says...".to_string()),
        });
        let Some(ResponsesInput::Items(items)) = payload.input.as_mut() else {
            panic!("expected items input");
        };
        items.insert(1, file_search_call);
        assert_eq!(items.len(), 3);

        assert!(payload.tools.is_none());
        preprocess_file_search_tools(&mut payload);

        let Some(ResponsesInput::Items(items)) = payload.input else {
            panic!("expected items input");
        };
        // The file_search_call expands to a pair, so 2 user messages + 2 = 4.
        assert_eq!(items.len(), 4);
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, ResponsesInputItem::FileSearchCall(_))),
            "no file_search_call items should remain"
        );
        let ResponsesInputItem::FunctionCall(ref fc) = items[1] else {
            panic!("expected a function_call at index 1, got {:?}", items[1]);
        };
        assert_eq!(fc.name, "file_search");
        assert_eq!(fc.call_id, "fs_1");
        assert!(fc.arguments.contains("policy"));
        let ResponsesInputItem::FunctionCallOutput(ref out) = items[2] else {
            panic!(
                "expected a function_call_output at index 2, got {:?}",
                items[2]
            );
        };
        assert_eq!(out.call_id, "fs_1");
        assert_eq!(out.output, "Retrieved: the policy says...");
    }

    #[test]
    fn test_format_file_search_call_sse_event() {
        use crate::api_types::responses::{
            FileSearchCallOutput, FileSearchCallOutputType, WebSearchStatus,
        };

        let output = FileSearchCallOutput {
            type_: FileSearchCallOutputType::FileSearchCall,
            id: "fs_123".to_string(),
            queries: vec!["test query".to_string()],
            status: WebSearchStatus::Completed,
            results: None,
            replay_content: Some("formatted".to_string()),
        };

        let sse_event = format_file_search_call_sse_event(&output);
        let event_str = std::str::from_utf8(&sse_event).unwrap();

        // Check SSE format
        assert!(event_str.starts_with("data: "));
        assert!(event_str.ends_with("\n\n"));

        // Parse the JSON
        let data_part = event_str
            .strip_prefix("data: ")
            .unwrap()
            .strip_suffix("\n\n")
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(data_part).unwrap();

        assert_eq!(parsed["type"], "response.output_item.done");
        assert_eq!(parsed["item"]["id"], "fs_123");
        assert_eq!(parsed["item"]["type"], "file_search_call");
        assert_eq!(parsed["item"]["queries"][0], "test query");
        assert_eq!(parsed["item"]["status"], "completed");
    }

    #[test]
    fn test_citation_tracker_new() {
        let tracker = CitationTracker::new();
        assert!(tracker.is_empty());
        assert!(tracker.get(1).is_none());
    }

    #[test]
    fn test_citation_tracker_add_from_response() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id_1 = Uuid::new_v4();
        let file_id_2 = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: file_id_1,
                    chunk_index: 0,
                    content: "Content 1".to_string(),
                    score: 0.95,
                    filename: Some("report.pdf".to_string()),
                    metadata: None,
                },
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: file_id_2,
                    chunk_index: 0,
                    content: "Content 2".to_string(),
                    score: 0.85,
                    filename: None, // No filename, should use file_id
                    metadata: None,
                },
            ],
            query: "test query".to_string(),
            vector_stores_searched: 1,
        };

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&response);

        assert!(!tracker.is_empty());

        // Source 1 should have the filename
        let source_1 = tracker.get(1).unwrap();
        assert_eq!(source_1.file_id, file_id_1);
        assert_eq!(source_1.filename, "report.pdf");

        // Source 2 should use file_id as filename
        let source_2 = tracker.get(2).unwrap();
        assert_eq!(source_2.file_id, file_id_2);
        assert_eq!(source_2.filename, file_id_2.to_string());

        // Source 3 doesn't exist
        assert!(tracker.get(3).is_none());
    }

    #[test]
    fn test_citation_tracker_parse_citations() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Content".to_string(),
                score: 0.95,
                filename: Some("report.pdf".to_string()),
                metadata: None,
            }],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&response);

        // Test basic citation parsing
        let text = "According to [Source 1], the revenue increased.";
        let annotations = tracker.parse_citations(text);

        assert_eq!(annotations.len(), 1);
        if let ResponsesAnnotation::FileCitation {
            file_id: fid,
            filename,
            index,
        } = &annotations[0]
        {
            assert_eq!(fid, &file_id.to_string());
            assert_eq!(filename, "report.pdf");
            // Index should be position of "[Source 1]" in the text
            assert_eq!(*index as usize, text.find("[Source 1]").unwrap());
        } else {
            panic!("Expected FileCitation annotation");
        }
    }

    #[test]
    fn test_citation_tracker_parse_citations_multiple() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id_1 = Uuid::new_v4();
        let file_id_2 = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: file_id_1,
                    chunk_index: 0,
                    content: "Content 1".to_string(),
                    score: 0.95,
                    filename: Some("doc1.pdf".to_string()),
                    metadata: None,
                },
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: file_id_2,
                    chunk_index: 0,
                    content: "Content 2".to_string(),
                    score: 0.85,
                    filename: Some("doc2.pdf".to_string()),
                    metadata: None,
                },
            ],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&response);

        // Test multiple citations
        let text = "First point [Source 1], second point [Source 2], and back to [Source 1].";
        let annotations = tracker.parse_citations(text);

        // Should have 3 annotations (Source 1 appears twice, Source 2 once)
        assert_eq!(annotations.len(), 3);

        // Annotations should be sorted by index
        let indices: Vec<u64> = annotations
            .iter()
            .map(|a| match a {
                ResponsesAnnotation::FileCitation { index, .. } => *index,
                _ => 0,
            })
            .collect();
        assert!(indices[0] < indices[1]);
        assert!(indices[1] < indices[2]);
    }

    #[test]
    fn test_citation_tracker_parse_citations_case_insensitive() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Content".to_string(),
                score: 0.95,
                filename: Some("report.pdf".to_string()),
                metadata: None,
            }],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&response);

        // Test case-insensitive matching
        let text = "[source 1] and [SOURCE 1] and [Source1]";
        let annotations = tracker.parse_citations(text);

        // Should match all three variations
        assert_eq!(annotations.len(), 3);
    }

    #[test]
    fn test_citation_tracker_parse_citations_unknown_source() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Content".to_string(),
                score: 0.95,
                filename: Some("report.pdf".to_string()),
                metadata: None,
            }],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&response);

        // Reference to unknown source should not produce annotation
        let text = "See [Source 1] and [Source 99].";
        let annotations = tracker.parse_citations(text);

        // Should only have 1 annotation (Source 99 is unknown)
        assert_eq!(annotations.len(), 1);
    }

    #[test]
    fn test_inject_citation_annotations_empty_tracker() {
        let tracker = CitationTracker::new();
        let chunk = b"data: {\"type\": \"response.content_part.done\"}\n\n";

        let result = inject_citation_annotations(chunk, &tracker);

        // Should return the chunk unchanged
        assert_eq!(result.as_ref(), chunk);
    }

    #[test]
    fn test_inject_citation_annotations_with_citations() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Content".to_string(),
                score: 0.95,
                filename: Some("report.pdf".to_string()),
                metadata: None,
            }],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&response);

        // Create an SSE event with a content_part.done containing a citation marker
        let event_json = serde_json::json!({
            "type": "response.content_part.done",
            "item_id": "msg_123",
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "According to [Source 1], the data shows growth.",
                "annotations": []
            }
        });
        let chunk = format!("data: {}\n\n", event_json);

        let result = inject_citation_annotations(chunk.as_bytes(), &tracker);
        let result_str = std::str::from_utf8(&result).unwrap();

        // Parse the result
        let data_part = result_str
            .strip_prefix("data: ")
            .unwrap()
            .strip_suffix("\n\n")
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(data_part).unwrap();

        // Check annotations were added
        let annotations = &parsed["part"]["annotations"];
        assert!(annotations.is_array());
        assert_eq!(annotations.as_array().unwrap().len(), 1);

        let annotation = &annotations[0];
        assert_eq!(annotation["type"], "file_citation");
        assert_eq!(annotation["file_id"], file_id.to_string());
        assert_eq!(annotation["filename"], "report.pdf");
    }

    #[test]
    fn test_inject_citation_annotations_passthrough_other_events() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let file_id = Uuid::new_v4();
        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id,
                chunk_index: 0,
                content: "Content".to_string(),
                score: 0.95,
                filename: Some("report.pdf".to_string()),
                metadata: None,
            }],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&response);

        // Events that aren't content_part.done should pass through unchanged
        let chunk = "data: {\"type\": \"response.output_text.delta\", \"delta\": \"Hello\"}\n\n";

        let result = inject_citation_annotations(chunk.as_bytes(), &tracker);
        let result_str = std::str::from_utf8(&result).unwrap();

        // Parse both and compare
        let original_data: serde_json::Value =
            serde_json::from_str(chunk.strip_prefix("data: ").unwrap().trim()).unwrap();
        let result_data: serde_json::Value = serde_json::from_str(
            result_str
                .strip_prefix("data: ")
                .unwrap()
                .strip_suffix("\n\n")
                .unwrap(),
        )
        .unwrap();

        assert_eq!(original_data, result_data);
    }

    #[test]
    fn test_inject_citation_annotations_done_message() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let mut tracker = CitationTracker::new();
        tracker.add_from_response(&FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id: Uuid::new_v4(),
                chunk_index: 0,
                content: "Content".to_string(),
                score: 0.95,
                filename: Some("report.pdf".to_string()),
                metadata: None,
            }],
            query: "test".to_string(),
            vector_stores_searched: 1,
        });

        let chunk = "data: [DONE]\n\n";
        let result = inject_citation_annotations(chunk.as_bytes(), &tracker);
        let result_str = std::str::from_utf8(&result).unwrap();

        assert_eq!(result_str, chunk);
    }

    // =========================================================================
    // FileSearchToolArguments Schema Tests
    // =========================================================================

    #[test]
    fn test_function_parameters_schema_structure() {
        let schema = FileSearchToolArguments::function_parameters_schema();

        // Check it's an object type
        assert_eq!(schema["type"], "object");

        // Check required properties
        let properties = &schema["properties"];
        assert!(properties.get("query").is_some());
        assert!(properties.get("max_num_results").is_some());
        assert!(properties.get("score_threshold").is_some());
        assert!(properties.get("filters").is_some());

        // Check query is required
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "query"));

        // Check additionalProperties is false
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn test_function_parameters_schema_query_field() {
        let schema = FileSearchToolArguments::function_parameters_schema();
        let query = &schema["properties"]["query"];

        assert_eq!(query["type"], "string");
        assert!(query.get("description").is_some());
    }

    #[test]
    fn test_function_parameters_schema_max_num_results_field() {
        let schema = FileSearchToolArguments::function_parameters_schema();
        let max_num_results = &schema["properties"]["max_num_results"];

        assert_eq!(max_num_results["type"], "integer");
        assert_eq!(max_num_results["minimum"], 1);
        assert_eq!(max_num_results["maximum"], 50);
    }

    #[test]
    fn test_function_parameters_schema_score_threshold_field() {
        let schema = FileSearchToolArguments::function_parameters_schema();
        let score_threshold = &schema["properties"]["score_threshold"];

        assert_eq!(score_threshold["type"], "number");
        assert_eq!(score_threshold["minimum"], 0.0);
        assert_eq!(score_threshold["maximum"], 1.0);
    }

    #[test]
    fn test_function_tool_definition_structure() {
        let def = FileSearchToolArguments::function_tool_definition();

        assert_eq!(def["type"], "function");
        assert_eq!(def["name"], "file_search");
        assert!(def.get("description").is_some());
        assert!(def.get("parameters").is_some());

        // Parameters should match the function_parameters_schema
        let params = &def["parameters"];
        assert_eq!(params["type"], "object");
        assert!(params["properties"].get("query").is_some());
    }

    #[test]
    fn test_function_name_constant() {
        assert_eq!(FileSearchToolArguments::FUNCTION_NAME, "file_search");
    }

    #[test]
    fn test_function_description_not_empty() {
        let desc = FileSearchToolArguments::function_description();
        assert!(!desc.is_empty());
        assert!(desc.contains("knowledge base"));
    }

    // =========================================================================
    // Preprocessing Tests
    // =========================================================================

    #[test]
    fn test_preprocess_file_search_tools_empty() {
        use crate::api_types::responses::CreateResponsesPayload;

        let mut payload = CreateResponsesPayload {
            input: None,
            instructions: None,
            metadata: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            model: None,
            models: None,
            text: None,
            reasoning: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            prompt_cache_key: None,
            previous_response_id: None,
            prompt: None,
            include: None,
            background: None,
            safety_identifier: None,
            store: None,
            service_tier: None,
            truncation: None,
            presence_penalty: None,
            frequency_penalty: None,
            stream: false,
            provider: None,
            plugins: None,
            user: None,
            sovereignty_requirements: None,
            skills: None,
            context_management: None,
        };

        // Should not panic with no tools
        preprocess_file_search_tools(&mut payload);
        assert!(payload.tools.is_none());
    }

    #[test]
    fn test_preprocess_file_search_tools_converts_file_search() {
        use crate::api_types::responses::{
            CreateResponsesPayload, FileSearchTool, FileSearchToolType, ResponsesToolDefinition,
        };

        let mut payload = CreateResponsesPayload {
            input: None,
            instructions: None,
            metadata: None,
            tools: Some(vec![ResponsesToolDefinition::FileSearch(FileSearchTool {
                type_: FileSearchToolType::FileSearch,
                vector_store_ids: vec!["vs_123".to_string()],
                max_num_results: None,
                ranking_options: None,
                filters: None,
                cache_control: None,
            })]),
            tool_choice: None,
            parallel_tool_calls: None,
            model: None,
            models: None,
            text: None,
            reasoning: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            prompt_cache_key: None,
            previous_response_id: None,
            prompt: None,
            include: None,
            background: None,
            safety_identifier: None,
            store: None,
            service_tier: None,
            truncation: None,
            presence_penalty: None,
            frequency_penalty: None,
            stream: false,
            provider: None,
            plugins: None,
            user: None,
            sovereignty_requirements: None,
            skills: None,
            context_management: None,
        };

        preprocess_file_search_tools(&mut payload);

        // Should have converted to a function tool
        let tools = payload.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 1);

        match &tools[0] {
            ResponsesToolDefinition::Function(f) => {
                assert_eq!(f.name, "file_search");
                assert!(f.parameters.is_some());
            }
            _ => panic!("Expected Function tool, got {:?}", tools[0]),
        }
    }

    #[test]
    fn test_preprocess_file_search_tools_preserves_other_tools() {
        use crate::api_types::responses::{
            CreateResponsesPayload, FileSearchTool, FileSearchToolType, FunctionTool,
            ResponsesToolDefinition,
        };

        let function_tool = FunctionTool::from_json(serde_json::json!({
            "type": "function",
            "name": "get_weather",
            "description": "Get weather for a location",
            "parameters": {
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                }
            }
        }))
        .unwrap();

        let mut payload = CreateResponsesPayload {
            input: None,
            instructions: None,
            metadata: None,
            tools: Some(vec![
                ResponsesToolDefinition::Function(function_tool.clone()),
                ResponsesToolDefinition::FileSearch(FileSearchTool {
                    type_: FileSearchToolType::FileSearch,
                    vector_store_ids: vec!["vs_123".to_string()],
                    max_num_results: None,
                    ranking_options: None,
                    filters: None,
                    cache_control: None,
                }),
            ]),
            tool_choice: None,
            parallel_tool_calls: None,
            model: None,
            models: None,
            text: None,
            reasoning: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            prompt_cache_key: None,
            previous_response_id: None,
            prompt: None,
            include: None,
            background: None,
            safety_identifier: None,
            store: None,
            service_tier: None,
            truncation: None,
            presence_penalty: None,
            frequency_penalty: None,
            stream: false,
            provider: None,
            plugins: None,
            user: None,
            sovereignty_requirements: None,
            skills: None,
            context_management: None,
        };

        preprocess_file_search_tools(&mut payload);

        let tools = payload.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 2);

        // First tool should be unchanged
        match &tools[0] {
            ResponsesToolDefinition::Function(f) => {
                assert_eq!(f.name, "get_weather");
            }
            _ => panic!("Expected Function tool"),
        }

        // Second tool should be converted from file_search
        match &tools[1] {
            ResponsesToolDefinition::Function(f) => {
                assert_eq!(f.name, "file_search");
            }
            _ => panic!("Expected converted file_search tool"),
        }
    }

    // =========================================================================
    // Filter Conversion Tests
    // =========================================================================

    #[test]
    fn test_convert_comparison_filter_eq() {
        use crate::{
            api_types::responses::{
                FileSearchComparisonFilter, FileSearchFilter, FileSearchFilterComparison,
            },
            models::{AttributeFilter, ComparisonOperator, FilterValue},
        };

        let api_filter = FileSearchFilter::Comparison(FileSearchComparisonFilter {
            type_: FileSearchFilterComparison::Eq,
            key: "author".to_string(),
            value: serde_json::json!("John Doe"),
        });

        let result = convert_file_search_filter(&api_filter);

        match result {
            AttributeFilter::Comparison(c) => {
                assert_eq!(c.operator, ComparisonOperator::Eq);
                assert_eq!(c.key, "author");
                assert_eq!(c.value, FilterValue::String("John Doe".to_string()));
            }
            _ => panic!("Expected Comparison variant"),
        }
    }

    #[test]
    fn test_convert_comparison_filter_all_operators() {
        use crate::{
            api_types::responses::{
                FileSearchComparisonFilter, FileSearchFilter, FileSearchFilterComparison,
            },
            models::{AttributeFilter, ComparisonOperator},
        };

        let operators = [
            (FileSearchFilterComparison::Eq, ComparisonOperator::Eq),
            (FileSearchFilterComparison::Ne, ComparisonOperator::Ne),
            (FileSearchFilterComparison::Gt, ComparisonOperator::Gt),
            (FileSearchFilterComparison::Gte, ComparisonOperator::Gte),
            (FileSearchFilterComparison::Lt, ComparisonOperator::Lt),
            (FileSearchFilterComparison::Lte, ComparisonOperator::Lte),
        ];

        for (api_op, expected_op) in operators {
            let api_filter = FileSearchFilter::Comparison(FileSearchComparisonFilter {
                type_: api_op,
                key: "test".to_string(),
                value: serde_json::json!(42),
            });

            let result = convert_file_search_filter(&api_filter);

            match result {
                AttributeFilter::Comparison(c) => {
                    assert_eq!(
                        c.operator, expected_op,
                        "Operator mismatch for {:?}",
                        api_op
                    );
                }
                _ => panic!("Expected Comparison variant"),
            }
        }
    }

    #[test]
    fn test_convert_comparison_filter_number_value() {
        use crate::{
            api_types::responses::{
                FileSearchComparisonFilter, FileSearchFilter, FileSearchFilterComparison,
            },
            models::{AttributeFilter, FilterValue},
        };

        let api_filter = FileSearchFilter::Comparison(FileSearchComparisonFilter {
            type_: FileSearchFilterComparison::Gte,
            key: "score".to_string(),
            value: serde_json::json!(0.75),
        });

        let result = convert_file_search_filter(&api_filter);

        match result {
            AttributeFilter::Comparison(c) => {
                assert_eq!(c.value, FilterValue::Number(0.75));
            }
            _ => panic!("Expected Comparison variant"),
        }
    }

    #[test]
    fn test_convert_comparison_filter_boolean_value() {
        use crate::{
            api_types::responses::{
                FileSearchComparisonFilter, FileSearchFilter, FileSearchFilterComparison,
            },
            models::{AttributeFilter, FilterValue},
        };

        let api_filter = FileSearchFilter::Comparison(FileSearchComparisonFilter {
            type_: FileSearchFilterComparison::Eq,
            key: "is_published".to_string(),
            value: serde_json::json!(true),
        });

        let result = convert_file_search_filter(&api_filter);

        match result {
            AttributeFilter::Comparison(c) => {
                assert_eq!(c.value, FilterValue::Boolean(true));
            }
            _ => panic!("Expected Comparison variant"),
        }
    }

    #[test]
    fn test_convert_compound_filter_and() {
        use crate::{
            api_types::responses::{
                FileSearchComparisonFilter, FileSearchCompoundFilter, FileSearchFilter,
                FileSearchFilterComparison, FileSearchFilterLogicalType,
            },
            models::{AttributeFilter, LogicalOperator},
        };

        let api_filter = FileSearchFilter::Compound(FileSearchCompoundFilter {
            type_: FileSearchFilterLogicalType::And,
            filters: vec![
                FileSearchFilter::Comparison(FileSearchComparisonFilter {
                    type_: FileSearchFilterComparison::Eq,
                    key: "author".to_string(),
                    value: serde_json::json!("Alice"),
                }),
                FileSearchFilter::Comparison(FileSearchComparisonFilter {
                    type_: FileSearchFilterComparison::Gte,
                    key: "year".to_string(),
                    value: serde_json::json!(2024),
                }),
            ],
        });

        let result = convert_file_search_filter(&api_filter);

        match result {
            AttributeFilter::Compound(c) => {
                assert_eq!(c.operator, LogicalOperator::And);
                assert_eq!(c.filters.len(), 2);
            }
            _ => panic!("Expected Compound variant"),
        }
    }

    #[test]
    fn test_convert_compound_filter_or() {
        use crate::{
            api_types::responses::{
                FileSearchComparisonFilter, FileSearchCompoundFilter, FileSearchFilter,
                FileSearchFilterComparison, FileSearchFilterLogicalType,
            },
            models::{AttributeFilter, LogicalOperator},
        };

        let api_filter = FileSearchFilter::Compound(FileSearchCompoundFilter {
            type_: FileSearchFilterLogicalType::Or,
            filters: vec![
                FileSearchFilter::Comparison(FileSearchComparisonFilter {
                    type_: FileSearchFilterComparison::Eq,
                    key: "category".to_string(),
                    value: serde_json::json!("docs"),
                }),
                FileSearchFilter::Comparison(FileSearchComparisonFilter {
                    type_: FileSearchFilterComparison::Eq,
                    key: "category".to_string(),
                    value: serde_json::json!("guides"),
                }),
            ],
        });

        let result = convert_file_search_filter(&api_filter);

        match result {
            AttributeFilter::Compound(c) => {
                assert_eq!(c.operator, LogicalOperator::Or);
                assert_eq!(c.filters.len(), 2);
            }
            _ => panic!("Expected Compound variant"),
        }
    }

    #[test]
    fn test_convert_nested_compound_filter() {
        use crate::{
            api_types::responses::{
                FileSearchComparisonFilter, FileSearchCompoundFilter, FileSearchFilter,
                FileSearchFilterComparison, FileSearchFilterLogicalType,
            },
            models::{AttributeFilter, LogicalOperator},
        };

        // Build: (category == "docs") AND ((author == "Alice") OR (author == "Bob"))
        let api_filter = FileSearchFilter::Compound(FileSearchCompoundFilter {
            type_: FileSearchFilterLogicalType::And,
            filters: vec![
                FileSearchFilter::Comparison(FileSearchComparisonFilter {
                    type_: FileSearchFilterComparison::Eq,
                    key: "category".to_string(),
                    value: serde_json::json!("docs"),
                }),
                FileSearchFilter::Compound(FileSearchCompoundFilter {
                    type_: FileSearchFilterLogicalType::Or,
                    filters: vec![
                        FileSearchFilter::Comparison(FileSearchComparisonFilter {
                            type_: FileSearchFilterComparison::Eq,
                            key: "author".to_string(),
                            value: serde_json::json!("Alice"),
                        }),
                        FileSearchFilter::Comparison(FileSearchComparisonFilter {
                            type_: FileSearchFilterComparison::Eq,
                            key: "author".to_string(),
                            value: serde_json::json!("Bob"),
                        }),
                    ],
                }),
            ],
        });

        let result = convert_file_search_filter(&api_filter);

        match result {
            AttributeFilter::Compound(outer) => {
                assert_eq!(outer.operator, LogicalOperator::And);
                assert_eq!(outer.filters.len(), 2);

                // Second filter should be the nested OR
                match &outer.filters[1] {
                    AttributeFilter::Compound(inner) => {
                        assert_eq!(inner.operator, LogicalOperator::Or);
                        assert_eq!(inner.filters.len(), 2);
                    }
                    _ => panic!("Expected nested Compound variant"),
                }
            }
            _ => panic!("Expected Compound variant"),
        }
    }

    #[test]
    fn test_convert_json_value_to_filter_value_array() {
        use crate::models::{FilterValue, FilterValueItem};

        let json_array = serde_json::json!(["tag1", "tag2", 123]);
        let result = convert_json_value_to_filter_value(&json_array);

        match result {
            FilterValue::Array(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], FilterValueItem::String("tag1".to_string()));
                assert_eq!(items[1], FilterValueItem::String("tag2".to_string()));
                assert_eq!(items[2], FilterValueItem::Number(123.0));
            }
            _ => panic!("Expected Array variant"),
        }
    }

    #[test]
    fn test_convert_json_value_to_filter_value_null_fallback() {
        use crate::models::FilterValue;

        let json_null = serde_json::json!(null);
        let result = convert_json_value_to_filter_value(&json_null);

        // Null should fall back to string representation
        assert_eq!(result, FilterValue::String("null".to_string()));
    }

    #[test]
    fn test_convert_json_value_to_filter_value_object_fallback() {
        use crate::models::FilterValue;

        let json_obj = serde_json::json!({"nested": "object"});
        let result = convert_json_value_to_filter_value(&json_obj);

        // Objects should fall back to string representation
        match result {
            FilterValue::String(s) => {
                assert!(s.contains("nested"));
                assert!(s.contains("object"));
            }
            _ => panic!("Expected String variant for object fallback"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Query Deduplication Tests (cache_key)
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_cache_key_identical_queries() {
        let call1 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "What is the return policy?".to_string(),
            vector_store_ids: vec!["vs_123".to_string(), "vs_456".to_string()],
            max_num_results: Some(5),
            score_threshold: Some(0.7),
            filters: None,
            ranking_options: None,
        };

        let call2 = FileSearchToolCall {
            id: "call_2".to_string(), // Different ID
            query: "What is the return policy?".to_string(),
            vector_store_ids: vec!["vs_123".to_string(), "vs_456".to_string()],
            max_num_results: Some(5),
            score_threshold: Some(0.7),
            filters: None,
            ranking_options: None,
        };

        assert_eq!(call1.cache_key(), call2.cache_key());
    }

    #[test]
    fn test_cache_key_different_queries() {
        let call1 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "What is the return policy?".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        let call2 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "How do I contact support?".to_string(), // Different query
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        assert_ne!(call1.cache_key(), call2.cache_key());
    }

    #[test]
    fn test_cache_key_vector_store_order_independent() {
        // Cache keys should be identical regardless of vector_store_ids order
        let call1 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec![
                "vs_aaa".to_string(),
                "vs_bbb".to_string(),
                "vs_ccc".to_string(),
            ],
            max_num_results: None,
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        let call2 = FileSearchToolCall {
            id: "call_2".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec![
                "vs_ccc".to_string(),
                "vs_aaa".to_string(),
                "vs_bbb".to_string(),
            ], // Different order
            max_num_results: None,
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        assert_eq!(call1.cache_key(), call2.cache_key());
    }

    #[test]
    fn test_cache_key_different_max_results() {
        let call1 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: Some(5),
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        let call2 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: Some(10), // Different max_results
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        assert_ne!(call1.cache_key(), call2.cache_key());
    }

    #[test]
    fn test_cache_key_different_score_threshold() {
        let call1 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: Some(0.5),
            filters: None,
            ranking_options: None,
        };

        let call2 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: Some(0.8), // Different threshold
            filters: None,
            ranking_options: None,
        };

        assert_ne!(call1.cache_key(), call2.cache_key());
    }

    #[test]
    fn test_cache_key_with_filters() {
        use crate::api_types::responses::{FileSearchComparisonFilter, FileSearchFilterComparison};

        let filter1 = FileSearchFilter::Comparison(FileSearchComparisonFilter {
            type_: FileSearchFilterComparison::Eq,
            key: "category".to_string(),
            value: serde_json::json!("policy"),
        });

        let filter2 = FileSearchFilter::Comparison(FileSearchComparisonFilter {
            type_: FileSearchFilterComparison::Eq,
            key: "category".to_string(),
            value: serde_json::json!("faq"), // Different value
        });

        let call1 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: None,
            filters: Some(filter1.clone()),
            ranking_options: None,
        };

        let call2 = FileSearchToolCall {
            id: "call_2".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: None,
            filters: Some(filter1), // Same filter
            ranking_options: None,
        };

        let call3 = FileSearchToolCall {
            id: "call_3".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: None,
            filters: Some(filter2), // Different filter
            ranking_options: None,
        };

        // Same filters should produce same key
        assert_eq!(call1.cache_key(), call2.cache_key());
        // Different filters should produce different key
        assert_ne!(call1.cache_key(), call3.cache_key());
    }

    #[test]
    fn test_cache_key_none_vs_some_options() {
        let call1 = FileSearchToolCall {
            id: "call_1".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        let call2 = FileSearchToolCall {
            id: "call_2".to_string(),
            query: "search query".to_string(),
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: Some(10),
            score_threshold: None,
            filters: None,
            ranking_options: None,
        };

        // None vs Some should produce different keys
        assert_ne!(call1.cache_key(), call2.cache_key());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // SSE Buffer Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_sse_buffer_single_complete_event() {
        let mut buffer = SseBuffer::new();
        buffer.extend(b"data: {\"type\": \"test\"}\n\n");

        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref(), b"data: {\"type\": \"test\"}\n\n");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_sse_buffer_multiple_events_single_chunk() {
        let mut buffer = SseBuffer::new();
        buffer.extend(b"data: {\"id\": 1}\n\ndata: {\"id\": 2}\n\ndata: {\"id\": 3}\n\n");

        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].as_ref(), b"data: {\"id\": 1}\n\n");
        assert_eq!(events[1].as_ref(), b"data: {\"id\": 2}\n\n");
        assert_eq!(events[2].as_ref(), b"data: {\"id\": 3}\n\n");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_sse_buffer_partial_event_across_chunks() {
        let mut buffer = SseBuffer::new();

        // First chunk has partial event
        buffer.extend(b"data: {\"type\":");
        let events = buffer.extract_complete_events();
        assert!(events.is_empty()); // No complete events yet
        assert!(!buffer.is_empty()); // Buffer has partial data

        // Second chunk completes the event
        buffer.extend(b" \"test\"}\n\n");
        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref(), b"data: {\"type\": \"test\"}\n\n");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_sse_buffer_json_split_at_various_points() {
        // Test JSON split in the middle of a key
        let mut buffer = SseBuffer::new();
        buffer.extend(b"data: {\"func");
        let events = buffer.extract_complete_events();
        assert!(events.is_empty());

        buffer.extend(b"tion_call\": true}\n\n");
        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);

        // Test split at delimiter boundary
        let mut buffer = SseBuffer::new();
        buffer.extend(b"data: {\"done\": true}\n");
        let events = buffer.extract_complete_events();
        assert!(events.is_empty()); // Only one \n, need \n\n

        buffer.extend(b"\n");
        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_sse_buffer_windows_line_endings() {
        let mut buffer = SseBuffer::new();
        buffer.extend(b"data: {\"type\": \"test\"}\r\n\r\n");

        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref(), b"data: {\"type\": \"test\"}\r\n\r\n");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_sse_buffer_mixed_complete_and_partial() {
        let mut buffer = SseBuffer::new();
        buffer.extend(b"data: {\"id\": 1}\n\ndata: {\"id\":");

        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref(), b"data: {\"id\": 1}\n\n");
        assert!(!buffer.is_empty()); // Partial event remains

        buffer.extend(b" 2}\n\n");
        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref(), b"data: {\"id\": 2}\n\n");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_sse_buffer_take_remaining() {
        let mut buffer = SseBuffer::new();
        buffer.extend(b"data: partial");

        let events = buffer.extract_complete_events();
        assert!(events.is_empty());

        let remaining = buffer.take_remaining();
        assert_eq!(remaining.as_ref(), b"data: partial");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_sse_buffer_empty() {
        let mut buffer = SseBuffer::new();
        assert!(buffer.is_empty());

        let events = buffer.extract_complete_events();
        assert!(events.is_empty());

        let remaining = buffer.take_remaining();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_sse_buffer_tool_call_split_across_chunks() {
        // Simulate a realistic scenario where a file_search tool call is split
        let mut buffer = SseBuffer::new();

        // First chunk: partial JSON
        buffer.extend(
            b"data: {\"type\": \"function_call\", \"name\": \"file_search\", \"call_id\": \"call_",
        );
        let events = buffer.extract_complete_events();
        assert!(events.is_empty());

        // Second chunk: rest of JSON
        buffer.extend(b"abc\", \"arguments\": \"{\\\"query\\\": \\\"test\\\"}\"}\n\n");
        let events = buffer.extract_complete_events();
        assert_eq!(events.len(), 1);

        // Now verify the complete event can be parsed
        let event_str = std::str::from_utf8(&events[0]).unwrap();
        assert!(event_str.contains("file_search"));
        assert!(event_str.contains("call_abc"));
    }

    #[test]
    fn test_format_search_results_truncated_no_limit() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let response = FileSearchResponse {
            results: vec![
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "A".repeat(1000),
                    score: 0.95,
                    filename: Some("file1.txt".to_string()),
                    metadata: None,
                },
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "B".repeat(1000),
                    score: 0.85,
                    filename: Some("file2.txt".to_string()),
                    metadata: None,
                },
            ],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        // With usize::MAX (no limit), all results should be included
        let formatted = format_search_results_truncated(&response, usize::MAX);
        assert!(formatted.contains("[Source 1: file1.txt"));
        assert!(formatted.contains("[Source 2: file2.txt"));
        assert!(!formatted.contains("truncated"));
    }

    #[test]
    fn test_format_search_results_truncated_with_limit() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let response = FileSearchResponse {
            results: vec![
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "A".repeat(500),
                    score: 0.95,
                    filename: Some("file1.txt".to_string()),
                    metadata: None,
                },
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "B".repeat(500),
                    score: 0.85,
                    filename: Some("file2.txt".to_string()),
                    metadata: None,
                },
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "C".repeat(500),
                    score: 0.75,
                    filename: Some("file3.txt".to_string()),
                    metadata: None,
                },
            ],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        // Set a limit that allows only 2 results
        // Each result is ~600 chars (500 content + ~100 header/formatting)
        // So 1500 chars should fit 2 results but not 3
        let formatted = format_search_results_truncated(&response, 1500);

        assert!(formatted.contains("[Source 1: file1.txt"));
        assert!(formatted.contains("[Source 2: file2.txt"));
        assert!(!formatted.contains("[Source 3: file3.txt"));
        assert!(formatted.contains("truncated to prevent context overflow"));
    }

    #[test]
    fn test_format_search_results_truncated_first_result_too_large() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let response = FileSearchResponse {
            results: vec![FileSearchResult {
                chunk_id: Uuid::new_v4(),
                vector_store_id: Uuid::new_v4(),
                file_id: Uuid::new_v4(),
                chunk_index: 0,
                content: "A".repeat(10000),
                score: 0.95,
                filename: Some("huge_file.txt".to_string()),
                metadata: None,
            }],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        // Limit smaller than the first result - should get empty results with truncation notice
        let formatted = format_search_results_truncated(&response, 500);

        assert!(formatted.contains("test")); // Query is in header
        assert!(formatted.contains("1 results")); // Header says 1 result
        assert!(!formatted.contains("[Source 1")); // But result not included
        assert!(formatted.contains("truncated to prevent context overflow"));
    }

    #[test]
    fn test_format_search_results_truncated_zero_means_unlimited() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let response = FileSearchResponse {
            results: vec![
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "A".repeat(1000),
                    score: 0.95,
                    filename: Some("file1.txt".to_string()),
                    metadata: None,
                },
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "B".repeat(1000),
                    score: 0.85,
                    filename: Some("file2.txt".to_string()),
                    metadata: None,
                },
            ],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        // max_chars = 0 should be treated as unlimited
        let formatted = format_search_results_truncated(&response, 0);
        assert!(formatted.contains("[Source 1: file1.txt"));
        assert!(formatted.contains("[Source 2: file2.txt"));
        assert!(!formatted.contains("truncated"));
    }

    #[test]
    fn test_format_search_results_truncated_preserves_complete_results() {
        use crate::services::{FileSearchResponse, FileSearchResult};

        let response = FileSearchResponse {
            results: vec![
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "Short content".to_string(),
                    score: 0.95,
                    filename: Some("small.txt".to_string()),
                    metadata: None,
                },
                FileSearchResult {
                    chunk_id: Uuid::new_v4(),
                    vector_store_id: Uuid::new_v4(),
                    file_id: Uuid::new_v4(),
                    chunk_index: 0,
                    content: "X".repeat(5000),
                    score: 0.85,
                    filename: Some("large.txt".to_string()),
                    metadata: None,
                },
            ],
            query: "test".to_string(),
            vector_stores_searched: 1,
        };

        // Set limit that fits first result but not second
        let formatted = format_search_results_truncated(&response, 500);

        // First result should be completely included
        assert!(formatted.contains("[Source 1: small.txt"));
        assert!(formatted.contains("Short content"));
        assert!(formatted.contains("95.0%"));

        // Second result should be completely excluded (not partially included)
        assert!(!formatted.contains("large.txt"));
        assert!(!formatted.contains("XXXXX")); // No partial content

        // Truncation notice present
        assert!(formatted.contains("truncated to prevent context overflow"));
    }
}
