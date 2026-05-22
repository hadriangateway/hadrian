//! Anthropic format conversion functions.
//!
//! Converts between OpenAI-compatible formats and Anthropic Messages API format.

use super::types::{
    AnthropicCacheControl, AnthropicCacheControlType, AnthropicContent, AnthropicEffort,
    AnthropicMessage, AnthropicOutputConfig, AnthropicResponse, AnthropicThinkingConfig,
    AnthropicTool, AnthropicToolChoice, ContentBlock, ImageSource, OpenAIChoice, OpenAIMessage,
    OpenAIResponse, OpenAIToolCall, OpenAIToolCallFunction, OpenAIUsage, PromptTokensDetails,
};
use crate::{
    api_types::{
        chat_completion::{
            CacheControl, ContentPart, CreateChatCompletionReasoning, Message, MessageContent,
            ReasoningEffort, Stop, ToolChoice, ToolChoiceDefaults, ToolDefinition,
        },
        responses::{
            CreateResponsesResponse, EasyInputMessageContent, EasyInputMessageRole,
            InputMessageItemRole, MessageType, OutputItemFunctionCall,
            OutputItemFunctionCallStatus, OutputItemFunctionCallType, OutputMessage,
            OutputMessageContentItem, OutputMessageStatus, ResponseInputContentItem, ResponseType,
            ResponsesInput, ResponsesInputItem, ResponsesOutputItem, ResponsesReasoning,
            ResponsesReasoningConfig, ResponsesReasoningConfigOutput, ResponsesReasoningEffort,
            ResponsesReasoningType, ResponsesResponseStatus, ResponsesToolChoice,
            ResponsesToolChoiceDefault, ResponsesToolDefinition, ResponsesUsage,
            ResponsesUsageInputTokensDetails, ResponsesUsageOutputTokensDetails,
        },
    },
    providers::image::parse_data_url,
    services::FileSearchToolArguments,
};

// ============================================================================
// Chat Completion Conversion Functions
// ============================================================================

/// Convert OpenAI-compatible CacheControl to Anthropic CacheControl.
///
/// Both formats use `type: "ephemeral"`, so this is a direct mapping.
fn convert_cache_control(cache_control: Option<&CacheControl>) -> Option<AnthropicCacheControl> {
    cache_control.map(|cc| AnthropicCacheControl {
        type_: match cc.type_ {
            crate::api_types::chat_completion::CacheControlType::Ephemeral => {
                AnthropicCacheControlType::Ephemeral
            }
        },
    })
}

/// Extract text content from MessageContent
pub fn extract_text(content: &MessageContent) -> String {
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

/// Convert MessageContent to Anthropic ContentBlocks, including images.
///
/// Note: HTTP image URLs should be preprocessed using `preprocess_messages_for_images`
/// before calling this function. Any remaining HTTP URLs will be skipped with a warning.
pub fn convert_content_to_blocks(content: &MessageContent) -> Vec<ContentBlock> {
    match content {
        MessageContent::Text(text) => {
            if text.is_empty() {
                vec![]
            } else {
                vec![ContentBlock::Text {
                    text: text.clone(),
                    cache_control: None,
                }]
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
                            blocks.push(ContentBlock::Text {
                                text: text.clone(),
                                cache_control: convert_cache_control(cache_control.as_ref()),
                            });
                        }
                    }
                    ContentPart::ImageUrl {
                        image_url,
                        cache_control,
                    } => {
                        let url = &image_url.url;
                        let cc = convert_cache_control(cache_control.as_ref());
                        // Check if it's an HTTPS URL - Anthropic supports direct URL references
                        if url.starts_with("https://") {
                            blocks.push(ContentBlock::Image {
                                source: ImageSource::Url { url: url.clone() },
                                cache_control: cc,
                            });
                        } else if url.starts_with("http://") {
                            // Anthropic only supports HTTPS URLs, not HTTP
                            tracing::warn!(
                                url = %url,
                                "Anthropic only supports HTTPS image URLs, not HTTP. Image skipped."
                            );
                        } else {
                            // Try to parse as data URL (base64)
                            match parse_data_url(url) {
                                Ok(image_data) => {
                                    blocks.push(ContentBlock::Image {
                                        source: ImageSource::Base64 {
                                            media_type: image_data.media_type,
                                            data: image_data.data,
                                        },
                                        cache_control: cc,
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        url = %url,
                                        error = %e,
                                        "Failed to parse image data URL. Image skipped."
                                    );
                                }
                            }
                        }
                    }
                    // Audio and video not supported by Anthropic
                    ContentPart::InputAudio { .. }
                    | ContentPart::InputVideo { .. }
                    | ContentPart::VideoUrl { .. } => {
                        tracing::warn!(
                            "Anthropic provider does not support audio/video content. Content skipped."
                        );
                    }
                }
            }
            blocks
        }
    }
}

/// Convert OpenAI tools to Anthropic format
pub fn convert_tools(tools: Option<Vec<ToolDefinition>>) -> Option<Vec<AnthropicTool>> {
    tools.map(|tools| {
        tools
            .into_iter()
            .map(|tool| AnthropicTool {
                name: tool.function.name,
                description: tool.function.description,
                input_schema: tool
                    .function
                    .parameters
                    .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                cache_control: convert_cache_control(tool.cache_control.as_ref()),
            })
            .collect()
    })
}

/// Convert OpenAI tool_choice to Anthropic format
pub fn convert_tool_choice(tool_choice: Option<ToolChoice>) -> Option<AnthropicToolChoice> {
    tool_choice.and_then(|tc| match tc {
        ToolChoice::String(default) => match default {
            ToolChoiceDefaults::Auto => Some(AnthropicToolChoice::Auto),
            ToolChoiceDefaults::Required => Some(AnthropicToolChoice::Any),
            ToolChoiceDefaults::None => None, // Anthropic doesn't have "none", just don't send tools
        },
        ToolChoice::Named(named) => Some(AnthropicToolChoice::Tool {
            name: named.function.name,
        }),
    })
}

/// Convert OpenAI messages to Anthropic format.
/// Returns (system_prompt, messages).
pub fn convert_messages(openai_messages: Vec<Message>) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages = Vec::new();
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

    for msg in openai_messages {
        match msg {
            Message::System { content, .. } | Message::Developer { content, .. } => {
                let text = extract_text(&content);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            Message::User { content, .. } => {
                // If we have pending tool results, add them as a user message first
                if !pending_tool_results.is_empty() {
                    messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Blocks(std::mem::take(
                            &mut pending_tool_results,
                        )),
                    });
                }
                // Use content blocks to support images and other multimodal content
                let blocks = convert_content_to_blocks(&content);
                if !blocks.is_empty() {
                    messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                // If we have pending tool results, add them as a user message first
                if !pending_tool_results.is_empty() {
                    messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Blocks(std::mem::take(
                            &mut pending_tool_results,
                        )),
                    });
                }

                let mut blocks = Vec::new();

                // Add text content if present
                if let Some(content) = content {
                    let text = extract_text(&content);
                    if !text.is_empty() {
                        blocks.push(ContentBlock::Text {
                            text,
                            cache_control: None,
                        });
                    }
                }

                // Add tool use blocks if present
                if let Some(tool_calls) = tool_calls {
                    for tool_call in tool_calls {
                        // Parse the JSON arguments string into a Value
                        let input = serde_json::from_str(&tool_call.function.arguments)
                            .unwrap_or(serde_json::json!({}));
                        blocks.push(ContentBlock::ToolUse {
                            id: tool_call.id,
                            name: tool_call.function.name,
                            input,
                            cache_control: None,
                        });
                    }
                }

                if !blocks.is_empty() {
                    messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                }
            }
            Message::Tool {
                content,
                tool_call_id,
            } => {
                // Collect tool results to be added as a user message
                pending_tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content: extract_text(&content),
                    cache_control: None,
                });
            }
        }
    }

    // Add any remaining tool results as a final user message
    if !pending_tool_results.is_empty() {
        messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(pending_tool_results),
        });
    }

    // Concatenate all system/developer messages with double newlines
    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    (system_prompt, messages)
}

/// Convert stop sequences from OpenAI format
pub fn convert_stop(stop: Option<Stop>) -> Option<Vec<String>> {
    stop.map(|s| match s {
        Stop::Single(s) => vec![s],
        Stop::Multiple(v) => v,
    })
}

/// Convert Anthropic response to OpenAI format.
pub fn convert_response(anthropic: AnthropicResponse) -> OpenAIResponse {
    let mut text_content = Vec::new();
    let mut thinking_content = Vec::new();
    let mut tool_calls = Vec::new();

    for block in anthropic.content {
        match block {
            ContentBlock::Text { text, .. } => {
                text_content.push(text);
            }
            ContentBlock::Thinking { thinking, .. } => {
                // Extract thinking content for reasoning field
                thinking_content.push(thinking);
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                tool_calls.push(OpenAIToolCall {
                    id,
                    type_: "function".to_string(),
                    function: OpenAIToolCallFunction {
                        name,
                        arguments: serde_json::to_string(&input).unwrap_or_default(),
                    },
                });
            }
            _ => {}
        }
    }

    let content = text_content.join("");
    let reasoning = thinking_content.join("");

    let finish_reason = match anthropic.stop_reason.as_deref() {
        Some("end_turn") => Some("stop".to_string()),
        Some("max_tokens") => Some("length".to_string()),
        Some("stop_sequence") => Some("stop".to_string()),
        Some("tool_use") => Some("tool_calls".to_string()),
        Some("pause_turn") => Some("stop".to_string()),
        Some("refusal") => Some("stop".to_string()),
        other => other.map(String::from),
    };

    OpenAIResponse {
        id: anthropic.id,
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: anthropic.model,
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
                name: None,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
            },
            finish_reason,
            logprobs: None,
        }],
        usage: Some(OpenAIUsage {
            prompt_tokens: anthropic.usage.input_tokens,
            completion_tokens: anthropic.usage.output_tokens,
            total_tokens: anthropic.usage.input_tokens + anthropic.usage.output_tokens,
            prompt_tokens_details: if anthropic.usage.cache_read_input_tokens > 0
                || anthropic.usage.cache_creation_input_tokens > 0
            {
                Some(PromptTokensDetails {
                    cached_tokens: anthropic.usage.cache_read_input_tokens,
                    cache_creation_input_tokens: anthropic.usage.cache_creation_input_tokens,
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

/// Convert OpenAI Responses API input to Anthropic Messages format.
/// Returns (system_prompt, messages).
pub fn convert_responses_input_to_messages(
    input: Option<ResponsesInput>,
    instructions: Option<String>,
) -> (Option<String>, Vec<AnthropicMessage>) {
    let system = instructions;
    let mut messages: Vec<AnthropicMessage> = Vec::new();

    let Some(input) = input else {
        return (system, messages);
    };

    match input {
        ResponsesInput::Text(text) => {
            // Simple text input becomes a single user message
            messages.push(AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text(text),
            });
        }
        ResponsesInput::Items(items) => {
            // Collect tool results to batch into user messages
            let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

            for item in items {
                match item {
                    ResponsesInputItem::EasyMessage(msg) => {
                        // Flush pending tool results before adding new message
                        if !pending_tool_results.is_empty() {
                            messages.push(AnthropicMessage {
                                role: "user".to_string(),
                                content: AnthropicContent::Blocks(std::mem::take(
                                    &mut pending_tool_results,
                                )),
                            });
                        }

                        let role = match msg.role {
                            EasyInputMessageRole::User => "user",
                            EasyInputMessageRole::Assistant => "assistant",
                            EasyInputMessageRole::System | EasyInputMessageRole::Developer => {
                                // System/developer messages are typically handled via instructions
                                // but if they appear in input, we skip them (already in system)
                                continue;
                            }
                        };

                        let content = match msg.content {
                            EasyInputMessageContent::Text(text) => AnthropicContent::Text(text),
                            EasyInputMessageContent::Parts(parts) => {
                                let blocks = convert_responses_content_to_blocks(&parts);
                                AnthropicContent::Blocks(blocks)
                            }
                        };

                        messages.push(AnthropicMessage {
                            role: role.to_string(),
                            content,
                        });
                    }
                    ResponsesInputItem::MessageItem(msg) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(AnthropicMessage {
                                role: "user".to_string(),
                                content: AnthropicContent::Blocks(std::mem::take(
                                    &mut pending_tool_results,
                                )),
                            });
                        }

                        let role = match msg.role {
                            InputMessageItemRole::User => "user",
                            InputMessageItemRole::System | InputMessageItemRole::Developer => {
                                continue;
                            }
                        };

                        let blocks = convert_responses_content_to_blocks(&msg.content);
                        messages.push(AnthropicMessage {
                            role: role.to_string(),
                            content: AnthropicContent::Blocks(blocks),
                        });
                    }
                    ResponsesInputItem::OutputMessage(msg) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(AnthropicMessage {
                                role: "user".to_string(),
                                content: AnthropicContent::Blocks(std::mem::take(
                                    &mut pending_tool_results,
                                )),
                            });
                        }

                        // Output message from assistant - convert content items to blocks
                        let mut blocks = Vec::new();
                        for content_item in msg.content {
                            match content_item {
                                OutputMessageContentItem::OutputText { text, .. } => {
                                    blocks.push(ContentBlock::Text {
                                        text,
                                        cache_control: None,
                                    });
                                }
                                OutputMessageContentItem::Refusal { refusal } => {
                                    // Convert refusal to text block
                                    blocks.push(ContentBlock::Text {
                                        text: refusal,
                                        cache_control: None,
                                    });
                                }
                            }
                        }

                        if !blocks.is_empty() {
                            messages.push(AnthropicMessage {
                                role: "assistant".to_string(),
                                content: AnthropicContent::Blocks(blocks),
                            });
                        }
                    }
                    ResponsesInputItem::FunctionCall(call) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(AnthropicMessage {
                                role: "user".to_string(),
                                content: AnthropicContent::Blocks(std::mem::take(
                                    &mut pending_tool_results,
                                )),
                            });
                        }

                        // Function call from assistant
                        let input: serde_json::Value =
                            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
                        let call_id = crate::providers::normalize_tool_call_id(&call.call_id);
                        messages.push(AnthropicMessage {
                            role: "assistant".to_string(),
                            content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                                id: call_id,
                                name: call.name.clone(),
                                input,
                                cache_control: None,
                            }]),
                        });
                    }
                    ResponsesInputItem::OutputFunctionCall(call) => {
                        // Flush pending tool results
                        if !pending_tool_results.is_empty() {
                            messages.push(AnthropicMessage {
                                role: "user".to_string(),
                                content: AnthropicContent::Blocks(std::mem::take(
                                    &mut pending_tool_results,
                                )),
                            });
                        }

                        // Output function call from assistant
                        let input: serde_json::Value =
                            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
                        let call_id = crate::providers::normalize_tool_call_id(&call.call_id);
                        messages.push(AnthropicMessage {
                            role: "assistant".to_string(),
                            content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                                id: call_id,
                                name: call.name.clone(),
                                input,
                                cache_control: None,
                            }]),
                        });
                    }
                    ResponsesInputItem::FunctionCallOutput(output) => {
                        // Collect tool results to batch into a user message
                        let call_id = crate::providers::normalize_tool_call_id(&output.call_id);
                        pending_tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: call_id,
                            content: output.output,
                            cache_control: None,
                        });
                    }
                    ResponsesInputItem::Reasoning(_) => {
                        // Reasoning blocks from previous responses are typically not sent back
                        // to the model (they're for client observation). Skip them.
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
                        // to Anthropic format - they're OpenAI-specific features
                    }
                }
            }

            // Flush any remaining tool results
            if !pending_tool_results.is_empty() {
                messages.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(pending_tool_results),
                });
            }
        }
    }

    (system, messages)
}

/// Convert Responses API content items to Anthropic content blocks.
pub fn convert_responses_content_to_blocks(
    items: &[ResponseInputContentItem],
) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    for item in items {
        match item {
            ResponseInputContentItem::InputText {
                text,
                cache_control,
            } => {
                if !text.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: text.clone(),
                        cache_control: convert_cache_control(cache_control.as_ref()),
                    });
                }
            }
            ResponseInputContentItem::InputImage {
                image_url,
                detail,
                cache_control,
            } => {
                if let Some(url) = image_url {
                    let cc = convert_cache_control(cache_control.as_ref());
                    // Check if it's an HTTPS URL - Anthropic supports direct URL references
                    if url.starts_with("https://") {
                        blocks.push(ContentBlock::Image {
                            source: ImageSource::Url { url: url.clone() },
                            cache_control: cc,
                        });
                    } else if url.starts_with("http://") {
                        // Anthropic only supports HTTPS URLs, not HTTP
                        tracing::warn!(
                            url = %url,
                            detail = ?detail,
                            "Anthropic only supports HTTPS image URLs, not HTTP. Image skipped."
                        );
                    } else {
                        // Try to parse as data URL (base64)
                        match parse_data_url(url) {
                            Ok(image_data) => {
                                blocks.push(ContentBlock::Image {
                                    source: ImageSource::Base64 {
                                        media_type: image_data.media_type,
                                        data: image_data.data,
                                    },
                                    cache_control: cc,
                                });
                            }
                            Err(e) => {
                                tracing::warn!(
                                    url = %url,
                                    detail = ?detail,
                                    error = %e,
                                    "Failed to parse image data URL. Image skipped."
                                );
                            }
                        }
                    }
                }
            }
            ResponseInputContentItem::InputFile { .. } => {
                // File inputs not directly supported by Anthropic Messages API
                tracing::warn!("File inputs not supported by Anthropic Messages API");
            }
            ResponseInputContentItem::InputAudio { .. } => {
                // Audio inputs not directly supported by Anthropic Messages API
                tracing::warn!("Audio inputs not supported by Anthropic Messages API");
            }
        }
    }

    blocks
}

/// Convert Responses API tools to Anthropic tools format.
pub fn convert_responses_tools(
    tools: Option<Vec<ResponsesToolDefinition>>,
) -> Option<Vec<AnthropicTool>> {
    let tools = tools?;
    let mut anthropic_tools = Vec::new();

    for tool in tools {
        match tool {
            ResponsesToolDefinition::Function(func) => {
                // Expected shape mirrors OpenAI's FunctionTool. `extras`
                // carries pipeline-specific extras like `cache_control`
                // (Hadrian extension) and the MCP rewrite's `annotations`.
                let parameters = func
                    .parameters
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                let cache_control = func
                    .extras
                    .get("cache_control")
                    .and_then(|v| serde_json::from_value::<CacheControl>(v.clone()).ok())
                    .as_ref()
                    .and_then(|cc| convert_cache_control(Some(cc)));

                anthropic_tools.push(AnthropicTool {
                    name: func.name,
                    description: func.description,
                    input_schema: parameters,
                    cache_control,
                });
            }
            ResponsesToolDefinition::WebSearchPreview(_)
            | ResponsesToolDefinition::WebSearchPreview20250311(_)
            | ResponsesToolDefinition::WebSearch(_)
            | ResponsesToolDefinition::WebSearch20250826(_) => {
                // Dead code: web_search tools are preprocessed to function tools in execution.rs
                // before reaching provider-specific conversion. This branch is a safety net.
                tracing::warn!("Unexpected web_search tool variant reached Anthropic conversion");
            }
            ResponsesToolDefinition::Shell(_) => {
                // Shell tool is OpenAI-specific (only the passthrough runtime forwards
                // it). When the upstream is Anthropic, the request shouldn't include a
                // shell tool — drop it with a warning rather than crashing.
                tracing::warn!(
                    "Shell tool reached Anthropic conversion — only OpenAI passthrough is \
                     supported for shell in the current build; dropping the tool definition"
                );
            }
            ResponsesToolDefinition::Mcp(_) => {
                // Under `passthrough_openai` mode this branch is unreachable
                // (preprocess rejects non-OpenAI providers). Under `hadrian_hosted`
                // the rewrite replaces every MCP entry with function tools before
                // we reach here. Anything slipping through is a defensive drop.
                tracing::warn!(
                    "MCP tool reached Anthropic conversion — should have been rewritten \
                     under hadrian_hosted or rejected under passthrough_openai; dropping"
                );
            }
            ResponsesToolDefinition::ToolSearch(_) => {
                // Hadrian-internal: under `hadrian_hosted` the MCP rewrite consumes
                // any caller-supplied `tool_search` entry (reading its `ranker`
                // override) and synthesizes its own function tool. Anything reaching
                // here is a defensive drop.
                tracing::warn!(
                    "tool_search tool reached Anthropic conversion — should have been \
                     consumed by the MCP rewrite; dropping"
                );
            }
            ResponsesToolDefinition::FileSearch(file_search) => {
                // File search is handled by the gateway middleware, but the model needs to know
                // how to call it. Convert to a function tool so the model can generate proper
                // tool calls that the middleware will intercept and execute.
                anthropic_tools.push(AnthropicTool {
                    name: FileSearchToolArguments::FUNCTION_NAME.to_string(),
                    description: Some(FileSearchToolArguments::function_description().to_string()),
                    input_schema: FileSearchToolArguments::function_parameters_schema(),
                    cache_control: convert_cache_control(file_search.cache_control.as_ref()),
                });
                tracing::debug!("Converted file_search tool to function definition for model");
            }
        }
    }

    if anthropic_tools.is_empty() {
        None
    } else {
        Some(anthropic_tools)
    }
}

/// Convert Responses API tool choice to Anthropic tool choice format.
pub fn convert_responses_tool_choice(
    tool_choice: Option<ResponsesToolChoice>,
) -> Option<AnthropicToolChoice> {
    tool_choice.and_then(|tc| match tc {
        ResponsesToolChoice::String(default) => match default {
            ResponsesToolChoiceDefault::Auto => Some(AnthropicToolChoice::Auto),
            ResponsesToolChoiceDefault::Required => Some(AnthropicToolChoice::Any),
            ResponsesToolChoiceDefault::None => None,
        },
        ResponsesToolChoice::Named(named) => Some(AnthropicToolChoice::Tool { name: named.name }),
        ResponsesToolChoice::WebSearch(_) => {
            // Web search tool choice not supported by Anthropic
            tracing::warn!("Web search tool choice not supported by Anthropic");
            None
        }
        ResponsesToolChoice::Shell(_) => Some(AnthropicToolChoice::Tool {
            name: "shell".to_string(),
        }),
        ResponsesToolChoice::Mcp(_) => {
            // Reaches Anthropic only when the hadrian_hosted rewrite was
            // skipped. Fall back to forcing any tool.
            tracing::warn!("MCP tool choice without a hosted rewrite; falling back to `any`");
            Some(AnthropicToolChoice::Any)
        }
    })
}

/// Convert Responses API reasoning config to Anthropic thinking config.
/// Check if a model supports adaptive thinking (Opus 4.6+).
pub(super) fn supports_adaptive_thinking(model: &str) -> bool {
    model.contains("opus-4-6") || model.contains("opus-4.6")
}

pub fn convert_reasoning_config(
    reasoning: Option<&ResponsesReasoningConfig>,
    model: &str,
) -> (
    Option<AnthropicThinkingConfig>,
    Option<AnthropicOutputConfig>,
) {
    let Some(reasoning) = reasoning else {
        return (None, None);
    };

    // Check if reasoning is explicitly disabled
    if reasoning.enabled == Some(false) {
        return (Some(AnthropicThinkingConfig::Disabled), None);
    }

    // If enabled or effort is specified
    if reasoning.enabled == Some(true)
        || reasoning.effort.is_some()
        || reasoning.max_tokens.is_some()
    {
        // For adaptive-capable models, use adaptive thinking with effort-based output config
        if supports_adaptive_thinking(model) && reasoning.max_tokens.is_none() {
            let effort = match reasoning.effort {
                Some(ResponsesReasoningEffort::None) => {
                    return (Some(AnthropicThinkingConfig::Disabled), None);
                }
                Some(ResponsesReasoningEffort::Minimal) | Some(ResponsesReasoningEffort::Low) => {
                    AnthropicEffort::Low
                }
                Some(ResponsesReasoningEffort::Medium) | None => AnthropicEffort::Medium,
                Some(ResponsesReasoningEffort::High) => AnthropicEffort::High,
            };
            return (
                Some(AnthropicThinkingConfig::Adaptive),
                Some(AnthropicOutputConfig { effort }),
            );
        }

        // Non-adaptive models: use fixed budget tokens
        let budget_tokens = if let Some(max) = reasoning.max_tokens {
            max as u32
        } else {
            match reasoning.effort {
                Some(ResponsesReasoningEffort::High) => 32000,
                Some(ResponsesReasoningEffort::Medium) => 16000,
                Some(ResponsesReasoningEffort::Low) => 8000,
                Some(ResponsesReasoningEffort::Minimal) => 2048,
                Some(ResponsesReasoningEffort::None) => {
                    return (Some(AnthropicThinkingConfig::Disabled), None);
                }
                None => 10000,
            }
        };

        // Minimum budget is 1024 tokens per Anthropic API requirements
        let budget_tokens = budget_tokens.max(1024);

        return (
            Some(AnthropicThinkingConfig::Enabled { budget_tokens }),
            None,
        );
    }

    (None, None)
}

/// Convert Chat Completion API reasoning config to Anthropic thinking config.
///
/// This is similar to `convert_reasoning_config` but works with the simpler
/// `CreateChatCompletionReasoning` type from the Chat Completion API.
pub fn convert_chat_completion_reasoning_config(
    reasoning: Option<&CreateChatCompletionReasoning>,
    model: &str,
) -> (
    Option<AnthropicThinkingConfig>,
    Option<AnthropicOutputConfig>,
) {
    let Some(reasoning) = reasoning else {
        return (None, None);
    };

    if let Some(effort) = reasoning.effort {
        // For adaptive-capable models, use adaptive thinking with effort-based output config
        if supports_adaptive_thinking(model) {
            let anthropic_effort = match effort {
                ReasoningEffort::None => {
                    return (Some(AnthropicThinkingConfig::Disabled), None);
                }
                ReasoningEffort::Minimal | ReasoningEffort::Low => AnthropicEffort::Low,
                ReasoningEffort::Medium => AnthropicEffort::Medium,
                ReasoningEffort::High => AnthropicEffort::High,
            };
            return (
                Some(AnthropicThinkingConfig::Adaptive),
                Some(AnthropicOutputConfig {
                    effort: anthropic_effort,
                }),
            );
        }

        // Non-adaptive models: use fixed budget tokens
        let budget_tokens = match effort {
            ReasoningEffort::High => 32000,
            ReasoningEffort::Medium => 16000,
            ReasoningEffort::Low => 8000,
            ReasoningEffort::Minimal => 2048,
            ReasoningEffort::None => {
                return (Some(AnthropicThinkingConfig::Disabled), None);
            }
        };

        // Minimum budget is 1024 tokens per Anthropic API requirements
        let budget_tokens = budget_tokens.max(1024);

        return (
            Some(AnthropicThinkingConfig::Enabled { budget_tokens }),
            None,
        );
    }

    (None, None)
}

/// Convert Anthropic response to OpenAI Responses API format.
pub fn convert_anthropic_to_responses_response(
    anthropic: AnthropicResponse,
    reasoning_config: Option<&ResponsesReasoningConfig>,
    user: Option<String>,
) -> CreateResponsesResponse {
    let mut output: Vec<ResponsesOutputItem> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_text: Option<String> = None;
    let mut thinking_signature: Option<String> = None;

    // Process content blocks
    for block in &anthropic.content {
        match block {
            ContentBlock::Text { text, .. } => {
                text_parts.push(text.clone());
            }
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                thinking_text = Some(thinking.clone());
                thinking_signature = signature.clone();
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                output.push(ResponsesOutputItem::FunctionCall(OutputItemFunctionCall {
                    type_: OutputItemFunctionCallType::FunctionCall,
                    id: Some(id.clone()),
                    name: name.clone(),
                    arguments: serde_json::to_string(input).unwrap_or_default(),
                    call_id: id.clone(),
                    status: Some(OutputItemFunctionCallStatus::Completed),
                }));
            }
            ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {
                // Images and tool results in response are unusual, skip them
            }
        }
    }

    // Add reasoning output if thinking was present
    if let Some(thinking) = thinking_text {
        output.push(ResponsesOutputItem::Reasoning(ResponsesReasoning {
            type_: ResponsesReasoningType::Reasoning,
            id: format!(
                "rs_{}",
                crate::providers::anthropic::stream::strip_anthropic_prefix(&anthropic.id, "msg_")
            ),
            content: None,   // Anthropic doesn't provide structured reasoning content
            summary: vec![], // Would need to generate summary
            encrypted_content: None,
            status: None,
            signature: thinking_signature,
            format: Some(
                crate::api_types::responses::OpenResponsesReasoningFormat::AnthropicClaudeV1,
            ),
        }));

        // Store the thinking text - in Responses API the thinking is typically not in output_text
        // but we could optionally include it
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
                id: format!(
                    "msg_{}",
                    crate::providers::anthropic::stream::strip_anthropic_prefix(
                        &anthropic.id,
                        "msg_"
                    )
                ),
                type_: MessageType::Message,
                role: "assistant".to_string(),
                content: message_content,
                status: Some(OutputMessageStatus::Completed),
            }),
        );
    }

    // Determine status based on stop_reason
    let status = match anthropic.stop_reason.as_deref() {
        Some("end_turn") => ResponsesResponseStatus::Completed,
        Some("max_tokens") => ResponsesResponseStatus::Incomplete,
        Some("stop_sequence") => ResponsesResponseStatus::Completed,
        Some("tool_use") => ResponsesResponseStatus::Completed,
        Some("pause_turn") => ResponsesResponseStatus::Completed,
        Some("refusal") => ResponsesResponseStatus::Completed,
        _ => ResponsesResponseStatus::Completed,
    };

    // Build reasoning config output if reasoning was requested
    let reasoning_output = reasoning_config.map(|config| ResponsesReasoningConfigOutput {
        effort: config.effort,
        summary: config.summary,
    });

    CreateResponsesResponse {
        id: anthropic.id.clone(),
        object: ResponseType::Response,
        created_at: chrono::Utc::now().timestamp() as f64,
        model: anthropic.model.clone(),
        status: Some(status),
        output,
        user,
        output_text,
        prompt_cache_key: None,
        safety_identifier: None,
        error: None,
        incomplete_details: None,
        usage: Some(ResponsesUsage {
            input_tokens: anthropic.usage.input_tokens,
            input_tokens_details: ResponsesUsageInputTokensDetails {
                cached_tokens: anthropic.usage.cache_read_input_tokens,
            },
            output_tokens: anthropic.usage.output_tokens,
            output_tokens_details: ResponsesUsageOutputTokensDetails {
                reasoning_tokens: 0, // Anthropic doesn't report this separately
            },
            total_tokens: anthropic.usage.input_tokens + anthropic.usage.output_tokens,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{
        chat_completion::{
            ContentPart, NamedToolChoice, NamedToolChoiceFunction, ToolCall, ToolCallFunction,
            ToolDefinitionFunction, ToolType,
        },
        responses::{
            EasyInputMessage, EasyInputMessageContent, EasyInputMessageRole, FunctionCallOutput,
            FunctionCallOutputType, FunctionTool, FunctionToolCall, FunctionToolCallType,
            ResponseInputImageDetail, ResponsesNamedToolChoice, ResponsesNamedToolChoiceType,
        },
    };

    #[test]
    fn test_convert_stop_single() {
        let stop = Some(Stop::Single("STOP".to_string()));
        let result = convert_stop(stop);
        assert_eq!(result, Some(vec!["STOP".to_string()]));
    }

    #[test]
    fn test_convert_stop_multiple() {
        let stop = Some(Stop::Multiple(vec![
            "STOP1".to_string(),
            "STOP2".to_string(),
        ]));
        let result = convert_stop(stop);
        assert_eq!(result, Some(vec!["STOP1".to_string(), "STOP2".to_string()]));
    }

    #[test]
    fn test_convert_stop_none() {
        let result = convert_stop(None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_convert_messages_simple_user() {
        let messages = vec![Message::User {
            content: MessageContent::Text("Hello".to_string()),
            name: None,
        }];

        let (system, anthropic_msgs) = convert_messages(messages);
        assert!(system.is_none());
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "user");
        // User messages are now always converted to Blocks to support multimodal content
        match &anthropic_msgs[0].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    ContentBlock::Text { text, .. } => assert_eq!(text, "Hello"),
                    _ => panic!("Expected Text block"),
                }
            }
            _ => panic!("Expected Blocks content"),
        }
    }

    #[test]
    fn test_convert_messages_system_message() {
        let messages = vec![
            Message::System {
                content: MessageContent::Text("You are helpful".to_string()),
                name: None,
            },
            Message::User {
                content: MessageContent::Text("Hi".to_string()),
                name: None,
            },
        ];

        let (system, anthropic_msgs) = convert_messages(messages);
        assert_eq!(system, Some("You are helpful".to_string()));
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "user");
    }

    #[test]
    fn test_convert_messages_multiple_system_messages() {
        // Multiple system messages should be concatenated with double newlines
        let messages = vec![
            Message::System {
                content: MessageContent::Text("First instruction".to_string()),
                name: None,
            },
            Message::System {
                content: MessageContent::Text("Second instruction".to_string()),
                name: None,
            },
            Message::User {
                content: MessageContent::Text("Hi".to_string()),
                name: None,
            },
        ];

        let (system, _) = convert_messages(messages);
        assert_eq!(
            system,
            Some("First instruction\n\nSecond instruction".to_string())
        );
    }

    #[test]
    fn test_convert_messages_system_and_developer_messages() {
        // System and Developer messages should be concatenated together
        let messages = vec![
            Message::System {
                content: MessageContent::Text("System rules".to_string()),
                name: None,
            },
            Message::Developer {
                content: MessageContent::Text("Developer context".to_string()),
                name: None,
            },
            Message::User {
                content: MessageContent::Text("Hi".to_string()),
                name: None,
            },
        ];

        let (system, _) = convert_messages(messages);
        assert_eq!(
            system,
            Some("System rules\n\nDeveloper context".to_string())
        );
    }

    #[test]
    fn test_convert_messages_assistant_with_tool_calls() {
        let messages = vec![Message::Assistant {
            content: Some(MessageContent::Text("Let me check the weather".to_string())),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_123".to_string(),
                type_: ToolType::Function,
                function: ToolCallFunction {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"NYC"}"#.to_string(),
                },
            }]),
            refusal: None,
            reasoning: None,
        }];

        let (_, anthropic_msgs) = convert_messages(messages);
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "assistant");
        match &anthropic_msgs[0].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                // First block is text
                match &blocks[0] {
                    ContentBlock::Text { text, .. } => {
                        assert_eq!(text, "Let me check the weather");
                    }
                    _ => panic!("Expected Text block"),
                }
                // Second block is tool_use
                match &blocks[1] {
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        assert_eq!(id, "call_123");
                        assert_eq!(name, "get_weather");
                        assert_eq!(input["city"], "NYC");
                    }
                    _ => panic!("Expected ToolUse block"),
                }
            }
            _ => panic!("Expected Blocks content"),
        }
    }

    #[test]
    fn test_convert_messages_tool_result() {
        let messages = vec![Message::Tool {
            content: MessageContent::Text("Sunny, 72°F".to_string()),
            tool_call_id: "call_123".to_string(),
        }];

        let (_, anthropic_msgs) = convert_messages(messages);
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "user");
        match &anthropic_msgs[0].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        assert_eq!(tool_use_id, "call_123");
                        assert_eq!(content, "Sunny, 72°F");
                    }
                    _ => panic!("Expected ToolResult block"),
                }
            }
            _ => panic!("Expected Blocks content"),
        }
    }

    #[test]
    fn test_convert_messages_with_image() {
        let messages = vec![Message::User {
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "What's in this image?".to_string(),
                    cache_control: None,
                },
                ContentPart::ImageUrl {
                    image_url: crate::api_types::chat_completion::ImageUrl {
                        url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                        detail: None,
                    },
                    cache_control: None,
                },
            ]),
            name: None,
        }];

        let (_, anthropic_msgs) = convert_messages(messages);
        assert_eq!(anthropic_msgs.len(), 1);
        // Images are now properly converted to ContentBlocks
        match &anthropic_msgs[0].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                // First block is text
                match &blocks[0] {
                    ContentBlock::Text { text, .. } => {
                        assert_eq!(text, "What's in this image?");
                    }
                    _ => panic!("Expected Text block"),
                }
                // Second block is image
                match &blocks[1] {
                    ContentBlock::Image { source, .. } => match source {
                        ImageSource::Base64 { media_type, data } => {
                            assert_eq!(media_type, "image/png");
                            assert_eq!(data, "iVBORw0KGgo=");
                        }
                        ImageSource::Url { .. } => panic!("Expected Base64 source, got Url"),
                    },
                    _ => panic!("Expected Image block"),
                }
            }
            _ => panic!("Expected Blocks content"),
        }
    }

    #[test]
    fn test_parse_data_url_in_convert_blocks() {
        // Test that convert_content_to_blocks properly uses parse_data_url
        // The actual parse_data_url tests are in src/providers/image.rs

        // Valid data URL should be converted to base64 image block
        let content = MessageContent::Parts(vec![ContentPart::ImageUrl {
            image_url: crate::api_types::chat_completion::ImageUrl {
                url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                detail: None,
            },
            cache_control: None,
        }]);
        let blocks = convert_content_to_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Image { source, .. } => match source {
                ImageSource::Base64 { media_type, data } => {
                    assert_eq!(media_type, "image/png");
                    assert_eq!(data, "iVBORw0KGgo=");
                }
                ImageSource::Url { .. } => panic!("Expected Base64 source"),
            },
            _ => panic!("Expected Image block"),
        }

        // HTTPS URL should be converted to URL source (Anthropic supports direct URLs)
        let content = MessageContent::Parts(vec![ContentPart::ImageUrl {
            image_url: crate::api_types::chat_completion::ImageUrl {
                url: "https://example.com/image.png".to_string(),
                detail: None,
            },
            cache_control: None,
        }]);
        let blocks = convert_content_to_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Image { source, .. } => match source {
                ImageSource::Url { url } => {
                    assert_eq!(url, "https://example.com/image.png");
                }
                ImageSource::Base64 { .. } => panic!("Expected Url source"),
            },
            _ => panic!("Expected Image block"),
        }

        // HTTP (non-secure) URL should be skipped - Anthropic only supports HTTPS
        let content = MessageContent::Parts(vec![ContentPart::ImageUrl {
            image_url: crate::api_types::chat_completion::ImageUrl {
                url: "http://example.com/image.png".to_string(),
                detail: None,
            },
            cache_control: None,
        }]);
        let blocks = convert_content_to_blocks(&content);
        assert!(blocks.is_empty(), "HTTP URL should be skipped");
    }

    #[test]
    fn test_convert_content_to_blocks_text_only() {
        let content = MessageContent::Text("Hello world".to_string());
        let blocks = convert_content_to_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "Hello world"),
            _ => panic!("Expected Text block"),
        }
    }

    #[test]
    fn test_convert_content_to_blocks_empty() {
        let content = MessageContent::Text("".to_string());
        let blocks = convert_content_to_blocks(&content);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_convert_tools() {
        let tools = Some(vec![ToolDefinition {
            type_: ToolType::Function,
            function: ToolDefinitionFunction {
                name: "get_weather".to_string(),
                description: Some("Get weather for a city".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"}
                    }
                })),
                strict: None,
            },
            cache_control: None,
        }]);

        let result = convert_tools(tools);
        assert!(result.is_some());
        let tools = result.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(
            tools[0].description,
            Some("Get weather for a city".to_string())
        );
    }

    #[test]
    fn test_convert_tools_none() {
        let result = convert_tools(None);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_cache_control() {
        use super::super::types::AnthropicCacheControlType;

        // Test conversion of cache_control
        let cc = CacheControl {
            type_: crate::api_types::chat_completion::CacheControlType::Ephemeral,
        };
        let result = convert_cache_control(Some(&cc));
        assert!(result.is_some());
        let anthropic_cc = result.unwrap();
        assert!(matches!(
            anthropic_cc.type_,
            AnthropicCacheControlType::Ephemeral
        ));

        // Test None case
        let result = convert_cache_control(None);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_content_to_blocks_with_cache_control() {
        use super::super::types::AnthropicCacheControlType;

        let content = MessageContent::Parts(vec![ContentPart::Text {
            text: "Hello".to_string(),
            cache_control: Some(CacheControl {
                type_: crate::api_types::chat_completion::CacheControlType::Ephemeral,
            }),
        }]);
        let blocks = convert_content_to_blocks(&content);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Text {
                text,
                cache_control,
            } => {
                assert_eq!(text, "Hello");
                assert!(cache_control.is_some());
                assert!(matches!(
                    cache_control.as_ref().unwrap().type_,
                    AnthropicCacheControlType::Ephemeral
                ));
            }
            _ => panic!("Expected Text block"),
        }
    }

    #[test]
    fn test_convert_tools_with_cache_control() {
        use super::super::types::AnthropicCacheControlType;

        let tools = Some(vec![ToolDefinition {
            type_: ToolType::Function,
            function: ToolDefinitionFunction {
                name: "get_weather".to_string(),
                description: Some("Get weather".to_string()),
                parameters: None,
                strict: None,
            },
            cache_control: Some(CacheControl {
                type_: crate::api_types::chat_completion::CacheControlType::Ephemeral,
            }),
        }]);

        let result = convert_tools(tools);
        assert!(result.is_some());
        let tools = result.unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].cache_control.is_some());
        assert!(matches!(
            tools[0].cache_control.as_ref().unwrap().type_,
            AnthropicCacheControlType::Ephemeral
        ));
    }

    #[test]
    fn test_convert_responses_content_with_cache_control() {
        use super::super::types::AnthropicCacheControlType;

        let items = vec![ResponseInputContentItem::InputText {
            text: "Hello".to_string(),
            cache_control: Some(CacheControl {
                type_: crate::api_types::chat_completion::CacheControlType::Ephemeral,
            }),
        }];

        let blocks = convert_responses_content_to_blocks(&items);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Text {
                text,
                cache_control,
            } => {
                assert_eq!(text, "Hello");
                assert!(cache_control.is_some());
                assert!(matches!(
                    cache_control.as_ref().unwrap().type_,
                    AnthropicCacheControlType::Ephemeral
                ));
            }
            _ => panic!("Expected Text block"),
        }
    }

    #[test]
    fn test_convert_tool_choice_auto() {
        let result = convert_tool_choice(Some(ToolChoice::String(ToolChoiceDefaults::Auto)));
        assert!(matches!(result, Some(AnthropicToolChoice::Auto)));
    }

    #[test]
    fn test_convert_tool_choice_required() {
        let result = convert_tool_choice(Some(ToolChoice::String(ToolChoiceDefaults::Required)));
        assert!(matches!(result, Some(AnthropicToolChoice::Any)));
    }

    #[test]
    fn test_convert_tool_choice_specific() {
        let result = convert_tool_choice(Some(ToolChoice::Named(NamedToolChoice {
            type_: ToolType::Function,
            function: NamedToolChoiceFunction {
                name: "get_weather".to_string(),
            },
        })));
        match result {
            Some(AnthropicToolChoice::Tool { name }) => {
                assert_eq!(name, "get_weather");
            }
            _ => panic!("Expected Tool choice"),
        }
    }

    #[test]
    fn test_convert_tool_choice_none() {
        let result = convert_tool_choice(None);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_response_text() {
        let anthropic_response = AnthropicResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                cache_control: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: super::super::types::AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let openai_response = convert_response(anthropic_response);
        assert_eq!(openai_response.id, "msg_123");
        assert_eq!(openai_response.model, "claude-sonnet-4-5-20250929");
        assert_eq!(openai_response.choices.len(), 1);
        assert_eq!(
            openai_response.choices[0].message.content,
            Some("Hello!".to_string())
        );
        assert_eq!(
            openai_response.choices[0].finish_reason,
            Some("stop".to_string())
        );
        assert!(openai_response.choices[0].message.tool_calls.is_none());

        let usage = openai_response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn test_convert_response_with_tool_use() {
        let anthropic_response = AnthropicResponse {
            id: "msg_456".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            content: vec![
                ContentBlock::Text {
                    text: "Let me check".to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolUse {
                    id: "call_789".to_string(),
                    name: "get_weather".to_string(),
                    input: serde_json::json!({"city": "NYC"}),
                    cache_control: None,
                },
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: super::super::types::AnthropicUsage {
                input_tokens: 20,
                output_tokens: 15,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let openai_response = convert_response(anthropic_response);
        assert_eq!(
            openai_response.choices[0].message.content,
            Some("Let me check".to_string())
        );
        assert_eq!(
            openai_response.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );

        let tool_calls = openai_response.choices[0]
            .message
            .tool_calls
            .as_ref()
            .unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_789");
        assert_eq!(tool_calls[0].type_, "function");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert!(tool_calls[0].function.arguments.contains("NYC"));
    }

    #[test]
    fn test_convert_response_stop_reasons() {
        let test_cases = vec![
            ("end_turn", "stop"),
            ("max_tokens", "length"),
            ("stop_sequence", "stop"),
            ("tool_use", "tool_calls"),
            ("pause_turn", "stop"),
            ("refusal", "stop"),
        ];

        for (anthropic_reason, expected_openai) in test_cases {
            let response = AnthropicResponse {
                id: "msg".to_string(),
                model: "claude".to_string(),
                content: vec![],
                stop_reason: Some(anthropic_reason.to_string()),
                usage: super::super::types::AnthropicUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            };

            let openai = convert_response(response);
            assert_eq!(
                openai.choices[0].finish_reason,
                Some(expected_openai.to_string()),
                "Failed for anthropic reason: {}",
                anthropic_reason
            );
        }
    }

    // Responses API conversion tests

    #[test]
    fn test_convert_responses_input_text() {
        let (system, messages) = convert_responses_input_to_messages(
            Some(ResponsesInput::Text("Hello, Claude!".to_string())),
            None,
        );

        assert!(system.is_none());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        match &messages[0].content {
            AnthropicContent::Text(text) => assert_eq!(text, "Hello, Claude!"),
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn test_convert_responses_input_with_instructions() {
        let (system, messages) = convert_responses_input_to_messages(
            Some(ResponsesInput::Text("Hello".to_string())),
            Some("You are a helpful assistant.".to_string()),
        );

        assert_eq!(system, Some("You are a helpful assistant.".to_string()));
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_convert_responses_input_easy_messages() {
        let items = vec![
            ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: None,
                role: EasyInputMessageRole::User,
                content: EasyInputMessageContent::Text("Hi there".to_string()),
            }),
            ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: None,
                role: EasyInputMessageRole::Assistant,
                content: EasyInputMessageContent::Text("Hello!".to_string()),
            }),
        ];

        let (system, messages) =
            convert_responses_input_to_messages(Some(ResponsesInput::Items(items)), None);

        assert!(system.is_none());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn test_convert_responses_input_function_call_and_output() {
        let items = vec![
            ResponsesInputItem::FunctionCall(FunctionToolCall {
                type_: FunctionToolCallType::FunctionCall,
                id: "fc_123".to_string(),
                call_id: "call_456".to_string(),
                name: "get_weather".to_string(),
                arguments: r#"{"city":"NYC"}"#.to_string(),
                status: None,
            }),
            ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: FunctionCallOutputType::FunctionCallOutput,
                id: None,
                call_id: "call_456".to_string(),
                output: "Sunny, 72°F".to_string(),
                status: None,
            }),
        ];

        let (_, messages) =
            convert_responses_input_to_messages(Some(ResponsesInput::Items(items)), None);

        assert_eq!(messages.len(), 2);

        // First message should be assistant with tool use
        assert_eq!(messages[0].role, "assistant");
        match &messages[0].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        assert_eq!(id, "call_456");
                        assert_eq!(name, "get_weather");
                        assert_eq!(input["city"], "NYC");
                    }
                    _ => panic!("Expected ToolUse block"),
                }
            }
            _ => panic!("Expected Blocks content"),
        }

        // Second message should be user with tool result
        assert_eq!(messages[1].role, "user");
        match &messages[1].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        assert_eq!(tool_use_id, "call_456");
                        assert_eq!(content, "Sunny, 72°F");
                    }
                    _ => panic!("Expected ToolResult block"),
                }
            }
            _ => panic!("Expected Blocks content"),
        }
    }

    #[test]
    fn test_convert_responses_content_to_blocks_text() {
        let items = vec![ResponseInputContentItem::InputText {
            text: "Hello world".to_string(),
            cache_control: None,
        }];

        let blocks = convert_responses_content_to_blocks(&items);

        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "Hello world"),
            _ => panic!("Expected Text block"),
        }
    }

    #[test]
    fn test_convert_responses_content_to_blocks_image_base64() {
        let items = vec![ResponseInputContentItem::InputImage {
            detail: ResponseInputImageDetail::Auto,
            image_url: Some("data:image/png;base64,iVBORw0KGgo=".to_string()),
            cache_control: None,
        }];

        let blocks = convert_responses_content_to_blocks(&items);

        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Image { source, .. } => match source {
                ImageSource::Base64 { media_type, data } => {
                    assert_eq!(media_type, "image/png");
                    assert_eq!(data, "iVBORw0KGgo=");
                }
                ImageSource::Url { .. } => panic!("Expected Base64 source"),
            },
            _ => panic!("Expected Image block"),
        }
    }

    #[test]
    fn test_convert_responses_content_to_blocks_image_url() {
        // HTTPS URL should be converted to URL source
        let items = vec![ResponseInputContentItem::InputImage {
            detail: ResponseInputImageDetail::Auto,
            image_url: Some("https://example.com/image.png".to_string()),
            cache_control: None,
        }];

        let blocks = convert_responses_content_to_blocks(&items);

        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Image { source, .. } => match source {
                ImageSource::Url { url } => {
                    assert_eq!(url, "https://example.com/image.png");
                }
                ImageSource::Base64 { .. } => panic!("Expected Url source"),
            },
            _ => panic!("Expected Image block"),
        }

        // HTTP (non-secure) URL should be skipped
        let items = vec![ResponseInputContentItem::InputImage {
            detail: ResponseInputImageDetail::Auto,
            image_url: Some("http://example.com/image.png".to_string()),
            cache_control: None,
        }];

        let blocks = convert_responses_content_to_blocks(&items);
        assert!(blocks.is_empty(), "HTTP URL should be skipped");
    }

    #[test]
    fn test_convert_responses_tools() {
        let tools = Some(vec![ResponsesToolDefinition::Function(
            FunctionTool::from_json(serde_json::json!({
                "type": "function",
                "name": "get_weather",
                "description": "Get weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"}
                    }
                }
            }))
            .unwrap(),
        )]);

        let result = convert_responses_tools(tools);

        assert!(result.is_some());
        let anthropic_tools = result.unwrap();
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0].name, "get_weather");
        assert_eq!(
            anthropic_tools[0].description,
            Some("Get weather for a city".to_string())
        );
    }

    #[test]
    fn test_convert_responses_tools_none() {
        let result = convert_responses_tools(None);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_responses_tool_choice_auto() {
        let choice = convert_responses_tool_choice(Some(ResponsesToolChoice::String(
            ResponsesToolChoiceDefault::Auto,
        )));
        assert!(matches!(choice, Some(AnthropicToolChoice::Auto)));
    }

    #[test]
    fn test_convert_responses_tool_choice_required() {
        let choice = convert_responses_tool_choice(Some(ResponsesToolChoice::String(
            ResponsesToolChoiceDefault::Required,
        )));
        assert!(matches!(choice, Some(AnthropicToolChoice::Any)));
    }

    #[test]
    fn test_convert_responses_tool_choice_named() {
        let choice = convert_responses_tool_choice(Some(ResponsesToolChoice::Named(
            ResponsesNamedToolChoice {
                type_: ResponsesNamedToolChoiceType::Function,
                name: "get_weather".to_string(),
            },
        )));

        match choice {
            Some(AnthropicToolChoice::Tool { name }) => assert_eq!(name, "get_weather"),
            _ => panic!("Expected Tool choice"),
        }
    }

    #[test]
    fn test_convert_reasoning_config_enabled() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            max_tokens: None,
            enabled: Some(true),
        };

        let (thinking, output_config) =
            convert_reasoning_config(Some(&config), "claude-sonnet-4-5-20250929");

        assert!(output_config.is_none());
        match thinking {
            Some(AnthropicThinkingConfig::Enabled { budget_tokens }) => {
                assert_eq!(budget_tokens, 32000); // High effort = 32000 tokens
            }
            _ => panic!("Expected Enabled thinking config"),
        }
    }

    #[test]
    fn test_convert_reasoning_config_disabled() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::None),
            summary: None,
            max_tokens: None,
            enabled: None,
        };

        let (thinking, output_config) =
            convert_reasoning_config(Some(&config), "claude-sonnet-4-5-20250929");
        assert!(output_config.is_none());
        assert!(matches!(thinking, Some(AnthropicThinkingConfig::Disabled)));
    }

    #[test]
    fn test_convert_reasoning_config_with_max_tokens() {
        let config = ResponsesReasoningConfig {
            effort: None,
            summary: None,
            max_tokens: Some(5000.0),
            enabled: Some(true),
        };

        let (thinking, output_config) =
            convert_reasoning_config(Some(&config), "claude-sonnet-4-5-20250929");

        assert!(output_config.is_none());
        match thinking {
            Some(AnthropicThinkingConfig::Enabled { budget_tokens }) => {
                assert_eq!(budget_tokens, 5000);
            }
            _ => panic!("Expected Enabled thinking config"),
        }
    }

    #[test]
    fn test_convert_reasoning_config_minimum_budget() {
        let config = ResponsesReasoningConfig {
            effort: None,
            summary: None,
            max_tokens: Some(500.0), // Below minimum
            enabled: Some(true),
        };

        let (thinking, output_config) =
            convert_reasoning_config(Some(&config), "claude-sonnet-4-5-20250929");

        assert!(output_config.is_none());
        match thinking {
            Some(AnthropicThinkingConfig::Enabled { budget_tokens }) => {
                assert_eq!(budget_tokens, 1024); // Should be raised to minimum
            }
            _ => panic!("Expected Enabled thinking config"),
        }
    }

    #[test]
    fn test_convert_anthropic_to_responses_response_text() {
        let anthropic_response = AnthropicResponse {
            id: "msg_1234567890abcdef".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                cache_control: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: super::super::types::AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let result = convert_anthropic_to_responses_response(anthropic_response, None, None);

        assert_eq!(result.id, "msg_1234567890abcdef");
        assert_eq!(result.model, "claude-sonnet-4-5-20250929");
        assert_eq!(result.output_text, Some("Hello!".to_string()));
        assert!(matches!(
            result.status,
            Some(ResponsesResponseStatus::Completed)
        ));

        // Check usage
        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn test_convert_anthropic_to_responses_response_with_tool_use() {
        let anthropic_response = AnthropicResponse {
            id: "msg_abcdef1234567890".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            content: vec![
                ContentBlock::Text {
                    text: "Let me check the weather.".to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "get_weather".to_string(),
                    input: serde_json::json!({"city": "NYC"}),
                    cache_control: None,
                },
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: super::super::types::AnthropicUsage {
                input_tokens: 20,
                output_tokens: 15,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let result = convert_anthropic_to_responses_response(anthropic_response, None, None);

        assert_eq!(
            result.output_text,
            Some("Let me check the weather.".to_string())
        );

        // Check that we have both message and function call in output
        assert_eq!(result.output.len(), 2);

        // First should be message
        match &result.output[0] {
            ResponsesOutputItem::Message(msg) => {
                assert_eq!(msg.role, "assistant");
            }
            _ => panic!("Expected Message output"),
        }

        // Second should be function call
        match &result.output[1] {
            ResponsesOutputItem::FunctionCall(call) => {
                assert_eq!(call.name, "get_weather");
                assert_eq!(call.call_id, "toolu_123");
            }
            _ => panic!("Expected FunctionCall output"),
        }
    }

    #[test]
    fn test_convert_anthropic_to_responses_response_with_thinking() {
        let anthropic_response = AnthropicResponse {
            id: "msg_thinking123456789".to_string(),
            model: "claude-opus-4-5-20251101".to_string(),
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me think about this...".to_string(),
                    signature: Some("sig_abc123".to_string()),
                },
                ContentBlock::Text {
                    text: "The answer is 42.".to_string(),
                    cache_control: None,
                },
            ],
            stop_reason: Some("end_turn".to_string()),
            usage: super::super::types::AnthropicUsage {
                input_tokens: 50,
                output_tokens: 100,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let reasoning_config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            max_tokens: None,
            enabled: Some(true),
        };

        let result = convert_anthropic_to_responses_response(
            anthropic_response,
            Some(&reasoning_config),
            None,
        );

        assert_eq!(result.output_text, Some("The answer is 42.".to_string()));

        // Check reasoning output
        let has_reasoning = result.output.iter().any(|item| {
            matches!(item, ResponsesOutputItem::Reasoning(r) if r.signature == Some("sig_abc123".to_string()))
        });
        assert!(has_reasoning, "Expected reasoning output with signature");

        // Check reasoning config in response
        assert!(result.reasoning.is_some());
        assert_eq!(
            result.reasoning.unwrap().effort,
            Some(ResponsesReasoningEffort::High)
        );
    }

    #[test]
    fn test_convert_anthropic_to_responses_response_max_tokens() {
        let anthropic_response = AnthropicResponse {
            id: "msg_maxed12345678901".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            content: vec![ContentBlock::Text {
                text: "This response was truncated...".to_string(),
                cache_control: None,
            }],
            stop_reason: Some("max_tokens".to_string()),
            usage: super::super::types::AnthropicUsage {
                input_tokens: 10,
                output_tokens: 4096,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let result = convert_anthropic_to_responses_response(anthropic_response, None, None);

        assert!(matches!(
            result.status,
            Some(ResponsesResponseStatus::Incomplete)
        ));
    }

    #[test]
    fn test_convert_responses_tools_file_search() {
        use crate::api_types::responses::{FileSearchTool, FileSearchToolType};

        let tools = Some(vec![ResponsesToolDefinition::FileSearch(FileSearchTool {
            type_: FileSearchToolType::FileSearch,
            vector_store_ids: vec!["vs_123".to_string()],
            max_num_results: None,
            ranking_options: None,
            filters: None,
            cache_control: None,
        })]);

        let result = convert_responses_tools(tools);

        // FileSearch should be converted to a function tool
        assert!(result.is_some());
        let anthropic_tools = result.unwrap();
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0].name, "file_search");
        assert!(anthropic_tools[0].description.is_some());

        // Check the input schema contains the query property
        let schema = &anthropic_tools[0].input_schema;
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].get("query").is_some());
    }

    #[test]
    fn test_convert_responses_tools_mixed_with_file_search() {
        use crate::api_types::responses::{FileSearchTool, FileSearchToolType};

        let tools = Some(vec![
            ResponsesToolDefinition::Function(
                FunctionTool::from_json(serde_json::json!({
                    "type": "function",
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {"type": "object", "properties": {}}
                }))
                .unwrap(),
            ),
            ResponsesToolDefinition::FileSearch(FileSearchTool {
                type_: FileSearchToolType::FileSearch,
                vector_store_ids: vec!["vs_456".to_string()],
                max_num_results: Some(10),
                ranking_options: None,
                filters: None,
                cache_control: None,
            }),
        ]);

        let result = convert_responses_tools(tools);

        assert!(result.is_some());
        let anthropic_tools = result.unwrap();
        assert_eq!(anthropic_tools.len(), 2);

        // First should be the regular function
        assert_eq!(anthropic_tools[0].name, "get_weather");

        // Second should be the converted file_search
        assert_eq!(anthropic_tools[1].name, "file_search");
    }

    #[test]
    fn test_anthropic_usage_with_cache_tokens() {
        use super::super::types::AnthropicUsage;

        let json = r#"{
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 25,
            "cache_creation_input_tokens": 10
        }"#;

        let usage: AnthropicUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 25);
        assert_eq!(usage.cache_creation_input_tokens, 10);
    }

    #[test]
    fn test_anthropic_usage_without_cache_tokens() {
        use super::super::types::AnthropicUsage;

        // Cache tokens should default to 0 when not present
        let json = r#"{
            "input_tokens": 100,
            "output_tokens": 50
        }"#;

        let usage: AnthropicUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn test_openai_usage_with_cache_tokens() {
        use super::super::types::{OpenAIUsage, PromptTokensDetails};

        let usage = OpenAIUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: 25,
                cache_creation_input_tokens: 0,
            }),
        };

        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"prompt_tokens_details\""));
        assert!(json.contains("\"cached_tokens\":25"));
    }

    #[test]
    fn test_openai_usage_without_cache_tokens() {
        use super::super::types::OpenAIUsage;

        let usage = OpenAIUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_tokens_details: None,
        };

        let json = serde_json::to_string(&usage).unwrap();
        // prompt_tokens_details should be omitted when None
        assert!(!json.contains("prompt_tokens_details"));
    }

    #[test]
    fn test_convert_anthropic_to_openai_with_cache_tokens() {
        use super::super::types::{AnthropicResponse, AnthropicUsage, ContentBlock};

        let anthropic_response = AnthropicResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                cache_control: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 25,
                cache_creation_input_tokens: 10,
            },
        };

        let openai_response = convert_response(anthropic_response);

        let usage = openai_response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);

        // Should have cache tokens
        let details = usage.prompt_tokens_details.unwrap();
        assert_eq!(details.cached_tokens, 25);
    }

    #[test]
    fn test_convert_anthropic_to_openai_without_cache_tokens() {
        use super::super::types::{AnthropicResponse, AnthropicUsage, ContentBlock};

        let anthropic_response = AnthropicResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                cache_control: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let openai_response = convert_response(anthropic_response);

        let usage = openai_response.usage.unwrap();
        // Should NOT have cache tokens when they're 0
        assert!(usage.prompt_tokens_details.is_none());
    }

    // ============================================================================
    // Chat Completion Thinking/Reasoning Tests
    // ============================================================================

    #[test]
    fn test_convert_response_with_thinking_content() {
        use super::super::types::{AnthropicResponse, AnthropicUsage, ContentBlock};

        let anthropic_response = AnthropicResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me analyze this problem...".to_string(),
                    signature: Some("sig123".to_string()),
                },
                ContentBlock::Text {
                    text: "The answer is 42.".to_string(),
                    cache_control: None,
                },
            ],
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let openai_response = convert_response(anthropic_response);

        // Check content contains only the text, not thinking
        assert_eq!(
            openai_response.choices[0].message.content,
            Some("The answer is 42.".to_string())
        );

        // Check reasoning contains the thinking content
        assert_eq!(
            openai_response.choices[0].message.reasoning,
            Some("Let me analyze this problem...".to_string())
        );
    }

    #[test]
    fn test_convert_response_without_thinking_content() {
        use super::super::types::{AnthropicResponse, AnthropicUsage, ContentBlock};

        let anthropic_response = AnthropicResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                cache_control: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        };

        let openai_response = convert_response(anthropic_response);

        assert_eq!(
            openai_response.choices[0].message.content,
            Some("Hello!".to_string())
        );
        assert_eq!(openai_response.choices[0].message.reasoning, None);
    }

    #[test]
    fn test_convert_chat_completion_reasoning_config_high() {
        use super::super::types::AnthropicThinkingConfig;
        use crate::api_types::chat_completion::{CreateChatCompletionReasoning, ReasoningEffort};

        let reasoning = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::High),
            summary: None,
        };

        let (thinking, output_config) = convert_chat_completion_reasoning_config(
            Some(&reasoning),
            "claude-sonnet-4-5-20250929",
        );

        assert!(output_config.is_none());
        match thinking {
            Some(AnthropicThinkingConfig::Enabled { budget_tokens }) => {
                assert_eq!(budget_tokens, 32000);
            }
            _ => panic!("Expected Enabled with budget_tokens"),
        }
    }

    #[test]
    fn test_convert_chat_completion_reasoning_config_none_disables() {
        use super::super::types::AnthropicThinkingConfig;
        use crate::api_types::chat_completion::{CreateChatCompletionReasoning, ReasoningEffort};

        let reasoning = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::None),
            summary: None,
        };

        let (thinking, output_config) = convert_chat_completion_reasoning_config(
            Some(&reasoning),
            "claude-sonnet-4-5-20250929",
        );

        assert!(output_config.is_none());
        assert!(matches!(thinking, Some(AnthropicThinkingConfig::Disabled)));
    }

    #[test]
    fn test_convert_chat_completion_reasoning_config_no_effort() {
        use crate::api_types::chat_completion::CreateChatCompletionReasoning;

        // If effort is not specified, no thinking config is generated
        let reasoning = CreateChatCompletionReasoning {
            effort: None,
            summary: None,
        };

        let (thinking, output_config) = convert_chat_completion_reasoning_config(
            Some(&reasoning),
            "claude-sonnet-4-5-20250929",
        );
        assert!(thinking.is_none());
        assert!(output_config.is_none());
    }

    // ============================================================================
    // Adaptive Thinking Tests
    // ============================================================================

    #[test]
    fn test_supports_adaptive_thinking() {
        assert!(supports_adaptive_thinking("claude-opus-4-6-20260525"));
        assert!(supports_adaptive_thinking("claude-opus-4.6-20260525"));
        assert!(supports_adaptive_thinking("some-prefix-opus-4-6-suffix"));
        assert!(!supports_adaptive_thinking("claude-sonnet-4-5-20250929"));
        assert!(!supports_adaptive_thinking("claude-opus-4-5-20251101"));
        assert!(!supports_adaptive_thinking("gpt-4"));
    }

    #[test]
    fn test_convert_reasoning_config_adaptive() {
        use super::super::types::AnthropicEffort;

        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            max_tokens: None,
            enabled: Some(true),
        };

        let (thinking, output_config) =
            convert_reasoning_config(Some(&config), "claude-opus-4-6-20260525");

        assert!(
            matches!(thinking, Some(AnthropicThinkingConfig::Adaptive)),
            "Expected Adaptive thinking config for opus-4-6"
        );
        let output = output_config.expect("Expected output config for adaptive thinking");
        assert!(matches!(output.effort, AnthropicEffort::High));
    }

    #[test]
    fn test_convert_reasoning_config_adaptive_with_max_tokens_falls_back_to_enabled() {
        let config = ResponsesReasoningConfig {
            effort: Some(ResponsesReasoningEffort::High),
            summary: None,
            max_tokens: Some(5000.0), // max_tokens forces non-adaptive path
            enabled: Some(true),
        };

        let (thinking, output_config) =
            convert_reasoning_config(Some(&config), "claude-opus-4-6-20260525");

        assert!(output_config.is_none());
        match thinking {
            Some(AnthropicThinkingConfig::Enabled { budget_tokens }) => {
                assert_eq!(budget_tokens, 5000);
            }
            _ => panic!("Expected Enabled thinking config when max_tokens is set"),
        }
    }

    #[test]
    fn test_convert_chat_completion_reasoning_config_adaptive() {
        use super::super::types::AnthropicEffort;
        use crate::api_types::chat_completion::{CreateChatCompletionReasoning, ReasoningEffort};

        let reasoning = CreateChatCompletionReasoning {
            effort: Some(ReasoningEffort::High),
            summary: None,
        };

        let (thinking, output_config) =
            convert_chat_completion_reasoning_config(Some(&reasoning), "claude-opus-4-6-20260525");

        assert!(
            matches!(thinking, Some(AnthropicThinkingConfig::Adaptive)),
            "Expected Adaptive thinking config for opus-4-6"
        );
        let output = output_config.expect("Expected output config for adaptive thinking");
        assert!(matches!(output.effort, AnthropicEffort::High));
    }

    #[test]
    fn test_adaptive_thinking_config_serializes_correctly() {
        let config = AnthropicThinkingConfig::Adaptive;
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json, serde_json::json!({"type": "adaptive"}));
    }

    #[test]
    fn test_output_config_serializes_as_output_config() {
        use super::super::types::{AnthropicEffort, AnthropicOutputConfig, AnthropicRequest};

        let request = AnthropicRequest {
            model: "claude-opus-4-6-20260525".to_string(),
            messages: vec![],
            max_tokens: 4096,
            system: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            tool_choice: None,
            thinking: Some(AnthropicThinkingConfig::Adaptive),
            output_config: Some(AnthropicOutputConfig {
                effort: AnthropicEffort::High,
            }),
            metadata: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        // Must serialize as "output_config", not "output"
        assert!(
            json.get("output_config").is_some(),
            "expected 'output_config' key"
        );
        assert!(json.get("output").is_none(), "should not have 'output' key");
        assert_eq!(json["output_config"]["effort"], "high");
    }
}
