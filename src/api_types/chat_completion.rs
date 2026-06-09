use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use validator::Validate;

/// Cache control type for prompt caching
///
/// **Hadrian Extension:** This field is not part of the OpenAI API specification.
/// It is a Hadrian-specific extension to support Anthropic and Bedrock prompt caching.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum CacheControlType {
    /// Ephemeral cache - cached content may be evicted at any time
    Ephemeral,
}

/// Cache control configuration for prompt caching
///
/// **Hadrian Extension:** This field is not part of the OpenAI API specification.
/// It is a Hadrian-specific extension to support Anthropic and Bedrock prompt caching.
///
/// When present on content blocks or tools, it signals that the content up to this
/// point should be cached for future requests:
/// - **Anthropic:** Passed through as `cache_control: { type: "ephemeral" }`
/// - **Bedrock:** Transformed into `cachePoint` blocks after the content
/// - **OpenAI/Azure:** Ignored (these providers use automatic caching)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CacheControl {
    /// The type of cache control
    #[serde(rename = "type")]
    pub type_: CacheControlType,
}

/// Reasoning effort level
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    /// Between `High` and `Max` (Claude Opus 4.7+ / GPT-5.x). Providers that
    /// don't support it clamp down to `High`.
    XHigh,
    /// Maximum effort (Claude Opus-tier). Providers that don't support it clamp
    /// down to `High`.
    Max,
}

/// Reasoning summary format
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummary {
    Auto,
    Concise,
    Detailed,
}

/// Reasoning configuration for chat completion
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateChatCompletionReasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
}

/// Response format for chat completion
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { json_schema: JsonSchemaConfig },
    Grammar { grammar: String },
    Python,
}

/// JSON schema configuration for structured output
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct JsonSchemaConfig {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Stop sequence(s) for generation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum Stop {
    Single(String),
    Multiple(Vec<String>),
}

/// Stream options
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// Default tool choice options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceDefaults {
    None,
    Auto,
    Required,
}

/// Tool choice configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum ToolChoice {
    String(ToolChoiceDefaults),
    Named(NamedToolChoice),
}

/// Named tool choice
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct NamedToolChoice {
    #[serde(rename = "type")]
    pub type_: ToolType,
    pub function: NamedToolChoiceFunction,
}

/// Named tool choice function reference
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct NamedToolChoiceFunction {
    pub name: String,
}

/// Tool type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    Function,
}

/// Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub type_: ToolType,
    pub function: ToolDefinitionFunction,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Tool function definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ToolDefinitionFunction {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for function parameters
    #[serde(default)]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub parameters: Option<serde_json::Value>,
    pub strict: Option<bool>,
}

/// Message content (text or multimodal parts)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// Content part for multimodal messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ImageUrl {
        image_url: ImageUrl,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    InputAudio {
        input_audio: InputAudio,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    InputVideo {
        video_url: VideoUrl,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    VideoUrl {
        video_url: VideoUrl,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

/// Image detail level
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ImageUrlDetail {
    Auto,
    Low,
    High,
}

/// Image URL reference
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageUrlDetail>,
}

/// Video URL reference
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct VideoUrl {
    pub url: String,
}

/// Audio input format
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum InputAudioFormat {
    Wav,
    Mp3,
    Flac,
    M4a,
    Ogg,
    Pcm16,
    Pcm24,
}

/// Audio input
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct InputAudio {
    /// Base64-encoded audio data
    pub data: String,
    pub format: InputAudioFormat,
}

/// Chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: MessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        content: MessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        #[serde(default)]
        content: Option<MessageContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    Tool {
        content: MessageContent,
        tool_call_id: String,
    },
    Developer {
        content: MessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

/// Tool call made by the model
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: ToolType,
    pub function: ToolCallFunction,
}

/// Tool call function details
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ToolCallFunction {
    pub name: String,
    /// JSON-encoded arguments
    pub arguments: String,
}

/// Create chat completion request (OpenAI-compatible)
#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateChatCompletionPayload {
    /// Conversation messages
    #[validate(length(min = 1))]
    pub messages: Vec<Message>,

    /// Model to use for completion
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// **Hadrian Extension:** List of models for multi-model routing (alternative to single model)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,

    /// Penalize repeated tokens (-2.0 to 2.0)
    #[validate(range(min = -2.0, max = 2.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,

    /// Token bias map
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub logit_bias: Option<HashMap<String, f64>>,

    /// Return log probabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<bool>,

    /// Number of top log probabilities to return (0-20)
    #[validate(range(min = 0, max = 20))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,

    /// Maximum completion tokens
    #[validate(range(min = 1))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,

    /// Maximum tokens (deprecated, use max_completion_tokens)
    #[validate(range(min = 1))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,

    /// Request metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub metadata: Option<HashMap<String, String>>,

    /// Penalize new topics (-2.0 to 2.0)
    #[validate(range(min = -2.0, max = 2.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,

    /// **Hadrian Extension:** Reasoning/extended thinking configuration (Anthropic, O1/O3 models)
    #[validate(nested)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<CreateChatCompletionReasoning>,

    /// Output format
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    /// Random seed for reproducibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,

    /// Stop sequence(s)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,

    /// Enable streaming
    #[serde(default)]
    pub stream: bool,

    /// Stream options
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,

    /// Sampling temperature (0.0 to 2.0)
    #[validate(range(min = 0.0, max = 2.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Tool choice configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,

    /// Available tools
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,

    /// Nucleus sampling probability (0.0 to 1.0)
    #[validate(range(min = 0.0, max = 1.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// User identifier for abuse detection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// **Hadrian Extension:** Per-request sovereignty requirements.
    /// Merged with API key requirements (most restrictive wins).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sovereignty_requirements: Option<crate::config::SovereigntyRequirements>,
}
