//! Conversion functions between Vertex AI and OpenAI/Responses API formats.

use chrono::Utc;

use super::types::{
    OpenAIChoice, OpenAIMessage, OpenAIResponse, OpenAIToolCall, OpenAIToolCallFunction,
    OpenAIUsage, VertexContent, VertexFunctionCallingConfig, VertexFunctionCallingMode,
    VertexFunctionDeclaration, VertexGenerateContentResponse, VertexPart, VertexThinkingConfig,
    VertexThinkingLevel, VertexTool, VertexToolConfig,
};
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
            OutputItemReasoningStatus, OutputMessage, OutputMessageContentItem,
            OutputMessageStatus, ReasoningSummaryText, ReasoningSummaryTextType,
            ResponseInputContentItem, ResponseType, ResponsesInput, ResponsesInputItem,
            ResponsesOutputItem, ResponsesReasoning, ResponsesReasoningConfig,
            ResponsesReasoningConfigOutput, ResponsesReasoningEffort, ResponsesReasoningType,
            ResponsesResponseStatus, ResponsesToolChoice, ResponsesToolChoiceDefault,
            ResponsesToolDefinition, ResponsesUsage, ResponsesUsageInputTokensDetails,
            ResponsesUsageOutputTokensDetails,
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

/// Convert MessageContent to Vertex parts, including images.
///
/// Note: HTTP image URLs should be preprocessed before calling this function
/// using `preprocess_messages_for_images()` from the image module.
pub(super) fn convert_content_to_parts(content: &MessageContent) -> Vec<VertexPart> {
    match content {
        MessageContent::Text(text) => {
            if text.is_empty() {
                vec![]
            } else {
                vec![VertexPart::text(text.clone())]
            }
        }
        MessageContent::Parts(parts) => {
            let mut vertex_parts = Vec::new();
            for part in parts {
                match part {
                    ContentPart::Text { text, .. } => {
                        if !text.is_empty() {
                            vertex_parts.push(VertexPart::text(text.clone()));
                        }
                    }
                    ContentPart::ImageUrl { image_url, .. } => {
                        // Try to parse as data URL (base64) using shared utility
                        match parse_data_url(&image_url.url) {
                            Ok(image_data) => {
                                // Vertex supports png, jpeg, gif, webp formats
                                vertex_parts.push(VertexPart::inline_data(
                                    image_data.media_type,
                                    image_data.data,
                                ));
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
                    }
                    // Audio and video not supported in this implementation
                    ContentPart::InputAudio { .. }
                    | ContentPart::InputVideo { .. }
                    | ContentPart::VideoUrl { .. } => {
                        tracing::warn!(
                            "Vertex provider does not support audio/video content in this implementation. Content skipped."
                        );
                    }
                }
            }
            vertex_parts
        }
    }
}

/// Convert OpenAI tools to Vertex format
pub(super) fn convert_tools(tools: Option<Vec<ToolDefinition>>) -> Option<Vec<VertexTool>> {
    tools.map(|tools| {
        vec![VertexTool {
            function_declarations: tools
                .into_iter()
                .map(|tool| VertexFunctionDeclaration {
                    name: tool.function.name,
                    description: tool.function.description,
                    parameters: tool.function.parameters,
                })
                .collect(),
        }]
    })
}

/// Convert OpenAI tool_choice to Vertex format
pub(super) fn convert_tool_choice(tool_choice: Option<ToolChoice>) -> Option<VertexToolConfig> {
    tool_choice.map(|tc| {
        let (mode, allowed_names) = match tc {
            ToolChoice::String(default) => match default {
                ToolChoiceDefaults::Auto => (VertexFunctionCallingMode::Auto, None),
                ToolChoiceDefaults::Required => (VertexFunctionCallingMode::Any, None),
                ToolChoiceDefaults::None => (VertexFunctionCallingMode::None, None),
            },
            ToolChoice::Named(named) => (
                VertexFunctionCallingMode::Any,
                Some(vec![named.function.name]),
            ),
        };
        VertexToolConfig {
            function_calling_config: VertexFunctionCallingConfig {
                mode,
                allowed_function_names: allowed_names,
            },
        }
    })
}

/// Convert OpenAI messages to Vertex format.
pub(super) fn convert_messages(
    openai_messages: Vec<Message>,
    tool_call_names: &mut std::collections::HashMap<String, String>,
) -> (Option<VertexContent>, Vec<VertexContent>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut contents = Vec::new();
    let mut pending_function_responses: Vec<VertexPart> = Vec::new();

    for msg in openai_messages {
        match msg {
            Message::System { content, .. } | Message::Developer { content, .. } => {
                let text = extract_text(&content);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            Message::User { content, .. } => {
                // Flush any pending function responses first
                if !pending_function_responses.is_empty() {
                    contents.push(VertexContent {
                        role: "user".to_string(),
                        parts: std::mem::take(&mut pending_function_responses),
                    });
                }
                // Use content conversion to support images and other multimodal content
                let parts = convert_content_to_parts(&content);
                if !parts.is_empty() {
                    contents.push(VertexContent {
                        role: "user".to_string(),
                        parts,
                    });
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                // Flush any pending function responses first
                if !pending_function_responses.is_empty() {
                    contents.push(VertexContent {
                        role: "user".to_string(),
                        parts: std::mem::take(&mut pending_function_responses),
                    });
                }

                let mut parts = Vec::new();

                // Add text content if present
                if let Some(content) = content {
                    let text = extract_text(&content);
                    if !text.is_empty() {
                        parts.push(VertexPart::text(text));
                    }
                }

                // Add function calls if present
                if let Some(tool_calls) = tool_calls {
                    for tool_call in tool_calls {
                        // Store the mapping of tool_call_id to function name for later
                        tool_call_names
                            .insert(tool_call.id.clone(), tool_call.function.name.clone());

                        // Parse the JSON arguments string into a Value
                        let args = serde_json::from_str(&tool_call.function.arguments)
                            .unwrap_or(serde_json::json!({}));
                        parts.push(VertexPart::function_call(tool_call.function.name, args));
                    }
                }

                if !parts.is_empty() {
                    contents.push(VertexContent {
                        role: "model".to_string(), // Gemini uses "model" instead of "assistant"
                        parts,
                    });
                }
            }
            Message::Tool {
                content,
                tool_call_id,
            } => {
                // Get the function name from our mapping
                let function_name = tool_call_names
                    .get(&tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());

                // Parse the content as JSON if possible, otherwise wrap in a text response
                let response_value = serde_json::from_str(&extract_text(&content))
                    .unwrap_or_else(|_| serde_json::json!({"result": extract_text(&content)}));

                pending_function_responses
                    .push(VertexPart::function_response(function_name, response_value));
            }
        }
    }

    // Flush any remaining function responses
    if !pending_function_responses.is_empty() {
        contents.push(VertexContent {
            role: "user".to_string(),
            parts: pending_function_responses,
        });
    }

    // Concatenate all system/developer messages with double newlines
    let system_instruction = if system_parts.is_empty() {
        None
    } else {
        Some(VertexContent {
            role: "user".to_string(), // System instruction uses user role in Gemini
            parts: vec![VertexPart::text(system_parts.join("\n\n"))],
        })
    };

    (system_instruction, contents)
}

/// Convert stop sequences from OpenAI format
pub(super) fn convert_stop(stop: Option<Stop>) -> Option<Vec<String>> {
    stop.map(|s| match s {
        Stop::Single(s) => vec![s],
        Stop::Multiple(v) => v,
    })
}

/// Check if a model is Gemini 3+ (uses thinkingLevel) vs Gemini 2.5 (uses thinkingBudget).
fn is_gemini_3_model(model: &str) -> bool {
    model.contains("gemini-3") || model.contains("gemini3")
}

/// Convert ResponsesReasoningConfig to Vertex ThinkingConfig.
///
/// Gemini 3+ models use `thinking_level` parameter.
/// Gemini 2.5 models use `thinking_budget` parameter.
pub(super) fn convert_reasoning_to_thinking_config(
    reasoning: Option<&ResponsesReasoningConfig>,
    model: &str,
) -> Option<VertexThinkingConfig> {
    let reasoning = reasoning?;

    // Check if reasoning is explicitly disabled
    if reasoning.enabled == Some(false) {
        // For Gemini 2.5 Flash models, setting budget to 0 disables thinking
        if !is_gemini_3_model(model) && model.contains("flash") {
            return Some(VertexThinkingConfig {
                thinking_level: None,
                thinking_budget: Some(0),
                include_thoughts: None,
            });
        }
        // Gemini 3 Pro cannot disable thinking, so return None
        return None;
    }

    // Convert effort to the appropriate parameter based on model version
    if is_gemini_3_model(model) {
        // Gemini 3+ models use thinking_level
        let thinking_level = reasoning.effort.map(|effort| match effort {
            ResponsesReasoningEffort::None | ResponsesReasoningEffort::Minimal => {
                VertexThinkingLevel::Minimal
            }
            ResponsesReasoningEffort::Low => VertexThinkingLevel::Low,
            ResponsesReasoningEffort::Medium => VertexThinkingLevel::Medium,
            ResponsesReasoningEffort::High => VertexThinkingLevel::High,
        });

        Some(VertexThinkingConfig {
            thinking_level,
            thinking_budget: None,
            include_thoughts: Some(true), // Always include thoughts for Responses API
        })
    } else {
        // Gemini 2.5 models use thinking_budget
        let thinking_budget = reasoning.max_tokens.map(|t| t as i32).or_else(|| {
            // Map effort to budget ranges
            reasoning.effort.map(|effort| match effort {
                ResponsesReasoningEffort::None => 0,
                ResponsesReasoningEffort::Minimal => 1024,
                ResponsesReasoningEffort::Low => 4096,
                ResponsesReasoningEffort::Medium => 8192,
                ResponsesReasoningEffort::High => -1, // Dynamic budget
            })
        });

        // If no budget or effort specified but reasoning was requested, use dynamic
        let thinking_budget = thinking_budget.or(Some(-1));

        Some(VertexThinkingConfig {
            thinking_level: None,
            thinking_budget,
            include_thoughts: Some(true), // Always include thoughts for Responses API
        })
    }
}

/// Convert Chat Completion API reasoning config to Vertex ThinkingConfig.
///
/// This is similar to `convert_reasoning_to_thinking_config` but works with the simpler
/// `CreateChatCompletionReasoning` type from the Chat Completion API.
pub(super) fn convert_chat_completion_reasoning_to_thinking_config(
    reasoning: Option<&CreateChatCompletionReasoning>,
    model: &str,
) -> Option<VertexThinkingConfig> {
    let reasoning = reasoning?;

    // Only process if effort is specified
    let effort = reasoning.effort?;

    // Convert effort to the appropriate parameter based on model version
    if is_gemini_3_model(model) {
        // Gemini 3+ models use thinking_level
        let thinking_level = match effort {
            ReasoningEffort::None | ReasoningEffort::Minimal => VertexThinkingLevel::Minimal,
            ReasoningEffort::Low => VertexThinkingLevel::Low,
            ReasoningEffort::Medium => VertexThinkingLevel::Medium,
            ReasoningEffort::High => VertexThinkingLevel::High,
        };

        Some(VertexThinkingConfig {
            thinking_level: Some(thinking_level),
            thinking_budget: None,
            include_thoughts: Some(true), // Include thoughts in response
        })
    } else {
        // Gemini 2.5 models use thinking_budget
        let thinking_budget = match effort {
            ReasoningEffort::None => 0,
            ReasoningEffort::Minimal => 1024,
            ReasoningEffort::Low => 4096,
            ReasoningEffort::Medium => 8192,
            ReasoningEffort::High => -1, // Dynamic budget
        };

        Some(VertexThinkingConfig {
            thinking_level: None,
            thinking_budget: Some(thinking_budget),
            include_thoughts: Some(true), // Include thoughts in response
        })
    }
}

/// Convert Vertex response to OpenAI format.
pub(super) fn convert_response(
    vertex: VertexGenerateContentResponse,
    model: &str,
) -> OpenAIResponse {
    let (content, reasoning, tool_calls, finish_reason) = vertex
        .candidates
        .first()
        .map(|c| {
            let mut text_content = Vec::new();
            let mut thinking_content = Vec::new();
            let mut tool_calls = Vec::new();

            for part in &c.content.parts {
                if let Some(text) = &part.text {
                    // Separate thinking content (thought == true) from regular text
                    if part.thought {
                        thinking_content.push(text.clone());
                    } else {
                        text_content.push(text.clone());
                    }
                }
                if let Some(fc) = &part.function_call {
                    tool_calls.push(OpenAIToolCall {
                        id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                        type_: "function".to_string(),
                        function: OpenAIToolCallFunction {
                            name: fc.name.clone(),
                            arguments: serde_json::to_string(&fc.args).unwrap_or_default(),
                        },
                    });
                }
            }

            let text = text_content.join("");
            let thinking = thinking_content.join("");

            let reason = match c.finish_reason.as_deref() {
                Some("STOP") => {
                    if tool_calls.is_empty() {
                        Some("stop".to_string())
                    } else {
                        Some("tool_calls".to_string())
                    }
                }
                Some("MAX_TOKENS") => Some("length".to_string()),
                // Safety-related finish reasons -> content_filter
                Some("SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII") => {
                    Some("content_filter".to_string())
                }
                // Non-error completion reasons -> stop
                Some("RECITATION" | "OTHER" | "FINISH_REASON_UNSPECIFIED") => {
                    Some("stop".to_string())
                }
                other => other.map(String::from),
            };

            (text, thinking, tool_calls, reason)
        })
        .unwrap_or_default();

    let usage = vertex.usage_metadata.map(|u| OpenAIUsage {
        prompt_tokens: u.prompt_token_count,
        completion_tokens: u.candidates_token_count,
        total_tokens: u.total_token_count,
    });

    OpenAIResponse {
        id: format!("vertex-{}", uuid::Uuid::new_v4()),
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
        usage,
        system_fingerprint: None,
    }
}

// ============================================================================
// Responses API Conversion Functions
// ============================================================================

/// Convert OpenAI Responses API input to Vertex AI format.
/// Returns (system_instruction, contents).
pub(super) fn convert_responses_input_to_vertex(
    input: Option<ResponsesInput>,
    instructions: Option<String>,
) -> (Option<VertexContent>, Vec<VertexContent>) {
    let system_instruction = instructions.map(|text| VertexContent {
        role: "user".to_string(),
        parts: vec![VertexPart::text(text)],
    });

    let mut contents: Vec<VertexContent> = Vec::new();

    let Some(input) = input else {
        return (system_instruction, contents);
    };

    // Track function call IDs to function names for tool results
    let mut function_call_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // Collect function responses to batch into user messages
    let mut pending_function_responses: Vec<VertexPart> = Vec::new();

    match input {
        ResponsesInput::Text(text) => {
            // Simple text input becomes a single user message
            contents.push(VertexContent {
                role: "user".to_string(),
                parts: vec![VertexPart::text(text)],
            });
        }
        ResponsesInput::Items(items) => {
            for item in items {
                match item {
                    ResponsesInputItem::EasyMessage(msg) => {
                        // Flush pending function responses before adding new message
                        if !pending_function_responses.is_empty() {
                            contents.push(VertexContent {
                                role: "user".to_string(),
                                parts: std::mem::take(&mut pending_function_responses),
                            });
                        }

                        let role = match msg.role {
                            EasyInputMessageRole::User => "user",
                            EasyInputMessageRole::Assistant => "model",
                            EasyInputMessageRole::System | EasyInputMessageRole::Developer => {
                                // System/developer messages handled via instructions
                                continue;
                            }
                        };

                        let parts = match msg.content {
                            EasyInputMessageContent::Text(text) => {
                                if text.is_empty() {
                                    continue;
                                }
                                vec![VertexPart::text(text)]
                            }
                            EasyInputMessageContent::Parts(parts) => {
                                convert_responses_content_to_vertex_parts(&parts)
                            }
                        };

                        if !parts.is_empty() {
                            contents.push(VertexContent {
                                role: role.to_string(),
                                parts,
                            });
                        }
                    }
                    ResponsesInputItem::MessageItem(msg) => {
                        // Flush pending function responses
                        if !pending_function_responses.is_empty() {
                            contents.push(VertexContent {
                                role: "user".to_string(),
                                parts: std::mem::take(&mut pending_function_responses),
                            });
                        }

                        let role = match msg.role {
                            InputMessageItemRole::User => "user",
                            InputMessageItemRole::System | InputMessageItemRole::Developer => {
                                continue;
                            }
                        };

                        let parts = convert_responses_content_to_vertex_parts(&msg.content);
                        if !parts.is_empty() {
                            contents.push(VertexContent {
                                role: role.to_string(),
                                parts,
                            });
                        }
                    }
                    ResponsesInputItem::OutputMessage(msg) => {
                        // Flush pending function responses
                        if !pending_function_responses.is_empty() {
                            contents.push(VertexContent {
                                role: "user".to_string(),
                                parts: std::mem::take(&mut pending_function_responses),
                            });
                        }

                        // Output message from assistant
                        let mut parts = Vec::new();
                        for content_item in msg.content {
                            match content_item {
                                OutputMessageContentItem::OutputText { text, .. } => {
                                    if !text.is_empty() {
                                        parts.push(VertexPart::text(text));
                                    }
                                }
                                OutputMessageContentItem::Refusal { refusal } => {
                                    if !refusal.is_empty() {
                                        parts.push(VertexPart::text(refusal));
                                    }
                                }
                            }
                        }

                        if !parts.is_empty() {
                            contents.push(VertexContent {
                                role: "model".to_string(),
                                parts,
                            });
                        }
                    }
                    ResponsesInputItem::FunctionCall(call) => {
                        // Flush pending function responses
                        if !pending_function_responses.is_empty() {
                            contents.push(VertexContent {
                                role: "user".to_string(),
                                parts: std::mem::take(&mut pending_function_responses),
                            });
                        }

                        // Store mapping for function responses
                        function_call_names.insert(call.call_id.clone(), call.name.clone());

                        // Function call from assistant
                        let args: serde_json::Value =
                            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
                        contents.push(VertexContent {
                            role: "model".to_string(),
                            parts: vec![VertexPart::function_call(call.name, args)],
                        });
                    }
                    ResponsesInputItem::OutputFunctionCall(call) => {
                        // Flush pending function responses
                        if !pending_function_responses.is_empty() {
                            contents.push(VertexContent {
                                role: "user".to_string(),
                                parts: std::mem::take(&mut pending_function_responses),
                            });
                        }

                        // Store mapping for function responses
                        function_call_names.insert(call.call_id.clone(), call.name.clone());

                        // Output function call from assistant
                        let args: serde_json::Value =
                            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
                        contents.push(VertexContent {
                            role: "model".to_string(),
                            parts: vec![VertexPart::function_call(call.name, args)],
                        });
                    }
                    ResponsesInputItem::FunctionCallOutput(output) => {
                        // Get function name from mapping
                        let function_name = function_call_names
                            .get(&output.call_id)
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string());

                        // Parse output as JSON or wrap in result object
                        let response_value = serde_json::from_str(&output.output)
                            .unwrap_or_else(|_| serde_json::json!({"result": output.output}));

                        pending_function_responses
                            .push(VertexPart::function_response(function_name, response_value));
                    }
                    ResponsesInputItem::Reasoning(_) => {
                        // Reasoning blocks from previous responses - skip
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
                        // Server-side tool calls not supported by Vertex
                    }
                }
            }

            // Flush any remaining function responses
            if !pending_function_responses.is_empty() {
                contents.push(VertexContent {
                    role: "user".to_string(),
                    parts: pending_function_responses,
                });
            }
        }
    }

    (system_instruction, contents)
}

/// Convert Responses API content items to Vertex parts.
pub(super) fn convert_responses_content_to_vertex_parts(
    items: &[ResponseInputContentItem],
) -> Vec<VertexPart> {
    let mut parts = Vec::new();

    for item in items {
        match item {
            ResponseInputContentItem::InputText { text, .. } => {
                if !text.is_empty() {
                    parts.push(VertexPart::text(text.clone()));
                }
            }
            ResponseInputContentItem::InputImage { image_url, .. } => {
                if let Some(url) = image_url {
                    match parse_data_url(url) {
                        Ok(image_data) => {
                            parts.push(VertexPart::inline_data(
                                image_data.media_type,
                                image_data.data,
                            ));
                        }
                        Err(e) => {
                            tracing::warn!(
                                url = %url,
                                error = %e,
                                "Failed to parse image data URL, skipping in Vertex conversion"
                            );
                        }
                    }
                }
            }
            ResponseInputContentItem::InputFile { .. } => {
                tracing::warn!("File inputs not supported by Vertex AI in Responses API");
            }
            ResponseInputContentItem::InputAudio { .. } => {
                tracing::warn!("Audio inputs not supported by Vertex AI in Responses API");
            }
        }
    }

    parts
}

/// Convert Responses API tools to Vertex format.
pub(super) fn convert_responses_tools_to_vertex(
    tools: Option<Vec<ResponsesToolDefinition>>,
) -> Option<Vec<VertexTool>> {
    let tools = tools?;
    let mut function_declarations = Vec::new();

    for tool in tools {
        match tool {
            ResponsesToolDefinition::Function(func) => {
                function_declarations.push(VertexFunctionDeclaration {
                    name: func.name,
                    description: func.description,
                    parameters: func.parameters,
                });
            }
            ResponsesToolDefinition::WebSearchPreview(_)
            | ResponsesToolDefinition::WebSearchPreview20250311(_)
            | ResponsesToolDefinition::WebSearch(_)
            | ResponsesToolDefinition::WebSearch20250826(_) => {
                // Dead code: preprocessed to function tools in execution.rs
                tracing::warn!("Unexpected web_search tool variant reached Vertex conversion");
            }
            ResponsesToolDefinition::Shell(_) => {
                tracing::warn!(
                    "Shell tool reached Vertex conversion — only OpenAI passthrough is \
                     supported for shell in the current build; dropping the tool definition"
                );
            }
            ResponsesToolDefinition::Mcp(_) => {
                tracing::warn!(
                    "MCP tool reached Vertex conversion — `mcp` requires `mode = \
                     passthrough_openai` and an OpenAI/Azure upstream; dropping the tool definition"
                );
            }
            ResponsesToolDefinition::ToolSearch(_) => {
                tracing::warn!(
                    "tool_search tool reached Vertex conversion — should have been consumed \
                     by the MCP rewrite under hadrian_hosted; dropping the tool definition"
                );
            }
            ResponsesToolDefinition::FileSearch(_) => {
                // File search is handled by the gateway middleware, but the model needs to know
                // how to call it. Convert to a function declaration so the model can generate proper
                // tool calls that the middleware will intercept and execute.
                function_declarations.push(VertexFunctionDeclaration {
                    name: FileSearchToolArguments::FUNCTION_NAME.to_string(),
                    description: Some(FileSearchToolArguments::function_description().to_string()),
                    parameters: Some(FileSearchToolArguments::function_parameters_schema()),
                });
                tracing::debug!("Converted file_search tool to function declaration for model");
            }
        }
    }

    if function_declarations.is_empty() {
        None
    } else {
        Some(vec![VertexTool {
            function_declarations,
        }])
    }
}

/// Convert Responses API tool choice to Vertex format.
pub(super) fn convert_responses_tool_choice_to_vertex(
    tool_choice: Option<ResponsesToolChoice>,
) -> Option<VertexToolConfig> {
    tool_choice.map(|tc| {
        let (mode, allowed_names) = match tc {
            ResponsesToolChoice::String(default) => match default {
                ResponsesToolChoiceDefault::Auto => (VertexFunctionCallingMode::Auto, None),
                ResponsesToolChoiceDefault::Required => (VertexFunctionCallingMode::Any, None),
                ResponsesToolChoiceDefault::None => (VertexFunctionCallingMode::None, None),
            },
            ResponsesToolChoice::Named(named) => {
                (VertexFunctionCallingMode::Any, Some(vec![named.name]))
            }
            ResponsesToolChoice::WebSearch(_) => {
                tracing::warn!("Web search tool choice not supported by Vertex AI");
                (VertexFunctionCallingMode::Auto, None)
            }
            ResponsesToolChoice::Shell(_) => (
                VertexFunctionCallingMode::Any,
                Some(vec!["shell".to_string()]),
            ),
            ResponsesToolChoice::Mcp(_) => {
                // Reaches Vertex only when the hadrian_hosted rewrite
                // was skipped. Fall back to forcing any tool.
                tracing::warn!("MCP tool choice without a hosted rewrite; falling back to `any`");
                (VertexFunctionCallingMode::Any, None)
            }
        };
        VertexToolConfig {
            function_calling_config: VertexFunctionCallingConfig {
                mode,
                allowed_function_names: allowed_names,
            },
        }
    })
}

/// Convert Vertex response to OpenAI Responses API format.
pub(super) fn convert_vertex_to_responses_response(
    vertex: VertexGenerateContentResponse,
    model: &str,
    reasoning_config: Option<&ResponsesReasoningConfig>,
    user: Option<String>,
) -> CreateResponsesResponse {
    let mut output: Vec<ResponsesOutputItem> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();

    // Process candidates, separating thinking content from regular text
    if let Some(candidate) = vertex.candidates.first() {
        for part in &candidate.content.parts {
            if let Some(text) = &part.text
                && !text.is_empty()
            {
                // Separate thinking content (thought == true) from regular text
                if part.thought {
                    thinking_parts.push(text.clone());
                } else {
                    text_parts.push(text.clone());
                }
            }
            if let Some(fc) = &part.function_call {
                let call_id = format!("call_{}", uuid::Uuid::new_v4().simple());
                output.push(ResponsesOutputItem::FunctionCall(OutputItemFunctionCall {
                    type_: OutputItemFunctionCallType::FunctionCall,
                    id: Some(call_id.clone()),
                    name: fc.name.clone(),
                    arguments: serde_json::to_string(&fc.args).unwrap_or_default(),
                    call_id,
                    status: Some(OutputItemFunctionCallStatus::Completed),
                }));
            }
        }
    }

    // Create output text (regular content only, not thinking)
    let output_text = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(""))
    };

    // Create message content
    let message_content: Vec<OutputMessageContentItem> = if let Some(ref text) = output_text {
        vec![OutputMessageContentItem::OutputText {
            text: text.clone(),
            annotations: vec![],
            logprobs: vec![],
        }]
    } else {
        vec![]
    };

    // Add reasoning output item if there's thinking content
    if !thinking_parts.is_empty() {
        let thinking_text = thinking_parts.join("");
        output.push(ResponsesOutputItem::Reasoning(ResponsesReasoning {
            type_: ResponsesReasoningType::Reasoning,
            id: format!("rs_{}", uuid::Uuid::new_v4().simple()),
            content: None, // Gemini provides summaries, not detailed thinking content
            summary: vec![ReasoningSummaryText {
                type_: ReasoningSummaryTextType::SummaryText,
                text: thinking_text,
            }],
            encrypted_content: None,
            status: Some(OutputItemReasoningStatus::Completed),
            signature: None,
            format: Some(OpenResponsesReasoningFormat::GoogleGeminiV1),
        }));
    }

    // Add message if there's content or no function calls (and no reasoning)
    if !message_content.is_empty() || (output.is_empty()) {
        let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        // Insert message at the beginning (after reasoning if present)
        let insert_pos = if !thinking_parts.is_empty() { 1 } else { 0 };
        output.insert(
            insert_pos,
            ResponsesOutputItem::Message(OutputMessage {
                id: msg_id,
                type_: MessageType::Message,
                role: "assistant".to_string(),
                content: message_content,
                status: Some(OutputMessageStatus::Completed),
            }),
        );
    }

    // Determine status based on finish_reason
    let status = vertex
        .candidates
        .first()
        .and_then(|c| c.finish_reason.as_deref())
        .map(|reason| match reason {
            "STOP" | "RECITATION" | "OTHER" | "FINISH_REASON_UNSPECIFIED" => {
                ResponsesResponseStatus::Completed
            }
            "MAX_TOKENS" => ResponsesResponseStatus::Incomplete,
            // Safety-related finish reasons -> Failed
            "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" => {
                ResponsesResponseStatus::Failed
            }
            _ => ResponsesResponseStatus::Completed,
        })
        .unwrap_or(ResponsesResponseStatus::Completed);

    // Build reasoning config output
    let reasoning_output = reasoning_config.map(|config| ResponsesReasoningConfigOutput {
        effort: config.effort,
        summary: config.summary,
    });

    // Build usage, including thoughts_token_count as reasoning_tokens
    let usage = vertex.usage_metadata.map(|u| ResponsesUsage {
        input_tokens: u.prompt_token_count,
        input_tokens_details: ResponsesUsageInputTokensDetails { cached_tokens: 0 },
        output_tokens: u.candidates_token_count,
        output_tokens_details: ResponsesUsageOutputTokensDetails {
            reasoning_tokens: u.thoughts_token_count,
        },
        total_tokens: u.total_token_count,
        cost: None,
        is_byok: None,
        cost_details: None,
    });

    CreateResponsesResponse {
        id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
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
        usage,
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
mod image_tests {
    use super::*;
    use crate::api_types::chat_completion::ImageUrl;

    // Note: Tests for parse_data_url are in src/providers/image.rs
    // These tests focus on Vertex-specific image handling integration

    #[test]
    fn test_convert_content_to_parts_text_only() {
        let content = MessageContent::Text("Hello world".to_string());
        let parts = convert_content_to_parts(&content);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].text, Some("Hello world".to_string()));
        assert!(parts[0].inline_data.is_none());
    }

    #[test]
    fn test_convert_content_to_parts_empty() {
        let content = MessageContent::Text("".to_string());
        let parts = convert_content_to_parts(&content);
        assert!(parts.is_empty());
    }

    #[test]
    fn test_convert_content_to_parts_with_data_url_image() {
        // Tests integration of shared parse_data_url with Vertex content conversion
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

        let parts = convert_content_to_parts(&content);
        assert_eq!(parts.len(), 2);

        // First part is text
        assert_eq!(parts[0].text, Some("What's in this image?".to_string()));
        assert!(parts[0].inline_data.is_none());

        // Second part is image (using shared parse_data_url)
        assert!(parts[1].text.is_none());
        let inline_data = parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline_data.mime_type, "image/png");
        assert_eq!(inline_data.data, "iVBORw0KGgo=");
    }

    #[test]
    fn test_convert_content_to_parts_multiple_image_formats() {
        // Test various image formats Vertex supports: png, jpeg, gif, webp
        let content = MessageContent::Parts(vec![
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/jpeg;base64,/9j/4AAQ".to_string(),
                    detail: None,
                },
                cache_control: None,
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/gif;base64,R0lGODlh".to_string(),
                    detail: None,
                },
                cache_control: None,
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/webp;base64,UklGRg==".to_string(),
                    detail: None,
                },
                cache_control: None,
            },
        ]);

        let parts = convert_content_to_parts(&content);
        assert_eq!(parts.len(), 3);

        assert_eq!(
            parts[0].inline_data.as_ref().unwrap().mime_type,
            "image/jpeg"
        );
        assert_eq!(
            parts[1].inline_data.as_ref().unwrap().mime_type,
            "image/gif"
        );
        assert_eq!(
            parts[2].inline_data.as_ref().unwrap().mime_type,
            "image/webp"
        );
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

        let mut tool_call_names = std::collections::HashMap::new();
        let (_, vertex_contents) = convert_messages(messages, &mut tool_call_names);
        assert_eq!(vertex_contents.len(), 1);
        assert_eq!(vertex_contents[0].role, "user");
        assert_eq!(vertex_contents[0].parts.len(), 2);

        // First part is text
        assert_eq!(
            vertex_contents[0].parts[0].text,
            Some("Describe this image".to_string())
        );

        // Second part is image
        let inline_data = vertex_contents[0].parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline_data.mime_type, "image/jpeg");
        assert_eq!(inline_data.data, "/9j/4AAQ");
    }
}

#[cfg(test)]
mod responses_api_tests {
    use super::{
        super::types::{
            VertexCandidate, VertexFunctionCall, VertexResponseContent, VertexResponsePart,
            VertexUsageMetadata,
        },
        *,
    };
    use crate::api_types::responses::{
        EasyInputMessage, EasyInputMessageContent, FunctionCallOutput, FunctionCallOutputType,
        FunctionTool, FunctionToolCall, FunctionToolCallType, OutputItemFunctionCallStatus,
        OutputItemFunctionCallType, OutputMessage, OutputMessageContentItem, OutputMessageStatus,
        ResponseInputImageDetail, ResponsesNamedToolChoice, ResponsesNamedToolChoiceType,
    };

    // ============================================================================
    // Input Conversion Tests
    // ============================================================================

    #[test]
    fn test_convert_responses_input_text() {
        let input = Some(ResponsesInput::Text("Hello, world!".to_string()));
        let (system, contents) = convert_responses_input_to_vertex(input, None);

        assert!(system.is_none());
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[0].parts.len(), 1);
        assert_eq!(contents[0].parts[0].text, Some("Hello, world!".to_string()));
    }

    #[test]
    fn test_convert_responses_input_with_instructions() {
        let input = Some(ResponsesInput::Text("What's 2+2?".to_string()));
        let instructions = Some("You are a helpful math assistant.".to_string());
        let (system, contents) = convert_responses_input_to_vertex(input, instructions);

        // System instruction should be set
        assert!(system.is_some());
        let system = system.unwrap();
        assert_eq!(system.role, "user"); // Gemini uses user role for system instruction
        assert_eq!(system.parts.len(), 1);
        assert_eq!(
            system.parts[0].text,
            Some("You are a helpful math assistant.".to_string())
        );

        // Contents should have user message
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
    }

    #[test]
    fn test_convert_responses_input_easy_messages() {
        let input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: Some(MessageType::Message),
                role: EasyInputMessageRole::User,
                content: EasyInputMessageContent::Text("Hello".to_string()),
            }),
            ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: Some(MessageType::Message),
                role: EasyInputMessageRole::Assistant,
                content: EasyInputMessageContent::Text("Hi there!".to_string()),
            }),
            ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: Some(MessageType::Message),
                role: EasyInputMessageRole::User,
                content: EasyInputMessageContent::Text("How are you?".to_string()),
            }),
        ]));

        let (_, contents) = convert_responses_input_to_vertex(input, None);

        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[0].parts[0].text, Some("Hello".to_string()));
        assert_eq!(contents[1].role, "model"); // Assistant -> model
        assert_eq!(contents[1].parts[0].text, Some("Hi there!".to_string()));
        assert_eq!(contents[2].role, "user");
        assert_eq!(contents[2].parts[0].text, Some("How are you?".to_string()));
    }

    #[test]
    fn test_convert_responses_input_with_function_call() {
        let input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: Some(MessageType::Message),
                role: EasyInputMessageRole::User,
                content: EasyInputMessageContent::Text(
                    "What's the weather in Seattle?".to_string(),
                ),
            }),
            ResponsesInputItem::FunctionCall(FunctionToolCall {
                type_: FunctionToolCallType::FunctionCall,
                id: "fc_123".to_string(),
                call_id: "call_abc".to_string(),
                name: "get_weather".to_string(),
                arguments: r#"{"location": "Seattle"}"#.to_string(),
                status: None,
            }),
            ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: FunctionCallOutputType::FunctionCallOutput,
                id: None,
                call_id: "call_abc".to_string(),
                output: r#"{"temp": 55, "condition": "cloudy"}"#.to_string(),
                status: None,
            }),
        ]));

        let (_, contents) = convert_responses_input_to_vertex(input, None);

        assert_eq!(contents.len(), 3);

        // User message
        assert_eq!(contents[0].role, "user");

        // Function call from model
        assert_eq!(contents[1].role, "model");
        let fc = contents[1].parts[0].function_call.as_ref().unwrap();
        assert_eq!(fc.name, "get_weather");
        assert_eq!(fc.args, serde_json::json!({"location": "Seattle"}));

        // Function response in user message
        assert_eq!(contents[2].role, "user");
        let fr = contents[2].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr.name, "get_weather");
        assert_eq!(
            fr.response,
            serde_json::json!({"temp": 55, "condition": "cloudy"})
        );
    }

    #[test]
    fn test_convert_responses_input_output_message() {
        let input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::OutputMessage(OutputMessage {
                id: "msg_123".to_string(),
                type_: MessageType::Message,
                role: "assistant".to_string(),
                content: vec![OutputMessageContentItem::OutputText {
                    text: "I'm a previous assistant response.".to_string(),
                    annotations: vec![],
                    logprobs: vec![],
                }],
                status: Some(OutputMessageStatus::Completed),
            }),
        ]));

        let (_, contents) = convert_responses_input_to_vertex(input, None);

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "model");
        assert_eq!(
            contents[0].parts[0].text,
            Some("I'm a previous assistant response.".to_string())
        );
    }

    #[test]
    fn test_convert_responses_content_with_image() {
        let items = vec![
            ResponseInputContentItem::InputText {
                text: "What's in this image?".to_string(),
                cache_control: None,
            },
            ResponseInputContentItem::InputImage {
                detail: ResponseInputImageDetail::Auto,
                image_url: Some("data:image/png;base64,iVBORw0KGgo=".to_string()),
                cache_control: None,
            },
        ];

        let parts = convert_responses_content_to_vertex_parts(&items);

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].text, Some("What's in this image?".to_string()));
        let inline_data = parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline_data.mime_type, "image/png");
        assert_eq!(inline_data.data, "iVBORw0KGgo=");
    }

    // ============================================================================
    // Tools Conversion Tests
    // ============================================================================

    #[test]
    fn test_convert_responses_tools_function() {
        let tools = Some(vec![ResponsesToolDefinition::Function(
            FunctionTool::from_json(serde_json::json!({
                "type": "function",
                "name": "get_weather",
                "description": "Get weather for a location",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }
            }))
            .unwrap(),
        )]);

        let vertex_tools = convert_responses_tools_to_vertex(tools).unwrap();

        assert_eq!(vertex_tools.len(), 1);
        assert_eq!(vertex_tools[0].function_declarations.len(), 1);
        let fd = &vertex_tools[0].function_declarations[0];
        assert_eq!(fd.name, "get_weather");
        assert_eq!(
            fd.description,
            Some("Get weather for a location".to_string())
        );
        assert!(fd.parameters.is_some());
    }

    #[test]
    fn test_convert_responses_tools_multiple() {
        let tools = Some(vec![
            ResponsesToolDefinition::Function(
                FunctionTool::from_json(serde_json::json!({
                    "name": "tool1",
                    "description": "First tool"
                }))
                .unwrap(),
            ),
            ResponsesToolDefinition::Function(
                FunctionTool::from_json(serde_json::json!({
                    "name": "tool2",
                    "description": "Second tool"
                }))
                .unwrap(),
            ),
        ]);

        let vertex_tools = convert_responses_tools_to_vertex(tools).unwrap();

        assert_eq!(vertex_tools.len(), 1); // All functions in one VertexTool
        assert_eq!(vertex_tools[0].function_declarations.len(), 2);
        assert_eq!(vertex_tools[0].function_declarations[0].name, "tool1");
        assert_eq!(vertex_tools[0].function_declarations[1].name, "tool2");
    }

    #[test]
    fn test_convert_responses_tools_none() {
        let result = convert_responses_tools_to_vertex(None);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_responses_tools_empty() {
        let result = convert_responses_tools_to_vertex(Some(vec![]));
        assert!(result.is_none());
    }

    // ============================================================================
    // Tool Choice Conversion Tests
    // ============================================================================

    #[test]
    fn test_convert_responses_tool_choice_auto() {
        let choice = Some(ResponsesToolChoice::String(
            ResponsesToolChoiceDefault::Auto,
        ));
        let config = convert_responses_tool_choice_to_vertex(choice).unwrap();

        matches!(
            config.function_calling_config.mode,
            VertexFunctionCallingMode::Auto
        );
        assert!(
            config
                .function_calling_config
                .allowed_function_names
                .is_none()
        );
    }

    #[test]
    fn test_convert_responses_tool_choice_required() {
        let choice = Some(ResponsesToolChoice::String(
            ResponsesToolChoiceDefault::Required,
        ));
        let config = convert_responses_tool_choice_to_vertex(choice).unwrap();

        matches!(
            config.function_calling_config.mode,
            VertexFunctionCallingMode::Any
        );
    }

    #[test]
    fn test_convert_responses_tool_choice_none() {
        let choice = Some(ResponsesToolChoice::String(
            ResponsesToolChoiceDefault::None,
        ));
        let config = convert_responses_tool_choice_to_vertex(choice).unwrap();

        matches!(
            config.function_calling_config.mode,
            VertexFunctionCallingMode::None
        );
    }

    #[test]
    fn test_convert_responses_tool_choice_named() {
        let choice = Some(ResponsesToolChoice::Named(ResponsesNamedToolChoice {
            type_: ResponsesNamedToolChoiceType::Function,
            name: "specific_function".to_string(),
        }));
        let config = convert_responses_tool_choice_to_vertex(choice).unwrap();

        matches!(
            config.function_calling_config.mode,
            VertexFunctionCallingMode::Any
        );
        assert_eq!(
            config.function_calling_config.allowed_function_names,
            Some(vec!["specific_function".to_string()])
        );
    }

    // ============================================================================
    // Response Conversion Tests
    // ============================================================================

    #[test]
    fn test_convert_vertex_to_responses_text() {
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![VertexResponsePart {
                        text: Some("Hello, world!".to_string()),
                        function_call: None,
                        thought: false,
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(VertexUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
                thoughts_token_count: 0,
            }),
        };

        let response =
            convert_vertex_to_responses_response(vertex_response, "gemini-2.0-flash", None, None);

        assert!(response.id.starts_with("resp_"));
        assert_eq!(response.model, "gemini-2.0-flash");
        assert_eq!(response.status, Some(ResponsesResponseStatus::Completed));
        assert_eq!(response.output_text, Some("Hello, world!".to_string()));

        // Check output structure
        assert_eq!(response.output.len(), 1);
        match &response.output[0] {
            ResponsesOutputItem::Message(msg) => {
                assert_eq!(msg.role, "assistant");
                assert_eq!(msg.content.len(), 1);
                match &msg.content[0] {
                    OutputMessageContentItem::OutputText { text, .. } => {
                        assert_eq!(text, "Hello, world!");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message"),
        }

        // Check usage
        let usage = response.usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn test_convert_vertex_to_responses_function_call() {
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![VertexResponsePart {
                        text: None,
                        function_call: Some(VertexFunctionCall {
                            name: "get_weather".to_string(),
                            args: serde_json::json!({"location": "Seattle"}),
                        }),
                        thought: false,
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let response =
            convert_vertex_to_responses_response(vertex_response, "gemini-2.0-flash", None, None);

        // Should have only function call (no text means no message needed)
        assert_eq!(response.output.len(), 1);

        // Check function call output
        match &response.output[0] {
            ResponsesOutputItem::FunctionCall(fc) => {
                assert_eq!(fc.type_, OutputItemFunctionCallType::FunctionCall);
                assert_eq!(fc.name, "get_weather");
                assert_eq!(fc.arguments, r#"{"location":"Seattle"}"#);
                assert!(fc.call_id.starts_with("call_"));
                assert_eq!(fc.status, Some(OutputItemFunctionCallStatus::Completed));
            }
            _ => panic!("Expected FunctionCall"),
        }
    }

    #[test]
    fn test_convert_vertex_to_responses_max_tokens() {
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![VertexResponsePart {
                        text: Some("Truncated response...".to_string()),
                        function_call: None,
                        thought: false,
                    }],
                },
                finish_reason: Some("MAX_TOKENS".to_string()),
            }],
            usage_metadata: None,
        };

        let response =
            convert_vertex_to_responses_response(vertex_response, "gemini-2.0-flash", None, None);

        assert_eq!(response.status, Some(ResponsesResponseStatus::Incomplete));
    }

    #[test]
    fn test_convert_vertex_to_responses_safety() {
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent { parts: vec![] },
                finish_reason: Some("SAFETY".to_string()),
            }],
            usage_metadata: None,
        };

        let response =
            convert_vertex_to_responses_response(vertex_response, "gemini-2.0-flash", None, None);

        assert_eq!(response.status, Some(ResponsesResponseStatus::Failed));
    }

    #[test]
    fn test_convert_vertex_to_responses_with_user() {
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![VertexResponsePart {
                        text: Some("Hello!".to_string()),
                        function_call: None,
                        thought: false,
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let response = convert_vertex_to_responses_response(
            vertex_response,
            "gemini-2.0-flash",
            None,
            Some("user_123".to_string()),
        );

        assert_eq!(response.user, Some("user_123".to_string()));
    }

    #[test]
    fn test_convert_vertex_to_responses_multiple_text_parts() {
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![
                        VertexResponsePart {
                            text: Some("Part 1. ".to_string()),
                            function_call: None,
                            thought: false,
                        },
                        VertexResponsePart {
                            text: Some("Part 2.".to_string()),
                            function_call: None,
                            thought: false,
                        },
                    ],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let response =
            convert_vertex_to_responses_response(vertex_response, "gemini-2.0-flash", None, None);

        // Text parts should be joined
        assert_eq!(response.output_text, Some("Part 1. Part 2.".to_string()));
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

        let result = convert_responses_tools_to_vertex(tools);

        // FileSearch should be converted to a function declaration
        assert!(result.is_some());
        let vertex_tools = result.unwrap();
        assert_eq!(vertex_tools.len(), 1);
        assert_eq!(vertex_tools[0].function_declarations.len(), 1);

        let fd = &vertex_tools[0].function_declarations[0];
        assert_eq!(fd.name, "file_search");
        assert!(fd.description.is_some());
        assert!(fd.parameters.is_some());

        // Check the parameters contain the query property
        let params = fd.parameters.as_ref().unwrap();
        assert_eq!(params["type"], "object");
        assert!(params["properties"].get("query").is_some());
    }

    #[test]
    fn test_convert_responses_tools_mixed_with_file_search() {
        use crate::api_types::responses::{FileSearchTool, FileSearchToolType};

        let tools = Some(vec![
            ResponsesToolDefinition::Function(
                FunctionTool::from_json(serde_json::json!({
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

        let result = convert_responses_tools_to_vertex(tools);

        assert!(result.is_some());
        let vertex_tools = result.unwrap();
        assert_eq!(vertex_tools.len(), 1);
        assert_eq!(vertex_tools[0].function_declarations.len(), 2);

        // First should be the regular function
        assert_eq!(vertex_tools[0].function_declarations[0].name, "get_weather");

        // Second should be the converted file_search
        assert_eq!(vertex_tools[0].function_declarations[1].name, "file_search");
    }

    #[test]
    fn test_convert_vertex_to_responses_safety_finish_reasons() {
        // Test all safety-related finish reasons map to Failed status
        for finish_reason in ["SAFETY", "PROHIBITED_CONTENT", "BLOCKLIST", "SPII"] {
            let vertex_response = VertexGenerateContentResponse {
                candidates: vec![VertexCandidate {
                    content: VertexResponseContent { parts: vec![] },
                    finish_reason: Some(finish_reason.to_string()),
                }],
                usage_metadata: None,
            };

            let response = convert_vertex_to_responses_response(
                vertex_response,
                "gemini-2.0-flash",
                None,
                None,
            );

            assert_eq!(
                response.status,
                Some(ResponsesResponseStatus::Failed),
                "Expected Failed status for finish_reason: {}",
                finish_reason
            );
        }
    }

    #[test]
    fn test_convert_vertex_to_responses_completed_finish_reasons() {
        // Test all completed-related finish reasons map to Completed status
        for finish_reason in ["STOP", "RECITATION", "OTHER", "FINISH_REASON_UNSPECIFIED"] {
            let vertex_response = VertexGenerateContentResponse {
                candidates: vec![VertexCandidate {
                    content: VertexResponseContent {
                        parts: vec![VertexResponsePart {
                            text: Some("Test".to_string()),
                            function_call: None,
                            thought: false,
                        }],
                    },
                    finish_reason: Some(finish_reason.to_string()),
                }],
                usage_metadata: None,
            };

            let response = convert_vertex_to_responses_response(
                vertex_response,
                "gemini-2.0-flash",
                None,
                None,
            );

            assert_eq!(
                response.status,
                Some(ResponsesResponseStatus::Completed),
                "Expected Completed status for finish_reason: {}",
                finish_reason
            );
        }
    }

    #[test]
    fn test_convert_vertex_to_openai_finish_reasons() {
        // Test Chat Completion finish reason mappings
        let test_cases = [
            ("STOP", "stop"),
            ("MAX_TOKENS", "length"),
            ("SAFETY", "content_filter"),
            ("PROHIBITED_CONTENT", "content_filter"),
            ("BLOCKLIST", "content_filter"),
            ("SPII", "content_filter"),
            ("RECITATION", "stop"),
            ("OTHER", "stop"),
            ("FINISH_REASON_UNSPECIFIED", "stop"),
        ];

        for (vertex_reason, expected_openai_reason) in test_cases {
            let vertex_response = VertexGenerateContentResponse {
                candidates: vec![VertexCandidate {
                    content: VertexResponseContent {
                        parts: vec![VertexResponsePart {
                            text: Some("Test".to_string()),
                            function_call: None,
                            thought: false,
                        }],
                    },
                    finish_reason: Some(vertex_reason.to_string()),
                }],
                usage_metadata: None,
            };

            let response = convert_response(vertex_response, "gemini-2.0-flash");

            assert_eq!(
                response.choices[0].finish_reason,
                Some(expected_openai_reason.to_string()),
                "Expected '{}' for Vertex finish_reason '{}', got {:?}",
                expected_openai_reason,
                vertex_reason,
                response.choices[0].finish_reason
            );
        }
    }

    // ============================================================================
    // Thinking/Reasoning Content Extraction Tests
    // ============================================================================

    #[test]
    fn test_convert_response_with_thinking_content() {
        // Test Chat Completion API thinking extraction
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![
                        VertexResponsePart {
                            text: Some("Let me think about this...".to_string()),
                            function_call: None,
                            thought: true, // This is thinking content
                        },
                        VertexResponsePart {
                            text: Some("The answer is 42.".to_string()),
                            function_call: None,
                            thought: false, // This is regular content
                        },
                    ],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(VertexUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 15,
                total_token_count: 25,
                thoughts_token_count: 8,
            }),
        };

        let response = convert_response(vertex_response, "gemini-2.0-flash-thinking");

        // Check that thinking content is separated into reasoning field
        assert_eq!(
            response.choices[0].message.content,
            Some("The answer is 42.".to_string())
        );
        assert_eq!(
            response.choices[0].message.reasoning,
            Some("Let me think about this...".to_string())
        );
    }

    #[test]
    fn test_convert_response_without_thinking_content() {
        // Test that responses without thinking don't have reasoning field
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![VertexResponsePart {
                        text: Some("The answer is 42.".to_string()),
                        function_call: None,
                        thought: false,
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let response = convert_response(vertex_response, "gemini-2.0-flash");

        assert_eq!(
            response.choices[0].message.content,
            Some("The answer is 42.".to_string())
        );
        assert_eq!(response.choices[0].message.reasoning, None);
    }

    #[test]
    fn test_convert_response_only_thinking_content() {
        // Test response with only thinking content (no regular text)
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![VertexResponsePart {
                        text: Some("Deep reasoning process...".to_string()),
                        function_call: None,
                        thought: true,
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let response = convert_response(vertex_response, "gemini-2.0-flash-thinking");

        assert_eq!(response.choices[0].message.content, None);
        assert_eq!(
            response.choices[0].message.reasoning,
            Some("Deep reasoning process...".to_string())
        );
    }

    #[test]
    fn test_convert_vertex_to_responses_with_thinking() {
        // Test Responses API thinking extraction
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![
                        VertexResponsePart {
                            text: Some("I need to analyze this carefully.".to_string()),
                            function_call: None,
                            thought: true,
                        },
                        VertexResponsePart {
                            text: Some("The result is 42.".to_string()),
                            function_call: None,
                            thought: false,
                        },
                    ],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(VertexUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 20,
                total_token_count: 30,
                thoughts_token_count: 12,
            }),
        };

        let response = convert_vertex_to_responses_response(
            vertex_response,
            "gemini-2.0-flash-thinking",
            None,
            None,
        );

        // Check output structure - should have Reasoning first, then Message
        assert_eq!(response.output.len(), 2);

        // First output item should be Reasoning
        match &response.output[0] {
            ResponsesOutputItem::Reasoning(reasoning) => {
                assert!(reasoning.id.starts_with("rs_"));
                assert_eq!(reasoning.type_, ResponsesReasoningType::Reasoning);
                assert_eq!(reasoning.summary.len(), 1);
                assert_eq!(
                    reasoning.summary[0].text,
                    "I need to analyze this carefully."
                );
                assert_eq!(reasoning.status, Some(OutputItemReasoningStatus::Completed));
                assert_eq!(
                    reasoning.format,
                    Some(OpenResponsesReasoningFormat::GoogleGeminiV1)
                );
            }
            _ => panic!("Expected Reasoning as first output item"),
        }

        // Second output item should be Message
        match &response.output[1] {
            ResponsesOutputItem::Message(msg) => {
                assert_eq!(msg.role, "assistant");
                assert_eq!(msg.content.len(), 1);
                match &msg.content[0] {
                    OutputMessageContentItem::OutputText { text, .. } => {
                        assert_eq!(text, "The result is 42.");
                    }
                    _ => panic!("Expected OutputText"),
                }
            }
            _ => panic!("Expected Message as second output item"),
        }

        // Check that output_text only contains regular text
        assert_eq!(response.output_text, Some("The result is 42.".to_string()));

        // Check usage includes reasoning tokens
        let usage = response.usage.unwrap();
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 12);
    }

    #[test]
    fn test_convert_vertex_to_responses_without_thinking() {
        // Test that responses without thinking don't have Reasoning output
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![VertexResponsePart {
                        text: Some("Simple response.".to_string()),
                        function_call: None,
                        thought: false,
                    }],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(VertexUsageMetadata {
                prompt_token_count: 5,
                candidates_token_count: 3,
                total_token_count: 8,
                thoughts_token_count: 0,
            }),
        };

        let response =
            convert_vertex_to_responses_response(vertex_response, "gemini-2.0-flash", None, None);

        // Should only have Message, no Reasoning
        assert_eq!(response.output.len(), 1);
        match &response.output[0] {
            ResponsesOutputItem::Message(msg) => {
                assert_eq!(msg.content.len(), 1);
            }
            _ => panic!("Expected Message, not Reasoning"),
        }

        // Check usage has 0 reasoning tokens
        let usage = response.usage.unwrap();
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 0);
    }

    #[test]
    fn test_convert_vertex_to_responses_multiple_thinking_parts() {
        // Test multiple thinking parts are concatenated
        let vertex_response = VertexGenerateContentResponse {
            candidates: vec![VertexCandidate {
                content: VertexResponseContent {
                    parts: vec![
                        VertexResponsePart {
                            text: Some("First thought. ".to_string()),
                            function_call: None,
                            thought: true,
                        },
                        VertexResponsePart {
                            text: Some("Second thought.".to_string()),
                            function_call: None,
                            thought: true,
                        },
                        VertexResponsePart {
                            text: Some("Final answer.".to_string()),
                            function_call: None,
                            thought: false,
                        },
                    ],
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: None,
        };

        let response = convert_vertex_to_responses_response(
            vertex_response,
            "gemini-2.0-flash-thinking",
            None,
            None,
        );

        // Check Reasoning has concatenated thinking
        match &response.output[0] {
            ResponsesOutputItem::Reasoning(reasoning) => {
                assert_eq!(reasoning.summary[0].text, "First thought. Second thought.");
            }
            _ => panic!("Expected Reasoning"),
        }

        assert_eq!(response.output_text, Some("Final answer.".to_string()));
    }
}
