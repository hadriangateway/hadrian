#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize, Serializer};

use super::chat_completion::CacheControl;

/// Serialize f64 as i64 when it's a whole number, to satisfy APIs that expect integer types.
fn serialize_as_integer<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(v) if v.fract() == 0.0 => serializer.serialize_i64(*v as i64),
        Some(v) => serializer.serialize_f64(*v),
        None => serializer.serialize_none(),
    }
}
use validator::Validate;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseInputImageDetail {
    #[default]
    Auto,
    High,
    Low,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponseInputAudioFormat {
    Mp3,
    Wav,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EasyInputMessageRole {
    User,
    System,
    Assistant,
    Developer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputMessageItemRole {
    User,
    System,
    Developer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    InProgress,
    Completed,
    Incomplete,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputMessageStatus {
    Completed,
    Incomplete,
    InProgress,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputItemReasoningStatus {
    Completed,
    Incomplete,
    InProgress,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputItemFunctionCallStatus {
    Completed,
    Incomplete,
    InProgress,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchStatus {
    Completed,
    Searching,
    InProgress,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenerationStatus {
    InProgress,
    Completed,
    Generating,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OpenResponsesReasoningFormat {
    Unknown,
    #[serde(rename = "openai-responses-v1")]
    OpenaiResponsesV1,
    #[serde(rename = "xai-responses-v1")]
    XaiResponsesV1,
    #[serde(rename = "anthropic-claude-v1")]
    AnthropicClaudeV1,
    #[serde(rename = "google-gemini-v1")]
    GoogleGeminiV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponsesSearchContextSize {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponsesReasoningEffort {
    High,
    Medium,
    Low,
    Minimal,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponsesReasoningSummary {
    Auto,
    Concise,
    Detailed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponseTextConfigVerbosity {
    High,
    Low,
    Medium,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DataVectorStore {
    Deny,
    Allow,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Quantization {
    Int4,
    Int8,
    Fp4,
    Fp6,
    Fp8,
    Fp16,
    Bf16,
    Fp32,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderSort {
    Price,
    Throughput,
    Latency,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponsesResponseStatus {
    Completed,
    Incomplete,
    InProgress,
    Failed,
    Cancelled,
    Queued,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponsesErrorCode {
    ServerError,
    RateLimitExceeded,
    InvalidPrompt,
    VectorStoreTimeout,
    InvalidImage,
    InvalidImageFormat,
    InvalidBase64Image,
    InvalidImageUrl,
    ImageTooLarge,
    ImageTooSmall,
    ImageParseError,
    ImageContentPolicyViolation,
    InvalidImageMode,
    ImageFileTooLarge,
    UnsupportedImageMediaType,
    EmptyImageFile,
    FailedToDownloadImage,
    ImageFileNotFound,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteDetailsReason {
    MaxOutputTokens,
    ContentFilter,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponsesIncludable {
    #[serde(rename = "file_search_call.results")]
    FileSearchCallResults,
    #[serde(rename = "message.input_image.image_url")]
    MessageInputImageImageUrl,
    #[serde(rename = "computer_call_output.output.image_url")]
    ComputerCallOutputOutputImageUrl,
    #[serde(rename = "reasoning.encrypted_content")]
    ReasoningEncryptedContent,
    #[serde(rename = "code_interpreter_call.outputs")]
    CodeInterpreterCallOutputs,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponsesServiceTier {
    Auto,
    Default,
    Flex,
    Priority,
    Scale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponsesTruncation {
    Auto,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseInputAudioData {
    pub data: String,
    pub format: ResponseInputAudioFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // Intentional: matches OpenAI API spec
pub enum ResponseInputContentItem {
    InputText {
        text: String,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    InputImage {
        #[serde(default)]
        detail: ResponseInputImageDetail,
        #[serde(skip_serializing_if = "Option::is_none")]
        image_url: Option<String>,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    InputFile {
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_url: Option<String>,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    InputAudio {
        input_audio: ResponseInputAudioData,
        /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EasyInputMessageContent {
    Text(String),
    Parts(Vec<ResponseInputContentItem>),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningTextContentType {
    ReasoningText,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningTextContent {
    #[serde(rename = "type")]
    pub type_: ReasoningTextContentType,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSummaryTextType {
    SummaryText,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningSummaryText {
    #[serde(rename = "type")]
    pub type_: ReasoningSummaryTextType,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponsesReasoningType {
    Reasoning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesReasoning {
    #[serde(rename = "type")]
    pub type_: ResponsesReasoningType,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ReasoningTextContent>>,
    pub summary: Vec<ReasoningSummaryText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<OutputItemReasoningStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<OpenResponsesReasoningFormat>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Citation & Annotation Types
// ─────────────────────────────────────────────────────────────────────────────
//
// Annotations are metadata attached to response text indicating source citations.
// When file_search finds relevant content, the model references it using markers
// like `[Source 1]`. These markers are then converted to structured annotations
// with byte positions pointing to where the citation appears in the response.
//
// ## Citation Flow
//
// 1. File search returns results numbered as Source 1, Source 2, etc.
// 2. Model generates response text with citation markers: "According to [Source 1]..."
// 3. CitationTracker parses markers and creates FileCitation annotations
// 4. Annotations are injected into `response.content_part.done` SSE events
//
// ## Frontend Rendering
//
// Clients should:
// 1. Parse the `annotations` array from `output_text` content items
// 2. Use `index` to locate citation markers in the text
// 3. Optionally replace markers with interactive citation UI elements
// 4. Link citations to files using `file_id` for navigation/preview
//
// ## Example Response
//
// ```json
// {
//   "type": "output_text",
//   "text": "According to [Source 1], revenue increased by 15%.",
//   "annotations": [
//     {
//       "type": "file_citation",
//       "file_id": "file-abc123",
//       "filename": "q3_report.pdf",
//       "index": 13
//     }
//   ]
// }
// ```
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileCitationType {
    FileCitation,
}

/// A citation pointing to a file used as a source in the response.
///
/// Generated when the model references content from file_search results.
/// The `index` field indicates where in the response text the citation
/// marker (e.g., `[Source 1]`) appears.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileCitation {
    #[serde(rename = "type")]
    pub type_: FileCitationType,
    /// The unique identifier of the cited file (prefixed with `file-`).
    pub file_id: String,
    /// The display name of the cited file.
    pub filename: String,
    /// Byte offset in the response text where the citation marker starts.
    pub index: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UrlCitationType {
    UrlCitation,
}

/// A citation pointing to a URL used as a source in the response.
///
/// Generated when the model references content from web search results.
/// The `start_index` and `end_index` fields define the byte range in
/// the response text that should be associated with this citation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UrlCitation {
    #[serde(rename = "type")]
    pub type_: UrlCitationType,
    /// The source URL.
    pub url: String,
    /// The title of the web page.
    pub title: String,
    /// Byte offset where the cited text range begins.
    pub start_index: u64,
    /// Byte offset where the cited text range ends (exclusive).
    pub end_index: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilePathType {
    FilePath,
}

/// A reference to a file path generated by the model (e.g., from code_interpreter).
///
/// Unlike `FileCitation` which points to source files, `FilePath` references
/// files that were created or modified during response generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePath {
    #[serde(rename = "type")]
    pub type_: FilePathType,
    /// The unique identifier of the referenced file.
    pub file_id: String,
    /// Byte offset in the response text where the file reference appears.
    pub index: u64,
}

/// Annotation types that can be attached to response text.
///
/// Annotations provide structured metadata about citations and references
/// within model-generated text. They enable clients to render interactive
/// citations, link to source materials, and provide transparency about
/// which sources informed the response.
///
/// ## Annotation Types
///
/// - **FileCitation**: References a file from vector store search results.
///   The model marks these with `[Source N]` patterns which are converted
///   to structured annotations with the source file information.
///
/// - **UrlCitation**: References a web page from web search results.
///   Includes the full URL and page title for linking.
///
/// - **FilePath**: References a file generated during response creation
///   (e.g., by code_interpreter). Points to downloadable output files.
///
/// - **ContainerFileCitation**: References a file written to the shell
///   tool's `/mnt/data` workspace. Carries both the container and file
///   IDs so the client can later download the bytes via the container
///   files API.
///
/// ## Index Fields
///
/// All annotation types include index fields indicating byte positions
/// in the response text:
///
/// - `index`: Single position where a marker like `[Source 1]` starts
/// - `start_index`/`end_index`: Range of text associated with a citation
///
/// These are **byte offsets**, not character offsets. For UTF-8 text with
/// multi-byte characters, clients must account for encoding when mapping
/// indices to display positions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesAnnotation {
    /// Citation to a file from vector store search.
    FileCitation {
        /// The unique identifier of the cited file.
        file_id: String,
        /// The display name of the cited file.
        filename: String,
        /// Byte offset where the citation marker (e.g., `[Source 1]`) starts.
        index: u64,
    },
    /// Citation to a URL from web search.
    UrlCitation {
        /// The source URL.
        url: String,
        /// The title of the web page.
        title: String,
        /// Byte offset where the cited text range begins.
        start_index: u64,
        /// Byte offset where the cited text range ends (exclusive).
        end_index: u64,
    },
    /// Reference to a generated file path.
    FilePath {
        /// The unique identifier of the referenced file.
        file_id: String,
        /// Byte offset where the file reference appears.
        index: u64,
    },
    /// Citation to a file written to the shell tool's container
    /// workspace (`/mnt/data`). Shape matches OpenAI's Responses API so
    /// existing clients can render it without code changes.
    ContainerFileCitation {
        /// ID of the container the file lives in (`cntr_<uuid>`).
        container_id: String,
        /// Stable file identifier (`cfile_<uuid>`) usable with the
        /// container files API to download the bytes.
        file_id: String,
        /// Display name of the file inside `/mnt/data`.
        filename: String,
        /// Byte offset where the cited range begins. Hadrian emits `0`
        /// because we don't attempt to parse model output for filename
        /// mentions; clients should render the annotation as a
        /// whole-message reference.
        #[serde(default)]
        start_index: u64,
        /// Byte offset where the cited range ends. Same caveat as
        /// `start_index`.
        #[serde(default)]
        end_index: u64,
        /// Optional single-position index. Hadrian emits `null`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputMessageContentItem {
    OutputText {
        text: String,
        #[serde(default)]
        annotations: Vec<ResponsesAnnotation>,
        #[serde(default)]
        logprobs: Vec<serde_json::Value>,
    },
    Refusal {
        refusal: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EasyInputMessage {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<MessageType>,
    pub role: EasyInputMessageRole,
    pub content: EasyInputMessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMessageItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<MessageType>,
    pub role: InputMessageItemRole,
    pub content: Vec<ResponseInputContentItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputMessage {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: MessageType,
    pub role: String, // "assistant"
    pub content: Vec<OutputMessageContentItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<OutputMessageStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FunctionToolCallType {
    FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionToolCall {
    #[serde(rename = "type")]
    pub type_: FunctionToolCallType,
    pub id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FunctionCallOutputType {
    FunctionCallOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCallOutput {
    #[serde(rename = "type")]
    pub type_: FunctionCallOutputType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub call_id: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputItemFunctionCallType {
    FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputItemFunctionCall {
    #[serde(rename = "type")]
    pub type_: OutputItemFunctionCallType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub arguments: String,
    pub call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<OutputItemFunctionCallStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchCallOutputType {
    WebSearchCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchCallOutput {
    #[serde(rename = "type")]
    pub type_: WebSearchCallOutputType,
    pub id: String,
    pub status: WebSearchStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileSearchCallOutputType {
    FileSearchCall,
}

/// Content item within a file search result.
///
/// Matches OpenAI's format where content is an array of typed items.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileSearchResultContent {
    /// Text content from the search result.
    Text { text: String },
}

/// A single result item from a file search operation.
///
/// This matches OpenAI's file search result schema when `include=["file_search_call.results"]`
/// is specified in the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchResultItem {
    /// The ID of the file this result came from.
    pub file_id: String,
    /// The filename of the source file.
    pub filename: String,
    /// Relevance score between 0 and 1.
    pub score: f64,
    /// Optional attributes/metadata associated with the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<HashMap<String, serde_json::Value>>,
    /// The content retrieved from the file.
    /// OpenAI uses an array format with typed content items.
    pub content: Vec<FileSearchResultContent>,
}

/// Output item for a file_search tool call.
///
/// When the model invokes file_search, this output item is included in the response
/// to show the queries that were searched and (optionally) the search results.
///
/// The `results` field is only populated when the request includes
/// `include=["file_search_call.results"]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchCallOutput {
    #[serde(rename = "type")]
    pub type_: FileSearchCallOutputType,
    /// Unique identifier for this file search call.
    pub id: String,
    /// The search queries executed.
    pub queries: Vec<String>,
    /// Status of the file search operation.
    pub status: WebSearchStatus,
    /// Search results, only included when requested via the `include` parameter.
    /// When not included, this field is omitted from the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<FileSearchResultItem>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellCallType {
    ShellCall,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellCallOutputItemType {
    ShellCallOutput,
}

/// Status of a `shell_call` / `shell_call_output` item. Mirrors
/// OpenAI's `LocalShellCallStatus` / `LocalShellCallOutputStatusEnum`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellCallStatus {
    #[default]
    InProgress,
    Completed,
    /// Call was abandoned mid-flight (cancelled, killed, or timed out
    /// before the runtime reported an exit). Distinct from `completed`
    /// with a non-zero exit — those still report `completed`.
    Incomplete,
}

/// The `action` object on a `shell_call`. Mirrors OpenAI's
/// `FunctionShellAction`: a list of shell command strings to run as a
/// single script, plus optional per-call overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCallAction {
    /// Shell command lines, joined with newlines and run as one script.
    pub commands: Vec<String>,
    /// Per-call timeout in milliseconds. Clamped by the operator's
    /// `command_timeout_secs * 1000` cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Per-call cap on stdout+stderr characters fed back to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_length: Option<usize>,
    /// **Hadrian Extension:** Per-call env vars. Not part of OpenAI's
    /// `FunctionShellAction`; carried so non-OpenAI providers can drive
    /// the same code path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// **Hadrian Extension:** Per-call working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

/// Environment the shell call executed under. Mirrors OpenAI's union of
/// `LocalEnvironmentResource` and `ContainerReferenceResource` on
/// returned `shell_call` items.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellCallEnvironment {
    /// Call ran against the API client's local environment (the spec's
    /// `local` environment type).
    Local,
    /// Call ran against a managed container; `container_id` is the
    /// `cntr_<hex>` of the workspace that executed it.
    ContainerReference { container_id: String },
}

/// `shell_call` output item — the model-emitted request to run a shell
/// command. Spec name: `FunctionShellCall`.
///
/// Paired with a [`ShellCallOutputItem`] carrying the result. The two
/// items share a `call_id`; the `id` is the per-item identifier the
/// API assigns when persisting the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCall {
    #[serde(rename = "type")]
    pub type_: ShellCallType,
    /// Per-item identifier assigned by the API when the response is
    /// persisted. Not generated by the model.
    pub id: String,
    /// Model-generated call identifier. Pairs the shell_call with its
    /// shell_call_output. Required by spec.
    pub call_id: String,
    /// Action object describing the commands and per-call overrides.
    pub action: ShellCallAction,
    /// Lifecycle status — `in_progress`, `completed`, or `incomplete`.
    pub status: ShellCallStatus,
    /// Environment the call executed under (or will execute under,
    /// while `status == in_progress`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ShellCallEnvironment>,
    /// **Hadrian Extension:** principal that authored the item
    /// (`"model"`, `"gateway"`, `"client"`). Mirrors OpenAI's
    /// `created_by` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

/// One content chunk in a [`ShellCallOutputItem`]. Spec name:
/// `FunctionShellCallOutputContent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCallOutputContent {
    /// Captured stdout for this chunk.
    pub stdout: String,
    /// Captured stderr for this chunk.
    pub stderr: String,
    /// What ended the call — either an exit code or a timeout.
    pub outcome: ShellCallOutcome,
    /// **Hadrian Extension:** principal that produced the chunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

/// Discriminated outcome on a shell call output chunk. Spec union of
/// `FunctionShellCallOutputExitOutcome` and
/// `FunctionShellCallOutputTimeoutOutcome`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellCallOutcome {
    /// Process exited normally with `exit_code`.
    Exit { exit_code: i32 },
    /// Call exceeded its configured time limit.
    Timeout,
}

/// `shell_call_output` output item — the result of a [`ShellCall`].
/// Spec name: `FunctionShellCallOutput`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCallOutputItem {
    #[serde(rename = "type")]
    pub type_: ShellCallOutputItemType,
    /// Per-item identifier assigned by the API when the response is
    /// persisted.
    pub id: String,
    /// Matches the `call_id` of the paired [`ShellCall`].
    pub call_id: String,
    /// Lifecycle status — `in_progress`, `completed`, or `incomplete`.
    pub status: ShellCallStatus,
    /// Output content chunks. Currently a single entry per call; the
    /// array form leaves room for the model to stream multi-chunk
    /// transcripts in the future.
    pub output: Vec<ShellCallOutputContent>,
    /// Echoes the model-emitted `action.max_output_length` so clients
    /// can see what the model asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_length: Option<usize>,
    /// **Hadrian Extension:** Files written to `/mnt/data` during this
    /// command. Populated when the configured shell runtime supports
    /// `file_io` and `[features.containers]` is enabled. Each entry's
    /// `file_id` matches a `container_file_citation` annotation on the
    /// assistant's reply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_files: Vec<ContainerFileRef>,
    /// **Hadrian Extension:** principal that authored the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

/// Reference to one file produced or modified by a shell command.
///
/// Bytes are persisted in the `container_files` table so the
/// `GET /v1/containers/{container_id}/files/{file_id}/content` endpoint
/// can serve them; transient bytes also live in-process during a
/// response for the active stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerFileRef {
    /// Container the file lives in (`cntr_<uuid>`).
    pub container_id: String,
    /// Stable identifier (`cfile_<uuid>`).
    pub file_id: String,
    /// Display name, taken from the path under `/mnt/data`.
    pub filename: String,
    /// Absolute path inside the container.
    pub path: String,
    /// Size in bytes.
    pub bytes: u64,
    /// Best-effort MIME type derived from the filename extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// `"user"` if staged in from the request, `"assistant"` if written
    /// by the model.
    pub source: ContainerFileSource,
}

/// Origin of a captured container file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerFileSource {
    /// File was staged into `/mnt/data` from an `input_file` part on
    /// the request.
    User,
    /// File was written by the model during a shell command.
    Assistant,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionItemType {
    Compaction,
}

/// `compaction` output item — the marker the gateway (or upstream
/// provider with native compaction) emits when conversation history
/// is summarized to fit within the context window. Spec name:
/// `CompactionBody`.
///
/// OpenAI's `/v1/responses/compact` endpoint returns this item type;
/// it can also appear inline in a streamed response when
/// `context_management` triggers a server-side compaction pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionItem {
    #[serde(rename = "type")]
    pub type_: CompactionItemType,
    /// Per-item identifier assigned by the API.
    pub id: String,
    /// Encrypted opaque blob produced by the compactor; replayed on
    /// future turns to restore prior state. For OpenAI's native
    /// compaction this is an encrypted token-efficient representation;
    /// Hadrian's gateway-side compactor uses a plain-text fallback
    /// (an English summary message) since we cannot mint encrypted
    /// tokens the upstream model would understand. SDK consumers
    /// treat the field opaquely.
    pub encrypted_content: String,
    /// **Hadrian Extension:** principal that authored the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpListToolsItemType {
    McpListTools,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpItemStatus {
    #[default]
    InProgress,
    /// HTTP request to the MCP server is in flight (after arguments are
    /// finalized, before the terminal `completed` / `failed`).
    Calling,
    Completed,
    Incomplete,
    Failed,
}

/// Tool metadata advertised by a remote MCP server (one entry per tool
/// exposed via the server's `tools/list` response). Surfaced in
/// `mcp_list_tools` items so the model — and the persisted response —
/// records what tool surface was available at call time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpListedTool {
    /// Tool name as exposed by the MCP server.
    pub name: String,
    /// Human-readable description forwarded from the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the tool's parameters (forwarded verbatim
    /// from the server's `inputSchema`). Opaque to Hadrian.
    pub input_schema: serde_json::Value,
    /// Optional annotations (read-only hints, idempotency, etc.) — kept
    /// verbatim from the MCP server response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
}

/// `mcp_list_tools` output item — the snapshot of tools the model
/// could call on a given MCP server during this response. Emitted once
/// per server per response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpListToolsItem {
    #[serde(rename = "type")]
    pub type_: McpListToolsItemType,
    /// Per-item identifier assigned by the API.
    pub id: String,
    /// Matches the `server_label` on the requesting `McpTool`.
    pub server_label: String,
    /// Tool catalog returned by the server. Empty when `error` is set.
    pub tools: Vec<McpListedTool>,
    /// Populated when the `tools/list` call failed; carries the error
    /// message verbatim so SDKs can surface it. Always serialized
    /// (`null` on success) to match OpenAI's schema.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpCallItemType {
    McpCall,
}

/// `mcp_call` output item — the model-initiated invocation of a
/// remote MCP tool. Mirrors OpenAI's `MCPToolCall`: the result is
/// carried inline (`output` / `error`) on the same item, not as a
/// separate `mcp_call_output` item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCallItem {
    #[serde(rename = "type")]
    pub type_: McpCallItemType,
    /// Per-item identifier assigned by the API.
    pub id: String,
    /// Matches the `server_label` on the requesting `McpTool`.
    pub server_label: String,
    /// Tool name as advertised on the remote server.
    pub name: String,
    /// Arguments as a serialized JSON string (mirrors the
    /// `function_call.arguments` shape so SDKs can reuse helpers).
    pub arguments: String,
    /// Lifecycle status.
    pub status: McpItemStatus,
    /// Serialized tool result returned by the MCP server. `null` until
    /// the call terminates successfully. Always serialized to match
    /// OpenAI's schema.
    #[serde(default)]
    pub output: Option<String>,
    /// Populated when the call failed (transport error, MCP protocol
    /// error, or `isError=true` content block). `null` on success.
    /// Always serialized to match OpenAI's schema.
    #[serde(default)]
    pub error: Option<String>,
    /// `id` of the matching `mcp_approval_request` item when this call
    /// was gated by `require_approval`. `null` for calls that ran
    /// without approval. Always serialized to match OpenAI's schema.
    #[serde(default)]
    pub approval_request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalRequestItemType {
    McpApprovalRequest,
}

/// `mcp_approval_request` output item — emitted when the configured
/// `require_approval` policy gates a model-initiated MCP call. The
/// caller resumes execution by sending a matching
/// [`McpApprovalResponseItem`] back on the next request's input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpApprovalRequestItem {
    #[serde(rename = "type")]
    pub type_: McpApprovalRequestItemType,
    /// Per-item identifier. The caller echoes this as
    /// `approval_request_id` on their `mcp_approval_response`.
    pub id: String,
    /// Matches the `server_label` on the requesting `McpTool`.
    pub server_label: String,
    /// Tool name the model wants to call.
    pub name: String,
    /// Arguments the model proposed (JSON string, matching `McpCallItem`).
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalResponseItemType {
    McpApprovalResponse,
}

/// `mcp_approval_response` input item — the caller's decision on a
/// pending [`McpApprovalRequestItem`]. Sent on the next request's
/// input to resume (or refuse) the parked MCP call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpApprovalResponseItem {
    #[serde(rename = "type")]
    pub type_: McpApprovalResponseItemType,
    /// Echoes the `id` of the prior `McpApprovalRequestItem`.
    pub approval_request_id: String,
    /// `true` to let the call proceed, `false` to refuse.
    pub approve: bool,
    /// Optional free-text rationale. Surfaced to the model in the
    /// refusal payload so it can react to the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Whether a tool search ran on the server (Hadrian / OpenAI) or was
/// delegated to the client (BYOT). Hadrian only emits `Server`. Mirrors
/// OpenAI's `ToolSearchExecutionType`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchExecution {
    #[default]
    Server,
    Client,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchCallItemType {
    ToolSearchCall,
}

/// `tool_search_call` output item — the model's request to search the
/// deferred tool catalog. Mirrors OpenAI's `ToolSearchCall`. Under
/// `hadrian_hosted` this is synthesized from the model's underlying
/// `tool_search` function call; the raw function-call plumbing is
/// suppressed so the stream carries only this spec-shaped item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSearchCallItem {
    #[serde(rename = "type")]
    pub type_: ToolSearchCallItemType,
    /// Per-item identifier assigned by the API.
    pub id: String,
    /// Identifier the model assigned to the call. Always serialized
    /// (`null` when absent) to match OpenAI's schema.
    #[serde(default)]
    pub call_id: Option<String>,
    /// Whether the search ran server-side or client-side.
    pub execution: ToolSearchExecution,
    /// Arguments the model passed to the search (the query, optional
    /// `server_label`). Carried as a JSON value, matching OpenAI.
    pub arguments: serde_json::Value,
    /// Lifecycle status.
    pub status: ToolCallStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchOutputItemType {
    ToolSearchOutput,
}

/// `tool_search_output` output item — the tool definitions the search
/// surfaced for the matching `tool_search_call`. Mirrors OpenAI's
/// `ToolSearchOutput`. The model can then call any of the returned
/// tools; under `hadrian_hosted` those definitions are also injected
/// into the continuation request so the call actually resolves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSearchOutputItem {
    #[serde(rename = "type")]
    pub type_: ToolSearchOutputItemType,
    /// Per-item identifier assigned by the API.
    pub id: String,
    /// Echoes the `call_id` of the matching `tool_search_call`. Always
    /// serialized (`null` when absent) to match OpenAI's schema.
    #[serde(default)]
    pub call_id: Option<String>,
    /// Whether the search ran server-side or client-side.
    pub execution: ToolSearchExecution,
    /// The loaded tool definitions returned by the search.
    pub tools: Vec<serde_json::Value>,
    /// Lifecycle status.
    pub status: ToolCallStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenerationCallType {
    ImageGenerationCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationCall {
    #[serde(rename = "type")]
    pub type_: ImageGenerationCallType,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub status: ImageGenerationStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInputItem {
    Reasoning(ResponsesReasoning),
    EasyMessage(EasyInputMessage),
    MessageItem(InputMessageItem),
    FunctionCall(FunctionToolCall),
    FunctionCallOutput(FunctionCallOutput),
    OutputMessage(OutputMessage),
    OutputFunctionCall(OutputItemFunctionCall),
    WebSearchCall(WebSearchCallOutput),
    FileSearchCall(FileSearchCallOutput),
    ShellCall(ShellCall),
    ShellCallOutput(ShellCallOutputItem),
    McpListTools(McpListToolsItem),
    McpCall(McpCallItem),
    McpApprovalRequest(McpApprovalRequestItem),
    McpApprovalResponse(McpApprovalResponseItem),
    ToolSearchCall(ToolSearchCallItem),
    ToolSearchOutput(ToolSearchOutputItem),
    Compaction(CompactionItem),
    ImageGeneration(ImageGenerationCall),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInput {
    Text(String),
    Items(Vec<ResponsesInputItem>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesOutputItem {
    Message(OutputMessage),
    Reasoning(ResponsesReasoning),
    FunctionCall(OutputItemFunctionCall),
    WebSearchCall(WebSearchCallOutput),
    FileSearchCall(FileSearchCallOutput),
    ShellCall(ShellCall),
    ShellCallOutput(ShellCallOutputItem),
    McpListTools(McpListToolsItem),
    McpCall(McpCallItem),
    McpApprovalRequest(McpApprovalRequestItem),
    ToolSearchCall(ToolSearchCallItem),
    ToolSearchOutput(ToolSearchOutputItem),
    Compaction(CompactionItem),
    ImageGeneration(ImageGenerationCall),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchUserLocationType {
    Approximate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchUserLocation {
    #[serde(rename = "type")]
    pub type_: WebSearchUserLocationType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum WebSearchPreviewToolType {
    WebSearchPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebSearchPreviewTool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: WebSearchPreviewToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub enum WebSearchPreview20250311ToolType {
    #[serde(rename = "web_search_preview_2025_03_11")]
    WebSearchPreview20250311,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebSearchPreview20250311Tool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: WebSearchPreview20250311ToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum WebSearchToolType {
    WebSearch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebSearchTool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: WebSearchToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub filters: Option<WebSearchFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub enum WebSearch20250826ToolType {
    #[serde(rename = "web_search_2025_08_26")]
    WebSearch20250826,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebSearch20250826Tool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: WebSearch20250826ToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub filters: Option<WebSearchFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub cache_control: Option<CacheControl>,
}

// ─────────────────────────────────────────────────────────────────────────────
// File Search Tool (for Responses API RAG)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileSearchToolType {
    FileSearch,
}

/// File search ranking options for controlling result relevance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchRankingOptions {
    /// The ranker to use for scoring results.
    /// Values: "auto" or "default-2024-11-15"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranker: Option<String>,
    /// Minimum score threshold (0.0-1.0) for results to be included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_threshold: Option<f64>,
}

/// Filter comparison types for file search metadata filtering.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileSearchFilterComparison {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

/// A single comparison filter for file search.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct FileSearchComparisonFilter {
    #[serde(rename = "type")]
    pub type_: FileSearchFilterComparison,
    pub key: String,
    pub value: serde_json::Value,
}

/// Logical operator types for compound filters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileSearchFilterLogicalType {
    And,
    Or,
}

/// A compound filter combining multiple filters with a logical operator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct FileSearchCompoundFilter {
    #[serde(rename = "type")]
    pub type_: FileSearchFilterLogicalType,
    pub filters: Vec<FileSearchFilter>,
}

/// File search filter - either a comparison or a compound filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum FileSearchFilter {
    Comparison(FileSearchComparisonFilter),
    Compound(FileSearchCompoundFilter),
}

/// File search tool for RAG in the Responses API.
/// Enables semantic search across vector stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct FileSearchTool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: FileSearchToolType,
    /// Vector store IDs to search across.
    pub vector_store_ids: Vec<String>,
    /// Maximum number of results to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_num_results: Option<usize>,
    /// Ranking options for controlling result relevance.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub ranking_options: Option<FileSearchRankingOptions>,
    /// Metadata filters to apply to the search.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub filters: Option<FileSearchFilter>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub cache_control: Option<CacheControl>,
}

impl FileSearchTool {
    /// Check if this is a file_search tool.
    pub fn is_file_search(&self) -> bool {
        matches!(self.type_, FileSearchToolType::FileSearch)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ShellToolType {
    Shell,
}

/// Shell tool — instructs the model that it may call `shell` and the
/// gateway will execute the resulting commands in a sandboxed runtime
/// (or forward them to the upstream provider's hosted runtime if
/// configured for passthrough).
///
/// Mirrors OpenAI's `shell` tool definition for GPT-5.2+. The
/// `environment` block lets a caller request narrower constraints than
/// the operator's defaults — anything outside those defaults is
/// rejected with `400` at request validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ShellTool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: ShellToolType,
    /// Runtime environment overrides. Every field is a **subset** of
    /// what `[features.server_tools.shell_limits]` permits; requests
    /// asking for more than the operator allows are rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub environment: Option<ShellEnvironment>,
}

/// Per-request runtime-environment overrides. Tagged on `type` per
/// OpenAI's `shell` tool spec:
///
/// - `container_auto` — let the gateway provision a container, with
///   optional `memory_limit`, `network_policy`, `file_ids`, and `skills`.
/// - `container_reference` — attach to a specific `container_id`
///   created earlier via `POST /v1/containers` (or chained via a prior
///   response). Operator caps still apply at execution time.
/// - `local` — the API client itself runs shell commands and returns
///   `shell_call_output` items back. Requires the `client_passthrough`
///   runtime; rejected with 400 otherwise.
///
/// Hadrian historically accepted a flat object with `container_auto`
/// as a nested field plus environment-level `domain_secrets`; that
/// shape is no longer accepted — the wire format must match OpenAI's
/// SDK contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellEnvironment {
    /// Provision a fresh (or reuse a chained) container with the
    /// supplied policy. Equivalent to OpenAI's
    /// `{"type": "container_auto", …}`.
    ContainerAuto(ShellContainerAuto),
    /// Attach to an existing container by id. Used when the caller
    /// pre-created a container via `POST /v1/containers` and wants
    /// every response to land in that workspace.
    ContainerReference(ShellContainerReference),
    /// API client runs shell commands locally and returns
    /// `shell_call_output` items. Spec name: `LocalEnvironmentParam`.
    Local(ShellLocalEnvironment),
}

impl ShellEnvironment {
    /// True when the environment is the auto variant. Used by the
    /// request-resolver to decide whether to consult container-level
    /// fields like `memory_limit`.
    pub fn is_auto(&self) -> bool {
        matches!(self, ShellEnvironment::ContainerAuto(_))
    }

    /// True when the environment delegates execution to the API client.
    pub fn is_local(&self) -> bool {
        matches!(self, ShellEnvironment::Local(_))
    }

    /// Egress policy regardless of variant. `local` has no egress
    /// policy (the client manages its own network).
    pub fn network_policy(&self) -> Option<&ShellNetworkPolicy> {
        match self {
            ShellEnvironment::ContainerAuto(a) => a.network_policy.as_ref(),
            ShellEnvironment::ContainerReference(r) => r.network_policy.as_ref(),
            ShellEnvironment::Local(_) => None,
        }
    }

    /// `container_id` when explicitly referenced.
    pub fn container_reference_id(&self) -> Option<&str> {
        match self {
            ShellEnvironment::ContainerReference(r) => Some(r.container_id.as_str()),
            _ => None,
        }
    }
}

/// Container auto-provisioning overrides (the OpenAI `container_auto`
/// shape: `ContainerAutoParam`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellContainerAuto {
    /// Memory ceiling for the container, e.g. `"512m"`, `"1g"`. Parsed
    /// case-insensitively. Capped by the operator's `max_mem_limit_mb`.
    /// Spec enumerates `1g | 4g | 16g | 64g` only; Hadrian accepts
    /// arbitrary `<n>[k|m|g]` values for runtime-tuning flexibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<String>,
    /// Network policy applied to outbound traffic from the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy: Option<ShellNetworkPolicy>,
    /// Pre-uploaded Files-API IDs to stage into `/mnt/data` at session
    /// start. Spec: `ContainerAutoParam.file_ids`. Resolved via the
    /// same Files-API path as `input_file` parts on the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<String>>,
    /// Skills to mount for this session. Spec: `ContainerAutoParam.skills`.
    /// Same type as the top-level `skills` field on the request — they
    /// merge with whatever's defined at request scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<RequestSkill>>,
    /// Per-request idle-TTL override. Spec: `ContainerAutoParam.expires_after`
    /// — `{anchor: "last_active_at", minutes: N}`. Capped by
    /// `[features.containers].max_idle_ttl_secs / 60`; omitted means
    /// fall back to the container row's persisted TTL (when chained)
    /// or `default_idle_ttl_secs` (for a fresh auto session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_after: Option<ContainerExpiresAfter>,
}

/// Reference to an existing container created via
/// `POST /v1/containers`. Per OpenAI's spec, the network policy can
/// still be tightened per-request; widening beyond the container's
/// stored policy (or the operator caps) is rejected.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellContainerReference {
    /// `cntr_<hex>` of the container to attach to. Must belong to the
    /// caller's organization and be in `active` status.
    pub container_id: String,
    /// Network policy applied to outbound traffic from the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy: Option<ShellNetworkPolicy>,
}

/// Local-execution environment. Spec name: `LocalEnvironmentParam`.
/// The model emits `shell_call` items and the API client (not the
/// gateway) executes them and returns `shell_call_output` items.
/// Hadrian routes the call through but doesn't execute it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellLocalEnvironment {
    /// Skills the client claims are mounted on its local filesystem.
    /// Each entry carries the path on the client side so the model
    /// can refer to it. Spec name: `LocalSkillParam`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<LocalSkill>>,
}

/// A skill mounted on the API client's local filesystem. Spec name:
/// `LocalSkillParam`. Carries the client-side `path` rather than a
/// gateway-resolvable id since the client owns the filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSkill {
    /// Display name surfaced to the model.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Path on the client where the skill bundle lives.
    pub path: String,
}

/// Container TTL hint. Mirrors OpenAI's `expires_after` shape — only
/// `last_active_at` is honored today (the same anchor the operator's
/// idle reaper uses), with `minutes` setting the per-container idle
/// TTL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ContainerExpiresAfter {
    /// Anchor for the TTL countdown. Today only `"last_active_at"` is
    /// supported; unknown anchors reject with 400.
    #[serde(default)]
    pub anchor: ContainerExpiresAfterAnchor,
    /// Minutes after `anchor` when the container expires. Validated
    /// against `[features.containers].max_idle_ttl_secs / 60`.
    pub minutes: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ContainerExpiresAfterAnchor {
    #[default]
    LastActiveAt,
}

/// Per-domain egress policy. Matches OpenAI's `network_policy` shape:
/// `{ "type": "allowlist", "allowed_domains": [...], "domain_secrets": [...] }`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ShellNetworkPolicy {
    /// Policy kind. Only `allowlist` is supported today; the field is
    /// here for forward compatibility with future OpenAI additions.
    #[serde(default, rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(value_type = String, rename = "type"))]
    pub type_: ShellNetworkPolicyType,
    /// Hostnames or hostname patterns (`*.example.com`) the container
    /// may make outbound requests to. Must be a subset of the
    /// operator's `allowed_egress_hosts`.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Secrets injected for outbound traffic. Accepts either OpenAI's
    /// inline `{domain, name, value}` form or Hadrian's safer
    /// `{placeholder, allowed_domains}` reference form — see
    /// [`ShellDomainSecret`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<Object>))]
    pub domain_secrets: Vec<ShellDomainSecret>,
}

/// Discriminator on `network_policy.type`. Today only `allowlist` is
/// defined; the `Other` catch-all preserves the upstream value so a
/// future OpenAI addition (e.g. `denylist`) doesn't reject requests at
/// the gateway. The shell executor still applies the allowlist policy
/// regardless — operators get unknown types logged and treated as
/// `allowlist`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", untagged)]
pub enum ShellNetworkPolicyType {
    Known(KnownShellNetworkPolicyType),
    /// Forward-compat: anything the gateway hasn't been taught yet.
    Other(String),
}

impl Default for ShellNetworkPolicyType {
    fn default() -> Self {
        Self::Known(KnownShellNetworkPolicyType::Allowlist)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnownShellNetworkPolicyType {
    #[default]
    Allowlist,
    /// Deny-all egress. Spec: `ContainerNetworkPolicyDisabledParam`.
    /// `allowed_domains` and `domain_secrets` must be empty when this
    /// is set; the resolver rejects requests that combine them.
    Disabled,
}

/// One entry in `network_policy.domain_secrets`. Accepts two forms via
/// untagged dispatch so OpenAI SDKs and Hadrian-aware callers both
/// work:
///
/// - **Inline (OpenAI)** — `{ "domain": "...", "name": "...", "value": "..." }`
///   the secret value travels on the wire. Useful for ad-hoc tokens
///   the caller owns directly.
/// - **Reference (Hadrian extension)** — `{ "placeholder": "...", "allowed_domains": [...] }`
///   matches an operator-configured secret in
///   `[features.server_tools.shell_limits].allowed_domain_secrets` so
///   the raw value never leaves the gateway.
///
/// Reference form is parsed first so a `{placeholder, ...}` payload
/// never accidentally matches the inline shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ShellDomainSecret {
    Reference(ShellDomainSecretRef),
    Inline(ShellDomainSecretInline),
}

/// Hadrian-extension placeholder reference into the operator's
/// pre-configured secret store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellDomainSecretRef {
    /// Placeholder name, matched against
    /// `allowed_domain_secrets[<name>]` in operator config.
    pub placeholder: String,
    /// Hosts this secret may flow to. Must be a subset of the
    /// operator-configured `allowed_hosts` for the placeholder; empty
    /// means "all hosts the operator permits for this secret".
    #[serde(default)]
    pub allowed_domains: Vec<String>,
}

/// One entry in `skills` on a `/v1/responses` or `/v1/containers`
/// request. Tagged per OpenAI's spec:
///
/// - `skill_reference` resolves a stored skill by UUID.
/// - `inline` embeds an ephemeral skill bundle in the request itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestSkill {
    /// Mount a skill that was previously created via the skills API.
    SkillReference(SkillReference),
    /// Mount an ephemeral skill bundle carried inline on the request.
    Inline(InlineSkill),
}

/// Reference to an existing skill created via the `/v1/skills` API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillReference {
    /// The referenced skill: a prefixed id (`skill_…`), a bare UUID, or the
    /// skill's name slug. Must belong to the caller's organization.
    pub skill_id: String,
    /// Version selector. Omit for the skill's **default** version, `"latest"`
    /// for the newest published version, or a positive integer for that exact
    /// version. Any other value rejects the request with 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Inline skill bundle. Mounted under `/skills/<synthetic-id>/` for
/// the lifetime of the request (or the container, when supplied at
/// container creation).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineSkill {
    /// Display name for the skill. Surfaced to the model in the
    /// auto-prepended `instructions` preamble.
    pub name: String,
    /// Human-readable description. Also surfaced to the model.
    pub description: String,
    /// Encoded payload. `source.type = "base64"` only today.
    pub source: InlineSkillSource,
}

/// Payload of an [`InlineSkill`]. Tagged so future encodings (e.g.
/// `url`, `file_id`) slot in without breaking older clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InlineSkillSource {
    /// Base64-encoded payload. `media_type` controls how the bytes are
    /// interpreted: `text/markdown` ⇒ single `SKILL.md`. Other media
    /// types reject with 400 until multi-file (zip) support lands.
    Base64 { media_type: String, data: String },
}

/// OpenAI-compatible inline secret. The raw value travels with the
/// request; the gateway scopes it to `domain` at egress.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellDomainSecretInline {
    /// Host (or `*.suffix`) the secret may flow to.
    pub domain: String,
    /// Environment-variable name the secret is exposed as inside the
    /// container.
    pub name: String,
    /// Raw secret value. Subject to the same operator host caps as the
    /// reference form.
    pub value: String,
}

impl ShellTool {
    pub fn is_shell(&self) -> bool {
        matches!(self.type_, ShellToolType::Shell)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum McpToolType {
    Mcp,
}

/// MCP tool — lets the model invoke tools exposed by a remote Model
/// Context Protocol server (Atlassian, Notion, GitHub, HuggingFace, …).
///
/// Mirrors OpenAI's `mcp` tool. Two mutually-exclusive shapes: pointing
/// at a remote server via `server_url`, or pointing at an OpenAI
/// first-party connector via `connector_id`. The caller supplies any
/// `authorization` bearer token directly on the tool entry; Hadrian
/// does not persist or rewrite it.
///
/// Under `mode = passthrough_openai` the tool entry is forwarded
/// verbatim to OpenAI / Azure OpenAI, and non-OpenAI providers reject
/// with `mcp_passthrough_unsupported_provider`. Under
/// `mode = hadrian_hosted` the gateway runs the MCP client loop itself
/// via `rmcp` and rewrites the tool into per-tool function tools, so
/// any provider can drive it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct McpTool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: McpToolType,
    /// Stable identifier surfaced in `mcp_list_tools` and `mcp_call`
    /// items. Required even when using `connector_id` so output items
    /// have a consistent label.
    pub server_label: String,
    /// URL of the remote MCP server (Streamable HTTP). Mutually
    /// exclusive with `connector_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    /// Identifier for an OpenAI-maintained connector
    /// (e.g. `connector_googlecalendar`). Mutually exclusive with
    /// `server_url`. Only usable under `mode = passthrough_openai`;
    /// `hadrian_hosted` cannot reach OpenAI's connector registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<String>,
    /// Human-readable description surfaced to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_description: Option<String>,
    /// Bearer / OAuth access token. The caller obtains this
    /// out-of-band (OpenAI's API does not run the OAuth dance for the
    /// remote server, and neither does Hadrian). Sent verbatim to the
    /// MCP server's `Authorization` header. Hadrian does not persist
    /// the value — clients must include it on every request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<String>,
    /// Additional HTTP headers sent with every JSON-RPC call to the
    /// MCP server. Useful for region or workspace selectors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    /// Approval gate. Spec default is `"always"` when omitted — under
    /// `hadrian_hosted` the executor enforces that default; under
    /// `passthrough_openai` the field is forwarded verbatim and OpenAI
    /// applies its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_approval: Option<McpRequireApproval>,
    /// Restrict which tools from the server are exposed to the model.
    /// Accepts either a flat list of tool names or an object form for
    /// forward-compat with extra knobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<McpAllowedTools>,
    /// Delay loading the tool definitions until the model discovers them
    /// via tool search, rather than dumping the whole catalog into the
    /// prompt. Forwarded verbatim under `passthrough_openai` (OpenAI runs
    /// its native tool search). Under `hadrian_hosted` deferral is
    /// realized by **Hadrian-side tool search**: the gateway exposes a
    /// single `tool_search` function tool, keeps the catalog server-side,
    /// and lazily injects matched per-tool function definitions as the
    /// model discovers them — so deferral works behind every provider,
    /// not just OpenAI. The upstream `tools/list` catalog fetch always
    /// runs eagerly at rewrite time (it's needed to know what's
    /// searchable); only the per-tool definitions are deferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    /// **Hadrian Extension:** opt out of Hadrian-side tool search and let
    /// the upstream handle `defer_loading` natively. Only honored when
    /// `defer_loading` is set, the gateway is in `hadrian_hosted` mode,
    /// and the resolved provider is OpenAI / Azure OpenAI (which
    /// implement native tool search); rejected with HTTP 400 otherwise.
    /// Default/`None` keeps the provider-agnostic Hadrian-side path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading_passthrough: Option<bool>,
    /// **Hadrian Extension:** upper bound, in seconds, on a single
    /// `tools/call` round-trip to this MCP server under `hadrian_hosted`.
    /// Overrides the `[features.mcp].call_timeout_secs` deployment default
    /// (300s). On expiry the in-flight `mcp_call` terminates with
    /// `status="incomplete"` and a timeout `error`. Not part of OpenAI's
    /// spec; ignored under `passthrough_openai` (the upstream owns the
    /// call loop there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_timeout_secs: Option<u64>,
}

impl McpTool {
    pub fn is_mcp(&self) -> bool {
        matches!(self.type_, McpToolType::Mcp)
    }

    /// True if exactly one of `server_url` / `connector_id` is set.
    /// `McpTool` deliberately doesn't enforce this at deserialize time
    /// so the preprocessor can surface a clean 400 error instead of a
    /// serde error blob.
    pub fn has_exactly_one_target(&self) -> bool {
        self.server_url.is_some() ^ self.connector_id.is_some()
    }
}

/// Closed enum of OpenAI's first-party connector ids. OpenAI's OpenAPI
/// defines `connector_id` as an enum of exactly these values; we mirror
/// the list so the preprocess can reject unknown ids early with a
/// stable error code rather than waiting for the upstream to 4xx.
pub const MCP_CONNECTOR_IDS: &[&str] = &[
    "connector_dropbox",
    "connector_gmail",
    "connector_googlecalendar",
    "connector_googledrive",
    "connector_microsoftteams",
    "connector_outlookcalendar",
    "connector_outlookemail",
    "connector_sharepoint",
];

/// True iff `id` matches one of [`MCP_CONNECTOR_IDS`].
pub fn is_known_mcp_connector_id(id: &str) -> bool {
    MCP_CONNECTOR_IDS.contains(&id)
}

/// Approval gating shape for the MCP tool. Accepts the spec's string
/// shorthand (`"always"` / `"never"`) or an object with `always` / `never`
/// filters that opt subsets in or out of the approval gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum McpRequireApproval {
    Mode(McpApprovalMode),
    Filter(McpApprovalFilter),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalMode {
    Always,
    Never,
}

/// Object form of `require_approval`. `always` lists tools that must
/// be gated; `never` lists tools that bypass the gate. Tools not named
/// in either fall back to the default (gate). Mirrors OpenAI's
/// `MCPToolApprovalFilter`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct McpApprovalFilter {
    /// Tools that require approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always: Option<McpToolFilter>,
    /// Tools exempt from approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub never: Option<McpToolFilter>,
}

/// Tool whitelist for an `McpTool`. Untagged so OpenAI's accepted
/// shorthand `["tool_a", "tool_b"]` round-trips as the `List` variant
/// and the `{tool_names: [...], read_only?: bool}` object form picks
/// up extra knobs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum McpAllowedTools {
    List(Vec<String>),
    Filter(McpToolFilter),
}

/// Spec `MCPToolFilter`: a set of tool names plus an optional
/// `read_only` predicate that matches tools whose MCP `readOnlyHint`
/// annotation is true. Either field is optional; both are AND-combined
/// (tool name must be in `tool_names` *and* `read_only` must match if set).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct McpToolFilter {
    /// Tool names to include. Omit to match any name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_names: Option<Vec<String>>,
    /// Restrict to tools whose annotation `readOnlyHint == true`.
    /// `None` means don't filter on this property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchToolType {
    ToolSearch,
}

/// Ranking strategy for Hadrian-side tool search over a deferred MCP
/// catalog. `Hybrid` fuses semantic + lexical relevance (RRF); `Semantic`
/// is embedding cosine only; `Lexical` is token/substring scoring with no
/// embedding dependency.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchRankerKind {
    Hybrid,
    Semantic,
    Lexical,
}

/// `tool_search` tool — OpenAI's `ToolSearchToolParam`. Configures
/// discovery of deferred tools (`defer_loading: true`). Under
/// `hadrian_hosted` a caller need not supply this: Hadrian synthesizes
/// its own search tool whenever a deferred MCP server is present. A
/// caller may still include the entry to set the Hadrian-extension
/// `ranker`; Hadrian reads that override and does not forward the entry
/// to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ToolSearchTool {
    #[serde(rename = "type")]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: ToolSearchToolType,
    /// Whether the search executes server-side (hosted) or client-side
    /// (BYOT). Hadrian only supports server-side execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub execution: Option<ToolSearchExecution>,
    /// Description shown to the model for a client-executed search tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Parameter schema for a client-executed search tool. Opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub parameters: Option<serde_json::Value>,
    /// **Hadrian Extension:** per-request override of the ranking
    /// strategy for Hadrian-side tool search. Takes precedence over the
    /// `[features.mcp.tool_search].ranker` deployment default. Requesting
    /// `semantic` on a deployment without an embedding provider returns
    /// HTTP 400 (`tool_search_ranker_unavailable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<String>))]
    pub ranker: Option<ToolSearchRankerKind>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FunctionToolType {
    #[default]
    Function,
}

/// Custom function tool. **Schema-only**: at the wire level the gateway
/// keeps `Function` as an opaque `serde_json::Value` so existing
/// rewrite pipelines (web_search → function, shell → function, MCP →
/// per-tool function) can construct/inspect it via JSON without
/// touching a typed struct. This struct exists purely to give the
/// OpenAPI spec a typed shape for the function-tool variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct FunctionTool {
    #[serde(rename = "type", default)]
    #[cfg_attr(feature = "utoipa", schema(rename = "type"))]
    pub type_: FunctionToolType,
    /// Function name. Must be `[A-Za-z0-9_-]{1,64}`.
    pub name: String,
    /// Human-readable description surfaced to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the function's parameters. Forwarded
    /// verbatim to the provider; the gateway treats it as opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Option<Object>))]
    pub parameters: Option<serde_json::Value>,
    /// When true, the model is constrained to emit arguments that
    /// strictly conform to `parameters`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    /// Delay loading the function definition until the model discovers it
    /// via tool search. On a caller-supplied function tool this is
    /// forwarded to the provider and treated as opaque. On the per-tool
    /// function tools the MCP rewrite synthesizes it is only set on the
    /// native-passthrough path (`McpTool::defer_loading_passthrough` on an
    /// OpenAI/Azure upstream); the default `hadrian_hosted` path defers via
    /// Hadrian-side tool search instead and does not set this flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    /// Extra fields the gateway forwards verbatim (e.g. MCP `annotations`
    /// on rewritten function tools). Not part of OpenAI's documented
    /// shape; preserved here so internal rewrite pipelines can attach
    /// provider-specific metadata without losing it through round-trips.
    #[serde(flatten)]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub extras: std::collections::HashMap<String, serde_json::Value>,
}

/// Tool definition - one of the supported tool variants. The schema
/// emits a `oneOf` over the variants; each variant constrains its
/// `type` field to a single literal, so consumers (and the OpenAPI
/// conformance script) can match variants by `type`. The `Function`
/// variant stays as an opaque `serde_json::Value` at the data level
/// so existing rewrite pipelines (web_search → function, shell →
/// function, MCP → per-tool function) keep constructing it via JSON;
/// the schema reports it as a [`FunctionTool`] for documentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum ResponsesToolDefinition {
    FileSearch(FileSearchTool), // Must be before Function to match type field first
    WebSearchPreview(WebSearchPreviewTool),
    WebSearchPreview20250311(WebSearchPreview20250311Tool),
    WebSearch(WebSearchTool),
    WebSearch20250826(WebSearch20250826Tool),
    Shell(ShellTool),
    Mcp(McpTool),
    ToolSearch(ToolSearchTool), // Must be before Function to match type field first
    Function(FunctionTool),
}

impl FunctionTool {
    /// Parse a JSON object into `FunctionTool`. Rejects on schema
    /// mismatch — used by the various rewrite pipelines (web_search,
    /// shell, file_search, mcp) that build function tools as JSON
    /// before wrapping in the enum variant.
    pub fn from_json(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }
}

impl ResponsesToolDefinition {
    /// Returns the file search tool if this is a file_search tool definition.
    pub fn as_file_search(&self) -> Option<&FileSearchTool> {
        match self {
            ResponsesToolDefinition::FileSearch(tool) => Some(tool),
            _ => None,
        }
    }

    /// Returns true if this is a file_search tool.
    pub fn is_file_search(&self) -> bool {
        matches!(self, ResponsesToolDefinition::FileSearch(_))
    }

    /// Returns true if this is any web_search tool variant.
    pub fn is_web_search(&self) -> bool {
        matches!(
            self,
            ResponsesToolDefinition::WebSearchPreview(_)
                | ResponsesToolDefinition::WebSearchPreview20250311(_)
                | ResponsesToolDefinition::WebSearch(_)
                | ResponsesToolDefinition::WebSearch20250826(_)
        )
    }

    /// Returns true if this is a shell tool.
    pub fn is_shell(&self) -> bool {
        matches!(self, ResponsesToolDefinition::Shell(_))
    }

    /// Returns the shell tool definition if this is a shell tool.
    pub fn as_shell(&self) -> Option<&ShellTool> {
        match self {
            ResponsesToolDefinition::Shell(tool) => Some(tool),
            _ => None,
        }
    }

    /// Returns true if this is an MCP tool.
    pub fn is_mcp(&self) -> bool {
        matches!(self, ResponsesToolDefinition::Mcp(_))
    }

    /// Returns the MCP tool definition if this is an MCP tool.
    pub fn as_mcp(&self) -> Option<&McpTool> {
        match self {
            ResponsesToolDefinition::Mcp(tool) => Some(tool),
            _ => None,
        }
    }

    /// Returns true if this is a tool_search tool.
    pub fn is_tool_search(&self) -> bool {
        matches!(self, ResponsesToolDefinition::ToolSearch(_))
    }

    /// Returns the tool_search tool definition if this is one.
    pub fn as_tool_search(&self) -> Option<&ToolSearchTool> {
        match self {
            ResponsesToolDefinition::ToolSearch(tool) => Some(tool),
            _ => None,
        }
    }

    /// Extracts `search_context_size` from any web_search tool variant.
    pub fn web_search_context_size(&self) -> Option<ResponsesSearchContextSize> {
        match self {
            ResponsesToolDefinition::WebSearchPreview(t) => t.search_context_size,
            ResponsesToolDefinition::WebSearchPreview20250311(t) => t.search_context_size,
            ResponsesToolDefinition::WebSearch(t) => t.search_context_size,
            ResponsesToolDefinition::WebSearch20250826(t) => t.search_context_size,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponsesToolChoiceDefault {
    Auto,
    None,
    Required,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponsesNamedToolChoiceType {
    Function,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesNamedToolChoice {
    #[serde(rename = "type")]
    pub type_: ResponsesNamedToolChoiceType,
    pub name: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WebSearchToolChoiceType {
    #[serde(rename = "web_search_preview_2025_03_11")]
    WebSearchPreview20250311,
    #[serde(rename = "web_search_preview")]
    WebSearchPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesWebSearchToolChoice {
    #[serde(rename = "type")]
    pub type_: WebSearchToolChoiceType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellToolChoiceType {
    Shell,
}

/// Force the model to call the shell tool. Spec name:
/// `ToolChoiceShell` — `{"type": "shell"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesShellToolChoice {
    #[serde(rename = "type")]
    pub type_: ShellToolChoiceType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpToolChoiceType {
    Mcp,
}

/// Force the model to call a specific MCP tool. Mirrors OpenAI's
/// `{"type": "mcp", "server_label": "...", "name": "..."}`. Under
/// `hadrian_hosted` mode the rewrite turns this into a function-tool
/// choice (`{"type": "function", "name": "mcp_<label>__<name>"}`)
/// before the request reaches the provider; under passthrough this
/// is forwarded verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesMcpToolChoice {
    #[serde(rename = "type")]
    pub type_: McpToolChoiceType,
    pub server_label: String,
    /// Tool name as advertised by the MCP server. Omit to mean "any
    /// tool from this server" — matches OpenAI's documented shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesToolChoice {
    String(ResponsesToolChoiceDefault),
    WebSearch(ResponsesWebSearchToolChoice),
    Shell(ResponsesShellToolChoice),
    Mcp(ResponsesMcpToolChoice),
    Named(ResponsesNamedToolChoice),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormatTextConfig {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        schema: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct ResponseTextConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ResponseFormatTextConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<ResponseTextConfigVerbosity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct ResponsesReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ResponsesReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ResponsesReasoningSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PromptVariable {
    Text(String),
    InputText {
        #[serde(rename = "type")]
        type_: String,
        text: String,
    },
    InputImage {
        #[serde(rename = "type")]
        type_: String,
        detail: ResponseInputImageDetail,
        #[serde(skip_serializing_if = "Option::is_none")]
        image_url: Option<String>,
    },
    InputFile {
        #[serde(rename = "type")]
        type_: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_url: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesPrompt {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<HashMap<String, PromptVariable>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BigNumberUnion {
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMaxPrice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<BigNumberUnion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<BigNumberUnion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<BigNumberUnion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<BigNumberUnion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<BigNumberUnion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderNameOrString {
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_vector_store: Option<DataVectorStore>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce_distillable_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<ProviderNameOrString>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<ProviderNameOrString>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<ProviderNameOrString>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<Quantization>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<ProviderSort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_price: Option<ProviderMaxPrice>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebPluginEngine {
    Native,
    Exa,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FileParserPdfEngine {
    MistralOcr,
    PdfText,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileParserPdfConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<FileParserPdfEngine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
pub enum ResponsesPlugin {
    Moderation,
    Web {
        #[serde(skip_serializing_if = "Option::is_none")]
        max_results: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        search_prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        engine: Option<WebPluginEngine>,
    },
    #[serde(rename = "file-parser")]
    FileParser {
        #[serde(skip_serializing_if = "Option::is_none")]
        max_files: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pdf: Option<FileParserPdfConfig>,
    },
}

/// Create responses request (OpenAI Responses API)
#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateResponsesPayload {
    /// Input messages/items
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub input: Option<ResponsesInput>,

    /// System instructions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    /// Request metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub metadata: Option<HashMap<String, String>>,

    /// Available tools
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponsesToolDefinition>>,

    /// Tool choice configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub tool_choice: Option<ResponsesToolChoice>,

    /// Allow parallel tool calls
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,

    /// Model to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// **Hadrian Extension:** List of models for multi-model routing (alternative to single model)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,

    /// Text configuration
    #[validate(nested)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub text: Option<ResponseTextConfig>,

    /// Reasoning configuration
    #[validate(nested)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub reasoning: Option<ResponsesReasoningConfig>,

    /// Maximum output tokens
    #[validate(range(min = 1.0))]
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_as_integer"
    )]
    pub max_output_tokens: Option<f64>,

    /// Sampling temperature (0.0 to 2.0)
    #[validate(range(min = 0.0, max = 2.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Nucleus sampling probability (0.0 to 1.0)
    #[validate(range(min = 0.0, max = 1.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// **Hadrian Extension:** Top-k sampling (supported by some providers like Anthropic)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<f64>,

    /// Prompt cache key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,

    /// Previous response ID for conversation continuation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,

    /// Prompt template reference
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub prompt: Option<ResponsesPrompt>,

    /// Items to include in response
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<String>))]
    pub include: Option<Vec<ResponsesIncludable>>,

    /// Run in background
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,

    /// Safety identifier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,

    /// Store response
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,

    /// Service tier
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub service_tier: Option<serde_json::Value>,

    /// Truncation strategy
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub truncation: Option<serde_json::Value>,

    /// **Hadrian Extension:** Presence penalty (-2.0 to 2.0)
    #[validate(range(min = -2.0, max = 2.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,

    /// **Hadrian Extension:** Frequency penalty (-2.0 to 2.0)
    #[validate(range(min = -2.0, max = 2.0))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,

    /// Enable streaming
    #[serde(default)]
    pub stream: bool,

    /// **Hadrian Extension:** Provider routing configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub provider: Option<ResponsesProviderConfig>,

    /// **Hadrian Extension:** Plugins to enable for this request
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<Object>))]
    pub plugins: Option<Vec<ResponsesPlugin>>,

    /// User identifier for abuse detection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// **Hadrian Extension:** Per-request sovereignty requirements.
    /// Merged with API key requirements (most restrictive wins).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sovereignty_requirements: Option<crate::config::SovereigntyRequirements>,

    /// **Hadrian Extension:** Skills to mount into the shell-tool
    /// session. Hadrian treats skills as a top-level request field
    /// rather than threading them through the OpenAI shell-tool
    /// `environment` block, so they're accepted on every responses
    /// request regardless of whether the upstream provider has its own
    /// skills surface.
    ///
    /// Mirrors OpenAI's typed shape — each entry is either a reference
    /// to a stored skill or an inline bundle:
    ///
    /// ```json
    /// [
    ///   {"type": "skill_reference", "skill_id": "skill_…", "version": "latest"},
    ///   {"type": "inline", "name": "extract-csv", "description": "...",
    ///    "source": {"type": "base64", "media_type": "text/markdown", "data": "..."}}
    /// ]
    /// ```
    ///
    /// For `skill_reference`, `skill_id` is a prefixed id (`skill_…`), a bare
    /// UUID, or the skill's name slug, owned by the caller's org. `version` is
    /// optional: omit for the skill's **default** version, `"latest"` for the
    /// newest, or a positive integer for that exact version (default and latest
    /// can differ).
    ///
    /// For `inline`, the decoded `source.data` is mounted as an
    /// ephemeral skill bundle: `text/markdown` is treated as the
    /// `SKILL.md` content; other media types are rejected today.
    ///
    /// Each resolved skill's `SKILL.md` is prepended to `instructions`
    /// so the model knows the skill is available; all skill files are
    /// materialized under `/skills/<skill_id>/` inside the sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<Object>))]
    pub skills: Option<Vec<RequestSkill>>,

    /// Context management directives. Forwarded verbatim to providers
    /// that support server-side compaction (OpenAI, Azure OpenAI); for
    /// others the field is ignored at the adapter layer. See OpenAI's
    /// compaction guide for the schema; the canonical entry is
    /// `{"type": "compaction", "compact_threshold": <tokens>}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<Object>))]
    pub context_management: Option<Vec<ContextManagementItem>>,
}

/// Entry in `CreateResponsesPayload::context_management`.
///
/// Mirrors the OpenAI Responses API directive — we surface the
/// `compaction` variant explicitly so callers get type-checked help
/// when wiring it up. Unknown variants are accepted via the `Other`
/// catch-all and forwarded to the upstream provider verbatim; we
/// prefer that over `deny_unknown` so OpenAI's spec can grow new
/// types without rejecting requests at the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextManagementItem {
    /// Server-side compaction. When the rendered token count crosses
    /// `compact_threshold`, the provider emits a compaction item in
    /// the same response stream and prunes context before continuing
    /// inference.
    ///
    /// For non-OpenAI providers Hadrian runs a **gateway-side**
    /// compactor before dispatch. The Hadrian-extension `strategy`
    /// and `prompt` fields let the caller pick the algorithm and
    /// (when strategy is `llm`) override the default summarisation
    /// prompt.
    Compaction {
        /// Token count that triggers a compaction pass. Provider
        /// validates the range.
        #[serde(
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_as_integer"
        )]
        compact_threshold: Option<f64>,
        /// **Hadrian Extension:** which compactor to run when behind a
        /// provider without native server-side compaction (Anthropic,
        /// Bedrock, Vertex). Omitting the field uses the operator
        /// default from `[features.responses.compaction].default_strategy`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strategy: Option<CompactionStrategy>,
        /// **Hadrian Extension:** override prompt for `strategy = llm`.
        /// Empty / missing falls back to the operator default. Ignored
        /// when strategy is `truncate` or the request is routed to a
        /// provider with native compaction (forwarded verbatim).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// **Hadrian Extension:** Forward-compatibility catch-all for
    /// `type` values Hadrian doesn't know about. The raw value is
    /// preserved and forwarded to the upstream provider unchanged.
    #[serde(other)]
    Other,
}

/// **Hadrian Extension:** compactor strategy for non-OpenAI providers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// Summarise older items via a single follow-up call to the
    /// active provider; replace them with a `compaction` input item
    /// carrying the summary. Higher quality, costs one extra inference.
    Llm,
    /// Drop oldest non-system items until under `compact_threshold`.
    /// Deterministic and free; preserves no narrative.
    Truncate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesErrorField {
    pub code: ResponsesErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesIncompleteDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<IncompleteDetailsReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesUsageInputTokensDetails {
    pub cached_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesUsageOutputTokensDetails {
    pub reasoning_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesUsageCostDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_inference_cost: Option<f64>,
    pub upstream_inference_input_cost: f64,
    pub upstream_inference_output_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesUsage {
    pub input_tokens: i64,
    pub input_tokens_details: ResponsesUsageInputTokensDetails,
    pub output_tokens: i64,
    pub output_tokens_details: ResponsesUsageOutputTokensDetails,
    pub total_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_byok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_details: Option<ResponsesUsageCostDetails>,
}

impl ResponsesUsage {
    /// Fold `other` into `self`, summing one server-tool loop turn's usage
    /// into the running total. Token counts and cost components add; `cost`
    /// and `cost_details.upstream_inference_cost` add when both present and
    /// otherwise take the present value; `is_byok` is sticky-true (once any
    /// turn was BYOK the whole response is marked BYOK, since the wire shape
    /// has no per-turn breakdown).
    pub fn accumulate(&mut self, other: &ResponsesUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
        self.input_tokens_details.cached_tokens += other.input_tokens_details.cached_tokens;
        self.output_tokens_details.reasoning_tokens += other.output_tokens_details.reasoning_tokens;
        match (self.cost.as_mut(), other.cost) {
            (Some(a), Some(b)) => *a += b,
            (None, Some(b)) => self.cost = Some(b),
            _ => {}
        }
        if other.is_byok == Some(true) {
            self.is_byok = Some(true);
        }
        if let Some(add_details) = &other.cost_details {
            let target = self.cost_details.get_or_insert(ResponsesUsageCostDetails {
                upstream_inference_cost: None,
                upstream_inference_input_cost: 0.0,
                upstream_inference_output_cost: 0.0,
            });
            target.upstream_inference_input_cost += add_details.upstream_inference_input_cost;
            target.upstream_inference_output_cost += add_details.upstream_inference_output_cost;
            match (
                target.upstream_inference_cost.as_mut(),
                add_details.upstream_inference_cost,
            ) {
                (Some(a), Some(b)) => *a += b,
                (None, Some(b)) => target.upstream_inference_cost = Some(b),
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesReasoningConfigOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ResponsesReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ResponsesReasoningSummary>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponseType {
    Response,
}

/// Request body for `POST /v1/responses/compact`.
///
/// Mirrors OpenAI's `CompactResponseMethodPublicBody`: `model` is
/// required; everything else is optional. Hadrian-specific extensions
/// (`models`, `stream`, `sovereignty_requirements`) ride alongside
/// because the gateway still routes, streams, and enforces sovereignty
/// for compaction requests just like the main `/responses` handler.
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CompactRequest {
    /// Model to compact with. Required by OpenAI; Hadrian also accepts
    /// it as the routing key when the gateway picks an upstream
    /// provider.
    pub model: String,

    /// Conversation history to compact. Accepts a plain string or the
    /// typed item array used by `POST /responses`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub input: Option<ResponsesInput>,

    /// Continue compacting from a previously persisted response. Spec:
    /// `CompactResponseMethodPublicBody.previous_response_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,

    /// System instructions threaded into the compaction prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    /// OpenAI prompt-cache key passthrough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,

    /// **Hadrian Extension:** alternate model list for multi-model
    /// routing. The first entry the gateway resolves successfully wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,

    /// **Hadrian Extension:** stream the compacted response as SSE.
    /// Forwarded verbatim to the upstream provider when supported.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,

    /// **Hadrian Extension:** per-request data-sovereignty requirements.
    /// Merged with API-key-level requirements; the most restrictive
    /// wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty_requirements: Option<crate::config::SovereigntyRequirements>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateResponsesResponse {
    pub id: String,
    pub object: ResponseType,
    pub created_at: f64,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ResponsesResponseStatus>,
    pub output: Vec<ResponsesOutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_text: Option<String>,
    pub prompt_cache_key: Option<String>,
    pub safety_identifier: Option<String>,
    pub error: Option<ResponsesErrorField>,
    pub incomplete_details: Option<ResponsesIncompleteDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponsesUsage>,
    pub completed_at: Option<f64>,
    pub max_tool_calls: Option<f64>,
    pub top_logprobs: Option<f64>,
    #[serde(serialize_with = "serialize_as_integer")]
    pub max_output_tokens: Option<f64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub instructions: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
    pub tools: Option<Vec<ResponsesToolDefinition>>,
    pub tool_choice: Option<ResponsesToolChoice>,
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<ResponsesPrompt>,
    pub background: Option<bool>,
    pub previous_response_id: Option<String>,
    pub reasoning: Option<ResponsesReasoningConfigOutput>,
    pub service_tier: Option<ResponsesServiceTier>,
    pub store: Option<bool>,
    pub truncation: Option<ResponsesTruncation>,
    pub text: Option<ResponseTextConfig>,
}

impl CreateResponsesResponse {
    /// Serialize to JSON with echo fields merged in per OpenAI Responses API spec.
    pub fn to_json_with_echo(
        &self,
        echo_fields: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Value {
        let mut val = serde_json::to_value(self).unwrap_or_default();
        if let serde_json::Value::Object(ref mut map) = val {
            if self.status == Some(ResponsesResponseStatus::Completed) {
                map.insert(
                    "completed_at".into(),
                    serde_json::json!(chrono::Utc::now().timestamp() as f64),
                );
            }
            for (k, v) in echo_fields {
                // Don't overwrite reasoning if the struct already has it set from conversion
                if k == "reasoning" && self.reasoning.is_some() {
                    continue;
                }
                map.insert(k, v);
            }
        }
        val
    }
}

/// Build a Responses API `response` JSON object for streaming events.
///
/// Shared by all provider stream transformers (Anthropic, Bedrock, Vertex).
pub fn build_streaming_response_json(
    id: &str,
    model: &str,
    created_at: f64,
    status: &str,
    output: serde_json::Value,
    echo_fields: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::json!(id));
    obj.insert("object".into(), serde_json::json!("response"));
    obj.insert("created_at".into(), serde_json::json!(created_at));
    obj.insert("model".into(), serde_json::json!(model));
    obj.insert("status".into(), serde_json::json!(status));
    obj.insert("output".into(), output);
    obj.insert("completed_at".into(), serde_json::Value::Null);
    obj.insert("error".into(), serde_json::Value::Null);
    obj.insert("incomplete_details".into(), serde_json::Value::Null);
    obj.insert("usage".into(), serde_json::Value::Null);
    for (k, v) in echo_fields {
        obj.insert(k.clone(), v.clone());
    }
    obj
}

impl CreateResponsesPayload {
    /// Strip gateway-managed fields before the payload is serialized and
    /// forwarded to an OpenAI-compatible upstream provider (OpenAI, Azure,
    /// OpenRouter, …).
    ///
    /// These fields are orchestration concerns that Hadrian consumes itself
    /// and the upstream must never see:
    ///
    /// - `store` — Hadrian persists responses to its own DB under its own IDs.
    ///   Forced to `false` (not omitted): OpenAI defaults `store` to `true`, so
    ///   omitting it would make the upstream double-store, and strict backends
    ///   like OpenRouter reject `store: true` outright.
    /// - `background` — Hadrian runs background generation via its own job
    ///   queue (`jobs/background_responses`), never delegated upstream.
    /// - `models` — multi-model routing list, already resolved by the router.
    /// - `provider` — Hadrian provider-routing config.
    /// - `plugins` — Hadrian plugins.
    /// - `sovereignty_requirements` — enforced by gateway middleware.
    /// - `skills` — resolved into `instructions` + sandbox mounts before
    ///   dispatch; the raw refs (incl. inline bundles) must not leak upstream.
    ///
    /// All of these are fully consumed in the route handler before dispatch, so
    /// dropping them here is safe. Provider adapters that build their own
    /// request structs (Anthropic, Bedrock, Vertex) never serialize the raw
    /// payload, so this only matters for the OpenAI-compatible passthrough.
    pub fn strip_gateway_fields(&mut self) {
        self.store = Some(false);
        self.background = None;
        self.models = None;
        self.provider = None;
        self.plugins = None;
        self.sovereignty_requirements = None;
        self.skills = None;
    }

    /// Produce a JSON map of echo fields for streaming response.completed events.
    pub fn echo_fields_json(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();

        // Echo tools with required fields (strict, description) filled in
        let tools_json =
            serde_json::to_value(self.tools.clone().unwrap_or_default()).unwrap_or_default();
        let tools_json = if let serde_json::Value::Array(tools) = tools_json {
            serde_json::Value::Array(
                tools
                    .into_iter()
                    .map(|mut t| {
                        if let serde_json::Value::Object(ref mut obj) = t {
                            let is_function =
                                obj.get("type").and_then(|v| v.as_str()) == Some("function");
                            if is_function {
                                obj.entry("strict").or_insert(serde_json::Value::Null);
                                obj.entry("description").or_insert(serde_json::Value::Null);
                            }
                        }
                        t
                    })
                    .collect(),
            )
        } else {
            tools_json
        };
        m.insert("tools".into(), tools_json);
        m.insert(
            "tool_choice".into(),
            serde_json::to_value(
                self.tool_choice
                    .clone()
                    .unwrap_or(ResponsesToolChoice::String(
                        ResponsesToolChoiceDefault::Auto,
                    )),
            )
            .unwrap_or_default(),
        );
        m.insert(
            "parallel_tool_calls".into(),
            serde_json::Value::Bool(self.parallel_tool_calls.unwrap_or(true)),
        );
        m.insert(
            "temperature".into(),
            serde_json::json!(self.temperature.unwrap_or(1.0)),
        );
        m.insert("top_p".into(), serde_json::json!(self.top_p.unwrap_or(1.0)));
        m.insert(
            "store".into(),
            serde_json::Value::Bool(self.store.unwrap_or(true)),
        );
        m.insert(
            "background".into(),
            serde_json::Value::Bool(self.background.unwrap_or(false)),
        );
        m.insert(
            "truncation".into(),
            serde_json::to_value(
                self.truncation
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<ResponsesTruncation>(v.clone()).ok())
                    .unwrap_or(ResponsesTruncation::Disabled),
            )
            .unwrap_or_default(),
        );
        m.insert(
            "text".into(),
            serde_json::to_value(self.text.clone().unwrap_or(ResponseTextConfig {
                format: Some(ResponseFormatTextConfig::Text),
                verbosity: None,
            }))
            .unwrap_or_default(),
        );
        m.insert(
            "service_tier".into(),
            serde_json::to_value(
                self.service_tier
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<ResponsesServiceTier>(v.clone()).ok())
                    .unwrap_or(ResponsesServiceTier::Default),
            )
            .unwrap_or_default(),
        );
        m.insert(
            "presence_penalty".into(),
            serde_json::json!(self.presence_penalty.unwrap_or(0.0)),
        );
        m.insert(
            "frequency_penalty".into(),
            serde_json::json!(self.frequency_penalty.unwrap_or(0.0)),
        );
        m.insert(
            "instructions".into(),
            self.instructions
                .as_ref()
                .map(|s| serde_json::Value::String(s.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        m.insert(
            "previous_response_id".into(),
            self.previous_response_id
                .as_ref()
                .map(|s| serde_json::Value::String(s.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        m.insert(
            "prompt_cache_key".into(),
            self.prompt_cache_key
                .as_ref()
                .map(|s| serde_json::Value::String(s.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        m.insert(
            "safety_identifier".into(),
            self.safety_identifier
                .as_ref()
                .map(|s| serde_json::Value::String(s.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        m.insert(
            "max_output_tokens".into(),
            self.max_output_tokens
                .map(|v| {
                    if v.fract() == 0.0 {
                        serde_json::json!(v as i64)
                    } else {
                        serde_json::json!(v)
                    }
                })
                .unwrap_or(serde_json::Value::Null),
        );
        m.insert(
            "metadata".into(),
            self.metadata
                .as_ref()
                .map(|md| serde_json::to_value(md).unwrap_or_default())
                .unwrap_or_else(|| serde_json::json!({})),
        );
        m.insert("max_tool_calls".into(), serde_json::Value::Null);
        // top_logprobs is not a request parameter on the Responses API; default to 0 per spec
        m.insert("top_logprobs".into(), serde_json::json!(0));
        // Ensure reasoning is echoed (null if not configured)
        m.insert(
            "reasoning".into(),
            self.reasoning
                .as_ref()
                .map(|r| {
                    serde_json::json!({
                        "effort": r.effort.as_ref().map(|e| serde_json::to_value(e).unwrap_or_default()),
                        "summary": r.summary.as_ref().map(|s| serde_json::to_value(s).unwrap_or_default()),
                    })
                })
                .unwrap_or(serde_json::Value::Null),
        );
        m
    }
}

#[cfg(test)]
mod context_management_tests {
    use super::*;

    #[test]
    fn compaction_round_trip_matches_openai_spec() {
        let raw = serde_json::json!([{"type": "compaction", "compact_threshold": 200000}]);
        let items: Vec<ContextManagementItem> = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            ContextManagementItem::Compaction {
                compact_threshold,
                strategy,
                prompt,
            } => {
                assert_eq!(compact_threshold.unwrap() as i64, 200000);
                assert!(strategy.is_none());
                assert!(prompt.is_none());
            }
            ContextManagementItem::Other => {
                panic!("expected compaction variant, got Other");
            }
        }
        // Re-serialize and confirm the threshold is emitted as an
        // integer (matches OpenAI's wire format, not float) and the
        // Hadrian-extension fields are omitted when absent.
        let round = serde_json::to_value(&items).unwrap();
        assert_eq!(round, raw);
    }

    #[test]
    fn compaction_accepts_hadrian_extension_fields() {
        let raw = serde_json::json!([{
            "type": "compaction",
            "compact_threshold": 8000,
            "strategy": "llm",
            "prompt": "Summarize the conversation so far in <= 200 words."
        }]);
        let items: Vec<ContextManagementItem> = serde_json::from_value(raw).unwrap();
        match &items[0] {
            ContextManagementItem::Compaction {
                compact_threshold,
                strategy,
                prompt,
            } => {
                assert_eq!(compact_threshold.unwrap() as i64, 8000);
                assert_eq!(*strategy, Some(CompactionStrategy::Llm));
                assert!(prompt.as_deref().unwrap().starts_with("Summarize"));
            }
            ContextManagementItem::Other => panic!("expected compaction"),
        }
    }

    #[test]
    fn unknown_context_management_variant_is_accepted() {
        // Forward-compatibility: unknown `type` values deserialize into
        // the catch-all `Other` variant rather than failing the whole
        // request, so OpenAI can add new context-management directives
        // without breaking Hadrian.
        let raw = serde_json::json!([{"type": "summarize", "compact_threshold": 1000}]);
        let parsed: Vec<ContextManagementItem> =
            serde_json::from_value(raw).expect("unknown variants should deserialize to Other");
        assert!(matches!(parsed[0], ContextManagementItem::Other));
    }

    #[test]
    fn strip_gateway_fields_drops_orchestration_fields() {
        let mut payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "model": "openrouter/some-model",
            "store": true,
            "background": true,
            "models": ["a", "b"],
            "provider": {"order": ["x"]},
            "plugins": [],
            "sovereignty_requirements": {},
            "skills": [{"type": "skill_reference", "skill_id": "00000000-0000-0000-0000-000000000000"}],
            "temperature": 0.5
        }))
        .expect("payload parses");

        payload.strip_gateway_fields();

        // store is forced to false (not omitted) so OpenAI's default of true
        // can't cause a double-store and OpenRouter doesn't reject it.
        assert_eq!(payload.store, Some(false));
        assert_eq!(payload.background, None);
        assert!(payload.models.is_none());
        assert!(payload.provider.is_none());
        assert!(payload.plugins.is_none());
        assert!(payload.sovereignty_requirements.is_none());
        assert!(payload.skills.is_none());

        // Non-gateway fields are untouched.
        assert_eq!(payload.model.as_deref(), Some("openrouter/some-model"));
        assert_eq!(payload.temperature, Some(0.5));

        // The serialized upstream body carries store:false and none of the
        // gateway-only keys.
        let body = serde_json::to_value(&payload).unwrap();
        assert_eq!(body["store"], serde_json::json!(false));
        for key in [
            "background",
            "models",
            "provider",
            "plugins",
            "sovereignty_requirements",
            "skills",
        ] {
            assert!(body.get(key).is_none(), "{key} should not be serialized");
        }
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    fn usage(input: i64, output: i64, cost: Option<f64>) -> ResponsesUsage {
        ResponsesUsage {
            input_tokens: input,
            input_tokens_details: ResponsesUsageInputTokensDetails { cached_tokens: 1 },
            output_tokens: output,
            output_tokens_details: ResponsesUsageOutputTokensDetails {
                reasoning_tokens: 2,
            },
            total_tokens: input + output,
            cost,
            is_byok: None,
            cost_details: None,
        }
    }

    #[test]
    fn accumulate_sums_tokens_and_cost() {
        let mut acc = usage(100, 50, Some(0.001));
        acc.accumulate(&usage(200, 30, Some(0.002)));
        assert_eq!(acc.input_tokens, 300);
        assert_eq!(acc.output_tokens, 80);
        assert_eq!(acc.total_tokens, 380);
        assert_eq!(acc.input_tokens_details.cached_tokens, 2);
        assert_eq!(acc.output_tokens_details.reasoning_tokens, 4);
        assert!((acc.cost.unwrap() - 0.003).abs() < 1e-9);
    }

    #[test]
    fn accumulate_takes_present_cost_and_is_sticky_byok() {
        let mut acc = usage(1, 1, None);
        let mut add = usage(1, 1, Some(0.5));
        add.is_byok = Some(true);
        acc.accumulate(&add);
        assert_eq!(acc.cost, Some(0.5));
        assert_eq!(acc.is_byok, Some(true));
        // A later non-BYOK turn does not clear the sticky flag.
        acc.accumulate(&usage(1, 1, None));
        assert_eq!(acc.is_byok, Some(true));
    }

    #[test]
    fn accumulate_merges_cost_details_componentwise() {
        let mut acc = usage(1, 1, None);
        let mut add = usage(1, 1, None);
        add.cost_details = Some(ResponsesUsageCostDetails {
            upstream_inference_cost: Some(0.9),
            upstream_inference_input_cost: 0.4,
            upstream_inference_output_cost: 0.5,
        });
        acc.accumulate(&add);
        acc.accumulate(&add);
        let d = acc.cost_details.unwrap();
        assert!((d.upstream_inference_input_cost - 0.8).abs() < 1e-9);
        assert!((d.upstream_inference_output_cost - 1.0).abs() < 1e-9);
        assert!((d.upstream_inference_cost.unwrap() - 1.8).abs() < 1e-9);
    }
}

#[cfg(test)]
mod mcp_tool_tests {
    use super::*;

    #[test]
    fn mcp_tool_with_server_url_round_trip() {
        let raw = serde_json::json!({
            "type": "mcp",
            "server_label": "atlassian",
            "server_url": "https://mcp.atlassian.com/v1/mcp",
            "authorization": "Bearer ya29.example",
            "require_approval": "always",
            "allowed_tools": ["jira_search", "confluence_get"]
        });
        let tool: McpTool = serde_json::from_value(raw.clone()).expect("parses");
        assert_eq!(tool.server_label, "atlassian");
        assert_eq!(
            tool.server_url.as_deref(),
            Some("https://mcp.atlassian.com/v1/mcp")
        );
        assert!(tool.connector_id.is_none());
        assert_eq!(tool.authorization.as_deref(), Some("Bearer ya29.example"));
        assert!(matches!(
            tool.require_approval,
            Some(McpRequireApproval::Mode(McpApprovalMode::Always))
        ));
        assert!(matches!(
            tool.allowed_tools,
            Some(McpAllowedTools::List(ref v)) if v == &["jira_search".to_string(), "confluence_get".to_string()]
        ));
        assert!(tool.has_exactly_one_target());

        // Round-trip back to JSON.
        let reserialized = serde_json::to_value(&tool).expect("serializes");
        assert_eq!(reserialized["type"], "mcp");
        assert_eq!(
            reserialized["server_url"],
            "https://mcp.atlassian.com/v1/mcp"
        );
    }

    #[test]
    fn mcp_tool_with_connector_id() {
        let raw = serde_json::json!({
            "type": "mcp",
            "server_label": "gcal",
            "connector_id": "connector_googlecalendar",
            "authorization": "Bearer xyz"
        });
        let tool: McpTool = serde_json::from_value(raw).expect("parses");
        assert!(tool.server_url.is_none());
        assert_eq!(
            tool.connector_id.as_deref(),
            Some("connector_googlecalendar")
        );
        assert!(tool.has_exactly_one_target());
    }

    #[test]
    fn mcp_tool_rejects_no_target() {
        // No server_url, no connector_id — accepts at deserialize time, fails
        // the explicit `has_exactly_one_target()` check the preprocess uses.
        let raw = serde_json::json!({"type": "mcp", "server_label": "broken"});
        let tool: McpTool = serde_json::from_value(raw).expect("deserializes");
        assert!(!tool.has_exactly_one_target());
    }

    #[test]
    fn mcp_tool_rejects_both_targets() {
        let raw = serde_json::json!({
            "type": "mcp",
            "server_label": "both",
            "server_url": "https://x",
            "connector_id": "connector_googlecalendar"
        });
        let tool: McpTool = serde_json::from_value(raw).expect("deserializes");
        assert!(!tool.has_exactly_one_target());
    }

    #[test]
    fn require_approval_parses_string_and_object() {
        let s: McpRequireApproval = serde_json::from_value(serde_json::json!("never")).unwrap();
        assert!(matches!(
            s,
            McpRequireApproval::Mode(McpApprovalMode::Never)
        ));

        let o: McpRequireApproval = serde_json::from_value(serde_json::json!({
            "always": {"tool_names": ["a", "b"]},
            "never": {"read_only": true}
        }))
        .unwrap();
        match o {
            McpRequireApproval::Filter(f) => {
                let always = f.always.expect("always filter set");
                assert_eq!(
                    always.tool_names,
                    Some(vec!["a".to_string(), "b".to_string()])
                );
                let never = f.never.expect("never filter set");
                assert_eq!(never.read_only, Some(true));
            }
            other => panic!("expected Filter variant, got {other:?}"),
        }
    }

    #[test]
    fn allowed_tools_parses_list_and_object() {
        let l: McpAllowedTools = serde_json::from_value(serde_json::json!(["x", "y"])).unwrap();
        assert!(matches!(l, McpAllowedTools::List(ref v) if v.len() == 2));

        let o: McpAllowedTools =
            serde_json::from_value(serde_json::json!({"tool_names": ["x"], "read_only": true}))
                .unwrap();
        match o {
            McpAllowedTools::Filter(f) => {
                assert_eq!(f.tool_names, Some(vec!["x".to_string()]));
                assert_eq!(f.read_only, Some(true));
            }
            other => panic!("expected Filter variant, got {other:?}"),
        }
    }

    #[test]
    fn responses_tool_definition_picks_mcp_variant() {
        let raw = serde_json::json!({
            "type": "mcp",
            "server_label": "test",
            "server_url": "https://x"
        });
        let def: ResponsesToolDefinition = serde_json::from_value(raw).expect("parses");
        assert!(def.is_mcp());
        assert!(!def.is_shell());
        assert!(!def.is_function_tool_with_value());
    }

    #[test]
    fn responses_tool_definition_picks_tool_search_variant() {
        let raw = serde_json::json!({
            "type": "tool_search",
            "execution": "server",
            "ranker": "semantic"
        });
        let def: ResponsesToolDefinition = serde_json::from_value(raw).expect("parses");
        assert!(def.is_tool_search());
        let t = def.as_tool_search().expect("tool_search");
        assert_eq!(t.execution, Some(ToolSearchExecution::Server));
        assert_eq!(t.ranker, Some(ToolSearchRankerKind::Semantic));
    }

    #[test]
    fn tool_search_tool_minimal_round_trip() {
        // Only `type` is required per OpenAI's ToolSearchToolParam.
        let raw = serde_json::json!({ "type": "tool_search" });
        let def: ResponsesToolDefinition = serde_json::from_value(raw).expect("parses");
        let t = def.as_tool_search().expect("tool_search");
        assert!(t.execution.is_none());
        assert!(t.ranker.is_none());
        // Round-trips without injecting null fields.
        let back = serde_json::to_value(&def).expect("serializes");
        assert_eq!(back, serde_json::json!({ "type": "tool_search" }));
    }

    #[test]
    fn tool_search_call_and_output_items_round_trip() {
        let call_raw = serde_json::json!({
            "type": "tool_search_call",
            "id": "ts_1",
            "call_id": "call_abc",
            "execution": "server",
            "arguments": {"query": "search jira"},
            "status": "completed"
        });
        let call: ToolSearchCallItem = serde_json::from_value(call_raw.clone()).expect("parses");
        assert_eq!(call.id, "ts_1");
        assert_eq!(call.call_id.as_deref(), Some("call_abc"));
        assert_eq!(call.execution, ToolSearchExecution::Server);
        assert_eq!(serde_json::to_value(&call).unwrap(), call_raw);

        let out_raw = serde_json::json!({
            "type": "tool_search_output",
            "id": "tso_1",
            "call_id": "call_abc",
            "execution": "server",
            "tools": [{"type": "function", "name": "mcp_atlassian__jira_search"}],
            "status": "completed"
        });
        let out: ToolSearchOutputItem = serde_json::from_value(out_raw.clone()).expect("parses");
        assert_eq!(out.tools.len(), 1);
        assert_eq!(serde_json::to_value(&out).unwrap(), out_raw);

        // Both round-trip through the untagged output-item enum.
        let item: ResponsesOutputItem = serde_json::from_value(call_raw).expect("parses");
        assert!(matches!(item, ResponsesOutputItem::ToolSearchCall(_)));
        let item: ResponsesOutputItem = serde_json::from_value(out_raw).expect("parses");
        assert!(matches!(item, ResponsesOutputItem::ToolSearchOutput(_)));
    }

    #[test]
    fn mcp_tool_defer_loading_passthrough_extension_parses() {
        let raw = serde_json::json!({
            "type": "mcp",
            "server_label": "atlassian",
            "server_url": "https://x",
            "defer_loading": true,
            "defer_loading_passthrough": true
        });
        let def: ResponsesToolDefinition = serde_json::from_value(raw).expect("parses");
        let mcp = def.as_mcp().expect("mcp");
        assert_eq!(mcp.defer_loading, Some(true));
        assert_eq!(mcp.defer_loading_passthrough, Some(true));
    }

    #[test]
    fn mcp_list_tools_item_round_trip() {
        let raw = serde_json::json!({
            "type": "mcp_list_tools",
            "id": "mcptl_1",
            "server_label": "atlassian",
            "tools": [{
                "name": "jira_search",
                "description": "Search Jira issues",
                "input_schema": {"type": "object"}
            }]
        });
        let item: McpListToolsItem = serde_json::from_value(raw).expect("parses");
        assert_eq!(item.id, "mcptl_1");
        assert_eq!(item.tools.len(), 1);
        assert_eq!(item.tools[0].name, "jira_search");
        assert!(item.error.is_none());
    }

    #[test]
    fn mcp_list_tools_item_carries_error() {
        let raw = serde_json::json!({
            "type": "mcp_list_tools",
            "id": "mcptl_err",
            "server_label": "atlassian",
            "tools": [],
            "error": "503 from upstream"
        });
        let item: McpListToolsItem = serde_json::from_value(raw).expect("parses");
        assert_eq!(item.error.as_deref(), Some("503 from upstream"));
        assert!(item.tools.is_empty());
    }

    #[test]
    fn mcp_call_inlines_output_and_error() {
        let call_raw = serde_json::json!({
            "type": "mcp_call",
            "id": "mcpc_1",
            "server_label": "atlassian",
            "name": "jira_search",
            "arguments": "{\"query\": \"bugs\"}",
            "status": "completed",
            "output": "{\"issues\": []}",
            "approval_request_id": "mcpr_1"
        });
        let call: McpCallItem = serde_json::from_value(call_raw).expect("parses");
        assert!(matches!(call.status, McpItemStatus::Completed));
        assert_eq!(call.output.as_deref(), Some("{\"issues\": []}"));
        assert!(call.error.is_none());
        assert_eq!(call.approval_request_id.as_deref(), Some("mcpr_1"));

        let failed_raw = serde_json::json!({
            "type": "mcp_call",
            "id": "mcpc_2",
            "server_label": "atlassian",
            "name": "jira_create",
            "arguments": "{}",
            "status": "failed",
            "error": "timeout"
        });
        let failed: McpCallItem = serde_json::from_value(failed_raw).expect("parses");
        assert!(matches!(failed.status, McpItemStatus::Failed));
        assert_eq!(failed.error.as_deref(), Some("timeout"));
        assert!(failed.output.is_none());
    }

    #[test]
    fn mcp_item_status_round_trips_calling() {
        // `calling` is part of the spec enum; make sure the deserializer
        // accepts it round-trip.
        let raw = serde_json::json!({
            "type": "mcp_call",
            "id": "mcpc_x",
            "server_label": "x",
            "name": "y",
            "arguments": "{}",
            "status": "calling"
        });
        let call: McpCallItem = serde_json::from_value(raw).expect("parses");
        assert!(matches!(call.status, McpItemStatus::Calling));
        let v = serde_json::to_value(&call).expect("serializes");
        assert_eq!(v["status"], "calling");
    }

    #[test]
    fn mcp_approval_request_and_response_round_trip() {
        let req_raw = serde_json::json!({
            "type": "mcp_approval_request",
            "id": "mcpr_1",
            "server_label": "atlassian",
            "name": "jira_create",
            "arguments": "{\"summary\":\"bug\"}"
        });
        let req: McpApprovalRequestItem = serde_json::from_value(req_raw).expect("parses");
        assert_eq!(req.id, "mcpr_1");

        let resp_raw = serde_json::json!({
            "type": "mcp_approval_response",
            "approval_request_id": "mcpr_1",
            "approve": false,
            "reason": "policy violation"
        });
        let resp: McpApprovalResponseItem = serde_json::from_value(resp_raw).expect("parses");
        assert_eq!(resp.approval_request_id, "mcpr_1");
        assert!(!resp.approve);
        assert_eq!(resp.reason.as_deref(), Some("policy violation"));
    }

    #[test]
    fn output_item_picks_mcp_variants() {
        let raw = serde_json::json!({
            "type": "mcp_call",
            "id": "mcpc_1",
            "server_label": "x",
            "name": "y",
            "arguments": "{}",
            "status": "in_progress"
        });
        let item: ResponsesOutputItem = serde_json::from_value(raw).expect("parses as output item");
        assert!(matches!(item, ResponsesOutputItem::McpCall(_)));
    }
}

impl ResponsesToolDefinition {
    #[cfg(test)]
    fn is_function_tool_with_value(&self) -> bool {
        matches!(self, ResponsesToolDefinition::Function(_))
    }
}
