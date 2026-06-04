//! Server-side conversation reconstruction for `previous_response_id`.
//!
//! Hadrian is the system of record for stored responses: it persists each
//! turn under its own `resp_…` id and assigns response ids the client chains
//! against. Upstream providers cannot be relied on to resolve
//! `previous_response_id` — Anthropic/Bedrock/Vertex have no such concept, and
//! OpenAI-compatible passthroughs (e.g. OpenRouter) are stateless. So when a
//! request carries `previous_response_id`, the gateway walks the stored chain,
//! rebuilds the full prior transcript (each turn's input items + output items),
//! and prepends it to the new turn's input before dispatch. The
//! `previous_response_id` field is then stripped so nothing is forwarded
//! upstream.
//!
//! Each stored row keeps only *its own* turn's input (see the persisted
//! snapshot in `routes/api/chat.rs`), so reconstruction walks the
//! `previous_response_id` links back to the root rather than reading a single
//! pre-expanded transcript — no double-counting, and `GET` still reflects what
//! the caller actually sent.

use uuid::Uuid;

use crate::{
    api_types::responses::{
        EasyInputMessage, EasyInputMessageContent, EasyInputMessageRole, ResponsesInput,
        ResponsesInputItem, ResponsesOutputItem,
    },
    db::repos::ResponseRecord,
    services::{ResponsesStore, ResponsesStoreError},
};

/// Upper bound on how many turns a single chain may reference. Guards against
/// pathological depth and any accidental cycle in stored `previous_response_id`
/// links (which should be a DAG, but we never want an unbounded loop).
const MAX_CHAIN_DEPTH: usize = 200;

/// Why reconstruction failed. Maps to a client-facing 404/400 in the handler.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    /// A `previous_response_id` in the chain doesn't resolve to a stored row in
    /// this org. Surfaced to the caller — OpenAI returns 404 for an unknown
    /// `previous_response_id`, and chaining against a missing turn is a bug we
    /// must not paper over by silently dropping history.
    #[error("previous response '{0}' not found")]
    NotFound(String),
    /// The chain is longer than [`MAX_CHAIN_DEPTH`].
    #[error("conversation chain exceeds maximum depth of {MAX_CHAIN_DEPTH}")]
    TooDeep,
    /// Store/DB failure while walking the chain.
    #[error("failed to load previous response: {0}")]
    Store(ResponsesStoreError),
    /// A stored row in the chain has a present `input`/`output` field that no
    /// longer deserializes (data corruption or schema drift — e.g. an old
    /// binary reading a row with a newer output-item type). We refuse to
    /// silently drop the turn: dropping it would feed the model a transcript
    /// with unanswered messages, which it tends to repeat or hallucinate
    /// around. Surfaced loudly so operators can catch the integrity/schema
    /// issue instead of shipping invisible model-visible corruption.
    #[error("stored response '{id}' has an unreadable `{field}` field")]
    CorruptRecord { id: String, field: &'static str },
}

/// Convert a stored response's `input` snapshot into input items. Bare-string
/// inputs become a single user message, mirroring how the rest of the pipeline
/// normalizes `ResponsesInput::Text`.
fn input_to_items(input: ResponsesInput) -> Vec<ResponsesInputItem> {
    match input {
        ResponsesInput::Text(text) => vec![ResponsesInputItem::EasyMessage(EasyInputMessage {
            type_: None,
            role: EasyInputMessageRole::User,
            content: EasyInputMessageContent::Text(text),
        })],
        ResponsesInput::Items(items) => items,
    }
}

/// Map a prior turn's output item to the equivalent input item so it can be
/// replayed as conversation history. The inner payloads are identical between
/// the two enums, so this is a total, lossless 1:1 mapping.
///
/// Note the hosted server-tool items (`ShellCall` / `ShellCallOutput`,
/// `WebSearchCall`, `FileSearchCall`, `McpCall`, …) are replayed verbatim here —
/// the per-provider preprocess layer in `routes/execution.rs` is what normalizes
/// them before dispatch, since that's the mode-aware layer that knows whether
/// each tool stayed native or was rewritten to a function. `preprocess_shell_tools`
/// (`services/shell_tool.rs`) rewrites the two-item shell history in place, while
/// `web_search`, `file_search`, and MCP share
/// `server_tool_history::rewrite_hosted_calls_to_function_pairs` to expand their
/// single hosted item into a `function_call` / `function_call_output` pair there.
fn output_item_to_input(item: ResponsesOutputItem) -> ResponsesInputItem {
    match item {
        ResponsesOutputItem::Message(m) => ResponsesInputItem::OutputMessage(m),
        ResponsesOutputItem::Reasoning(r) => ResponsesInputItem::Reasoning(r),
        ResponsesOutputItem::FunctionCall(f) => ResponsesInputItem::OutputFunctionCall(f),
        ResponsesOutputItem::WebSearchCall(w) => ResponsesInputItem::WebSearchCall(w),
        ResponsesOutputItem::FileSearchCall(f) => ResponsesInputItem::FileSearchCall(f),
        ResponsesOutputItem::ShellCall(s) => ResponsesInputItem::ShellCall(s),
        ResponsesOutputItem::ShellCallOutput(s) => ResponsesInputItem::ShellCallOutput(s),
        ResponsesOutputItem::McpListTools(m) => ResponsesInputItem::McpListTools(m),
        ResponsesOutputItem::McpCall(m) => ResponsesInputItem::McpCall(m),
        ResponsesOutputItem::McpApprovalRequest(m) => ResponsesInputItem::McpApprovalRequest(m),
        ResponsesOutputItem::ToolSearchCall(t) => ResponsesInputItem::ToolSearchCall(t),
        ResponsesOutputItem::ToolSearchOutput(t) => ResponsesInputItem::ToolSearchOutput(t),
        ResponsesOutputItem::Compaction(c) => ResponsesInputItem::Compaction(c),
        ResponsesOutputItem::ImageGeneration(i) => ResponsesInputItem::ImageGeneration(i),
    }
}

/// Pull the `previous_response_id` link out of a stored request payload, if any.
fn parent_of(record: &ResponseRecord) -> Option<String> {
    record
        .request_payload
        .get("previous_response_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// Expand one stored turn into its input items: the caller's input for that
/// turn followed by the assistant's output items.
///
/// An absent `input`/`output` (`None`) is legitimate (e.g. a failed turn has no
/// output) and skipped. A field that is *present* but fails to deserialize is a
/// hard error — see [`ChainError::CorruptRecord`] — rather than a silent drop.
fn record_to_items(
    record: &ResponseRecord,
    out: &mut Vec<ResponsesInputItem>,
) -> Result<(), ChainError> {
    if let Some(input) = record.request_payload.get("input").cloned() {
        let parsed = serde_json::from_value::<ResponsesInput>(input).map_err(|e| {
            tracing::error!(
                response_id = %record.id,
                error = %e,
                "stored response has an unreadable `input` field; refusing to reconstruct a corrupt transcript"
            );
            ChainError::CorruptRecord {
                id: record.id.clone(),
                field: "input",
            }
        })?;
        out.extend(input_to_items(parsed));
    }
    if let Some(output) = record.output.clone() {
        let items = serde_json::from_value::<Vec<ResponsesOutputItem>>(output).map_err(|e| {
            tracing::error!(
                response_id = %record.id,
                error = %e,
                "stored response has an unreadable `output` field; refusing to reconstruct a corrupt transcript"
            );
            ChainError::CorruptRecord {
                id: record.id.clone(),
                field: "output",
            }
        })?;
        out.extend(items.into_iter().map(output_item_to_input));
    }
    Ok(())
}

/// Rebuild the full input for a new turn that chains off `previous_response_id`.
///
/// Walks the stored chain back to its root, replays every prior turn's input +
/// output in chronological order, then appends `current_input` (this turn's
/// new input). Returns the flattened item list to assign to `payload.input`.
///
/// Errors if any link in the chain is missing ([`ChainError::NotFound`]) or the
/// chain is too deep ([`ChainError::TooDeep`]).
pub async fn reconstruct_input(
    store: &ResponsesStore,
    org_id: Uuid,
    previous_response_id: &str,
    current_input: Option<ResponsesInput>,
) -> Result<Vec<ResponsesInputItem>, ChainError> {
    // Walk newest → oldest, following `previous_response_id` links.
    let mut chain: Vec<ResponseRecord> = Vec::new();
    let mut cursor = Some(previous_response_id.to_owned());
    while let Some(id) = cursor {
        if chain.len() >= MAX_CHAIN_DEPTH {
            return Err(ChainError::TooDeep);
        }
        let record = store.get(&id, org_id).await.map_err(|e| match e {
            ResponsesStoreError::NotFound => ChainError::NotFound(id.clone()),
            other => ChainError::Store(other),
        })?;
        cursor = parent_of(&record);
        chain.push(record);
    }

    // Replay oldest → newest.
    let mut items = Vec::new();
    for record in chain.iter().rev() {
        record_to_items(record, &mut items)?;
    }
    if let Some(current) = current_input {
        items.extend(input_to_items(current));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(
        id: &str,
        prev: Option<&str>,
        input: serde_json::Value,
        output: serde_json::Value,
    ) -> ResponseRecord {
        let mut payload = json!({ "input": input });
        if let Some(p) = prev {
            payload["previous_response_id"] = json!(p);
        }
        ResponseRecord {
            id: id.to_string(),
            org_id: Uuid::nil(),
            owner_type: crate::db::repos::ResponseOwnerType::Organization,
            owner_id: Uuid::nil(),
            project_id: None,
            user_id: None,
            api_key_id: None,
            service_account_id: None,
            status: crate::db::repos::ResponseStatus::Completed,
            background: false,
            model: "m".into(),
            provider: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            request_payload: payload,
            output: Some(output),
            usage: None,
            error: None,
            retention_expires_at: chrono::Utc::now(),
            last_sequence_number: 0,
            last_heartbeat_at: None,
            container_id: None,
        }
    }

    fn assistant_output(text: &str) -> serde_json::Value {
        json!([{
            "id": "msg_x",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}]
        }])
    }

    #[test]
    fn input_text_becomes_user_message() {
        let items = input_to_items(ResponsesInput::Text("Hi?".into()));
        assert_eq!(items.len(), 1);
        let v = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "Hi?");
    }

    #[test]
    fn record_expands_to_input_then_output() {
        let r = record("resp_1", None, json!("Hi?"), assistant_output("Hello!"));
        let mut items = Vec::new();
        record_to_items(&r, &mut items).expect("valid record");
        // user "Hi?" then assistant "Hello!"
        assert_eq!(items.len(), 2);
        let user = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(user["role"], "user");
        let asst = serde_json::to_value(&items[1]).unwrap();
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"][0]["text"], "Hello!");
    }

    #[test]
    fn unreadable_output_is_a_hard_error_not_a_silent_drop() {
        // A present-but-unparseable `output` (here: not even an array) must
        // error rather than silently dropping the assistant turn.
        let r = record("resp_bad", None, json!("Hi?"), json!("not-an-array"));
        let mut items = Vec::new();
        let err = record_to_items(&r, &mut items).expect_err("should reject corrupt output");
        assert!(matches!(
            err,
            ChainError::CorruptRecord {
                field: "output",
                ..
            }
        ));
    }

    #[test]
    fn web_search_call_output_replays_with_action_and_content() {
        // A stored `web_search_call` output item must round-trip through
        // reconstruction as a `WebSearchCall` *input* item carrying its
        // `action` (query + sources) and the Hadrian `replay_content`, so the
        // per-provider preprocess can rebuild the function-call pair. Guards the
        // untagged-enum deserialization against the added optional fields.
        let output = json!([{
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {
                "type": "search",
                "query": "rust 2024",
                "sources": [{"type": "url", "url": "https://example.com"}]
            },
            "replay_content": "Web search results for \"rust 2024\""
        }]);
        let r = record("resp_1", None, json!("hi"), output);
        let mut items = Vec::new();
        record_to_items(&r, &mut items).expect("valid record");
        // user "hi" then the web_search_call
        assert_eq!(items.len(), 2);
        assert!(
            matches!(items[1], ResponsesInputItem::WebSearchCall(_)),
            "must deserialize as the WebSearchCall variant"
        );
        let ws = serde_json::to_value(&items[1]).unwrap();
        assert_eq!(ws["type"], "web_search_call");
        assert_eq!(ws["action"]["query"], "rust 2024");
        assert_eq!(ws["action"]["sources"][0]["url"], "https://example.com");
        assert_eq!(ws["replay_content"], "Web search results for \"rust 2024\"");
    }

    #[test]
    fn parent_link_is_followed() {
        let root = record("resp_1", None, json!("Hi?"), assistant_output("Hello!"));
        assert_eq!(parent_of(&root), None);
        let child = record(
            "resp_2",
            Some("resp_1"),
            json!("More?"),
            assistant_output("Sure."),
        );
        assert_eq!(parent_of(&child).as_deref(), Some("resp_1"));
    }
}
