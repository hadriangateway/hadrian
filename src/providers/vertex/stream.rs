//! Streaming transformers for converting Vertex SSE to OpenAI and Responses API formats.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use chrono::Utc;
use futures_util::stream::Stream;
use serde::Serialize;

use super::types::VertexGenerateContentResponse;
use crate::config::StreamingBufferConfig;

// ============================================================================
// OpenAI Streaming Types
// ============================================================================

/// OpenAI-compatible streaming chunk
#[derive(Debug, Serialize)]
pub(super) struct OpenAIStreamChunk {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIStreamChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAIStreamUsage>,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAIStreamChoice {
    pub index: i32,
    pub delta: OpenAIDelta,
    /// Required per OpenAI spec (can be null until stream completes)
    pub finish_reason: Option<String>,
    pub logprobs: Option<()>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct OpenAIDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIStreamToolCall>>,
    /// Reasoning/thinking content from extended thinking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAIStreamToolCall {
    pub index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<&'static str>,
    pub function: OpenAIStreamFunction,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAIStreamFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAIStreamUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

// ============================================================================
// Vertex to OpenAI Streaming
// ============================================================================

/// State for tracking the stream transformation
#[derive(Debug)]
pub struct StreamState {
    pub message_id: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Reasoning/thinking token count
    pub reasoning_tokens: i64,
    /// Buffer for incomplete SSE data
    pub buffer: String,
    /// Whether we've sent the initial role delta
    pub sent_role: bool,
    /// Track tool calls by their index
    pub tool_call_count: usize,
    /// Error state for buffer overflow
    pub buffer_overflow: bool,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            message_id: format!("vertex-{}", uuid::Uuid::new_v4()),
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            buffer: String::new(),
            sent_role: false,
            tool_call_count: 0,
            buffer_overflow: false,
        }
    }
}

/// Stream transformer that converts Vertex SSE to OpenAI SSE format
pub struct VertexToOpenAIStream<S> {
    pub inner: S,
    pub state: StreamState,
    /// Output buffer for generated SSE chunks
    pub output_buffer: std::collections::VecDeque<Bytes>,
    /// Maximum input buffer size in bytes
    pub max_input_buffer_bytes: usize,
    /// Maximum output buffer chunks
    pub max_output_buffer_chunks: usize,
}

impl<S> VertexToOpenAIStream<S> {
    pub fn new(inner: S, model: String, streaming_buffer: &StreamingBufferConfig) -> Self {
        Self {
            inner,
            state: StreamState {
                model,
                ..StreamState::default()
            },
            output_buffer: std::collections::VecDeque::new(),
            max_input_buffer_bytes: streaming_buffer.max_input_buffer_bytes,
            max_output_buffer_chunks: streaming_buffer.max_output_buffer_chunks,
        }
    }

    fn created_timestamp() -> i64 {
        Utc::now().timestamp()
    }

    /// Parse a Vertex SSE line and generate OpenAI SSE chunks
    pub fn process_sse_line(&mut self, line: &str) {
        // Vertex SSE format: "data: {json}"
        if let Some(json_str) = line.strip_prefix("data: ") {
            let json_str = json_str.trim();
            if json_str.is_empty() {
                return;
            }

            // Parse Vertex response chunk
            match serde_json::from_str::<VertexGenerateContentResponse>(json_str) {
                Ok(response) => self.handle_response(response),
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse Vertex SSE event: {}, json: {}",
                        e,
                        json_str
                    );
                }
            }
        }
    }

    pub(super) fn handle_response(&mut self, response: VertexGenerateContentResponse) {
        // Send initial role chunk if not sent yet
        if !self.state.sent_role {
            let chunk = OpenAIStreamChunk {
                id: self.state.message_id.clone(),
                object: "chat.completion.chunk",
                created: Self::created_timestamp(),
                model: self.state.model.clone(),
                choices: vec![OpenAIStreamChoice {
                    index: 0,
                    delta: OpenAIDelta {
                        role: Some("assistant"),
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                usage: None,
            };
            self.emit_chunk(&chunk);
            self.state.sent_role = true;
        }

        // Update usage if available
        if let Some(usage) = &response.usage_metadata {
            self.state.input_tokens = usage.prompt_token_count;
            self.state.output_tokens = usage.candidates_token_count;
            self.state.reasoning_tokens = usage.thoughts_token_count;
        }

        // Process candidates
        if let Some(candidate) = response.candidates.first() {
            let mut text_content = Vec::new();
            let mut reasoning_content = Vec::new();
            let mut tool_calls = Vec::new();

            for part in &candidate.content.parts {
                // Handle thinking/reasoning content (thought: true)
                if part.thought {
                    if let Some(text) = part.text.as_ref().filter(|t| !t.is_empty()) {
                        reasoning_content.push(text.clone());
                    }
                    continue;
                }

                // Handle regular text content
                if let Some(text) = part.text.as_ref().filter(|t| !t.is_empty()) {
                    text_content.push(text.clone());
                }
                if let Some(fc) = &part.function_call {
                    let tool_index = self.state.tool_call_count;
                    self.state.tool_call_count += 1;
                    tool_calls.push(OpenAIStreamToolCall {
                        index: tool_index as i32,
                        id: Some(format!("call_{}", uuid::Uuid::new_v4().simple())),
                        type_: Some("function"),
                        function: OpenAIStreamFunction {
                            name: Some(fc.name.clone()),
                            arguments: Some(serde_json::to_string(&fc.args).unwrap_or_default()),
                        },
                    });
                }
            }

            // Emit reasoning content delta if any
            if !reasoning_content.is_empty() {
                let chunk = OpenAIStreamChunk {
                    id: self.state.message_id.clone(),
                    object: "chat.completion.chunk",
                    created: Self::created_timestamp(),
                    model: self.state.model.clone(),
                    choices: vec![OpenAIStreamChoice {
                        index: 0,
                        delta: OpenAIDelta {
                            role: None,
                            content: None,
                            tool_calls: None,
                            reasoning: Some(reasoning_content.join("")),
                        },
                        finish_reason: None,
                        logprobs: None,
                    }],
                    usage: None,
                };
                self.emit_chunk(&chunk);
            }

            // Emit text content delta if any
            if !text_content.is_empty() {
                let chunk = OpenAIStreamChunk {
                    id: self.state.message_id.clone(),
                    object: "chat.completion.chunk",
                    created: Self::created_timestamp(),
                    model: self.state.model.clone(),
                    choices: vec![OpenAIStreamChoice {
                        index: 0,
                        delta: OpenAIDelta {
                            role: None,
                            content: Some(text_content.join("")),
                            tool_calls: None,
                            reasoning: None,
                        },
                        finish_reason: None,
                        logprobs: None,
                    }],
                    usage: None,
                };
                self.emit_chunk(&chunk);
            }

            // Emit tool calls if any
            if !tool_calls.is_empty() {
                let chunk = OpenAIStreamChunk {
                    id: self.state.message_id.clone(),
                    object: "chat.completion.chunk",
                    created: Self::created_timestamp(),
                    model: self.state.model.clone(),
                    choices: vec![OpenAIStreamChoice {
                        index: 0,
                        delta: OpenAIDelta {
                            role: None,
                            content: None,
                            tool_calls: Some(tool_calls),
                            reasoning: None,
                        },
                        finish_reason: None,
                        logprobs: None,
                    }],
                    usage: None,
                };
                self.emit_chunk(&chunk);
            }

            // Check for finish reason
            if let Some(finish_reason) = &candidate.finish_reason {
                let openai_reason = match finish_reason.as_str() {
                    "STOP" => {
                        if self.state.tool_call_count > 0 {
                            "tool_calls"
                        } else {
                            "stop"
                        }
                    }
                    "MAX_TOKENS" => "length",
                    // Safety-related finish reasons -> content_filter
                    "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" => "content_filter",
                    // Non-error completion reasons -> stop
                    "RECITATION" | "OTHER" | "FINISH_REASON_UNSPECIFIED" => "stop",
                    other => other,
                };

                let chunk = OpenAIStreamChunk {
                    id: self.state.message_id.clone(),
                    object: "chat.completion.chunk",
                    created: Self::created_timestamp(),
                    model: self.state.model.clone(),
                    choices: vec![OpenAIStreamChoice {
                        index: 0,
                        delta: OpenAIDelta::default(),
                        finish_reason: Some(openai_reason.to_string()),
                        logprobs: None,
                    }],
                    usage: None,
                };
                self.emit_chunk(&chunk);

                // Emit final chunk with usage
                let usage_chunk = OpenAIStreamChunk {
                    id: self.state.message_id.clone(),
                    object: "chat.completion.chunk",
                    created: Self::created_timestamp(),
                    model: self.state.model.clone(),
                    choices: vec![],
                    usage: Some(OpenAIStreamUsage {
                        prompt_tokens: self.state.input_tokens,
                        completion_tokens: self.state.output_tokens,
                        total_tokens: self.state.input_tokens + self.state.output_tokens,
                    }),
                };
                self.emit_chunk(&usage_chunk);

                // Emit [DONE]
                self.output_buffer
                    .push_back(Bytes::from("data: [DONE]\n\n"));
            }
        }
    }

    fn emit_chunk(&mut self, chunk: &OpenAIStreamChunk) {
        if let Ok(json) = serde_json::to_string(chunk) {
            let sse = format!("data: {}\n\n", json);
            self.output_buffer.push_back(Bytes::from(sse));
        }
    }

    /// Process a chunk of bytes, potentially containing multiple SSE events
    pub fn process_bytes(&mut self, bytes: &[u8]) {
        // Check if we're already in error state
        if self.state.buffer_overflow {
            return;
        }

        if let Ok(text) = std::str::from_utf8(bytes) {
            // Check input buffer limit before adding
            if self.state.buffer.len() + text.len() > self.max_input_buffer_bytes {
                tracing::error!(
                    buffer_size = self.state.buffer.len(),
                    incoming_size = text.len(),
                    max_size = self.max_input_buffer_bytes,
                    "SSE input buffer overflow - possible DoS or malformed response"
                );
                self.state.buffer_overflow = true;
                return;
            }

            self.state.buffer.push_str(text);

            // Process complete lines
            while let Some(pos) = self.state.buffer.find('\n') {
                // Extract and process line, avoiding allocation for empty lines
                let trimmed = self.state.buffer[..pos].trim();
                if !trimmed.is_empty() {
                    // Only allocate when we need to process the line
                    let line = trimmed.to_string();
                    self.process_sse_line(&line);
                }
                // Remove processed bytes in-place (no allocation, reuses buffer capacity)
                self.state.buffer.drain(..=pos);

                // Check output buffer limit
                if self.output_buffer.len() > self.max_output_buffer_chunks {
                    tracing::error!(
                        buffer_size = self.output_buffer.len(),
                        max_size = self.max_output_buffer_chunks,
                        "SSE output buffer overflow - slow consumer or producer overload"
                    );
                    self.state.buffer_overflow = true;
                    return;
                }
            }
        }
    }
}

impl<S> Stream for VertexToOpenAIStream<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.state.buffer_overflow {
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "SSE buffer overflow",
            ))));
        }

        if let Some(out) = self.output_buffer.pop_front() {
            return Poll::Ready(Some(Ok(out)));
        }

        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.process_bytes(&bytes);

                    if self.state.buffer_overflow {
                        return Poll::Ready(Some(Err(io::Error::new(
                            io::ErrorKind::OutOfMemory,
                            "SSE buffer overflow",
                        ))));
                    }

                    if let Some(out) = self.output_buffer.pop_front() {
                        return Poll::Ready(Some(Ok(out)));
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => {
                    return match self.output_buffer.pop_front() {
                        Some(out) => Poll::Ready(Some(Ok(out))),
                        None => Poll::Ready(None),
                    };
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

// ============================================================================
// Vertex to Responses API Streaming
// ============================================================================

/// State for tracking the Vertex to Responses API stream transformation
#[derive(Debug, Default)]
pub(super) struct ResponsesStreamState {
    pub response_id: String,
    pub message_id: String,
    pub reasoning_id: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    /// Accumulated text content
    pub text_content: String,
    /// Accumulated reasoning/thinking content
    pub reasoning_content: String,
    /// Tracks function calls: (call_id, name, arguments)
    pub function_calls: Vec<(String, String, String)>,
    /// Buffer for incomplete SSE data
    pub buffer: String,
    /// Whether we've emitted the response.created event
    pub emitted_response_created: bool,
    /// Whether we've emitted the output_item.added for reasoning
    pub emitted_reasoning_added: bool,
    /// Whether we've emitted the output_item.added for message
    pub emitted_message_added: bool,
    /// Whether we've emitted the content_part.added for text
    pub emitted_content_part_added: bool,
    /// Error state for buffer overflow
    pub buffer_overflow: bool,
    /// Finish reason from Vertex
    pub finish_reason: Option<String>,
    /// Sequence number for Responses API events
    pub sequence_number: i32,
    /// Echo fields from request payload for response.completed
    pub echo_fields: serde_json::Map<String, serde_json::Value>,
}

/// Stream transformer that converts Vertex SSE to OpenAI Responses API SSE format
pub struct VertexToResponsesStream<S> {
    inner: S,
    state: ResponsesStreamState,
    /// Output buffer for generated SSE chunks
    output_buffer: std::collections::VecDeque<Bytes>,
    /// Maximum input buffer size in bytes
    max_input_buffer_bytes: usize,
    /// Maximum output buffer chunks
    max_output_buffer_chunks: usize,
}

impl<S> VertexToResponsesStream<S> {
    pub fn new(
        inner: S,
        model: String,
        streaming_buffer: &StreamingBufferConfig,
        echo_fields: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            inner,
            state: ResponsesStreamState {
                response_id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
                message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                reasoning_id: format!("rs_{}", uuid::Uuid::new_v4().simple()),
                model,
                echo_fields,
                ..ResponsesStreamState::default()
            },
            output_buffer: std::collections::VecDeque::new(),
            max_input_buffer_bytes: streaming_buffer.max_input_buffer_bytes,
            max_output_buffer_chunks: streaming_buffer.max_output_buffer_chunks,
        }
    }

    fn created_timestamp() -> f64 {
        Utc::now().timestamp() as f64
    }

    fn next_sequence(&mut self) -> i32 {
        let seq = self.state.sequence_number;
        self.state.sequence_number += 1;
        seq
    }

    fn build_response_json(
        &self,
        status: &str,
        output: serde_json::Value,
    ) -> serde_json::Map<String, serde_json::Value> {
        crate::api_types::responses::build_streaming_response_json(
            &self.state.response_id,
            &self.state.model,
            Self::created_timestamp(),
            status,
            output,
            &self.state.echo_fields,
        )
    }

    /// Parse a Vertex SSE line and generate Responses API SSE chunks
    fn process_sse_line(&mut self, line: &str) {
        // Vertex SSE format: "data: {json}"
        if let Some(json_str) = line.strip_prefix("data: ") {
            let json_str = json_str.trim();
            if json_str.is_empty() {
                return;
            }

            // Pass through [DONE] marker
            if json_str == "[DONE]" {
                self.output_buffer
                    .push_back(Bytes::from("data: [DONE]\n\n"));
                return;
            }

            // Parse Vertex response chunk
            match serde_json::from_str::<VertexGenerateContentResponse>(json_str) {
                Ok(response) => self.handle_response(response),
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse Vertex SSE event for Responses API: {}, json: {}",
                        e,
                        json_str
                    );
                }
            }
        }
    }

    fn handle_response(&mut self, response: VertexGenerateContentResponse) {
        // Emit response.created on first chunk
        if !self.state.emitted_response_created {
            self.state.emitted_response_created = true;
            let resp = self.build_response_json("in_progress", serde_json::json!([]));
            self.emit_event("response.created", serde_json::json!({ "response": resp }));
        }

        // Update usage if available
        if let Some(usage) = &response.usage_metadata {
            self.state.input_tokens = usage.prompt_token_count;
            self.state.output_tokens = usage.candidates_token_count;
            self.state.reasoning_tokens = usage.thoughts_token_count;
        }

        // Process candidates
        if let Some(candidate) = response.candidates.first() {
            let mut new_text = String::new();
            let mut new_reasoning = String::new();

            for part in &candidate.content.parts {
                // Handle thinking/reasoning content (thought: true)
                if part.thought {
                    if let Some(text) = part.text.as_ref().filter(|t| !t.is_empty()) {
                        new_reasoning.push_str(text);
                    }
                    continue;
                }

                // Handle regular text content
                if let Some(text) = part.text.as_ref().filter(|t| !t.is_empty()) {
                    new_text.push_str(text);
                }

                // Handle function calls
                if let Some(fc) = &part.function_call {
                    let call_id = format!("call_{}", uuid::Uuid::new_v4().simple());
                    let arguments = serde_json::to_string(&fc.args).unwrap_or_default();

                    // Calculate output index (reasoning first, then message, then function calls)
                    let mut output_index = 0;
                    if self.state.emitted_reasoning_added {
                        output_index += 1;
                    }
                    if self.state.emitted_message_added {
                        output_index += 1;
                    }
                    output_index += self.state.function_calls.len();

                    // Emit output_item.added for the function call
                    let fc_id = format!("fc_{}", call_id[5..].chars().take(24).collect::<String>());
                    self.emit_event(
                        "response.output_item.added",
                        serde_json::json!({
                            "output_index": output_index,
                            "item": {
                                "type": "function_call",
                                "id": fc_id,
                                "call_id": call_id.clone(),
                                "name": fc.name,
                                "arguments": "",
                                "status": "in_progress"
                            }
                        }),
                    );

                    // Emit function call arguments (Vertex sends complete arguments at once)
                    self.emit_event(
                        "response.function_call_arguments.delta",
                        serde_json::json!({
                            "item_id": fc_id,
                            "output_index": output_index,
                            "delta": arguments
                        }),
                    );

                    self.state
                        .function_calls
                        .push((call_id, fc.name.clone(), arguments));
                }
            }

            // Emit reasoning content if any (reasoning comes before message in output)
            if !new_reasoning.is_empty() {
                if !self.state.emitted_reasoning_added {
                    self.state.emitted_reasoning_added = true;
                    // Reasoning is always at output_index 0
                    self.emit_event(
                        "response.output_item.added",
                        serde_json::json!({
                            "output_index": 0,
                            "item": {
                                "type": "reasoning",
                                "id": self.state.reasoning_id,
                                "summary": [],
                                "status": "in_progress"
                            }
                        }),
                    );
                }

                // Emit reasoning summary delta
                self.emit_event(
                    "response.reasoning_summary_text.delta",
                    serde_json::json!({
                        "item_id": self.state.reasoning_id,
                        "output_index": 0,
                        "summary_index": 0,
                        "delta": new_reasoning
                    }),
                );

                self.state.reasoning_content.push_str(&new_reasoning);
            }

            // Emit text content if any
            if !new_text.is_empty() {
                // Message output index depends on whether reasoning was emitted
                let message_output_index = if self.state.emitted_reasoning_added {
                    1
                } else {
                    0
                };

                // Emit message/content_part added on first text
                if !self.state.emitted_message_added {
                    self.state.emitted_message_added = true;
                    self.emit_event(
                        "response.output_item.added",
                        serde_json::json!({
                            "output_index": message_output_index,
                            "item": {
                                "type": "message",
                                "id": self.state.message_id,
                                "role": "assistant",
                                "content": [],
                                "status": "in_progress"
                            }
                        }),
                    );
                }

                if !self.state.emitted_content_part_added {
                    self.state.emitted_content_part_added = true;
                    self.emit_event(
                        "response.content_part.added",
                        serde_json::json!({
                            "item_id": self.state.message_id,
                            "output_index": message_output_index,
                            "content_index": 0,
                            "part": {
                                "type": "output_text",
                                "text": "",
                                "annotations": [],
                                "logprobs": []
                            }
                        }),
                    );
                }

                // Emit text delta
                self.emit_event(
                    "response.output_text.delta",
                    serde_json::json!({
                        "item_id": self.state.message_id,
                        "output_index": message_output_index,
                        "content_index": 0,
                        "delta": new_text
                    }),
                );

                self.state.text_content.push_str(&new_text);
            }

            // Check for finish reason
            if let Some(finish_reason) = &candidate.finish_reason {
                self.state.finish_reason = Some(finish_reason.clone());
                self.emit_completion_events();
            }
        }
    }

    fn emit_completion_events(&mut self) {
        // Calculate output indices based on what was emitted
        let message_output_index = if self.state.emitted_reasoning_added {
            1
        } else {
            0
        };

        // Emit reasoning done first if we have reasoning content (output_index 0)
        if !self.state.reasoning_content.is_empty() {
            // Emit reasoning summary text done
            self.emit_event(
                "response.reasoning_summary_text.done",
                serde_json::json!({
                    "item_id": self.state.reasoning_id,
                    "output_index": 0,
                    "summary_index": 0,
                    "text": self.state.reasoning_content
                }),
            );

            // Emit output_item.done for reasoning
            self.emit_event(
                "response.output_item.done",
                serde_json::json!({
                    "output_index": 0,
                    "item": {
                        "type": "reasoning",
                        "id": self.state.reasoning_id,
                        "summary": [{
                            "type": "summary_text",
                            "text": self.state.reasoning_content
                        }],
                        "status": "completed"
                    }
                }),
            );
        }

        // Emit text done if we have text content
        if !self.state.text_content.is_empty() {
            self.emit_event(
                "response.output_text.done",
                serde_json::json!({
                    "item_id": self.state.message_id,
                    "output_index": message_output_index,
                    "content_index": 0,
                    "text": self.state.text_content
                }),
            );

            // Emit content_part.done
            self.emit_event(
                "response.content_part.done",
                serde_json::json!({
                    "item_id": self.state.message_id,
                    "output_index": message_output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": self.state.text_content,
                        "annotations": [],
                        "logprobs": []
                    }
                }),
            );

            // Emit output_item.done for message
            self.emit_event(
                "response.output_item.done",
                serde_json::json!({
                    "output_index": message_output_index,
                    "item": {
                        "type": "message",
                        "id": self.state.message_id,
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": self.state.text_content,
                            "annotations": [],
                            "logprobs": []
                        }]
                    }
                }),
            );
        }

        // Emit completion events for function calls
        // Clone the data to avoid borrow issues
        let function_calls: Vec<_> = self
            .state
            .function_calls
            .iter()
            .enumerate()
            .map(|(i, (call_id, name, arguments))| {
                (i, call_id.clone(), name.clone(), arguments.clone())
            })
            .collect();

        for (i, call_id, name, arguments) in function_calls {
            // Function call index: after reasoning (if any) and message (if any)
            let mut output_index = i;
            if self.state.emitted_reasoning_added {
                output_index += 1;
            }
            if self.state.emitted_message_added {
                output_index += 1;
            }

            let fc_id = format!("fc_{}", call_id[5..].chars().take(24).collect::<String>());

            self.emit_event(
                "response.function_call_arguments.done",
                serde_json::json!({
                    "item_id": fc_id,
                    "output_index": output_index,
                    "arguments": arguments
                }),
            );

            self.emit_event(
                "response.output_item.done",
                serde_json::json!({
                    "output_index": output_index,
                    "item": {
                        "type": "function_call",
                        "id": fc_id,
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments,
                        "status": "completed"
                    }
                }),
            );
        }

        // Build final output array (reasoning first, then message, then function calls)
        let mut output = Vec::new();

        // Reasoning comes first (output_index 0)
        if self.state.emitted_reasoning_added {
            output.push(serde_json::json!({
                "type": "reasoning",
                "id": self.state.reasoning_id,
                "summary": [{
                    "type": "summary_text",
                    "text": self.state.reasoning_content
                }],
                "status": "completed"
            }));
        }

        // Message comes next
        if self.state.emitted_message_added {
            output.push(serde_json::json!({
                "type": "message",
                "id": self.state.message_id,
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": self.state.text_content,
                    "annotations": [],
                    "logprobs": []
                }]
            }));
        }

        // Function calls last
        for (call_id, name, arguments) in &self.state.function_calls {
            let fc_id = format!("fc_{}", call_id[5..].chars().take(24).collect::<String>());
            output.push(serde_json::json!({
                "type": "function_call",
                "id": fc_id,
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
                "status": "completed"
            }));
        }

        // Determine status
        let status = match self.state.finish_reason.as_deref() {
            Some("MAX_TOKENS") => "incomplete",
            // Safety-related finish reasons -> failed
            Some("SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII") => "failed",
            _ => "completed",
        };

        // Build response object with echo fields
        let mut response_obj = self.build_response_json(status, serde_json::json!(output));
        response_obj.insert(
            "usage".into(),
            serde_json::json!({
                "input_tokens": self.state.input_tokens,
                "input_tokens_details": { "cached_tokens": 0 },
                "output_tokens": self.state.output_tokens,
                "output_tokens_details": { "reasoning_tokens": self.state.reasoning_tokens },
                "total_tokens": self.state.input_tokens + self.state.output_tokens
            }),
        );
        if status == "completed" {
            response_obj.insert(
                "completed_at".into(),
                serde_json::json!(Self::created_timestamp()),
            );
        }

        // Emit response.completed with reasoning tokens in usage
        self.emit_event(
            "response.completed",
            serde_json::json!({ "response": response_obj }),
        );
    }

    fn emit_event(&mut self, event_type: &str, data: serde_json::Value) {
        let seq = self.next_sequence();
        let event = serde_json::json!({
            "type": event_type,
            "sequence_number": seq,
        });
        // Merge the data into the event
        let mut event_obj = event.as_object().unwrap().clone();
        if let serde_json::Value::Object(data_obj) = data {
            for (k, v) in data_obj {
                event_obj.insert(k, v);
            }
        }
        // Add logprobs for text-related events (required per OpenAI spec)
        if (event_type == "response.output_text.delta" || event_type == "response.output_text.done")
            && !event_obj.contains_key("logprobs")
        {
            event_obj.insert("logprobs".to_string(), serde_json::Value::Array(vec![]));
        }
        if let Ok(json) = serde_json::to_string(&serde_json::Value::Object(event_obj)) {
            let sse = format!("data: {}\n\n", json);
            self.output_buffer.push_back(Bytes::from(sse));
        }
    }

    /// Process a chunk of bytes, potentially containing multiple SSE events
    fn process_bytes(&mut self, bytes: &[u8]) {
        // Check if we're already in error state
        if self.state.buffer_overflow {
            return;
        }

        if let Ok(text) = std::str::from_utf8(bytes) {
            // Check input buffer limit before adding
            if self.state.buffer.len() + text.len() > self.max_input_buffer_bytes {
                tracing::error!(
                    buffer_size = self.state.buffer.len(),
                    incoming_size = text.len(),
                    max_size = self.max_input_buffer_bytes,
                    "Vertex Responses API SSE input buffer overflow"
                );
                self.state.buffer_overflow = true;
                return;
            }

            self.state.buffer.push_str(text);

            // Process complete lines
            while let Some(pos) = self.state.buffer.find('\n') {
                let trimmed = self.state.buffer[..pos].trim();
                if !trimmed.is_empty() {
                    let line = trimmed.to_string();
                    self.process_sse_line(&line);
                }
                self.state.buffer.drain(..=pos);

                // Check output buffer limit
                if self.output_buffer.len() > self.max_output_buffer_chunks {
                    tracing::error!(
                        buffer_size = self.output_buffer.len(),
                        max_size = self.max_output_buffer_chunks,
                        "Vertex Responses API SSE output buffer overflow"
                    );
                    self.state.buffer_overflow = true;
                    return;
                }
            }
        }
    }
}

impl<S> Stream for VertexToResponsesStream<S>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check for buffer overflow error
        if self.state.buffer_overflow {
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "SSE buffer overflow",
            ))));
        }

        // First, return any buffered output
        if !self.output_buffer.is_empty() {
            return Poll::Ready(Some(Ok(self
                .output_buffer
                .pop_front()
                .expect("non-empty checked above"))));
        }

        // Poll the inner stream
        let inner = Pin::new(&mut self.inner);
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                // Process the Vertex SSE bytes
                self.process_bytes(&bytes);

                // Check for buffer overflow after processing
                if self.state.buffer_overflow {
                    return Poll::Ready(Some(Err(io::Error::new(
                        io::ErrorKind::OutOfMemory,
                        "SSE buffer overflow",
                    ))));
                }

                // Return buffered output or wake for more
                if !self.output_buffer.is_empty() {
                    Poll::Ready(Some(Ok(self
                        .output_buffer
                        .pop_front()
                        .expect("non-empty checked above"))))
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                // Stream ended - flush any remaining buffer
                if !self.output_buffer.is_empty() {
                    Poll::Ready(Some(Ok(self
                        .output_buffer
                        .pop_front()
                        .expect("non-empty checked above"))))
                } else {
                    Poll::Ready(None)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_delta_with_reasoning() {
        let delta = OpenAIDelta {
            role: None,
            content: None,
            tool_calls: None,
            reasoning: Some("thinking content".to_string()),
        };

        let json = serde_json::to_string(&delta).unwrap();
        assert!(json.contains(r#""reasoning":"thinking content""#));
        assert!(!json.contains(r#""content""#)); // content is None, should be skipped
    }

    #[test]
    fn test_openai_delta_without_reasoning() {
        let delta = OpenAIDelta {
            role: Some("assistant"),
            content: Some("Hello".to_string()),
            tool_calls: None,
            reasoning: None,
        };

        let json = serde_json::to_string(&delta).unwrap();
        assert!(json.contains(r#""content":"Hello""#));
        assert!(!json.contains(r#"reasoning"#)); // reasoning is None, should be skipped
    }

    #[test]
    fn test_stream_state_default() {
        let state = StreamState::default();
        assert_eq!(state.input_tokens, 0);
        assert_eq!(state.output_tokens, 0);
        assert_eq!(state.reasoning_tokens, 0);
        assert!(!state.sent_role);
        assert_eq!(state.tool_call_count, 0);
    }
}
