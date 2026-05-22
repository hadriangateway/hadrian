//! Conversion functions between OpenAI and Bedrock formats.
//!
//! This module handles bidirectional conversion between:
//! - OpenAI chat completion format <-> Bedrock Converse API
//! - OpenAI streaming format <-> Bedrock event stream
//! - OpenAI Responses API format <-> Bedrock Converse API
//! - Tool definitions and calls

use chrono::Utc;

use super::types::*;
use crate::{
    api_types::{
        chat_completion::{
            ContentPart, CreateChatCompletionReasoning, Message, MessageContent, ReasoningEffort,
            Stop, ToolChoice, ToolChoiceDefaults, ToolDefinition,
        },
        responses::{
            CreateResponsesResponse, EasyInputMessageContent, EasyInputMessageRole,
            InputMessageItemRole, MessageType, OpenResponsesReasoningFormat,
            OutputItemFunctionCall, OutputItemFunctionCallStatus, OutputItemFunctionCallType,
            OutputMessage, OutputMessageContentItem, OutputMessageStatus, ResponseInputContentItem,
            ResponseType, ResponsesInput, ResponsesInputItem, ResponsesOutputItem,
            ResponsesReasoning, ResponsesReasoningConfig, ResponsesReasoningConfigOutput,
            ResponsesReasoningEffort, ResponsesReasoningType, ResponsesResponseStatus,
            ResponsesToolChoice, ResponsesToolChoiceDefault, ResponsesToolDefinition,
            ResponsesUsage, ResponsesUsageInputTokensDetails, ResponsesUsageOutputTokensDetails,
        },
    },
    providers::image::parse_data_url,
    services::FileSearchToolArguments,
};

/// Extract text content from MessageContent
pub(super) fn extract_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| {
                if let ContentPart::Text { text, .. } = p {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

/// Convert media type to Bedrock image format
pub(super) fn media_type_to_bedrock_format(media_type: &str) -> Option<String> {
    match media_type {
        "image/png" => Some("png".to_string()),
        "image/jpeg" | "image/jpg" => Some("jpeg".to_string()),
        "image/gif" => Some("gif".to_string()),
        "image/webp" => Some("webp".to_string()),
        _ => None,
    }
}

/// Convert MessageContent to Bedrock content blocks, including images.
///
/// Note: HTTP image URLs should be preprocessed using `preprocess_messages_for_images`
/// before calling this function. Any remaining HTTP URLs will be skipped with a warning.
///
/// When content parts have `cache_control`, a separate `cachePoint` block is inserted
/// after that content. This is how Bedrock's prompt caching works (vs Anthropic's inline
/// `cache_control` property).
pub(super) fn convert_content_to_bedrock(content: &MessageContent) -> Vec<BedrockContent> {
    match content {
        MessageContent::Text(text) => {
            if text.is_empty() {
                vec![]
            } else {
                vec![BedrockContent::text(text.clone())]
            }
        }
        MessageContent::Parts(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                match part {
                    ContentPart::Text {
                        text,
                        cache_control,
                    } => {
                        if !text.is_empty() {
                            blocks.push(BedrockContent::text(text.clone()));
                            // Insert cache point after content if cache_control is set
                            if cache_control.is_some() {
                                blocks.push(BedrockContent::cache_point());
                            }
                        }
                    }
                    ContentPart::ImageUrl {
                        image_url,
                        cache_control,
                    } => {
                        // Track if we successfully added the image block
                        let mut added_image = false;

                        // Try to parse as data URL (base64) using shared utilities
                        match parse_data_url(&image_url.url) {
                            Ok(image_data) => {
                                if let Some(format) =
                                    media_type_to_bedrock_format(&image_data.media_type)
                                {
                                    blocks.push(BedrockContent::image(format, image_data.data));
                                    added_image = true;
                                } else {
                                    tracing::warn!(
                                        media_type = %image_data.media_type,
                                        "Bedrock provider does not support this image format. Supported: png, jpeg, gif, webp. Image skipped."
                                    );
                                }
                            }
                            Err(e) => {
                                // Log specific error for debugging malformed data URLs
                                tracing::warn!(
                                    url = %image_url.url,
                                    error = %e,
                                    "Failed to parse image data URL. HTTP URLs should be preprocessed. Image skipped."
                                );
                            }
                        }

                        // Insert cache point after image if cache_control is set and image was added
                        if added_image && cache_control.is_some() {
                            blocks.push(BedrockContent::cache_point());
                        }
                    }
                    // Audio and video not supported by Bedrock
                    ContentPart::InputAudio { .. }
                    | ContentPart::InputVideo { .. }
                    | ContentPart::VideoUrl { .. } => {
                        tracing::warn!(
                            "Bedrock provider does not support audio/video content. Content skipped."
                        );
                    }
                }
            }
            blocks
        }
    }
}

/// Convert OpenAI tools to Bedrock format.
///
/// When tools have `cache_control`, a separate `cachePoint` entry is inserted
/// after that tool in the tools array. This is how Bedrock's prompt caching
/// works for tools.
pub(super) fn convert_tools(tools: Option<Vec<ToolDefinition>>) -> Option<Vec<BedrockTool>> {
    tools.map(|tools| {
        let mut bedrock_tools = Vec::new();
        for tool in tools {
            bedrock_tools.push(BedrockTool::with_spec(BedrockToolSpec {
                name: tool.function.name.clone(),
                description: tool.function.description.clone(),
                input_schema: BedrockInputSchema {
                    json: tool
                        .function
                        .parameters
                        .clone()
                        .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                },
            }));

            // Insert cache point after tool if cache_control is set
            if tool.cache_control.is_some() {
                bedrock_tools.push(BedrockTool::cache_point());
            }
        }
        bedrock_tools
    })
}

/// Convert OpenAI tool_choice to Bedrock format.
///
/// When `tool_choice: "none"` is specified, returns `None` to omit tool_choice
/// from the request entirely. Bedrock doesn't have an explicit "none" option,
/// and setting it to "auto" would incorrectly allow tool usage.
pub(super) fn convert_tool_choice(tool_choice: Option<ToolChoice>) -> Option<BedrockToolChoice> {
    tool_choice.and_then(|tc| match tc {
        ToolChoice::String(default) => match default {
            ToolChoiceDefaults::Auto => Some(BedrockToolChoice::Auto {}),
            ToolChoiceDefaults::Required => Some(BedrockToolChoice::Any {}),
            ToolChoiceDefaults::None => None,
        },
        ToolChoice::Named(named) => Some(BedrockToolChoice::Tool {
            name: named.function.name,
        }),
    })
}

/// Convert OpenAI messages to Bedrock format.
///
/// When content parts have `cache_control`, a separate `cachePoint` block is inserted
/// after that content. This applies to both regular messages and system messages.
pub(super) fn convert_messages(
    openai_messages: Vec<Message>,
) -> (Option<Vec<BedrockSystemContent>>, Vec<BedrockMessage>) {
    let mut system_blocks: Vec<BedrockSystemContent> = Vec::new();
    let mut messages = Vec::new();
    let mut pending_tool_results: Vec<BedrockContent> = Vec::new();

    for msg in openai_messages {
        match msg {
            Message::System { content, .. } | Message::Developer { content, .. } => {
                // Convert system message content, preserving cache_control markers
                match &content {
                    MessageContent::Text(text) => {
                        if !text.is_empty() {
                            system_blocks.push(BedrockSystemContent::text(text.clone()));
                        }
                    }
                    MessageContent::Parts(parts) => {
                        for part in parts {
                            if let ContentPart::Text {
                                text,
                                cache_control,
                            } = part
                                && !text.is_empty()
                            {
                                system_blocks.push(BedrockSystemContent::text(text.clone()));
                                // Insert cache point after content if cache_control is set
                                if cache_control.is_some() {
                                    system_blocks.push(BedrockSystemContent::cache_point());
                                }
                            }
                            // Other content types (images, etc.) are not typically in system messages
                        }
                    }
                }
            }
            Message::User { content, .. } => {
                // Flush any pending tool results first
                if !pending_tool_results.is_empty() {
                    messages.push(BedrockMessage {
                        role: "user".to_string(),
                        content: std::mem::take(&mut pending_tool_results),
                    });
                }
                // Use content conversion to support images and other multimodal content
                let content_blocks = convert_content_to_bedrock(&content);
                if !content_blocks.is_empty() {
                    messages.push(BedrockMessage {
                        role: "user".to_string(),
                        content: content_blocks,
                    });
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                // Flush any pending tool results first
                if !pending_tool_results.is_empty() {
                    messages.push(BedrockMessage {
                        role: "user".to_string(),
                        content: std::mem::take(&mut pending_tool_results),
                    });
                }

                let mut content_blocks = Vec::new();

                // Add text content if present
                if let Some(content) = content {
                    let text = extract_text(&content);
                    if !text.is_empty() {
                        content_blocks.push(BedrockContent::text(text));
                    }
                }

                // Add tool use blocks if present
                if let Some(tool_calls) = tool_calls {
                    for tool_call in tool_calls {
                        // Parse the JSON arguments string into a Value
                        let input = serde_json::from_str(&tool_call.function.arguments)
                            .unwrap_or(serde_json::json!({}));
                        content_blocks.push(BedrockContent::tool_use(
                            tool_call.id,
                            tool_call.function.name,
                            input,
                        ));
                    }
                }

                if !content_blocks.is_empty() {
                    messages.push(BedrockMessage {
                        role: "assistant".to_string(),
                        content: content_blocks,
                    });
                }
            }
            Message::Tool {
                content,
                tool_call_id,
            } => {
                // Collect tool results to be sent as a user message.
                // OpenAI's Chat Completions API doesn't have an error indicator for tool results,
                // so we default to success. The actual error would be in the content itself.
                pending_tool_results.push(BedrockContent::tool_result(
                    tool_call_id,
                    extract_text(&content),
                    Some(BedrockToolResultStatus::Success),
                ));
            }
        }
    }

    // Flush any remaining tool results
    if !pending_tool_results.is_empty() {
        messages.push(BedrockMessage {
            role: "user".to_string(),
            content: pending_tool_results,
        });
    }

    // Return system blocks if any were collected
    let system_content = if system_blocks.is_empty() {
        None
    } else {
        Some(system_blocks)
    };

    (system_content, messages)
}

/// Convert stop sequences from OpenAI format
pub(super) fn convert_stop(stop: Option<Stop>) -> Option<Vec<String>> {
    stop.map(|s| match s {
        Stop::Single(s) => vec![s],
        Stop::Multiple(v) => v,
    })
}

/// Convert Bedrock response to OpenAI format.
pub(super) fn convert_response(bedrock: BedrockConverseResponse, model: &str) -> OpenAIResponse {
    let mut text_content = Vec::new();
    let mut reasoning_content = Vec::new();
    let mut tool_calls = Vec::new();

    for content in bedrock.output.message.content {
        if let Some(text) = content.text {
            text_content.push(text);
        }
        if let Some(tool_use) = content.tool_use {
            tool_calls.push(OpenAIToolCall {
                id: tool_use.tool_use_id,
                type_: "function".to_string(),
                function: OpenAIToolCallFunction {
                    name: tool_use.name,
                    arguments: serde_json::to_string(&tool_use.input).unwrap_or_default(),
                },
            });
        }
        // Extract reasoning content from extended thinking
        if let Some(reasoning) = content.reasoning_content
            && let Some(reasoning_text) = reasoning.reasoning_text
        {
            reasoning_content.push(reasoning_text.text);
        }
    }

    let content = text_content.join("");
    let reasoning = reasoning_content.join("");

    let finish_reason = match bedrock.stop_reason.as_deref() {
        Some("end_turn") => Some("stop".to_string()),
        Some("max_tokens") => Some("length".to_string()),
        Some("stop_sequence") => Some("stop".to_string()),
        Some("tool_use") => Some("tool_calls".to_string()),
        Some("guardrail_intervened") => Some("content_filter".to_string()),
        Some("content_filtered") => Some("content_filter".to_string()),
        other => other.map(String::from),
    };

    OpenAIResponse {
        id: format!("bedrock-{}", uuid::Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: Utc::now().timestamp(),
        model: model.to_string(),
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIMessage {
                role: "assistant".to_string(),
                content: if content.is_empty() {
                    None
                } else {
                    Some(content)
                },
                refusal: None,
                reasoning: if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
            },
            finish_reason,
            logprobs: None,
        }],
        usage: Some(OpenAIUsage {
            prompt_tokens: bedrock.usage.input_tokens,
            completion_tokens: bedrock.usage.output_tokens,
            total_tokens: bedrock.usage.input_tokens + bedrock.usage.output_tokens,
            prompt_tokens_details: if bedrock.usage.cache_read_input_tokens > 0 {
                Some(PromptTokensDetails {
                    cached_tokens: bedrock.usage.cache_read_input_tokens,
                })
            } else {
                None
            },
        }),
        system_fingerprint: None,
    }
}

// ============================================================================
// Responses API Conversion Functions
// ============================================================================

/// Convert OpenAI Responses API input to Bedrock Messages format.
/// Returns (system_prompt, messages).
pub(super) fn convert_responses_input_to_bedrock_messages(
    input: Option<ResponsesInput>,
    instructions: Option<String>,
) -> (Option<Vec<BedrockSystemContent>>, Vec<BedrockMessage>) {
    let system = instructions.map(|text| vec![BedrockSystemContent::text(text)]);
    let mut messages: Vec<BedrockMessage> = Vec::new();

    let Some(input) = input else {
        return (system, messages);
    };

    match input {
        ResponsesInput::Text(text) => {
            // Simple text input becomes a single user message
            messages.push(BedrockMessage {
                role: "user".to_string(),
                content: vec![BedrockContent::text(text)],
            });
        }
        ResponsesInput::Items(items) => {
            // Collect tool results to batch into user messages
            let mut pending_tool_results: Vec<BedrockContent> = Vec::new();

            for item in items {
                match item {
                    ResponsesInputItem::EasyMessage(msg) => {
                        // Flush pending tool results before adding new message
                        if !pending_tool_results.is_empty() {
                            messages.push(BedrockMessage {
                                role: "user".to_string(),
                                content: std::mem::take(&mut pending_tool_results),
                            });
                        }

                        let role = match msg.role {
                            EasyInputMessageRole::User => "user",
                            EasyInputMessageRole::Assistant => "assistant",
                            EasyInputMessageRole::System | EasyInputMessageRole::Developer => {
                                // System/developer messages are handled via instructions
                                continue;
                            }
                        };

                        let content = match msg.content {
                            EasyInputMessageContent::Text(text) => vec![BedrockContent::text(text)],
                            EasyInputMessageContent::Parts(parts) => {
                                convert_responses_content_to_bedrock(&parts)
                            }
                        };

                        if !content.is_empty() {
                            messages.push(BedrockMessage {
                                role: role.to_string(),
                                content,
                            });
                        }
                    }
                    ResponsesInputItem::MessageItem(msg) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(BedrockMessage {
                                role: "user".to_string(),
                                content: std::mem::take(&mut pending_tool_results),
                            });
                        }

                        let role = match msg.role {
                            InputMessageItemRole::User => "user",
                            InputMessageItemRole::System | InputMessageItemRole::Developer => {
                                continue;
                            }
                        };

                        let content = convert_responses_content_to_bedrock(&msg.content);
                        if !content.is_empty() {
                            messages.push(BedrockMessage {
                                role: role.to_string(),
                                content,
                            });
                        }
                    }
                    ResponsesInputItem::OutputMessage(msg) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(BedrockMessage {
                                role: "user".to_string(),
                                content: std::mem::take(&mut pending_tool_results),
                            });
                        }

                        // Output message from assistant - convert content items to blocks
                        let mut content = Vec::new();
                        for content_item in msg.content {
                            match content_item {
                                OutputMessageContentItem::OutputText { text, .. } => {
                                    content.push(BedrockContent::text(text));
                                }
                                OutputMessageContentItem::Refusal { refusal } => {
                                    content.push(BedrockContent::text(refusal));
                                }
                            }
                        }

                        if !content.is_empty() {
                            messages.push(BedrockMessage {
                                role: "assistant".to_string(),
                                content,
                            });
                        }
                    }
                    ResponsesInputItem::FunctionCall(call) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(BedrockMessage {
                                role: "user".to_string(),
                                content: std::mem::take(&mut pending_tool_results),
                            });
                        }

                        // Function call from assistant
                        let input: serde_json::Value =
                            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
                        let call_id = crate::providers::normalize_tool_call_id(&call.call_id);
                        messages.push(BedrockMessage {
                            role: "assistant".to_string(),
                            content: vec![BedrockContent::tool_use(
                                call_id,
                                call.name.clone(),
                                input,
                            )],
                        });
                    }
                    ResponsesInputItem::OutputFunctionCall(call) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(BedrockMessage {
                                role: "user".to_string(),
                                content: std::mem::take(&mut pending_tool_results),
                            });
                        }

                        // Output function call from assistant
                        let input: serde_json::Value =
                            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
                        let call_id = crate::providers::normalize_tool_call_id(&call.call_id);
                        messages.push(BedrockMessage {
                            role: "assistant".to_string(),
                            content: vec![BedrockContent::tool_use(
                                call_id,
                                call.name.clone(),
                                input,
                            )],
                        });
                    }
                    ResponsesInputItem::FunctionCallOutput(output) => {
                        // Collect tool results to batch into a user message.
                        // Map OpenAI's ToolCallStatus to Bedrock's status:
                        // - Completed -> success
                        // - Incomplete -> error (incomplete indicates failure)
                        // - InProgress/None -> success (default)
                        use crate::api_types::responses::ToolCallStatus;
                        let status = match output.status {
                            Some(ToolCallStatus::Incomplete) => BedrockToolResultStatus::Error,
                            _ => BedrockToolResultStatus::Success,
                        };
                        let call_id = crate::providers::normalize_tool_call_id(&output.call_id);
                        pending_tool_results.push(BedrockContent::tool_result(
                            call_id,
                            output.output,
                            Some(status),
                        ));
                    }
                    ResponsesInputItem::Reasoning(_) => {
                        // Reasoning blocks from previous responses are typically not sent back
                    }
                    ResponsesInputItem::WebSearchCall(_)
                    | ResponsesInputItem::FileSearchCall(_)
                    | ResponsesInputItem::ShellCall(_)
                    | ResponsesInputItem::ShellCallOutput(_)
                    | ResponsesInputItem::McpListTools(_)
                    | ResponsesInputItem::McpCall(_)
                    | ResponsesInputItem::McpApprovalRequest(_)
                    | ResponsesInputItem::McpApprovalResponse(_)
                    | ResponsesInputItem::ToolSearchCall(_)
                    | ResponsesInputItem::ToolSearchOutput(_)
                    | ResponsesInputItem::Compaction(_)
                    | ResponsesInputItem::ImageGeneration(_) => {
                        // These are server-side tool calls that don't need translation
                    }
                }
            }

            // Flush any remaining tool results
            if !pending_tool_results.is_empty() {
                messages.push(BedrockMessage {
                    role: "user".to_string(),
                    content: pending_tool_results,
                });
            }
        }
    }

    (system, messages)
}

/// Convert Responses API content items to Bedrock content blocks.
///
/// When content items have `cache_control`, a separate `cachePoint` block is inserted
/// after that content. This is how Bedrock's prompt caching works.
pub(super) fn convert_responses_content_to_bedrock(
    items: &[ResponseInputContentItem],
) -> Vec<BedrockContent> {
    let mut blocks = Vec::new();

    for item in items {
        match item {
            ResponseInputContentItem::InputText {
                text,
                cache_control,
            } => {
                if !text.is_empty() {
                    blocks.push(BedrockContent::text(text.clone()));
                    // Insert cache point after content if cache_control is set
                    if cache_control.is_some() {
                        blocks.push(BedrockContent::cache_point());
                    }
                }
            }
            ResponseInputContentItem::InputImage {
                image_url,
                detail,
                cache_control,
            } => {
                // Track if we successfully added the image block
                let mut added_image = false;

                // If we have an image URL that's a data URL, parse it
                if let Some(url) = image_url {
                    match parse_data_url(url) {
                        Ok(image_data) => {
                            if let Some(format) =
                                media_type_to_bedrock_format(&image_data.media_type)
                            {
                                blocks.push(BedrockContent::image(format, image_data.data));
                                added_image = true;
                            } else {
                                tracing::warn!(
                                    media_type = %image_data.media_type,
                                    detail = ?detail,
                                    "Unsupported image format for Bedrock, skipping"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                url = %url,
                                error = %e,
                                "Failed to parse image data URL, skipping in Bedrock conversion"
                            );
                        }
                    }
                }

                // Insert cache point after image if cache_control is set and image was added
                if added_image && cache_control.is_some() {
                    blocks.push(BedrockContent::cache_point());
                }
            }
            ResponseInputContentItem::InputFile { .. } => {
                tracing::warn!("File inputs not supported by Bedrock Converse API");
            }
            ResponseInputContentItem::InputAudio { .. } => {
                tracing::warn!("Audio inputs not supported by Bedrock Converse API");
            }
        }
    }

    blocks
}

/// Convert Responses API tools to Bedrock tools format.
///
/// When tools have `cache_control`, a separate `cachePoint` entry is inserted
/// after that tool in the tools array. This is how Bedrock's prompt caching
/// works for tools.
pub(super) fn convert_responses_tools_to_bedrock(
    tools: Option<Vec<ResponsesToolDefinition>>,
) -> Option<Vec<BedrockTool>> {
    let tools = tools?;
    let mut bedrock_tools = Vec::new();

    for tool in tools {
        match tool {
            ResponsesToolDefinition::Function(func) => {
                let parameters = func
                    .parameters
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                let has_cache_control = func.extras.contains_key("cache_control");

                bedrock_tools.push(BedrockTool::with_spec(BedrockToolSpec {
                    name: func.name,
                    description: func.description,
                    input_schema: BedrockInputSchema { json: parameters },
                }));

                // Insert cache point after tool if cache_control is set
                if has_cache_control {
                    bedrock_tools.push(BedrockTool::cache_point());
                }
            }
            ResponsesToolDefinition::WebSearchPreview(_)
            | ResponsesToolDefinition::WebSearchPreview20250311(_)
            | ResponsesToolDefinition::WebSearch(_)
            | ResponsesToolDefinition::WebSearch20250826(_) => {
                // Dead code: preprocessed to function tools in execution.rs
                tracing::warn!("Unexpected web_search tool variant reached Bedrock conversion");
            }
            ResponsesToolDefinition::Shell(_) => {
                tracing::warn!(
                    "Shell tool reached Bedrock conversion — only OpenAI passthrough is \
                     supported for shell in the current build; dropping the tool definition"
                );
            }
            ResponsesToolDefinition::Mcp(_) => {
                tracing::warn!(
                    "MCP tool reached Bedrock conversion — `mcp` requires `mode = \
                     passthrough_openai` and an OpenAI/Azure upstream; dropping the tool definition"
                );
            }
            ResponsesToolDefinition::ToolSearch(_) => {
                tracing::warn!(
                    "tool_search tool reached Bedrock conversion — should have been consumed \
                     by the MCP rewrite under hadrian_hosted; dropping the tool definition"
                );
            }
            ResponsesToolDefinition::FileSearch(file_search) => {
                // File search is handled by the gateway middleware
                bedrock_tools.push(BedrockTool::with_spec(BedrockToolSpec {
                    name: FileSearchToolArguments::FUNCTION_NAME.to_string(),
                    description: Some(FileSearchToolArguments::function_description().to_string()),
                    input_schema: BedrockInputSchema {
                        json: FileSearchToolArguments::function_parameters_schema(),
                    },
                }));

                // Insert cache point after tool if cache_control is set
                if file_search.cache_control.is_some() {
                    bedrock_tools.push(BedrockTool::cache_point());
                }

                tracing::debug!("Converted file_search tool to function definition for Bedrock");
            }
        }
    }

    if bedrock_tools.is_empty() {
        None
    } else {
        Some(bedrock_tools)
    }
}

/// Convert Responses API tool choice to Bedrock tool choice format.
pub(super) fn convert_responses_tool_choice_to_bedrock(
    tool_choice: Option<ResponsesToolChoice>,
) -> Option<BedrockToolChoice> {
    tool_choice.and_then(|tc| match tc {
        ResponsesToolChoice::String(default) => match default {
            ResponsesToolChoiceDefault::Auto => Some(BedrockToolChoice::Auto {}),
            ResponsesToolChoiceDefault::Required => Some(BedrockToolChoice::Any {}),
            ResponsesToolChoiceDefault::None => None,
        },
        ResponsesToolChoice::Named(named) => Some(BedrockToolChoice::Tool { name: named.name }),
        ResponsesToolChoice::WebSearch(_) => {
            tracing::warn!("Web search tool choice not supported by Bedrock");
            None
        }
        ResponsesToolChoice::Shell(_) => Some(BedrockToolChoice::Tool {
            name: "shell".to_string(),
        }),
        ResponsesToolChoice::Mcp(_) => {
            // Reaches Bedrock only when the hadrian_hosted rewrite was
            // skipped (mcp feature off or no matching `mcp` tool entry).
            // No equivalent on Bedrock — fall back to forcing any tool.
            tracing::warn!("MCP tool choice without a hosted rewrite; falling back to `any`");
            Some(BedrockToolChoice::Any {})
        }
    })
}

/// Convert Bedrock Converse response to OpenAI Responses API format.
pub(super) fn convert_bedrock_to_responses_response(
    bedrock: BedrockConverseResponse,
    model: &str,
    reasoning_config: Option<&ResponsesReasoningConfig>,
    user: Option<String>,
) -> CreateResponsesResponse {
    let mut output: Vec<ResponsesOutputItem> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_text: Option<String> = None;
    let mut thinking_signature: Option<String> = None;

    // Generate a response ID for deriving other IDs
    let response_id = uuid::Uuid::new_v4().simple().to_string();

    // Process content blocks
    for block in &bedrock.output.message.content {
        if let Some(text) = &block.text {
            text_parts.push(text.clone());
        }
        if let Some(tool_use) = &block.tool_use {
            output.push(ResponsesOutputItem::FunctionCall(OutputItemFunctionCall {
                type_: OutputItemFunctionCallType::FunctionCall,
                id: Some(tool_use.tool_use_id.clone()),
                name: tool_use.name.clone(),
                arguments: serde_json::to_string(&tool_use.input).unwrap_or_default(),
                call_id: tool_use.tool_use_id.clone(),
                status: Some(OutputItemFunctionCallStatus::Completed),
            }));
        }
        // Extract reasoning content from extended thinking (Claude 4+ / Nova models)
        if let Some(reasoning) = &block.reasoning_content
            && let Some(reasoning_text) = &reasoning.reasoning_text
        {
            thinking_text = Some(reasoning_text.text.clone());
            thinking_signature = reasoning_text.signature.clone();
        }
    }

    // Add reasoning output if thinking was present
    if let Some(thinking) = &thinking_text {
        output.push(ResponsesOutputItem::Reasoning(ResponsesReasoning {
            type_: ResponsesReasoningType::Reasoning,
            id: format!("rs_{}", &response_id[..24.min(response_id.len())]),
            content: None,   // Bedrock doesn't provide structured reasoning content
            summary: vec![], // Would need to generate summary
            encrypted_content: None,
            status: None,
            signature: thinking_signature.clone(),
            // Use Anthropic format for Claude models, as Bedrock Claude uses the same thinking format
            format: if is_claude_model(model) {
                Some(OpenResponsesReasoningFormat::AnthropicClaudeV1)
            } else {
                None
            },
        }));

        // Store the thinking text - in Responses API the thinking is typically not in output_text
        let _ = thinking; // Unused for now
    }

    // Create output message with text content
    let output_text = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(""))
    };

    // Add the message output
    let message_content: Vec<OutputMessageContentItem> = if let Some(ref text) = output_text {
        vec![OutputMessageContentItem::OutputText {
            text: text.clone(),
            annotations: vec![],
            logprobs: vec![],
        }]
    } else {
        vec![]
    };

    // Only add message if there's content or no function calls
    if !message_content.is_empty() || output.is_empty() {
        output.insert(
            0,
            ResponsesOutputItem::Message(OutputMessage {
                id: format!("msg_{}", &response_id[..24.min(response_id.len())]),
                type_: MessageType::Message,
                role: "assistant".to_string(),
                content: message_content,
                status: Some(OutputMessageStatus::Completed),
            }),
        );
    }

    // Determine status based on stop_reason
    let status = match bedrock.stop_reason.as_deref() {
        Some("end_turn") => ResponsesResponseStatus::Completed,
        Some("max_tokens") => ResponsesResponseStatus::Incomplete,
        Some("stop_sequence") => ResponsesResponseStatus::Completed,
        Some("tool_use") => ResponsesResponseStatus::Completed,
        Some("guardrail_intervened") => ResponsesResponseStatus::Failed,
        Some("content_filtered") => ResponsesResponseStatus::Failed,
        _ => ResponsesResponseStatus::Completed,
    };

    // Build reasoning config output if reasoning was requested
    let reasoning_output = reasoning_config.map(|config| ResponsesReasoningConfigOutput {
        effort: config.effort,
        summary: config.summary,
    });

    // Bedrock doesn't report reasoning tokens separately in usage, so we default to 0
    // Future: May need to track this if Bedrock adds reasoning token metrics
    let reasoning_tokens = 0;

    CreateResponsesResponse {
        id: format!("resp_{}", response_id),
        object: ResponseType::Response,
        created_at: Utc::now().timestamp() as f64,
        model: model.to_string(),
        status: Some(status),
        output,
        user,
        output_text,
        prompt_cache_key: None,
        safety_identifier: None,
        error: None,
        incomplete_details: None,
        usage: Some(ResponsesUsage {
            input_tokens: bedrock.usage.input_tokens,
            input_tokens_details: ResponsesUsageInputTokensDetails {
                cached_tokens: bedrock.usage.cache_read_input_tokens,
            },
            output_tokens: bedrock.usage.output_tokens,
            output_tokens_details: ResponsesUsageOutputTokensDetails { reasoning_tokens },
            total_tokens: bedrock.usage.input_tokens + bedrock.usage.output_tokens,
            cost: None,
            is_byok: None,
            cost_details: None,
        }),
        completed_at: None,
        max_tool_calls: None,
        top_logprobs: None,
        max_output_tokens: None,
        temperature: None,
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        instructions: None,
        metadata: None,
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        prompt: None,
        background: None,
        previous_response_id: None,
        reasoning: reasoning_output,
        service_tier: None,
        store: None,
        truncation: None,
        text: None,
    }
}

// ============================================================================
// Reasoning/Thinking Configuration Functions
// ============================================================================

/// Convert Chat Completion reasoning config to Bedrock Claude format.
///
/// For Anthropic Claude models on Bedrock, the thinking configuration uses
/// `reasoning_config` in `additionalModelRequestFields`.
///
/// For adaptive-capable models (Opus 4.6+), emits `{ "type": "adaptive" }` with
/// `anthropic_beta` and optional `output_config` for effort.
///
/// Maps effort levels to budget tokens (non-adaptive models):
/// - High -> 32000 tokens
/// - Medium -> 16000 tokens
/// - Low -> 8000 tokens
/// - Minimal -> 2048 tokens (raised to 1024 minimum)
/// - None -> Disabled
pub fn convert_chat_completion_reasoning_to_bedrock_claude(
    reasoning: Option<&CreateChatCompletionReasoning>,
    model: &str,
    interleaved_thinking_models: &[String],
) -> Option<serde_json::Value> {
    let reasoning = reasoning?;

    if let Some(effort) = reasoning.effort {
        if effort == ReasoningEffort::None {
            return Some(serde_json::json!({
                "reasoning_config": { "type": "disabled" }
            }));
        }

        // Adaptive-capable models use adaptive thinking with effort-based output config
        if supports_adaptive_thinking(model) {
            let anthropic_effort = match effort {
                ReasoningEffort::Minimal | ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
                ReasoningEffort::None => unreachable!(),
            };
            let mut config = serde_json::json!({
                "reasoning_config": { "type": "adaptive" },
                "output_config": { "effort": anthropic_effort }
            });
            if matches_interleaved_thinking_model(model, interleaved_thinking_models) {
                config["anthropic_beta"] = serde_json::json!(["interleaved-thinking-2025-05-14"]);
            }
            return Some(config);
        }

        let reasoning_config = match effort {
            ReasoningEffort::High => serde_json::json!({
                "type": "enabled",
                "budget_tokens": 32000
            }),
            ReasoningEffort::Medium => serde_json::json!({
                "type": "enabled",
                "budget_tokens": 16000
            }),
            ReasoningEffort::Low => serde_json::json!({
                "type": "enabled",
                "budget_tokens": 8000
            }),
            ReasoningEffort::Minimal => serde_json::json!({
                "type": "enabled",
                "budget_tokens": 2048
            }),
            ReasoningEffort::None => unreachable!(),
        };

        return Some(serde_json::json!({
            "reasoning_config": reasoning_config
        }));
    }

    None
}

/// Convert Chat Completion reasoning config to Bedrock Nova format.
///
/// For Amazon Nova models on Bedrock, the thinking configuration uses
/// `reasoningConfig` (camelCase) in `additionalModelRequestFields`:
/// ```json
/// {
///   "type": "enabled",
///   "maxReasoningEffort": "high"
/// }
/// ```
///
/// Maps effort levels:
/// - High -> "high" (also clears maxTokens, temperature, topP per Nova docs)
/// - Medium -> "medium"
/// - Low/Minimal -> "low"
/// - None -> Disabled
pub fn convert_chat_completion_reasoning_to_bedrock_nova(
    reasoning: Option<&CreateChatCompletionReasoning>,
) -> Option<serde_json::Value> {
    let reasoning = reasoning?;

    if let Some(effort) = reasoning.effort {
        let reasoning_config = match effort {
            ReasoningEffort::High => serde_json::json!({
                "type": "enabled",
                "maxReasoningEffort": "high"
            }),
            ReasoningEffort::Medium => serde_json::json!({
                "type": "enabled",
                "maxReasoningEffort": "medium"
            }),
            ReasoningEffort::Low | ReasoningEffort::Minimal => serde_json::json!({
                "type": "enabled",
                "maxReasoningEffort": "low"
            }),
            ReasoningEffort::None => serde_json::json!({
                "type": "disabled"
            }),
        };

        // Wrap in additionalModelRequestFields format (camelCase for Nova)
        return Some(serde_json::json!({
            "reasoningConfig": reasoning_config
        }));
    }

    None
}

/// Check if a model is an Anthropic Claude model (for Bedrock)
pub fn is_claude_model(model: &str) -> bool {
    model.contains("anthropic") || model.contains("claude")
}

/// Check if a model is an Amazon Nova model (for Bedrock)
pub fn is_nova_model(model: &str) -> bool {
    model.contains("amazon.nova")
}

/// Check if a Bedrock Claude model supports adaptive thinking (Opus 4.6+).
fn supports_adaptive_thinking(model: &str) -> bool {
    model.contains("opus-4-6") || model.contains("opus-4.6")
}

/// Whether the model matches a substring in the configured allowlist for
/// the `interleaved-thinking-2025-05-14` beta header. Bedrock-hosted Claude
/// models that don't accept the header reject the request, so the header is
/// gated by an opt-in list (matches the Anthropic provider's behaviour).
fn matches_interleaved_thinking_model(model: &str, allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|pattern| !pattern.is_empty() && model.contains(pattern))
}

/// Convert Responses API reasoning config to Bedrock Claude format.
///
/// For Anthropic Claude models on Bedrock, the thinking configuration uses
/// `reasoning_config` in `additionalModelRequestFields`.
///
/// For adaptive-capable models (Opus 4.6+), emits `{ "type": "adaptive" }` with
/// `anthropic_beta` and optional `output_config` for effort.
///
/// This is the Responses API version of `convert_chat_completion_reasoning_to_bedrock_claude`.
pub fn convert_responses_reasoning_to_bedrock_claude(
    reasoning: Option<&ResponsesReasoningConfig>,
    model: &str,
    interleaved_thinking_models: &[String],
) -> Option<serde_json::Value> {
    let reasoning = reasoning?;

    // Check if reasoning is explicitly disabled
    if reasoning.enabled == Some(false) {
        return Some(serde_json::json!({
            "reasoning_config": { "type": "disabled" }
        }));
    }

    // If enabled or effort is specified, calculate budget tokens
    if reasoning.enabled == Some(true)
        || reasoning.effort.is_some()
        || reasoning.max_tokens.is_some()
    {
        // For adaptive-capable models with no explicit max_tokens, use adaptive thinking
        if supports_adaptive_thinking(model) && reasoning.max_tokens.is_none() {
            let anthropic_effort = match reasoning.effort {
                Some(ResponsesReasoningEffort::None) => {
                    return Some(serde_json::json!({
                        "reasoning_config": { "type": "disabled" }
                    }));
                }
                Some(ResponsesReasoningEffort::Minimal) | Some(ResponsesReasoningEffort::Low) => {
                    "low"
                }
                Some(ResponsesReasoningEffort::Medium) | None => "medium",
                Some(ResponsesReasoningEffort::High) => "high",
            };
            let mut config = serde_json::json!({
                "reasoning_config": { "type": "adaptive" },
                "output_config": { "effort": anthropic_effort }
            });
            if matches_interleaved_thinking_model(model, interleaved_thinking_models) {
                config["anthropic_beta"] = serde_json::json!(["interleaved-thinking-2025-05-14"]);
            }
            return Some(config);
        }

        // Non-adaptive: use fixed budget tokens
        let budget_tokens = if let Some(max) = reasoning.max_tokens {
            max as u32
        } else {
            match reasoning.effort {
                Some(ResponsesReasoningEffort::High) => 32000,
                Some(ResponsesReasoningEffort::Medium) => 16000,
                Some(ResponsesReasoningEffort::Low) => 8000,
                Some(ResponsesReasoningEffort::Minimal) => 2048,
                Some(ResponsesReasoningEffort::None) => {
                    return Some(serde_json::json!({
                        "reasoning_config": { "type": "disabled" }
                    }));
                }
                None => 10000,
            }
        };

        // Minimum budget is 1024 tokens per Anthropic API requirements
        let budget_tokens = budget_tokens.max(1024);

        return Some(serde_json::json!({
            "reasoning_config": {
                "type": "enabled",
                "budget_tokens": budget_tokens
            }
        }));
    }

    None
}

/// Convert Responses API reasoning config to Bedrock Nova format.
///
/// For Amazon Nova models on Bedrock, the thinking configuration uses
/// `reasoningConfig` (camelCase) in `additionalModelRequestFields`.
///
/// This is the Responses API version of `convert_chat_completion_reasoning_to_bedrock_nova`.
pub fn convert_responses_reasoning_to_bedrock_nova(
    reasoning: Option<&ResponsesReasoningConfig>,
) -> Option<serde_json::Value> {
    let reasoning = reasoning?;

    // Check if reasoning is explicitly disabled
    if reasoning.enabled == Some(false) {
        return Some(serde_json::json!({
            "reasoningConfig": { "type": "disabled" }
        }));
    }

    // If enabled or effort is specified
    if reasoning.enabled == Some(true) || reasoning.effort.is_some() {
        let reasoning_config = match reasoning.effort {
            Some(ResponsesReasoningEffort::High) => serde_json::json!({
                "type": "enabled",
                "maxReasoningEffort": "high"
            }),
            Some(ResponsesReasoningEffort::Medium) => serde_json::json!({
                "type": "enabled",
                "maxReasoningEffort": "medium"
            }),
            Some(ResponsesReasoningEffort::Low) | Some(ResponsesReasoningEffort::Minimal) => {
                serde_json::json!({
                    "type": "enabled",
                    "maxReasoningEffort": "low"
                })
            }
            Some(ResponsesReasoningEffort::None) => serde_json::json!({
                "type": "disabled"
            }),
            None => serde_json::json!({
                "type": "enabled",
                "maxReasoningEffort": "medium"
            }),
        };

        return Some(serde_json::json!({
            "reasoningConfig": reasoning_config
        }));
    }

    None
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::api_types::chat_completion::ImageUrl;

    #[test]
    fn test_parse_data_url_in_convert_content() {
        // Test that convert_content_to_bedrock properly uses parse_data_url
        // The actual parse_data_url tests are in src/providers/image.rs

        // Valid data URL should be converted to image block
        let content = MessageContent::Parts(vec![ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                detail: None,
            },
            cache_control: None,
        }]);
        let blocks = convert_content_to_bedrock(&content);
        assert_eq!(blocks.len(), 1);
        let image = blocks[0].image.as_ref().unwrap();
        assert_eq!(image.format, "png");
        assert_eq!(image.source.bytes, "iVBORw0KGgo=");

        // Invalid data URL should be skipped
        let content = MessageContent::Parts(vec![ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "https://example.com/image.png".to_string(),
                detail: None,
            },
            cache_control: None,
        }]);
        let blocks = convert_content_to_bedrock(&content);
        assert!(blocks.is_empty()); // HTTP URLs are skipped (should be preprocessed)
    }

    #[test]
    fn test_media_type_to_bedrock_format() {
        assert_eq!(
            media_type_to_bedrock_format("image/png"),
            Some("png".to_string())
        );
        assert_eq!(
            media_type_to_bedrock_format("image/jpeg"),
            Some("jpeg".to_string())
        );
        assert_eq!(
            media_type_to_bedrock_format("image/jpg"),
            Some("jpeg".to_string())
        );
        assert_eq!(
            media_type_to_bedrock_format("image/gif"),
            Some("gif".to_string())
        );
        assert_eq!(
            media_type_to_bedrock_format("image/webp"),
            Some("webp".to_string())
        );
        assert_eq!(media_type_to_bedrock_format("image/bmp"), None);
        assert_eq!(media_type_to_bedrock_format("text/plain"), None);
    }

    #[test]
    fn test_convert_content_to_bedrock_text_only() {
        let content = MessageContent::Text("Hello world".to_string());
        let blocks = convert_content_to_bedrock(&content);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, Some("Hello world".to_string()));
        assert!(blocks[0].image.is_none());
    }

    #[test]
    fn test_convert_content_to_bedrock_empty() {
        let content = MessageContent::Text("".to_string());
        let blocks = convert_content_to_bedrock(&content);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_convert_content_to_bedrock_with_image() {
        let content = MessageContent::Parts(vec![
            ContentPart::Text {
                text: "What's in this image?".to_string(),
                cache_control: None,
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                    detail: None,
                },
                cache_control: None,
            },
        ]);

        let blocks = convert_content_to_bedrock(&content);
        assert_eq!(blocks.len(), 2);

        // First block is text
        assert_eq!(blocks[0].text, Some("What's in this image?".to_string()));
        assert!(blocks[0].image.is_none());

        // Second block is image
        assert!(blocks[1].text.is_none());
        let image = blocks[1].image.as_ref().unwrap();
        assert_eq!(image.format, "png");
        assert_eq!(image.source.bytes, "iVBORw0KGgo=");
    }

    #[test]
    fn test_convert_messages_with_image() {
        let messages = vec![Message::User {
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Describe this image".to_string(),
                    cache_control: None,
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/jpeg;base64,/9j/4AAQ".to_string(),
                        detail: None,
                    },
                    cache_control: None,
                },
            ]),
            name: None,
        }];

        let (_, bedrock_msgs) = convert_messages(messages);
        assert_eq!(bedrock_msgs.len(), 1);
        assert_eq!(bedrock_msgs[0].role, "user");
        assert_eq!(bedrock_msgs[0].content.len(), 2);

        // First content is text
        assert_eq!(
            bedrock_msgs[0].content[0].text,
            Some("Describe this image".to_string())
        );

        // Second content is image
        let image = bedrock_msgs[0].content[1].image.as_ref().unwrap();
        assert_eq!(image.format, "jpeg");
        assert_eq!(image.source.bytes, "/9j/4AAQ");
    }
}

#[cfg(test)]
mod finish_reason_tests {
    use super::*;

    fn create_bedrock_response(stop_reason: &str) -> BedrockConverseResponse {
        BedrockConverseResponse {
            output: BedrockOutput {
                message: BedrockOutputMessage {
                    role: "assistant".to_string(),
                    content: vec![BedrockOutputContent {
                        text: Some("Test response".to_string()),
                        tool_use: None,
                        reasoning_content: None,
                    }],
                },
            },
            stop_reason: Some(stop_reason.to_string()),
            usage: BedrockUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            },
        }
    }

    #[test]
    fn test_finish_reason_end_turn() {
        let response = create_bedrock_response("end_turn");
        let openai = convert_response(response, "test-model");
        assert_eq!(openai.choices[0].finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn test_finish_reason_max_tokens() {
        let response = create_bedrock_response("max_tokens");
        let openai = convert_response(response, "test-model");
        assert_eq!(openai.choices[0].finish_reason, Some("length".to_string()));
    }

    #[test]
    fn test_finish_reason_tool_use() {
        let response = create_bedrock_response("tool_use");
        let openai = convert_response(response, "test-model");
        assert_eq!(
            openai.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
    }

    #[test]
    fn test_finish_reason_guardrail_intervened() {
        let response = create_bedrock_response("guardrail_intervened");
        let openai = convert_response(response, "test-model");
        assert_eq!(
            openai.choices[0].finish_reason,
            Some("content_filter".to_string())
        );
    }

    #[test]
    fn test_finish_reason_content_filtered() {
        let response = create_bedrock_response("content_filtered");
        let openai = convert_response(response, "test-model");
        assert_eq!(
            openai.choices[0].finish_reason,
            Some("content_filter".to_string())
        );
    }

    #[test]
    fn test_bedrock_usage_with_cache_tokens() {
        let json = r#"{
            "inputTokens": 100,
            "outputTokens": 50,
            "cacheReadInputTokens": 25,
            "cacheWriteInputTokens": 10
        }"#;

        let usage: BedrockUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 25);
        assert_eq!(usage.cache_write_input_tokens, 10);
    }

    #[test]
    fn test_bedrock_usage_without_cache_tokens() {
        // Cache tokens should default to 0 when not present
        let json = r#"{
            "inputTokens": 100,
            "outputTokens": 50
        }"#;

        let usage: BedrockUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.cache_write_input_tokens, 0);
    }

    #[test]
    fn test_convert_bedrock_to_openai_with_cache_tokens() {
        let response = BedrockConverseResponse {
            output: BedrockOutput {
                message: BedrockOutputMessage {
                    role: "assistant".to_string(),
                    content: vec![BedrockOutputContent {
                        text: Some("Hello!".to_string()),
                        tool_use: None,
                        reasoning_content: None,
                    }],
                },
            },
            usage: BedrockUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 25,
                cache_write_input_tokens: 10,
            },
            stop_reason: Some("end_turn".to_string()),
        };

        let openai = convert_response(response, "test-model");

        let usage = openai.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);

        // Should have cache tokens
        let details = usage.prompt_tokens_details.unwrap();
        assert_eq!(details.cached_tokens, 25);
    }

    #[test]
    fn test_convert_bedrock_to_openai_without_cache_tokens() {
        let response = BedrockConverseResponse {
            output: BedrockOutput {
                message: BedrockOutputMessage {
                    role: "assistant".to_string(),
                    content: vec![BedrockOutputContent {
                        text: Some("Hello!".to_string()),
                        tool_use: None,
                        reasoning_content: None,
                    }],
                },
            },
            usage: BedrockUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            },
            stop_reason: Some("end_turn".to_string()),
        };

        let openai = convert_response(response, "test-model");

        let usage = openai.usage.unwrap();
        // Should NOT have cache tokens when they're 0
        assert!(usage.prompt_tokens_details.is_none());
    }
}

#[cfg(test)]
mod tool_result_status_tests {
    use super::*;
    use crate::api_types::responses::{FunctionCallOutput, FunctionCallOutputType, ToolCallStatus};

    #[test]
    fn test_tool_result_status_serialization() {
        // Test that BedrockToolResultStatus serializes correctly
        let success_json = serde_json::to_string(&BedrockToolResultStatus::Success).unwrap();
        assert_eq!(success_json, "\"success\"");

        let error_json = serde_json::to_string(&BedrockToolResultStatus::Error).unwrap();
        assert_eq!(error_json, "\"error\"");
    }

    #[test]
    fn test_tool_result_with_status() {
        // Test that BedrockContent::tool_result includes status in serialized output
        let content = BedrockContent::tool_result(
            "tool-123".to_string(),
            "result".to_string(),
            Some(BedrockToolResultStatus::Success),
        );

        let json = serde_json::to_value(&content).unwrap();
        let tool_result = json.get("toolResult").unwrap();

        assert_eq!(tool_result.get("toolUseId").unwrap(), "tool-123");
        assert_eq!(tool_result.get("status").unwrap(), "success");
    }

    #[test]
    fn test_tool_result_with_error_status() {
        let content = BedrockContent::tool_result(
            "tool-456".to_string(),
            "error message".to_string(),
            Some(BedrockToolResultStatus::Error),
        );

        let json = serde_json::to_value(&content).unwrap();
        let tool_result = json.get("toolResult").unwrap();

        assert_eq!(tool_result.get("status").unwrap(), "error");
    }

    #[test]
    fn test_tool_result_without_status() {
        // Test that status is omitted when None (skip_serializing_if)
        let content =
            BedrockContent::tool_result("tool-789".to_string(), "result".to_string(), None);

        let json = serde_json::to_value(&content).unwrap();
        let tool_result = json.get("toolResult").unwrap();

        assert!(tool_result.get("status").is_none());
    }

    #[test]
    fn test_convert_messages_tool_result_has_success_status() {
        use crate::api_types::chat_completion::{ToolCall, ToolCallFunction, ToolType};

        // Chat Completions API tool messages should have success status
        let messages = vec![
            Message::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call-123".to_string(),
                    type_: ToolType::Function,
                    function: ToolCallFunction {
                        name: "get_weather".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                refusal: None,
                name: None,
                reasoning: None,
            },
            Message::Tool {
                tool_call_id: "call-123".to_string(),
                content: MessageContent::Text("sunny".to_string()),
            },
        ];

        let (_, bedrock_msgs) = convert_messages(messages);

        // The tool result should be in the second message (user role)
        assert_eq!(bedrock_msgs.len(), 2);
        assert_eq!(bedrock_msgs[1].role, "user");

        let json = serde_json::to_value(&bedrock_msgs[1].content[0]).unwrap();
        let tool_result = json.get("toolResult").unwrap();
        assert_eq!(tool_result.get("status").unwrap(), "success");
    }

    #[test]
    fn test_responses_function_call_output_completed_maps_to_success() {
        let output = FunctionCallOutput {
            type_: FunctionCallOutputType::FunctionCallOutput,
            id: Some("id-1".to_string()),
            call_id: "call-1".to_string(),
            output: "result".to_string(),
            status: Some(ToolCallStatus::Completed),
        };

        let input = ResponsesInput::Items(vec![ResponsesInputItem::FunctionCallOutput(output)]);
        let (_, messages) = convert_responses_input_to_bedrock_messages(Some(input), None);

        assert_eq!(messages.len(), 1);
        let json = serde_json::to_value(&messages[0].content[0]).unwrap();
        let tool_result = json.get("toolResult").unwrap();
        assert_eq!(tool_result.get("status").unwrap(), "success");
    }

    #[test]
    fn test_responses_function_call_output_incomplete_maps_to_error() {
        let output = FunctionCallOutput {
            type_: FunctionCallOutputType::FunctionCallOutput,
            id: Some("id-2".to_string()),
            call_id: "call-2".to_string(),
            output: "error: timeout".to_string(),
            status: Some(ToolCallStatus::Incomplete),
        };

        let input = ResponsesInput::Items(vec![ResponsesInputItem::FunctionCallOutput(output)]);
        let (_, messages) = convert_responses_input_to_bedrock_messages(Some(input), None);

        assert_eq!(messages.len(), 1);
        let json = serde_json::to_value(&messages[0].content[0]).unwrap();
        let tool_result = json.get("toolResult").unwrap();
        assert_eq!(tool_result.get("status").unwrap(), "error");
    }

    #[test]
    fn test_responses_function_call_output_none_status_maps_to_success() {
        let output = FunctionCallOutput {
            type_: FunctionCallOutputType::FunctionCallOutput,
            id: None,
            call_id: "call-3".to_string(),
            output: "result".to_string(),
            status: None,
        };

        let input = ResponsesInput::Items(vec![ResponsesInputItem::FunctionCallOutput(output)]);
        let (_, messages) = convert_responses_input_to_bedrock_messages(Some(input), None);

        assert_eq!(messages.len(), 1);
        let json = serde_json::to_value(&messages[0].content[0]).unwrap();
        let tool_result = json.get("toolResult").unwrap();
        assert_eq!(tool_result.get("status").unwrap(), "success");
    }
}

#[cfg(test)]
mod cache_control_tests {
    use super::*;
    use crate::api_types::{
        chat_completion::{
            CacheControl, CacheControlType, ContentPart, ImageUrl, MessageContent, ToolDefinition,
            ToolDefinitionFunction, ToolType,
        },
        responses::{
            FileSearchTool, FileSearchToolType, FunctionTool, ResponseInputContentItem,
            ResponseInputImageDetail, ResponsesToolDefinition,
        },
    };

    // =========================================================================
    // Chat Completions API - Content with cache_control
    // =========================================================================

    #[test]
    fn test_convert_content_to_bedrock_text_with_cache_control() {
        let content = MessageContent::Parts(vec![ContentPart::Text {
            text: "Cache this text".to_string(),
            cache_control: Some(CacheControl {
                type_: CacheControlType::Ephemeral,
            }),
        }]);

        let blocks = convert_content_to_bedrock(&content);

        // Should have 2 blocks: text + cache point
        assert_eq!(blocks.len(), 2);

        // First block is the text
        let json = serde_json::to_value(&blocks[0]).unwrap();
        assert_eq!(json.get("text").unwrap(), "Cache this text");
        assert!(json.get("cachePoint").is_none());

        // Second block is the cache point
        let json = serde_json::to_value(&blocks[1]).unwrap();
        assert!(json.get("text").is_none());
        let cache_point = json.get("cachePoint").unwrap();
        assert_eq!(cache_point.get("type").unwrap(), "default");
    }

    #[test]
    fn test_convert_content_to_bedrock_text_without_cache_control() {
        let content = MessageContent::Parts(vec![ContentPart::Text {
            text: "No cache".to_string(),
            cache_control: None,
        }]);

        let blocks = convert_content_to_bedrock(&content);

        // Should have only 1 block: text (no cache point)
        assert_eq!(blocks.len(), 1);
        let json = serde_json::to_value(&blocks[0]).unwrap();
        assert_eq!(json.get("text").unwrap(), "No cache");
        assert!(json.get("cachePoint").is_none());
    }

    #[test]
    fn test_convert_content_to_bedrock_image_with_cache_control() {
        let content = MessageContent::Parts(vec![ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                detail: None,
            },
            cache_control: Some(CacheControl {
                type_: CacheControlType::Ephemeral,
            }),
        }]);

        let blocks = convert_content_to_bedrock(&content);

        // Should have 2 blocks: image + cache point
        assert_eq!(blocks.len(), 2);

        // First block is the image
        let json = serde_json::to_value(&blocks[0]).unwrap();
        assert!(json.get("image").is_some());
        assert!(json.get("cachePoint").is_none());

        // Second block is the cache point
        let json = serde_json::to_value(&blocks[1]).unwrap();
        let cache_point = json.get("cachePoint").unwrap();
        assert_eq!(cache_point.get("type").unwrap(), "default");
    }

    #[test]
    fn test_convert_content_to_bedrock_multiple_parts_with_cache_control() {
        let content = MessageContent::Parts(vec![
            ContentPart::Text {
                text: "First part".to_string(),
                cache_control: None,
            },
            ContentPart::Text {
                text: "Second part - cached".to_string(),
                cache_control: Some(CacheControl {
                    type_: CacheControlType::Ephemeral,
                }),
            },
            ContentPart::Text {
                text: "Third part".to_string(),
                cache_control: None,
            },
        ]);

        let blocks = convert_content_to_bedrock(&content);

        // Should have 4 blocks: text1, text2, cache_point, text3
        assert_eq!(blocks.len(), 4);

        // Verify cache point is after second text
        let json = serde_json::to_value(&blocks[0]).unwrap();
        assert_eq!(json.get("text").unwrap(), "First part");

        let json = serde_json::to_value(&blocks[1]).unwrap();
        assert_eq!(json.get("text").unwrap(), "Second part - cached");

        let json = serde_json::to_value(&blocks[2]).unwrap();
        assert!(json.get("cachePoint").is_some());

        let json = serde_json::to_value(&blocks[3]).unwrap();
        assert_eq!(json.get("text").unwrap(), "Third part");
    }

    // =========================================================================
    // Chat Completions API - Tools with cache_control
    // =========================================================================

    #[test]
    fn test_convert_tools_with_cache_control() {
        let tools = Some(vec![ToolDefinition {
            type_: ToolType::Function,
            function: ToolDefinitionFunction {
                name: "get_weather".to_string(),
                description: Some("Get weather info".to_string()),
                parameters: Some(serde_json::json!({"type": "object", "properties": {}})),
                strict: None,
            },
            cache_control: Some(CacheControl {
                type_: CacheControlType::Ephemeral,
            }),
        }]);

        let result = convert_tools(tools);
        assert!(result.is_some());
        let bedrock_tools = result.unwrap();

        // Should have 2 entries: tool + cache point
        assert_eq!(bedrock_tools.len(), 2);

        // First is the tool spec
        let json = serde_json::to_value(&bedrock_tools[0]).unwrap();
        let tool_spec = json.get("toolSpec").unwrap();
        assert_eq!(tool_spec.get("name").unwrap(), "get_weather");
        assert!(json.get("cachePoint").is_none());

        // Second is the cache point
        let json = serde_json::to_value(&bedrock_tools[1]).unwrap();
        assert!(json.get("toolSpec").is_none());
        let cache_point = json.get("cachePoint").unwrap();
        assert_eq!(cache_point.get("type").unwrap(), "default");
    }

    #[test]
    fn test_convert_tools_without_cache_control() {
        let tools = Some(vec![ToolDefinition {
            type_: ToolType::Function,
            function: ToolDefinitionFunction {
                name: "get_weather".to_string(),
                description: Some("Get weather info".to_string()),
                parameters: None,
                strict: None,
            },
            cache_control: None,
        }]);

        let result = convert_tools(tools);
        assert!(result.is_some());
        let bedrock_tools = result.unwrap();

        // Should have only 1 entry: the tool (no cache point)
        assert_eq!(bedrock_tools.len(), 1);

        let json = serde_json::to_value(&bedrock_tools[0]).unwrap();
        assert!(json.get("toolSpec").is_some());
        assert!(json.get("cachePoint").is_none());
    }

    #[test]
    fn test_convert_tools_multiple_with_selective_cache_control() {
        let tools = Some(vec![
            ToolDefinition {
                type_: ToolType::Function,
                function: ToolDefinitionFunction {
                    name: "tool1".to_string(),
                    description: None,
                    parameters: None,
                    strict: None,
                },
                cache_control: None,
            },
            ToolDefinition {
                type_: ToolType::Function,
                function: ToolDefinitionFunction {
                    name: "tool2".to_string(),
                    description: None,
                    parameters: None,
                    strict: None,
                },
                cache_control: Some(CacheControl {
                    type_: CacheControlType::Ephemeral,
                }),
            },
        ]);

        let result = convert_tools(tools);
        assert!(result.is_some());
        let bedrock_tools = result.unwrap();

        // Should have 3 entries: tool1, tool2, cache_point
        assert_eq!(bedrock_tools.len(), 3);

        // Verify structure
        let json = serde_json::to_value(&bedrock_tools[0]).unwrap();
        assert_eq!(json.get("toolSpec").unwrap().get("name").unwrap(), "tool1");

        let json = serde_json::to_value(&bedrock_tools[1]).unwrap();
        assert_eq!(json.get("toolSpec").unwrap().get("name").unwrap(), "tool2");

        let json = serde_json::to_value(&bedrock_tools[2]).unwrap();
        assert!(json.get("cachePoint").is_some());
    }

    // =========================================================================
    // Chat Completions API - System messages with cache_control
    // =========================================================================

    #[test]
    fn test_convert_messages_system_with_cache_control() {
        let messages = vec![
            Message::System {
                content: MessageContent::Parts(vec![ContentPart::Text {
                    text: "You are a helpful assistant.".to_string(),
                    cache_control: Some(CacheControl {
                        type_: CacheControlType::Ephemeral,
                    }),
                }]),
                name: None,
            },
            Message::User {
                content: MessageContent::Text("Hello".to_string()),
                name: None,
            },
        ];

        let (system, bedrock_msgs) = convert_messages(messages);

        // System should have 2 blocks: text + cache point
        assert!(system.is_some());
        let system_blocks = system.unwrap();
        assert_eq!(system_blocks.len(), 2);

        // First block is text
        let json = serde_json::to_value(&system_blocks[0]).unwrap();
        assert_eq!(json.get("text").unwrap(), "You are a helpful assistant.");
        assert!(json.get("cachePoint").is_none());

        // Second block is cache point
        let json = serde_json::to_value(&system_blocks[1]).unwrap();
        assert!(json.get("text").is_none());
        let cache_point = json.get("cachePoint").unwrap();
        assert_eq!(cache_point.get("type").unwrap(), "default");

        // User message should be present
        assert_eq!(bedrock_msgs.len(), 1);
        assert_eq!(bedrock_msgs[0].role, "user");
    }

    #[test]
    fn test_convert_messages_developer_with_cache_control() {
        let messages = vec![
            Message::Developer {
                content: MessageContent::Parts(vec![ContentPart::Text {
                    text: "Developer instructions".to_string(),
                    cache_control: Some(CacheControl {
                        type_: CacheControlType::Ephemeral,
                    }),
                }]),
                name: None,
            },
            Message::User {
                content: MessageContent::Text("Hello".to_string()),
                name: None,
            },
        ];

        let (system, _) = convert_messages(messages);

        // Developer message is treated as system, should have cache point
        assert!(system.is_some());
        let system_blocks = system.unwrap();
        assert_eq!(system_blocks.len(), 2);

        let json = serde_json::to_value(&system_blocks[1]).unwrap();
        assert!(json.get("cachePoint").is_some());
    }

    // =========================================================================
    // Responses API - Content with cache_control
    // =========================================================================

    #[test]
    fn test_convert_responses_content_to_bedrock_text_with_cache_control() {
        let items = vec![ResponseInputContentItem::InputText {
            text: "Cache this".to_string(),
            cache_control: Some(CacheControl {
                type_: CacheControlType::Ephemeral,
            }),
        }];

        let blocks = convert_responses_content_to_bedrock(&items);

        // Should have 2 blocks: text + cache point
        assert_eq!(blocks.len(), 2);

        let json = serde_json::to_value(&blocks[0]).unwrap();
        assert_eq!(json.get("text").unwrap(), "Cache this");

        let json = serde_json::to_value(&blocks[1]).unwrap();
        assert!(json.get("cachePoint").is_some());
    }

    #[test]
    fn test_convert_responses_content_to_bedrock_image_with_cache_control() {
        let items = vec![ResponseInputContentItem::InputImage {
            detail: ResponseInputImageDetail::Auto,
            image_url: Some("data:image/png;base64,iVBORw0KGgo=".to_string()),
            cache_control: Some(CacheControl {
                type_: CacheControlType::Ephemeral,
            }),
        }];

        let blocks = convert_responses_content_to_bedrock(&items);

        // Should have 2 blocks: image + cache point
        assert_eq!(blocks.len(), 2);

        let json = serde_json::to_value(&blocks[0]).unwrap();
        assert!(json.get("image").is_some());

        let json = serde_json::to_value(&blocks[1]).unwrap();
        assert!(json.get("cachePoint").is_some());
    }

    #[test]
    fn test_convert_responses_content_to_bedrock_without_cache_control() {
        let items = vec![ResponseInputContentItem::InputText {
            text: "No cache".to_string(),
            cache_control: None,
        }];

        let blocks = convert_responses_content_to_bedrock(&items);

        // Should have only 1 block (no cache point)
        assert_eq!(blocks.len(), 1);

        let json = serde_json::to_value(&blocks[0]).unwrap();
        assert!(json.get("cachePoint").is_none());
    }

    // =========================================================================
    // Responses API - Tools with cache_control
    // =========================================================================

    #[test]
    fn test_convert_responses_tools_function_with_cache_control() {
        let tools = Some(vec![ResponsesToolDefinition::Function(
            FunctionTool::from_json(serde_json::json!({
                "type": "function",
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object", "properties": {}},
                "cache_control": {"type": "ephemeral"}
            }))
            .unwrap(),
        )]);

        let result = convert_responses_tools_to_bedrock(tools);
        assert!(result.is_some());
        let bedrock_tools = result.unwrap();

        // Should have 2 entries: tool + cache point
        assert_eq!(bedrock_tools.len(), 2);

        let json = serde_json::to_value(&bedrock_tools[0]).unwrap();
        assert!(json.get("toolSpec").is_some());

        let json = serde_json::to_value(&bedrock_tools[1]).unwrap();
        assert!(json.get("cachePoint").is_some());
    }

    #[test]
    fn test_convert_responses_tools_file_search_with_cache_control() {
        let tools = Some(vec![ResponsesToolDefinition::FileSearch(FileSearchTool {
            type_: FileSearchToolType::FileSearch,
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            ranking_options: None,
            filters: None,
            cache_control: Some(CacheControl {
                type_: CacheControlType::Ephemeral,
            }),
        })]);

        let result = convert_responses_tools_to_bedrock(tools);
        assert!(result.is_some());
        let bedrock_tools = result.unwrap();

        // Should have 2 entries: file_search tool + cache point
        assert_eq!(bedrock_tools.len(), 2);

        // First is the converted file_search function
        let json = serde_json::to_value(&bedrock_tools[0]).unwrap();
        let tool_spec = json.get("toolSpec").unwrap();
        assert_eq!(tool_spec.get("name").unwrap(), "file_search");

        // Second is the cache point
        let json = serde_json::to_value(&bedrock_tools[1]).unwrap();
        assert!(json.get("cachePoint").is_some());
    }

    #[test]
    fn test_convert_responses_tools_function_without_cache_control() {
        let tools = Some(vec![ResponsesToolDefinition::Function(
            FunctionTool::from_json(serde_json::json!({
                "type": "function",
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object", "properties": {}}
            }))
            .unwrap(),
        )]);

        let result = convert_responses_tools_to_bedrock(tools);
        assert!(result.is_some());
        let bedrock_tools = result.unwrap();

        // Should have only 1 entry (no cache point)
        assert_eq!(bedrock_tools.len(), 1);

        let json = serde_json::to_value(&bedrock_tools[0]).unwrap();
        assert!(json.get("toolSpec").is_some());
        assert!(json.get("cachePoint").is_none());
    }

    // =========================================================================
    // Cache point type serialization
    // =========================================================================

    #[test]
    fn test_cache_point_type_serialization() {
        let cache_point = BedrockCachePoint::default();
        let json = serde_json::to_value(&cache_point).unwrap();

        // Should serialize as {"type": "default"}
        assert_eq!(json.get("type").unwrap(), "default");
    }

    #[test]
    fn test_bedrock_content_cache_point_serialization() {
        let content = BedrockContent::cache_point();
        let json = serde_json::to_value(&content).unwrap();

        // Should have only cachePoint field (others are None with skip_serializing_if)
        assert!(json.get("text").is_none());
        assert!(json.get("image").is_none());
        assert!(json.get("toolUse").is_none());
        assert!(json.get("toolResult").is_none());

        let cache_point = json.get("cachePoint").unwrap();
        assert_eq!(cache_point.get("type").unwrap(), "default");
    }

    #[test]
    fn test_bedrock_system_content_cache_point_serialization() {
        let content = BedrockSystemContent::cache_point();
        let json = serde_json::to_value(&content).unwrap();

        // Should have only cachePoint field
        assert!(json.get("text").is_none());

        let cache_point = json.get("cachePoint").unwrap();
        assert_eq!(cache_point.get("type").unwrap(), "default");
    }

    #[test]
    fn test_bedrock_tool_cache_point_serialization() {
        let tool = BedrockTool::cache_point();
        let json = serde_json::to_value(&tool).unwrap();

        // Should have only cachePoint field
        assert!(json.get("toolSpec").is_none());

        let cache_point = json.get("cachePoint").unwrap();
        assert_eq!(cache_point.get("type").unwrap(), "default");
    }
}

#[cfg(test)]
mod reasoning_tests {
    use super::*;

    // =========================================================================
    // Responses API Reasoning Config Conversion
    // =========================================================================

    #[test]
    fn test_convert_responses_reasoning_to_bedrock_claude_high_effort() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            enabled: None,
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 32000);
    }

    #[test]
    fn test_convert_responses_reasoning_to_bedrock_claude_medium_effort() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::Medium),
            summary: None,
            enabled: None,
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 16000);
    }

    #[test]
    fn test_convert_responses_reasoning_to_bedrock_claude_none_effort() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::None),
            summary: None,
            enabled: None,
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "disabled");
    }

    #[test]
    fn test_convert_responses_reasoning_to_bedrock_claude_explicitly_disabled() {
        let config = ResponsesReasoningConfig {
            effort: None,
            summary: None,
            enabled: Some(false),
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "disabled");
    }

    #[test]
    fn test_convert_responses_reasoning_to_bedrock_claude_with_max_tokens() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            enabled: None,
            max_tokens: Some(5000.0),
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        // max_tokens takes precedence over effort
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 5000);
    }

    #[test]
    fn test_convert_responses_reasoning_to_bedrock_nova_high_effort() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            enabled: None,
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoningConfig").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("maxReasoningEffort").unwrap(), "high");
    }

    #[test]
    fn test_convert_responses_reasoning_to_bedrock_nova_low_effort() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::Low),
            summary: None,
            enabled: None,
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoningConfig").unwrap();
        assert_eq!(reasoning_config.get("maxReasoningEffort").unwrap(), "low");
    }

    // =========================================================================
    // Responses API Response Conversion with Reasoning
    // =========================================================================

    fn create_bedrock_response_with_reasoning(
        text: &str,
        reasoning_text: &str,
    ) -> BedrockConverseResponse {
        BedrockConverseResponse {
            output: BedrockOutput {
                message: BedrockOutputMessage {
                    role: "assistant".to_string(),
                    content: vec![BedrockOutputContent {
                        text: Some(text.to_string()),
                        tool_use: None,
                        reasoning_content: Some(BedrockReasoningContent {
                            reasoning_text: Some(BedrockReasoningText {
                                text: reasoning_text.to_string(),
                                signature: Some("sig_test123".to_string()),
                            }),
                        }),
                    }],
                },
            },
            stop_reason: Some("end_turn".to_string()),
            usage: BedrockUsage {
                input_tokens: 100,
                output_tokens: 200,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            },
        }
    }

    #[test]
    fn test_convert_bedrock_to_responses_response_with_reasoning_claude() {
        let response = create_bedrock_response_with_reasoning(
            "The answer is 42.",
            "Let me think about this step by step...",
        );

        let reasoning_config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            enabled: None,
            max_tokens: None,
        };

        let result = convert_bedrock_to_responses_response(
            response,
            "anthropic.claude-3-opus-20240229-v1:0",
            Some(&reasoning_config),
            None,
        );

        // Should have 2 output items: message and reasoning
        assert_eq!(result.output.len(), 2);

        // First should be the message (inserted at index 0)
        match &result.output[0] {
            ResponsesOutputItem::Message(msg) => {
                assert_eq!(msg.content.len(), 1);
                match &msg.content[0] {
                    OutputMessageContentItem::OutputText { text, .. } => {
                        assert_eq!(text, "The answer is 42.");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message at index 0"),
        }

        // Second should be reasoning
        match &result.output[1] {
            ResponsesOutputItem::Reasoning(reasoning) => {
                assert_eq!(reasoning.type_, ResponsesReasoningType::Reasoning);
                assert_eq!(reasoning.signature, Some("sig_test123".to_string()));
                // Should use Anthropic format for Claude models
                assert_eq!(
                    reasoning.format,
                    Some(OpenResponsesReasoningFormat::AnthropicClaudeV1)
                );
            }
            _ => panic!("Expected Reasoning at index 1"),
        }

        // Reasoning config should be echoed back
        assert!(result.reasoning.is_some());
        let reasoning_output = result.reasoning.unwrap();
        assert_eq!(
            reasoning_output.effort,
            Some(ResponsesReasoningEffort::High)
        );
    }

    #[test]
    fn test_convert_bedrock_to_responses_response_without_reasoning() {
        let response = BedrockConverseResponse {
            output: BedrockOutput {
                message: BedrockOutputMessage {
                    role: "assistant".to_string(),
                    content: vec![BedrockOutputContent {
                        text: Some("Hello world".to_string()),
                        tool_use: None,
                        reasoning_content: None,
                    }],
                },
            },
            stop_reason: Some("end_turn".to_string()),
            usage: BedrockUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            },
        };

        let result = convert_bedrock_to_responses_response(
            response,
            "anthropic.claude-3-sonnet",
            None,
            None,
        );

        // Should have only 1 output item: message
        assert_eq!(result.output.len(), 1);

        match &result.output[0] {
            ResponsesOutputItem::Message(msg) => {
                assert_eq!(msg.content.len(), 1);
            }
            _ => panic!("Expected Message"),
        }

        // No reasoning config should be in output
        assert!(result.reasoning.is_none());
    }

    #[test]
    fn test_convert_bedrock_to_responses_response_nova_model_no_format() {
        let response = create_bedrock_response_with_reasoning("Result", "Nova thinking...");

        let result =
            convert_bedrock_to_responses_response(response, "amazon.nova-pro-v1:0", None, None);

        // Should have 2 output items
        assert_eq!(result.output.len(), 2);

        // Reasoning should not have Anthropic format for Nova models
        match &result.output[1] {
            ResponsesOutputItem::Reasoning(reasoning) => {
                // Nova models don't use Anthropic format
                assert_eq!(reasoning.format, None);
            }
            _ => panic!("Expected Reasoning"),
        }
    }

    // =========================================================================
    // Chat Completion API Reasoning Config Conversion - Claude
    // =========================================================================

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_claude_high_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::High),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 32000);
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_claude_medium_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Medium),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 16000);
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_claude_low_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Low),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 8000);
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_claude_minimal_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Minimal),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 2048);
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_claude_none_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::None),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "disabled");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_claude_no_effort_returns_none() {
        let config = CreateChatCompletionReasoning {
            effort: None,
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_claude_none_input_returns_none() {
        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            None,
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_none());
    }

    // =========================================================================
    // Chat Completion API Reasoning Config Conversion - Nova
    // =========================================================================

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_nova_high_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::High),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoningConfig").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("maxReasoningEffort").unwrap(), "high");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_nova_medium_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Medium),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoningConfig").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(
            reasoning_config.get("maxReasoningEffort").unwrap(),
            "medium"
        );
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_nova_low_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Low),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoningConfig").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("maxReasoningEffort").unwrap(), "low");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_nova_minimal_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Minimal),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoningConfig").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        // Minimal maps to "low" for Nova
        assert_eq!(reasoning_config.get("maxReasoningEffort").unwrap(), "low");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_nova_none_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::None),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoningConfig").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "disabled");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_nova_no_effort_returns_none() {
        let config = CreateChatCompletionReasoning {
            effort: None,
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_nova(Some(&config));
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_chat_completion_reasoning_to_bedrock_nova_none_input_returns_none() {
        let result = convert_chat_completion_reasoning_to_bedrock_nova(None);
        assert!(result.is_none());
    }

    // =========================================================================
    // Adaptive Thinking (Opus 4.6+)
    // =========================================================================

    #[test]
    fn test_convert_chat_completion_reasoning_adaptive_high_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::High),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "adaptive");

        let beta = json.get("anthropic_beta").unwrap().as_array().unwrap();
        assert!(beta.iter().any(|v| v == "interleaved-thinking-2025-05-14"));

        let output_config = json.get("output_config").unwrap();
        assert_eq!(output_config.get("effort").unwrap(), "high");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_adaptive_low_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Low),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "adaptive");

        let output_config = json.get("output_config").unwrap();
        assert_eq!(output_config.get("effort").unwrap(), "low");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_adaptive_minimal_effort() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::Minimal),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let output_config = json.get("output_config").unwrap();
        assert_eq!(output_config.get("effort").unwrap(), "low");
    }

    #[test]
    fn test_convert_chat_completion_reasoning_adaptive_none_effort_disables() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::None),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "disabled");
        // Disabled should not have adaptive fields
        assert!(json.get("anthropic_beta").is_none());
    }

    #[test]
    fn test_convert_responses_reasoning_adaptive_high_effort() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            enabled: None,
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "adaptive");

        let beta = json.get("anthropic_beta").unwrap().as_array().unwrap();
        assert!(beta.iter().any(|v| v == "interleaved-thinking-2025-05-14"));

        let output_config = json.get("output_config").unwrap();
        assert_eq!(output_config.get("effort").unwrap(), "high");
    }

    #[test]
    fn test_convert_responses_reasoning_adaptive_default_effort() {
        // When effort is None but enabled is true, adaptive should default to "medium"
        let config = ResponsesReasoningConfig {
            effort: None,
            summary: None,
            enabled: Some(true),
            max_tokens: None,
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "adaptive");

        let output_config = json.get("output_config").unwrap();
        assert_eq!(output_config.get("effort").unwrap(), "medium");
    }

    #[test]
    fn test_convert_responses_reasoning_adaptive_with_max_tokens_falls_back_to_budget() {
        // When max_tokens is specified, even adaptive models should use fixed budget
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            enabled: None,
            max_tokens: Some(5000.0),
        };

        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        // With explicit max_tokens, should use budget-based (not adaptive)
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 5000);
        assert!(json.get("anthropic_beta").is_none());
    }

    #[test]
    fn test_non_adaptive_model_does_not_get_adaptive_config() {
        // Sonnet 4.5 should NOT get adaptive thinking
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::High),
            summary: None,
        };

        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            &["opus-4-6".to_string(), "opus-4.6".to_string()],
        );
        assert!(result.is_some());

        let json = result.unwrap();
        let reasoning_config = json.get("reasoning_config").unwrap();
        assert_eq!(reasoning_config.get("type").unwrap(), "enabled");
        assert_eq!(reasoning_config.get("budget_tokens").unwrap(), 32000);
        // Should NOT have adaptive fields
        assert!(json.get("anthropic_beta").is_none());
        assert!(json.get("output_config").is_none());
    }

    #[test]
    fn test_interleaved_thinking_allowlist_omits_header_when_empty() {
        let config = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::High),
            summary: None,
        };

        // Adaptive-capable model + empty allowlist -> still adaptive but no
        // anthropic_beta header (some Bedrock-hosted Claude models reject it).
        let result = convert_chat_completion_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &[],
        );
        let json = result.unwrap();
        assert_eq!(
            json.get("reasoning_config").unwrap().get("type").unwrap(),
            "adaptive"
        );
        assert!(json.get("anthropic_beta").is_none());
        assert_eq!(
            json.get("output_config").unwrap().get("effort").unwrap(),
            "high"
        );
    }

    #[test]
    fn test_interleaved_thinking_allowlist_substring_match() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::Medium),
            summary: None,
            enabled: None,
            max_tokens: None,
        };
        // Custom allowlist with a different substring still matches via
        // contains() — operators can opt models in/out without recompiling.
        let result = convert_responses_reasoning_to_bedrock_claude(
            Some(&config),
            "anthropic.claude-opus-4-6-20260525-v1:0",
            &["opus-4-6".to_string()],
        );
        let json = result.unwrap();
        let beta = json.get("anthropic_beta").unwrap().as_array().unwrap();
        assert!(beta.iter().any(|v| v == "interleaved-thinking-2025-05-14"));
    }
}
