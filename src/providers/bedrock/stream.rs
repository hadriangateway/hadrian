//! Stream transformation for Bedrock event streams.
//!
//! This module converts Bedrock's AWS event stream format to OpenAI's SSE format.
//! The Bedrock event stream uses the AWS event stream protocol (binary framing with headers),
//! while OpenAI uses Server-Sent Events (text-based "data: {...}\n\n" format).

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};
use bytes::Bytes;
use chrono::Utc;
use futures_util::stream::Stream;

use super::types::*;
use crate::config::StreamingBufferConfig;

/// Append `delta` to `buf` up to `max_bytes` total. Slices on a UTF-8
/// character boundary so the buffer remains valid UTF-8. Once the cap is hit
/// further deltas are dropped from in-memory state — pass-through SSE chunks
/// to the client are unaffected.
fn bounded_push(buf: &mut String, delta: &str, max_bytes: usize) {
    if buf.len() >= max_bytes {
        return;
    }
    let remaining = max_bytes - buf.len();
    if delta.len() <= remaining {
        buf.push_str(delta);
        return;
    }
    let mut end = remaining;
    while end > 0 && !delta.is_char_boundary(end) {
        end -= 1;
    }
    buf.push_str(&delta[..end]);
}

/// Stream state for tracking the transformation
#[derive(Debug, Default)]
pub(super) struct StreamState {
    pub message_id: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Cached tokens read from prompt cache
    pub cache_read_input_tokens: i64,
    /// Buffer for incomplete event stream data
    pub buffer: bytes::BytesMut,
    /// Event stream frame decoder
    pub decoder: MessageFrameDecoder,
    /// Whether we've sent the initial role delta
    pub sent_role: bool,
    /// Track tool calls by their index (block_index -> (tool_id, tool_name))
    pub tool_calls: std::collections::HashMap<i32, (String, String)>,
    /// Count of tool calls for OpenAI indexing
    pub tool_call_count: i32,
    /// Track reasoning/thinking content blocks by their Bedrock content block index
    pub reasoning_block_indices: Vec<i32>,
    /// Error state for buffer overflow
    pub buffer_overflow: bool,
}

/// Stream transformer that converts Bedrock event stream to OpenAI SSE format
pub(super) struct BedrockToOpenAIStream<S> {
    pub inner: S,
    pub state: StreamState,
    /// Output buffer for generated SSE chunks
    pub output_buffer: std::collections::VecDeque<Bytes>,
    /// Maximum input buffer size in bytes
    pub max_input_buffer_bytes: usize,
    /// Maximum output buffer chunks
    pub max_output_buffer_chunks: usize,
}

impl<S> BedrockToOpenAIStream<S> {
    pub fn new(inner: S, model: String, streaming_buffer: &StreamingBufferConfig) -> Self {
        Self {
            inner,
            state: StreamState {
                message_id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
                model,
                decoder: MessageFrameDecoder::new(),
                buffer: bytes::BytesMut::new(),
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

    /// Process a decoded event stream message
    pub fn process_event(&mut self, message: aws_smithy_types::event_stream::Message) {
        // Get the event type from headers
        let event_type = message.headers().iter().find_map(|h| {
            if h.name().as_str() == ":event-type" {
                h.value().as_string().ok().map(|s| s.as_str().to_string())
            } else {
                None
            }
        });

        // Check for exception
        let message_type = message.headers().iter().find_map(|h| {
            if h.name().as_str() == ":message-type" {
                h.value().as_string().ok().map(|s| s.as_str().to_string())
            } else {
                None
            }
        });

        if message_type.as_deref() == Some("exception") {
            // Handle exception - log and emit error
            let exception_type = message.headers().iter().find_map(|h| {
                if h.name().as_str() == ":exception-type" {
                    h.value().as_string().ok().map(|s| s.as_str().to_string())
                } else {
                    None
                }
            });
            tracing::error!(
                exception_type = ?exception_type,
                payload = ?String::from_utf8_lossy(message.payload()),
                "Bedrock stream exception"
            );
            return;
        }

        let Some(event_type) = event_type else {
            return;
        };

        let payload = message.payload();

        match event_type.as_str() {
            "messageStart" => {
                if let Ok(_start) = serde_json::from_slice::<BedrockMessageStart>(payload) {
                    // Send initial role chunk
                    // Note: Bedrock always returns "assistant" role, but we always emit "assistant"
                    // regardless of the actual role to match OpenAI streaming format
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
                                    reasoning: None,
                                    tool_calls: None,
                                },
                                finish_reason: None,
                                logprobs: None,
                            }],
                            usage: None,
                        };
                        self.emit_chunk(&chunk);
                        self.state.sent_role = true;
                    }
                }
            }
            "contentBlockStart" => {
                if let Ok(start) = serde_json::from_slice::<BedrockContentBlockStart>(payload) {
                    // Check if this is a tool use block
                    if let Some(start_data) = &start.start
                        && let Some(tool_use) = &start_data.tool_use
                    {
                        let openai_index = self.state.tool_call_count;
                        self.state.tool_call_count += 1;
                        self.state.tool_calls.insert(
                            start.content_block_index,
                            (tool_use.tool_use_id.clone(), tool_use.name.clone()),
                        );

                        // Emit tool call start
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
                                    reasoning: None,
                                    tool_calls: Some(vec![OpenAIStreamToolCall {
                                        index: openai_index,
                                        id: Some(tool_use.tool_use_id.clone()),
                                        type_: Some("function"),
                                        function: OpenAIStreamFunction {
                                            name: Some(tool_use.name.clone()),
                                            arguments: None,
                                        },
                                    }]),
                                },
                                finish_reason: None,
                                logprobs: None,
                            }],
                            usage: None,
                        };
                        self.emit_chunk(&chunk);
                    }
                    // Check if this is a reasoning content block (extended thinking)
                    else if let Some(start_data) = &start.start
                        && start_data.reasoning_content.is_some()
                    {
                        // Track this as a reasoning block for later delta handling
                        self.state
                            .reasoning_block_indices
                            .push(start.content_block_index);
                    }
                }
            }
            "contentBlockDelta" => {
                if let Ok(delta) = serde_json::from_slice::<BedrockContentBlockDelta>(payload) {
                    // Reasoning content delta (extended thinking)
                    // Check this first since reasoning blocks come before text
                    if let Some(reasoning) = &delta.delta.reasoning_content
                        && self
                            .state
                            .reasoning_block_indices
                            .contains(&delta.content_block_index)
                    {
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
                                    reasoning: Some(reasoning.text.clone()),
                                    tool_calls: None,
                                },
                                finish_reason: None,
                                logprobs: None,
                            }],
                            usage: None,
                        };
                        self.emit_chunk(&chunk);
                    }
                    // Text delta
                    else if let Some(text) = delta.delta.text
                        && !text.is_empty()
                    {
                        let chunk = OpenAIStreamChunk {
                            id: self.state.message_id.clone(),
                            object: "chat.completion.chunk",
                            created: Self::created_timestamp(),
                            model: self.state.model.clone(),
                            choices: vec![OpenAIStreamChoice {
                                index: 0,
                                delta: OpenAIDelta {
                                    role: None,
                                    content: Some(text),
                                    reasoning: None,
                                    tool_calls: None,
                                },
                                finish_reason: None,
                                logprobs: None,
                            }],
                            usage: None,
                        };
                        self.emit_chunk(&chunk);
                    }
                    // Tool use delta (partial JSON arguments)
                    else if let Some(tool_delta) = delta.delta.tool_use {
                        // Find the OpenAI index for this block
                        if let Some((tool_id, _)) =
                            self.state.tool_calls.get(&delta.content_block_index)
                        {
                            // Find the OpenAI index by counting tool calls before this one
                            let openai_index = self
                                .state
                                .tool_calls
                                .keys()
                                .filter(|&&k| k < delta.content_block_index)
                                .count() as i32;

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
                                        reasoning: None,
                                        tool_calls: Some(vec![OpenAIStreamToolCall {
                                            index: openai_index,
                                            id: Some(tool_id.clone()),
                                            type_: None,
                                            function: OpenAIStreamFunction {
                                                name: None,
                                                arguments: Some(tool_delta.input),
                                            },
                                        }]),
                                    },
                                    finish_reason: None,
                                    logprobs: None,
                                }],
                                usage: None,
                            };
                            self.emit_chunk(&chunk);
                        }
                    }
                }
            }
            "contentBlockStop" => {
                // We don't need to emit anything for content block stop
                let _ = serde_json::from_slice::<BedrockContentBlockStop>(payload);
            }
            "messageStop" => {
                if let Ok(stop) = serde_json::from_slice::<BedrockMessageStop>(payload) {
                    let finish_reason = match stop.stop_reason.as_str() {
                        "end_turn" => "stop",
                        "max_tokens" => "length",
                        "stop_sequence" => "stop",
                        "tool_use" => "tool_calls",
                        "guardrail_intervened" => "content_filter",
                        "content_filtered" => "content_filter",
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
                            finish_reason: Some(finish_reason.to_string()),
                            logprobs: None,
                        }],
                        usage: None,
                    };
                    self.emit_chunk(&chunk);
                }
            }
            "metadata" => {
                if let Ok(metadata) = serde_json::from_slice::<BedrockMetadata>(payload) {
                    self.state.input_tokens = metadata.usage.input_tokens;
                    self.state.output_tokens = metadata.usage.output_tokens;
                    self.state.cache_read_input_tokens = metadata.usage.cache_read_input_tokens;

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
                            prompt_tokens_details: if self.state.cache_read_input_tokens > 0 {
                                Some(StreamPromptTokensDetails {
                                    cached_tokens: self.state.cache_read_input_tokens,
                                })
                            } else {
                                None
                            },
                        }),
                    };
                    self.emit_chunk(&usage_chunk);

                    // Emit [DONE]
                    self.output_buffer
                        .push_back(Bytes::from("data: [DONE]\n\n"));
                }
            }
            _ => {
                tracing::debug!(event_type = %event_type, "Unknown Bedrock stream event");
            }
        }
    }

    pub fn emit_chunk(&mut self, chunk: &OpenAIStreamChunk) {
        if let Ok(json) = serde_json::to_string(chunk) {
            let sse = format!("data: {}\n\n", json);
            self.output_buffer.push_back(Bytes::from(sse));
        }
    }

    /// Process incoming bytes, potentially containing multiple event stream frames
    pub fn process_bytes(&mut self, bytes: &[u8]) {
        // Check if we're already in error state
        if self.state.buffer_overflow {
            return;
        }

        // Check input buffer limit before adding
        if self.state.buffer.len() + bytes.len() > self.max_input_buffer_bytes {
            tracing::error!(
                buffer_size = self.state.buffer.len(),
                incoming_size = bytes.len(),
                max_size = self.max_input_buffer_bytes,
                "Event stream input buffer overflow - possible DoS or malformed response"
            );
            self.state.buffer_overflow = true;
            return;
        }

        self.state.buffer.extend_from_slice(bytes);

        // Try to decode frames from the buffer
        loop {
            match self.state.decoder.decode_frame(&mut self.state.buffer) {
                Ok(DecodedFrame::Complete(message)) => {
                    self.process_event(message);

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
                Ok(DecodedFrame::Incomplete) => {
                    // Need more data
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to decode Bedrock event stream frame");
                    break;
                }
            }
        }
    }
}

impl<S> Stream for BedrockToOpenAIStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.state.buffer_overflow {
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Event stream buffer overflow",
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
                            "Event stream buffer overflow",
                        ))));
                    }

                    if let Some(out) = self.output_buffer.pop_front() {
                        return Poll::Ready(Some(Ok(out)));
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(io::Error::other(e)))),
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
// Responses API Stream Transformer
// ============================================================================

/// State for tracking the Responses API stream transformation
#[derive(Debug, Default)]
pub(super) struct ResponsesStreamState {
    pub response_id: String,
    pub message_id: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Cached tokens read from prompt cache
    pub cache_read_input_tokens: i64,
    /// Accumulated text content
    pub text_content: String,
    /// Tracks tool calls: (block_index, tool_id, tool_name, arguments)
    pub tool_calls: Vec<(i32, String, String, String)>,
    /// Buffer for incomplete event stream data
    pub buffer: bytes::BytesMut,
    /// Event stream frame decoder
    pub decoder: MessageFrameDecoder,
    /// Whether we've emitted the response.created event
    pub emitted_response_created: bool,
    /// Whether we've emitted the output_item.added for message
    pub emitted_message_added: bool,
    /// Whether we've emitted the content_part.added for text
    pub emitted_content_part_added: bool,
    /// Error state for buffer overflow
    pub buffer_overflow: bool,
    /// Stop reason from Bedrock
    pub stop_reason: Option<String>,
    /// Sequence number for events
    pub sequence_number: i32,
    /// Track reasoning/thinking content blocks by their Bedrock content block index
    pub reasoning_block_indices: Vec<i32>,
    /// Accumulated reasoning content
    pub reasoning_content: String,
    /// Accumulated reasoning signature for multi-turn verification
    pub reasoning_signature: String,
    /// ID for the reasoning output item
    pub reasoning_id: String,
    /// Whether we've emitted the output_item.added for reasoning
    pub emitted_reasoning_added: bool,
    /// Echo fields from request payload for response.completed
    pub echo_fields: serde_json::Map<String, serde_json::Value>,
}

/// Stream transformer that converts Bedrock event stream to OpenAI Responses API SSE format
pub struct BedrockToResponsesStream<S> {
    pub inner: S,
    pub state: ResponsesStreamState,
    /// Output buffer for generated SSE chunks
    pub output_buffer: std::collections::VecDeque<Bytes>,
    /// Maximum input buffer size in bytes
    pub max_input_buffer_bytes: usize,
    /// Maximum output buffer chunks
    pub max_output_buffer_chunks: usize,
    /// Maximum total bytes of accumulated text+reasoning state
    pub max_response_state_bytes: usize,
}

impl<S> BedrockToResponsesStream<S> {
    pub fn new(
        inner: S,
        model: String,
        streaming_buffer: &StreamingBufferConfig,
        echo_fields: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        let response_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
        let message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        // Generate reasoning ID from response ID (similar to Anthropic pattern)
        let reasoning_id = format!(
            "rs_{}",
            response_id[5..].chars().take(24).collect::<String>()
        );

        Self {
            inner,
            state: ResponsesStreamState {
                response_id,
                message_id,
                reasoning_id,
                model,
                decoder: MessageFrameDecoder::new(),
                buffer: bytes::BytesMut::new(),
                echo_fields,
                ..ResponsesStreamState::default()
            },
            output_buffer: std::collections::VecDeque::new(),
            max_input_buffer_bytes: streaming_buffer.max_input_buffer_bytes,
            max_output_buffer_chunks: streaming_buffer.max_output_buffer_chunks,
            max_response_state_bytes: streaming_buffer.max_response_state_bytes,
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

    /// Calculate the output index for message (accounting for reasoning if present)
    fn message_output_index(&self) -> usize {
        if self.state.emitted_reasoning_added {
            1 // Message comes after reasoning
        } else {
            0
        }
    }

    /// Calculate the output index for a tool call (accounting for reasoning and message)
    fn tool_output_index(&self, tool_index: usize) -> usize {
        let mut base = tool_index;
        if self.state.emitted_reasoning_added {
            base += 1;
        }
        if self.state.emitted_message_added {
            base += 1;
        }
        base
    }

    /// Process a decoded event stream message for Responses API
    pub fn process_event(&mut self, message: aws_smithy_types::event_stream::Message) {
        // Get the event type from headers
        let event_type = message.headers().iter().find_map(|h| {
            if h.name().as_str() == ":event-type" {
                h.value().as_string().ok().map(|s| s.as_str().to_string())
            } else {
                None
            }
        });

        // Check for exception
        let message_type = message.headers().iter().find_map(|h| {
            if h.name().as_str() == ":message-type" {
                h.value().as_string().ok().map(|s| s.as_str().to_string())
            } else {
                None
            }
        });

        if message_type.as_deref() == Some("exception") {
            let exception_type = message.headers().iter().find_map(|h| {
                if h.name().as_str() == ":exception-type" {
                    h.value().as_string().ok().map(|s| s.as_str().to_string())
                } else {
                    None
                }
            });
            tracing::error!(
                exception_type = ?exception_type,
                payload = ?String::from_utf8_lossy(message.payload()),
                "Bedrock stream exception"
            );
            // Emit error event
            self.emit_event(
                "error",
                serde_json::json!({
                    "type": "server_error",
                    "message": String::from_utf8_lossy(message.payload()).to_string()
                }),
            );
            return;
        }

        let Some(event_type) = event_type else {
            return;
        };

        let payload = message.payload();

        match event_type.as_str() {
            "messageStart" => {
                if let Ok(_start) = serde_json::from_slice::<BedrockMessageStart>(payload) {
                    // Emit response.created
                    if !self.state.emitted_response_created {
                        self.state.emitted_response_created = true;
                        let resp = self.build_response_json("in_progress", serde_json::json!([]));
                        self.emit_event(
                            "response.created",
                            serde_json::json!({ "response": resp }),
                        );

                        // Emit response.in_progress
                        let resp = self.build_response_json("in_progress", serde_json::json!([]));
                        self.emit_event(
                            "response.in_progress",
                            serde_json::json!({ "response": resp }),
                        );
                    }
                }
            }
            "contentBlockStart" => {
                if let Ok(start) = serde_json::from_slice::<BedrockContentBlockStart>(payload) {
                    // Check if this is a reasoning content block (extended thinking)
                    if let Some(start_data) = &start.start
                        && start_data.reasoning_content.is_some()
                    {
                        // Track this as a reasoning block
                        self.state
                            .reasoning_block_indices
                            .push(start.content_block_index);

                        // Emit output_item.added for reasoning (only once, at index 0)
                        if !self.state.emitted_reasoning_added {
                            self.state.emitted_reasoning_added = true;
                            self.emit_event(
                                "response.output_item.added",
                                serde_json::json!({
                                    "output_index": 0,
                                    "item": {
                                        "type": "reasoning",
                                        "id": self.state.reasoning_id,
                                        "summary": []
                                    }
                                }),
                            );
                        }
                    }
                    // Check if this is a tool use block
                    else if let Some(start_data) = &start.start
                        && let Some(tool_use) = &start_data.tool_use
                    {
                        // Track this tool call
                        let tool_index = self.state.tool_calls.len();
                        self.state.tool_calls.push((
                            start.content_block_index,
                            tool_use.tool_use_id.clone(),
                            tool_use.name.clone(),
                            String::new(),
                        ));

                        // Calculate output index (after reasoning and message if present)
                        let output_index = self.tool_output_index(tool_index);

                        // Emit output_item.added for the function call
                        let fc_id = format!("fc_{}", tool_use.tool_use_id);
                        self.emit_event(
                            "response.output_item.added",
                            serde_json::json!({
                                "output_index": output_index,
                                "item": {
                                    "type": "function_call",
                                    "id": fc_id,
                                    "call_id": tool_use.tool_use_id,
                                    "name": tool_use.name,
                                    "arguments": "",
                                    "status": "in_progress"
                                }
                            }),
                        );
                    } else {
                        // Text block - emit output_item.added for the message (only once)
                        if !self.state.emitted_message_added {
                            self.state.emitted_message_added = true;
                            let msg_output_index = self.message_output_index();
                            self.emit_event(
                                "response.output_item.added",
                                serde_json::json!({
                                    "output_index": msg_output_index,
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

                        // Emit content_part.added for text (only once)
                        if !self.state.emitted_content_part_added {
                            self.state.emitted_content_part_added = true;
                            let msg_output_index = self.message_output_index();
                            self.emit_event(
                                "response.content_part.added",
                                serde_json::json!({
                                    "item_id": self.state.message_id,
                                    "output_index": msg_output_index,
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
                    }
                }
            }
            "contentBlockDelta" => {
                if let Ok(delta) = serde_json::from_slice::<BedrockContentBlockDelta>(payload) {
                    // Reasoning content delta (extended thinking)
                    // Check this first since reasoning blocks come before text
                    if let Some(reasoning) = &delta.delta.reasoning_content
                        && self
                            .state
                            .reasoning_block_indices
                            .contains(&delta.content_block_index)
                    {
                        bounded_push(
                            &mut self.state.reasoning_content,
                            &reasoning.text,
                            self.max_response_state_bytes,
                        );

                        // Accumulate signature if present
                        if let Some(sig) = &reasoning.signature {
                            self.state.reasoning_signature.push_str(sig);
                        }

                        // Emit reasoning summary delta
                        self.emit_event(
                            "response.reasoning_summary_text.delta",
                            serde_json::json!({
                                "item_id": self.state.reasoning_id,
                                "output_index": 0,
                                "summary_index": 0,
                                "delta": reasoning.text
                            }),
                        );
                    }
                    // Text delta
                    else if let Some(text) = delta.delta.text
                        && !text.is_empty()
                    {
                        bounded_push(
                            &mut self.state.text_content,
                            &text,
                            self.max_response_state_bytes,
                        );

                        // Emit text delta
                        let msg_output_index = self.message_output_index();
                        self.emit_event(
                            "response.output_text.delta",
                            serde_json::json!({
                                "item_id": self.state.message_id,
                                "output_index": msg_output_index,
                                "content_index": 0,
                                "delta": text
                            }),
                        );
                    }
                    // Tool use delta (partial JSON arguments)
                    else if let Some(tool_delta) = delta.delta.tool_use {
                        // Find the tool call by block index
                        if let Some(tool_index) = self
                            .state
                            .tool_calls
                            .iter()
                            .position(|(idx, _, _, _)| *idx == delta.content_block_index)
                        {
                            // Update the arguments
                            self.state.tool_calls[tool_index]
                                .3
                                .push_str(&tool_delta.input);

                            let tool_id = self.state.tool_calls[tool_index].1.clone();
                            let output_index = self.tool_output_index(tool_index);

                            // Emit function call arguments delta
                            let fc_id = format!("fc_{}", tool_id);
                            self.emit_event(
                                "response.function_call_arguments.delta",
                                serde_json::json!({
                                    "item_id": fc_id,
                                    "output_index": output_index,
                                    "delta": tool_delta.input
                                }),
                            );
                        }
                    }
                }
            }
            "contentBlockStop" => {
                // Nothing special needed on content block stop
            }
            "messageStop" => {
                if let Ok(stop) = serde_json::from_slice::<BedrockMessageStop>(payload) {
                    self.state.stop_reason = Some(stop.stop_reason);
                }
            }
            "metadata" => {
                if let Ok(metadata) = serde_json::from_slice::<BedrockMetadata>(payload) {
                    self.state.input_tokens = metadata.usage.input_tokens;
                    self.state.output_tokens = metadata.usage.output_tokens;
                    self.state.cache_read_input_tokens = metadata.usage.cache_read_input_tokens;

                    // Emit completion events

                    // Emit reasoning done if we have reasoning content
                    if self.state.emitted_reasoning_added {
                        // Emit reasoning summary done
                        self.emit_event(
                            "response.reasoning_summary_text.done",
                            serde_json::json!({
                                "item_id": self.state.reasoning_id,
                                "output_index": 0,
                                "summary_index": 0,
                                "text": self.state.reasoning_content
                            }),
                        );

                        // Emit reasoning output_item.done (include signature for multi-turn)
                        let mut reasoning_item = serde_json::json!({
                            "type": "reasoning",
                            "id": self.state.reasoning_id,
                            "summary": [{
                                "type": "summary_text",
                                "text": self.state.reasoning_content
                            }]
                        });
                        if !self.state.reasoning_signature.is_empty() {
                            reasoning_item["signature"] =
                                serde_json::Value::String(self.state.reasoning_signature.clone());
                        }
                        self.emit_event(
                            "response.output_item.done",
                            serde_json::json!({
                                "output_index": 0,
                                "item": reasoning_item
                            }),
                        );
                    }

                    // Emit text done if we have text content
                    let msg_output_index = self.message_output_index();
                    if !self.state.text_content.is_empty() {
                        self.emit_event(
                            "response.output_text.done",
                            serde_json::json!({
                                "item_id": self.state.message_id,
                                "output_index": msg_output_index,
                                "content_index": 0,
                                "text": self.state.text_content
                            }),
                        );

                        // Emit content_part.done
                        self.emit_event(
                            "response.content_part.done",
                            serde_json::json!({
                                "item_id": self.state.message_id,
                                "output_index": msg_output_index,
                                "content_index": 0,
                                "part": {
                                    "type": "output_text",
                                    "text": self.state.text_content,
                                    "annotations": [],
                                    "logprobs": []
                                }
                            }),
                        );
                    }

                    // Emit output_item.done for message
                    if self.state.emitted_message_added {
                        self.emit_event(
                            "response.output_item.done",
                            serde_json::json!({
                                "output_index": msg_output_index,
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

                    // Emit function_call_arguments.done and output_item.done for each tool call
                    let tool_calls: Vec<_> = self
                        .state
                        .tool_calls
                        .iter()
                        .enumerate()
                        .map(|(i, (_, tool_id, tool_name, arguments))| {
                            (i, tool_id.clone(), tool_name.clone(), arguments.clone())
                        })
                        .collect();

                    for (i, tool_id, tool_name, arguments) in tool_calls {
                        let output_index = self.tool_output_index(i);
                        let fc_id = format!("fc_{}", tool_id);

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
                                    "call_id": tool_id,
                                    "name": tool_name,
                                    "arguments": arguments,
                                    "status": "completed"
                                }
                            }),
                        );
                    }

                    // Build final output array
                    let mut output = Vec::new();

                    // Reasoning comes first (at index 0)
                    if self.state.emitted_reasoning_added {
                        let mut reasoning_item = serde_json::json!({
                            "type": "reasoning",
                            "id": self.state.reasoning_id,
                            "summary": [{
                                "type": "summary_text",
                                "text": self.state.reasoning_content
                            }]
                        });
                        if !self.state.reasoning_signature.is_empty() {
                            reasoning_item["signature"] =
                                serde_json::Value::String(self.state.reasoning_signature.clone());
                        }
                        output.push(reasoning_item);
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

                    // Tool calls come last
                    for (_, tool_id, tool_name, arguments) in &self.state.tool_calls {
                        let fc_id = format!("fc_{}", tool_id);
                        output.push(serde_json::json!({
                            "type": "function_call",
                            "id": fc_id,
                            "call_id": tool_id,
                            "name": tool_name,
                            "arguments": arguments,
                            "status": "completed"
                        }));
                    }

                    // Determine status
                    let status = match self.state.stop_reason.as_deref() {
                        Some("max_tokens") => "incomplete",
                        Some("guardrail_intervened") => "failed",
                        Some("content_filtered") => "failed",
                        _ => "completed",
                    };

                    // Build response object with echo fields
                    let mut response_obj =
                        self.build_response_json(status, serde_json::json!(output));
                    response_obj.insert("usage".into(), serde_json::json!({
                        "input_tokens": self.state.input_tokens,
                        "input_tokens_details": { "cached_tokens": self.state.cache_read_input_tokens },
                        "output_tokens": self.state.output_tokens,
                        "output_tokens_details": { "reasoning_tokens": 0 },
                        "total_tokens": self.state.input_tokens + self.state.output_tokens
                    }));
                    if status == "completed" {
                        response_obj.insert(
                            "completed_at".into(),
                            serde_json::json!(Self::created_timestamp()),
                        );
                    }

                    // Emit response.completed
                    self.emit_event(
                        "response.completed",
                        serde_json::json!({ "response": response_obj }),
                    );

                    // Emit [DONE] to signal end of stream
                    self.output_buffer
                        .push_back(Bytes::from("data: [DONE]\n\n"));
                }
            }
            _ => {
                tracing::debug!(event_type = %event_type, "Unknown Bedrock stream event");
            }
        }
    }

    pub fn emit_event(&mut self, event_type: &str, data: serde_json::Value) {
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

    /// Process incoming bytes for Responses API streaming
    pub fn process_bytes(&mut self, bytes: &[u8]) {
        // Check if we're already in error state
        if self.state.buffer_overflow {
            return;
        }

        // Check input buffer limit before adding
        if self.state.buffer.len() + bytes.len() > self.max_input_buffer_bytes {
            tracing::error!(
                buffer_size = self.state.buffer.len(),
                incoming_size = bytes.len(),
                max_size = self.max_input_buffer_bytes,
                "Event stream input buffer overflow - possible DoS or malformed response"
            );
            self.state.buffer_overflow = true;
            return;
        }

        self.state.buffer.extend_from_slice(bytes);

        // Try to decode frames from the buffer
        loop {
            match self.state.decoder.decode_frame(&mut self.state.buffer) {
                Ok(DecodedFrame::Complete(message)) => {
                    self.process_event(message);

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
                Ok(DecodedFrame::Incomplete) => {
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to decode Bedrock event stream frame");
                    break;
                }
            }
        }
    }
}

impl<S> Stream for BedrockToResponsesStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.state.buffer_overflow {
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Event stream buffer overflow",
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
                            "Event stream buffer overflow",
                        ))));
                    }

                    if let Some(out) = self.output_buffer.pop_front() {
                        return Poll::Ready(Some(Ok(out)));
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(io::Error::other(e)))),
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

#[cfg(test)]
mod streaming_tests {
    use aws_smithy_eventstream::frame::write_message_to;
    use aws_smithy_types::event_stream::{Header, HeaderValue, Message as EventMessage};
    use futures_util::stream::{self, StreamExt};

    use super::*;

    /// Helper to create an event stream message with headers and payload
    fn create_event_message(event_type: &str, payload: &str) -> Vec<u8> {
        let message = EventMessage::new(payload.as_bytes().to_vec())
            .add_header(Header::new(
                ":message-type",
                HeaderValue::String("event".to_string().into()),
            ))
            .add_header(Header::new(
                ":event-type",
                HeaderValue::String(event_type.to_string().into()),
            ))
            .add_header(Header::new(
                ":content-type",
                HeaderValue::String("application/json".to_string().into()),
            ));

        let mut buffer = Vec::new();
        write_message_to(&message, &mut buffer).unwrap();
        buffer
    }

    #[test]
    fn test_parse_message_start() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );

        // Create messageStart event
        let payload = r#"{"role":"assistant"}"#;
        let event_bytes = create_event_message("messageStart", payload);

        transformer.process_bytes(&event_bytes);

        // Should have one output chunk with role
        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains("data:"));
        assert!(chunk.contains(r#""role":"assistant""#));
        assert!(transformer.state.sent_role);
    }

    #[test]
    fn test_parse_content_block_delta_text() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true; // Simulate role already sent

        // Create contentBlockDelta event with text
        let payload = r#"{"contentBlockIndex":0,"delta":{"text":"Hello, world!"}}"#;
        let event_bytes = create_event_message("contentBlockDelta", payload);

        transformer.process_bytes(&event_bytes);

        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains(r#""content":"Hello, world!""#));
    }

    #[test]
    fn test_parse_message_stop() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;

        // Create messageStop event
        let payload = r#"{"stopReason":"end_turn"}"#;
        let event_bytes = create_event_message("messageStop", payload);

        transformer.process_bytes(&event_bytes);

        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains(r#""finish_reason":"stop""#));
    }

    #[test]
    fn test_parse_metadata() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;

        // Create metadata event
        let payload = r#"{"usage":{"inputTokens":10,"outputTokens":20}}"#;
        let event_bytes = create_event_message("metadata", payload);

        transformer.process_bytes(&event_bytes);

        // Should have 2 chunks: usage info and [DONE]
        assert_eq!(transformer.output_buffer.len(), 2);
        let usage_chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(usage_chunk.contains(r#""prompt_tokens":10"#));
        assert!(usage_chunk.contains(r#""completion_tokens":20"#));
        assert!(usage_chunk.contains(r#""total_tokens":30"#));

        let done_chunk = String::from_utf8_lossy(&transformer.output_buffer[1]);
        assert_eq!(done_chunk.trim(), "data: [DONE]");
    }

    #[test]
    fn test_finish_reason_conversion() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();

        // Test end_turn -> stop
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream.clone(),
            "model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;
        transformer.process_bytes(&create_event_message(
            "messageStop",
            r#"{"stopReason":"end_turn"}"#,
        ));
        assert!(
            String::from_utf8_lossy(&transformer.output_buffer[0])
                .contains(r#""finish_reason":"stop""#)
        );

        // Test max_tokens -> length
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream.clone(),
            "model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;
        transformer.process_bytes(&create_event_message(
            "messageStop",
            r#"{"stopReason":"max_tokens"}"#,
        ));
        assert!(
            String::from_utf8_lossy(&transformer.output_buffer[0])
                .contains(r#""finish_reason":"length""#)
        );

        // Test tool_use -> tool_calls
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream.clone(),
            "model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;
        transformer.process_bytes(&create_event_message(
            "messageStop",
            r#"{"stopReason":"tool_use"}"#,
        ));
        assert!(
            String::from_utf8_lossy(&transformer.output_buffer[0])
                .contains(r#""finish_reason":"tool_calls""#)
        );

        // Test guardrail_intervened -> content_filter
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream.clone(),
            "model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;
        transformer.process_bytes(&create_event_message(
            "messageStop",
            r#"{"stopReason":"guardrail_intervened"}"#,
        ));
        assert!(
            String::from_utf8_lossy(&transformer.output_buffer[0])
                .contains(r#""finish_reason":"content_filter""#)
        );

        // Test content_filtered -> content_filter
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;
        transformer.process_bytes(&create_event_message(
            "messageStop",
            r#"{"stopReason":"content_filtered"}"#,
        ));
        assert!(
            String::from_utf8_lossy(&transformer.output_buffer[0])
                .contains(r#""finish_reason":"content_filter""#)
        );
    }

    #[test]
    fn test_tool_use_streaming() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;

        // Create contentBlockStart with tool use
        let start_payload = r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"call_123","name":"get_weather"}}}"#;
        transformer.process_bytes(&create_event_message("contentBlockStart", start_payload));

        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains(r#""id":"call_123""#));
        assert!(chunk.contains(r#""name":"get_weather""#));
        assert!(chunk.contains(r#""type":"function""#));

        // Clear buffer and send delta
        transformer.output_buffer.clear();
        let delta_payload =
            r#"{"contentBlockIndex":0,"delta":{"toolUse":{"input":"{\"location\":"}}}"#;
        transformer.process_bytes(&create_event_message("contentBlockDelta", delta_payload));

        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains(r#""arguments":"{\"location\":"#));
    }

    #[tokio::test]
    async fn test_stream_transformer_full_sequence() {
        // Create a stream of event bytes representing a complete response
        let events = vec![
            create_event_message("messageStart", r#"{"role":"assistant"}"#),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":0,"delta":{"text":"Hello"}}"#,
            ),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":0,"delta":{"text":", world!"}}"#,
            ),
            create_event_message("contentBlockStop", r#"{"contentBlockIndex":0}"#),
            create_event_message("messageStop", r#"{"stopReason":"end_turn"}"#),
            create_event_message(
                "metadata",
                r#"{"usage":{"inputTokens":5,"outputTokens":3}}"#,
            ),
        ];

        // Create a stream from the events
        let byte_stream = stream::iter(
            events
                .into_iter()
                .map(|e| Ok::<_, reqwest::Error>(Bytes::from(e))),
        );
        let mut transformer = BedrockToOpenAIStream::new(
            byte_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );

        // Collect all output
        let mut outputs = Vec::new();
        while let Some(result) = transformer.next().await {
            match result {
                Ok(bytes) => outputs.push(String::from_utf8_lossy(&bytes).to_string()),
                Err(e) => panic!("Stream error: {}", e),
            }
        }

        // Verify output sequence
        assert!(outputs.len() >= 5); // At least: role, 2 text deltas, finish, usage, done
        assert!(outputs[0].contains(r#""role":"assistant""#));
        assert!(outputs.iter().any(|o| o.contains(r#""content":"Hello""#)));
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""content":", world!""#))
        );
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""finish_reason":"stop""#))
        );
        assert!(outputs.iter().any(|o| o.contains(r#""total_tokens":8"#)));
        assert!(outputs.last().unwrap().contains("[DONE]"));
    }

    #[test]
    fn test_stream_state_initialization() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "my-model".into(),
            &StreamingBufferConfig::default(),
        );

        assert!(transformer.state.message_id.starts_with("chatcmpl-"));
        assert_eq!(transformer.state.model, "my-model");
        assert!(!transformer.state.sent_role);
        assert_eq!(transformer.state.input_tokens, 0);
        assert_eq!(transformer.state.output_tokens, 0);
        assert!(transformer.output_buffer.is_empty());
    }

    #[test]
    fn test_partial_frame_buffering() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );

        // Create a full event
        let event_bytes = create_event_message("messageStart", r#"{"role":"assistant"}"#);

        // Send only first half
        let half = event_bytes.len() / 2;
        transformer.process_bytes(&event_bytes[..half]);

        // No output yet (incomplete frame)
        assert!(transformer.output_buffer.is_empty());

        // Send second half
        transformer.process_bytes(&event_bytes[half..]);

        // Now we should have output
        assert_eq!(transformer.output_buffer.len(), 1);
        assert!(String::from_utf8_lossy(&transformer.output_buffer[0]).contains("assistant"));
    }

    #[test]
    fn test_reasoning_content_block_start() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;

        // Create contentBlockStart with reasoning content
        let start_payload = r#"{"contentBlockIndex":0,"start":{"reasoningContent":{}}}"#;
        transformer.process_bytes(&create_event_message("contentBlockStart", start_payload));

        // Should track the reasoning block index
        assert_eq!(transformer.state.reasoning_block_indices, vec![0]);
        // No output emitted for reasoning block start (unlike tool use)
        assert!(transformer.output_buffer.is_empty());
    }

    #[test]
    fn test_reasoning_content_delta() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;
        // Pre-register block 0 as a reasoning block
        transformer.state.reasoning_block_indices.push(0);

        // Create contentBlockDelta with reasoning content
        let delta_payload =
            r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Let me think..."}}}"#;
        transformer.process_bytes(&create_event_message("contentBlockDelta", delta_payload));

        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains(r#""reasoning":"Let me think...""#));
        // Should NOT have content field
        assert!(!chunk.contains(r#""content":"#));
    }

    #[test]
    fn test_reasoning_content_ignored_for_unknown_block() {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        let mut transformer = BedrockToOpenAIStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );
        transformer.state.sent_role = true;
        // Block 0 is NOT registered as a reasoning block

        // Create contentBlockDelta with reasoning content for untracked block
        let delta_payload =
            r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Ignored"}}}"#;
        transformer.process_bytes(&create_event_message("contentBlockDelta", delta_payload));

        // Should not emit anything since block 0 is not tracked
        assert!(transformer.output_buffer.is_empty());
    }

    #[tokio::test]
    async fn test_stream_with_reasoning_content() {
        // Create a stream of events with reasoning content followed by text
        let events = vec![
            create_event_message("messageStart", r#"{"role":"assistant"}"#),
            // Reasoning block starts
            create_event_message(
                "contentBlockStart",
                r#"{"contentBlockIndex":0,"start":{"reasoningContent":{}}}"#,
            ),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Thinking step 1"}}}"#,
            ),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":" and step 2"}}}"#,
            ),
            create_event_message("contentBlockStop", r#"{"contentBlockIndex":0}"#),
            // Text block starts
            create_event_message("contentBlockStart", r#"{"contentBlockIndex":1,"start":{}}"#),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":1,"delta":{"text":"Here is the answer."}}"#,
            ),
            create_event_message("contentBlockStop", r#"{"contentBlockIndex":1}"#),
            create_event_message("messageStop", r#"{"stopReason":"end_turn"}"#),
            create_event_message(
                "metadata",
                r#"{"usage":{"inputTokens":10,"outputTokens":20}}"#,
            ),
        ];

        let byte_stream = stream::iter(
            events
                .into_iter()
                .map(|e| Ok::<_, reqwest::Error>(Bytes::from(e))),
        );
        let mut transformer = BedrockToOpenAIStream::new(
            byte_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
        );

        let mut outputs = Vec::new();
        while let Some(result) = transformer.next().await {
            match result {
                Ok(bytes) => outputs.push(String::from_utf8_lossy(&bytes).to_string()),
                Err(e) => panic!("Stream error: {}", e),
            }
        }

        // Verify reasoning content was emitted
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""reasoning":"Thinking step 1""#))
        );
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""reasoning":" and step 2""#))
        );
        // Verify text content was emitted
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""content":"Here is the answer.""#))
        );
        // Verify finish reason
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""finish_reason":"stop""#))
        );
    }

    // ============================================================================
    // Responses API Streaming Tests
    // ============================================================================

    /// Helper to create a Responses API stream transformer for testing
    fn create_responses_transformer()
    -> BedrockToResponsesStream<futures_util::stream::Empty<Result<Bytes, reqwest::Error>>> {
        let empty_stream = stream::empty::<Result<Bytes, reqwest::Error>>();
        BedrockToResponsesStream::new(
            empty_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
            serde_json::Map::new(),
        )
    }

    #[test]
    fn test_responses_reasoning_content_block_start() {
        let mut transformer = create_responses_transformer();

        // First emit response.created via messageStart
        transformer.process_bytes(&create_event_message(
            "messageStart",
            r#"{"role":"assistant"}"#,
        ));
        transformer.output_buffer.clear();

        // Create contentBlockStart with reasoning content
        let start_payload = r#"{"contentBlockIndex":0,"start":{"reasoningContent":{}}}"#;
        transformer.process_bytes(&create_event_message("contentBlockStart", start_payload));

        // Should track the reasoning block index
        assert_eq!(transformer.state.reasoning_block_indices, vec![0]);
        assert!(transformer.state.emitted_reasoning_added);

        // Should emit output_item.added for reasoning
        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains(r#""type":"response.output_item.added""#));
        assert!(chunk.contains(r#""type":"reasoning""#));
        assert!(chunk.contains(&transformer.state.reasoning_id));
    }

    #[test]
    fn test_responses_reasoning_content_delta() {
        let mut transformer = create_responses_transformer();
        // Pre-register block 0 as a reasoning block
        transformer.state.reasoning_block_indices.push(0);
        transformer.state.emitted_reasoning_added = true;

        // Create contentBlockDelta with reasoning content
        let delta_payload =
            r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Thinking..."}}}"#;
        transformer.process_bytes(&create_event_message("contentBlockDelta", delta_payload));

        assert_eq!(transformer.output_buffer.len(), 1);
        let chunk = String::from_utf8_lossy(&transformer.output_buffer[0]);
        assert!(chunk.contains(r#""type":"response.reasoning_summary_text.delta""#));
        assert!(chunk.contains(r#""delta":"Thinking...""#));
        assert!(chunk.contains(r#""output_index":0"#));

        // Verify reasoning content is accumulated
        assert_eq!(transformer.state.reasoning_content, "Thinking...");
    }

    #[test]
    fn test_responses_message_output_index_with_reasoning() {
        let mut transformer = create_responses_transformer();

        // Without reasoning, message index is 0
        assert_eq!(transformer.message_output_index(), 0);

        // With reasoning, message index is 1
        transformer.state.emitted_reasoning_added = true;
        assert_eq!(transformer.message_output_index(), 1);
    }

    #[test]
    fn test_responses_tool_output_index_with_reasoning() {
        let mut transformer = create_responses_transformer();

        // Without reasoning or message, tool index 0 is 0
        assert_eq!(transformer.tool_output_index(0), 0);

        // With message only, tool index 0 is 1
        transformer.state.emitted_message_added = true;
        assert_eq!(transformer.tool_output_index(0), 1);

        // With reasoning and message, tool index 0 is 2
        transformer.state.emitted_reasoning_added = true;
        assert_eq!(transformer.tool_output_index(0), 2);

        // Tool index 1 would be 3
        assert_eq!(transformer.tool_output_index(1), 3);
    }

    #[tokio::test]
    async fn test_responses_stream_with_reasoning_content() {
        // Create a stream of events with reasoning content followed by text
        let events = vec![
            create_event_message("messageStart", r#"{"role":"assistant"}"#),
            // Reasoning block starts
            create_event_message(
                "contentBlockStart",
                r#"{"contentBlockIndex":0,"start":{"reasoningContent":{}}}"#,
            ),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Step 1: analyze"}}}"#,
            ),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":". Step 2: solve"}}}"#,
            ),
            create_event_message("contentBlockStop", r#"{"contentBlockIndex":0}"#),
            // Text block starts
            create_event_message("contentBlockStart", r#"{"contentBlockIndex":1,"start":{}}"#),
            create_event_message(
                "contentBlockDelta",
                r#"{"contentBlockIndex":1,"delta":{"text":"The answer is 42."}}"#,
            ),
            create_event_message("contentBlockStop", r#"{"contentBlockIndex":1}"#),
            create_event_message("messageStop", r#"{"stopReason":"end_turn"}"#),
            create_event_message(
                "metadata",
                r#"{"usage":{"inputTokens":10,"outputTokens":20}}"#,
            ),
        ];

        let byte_stream = stream::iter(
            events
                .into_iter()
                .map(|e| Ok::<_, reqwest::Error>(Bytes::from(e))),
        );
        let mut transformer = BedrockToResponsesStream::new(
            byte_stream,
            "test-model".into(),
            &StreamingBufferConfig::default(),
            serde_json::Map::new(),
        );

        let mut outputs = Vec::new();
        while let Some(result) = transformer.next().await {
            match result {
                Ok(bytes) => outputs.push(String::from_utf8_lossy(&bytes).to_string()),
                Err(e) => panic!("Stream error: {}", e),
            }
        }

        // Verify reasoning output_item.added was emitted
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""type":"reasoning""#) && o.contains("output_item.added"))
        );

        // Verify reasoning delta was emitted
        assert!(
            outputs
                .iter()
                .any(|o| o.contains("reasoning_summary_text.delta")
                    && o.contains(r#""delta":"Step 1: analyze""#))
        );

        // Verify message output index is 1 (after reasoning)
        assert!(
            outputs
                .iter()
                .any(|o| o.contains(r#""type":"message""#) && o.contains(r#""output_index":1"#))
        );

        // Verify text delta uses correct output index
        assert!(
            outputs
                .iter()
                .any(|o| o.contains("output_text.delta") && o.contains(r#""output_index":1"#))
        );

        // Verify reasoning_summary_text.done was emitted
        assert!(
            outputs
                .iter()
                .any(|o| o.contains("reasoning_summary_text.done"))
        );

        // Verify response.completed includes both reasoning and message
        let completed = outputs
            .iter()
            .find(|o| o.contains("response.completed"))
            .expect("Should have response.completed");
        assert!(completed.contains(r#""type":"reasoning""#));
        assert!(completed.contains(r#""type":"message""#));
    }
}
