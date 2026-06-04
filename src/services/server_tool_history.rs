//! Shared history-rewriting for server-executed tools.
//!
//! Hadrian self-executes its server tools by rewriting each to a function tool
//! (`web_search` / `file_search` / `mcp_<label>__<tool>`), so the upstream
//! provider never produces the corresponding native hosted item. The
//! spec-shaped hosted items Hadrian synthesizes for the client
//! (`web_search_call`, `file_search_call`, `mcp_call`) are persisted in the
//! stored response, so on a follow-up turn they come back as input — rebuilt
//! from a `previous_response_id` chain (`services/responses_chain.rs`) or
//! resent by a client doing manual multi-turn.
//!
//! Forwarded verbatim, no upstream accepts them: OpenAI-compatible providers
//! (e.g. OpenRouter) reject the turn with `invalid_prompt`, and
//! Anthropic/Bedrock/Vertex silently drop them. Rewriting each hosted item back
//! into the `function_call` + `function_call_output` pair every provider
//! understands — with the issued query as the call arguments and the retained
//! result text as the output — keeps multi-turn coherent behind any provider
//! and lets the model draw on the earlier results.
//!
//! All three 1→2 rewrites share this driver; each tool supplies only the
//! per-item conversion (`web_search_tool::rewrite_web_search_history`,
//! `file_search_tool::rewrite_file_search_history`,
//! `mcp::preprocess::rewrite_mcp_history`). The shell tool is the one server
//! tool that does *not* use this: its history is already two items
//! (`shell_call` plus `shell_call_output`), so it rewrites them 1:1 in place
//! (`shell_tool::rewrite_shell_history_to_function_calls`) and carries its own
//! output-ordering fixups.

use crate::api_types::responses::{
    CreateResponsesPayload, FunctionCallOutput, FunctionToolCall, ResponsesInput,
    ResponsesInputItem,
};

/// Expand every hosted server-tool item in `payload.input` into the
/// `function_call` + `function_call_output` pair every provider understands.
///
/// `expand` inspects each item and returns `Some((call, output))` for the ones
/// its tool owns, or `None` to leave the item untouched. The call and its output
/// share a `call_id` so the per-provider conversion pairs them. A no-op when the
/// input isn't an item list.
pub fn rewrite_hosted_calls_to_function_pairs(
    payload: &mut CreateResponsesPayload,
    expand: impl Fn(&ResponsesInputItem) -> Option<(FunctionToolCall, FunctionCallOutput)>,
) {
    let Some(ResponsesInput::Items(items)) = payload.input.as_mut() else {
        return;
    };
    // The common case — a turn with no prior hosted-tool calls — must allocate
    // nothing: scan for the first item that expands and bail if there is none.
    let Some(first) = items.iter().position(|item| expand(item).is_some()) else {
        return;
    };
    // Move the items out, pass the untouched `[0, first)` prefix through, and
    // expand from `first` on — calling `expand` at most once more per item.
    // Reserve for the worst case (every item from `first` expands into two) so
    // the output never reallocates mid-rewrite.
    let mut original = std::mem::take(items).into_iter();
    let mut rewritten = Vec::with_capacity(first + (original.len() - first) * 2);
    rewritten.extend(original.by_ref().take(first));
    for item in original {
        match expand(&item) {
            Some((call, output)) => {
                rewritten.push(ResponsesInputItem::FunctionCall(call));
                rewritten.push(ResponsesInputItem::FunctionCallOutput(output));
            }
            None => rewritten.push(item),
        }
    }
    *items = rewritten;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::responses::{FunctionCallOutputType, FunctionToolCallType};

    /// Treat every `FunctionCallOutput` input item as a hosted call to expand,
    /// pairing it with a fresh `function_call` keyed by the same `call_id`. This
    /// lets the test drive the generic driver without a real hosted-tool type.
    fn expand_outputs(item: &ResponsesInputItem) -> Option<(FunctionToolCall, FunctionCallOutput)> {
        let ResponsesInputItem::FunctionCallOutput(out) = item else {
            return None;
        };
        let call = FunctionToolCall {
            type_: FunctionToolCallType::FunctionCall,
            id: out.call_id.clone(),
            call_id: out.call_id.clone(),
            name: "t".to_string(),
            arguments: "{}".to_string(),
            status: None,
        };
        Some((call, out.clone()))
    }

    fn output_item(call_id: &str) -> ResponsesInputItem {
        ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
            type_: FunctionCallOutputType::FunctionCallOutput,
            id: None,
            call_id: call_id.to_string(),
            output: String::new(),
            status: None,
        })
    }

    fn user_msg(text: &str) -> ResponsesInputItem {
        serde_json::from_value(serde_json::json!({"role": "user", "content": text})).unwrap()
    }

    fn payload_with(items: Vec<ResponsesInputItem>) -> CreateResponsesPayload {
        let mut payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({"stream": false})).unwrap();
        payload.input = Some(ResponsesInput::Items(items));
        payload
    }

    #[test]
    fn no_matching_items_leaves_input_untouched() {
        let mut payload = payload_with(vec![user_msg("a"), user_msg("b")]);
        rewrite_hosted_calls_to_function_pairs(&mut payload, expand_outputs);
        let Some(ResponsesInput::Items(items)) = payload.input else {
            panic!("expected items");
        };
        assert_eq!(items.len(), 2, "no expansions should leave the list as-is");
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, ResponsesInputItem::FunctionCall(_))),
            "nothing should have been rewritten"
        );
    }

    #[test]
    fn multiple_matches_expand_each_to_a_pair() {
        let mut payload = payload_with(vec![
            user_msg("a"),
            output_item("c1"),
            user_msg("b"),
            output_item("c2"),
        ]);
        rewrite_hosted_calls_to_function_pairs(&mut payload, expand_outputs);
        let Some(ResponsesInput::Items(items)) = payload.input else {
            panic!("expected items");
        };
        // 2 user messages + 2 expansions × (call + output) = 6.
        assert_eq!(items.len(), 6);
        let fc_count = items
            .iter()
            .filter(|i| matches!(i, ResponsesInputItem::FunctionCall(_)))
            .count();
        assert_eq!(fc_count, 2, "each match yields one function_call");
    }
}
