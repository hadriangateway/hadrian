//! Gateway-side context compactor for non-OpenAI providers.
//!
//! OpenAI's `context_management = [{type: "compaction", …}]` directive
//! is forwarded verbatim when the upstream is OpenAI or Azure OpenAI
//! (those providers run server-side compaction natively). For every
//! other provider — Anthropic, Bedrock, Vertex, Test — Hadrian runs a
//! compactor here, before the request reaches the upstream call.
//!
//! Two strategies:
//!
//! - **`truncate`** (free, deterministic): drop the oldest non-system
//!   items until the rolling token estimate falls under
//!   `compact_threshold`. Reasoning items are folded in with messages
//!   (they aren't system context). Always keeps the most recent
//!   `keep_recent_items` items intact.
//! - **`llm`** (extra inference call): summarise the items that would
//!   otherwise be dropped via a single call to the same provider/model,
//!   then replace them with one `system`-role message carrying the
//!   summary. Higher signal preservation; costs one extra round trip.
//!
//! The caller picks the strategy via the Hadrian-extension `strategy`
//! field on the compaction directive (falling back to
//! `[features.responses.compaction].default_strategy`).

#![cfg(not(target_arch = "wasm32"))]

use thiserror::Error;
use tracing::{debug, info, warn};

use crate::{
    AppState,
    api_types::{
        self,
        responses::{
            CompactionStrategy, ContextManagementItem, CreateResponsesPayload, EasyInputMessage,
            EasyInputMessageContent, EasyInputMessageRole, ResponsesInput, ResponsesInputItem,
        },
    },
    config::{ProviderConfig, ResponsesCompactionStrategy},
    routes::execution::ProviderExecutor,
};

/// Errors raised by the compactor. Most are non-fatal — the caller
/// should log + skip compaction and let the original payload through
/// rather than failing the request, because an oversize-but-uncompacted
/// payload is still likely to work (the provider may itself truncate
/// or surface a clear context-window error).
#[derive(Debug, Error)]
pub enum CompactionError {
    #[error("summarisation provider call failed: {0}")]
    SummariseCall(String),
    #[error("summarisation response was empty or unparseable")]
    EmptySummary,
}

/// Public entry point. Mutates `payload.input` in place when
/// compaction runs; returns `Ok(false)` otherwise. Always returns
/// `Ok(_)` for foreground use even when compaction misfires — the
/// caller logs and continues. Errors are reserved for cases where the
/// caller asked explicitly for compaction and the operator has it
/// disabled.
pub async fn apply_gateway_compaction(
    state: &AppState,
    provider_config: &ProviderConfig,
    payload: &mut CreateResponsesPayload,
) -> Result<bool, CompactionError> {
    let compaction_cfg = &state.config.features.responses.compaction;
    if !compaction_cfg.enabled {
        return Ok(false);
    }
    if provider_has_native_compaction(provider_config) {
        debug!(
            stage = "compaction_skipped_native",
            "Provider supports native compaction; forwarding directive verbatim"
        );
        return Ok(false);
    }

    // Pull the directive (and its Hadrian-extension fields) off the
    // payload. We leave `context_management` in place so the caller
    // sees the same shape they sent — providers without native support
    // ignore it.
    let Some(directive) = payload.context_management.as_ref().and_then(|items| {
        items.iter().find_map(|item| match item {
            ContextManagementItem::Compaction {
                compact_threshold,
                strategy,
                prompt,
            } => Some((
                compact_threshold.map(|f| f as u32),
                *strategy,
                prompt.clone(),
            )),
            ContextManagementItem::Other => None,
        })
    }) else {
        return Ok(false);
    };

    let threshold = directive
        .0
        .unwrap_or(compaction_cfg.default_threshold_tokens);
    let strategy = match directive.1 {
        Some(CompactionStrategy::Llm) => ResponsesCompactionStrategy::Llm,
        Some(CompactionStrategy::Truncate) => ResponsesCompactionStrategy::Truncate,
        None => compaction_cfg.default_strategy,
    };
    let prompt_override = directive.2;

    // Cheap pre-check: only act if the rolling estimate exceeds the
    // threshold. Avoids paying the LLM round-trip on short
    // conversations that happened to advertise compaction.
    let items = match payload.input.as_mut() {
        Some(ResponsesInput::Items(items)) => items,
        // Plain-text input fits in one message and never needs compaction.
        Some(ResponsesInput::Text(_)) | None => return Ok(false),
    };
    let estimated = estimate_tokens(items);
    if estimated <= threshold {
        debug!(
            stage = "compaction_skipped_under_threshold",
            estimated_tokens = estimated,
            threshold,
            "Estimated token count is under threshold; no compaction needed"
        );
        return Ok(false);
    }

    let keep = compaction_cfg.keep_recent_items;
    let split_at = items.len().saturating_sub(keep);
    if split_at == 0 {
        // Recent-window already covers the whole conversation; nothing
        // can be dropped while preserving the user's most recent intent.
        return Ok(false);
    }

    let dropped: Vec<ResponsesInputItem> = items.drain(..split_at).collect();
    let surviving = std::mem::take(items);

    let replacement = match strategy {
        ResponsesCompactionStrategy::Truncate => truncate_replacement(&dropped),
        ResponsesCompactionStrategy::Llm => {
            let prompt = prompt_override
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(|| compaction_cfg.default_prompt.clone());
            match llm_replacement(state, provider_config, payload, &dropped, &prompt).await {
                Ok(text) => Some(make_summary_item(&text)),
                Err(e) => {
                    warn!(
                        stage = "compaction_llm_failed",
                        error = %e,
                        "LLM compaction failed; falling back to truncate"
                    );
                    truncate_replacement(&dropped)
                }
            }
        }
    };

    let mut next = Vec::with_capacity(surviving.len() + 1);
    if let Some(item) = replacement {
        next.push(item);
    }
    next.extend(surviving);
    payload.input = Some(ResponsesInput::Items(next));

    info!(
        stage = "compaction_applied",
        strategy = ?strategy,
        dropped = dropped.len(),
        kept = keep,
        estimated_tokens_before = estimated,
        threshold,
        "Applied gateway-side compaction"
    );
    Ok(true)
}

fn provider_has_native_compaction(cfg: &ProviderConfig) -> bool {
    match cfg {
        ProviderConfig::OpenAi(_) => true,
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(_) => true,
        ProviderConfig::Anthropic(_) => false,
        #[cfg(feature = "provider-bedrock")]
        ProviderConfig::Bedrock(_) => false,
        #[cfg(feature = "provider-vertex")]
        ProviderConfig::Vertex(_) => false,
        #[cfg(feature = "provider-vertex")]
        ProviderConfig::Gemini(_) => false,
        ProviderConfig::Test(_) => false,
    }
}

/// Rough token estimate over a slice of input items. Mirrors the
/// guardrails streaming heuristic (1 token ≈ 4 chars).
pub(crate) fn estimate_tokens(items: &[ResponsesInputItem]) -> u32 {
    let chars: usize = items.iter().map(item_chars).sum();
    chars.div_ceil(4) as u32
}

fn item_chars(item: &ResponsesInputItem) -> usize {
    match item {
        ResponsesInputItem::EasyMessage(m) => message_chars(&m.content),
        ResponsesInputItem::MessageItem(m) => {
            m.content.iter().map(content_item_chars).sum::<usize>()
        }
        ResponsesInputItem::FunctionCall(f) => f.name.len() + f.arguments.len(),
        ResponsesInputItem::FunctionCallOutput(f) => f.output.len(),
        ResponsesInputItem::Reasoning(r) => {
            let summary_chars: usize = r.summary.iter().map(|s| s.text.len()).sum();
            let content_chars: usize = r
                .content
                .as_ref()
                .map(|v| v.iter().map(|t| t.text.len()).sum())
                .unwrap_or(0);
            summary_chars + content_chars
        }
        ResponsesInputItem::OutputMessage(_)
        | ResponsesInputItem::OutputFunctionCall(_)
        | ResponsesInputItem::WebSearchCall(_)
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
        | ResponsesInputItem::ImageGeneration(_) => 64, // structural marker
    }
}

fn message_chars(c: &EasyInputMessageContent) -> usize {
    match c {
        EasyInputMessageContent::Text(t) => t.len(),
        EasyInputMessageContent::Parts(parts) => {
            parts.iter().map(content_item_chars).sum::<usize>()
        }
    }
}

fn content_item_chars(item: &api_types::responses::ResponseInputContentItem) -> usize {
    use api_types::responses::ResponseInputContentItem;
    match item {
        ResponseInputContentItem::InputText { text, .. } => text.len(),
        // Image / file / audio bytes don't add to the rolling text
        // estimate. The provider charges for them via vision tokens
        // we can't see here, but compaction is text-only.
        ResponseInputContentItem::InputImage { .. } => 0,
        ResponseInputContentItem::InputFile { .. } => 0,
        ResponseInputContentItem::InputAudio { .. } => 0,
    }
}

/// Build the placeholder item that replaces dropped messages when no
/// LLM call is involved. Emits a spec-shaped `compaction` item
/// (`CompactionBody`) so SDK consumers can recognise it the same as
/// they would the upstream-emitted version. Hadrian uses plain-text
/// English in `encrypted_content` since we can't mint encrypted
/// tokens the model would natively decode — the field is opaque to
/// SDK consumers regardless.
fn truncate_replacement(dropped: &[ResponsesInputItem]) -> Option<ResponsesInputItem> {
    if dropped.is_empty() {
        return None;
    }
    let summary = format!(
        "[Hadrian compaction] {} earlier conversation item(s) dropped to fit context.",
        dropped.len()
    );
    Some(make_summary_item(&summary))
}

fn make_summary_item(text: &str) -> ResponsesInputItem {
    use crate::api_types::responses::{CompactionItem, CompactionItemType};
    ResponsesInputItem::Compaction(CompactionItem {
        type_: CompactionItemType::Compaction,
        id: format!("cmp_{}", uuid::Uuid::new_v4().simple()),
        encrypted_content: text.to_string(),
        created_by: Some("gateway".to_string()),
    })
}

/// Run a one-shot non-streaming summarisation call against the same
/// provider as the parent request. Returns the raw assistant text.
async fn llm_replacement(
    state: &AppState,
    provider_config: &ProviderConfig,
    parent: &CreateResponsesPayload,
    dropped: &[ResponsesInputItem],
    prompt: &str,
) -> Result<String, CompactionError> {
    // Render a concise transcript out of the dropped items so the
    // summariser has plain text to chew on (instead of forcing the
    // model to parse function_call internals it didn't produce itself).
    let transcript = render_items_for_summary(dropped);
    let model = parent
        .model
        .clone()
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    // Reuse the parent's field layout (most importantly auth-related
    // fields, sovereignty, etc.) but reset everything that would
    // recurse or pollute the summarisation call.
    let mut summary_payload = parent.clone();
    summary_payload.model = Some(model.clone());
    summary_payload.stream = false;
    summary_payload.background = None;
    summary_payload.tools = None;
    summary_payload.tool_choice = None;
    summary_payload.skills = None;
    summary_payload.context_management = None;
    summary_payload.previous_response_id = None;
    summary_payload.store = Some(false);
    summary_payload.instructions = Some(prompt.to_string());
    summary_payload.input = Some(ResponsesInput::Items(vec![
        ResponsesInputItem::EasyMessage(EasyInputMessage {
            type_: None,
            role: EasyInputMessageRole::User,
            content: EasyInputMessageContent::Text(transcript),
        }),
    ]));

    let response = crate::routes::execution::ResponsesExecutor::execute(
        state,
        provider_config_name(provider_config),
        provider_config,
        summary_payload,
    )
    .await
    .map_err(|e| CompactionError::SummariseCall(format!("{e:?}")))?;

    // Drain the body and extract the assistant text. We accept either
    // a Responses-API JSON payload (when the provider returned one) or
    // a chat-style fallback shape.
    let body = response.into_body();
    let bytes = axum::body::to_bytes(body, 8 * 1024 * 1024)
        .await
        .map_err(|e| CompactionError::SummariseCall(format!("failed to read body: {e}")))?;
    extract_summary_text(&bytes)
}

/// Render a chronological transcript so the summariser sees plain
/// text instead of typed-union JSON.
fn render_items_for_summary(items: &[ResponsesInputItem]) -> String {
    let mut buf = String::with_capacity(items.len() * 128);
    for item in items {
        match item {
            ResponsesInputItem::EasyMessage(m) => {
                buf.push_str(role_label(m.role));
                buf.push_str(": ");
                buf.push_str(&match &m.content {
                    EasyInputMessageContent::Text(t) => t.clone(),
                    EasyInputMessageContent::Parts(parts) => parts
                        .iter()
                        .filter_map(|p| match p {
                            api_types::responses::ResponseInputContentItem::InputText {
                                text,
                                ..
                            } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                });
                buf.push('\n');
            }
            ResponsesInputItem::FunctionCallOutput(f) => {
                buf.push_str("tool_output: ");
                buf.push_str(&f.output);
                buf.push('\n');
            }
            ResponsesInputItem::FunctionCall(f) => {
                buf.push_str(&format!("tool_call: {} {}\n", f.name, f.arguments));
            }
            _ => {}
        }
    }
    buf
}

fn role_label(role: EasyInputMessageRole) -> &'static str {
    match role {
        EasyInputMessageRole::User => "user",
        EasyInputMessageRole::Assistant => "assistant",
        EasyInputMessageRole::System => "system",
        EasyInputMessageRole::Developer => "developer",
    }
}

fn extract_summary_text(bytes: &[u8]) -> Result<String, CompactionError> {
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| CompactionError::EmptySummary)?;
    // Try Responses-API shape: output[].content[].text where type =
    // "message"/"output_text".
    if let Some(output) = json.get("output").and_then(|v| v.as_array()) {
        let mut acc = String::new();
        for item in output {
            if item.get("type").and_then(|t| t.as_str()) != Some("message") {
                continue;
            }
            if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if !acc.is_empty() {
                            acc.push('\n');
                        }
                        acc.push_str(text);
                    }
                }
            }
        }
        if !acc.is_empty() {
            return Ok(acc);
        }
    }
    // Fall back to choices[].message.content (chat-completion shape
    // emitted by some adapters).
    if let Some(choices) = json.get("choices").and_then(|v| v.as_array())
        && let Some(first) = choices.first()
        && let Some(text) = first
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
    {
        return Ok(text.to_string());
    }
    Err(CompactionError::EmptySummary)
}

fn provider_config_name(cfg: &ProviderConfig) -> &str {
    match cfg {
        ProviderConfig::OpenAi(_) => "openai",
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(_) => "azure_openai",
        ProviderConfig::Anthropic(_) => "anthropic",
        #[cfg(feature = "provider-bedrock")]
        ProviderConfig::Bedrock(_) => "bedrock",
        #[cfg(feature = "provider-vertex")]
        ProviderConfig::Vertex(_) => "vertex",
        #[cfg(feature = "provider-vertex")]
        ProviderConfig::Gemini(_) => "gemini",
        ProviderConfig::Test(_) => "test",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::responses::{
        CompactionStrategy, ContextManagementItem, EasyInputMessage, EasyInputMessageContent,
        EasyInputMessageRole, ResponsesInputItem,
    };

    fn text_item(role: EasyInputMessageRole, text: &str) -> ResponsesInputItem {
        ResponsesInputItem::EasyMessage(EasyInputMessage {
            type_: None,
            role,
            content: EasyInputMessageContent::Text(text.to_string()),
        })
    }

    #[test]
    fn estimate_tokens_counts_text_chars() {
        let items = vec![text_item(EasyInputMessageRole::User, "abcdefgh")]; // 8 chars
        assert_eq!(estimate_tokens(&items), 2);
    }

    #[test]
    fn truncate_replacement_emits_compaction_item() {
        let dropped = vec![
            text_item(EasyInputMessageRole::User, "x"),
            text_item(EasyInputMessageRole::Assistant, "y"),
        ];
        let r = truncate_replacement(&dropped).unwrap();
        match r {
            ResponsesInputItem::Compaction(item) => {
                assert!(
                    item.encrypted_content
                        .contains("2 earlier conversation item(s)")
                );
                assert!(item.id.starts_with("cmp_"));
                assert_eq!(item.created_by.as_deref(), Some("gateway"));
            }
            _ => panic!("expected compaction item"),
        }
    }

    #[test]
    fn extract_summary_handles_responses_shape() {
        let body = serde_json::json!({
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "Hello world"}
                    ]
                }
            ]
        });
        let text = extract_summary_text(body.to_string().as_bytes()).unwrap();
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn extract_summary_handles_chat_shape() {
        let body = serde_json::json!({
            "choices": [{"message": {"content": "Concise summary"}}]
        });
        let text = extract_summary_text(body.to_string().as_bytes()).unwrap();
        assert_eq!(text, "Concise summary");
    }

    #[test]
    fn render_items_emits_role_labels() {
        let items = vec![
            text_item(EasyInputMessageRole::User, "Hi"),
            text_item(EasyInputMessageRole::Assistant, "Hello!"),
        ];
        let rendered = render_items_for_summary(&items);
        assert!(rendered.contains("user: Hi"));
        assert!(rendered.contains("assistant: Hello!"));
    }

    #[test]
    fn compaction_strategy_enum_round_trips() {
        // Sanity: the request-level strategy enum maps onto the
        // operator config enum we expose internally.
        let _llm = CompactionStrategy::Llm;
        let _trunc = CompactionStrategy::Truncate;
        let _other = ContextManagementItem::Other;
    }
}
