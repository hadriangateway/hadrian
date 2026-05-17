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
        /// Byte offset where the cited range begins. Phase 1 emits `0`
        /// because we don't attempt to parse model output for filename
        /// mentions; clients should render the annotation as a
        /// whole-message reference.
        #[serde(default)]
        start_index: u64,
        /// Byte offset where the cited range ends. Same caveat as
        /// `start_index`.
        #[serde(default)]
        end_index: u64,
        /// Optional single-position index. Phase 1 emits `null`.
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
pub enum ShellCallOutputType {
    ShellCall,
}

/// Output item for a `shell` tool call.
///
/// Emitted as the final visible record of a shell execution. The
/// per-command output chunks flow as `response.shell_call.output_chunk`
/// streaming events; this struct is the post-completion summary the
/// model and the client both retain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCallOutput {
    #[serde(rename = "type")]
    pub type_: ShellCallOutputType,
    /// Call ID issued by the model for this shell tool call.
    pub id: String,
    /// The command that was executed.
    pub command: String,
    /// Exit code returned by the command.
    pub exit_code: i32,
    /// Final status — typically `completed` or `failed`.
    pub status: WebSearchStatus,
    /// Truncated stdout for the model's context. The full stream lives
    /// in the event log.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// Truncated stderr for the model's context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// **Hadrian Extension:** Files written to `/mnt/data` during this
    /// command. Populated when the configured shell runtime supports
    /// `file_io` and `[features.containers]` is enabled. Each entry's
    /// `file_id` matches a `container_file_citation` annotation on the
    /// assistant's reply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_files: Vec<ContainerFileRef>,
}

/// Reference to one file produced or modified by a shell command.
///
/// Phase 1 stores the bytes in process memory keyed by `file_id`; Phase
/// 3 will back the same shape with the `container_files` table so the
/// `GET /v1/containers/{container_id}/files/{file_id}/content` endpoint
/// can serve them.
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
    /// the request. Reserved for Phase 2; Phase 1 only emits
    /// `Assistant` references.
    User,
    /// File was written by the model during a shell command.
    Assistant,
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
    ShellCall(ShellCallOutput),
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
    ShellCall(ShellCallOutput),
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
#[serde(rename_all = "snake_case")]
pub enum WebSearchPreviewToolType {
    WebSearchPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchPreviewTool {
    #[serde(rename = "type")]
    pub type_: WebSearchPreviewToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebSearchPreview20250311ToolType {
    #[serde(rename = "web_search_preview_2025_03_11")]
    WebSearchPreview20250311,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchPreview20250311Tool {
    #[serde(rename = "type")]
    pub type_: WebSearchPreview20250311ToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchToolType {
    WebSearch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchTool {
    #[serde(rename = "type")]
    pub type_: WebSearchToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<WebSearchFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebSearch20250826ToolType {
    #[serde(rename = "web_search_2025_08_26")]
    WebSearch20250826,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearch20250826Tool {
    #[serde(rename = "type")]
    pub type_: WebSearch20250826ToolType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<WebSearchFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<ResponsesSearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<WebSearchUserLocation>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ─────────────────────────────────────────────────────────────────────────────
// File Search Tool (for Responses API RAG)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
pub struct FileSearchTool {
    #[serde(rename = "type")]
    pub type_: FileSearchToolType,
    /// Vector store IDs to search across.
    pub vector_store_ids: Vec<String>,
    /// Maximum number of results to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_num_results: Option<usize>,
    /// Ranking options for controlling result relevance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking_options: Option<FileSearchRankingOptions>,
    /// Metadata filters to apply to the search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<FileSearchFilter>,
    /// **Hadrian Extension:** Cache control for prompt caching (Anthropic/Bedrock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl FileSearchTool {
    /// Check if this is a file_search tool.
    pub fn is_file_search(&self) -> bool {
        matches!(self.type_, FileSearchToolType::FileSearch)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellToolType {
    Shell,
}

/// Shell tool — instructs the model that it may call `shell` and the
/// gateway will execute the resulting commands in a sandboxed runtime
/// (or forward them to the upstream provider's hosted runtime if
/// configured for passthrough).
///
/// The exact spec mirrors OpenAI's `shell` tool definition for
/// GPT-5.2+. Hadrian extends it with optional `environment` overrides
/// the admin may set per-request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellTool {
    #[serde(rename = "type")]
    pub type_: ShellToolType,
    /// **Hadrian Extension:** Optional runtime-environment hints the
    /// model can see. Roughly mirrors OpenAI's `environment` field but
    /// admins can pre-seed values per-request without exposing them as
    /// real env vars to the sandbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<serde_json::Value>,
}

impl ShellTool {
    pub fn is_shell(&self) -> bool {
        matches!(self.type_, ShellToolType::Shell)
    }
}

/// Tool definition - can be a function tool, web search tool, file search tool,
/// or shell tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesToolDefinition {
    FileSearch(FileSearchTool), // Must be before Function to match type field first
    WebSearchPreview(WebSearchPreviewTool),
    WebSearchPreview20250311(WebSearchPreview20250311Tool),
    WebSearch(WebSearchTool),
    WebSearch20250826(WebSearch20250826Tool),
    Shell(ShellTool),
    Function(serde_json::Value), // Must be last - matches any JSON object
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesToolChoice {
    String(ResponsesToolChoiceDefault),
    Named(ResponsesNamedToolChoice),
    WebSearch(ResponsesWebSearchToolChoice),
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
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<Object>))]
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

    /// **Hadrian Extension:** Skill bundle IDs to mount into the
    /// session for tools that support skill mounting (e.g. the shell
    /// runtime). Each entry is a skill UUID owned by the caller's
    /// organization; unknown IDs fail the request with 400.
    ///
    /// Each skill's `SKILL.md` is prepended to `instructions` so the
    /// model knows the skill is available; all skill files are
    /// materialized under `/skills/<skill_id>/` inside the sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,

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
    Compaction {
        /// Token count that triggers a compaction pass. Provider
        /// validates the range.
        #[serde(
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_as_integer"
        )]
        compact_threshold: Option<f64>,
    },
    /// **Hadrian Extension:** Forward-compatibility catch-all for
    /// `type` values Hadrian doesn't know about. The raw value is
    /// preserved and forwarded to the upstream provider unchanged.
    #[serde(other)]
    Other,
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
            ContextManagementItem::Compaction { compact_threshold } => {
                assert_eq!(compact_threshold.unwrap() as i64, 200000);
            }
            ContextManagementItem::Other => {
                panic!("expected compaction variant, got Other");
            }
        }
        // Re-serialize and confirm the threshold is emitted as an
        // integer (matches OpenAI's wire format, not float).
        let round = serde_json::to_value(&items).unwrap();
        assert_eq!(round, raw);
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
}
