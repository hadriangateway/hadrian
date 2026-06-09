//! Anthropic API types.
//!
//! Request and response types for the Anthropic Messages API.

use serde::{Deserialize, Serialize};

// ============================================================================
// Request Types
// ============================================================================

/// Cache control for Anthropic prompt caching.
///
/// When present on content blocks or tools, signals that content up to this
/// point should be cached for future requests. Anthropic currently only
/// supports ephemeral caching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicCacheControl {
    #[serde(rename = "type")]
    pub type_: AnthropicCacheControlType,
}

/// Cache control type for Anthropic.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicCacheControlType {
    /// Ephemeral cache - cached content may be evicted at any time
    Ephemeral,
}

#[derive(Debug, Serialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,
    /// Extended thinking configuration for Claude 4+ models
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinkingConfig>,
    /// Output configuration (effort level for adaptive thinking)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<AnthropicOutputConfig>,
    /// Metadata for tracking (user_id for abuse detection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AnthropicMetadata>,
}

/// Anthropic metadata for tracking
#[derive(Debug, Serialize)]
pub struct AnthropicMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Anthropic extended thinking configuration
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicThinkingConfig {
    Enabled {
        budget_tokens: u32,
    },
    Disabled,
    /// Adaptive thinking — model decides how much to reason (Opus 4.6+).
    ///
    /// `display` controls whether thinking summaries are returned. On Opus
    /// 4.7/4.8 the default is omitted (empty thinking text), so we send
    /// `summarized` for those models to keep surfacing reasoning. The field is
    /// rejected by models that predate it, so it stays `None` on 4.6.
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<AnthropicThinkingDisplay>,
    },
}

/// Controls whether adaptive-thinking summaries are returned (Opus 4.7+).
///
/// Only `Summarized` is ever emitted — a `None` on the `display` field already
/// expresses the upstream default of omitting summaries, so there is no need for
/// an explicit `omitted` value.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicThinkingDisplay {
    Summarized,
}

/// Anthropic effort level for adaptive thinking.
///
/// `XHigh` is Opus 4.7+ only; `Max` is Opus-tier only (Opus 4.6+). The convert
/// layer clamps these down for models that don't support them.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// Output configuration for Anthropic requests (used with adaptive thinking).
#[derive(Debug, Serialize)]
pub struct AnthropicOutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<AnthropicEffort>,
    /// Task budget for an agentic loop (Opus 4.7/4.8, beta
    /// `task-budgets-2026-03-13`). Distinct from `max_tokens`: the model is
    /// told its budget and self-moderates. Minimum 20,000 tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_budget: Option<AnthropicTaskBudget>,
}

/// Task budget for an agentic loop. Serializes as
/// `{"type": "tokens", "total": N}`.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnthropicTaskBudget {
    Tokens { total: u32 },
}

/// Anthropic tool definition
#[derive(Debug, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    /// Cache control for prompt caching
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<AnthropicCacheControl>,
}

/// Anthropic tool choice
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    Image {
        source: ImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    /// Extended thinking block (Claude 4+ models)
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

/// Image source for Anthropic's Messages API.
///
/// Anthropic supports two image source types:
/// - `base64`: Inline base64-encoded image data with media type
/// - `url`: Direct HTTPS URL reference to an image
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

// ============================================================================
// Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct AnthropicResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
pub struct AnthropicUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Tokens read from the prompt cache (cache hit)
    #[serde(default)]
    pub cache_read_input_tokens: i64,
    /// Tokens written to the prompt cache (cache miss, will be cached).
    #[serde(default)]
    pub cache_creation_input_tokens: i64,
}

// ============================================================================
// OpenAI Response Types (for format conversion)
// ============================================================================

#[derive(Debug, Serialize)]
pub struct OpenAIResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    pub usage: Option<OpenAIUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenAIChoice {
    pub index: i32,
    pub message: OpenAIMessage,
    pub finish_reason: Option<String>,
    pub logprobs: Option<()>,
}

#[derive(Debug, Serialize)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: Option<String>,
    /// The refusal message generated by the model (required per OpenAI schema, null if not a refusal)
    pub refusal: Option<String>,
    /// Reasoning/thinking content from the model (when thinking is enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenAIToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub function: OpenAIToolCallFunction,
}

#[derive(Debug, Serialize)]
pub struct OpenAIToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// Breakdown of prompt tokens (OpenAI-compatible)
#[derive(Debug, Serialize)]
pub struct PromptTokensDetails {
    /// Cached tokens read from prompt cache
    pub cached_tokens: i64,
    /// Tokens written to the prompt cache (cache miss, will be cached)
    #[serde(skip_serializing_if = "crate::providers::anthropic::types::is_zero")]
    pub cache_creation_input_tokens: i64,
}

pub(crate) fn is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Serialize)]
pub struct OpenAIUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Breakdown of prompt tokens including cache information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}
