//! Function-tool rewrite for `hadrian_hosted` mode.
//!
//! When the operator picks `[features.mcp].mode = "hadrian_hosted"`,
//! Hadrian runs the MCP client loop itself. For *every* provider —
//! OpenAI, Azure, Anthropic, Bedrock, Vertex, Test — we rewrite the
//! request's `mcp` tool entries into N function tools, one per tool
//! the remote MCP server advertises via `tools/list`. The model sees
//! a flat list of function tools named
//! `mcp_<server_label>__<tool_name>`; the
//! [`McpExecutor`](super::executor) intercepts those calls, looks up
//! the right pooled client, and runs the actual `tools/call`.
//!
//! Uniform rewriting (even for OpenAI) is intentional: it keeps the
//! interception logic single-path and gives Hadrian-side policy
//! enforcement / audit on every call, regardless of upstream. The
//! tradeoff is OpenAI's first-party MCP optimizations aren't used in
//! `hadrian_hosted` — operators wanting those should pick
//! `passthrough_openai` instead.
//!
//! Failures here surface as HTTP 502 (bad gateway) when the upstream
//! MCP server is unreachable, or HTTP 400 (bad request) for caller-
//! side mistakes — duplicate `server_label`, ambiguous `tool_choice`,
//! `connector_id` under `hadrian_hosted`, etc. See
//! [`McpRewriteError::is_client_error`].

use std::sync::Arc;

use super::{McpService, McpToolMeta};
use crate::{
    api_types::responses::{
        CreateResponsesPayload, FunctionCallOutput, FunctionCallOutputType, FunctionToolCall,
        FunctionToolCallType, McpAllowedTools, McpCallItem, McpTool, McpToolFilter, ResponsesInput,
        ResponsesInputItem, ResponsesMcpToolChoice, ResponsesNamedToolChoice,
        ResponsesNamedToolChoiceType, ResponsesToolChoice, ResponsesToolDefinition,
    },
    services::{mcp::tool_search::TOOL_SEARCH_FUNCTION_NAME, mcp_tool::McpProviderKind},
};

/// Failures surfaced by [`rewrite_mcp_tools`]. `ListToolsFailed` maps
/// to HTTP 502 (the upstream MCP server is unreachable). The other
/// variants are caller errors and map to HTTP 400.
#[derive(Debug, thiserror::Error)]
pub enum McpRewriteError {
    #[error("MCP tools/list failed for server '{server_label}' ({server_url}): {message}")]
    ListToolsFailed {
        server_label: String,
        server_url: String,
        message: String,
    },
    #[error(
        "MCP request has conflicting server_label '{0}': it duplicates, or normalizes to the \
         same function-name prefix as, another `mcp` tool entry. server_labels must be unique \
         once reduced to `[A-Za-z0-9_]` (e.g. 'My-Co' and 'My_Co' collide); use distinct labels"
    )]
    DuplicateServerLabel(String),
    #[error(
        "MCP tool with server_label '{0}' has no server_url (connector_id requires passthrough mode)"
    )]
    MissingServerUrl(String),
    #[error(
        "MCP `tool_choice` for server_label '{server_label}' is ambiguous: \
         no `name` was supplied and {match_count} matching tool(s) survived the rewrite. \
         Under `hadrian_hosted` the upstream function tool_choice grammar has no \
         per-server constraint, so the `name` field is required when more than one tool \
         from the server is exposed (and at least one must be exposed). \
         Specify `tool_choice.name` or narrow `allowed_tools` until exactly one tool remains."
    )]
    AmbiguousToolChoice {
        server_label: String,
        match_count: usize,
    },
    #[error(
        "MCP tool with server_label '{0}' set `defer_loading_passthrough` but the resolved \
         provider does not support native tool search; only OpenAI / Azure OpenAI do. Drop \
         the flag to use Hadrian-side tool search, which works behind any provider"
    )]
    DeferLoadingPassthroughUnsupported(String),
    #[error(
        "MCP server '{server_label}' advertised tool '{tool}' with a definition Hadrian \
         cannot turn into a function tool: {message}"
    )]
    InvalidToolDefinition {
        server_label: String,
        tool: String,
        message: String,
    },
}

impl McpRewriteError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ListToolsFailed { .. } => "mcp_list_tools_failed",
            Self::DuplicateServerLabel(_) => "mcp_duplicate_server_label",
            Self::MissingServerUrl(_) => "mcp_missing_server_url",
            Self::AmbiguousToolChoice { .. } => "mcp_ambiguous_tool_choice",
            Self::DeferLoadingPassthroughUnsupported(_) => {
                "mcp_defer_loading_passthrough_unsupported"
            }
            Self::InvalidToolDefinition { .. } => "mcp_invalid_tool_definition",
        }
    }

    /// True when this error is a caller-side problem (HTTP 400) rather
    /// than an upstream / gateway failure (HTTP 502). Used by the route
    /// handler to pick the right status code.
    pub fn is_client_error(&self) -> bool {
        !matches!(
            self,
            Self::ListToolsFailed { .. } | Self::InvalidToolDefinition { .. }
        )
    }
}

/// Replace every `mcp` tool entry in `payload.tools` with function
/// tools the model can call. Mutates in place.
///
/// Each MCP server is handled one of three ways:
///
/// - **eager** (no `defer_loading`): one function tool per allowed tool,
///   exposed up front — the original behaviour.
/// - **deferred, Hadrian-side** (`defer_loading: true`, the default):
///   the server's per-tool functions are *not* exposed; instead a single
///   `tool_search` function tool is injected. [`ToolSearchExecutor`] runs
///   the search locally and injects matched tools on demand. Works behind
///   any provider.
/// - **deferred, native passthrough** (`defer_loading: true` +
///   `defer_loading_passthrough: true`, OpenAI/Azure only): per-tool
///   functions are exposed with `defer_loading` set, leaving the upstream
///   to run its own tool search.
///
/// A server pinned by `tool_choice` is always treated as eager (forcing a
/// tool implies it must be callable now, so deferral is moot).
///
/// No-op when the payload has no `mcp` tool entries.
pub async fn rewrite_mcp_tools(
    payload: &mut CreateResponsesPayload,
    mcp_service: &McpService,
    provider: McpProviderKind,
) -> Result<(), McpRewriteError> {
    // Spec: "as long as the `mcp_list_tools` item is present in the
    // context of an API request, the API will not fetch a list of
    // tools from the MCP server again." Build an index of any
    // caller-provided catalogs BEFORE acquiring the mutable borrow on
    // `payload.tools` below, so we can skip the upstream `tools/list`
    // round-trip for matching server_labels.
    let inlined_catalogs = collect_inlined_catalogs(payload);
    // Which server (if any) `tool_choice` pins — read before the mutable
    // borrow below. A pinned server is forced eager.
    let forced_label = mcp_choice_label(payload);

    // Translate any prior-turn `mcp_call` items the caller echoed back
    // into the `function_call` + `function_call_output` shape every
    // provider understands. Must run after `collect_inlined_catalogs`
    // (which reads `mcp_list_tools` items from the same input) and
    // before the tool-presence early-returns below, so multi-turn MCP
    // history survives even on a turn that carries no `mcp` tool entry.
    rewrite_mcp_history(payload);

    let Some(tools) = payload.tools.as_mut() else {
        return Ok(());
    };
    if !tools.iter().any(|t| t.is_mcp()) {
        return Ok(());
    }

    // First pass: duplicate detection on the *sanitized* label. Two
    // distinct raw labels that reduce to the same function-name prefix
    // (e.g. `"My-Co"` and `"My_Co"` → `"My_Co"`) would otherwise route
    // calls to whichever binding `resolve_binding` found first. Keying on
    // the sanitized form rejects both exact duplicates and collisions.
    {
        let mut seen = std::collections::HashSet::new();
        for t in tools.iter() {
            if let Some(mcp) = t.as_mcp()
                && !seen.insert(sanitize_label(&mcp.server_label, MAX_LABEL_LEN))
            {
                return Err(McpRewriteError::DuplicateServerLabel(
                    mcp.server_label.clone(),
                ));
            }
        }
    }

    // Second pass: rewrite. Walk + build a fresh Vec rather than
    // splice-in-place to keep ordering deterministic.
    let mut rewritten: Vec<ResponsesToolDefinition> = Vec::with_capacity(tools.len());
    // Servers exposed via Hadrian-side tool search (one shared
    // `tool_search` tool is injected for all of them at the end). Each
    // entry keeps the optional `server_description` so the model can use
    // it to decide when to search a given server.
    let mut deferred_servers: Vec<(String, Option<String>)> = Vec::new();
    for tool in tools.drain(..) {
        match tool {
            ResponsesToolDefinition::Mcp(mcp) => {
                let server_url = mcp
                    .server_url
                    .as_deref()
                    .ok_or_else(|| McpRewriteError::MissingServerUrl(mcp.server_label.clone()))?;

                let wants_defer = mcp.defer_loading == Some(true);
                let wants_passthrough = mcp.defer_loading_passthrough == Some(true);
                // The native-passthrough opt-in only works on an upstream
                // that actually implements tool search. Fail loud rather
                // than silently ignoring the caller's request.
                if wants_defer && wants_passthrough && !provider.supports_passthrough_mcp() {
                    return Err(McpRewriteError::DeferLoadingPassthroughUnsupported(
                        mcp.server_label.clone(),
                    ));
                }
                let forced = forced_label.as_deref() == Some(mcp.server_label.as_str());
                // Hadrian-side deferral: wants deferral, isn't opting into
                // native passthrough, and isn't pinned by tool_choice.
                let hadrian_deferred = wants_defer && !wants_passthrough && !forced;

                let headers = mcp.headers.clone().unwrap_or_default();
                // Always fetch + prime the catalog: the executor's
                // `tool_search` and `mcp_list_tools` reads rely on the
                // primed cache, and eager rewriting needs it directly.
                let catalog = match inlined_catalogs.get(&mcp.server_label) {
                    Some(inlined) => {
                        // Caller already has the catalog in context —
                        // skip the network round-trip. Prime the service
                        // cache so downstream reads (`cached_tools`,
                        // `read_only_hint_for`) see the same catalog.
                        let metas: Vec<McpToolMeta> =
                            inlined.iter().map(meta_from_listed_tool).collect();
                        mcp_service.prime_tools_cache(
                            server_url,
                            mcp.authorization.as_deref(),
                            &headers,
                            metas.clone(),
                        );
                        Arc::new(metas)
                    }
                    None => mcp_service
                        .list_tools(server_url, mcp.authorization.as_deref(), &headers)
                        .await
                        .map_err(|e| McpRewriteError::ListToolsFailed {
                            server_label: mcp.server_label.clone(),
                            server_url: server_url.to_string(),
                            message: e.to_string(),
                        })?,
                };

                if hadrian_deferred {
                    // Keep the catalog out of the prompt; expose it via
                    // the shared `tool_search` tool injected below.
                    tracing::debug!(
                        server_label = %mcp.server_label,
                        "MCP `defer_loading`: deferring catalog via Hadrian-side tool search"
                    );
                    deferred_servers
                        .push((mcp.server_label.clone(), mcp.server_description.clone()));
                    continue;
                }

                // Eager or native-passthrough: expose per-tool functions.
                // `defer_loading` is set on them only for the passthrough
                // path, where the upstream runs its own tool search.
                let defer_flag = wants_defer && wants_passthrough;
                for meta in catalog.iter() {
                    if !is_allowed(meta, mcp.allowed_tools.as_ref()) {
                        continue;
                    }
                    if !is_valid_tool_name(&meta.name) {
                        tracing::warn!(
                            tool = %meta.name,
                            server_label = %mcp.server_label,
                            "Skipping MCP tool with non-ASCII or invalid name (must match \
                             [A-Za-z0-9_-]{{1,48}})"
                        );
                        continue;
                    }
                    rewritten.push(build_function_tool(&mcp, meta, defer_flag)?);
                }
            }
            other => rewritten.push(other),
        }
    }

    // One shared `tool_search` tool covers every Hadrian-deferred server.
    if !deferred_servers.is_empty() {
        rewritten.push(build_tool_search_function(&deferred_servers));
    }

    *tools = rewritten;

    rewrite_tool_choice(payload)?;

    Ok(())
}

/// The `server_label` an `mcp` `tool_choice` pins, if any. Read before
/// the rewrite so a forced server can be exposed eagerly.
fn mcp_choice_label(payload: &CreateResponsesPayload) -> Option<String> {
    match payload.tool_choice.as_ref()? {
        ResponsesToolChoice::Mcp(c) => Some(c.server_label.clone()),
        _ => None,
    }
}

/// Build the single `tool_search` function tool the model uses to
/// discover deferred tools. Lists the searchable servers — with their
/// `server_description` when supplied — in the description so the model
/// can decide which server to search and when.
fn build_tool_search_function(
    deferred_servers: &[(String, Option<String>)],
) -> ResponsesToolDefinition {
    // `label` or `label (description)` per server, comma-joined.
    let servers = deferred_servers
        .iter()
        .map(
            |(label, desc)| match desc.as_deref().filter(|s| !s.is_empty()) {
                Some(d) => format!("{label} ({d})"),
                None => label.clone(),
            },
        )
        .collect::<Vec<_>>()
        .join(", ");
    let labels: Vec<&str> = deferred_servers.iter().map(|(l, _)| l.as_str()).collect();
    let description = format!(
        "Search for tools to call. Tools from these MCP servers are available but not loaded \
         up front: {servers}. Call this with a natural-language `query` describing the task to \
         discover matching tools; the matching tool definitions are then loaded and become \
         callable. Optionally pass `server_label` to restrict the search to one server."
    );
    let def = serde_json::json!({
        "type": "function",
        "name": TOOL_SEARCH_FUNCTION_NAME,
        "description": description,
        "parameters": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language description of the tool(s) you need."
                },
                "server_label": {
                    "type": "string",
                    "description": "Optional: restrict the search to this MCP server.",
                    "enum": labels,
                }
            },
            "required": ["query"],
            "additionalProperties": false
        },
        "strict": false
    });
    ResponsesToolDefinition::Function(
        crate::api_types::responses::FunctionTool::from_json(def)
            .expect("tool_search function-tool definition is well-formed"),
    )
}

/// Walk `payload.input` for `mcp_list_tools` items and return a map of
/// `server_label → Vec<McpListedTool>`. When the caller chains a prior
/// response via `previous_response_id` (or hand-rolls input items),
/// these catalogs are already in context and the API spec says we
/// shouldn't refetch.
fn collect_inlined_catalogs(
    payload: &CreateResponsesPayload,
) -> std::collections::HashMap<String, Vec<crate::api_types::responses::McpListedTool>> {
    let mut out = std::collections::HashMap::new();
    let Some(ResponsesInput::Items(items)) = payload.input.as_ref() else {
        return out;
    };
    for item in items {
        if let ResponsesInputItem::McpListTools(it) = item {
            // Skip catalogs flagged as errors — they have no usable
            // tools and we still need a fresh fetch.
            if it.error.is_some() {
                continue;
            }
            out.insert(it.server_label.clone(), it.tools.clone());
        }
    }
    out
}

/// Rewrite prior-turn `mcp_call` items echoed back in `payload.input`
/// into the `function_call` + `function_call_output` pair every provider
/// understands. Mutates in place; a no-op when no `mcp_call` items are
/// present.
///
/// Under `hadrian_hosted` the model only ever sees `mcp_<label>__<tool>`
/// *function* tools — the `function_call` it actually emits is suppressed
/// from the client and replaced by a synthesized `mcp_call` output item.
/// On a follow-up turn, a client that resends prior output items (the
/// standard Responses multi-turn pattern, and the *only* option for
/// providers without server-side `previous_response_id` state, i.e.
/// everything except OpenAI/Azure) sends those `mcp_call` items back as
/// input. Left untranslated they hit the per-provider conversion's
/// catch-all and are dropped — erasing the assistant's tool call and its
/// result from context. Rewriting them back into the function pair keeps
/// multi-turn MCP coherent behind any provider, mirroring exactly what
/// [`super::executor`] folds into the continuation during a live loop.
///
/// `mcp_list_tools` items are intentionally left for the provider
/// conversion to drop: they're output-only catalog snapshots the model
/// doesn't reason over, and dropping them loses no conversational
/// content. `mcp_approval_request` / `mcp_approval_response` items are
/// also left alone — pending approvals are resolved by [`super::resume`].
fn rewrite_mcp_history(payload: &mut CreateResponsesPayload) {
    let Some(ResponsesInput::Items(items)) = payload.input.as_mut() else {
        return;
    };
    if !items
        .iter()
        .any(|i| matches!(i, ResponsesInputItem::McpCall(_)))
    {
        return;
    }
    let mut rewritten = Vec::with_capacity(items.len() + 1);
    for item in std::mem::take(items) {
        match item {
            ResponsesInputItem::McpCall(call) => {
                let (function_call, output) = mcp_call_to_function_pair(call);
                rewritten.push(ResponsesInputItem::FunctionCall(function_call));
                rewritten.push(ResponsesInputItem::FunctionCallOutput(output));
            }
            other => rewritten.push(other),
        }
    }
    *items = rewritten;
}

/// Reconstruct the `(function_call, function_call_output)` pair for one
/// echoed `mcp_call` item. The two share a `call_id` derived from the
/// item id so the provider conversion pairs them (Anthropic `tool_use` /
/// `tool_result`, etc.). The result-string encoding matches the
/// executor's live-loop continuation: `output` verbatim on success,
/// `{"error": …}` on failure (the executor stores at most one of
/// `output` / `error`).
fn mcp_call_to_function_pair(call: McpCallItem) -> (FunctionToolCall, FunctionCallOutput) {
    let function_name = synthesize_function_name(&call.server_label, &call.name);
    // Reuse the item id as the pairing token — it's already unique per
    // response and never collides with a live `function_call.call_id`
    // (those are suppressed before the client ever sees them).
    let call_id = call.id.clone();
    let output_text = match (&call.output, &call.error) {
        (_, Some(err)) => serde_json::json!({ "error": err }).to_string(),
        (Some(out), None) => out.clone(),
        (None, None) => String::new(),
    };
    let function_call = FunctionToolCall {
        type_: FunctionToolCallType::FunctionCall,
        id: call.id,
        call_id: call_id.clone(),
        name: function_name,
        arguments: call.arguments,
        status: None,
    };
    let output = FunctionCallOutput {
        type_: FunctionCallOutputType::FunctionCallOutput,
        id: None,
        call_id,
        output: output_text,
        status: None,
    };
    (function_call, output)
}

fn meta_from_listed_tool(t: &crate::api_types::responses::McpListedTool) -> McpToolMeta {
    McpToolMeta {
        name: t.name.clone(),
        description: t.description.clone(),
        input_schema: t.input_schema.clone(),
        annotations: t.annotations.clone(),
    }
}

/// Translate `tool_choice = {"type": "mcp", server_label, name?}` into
/// the function-tool form the rewritten payload exposes.
///
/// OpenAI's `ToolChoiceMCP` schema accepts `name: null`, but the
/// schema description is "force the model to call a specific tool on a
/// remote MCP server" — i.e. `name` is the load-bearing field. Under
/// `passthrough_openai` the entry is forwarded verbatim and OpenAI's
/// hosted MCP runtime gets to apply its own semantics for the no-name
/// case. Under `hadrian_hosted` we rewrite to flat function tools, and
/// the upstream function `tool_choice` grammar has no "any function
/// from server L" predicate — so we can only honor the unambiguous
/// shapes:
///
/// - `{"type":"mcp","server_label":"L","name":"T"}` → named function
///   `mcp_<sanitized(L)>__T`.
/// - `{"type":"mcp","server_label":"L"}` with exactly one rewritten
///   function from `L` → pin to that single function.
/// - `{"type":"mcp","server_label":"L"}` with zero or ≥2 matches →
///   reject as `AmbiguousToolChoice`. The previous behaviour was to
///   fall back to `"required"`, which let the model pick from any
///   tool in the request (potentially a different server). Failing
///   fast is more honest than silently weakening the constraint.
fn rewrite_tool_choice(payload: &mut CreateResponsesPayload) -> Result<(), McpRewriteError> {
    let Some(choice) = payload.tool_choice.as_mut() else {
        return Ok(());
    };
    let ResponsesToolChoice::Mcp(ResponsesMcpToolChoice {
        server_label, name, ..
    }) = choice
    else {
        return Ok(());
    };
    let new_choice = match name {
        Some(tool_name) => ResponsesToolChoice::Named(ResponsesNamedToolChoice {
            type_: ResponsesNamedToolChoiceType::Function,
            name: synthesize_function_name(server_label, tool_name),
        }),
        None => {
            // Find every rewritten function whose synthesized prefix
            // matches this server label. The prefix is stable because
            // `sanitize_label` is deterministic.
            let sanitized = sanitize_label(server_label, MAX_LABEL_LEN);
            let prefix = format!("{FUNCTION_NAME_PREFIX}{sanitized}{SEPARATOR}");
            let mut matching: Vec<&str> = Vec::new();
            if let Some(tools) = payload.tools.as_ref() {
                for t in tools {
                    if let ResponsesToolDefinition::Function(f) = t
                        && f.name.starts_with(&prefix)
                    {
                        matching.push(f.name.as_str());
                    }
                }
            }
            match matching.as_slice() {
                [only] => ResponsesToolChoice::Named(ResponsesNamedToolChoice {
                    type_: ResponsesNamedToolChoiceType::Function,
                    name: (*only).to_string(),
                }),
                _ => {
                    return Err(McpRewriteError::AmbiguousToolChoice {
                        server_label: server_label.clone(),
                        match_count: matching.len(),
                    });
                }
            }
        }
    };
    *choice = new_choice;
    Ok(())
}

pub(crate) fn is_allowed(meta: &McpToolMeta, allowed: Option<&McpAllowedTools>) -> bool {
    match allowed {
        None => true,
        Some(McpAllowedTools::List(names)) => names.iter().any(|n| n == &meta.name),
        Some(McpAllowedTools::Filter(filter)) => filter_matches(filter, meta),
    }
}

/// True iff `meta` (name + `readOnlyHint` annotation) satisfies every
/// constraint declared on `filter`. Empty constraints (both fields
/// `None`) match every tool.
fn filter_matches(filter: &McpToolFilter, meta: &McpToolMeta) -> bool {
    if let Some(names) = filter.tool_names.as_ref()
        && !names.iter().any(|n| n == &meta.name)
    {
        return false;
    }
    if let Some(required) = filter.read_only {
        // Per the MCP tool filter spec, a `read_only` predicate matches
        // only tools carrying an explicit `readOnlyHint` annotation equal
        // to `required`. An absent annotation (`None`) matches neither
        // `true` nor `false` — don't coerce it to `false`.
        let hint = meta
            .annotations
            .as_ref()
            .and_then(|a| a.get("readOnlyHint"))
            .and_then(|v| v.as_bool());
        if hint != Some(required) {
            return false;
        }
    }
    true
}

/// Build the JSON function-tool definition the model will see for
/// one MCP-advertised tool. `defer_loading` is set on the function only
/// on the native-passthrough path (an OpenAI/Azure upstream running its
/// own tool search); the default Hadrian-side path defers via the
/// `tool_search` tool instead and passes `false`.
pub(crate) fn build_function_tool(
    mcp: &McpTool,
    meta: &McpToolMeta,
    defer_loading: bool,
) -> Result<ResponsesToolDefinition, McpRewriteError> {
    let function_name = synthesize_function_name(&mcp.server_label, &meta.name);
    let description = compose_description(mcp, meta);
    let parameters = if meta.input_schema.is_object() {
        meta.input_schema.clone()
    } else {
        serde_json::json!({"type": "object", "properties": {}})
    };

    let mut def = serde_json::json!({
        "type": "function",
        "name": function_name,
        "description": description,
        "parameters": parameters,
        "strict": false,
    });
    // `def` is a freshly constructed object literal, so `as_object_mut`
    // can't be `None` here.
    let obj = def
        .as_object_mut()
        .expect("function tool literal is an object");
    // Forward MCP tool annotations (read-only hint, idempotency,
    // destructive hint, title) verbatim so downstream consumers and
    // any provider that surfaces these to the model can use them.
    if let Some(ann) = meta.annotations.as_ref() {
        obj.insert("annotations".into(), ann.clone());
    }
    // Only the native-passthrough path forwards `defer_loading` onto the
    // function tool, leaving the OpenAI/Azure upstream to run its own
    // tool search. `defer_loading` is a valid field on `function` tools
    // per OpenAI's spec (`openapi/openai.openapi.json::FunctionToolParam`).
    if defer_loading {
        obj.insert("defer_loading".into(), serde_json::Value::Bool(true));
    }

    // `parameters` and `description` come from the remote MCP server
    // (untrusted). A pathological `input_schema` that fails to
    // deserialize into `FunctionTool` must surface as a 502, not panic
    // the request task.
    let function = crate::api_types::responses::FunctionTool::from_json(def).map_err(|e| {
        McpRewriteError::InvalidToolDefinition {
            server_label: mcp.server_label.clone(),
            tool: meta.name.clone(),
            message: e.to_string(),
        }
    })?;
    Ok(ResponsesToolDefinition::Function(function))
}

/// Stable function-name derivation. The **server label** is sanitized
/// and truncated to [`MAX_LABEL_LEN`]; the **tool name** is taken
/// verbatim because we need to round-trip it back to the MCP server
/// unchanged. The two budgets are sized so the combined
/// `mcp_<label>__<tool>` always fits the providers' 64-char function-name
/// regex (`[A-Za-z0-9_-]{1,64}`). Tool names that wouldn't pass
/// validation are rejected at rewrite time (see [`is_valid_tool_name`]).
pub fn synthesize_function_name(server_label: &str, tool_name: &str) -> String {
    let label = sanitize_label(server_label, MAX_LABEL_LEN);
    format!("{FUNCTION_NAME_PREFIX}{label}{SEPARATOR}{tool_name}")
}

/// Split a function name back into `(sanitized_label, tool_name)`.
/// The tool name is the original (un-sanitized) name the MCP server
/// exposed and is safe to pass to `tools/call`.
///
/// Returns `None` if `name` doesn't match the rewrite shape.
pub fn parse_function_name(name: &str) -> Option<(&str, &str)> {
    let stripped = name.strip_prefix(FUNCTION_NAME_PREFIX)?;
    let (label, tool) = stripped.split_once(SEPARATOR)?;
    if label.is_empty() || tool.is_empty() {
        return None;
    }
    Some((label, tool))
}

/// True iff `name` is a legal OpenAI function-tool name. Used to
/// filter MCP tools the model can't address — exotic names ("my.tool"
/// or non-ASCII) are skipped at rewrite time with a warning rather
/// than mangled (which would break the round-trip to `tools/call`).
pub fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

const FUNCTION_NAME_PREFIX: &str = "mcp_";
const SEPARATOR: &str = "__";
/// Every provider we target caps function/tool names at 64 chars
/// (`^[A-Za-z0-9_-]{1,64}$`). The synthesized name is
/// `mcp_<label>__<tool>`, so the label and tool budgets together must
/// leave room for the 6 fixed chars (`mcp_` + `__`). The static
/// assertion below enforces `prefix + label + sep + tool <= 64` so a
/// long label *and* a long tool name can never combine into a name the
/// upstream rejects (which would 400 the whole request). The tool name
/// is preserved verbatim — never truncated — so the round-trip back to
/// `tools/call` is exact; tools whose name alone exceeds `MAX_TOOL_LEN`
/// are skipped at rewrite time (see [`is_valid_tool_name`]).
const MAX_FUNCTION_NAME_LEN: usize = 64;
const MAX_LABEL_LEN: usize = 18;
const MAX_TOOL_LEN: usize = 40;
const _: () = assert!(
    FUNCTION_NAME_PREFIX.len() + MAX_LABEL_LEN + SEPARATOR.len() + MAX_TOOL_LEN
        <= MAX_FUNCTION_NAME_LEN,
    "synthesized MCP function name `mcp_<label>__<tool>` must fit the 64-char provider limit"
);

/// Replace anything outside `[A-Za-z0-9]` with `_`, collapse runs of
/// `_`, and truncate to `max_len`. Match-on-prefix detection means the
/// output must be ASCII-only.
///
/// Collapsing underscore runs is load-bearing: the synthesized function
/// name is `mcp_<label>__<tool>` and [`parse_function_name`] splits on
/// the first `__`. If a sanitized label could itself contain `__` (e.g.
/// raw `"a  b"` → `"a__b"`), the split would land mid-label and route a
/// call to the wrong tool. Collapsing guarantees the label has no `__`.
fn sanitize_label(s: &str, max_len: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_len));
    let mut prev_underscore = false;
    for ch in s.chars() {
        let mapped = if ch.is_ascii_alphanumeric() { ch } else { '_' };
        if mapped == '_' {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
        }
        out.push(mapped);
        if out.len() >= max_len {
            break;
        }
    }
    if out.is_empty() {
        out.push('x');
    }
    out
}

fn compose_description(mcp: &McpTool, meta: &McpToolMeta) -> String {
    // Fold in the operator-supplied `server_description` — OpenAI's spec
    // says it's "used to provide more context" and the model uses it to
    // decide when a server's tools are relevant.
    let mut prefix = format!("MCP tool from server `{}`.", mcp.server_label);
    if let Some(sd) = mcp.server_description.as_deref().filter(|s| !s.is_empty()) {
        prefix = format!("{prefix} {sd}");
    }
    match meta.description.as_deref() {
        Some(d) if !d.is_empty() => format!("{prefix} {d}"),
        _ => prefix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::responses::McpToolType;

    fn meta(name: &str, description: Option<&str>) -> McpToolMeta {
        McpToolMeta {
            name: name.to_string(),
            description: description.map(str::to_string),
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        }
    }

    fn mcp_with(label: &str, allowed: Option<McpAllowedTools>) -> McpTool {
        McpTool {
            type_: McpToolType::Mcp,
            server_label: label.to_string(),
            server_url: Some("https://x".to_string()),
            connector_id: None,
            server_description: None,
            authorization: None,
            headers: None,
            require_approval: None,
            allowed_tools: allowed,
            defer_loading: None,
            defer_loading_passthrough: None,
            call_timeout_secs: None,
        }
    }

    #[test]
    fn synthesized_name_round_trips() {
        let n = synthesize_function_name("atlassian", "jira_search");
        assert_eq!(n, "mcp_atlassian__jira_search");
        let (l, t) = parse_function_name(&n).unwrap();
        assert_eq!(l, "atlassian");
        assert_eq!(t, "jira_search");
    }

    #[test]
    fn synthesized_name_sanitizes_label_only() {
        // Label gets sanitized; tool name is preserved verbatim so the
        // call round-trips to the MCP server.
        let n = synthesize_function_name("My Co/Linear", "issue_list");
        assert!(n.starts_with("mcp_My_Co_Linear__"));
        let (l, t) = parse_function_name(&n).unwrap();
        assert_eq!(l, "My_Co_Linear");
        assert_eq!(t, "issue_list");
    }

    #[test]
    fn synthesized_name_truncates_long_labels() {
        let long_label = "a".repeat(100);
        let n = synthesize_function_name(&long_label, "tool");
        let (l, _) = parse_function_name(&n).unwrap();
        assert_eq!(l.len(), MAX_LABEL_LEN);
    }

    #[test]
    fn synthesized_name_fits_provider_limit_at_max_budgets() {
        // Worst case: a label that truncates to MAX_LABEL_LEN plus a tool
        // name at exactly MAX_TOOL_LEN must still fit the 64-char limit.
        let label = "a".repeat(100);
        let tool = "b".repeat(MAX_TOOL_LEN);
        assert!(is_valid_tool_name(&tool));
        let n = synthesize_function_name(&label, &tool);
        assert!(
            n.len() <= MAX_FUNCTION_NAME_LEN,
            "synthesized name {} chars exceeds {MAX_FUNCTION_NAME_LEN}",
            n.len()
        );
    }

    #[test]
    fn parse_rejects_non_prefix() {
        assert!(parse_function_name("foo_bar__baz").is_none());
        assert!(parse_function_name("mcp_only").is_none());
        assert!(parse_function_name("mcp___missing_label").is_none());
    }

    #[test]
    fn sanitize_label_collapses_underscore_runs_so_name_round_trips() {
        // A label with consecutive non-alnum chars must NOT yield `__`
        // inside the label, or `parse_function_name` (which splits on the
        // first `__`) would land mid-label and route to the wrong tool.
        let n = synthesize_function_name("a  b", "do__thing");
        assert_eq!(n, "mcp_a_b__do__thing");
        let (l, t) = parse_function_name(&n).unwrap();
        assert_eq!(l, "a_b");
        // Tool name (which legitimately contains `__`) round-trips intact.
        assert_eq!(t, "do__thing");
    }

    #[test]
    fn distinct_labels_collide_after_sanitization() {
        // `-` and `_` both sanitize to `_`, so these two raw labels map to
        // the same function-name prefix — duplicate detection keys on the
        // sanitized form to reject the collision.
        assert_eq!(
            sanitize_label("My-Co", MAX_LABEL_LEN),
            sanitize_label("My_Co", MAX_LABEL_LEN)
        );
    }

    #[test]
    fn is_valid_tool_name_accepts_typical_names() {
        assert!(is_valid_tool_name("jira_search"));
        assert!(is_valid_tool_name("get-user"));
        assert!(is_valid_tool_name("a"));
    }

    #[test]
    fn is_valid_tool_name_rejects_problematic_names() {
        assert!(!is_valid_tool_name(""));
        assert!(!is_valid_tool_name("issue.list")); // contains dot
        assert!(!is_valid_tool_name("my tool")); // contains space
        assert!(!is_valid_tool_name("héllo")); // non-ASCII
        assert!(!is_valid_tool_name(&"x".repeat(50))); // too long
    }

    #[test]
    fn is_allowed_none_means_all() {
        let m = meta("jira_search", None);
        assert!(is_allowed(&m, None));
    }

    #[test]
    fn is_allowed_list_form() {
        let m = meta("jira_search", None);
        assert!(is_allowed(
            &m,
            Some(&McpAllowedTools::List(vec!["jira_search".into()]))
        ));
        assert!(!is_allowed(
            &m,
            Some(&McpAllowedTools::List(vec!["other".into()]))
        ));
    }

    #[test]
    fn is_allowed_filter_form() {
        let m = meta("jira_search", None);
        assert!(is_allowed(
            &m,
            Some(&McpAllowedTools::Filter(McpToolFilter {
                tool_names: Some(vec!["jira_search".into()]),
                read_only: None,
            }))
        ));
    }

    #[test]
    fn is_allowed_filter_read_only_matches_annotation() {
        let mut m = meta("jira_search", None);
        m.annotations = Some(serde_json::json!({"readOnlyHint": true}));
        assert!(is_allowed(
            &m,
            Some(&McpAllowedTools::Filter(McpToolFilter {
                tool_names: None,
                read_only: Some(true),
            }))
        ));
        // read_only: true demands the hint to be true; a tool without
        // the annotation is excluded.
        let m2 = meta("jira_create", None);
        assert!(!is_allowed(
            &m2,
            Some(&McpAllowedTools::Filter(McpToolFilter {
                tool_names: None,
                read_only: Some(true),
            }))
        ));
    }

    #[test]
    fn description_falls_back_when_server_omits_one() {
        let m = mcp_with("atlassian", None);
        let t = meta("jira_search", None);
        let desc = compose_description(&m, &t);
        assert_eq!(desc, "MCP tool from server `atlassian`.");
    }

    #[test]
    fn description_includes_server_description() {
        let m = mcp_with("atlassian", None);
        let t = meta("jira_search", Some("Search Jira issues"));
        let desc = compose_description(&m, &t);
        assert!(desc.starts_with("MCP tool from server `atlassian`."));
        assert!(desc.contains("Search Jira issues"));
    }

    #[test]
    fn build_function_tool_produces_function_variant() {
        let m = mcp_with("atlassian", None);
        let t = meta("jira_search", Some("Search Jira"));
        let def = build_function_tool(&m, &t, false).expect("valid tool def");
        assert!(matches!(def, ResponsesToolDefinition::Function(_)));
        if let ResponsesToolDefinition::Function(func) = def {
            assert_eq!(func.name, "mcp_atlassian__jira_search");
            assert!(func.description.as_deref().unwrap().contains("Search Jira"));
            assert_eq!(
                func.parameters.as_ref().unwrap()["type"],
                serde_json::Value::String("object".into())
            );
            assert!(!func.extras.contains_key("annotations"));
        }
    }

    #[test]
    fn build_function_tool_sets_defer_loading_on_passthrough_path() {
        // `defer_loading` is set on the function only when the caller asks
        // for native passthrough (an OpenAI/Azure upstream runs its own
        // tool search). The flag is a valid field on FunctionToolParam.
        let m = mcp_with("atlassian", None);
        let t = meta("jira_search", None);
        let def = build_function_tool(&m, &t, true).expect("valid tool def");
        if let ResponsesToolDefinition::Function(func) = def {
            assert_eq!(func.defer_loading, Some(true));
        } else {
            panic!("expected function tool");
        }
    }

    #[test]
    fn build_function_tool_omits_defer_loading_on_default_path() {
        let m = mcp_with("atlassian", None);
        let t = meta("jira_search", None);
        let def = build_function_tool(&m, &t, false).expect("valid tool def");
        if let ResponsesToolDefinition::Function(func) = def {
            assert_eq!(func.defer_loading, None);
        } else {
            panic!("expected function tool");
        }
    }

    #[test]
    fn build_function_tool_forwards_annotations() {
        let m = mcp_with("atlassian", None);
        let mut t = meta("jira_search", None);
        t.annotations = Some(serde_json::json!({
            "readOnlyHint": true,
            "title": "Search Jira",
        }));
        let def = build_function_tool(&m, &t, false).expect("valid tool def");
        if let ResponsesToolDefinition::Function(func) = def {
            let ann = func.extras.get("annotations").expect("annotations present");
            assert_eq!(ann["readOnlyHint"], true);
            assert_eq!(ann["title"], "Search Jira");
        } else {
            panic!("expected function tool");
        }
    }

    #[tokio::test]
    async fn rewrite_uses_inlined_catalog_and_skips_upstream_fetch() {
        // When the caller's input already includes an `mcp_list_tools`
        // item for the same server_label, the rewrite uses that catalog
        // directly. We confirm the upstream wasn't contacted by pointing
        // server_url at a non-existent host — if the rewrite tried to
        // fetch, the test would fail with a network error.
        use crate::api_types::responses::{
            McpListToolsItem, McpListToolsItemType, McpListedTool, ResponsesInput,
            ResponsesInputItem,
        };

        let service = McpService::new();
        let payload_json = serde_json::json!({
            "tools": [{
                "type": "mcp",
                "server_label": "atlassian",
                "server_url": "http://127.0.0.1:1/never-reachable",
            }],
        });
        let mut payload: CreateResponsesPayload = serde_json::from_value(payload_json).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::McpListTools(McpListToolsItem {
                type_: McpListToolsItemType::McpListTools,
                id: "mcpl_abc".into(),
                server_label: "atlassian".into(),
                tools: vec![McpListedTool {
                    name: "jira_search".into(),
                    description: Some("Search Jira".into()),
                    input_schema: serde_json::json!({"type":"object"}),
                    annotations: None,
                }],
                error: None,
            }),
        ]));

        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Test)
            .await
            .expect("rewrite uses inlined catalog without contacting upstream");

        let tools = payload.tools.unwrap();
        assert_eq!(tools.len(), 1);
        if let ResponsesToolDefinition::Function(f) = &tools[0] {
            assert_eq!(f.name, "mcp_atlassian__jira_search");
        } else {
            panic!("expected one rewritten function tool");
        }

        // The cache was primed; the executor's cached_tools read path
        // sees the same catalog.
        let cached = service.cached_tools(
            "http://127.0.0.1:1/never-reachable",
            None,
            &std::collections::HashMap::new(),
        );
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().len(), 1);
    }

    /// Build a payload with one `mcp` tool entry whose catalog is inlined
    /// (so the rewrite never hits the network), plus optional `extra`
    /// fields merged onto the tool entry (e.g. `defer_loading`).
    fn payload_with_inlined_catalog(
        tool_names: &[&str],
        extra: serde_json::Value,
    ) -> CreateResponsesPayload {
        use crate::api_types::responses::{
            McpListToolsItem, McpListToolsItemType, McpListedTool, ResponsesInput,
            ResponsesInputItem,
        };
        let mut tool = serde_json::json!({
            "type": "mcp",
            "server_label": "atlassian",
            "server_url": "http://127.0.0.1:1/never-reachable",
        });
        if let (Some(obj), Some(extra)) = (tool.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                obj.insert(k.clone(), v.clone());
            }
        }
        let mut payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({ "tools": [tool] })).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::McpListTools(McpListToolsItem {
                type_: McpListToolsItemType::McpListTools,
                id: "mcpl_abc".into(),
                server_label: "atlassian".into(),
                tools: tool_names
                    .iter()
                    .map(|n| McpListedTool {
                        name: (*n).into(),
                        description: Some(format!("desc for {n}")),
                        input_schema: serde_json::json!({"type":"object"}),
                        annotations: None,
                    })
                    .collect(),
                error: None,
            }),
        ]));
        payload
    }

    fn tool_names(payload: &CreateResponsesPayload) -> Vec<String> {
        payload
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|t| match t {
                ResponsesToolDefinition::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn rewrite_deferred_server_injects_tool_search_not_per_tool_functions() {
        let service = McpService::new();
        let mut payload = payload_with_inlined_catalog(
            &["jira_search", "jira_create"],
            serde_json::json!({ "defer_loading": true }),
        );
        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Anthropic)
            .await
            .unwrap();

        let names = tool_names(&payload);
        // Exactly the search tool — no per-tool mcp_* functions.
        assert_eq!(names, vec![TOOL_SEARCH_FUNCTION_NAME.to_string()]);
        assert!(!names.iter().any(|n| n.starts_with("mcp_atlassian__")));

        // Catalog is still primed for the executor to search.
        let cached = service.cached_tools(
            "http://127.0.0.1:1/never-reachable",
            None,
            &std::collections::HashMap::new(),
        );
        assert_eq!(cached.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rewrite_eager_server_exposes_per_tool_functions() {
        let service = McpService::new();
        let mut payload =
            payload_with_inlined_catalog(&["jira_search", "jira_create"], serde_json::json!({}));
        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Anthropic)
            .await
            .unwrap();

        let mut names = tool_names(&payload);
        names.sort();
        assert_eq!(
            names,
            vec![
                "mcp_atlassian__jira_create".to_string(),
                "mcp_atlassian__jira_search".to_string(),
            ]
        );
        // No search tool when nothing is deferred.
        assert!(!names.iter().any(|n| n == TOOL_SEARCH_FUNCTION_NAME));
    }

    #[tokio::test]
    async fn rewrite_passthrough_keeps_functions_with_defer_loading_on_openai() {
        let service = McpService::new();
        let mut payload = payload_with_inlined_catalog(
            &["jira_search"],
            serde_json::json!({ "defer_loading": true, "defer_loading_passthrough": true }),
        );
        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::OpenAi)
            .await
            .unwrap();

        let tools = payload.tools.unwrap();
        assert_eq!(tools.len(), 1);
        // Native passthrough: per-tool function exposed with defer_loading set,
        // and NO Hadrian-side tool_search tool.
        match &tools[0] {
            ResponsesToolDefinition::Function(f) => {
                assert_eq!(f.name, "mcp_atlassian__jira_search");
                assert_eq!(f.defer_loading, Some(true));
            }
            other => panic!("expected function tool, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rewrite_passthrough_rejected_on_non_openai_provider() {
        let service = McpService::new();
        let mut payload = payload_with_inlined_catalog(
            &["jira_search"],
            serde_json::json!({ "defer_loading": true, "defer_loading_passthrough": true }),
        );
        let err = rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Anthropic)
            .await
            .expect_err("passthrough on non-OpenAI must be rejected");
        assert_eq!(err.code(), "mcp_defer_loading_passthrough_unsupported");
        assert!(err.is_client_error());
    }

    #[tokio::test]
    async fn rewrite_tool_choice_forces_deferred_server_eager() {
        // A deferred server pinned by tool_choice is exposed eagerly so
        // the forced call can resolve — no tool_search for it.
        let service = McpService::new();
        let mut payload = payload_with_inlined_catalog(
            &["jira_search", "jira_create"],
            serde_json::json!({ "defer_loading": true }),
        );
        payload.tool_choice = Some(
            serde_json::from_value(serde_json::json!({
                "type": "mcp",
                "server_label": "atlassian",
                "name": "jira_search"
            }))
            .unwrap(),
        );
        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Anthropic)
            .await
            .unwrap();

        let names = tool_names(&payload);
        // Forced eager → per-tool functions present, no tool_search.
        assert!(names.iter().any(|n| n == "mcp_atlassian__jira_search"));
        assert!(!names.iter().any(|n| n == TOOL_SEARCH_FUNCTION_NAME));
    }

    #[tokio::test]
    async fn rewrite_rejects_ambiguous_tool_choice_without_name() {
        // tool_choice = {type:"mcp", server_label:"L"} with no name
        // and >1 tools surviving the rewrite is ambiguous under
        // hadrian_hosted — fail with mcp_ambiguous_tool_choice rather
        // than silently weakening to "required".
        use crate::api_types::responses::{
            McpListToolsItem, McpListToolsItemType, McpListedTool, ResponsesInput,
            ResponsesInputItem,
        };

        let service = McpService::new();
        let payload_json = serde_json::json!({
            "tools": [{
                "type": "mcp",
                "server_label": "atlassian",
                "server_url": "http://127.0.0.1:1/never-reachable",
            }],
            "tool_choice": {
                "type": "mcp",
                "server_label": "atlassian",
            },
        });
        let mut payload: CreateResponsesPayload = serde_json::from_value(payload_json).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::McpListTools(McpListToolsItem {
                type_: McpListToolsItemType::McpListTools,
                id: "mcpl_abc".into(),
                server_label: "atlassian".into(),
                tools: vec![
                    McpListedTool {
                        name: "jira_search".into(),
                        description: None,
                        input_schema: serde_json::json!({"type":"object"}),
                        annotations: None,
                    },
                    McpListedTool {
                        name: "jira_create".into(),
                        description: None,
                        input_schema: serde_json::json!({"type":"object"}),
                        annotations: None,
                    },
                ],
                error: None,
            }),
        ]));

        let err = rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Test)
            .await
            .expect_err("expected ambiguous tool_choice rejection");
        match err {
            McpRewriteError::AmbiguousToolChoice {
                server_label,
                match_count,
            } => {
                assert_eq!(server_label, "atlassian");
                assert_eq!(match_count, 2);
            }
            other => panic!("expected AmbiguousToolChoice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rewrite_pins_to_single_match_when_name_omitted() {
        // The degenerate single-match case still works: tool_choice
        // without `name` resolves to the only tool surviving the rewrite.
        use crate::api_types::responses::{
            McpListToolsItem, McpListToolsItemType, McpListedTool, ResponsesInput,
            ResponsesInputItem,
        };

        let service = McpService::new();
        let payload_json = serde_json::json!({
            "tools": [{
                "type": "mcp",
                "server_label": "atlassian",
                "server_url": "http://127.0.0.1:1/never-reachable",
            }],
            "tool_choice": {
                "type": "mcp",
                "server_label": "atlassian",
            },
        });
        let mut payload: CreateResponsesPayload = serde_json::from_value(payload_json).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::McpListTools(McpListToolsItem {
                type_: McpListToolsItemType::McpListTools,
                id: "mcpl_abc".into(),
                server_label: "atlassian".into(),
                tools: vec![McpListedTool {
                    name: "jira_search".into(),
                    description: None,
                    input_schema: serde_json::json!({"type":"object"}),
                    annotations: None,
                }],
                error: None,
            }),
        ]));

        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Test)
            .await
            .unwrap();
        match payload.tool_choice.as_ref().unwrap() {
            ResponsesToolChoice::Named(n) => {
                assert_eq!(n.name, "mcp_atlassian__jira_search");
            }
            other => panic!("expected Named function tool_choice, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_tool_choice_is_client_error() {
        let e = McpRewriteError::AmbiguousToolChoice {
            server_label: "atlassian".into(),
            match_count: 2,
        };
        assert!(e.is_client_error());
        assert_eq!(e.code(), "mcp_ambiguous_tool_choice");
    }

    #[test]
    fn list_tools_failed_is_not_client_error() {
        let e = McpRewriteError::ListToolsFailed {
            server_label: "atlassian".into(),
            server_url: "https://x".into(),
            message: "boom".into(),
        };
        assert!(!e.is_client_error());
    }

    #[tokio::test]
    async fn rewrite_translates_prior_mcp_call_to_function_pair() {
        // A follow-up turn echoes a completed `mcp_call`; the rewrite must
        // turn it into a `function_call` + `function_call_output` pair so
        // non-OpenAI providers (which drop raw `mcp_call` input items)
        // still see the assistant's call and its result.
        use crate::api_types::responses::{
            McpCallItem, McpCallItemType, McpItemStatus, ResponsesInput, ResponsesInputItem,
        };

        let service = McpService::new();
        // No `mcp` tool entry on this turn — history rewrite must still run.
        let mut payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({})).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![ResponsesInputItem::McpCall(
            McpCallItem {
                type_: McpCallItemType::McpCall,
                id: "mcp_abc".into(),
                server_label: "atlassian".into(),
                name: "jira_search".into(),
                arguments: r#"{"q":"bug"}"#.into(),
                status: McpItemStatus::Completed,
                output: Some("found 3 issues".into()),
                error: None,
                approval_request_id: None,
            },
        )]));

        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Anthropic)
            .await
            .unwrap();

        let Some(ResponsesInput::Items(items)) = payload.input.as_ref() else {
            panic!("expected items input");
        };
        assert_eq!(items.len(), 2, "one mcp_call → call + output pair");
        match &items[0] {
            ResponsesInputItem::FunctionCall(fc) => {
                assert_eq!(fc.name, "mcp_atlassian__jira_search");
                assert_eq!(fc.arguments, r#"{"q":"bug"}"#);
                assert_eq!(fc.call_id, "mcp_abc");
            }
            other => panic!("expected function_call, got {other:?}"),
        }
        match &items[1] {
            ResponsesInputItem::FunctionCallOutput(out) => {
                assert_eq!(out.call_id, "mcp_abc");
                assert_eq!(out.output, "found 3 issues");
            }
            other => panic!("expected function_call_output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rewrite_translates_failed_mcp_call_with_error_envelope() {
        // A failed `mcp_call` (output null, error set) folds into the same
        // `{"error": …}` envelope the executor uses in the live loop.
        use crate::api_types::responses::{
            McpCallItem, McpCallItemType, McpItemStatus, ResponsesInput, ResponsesInputItem,
        };

        let service = McpService::new();
        let mut payload: CreateResponsesPayload =
            serde_json::from_value(serde_json::json!({})).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![ResponsesInputItem::McpCall(
            McpCallItem {
                type_: McpCallItemType::McpCall,
                id: "mcp_err".into(),
                server_label: "atlassian".into(),
                name: "jira_search".into(),
                arguments: "{}".into(),
                status: McpItemStatus::Failed,
                output: None,
                error: Some("upstream 500".into()),
                approval_request_id: None,
            },
        )]));

        rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Anthropic)
            .await
            .unwrap();

        let Some(ResponsesInput::Items(items)) = payload.input.as_ref() else {
            panic!("expected items input");
        };
        match &items[1] {
            ResponsesInputItem::FunctionCallOutput(out) => {
                assert_eq!(out.output, r#"{"error":"upstream 500"}"#);
            }
            other => panic!("expected function_call_output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rewrite_ignores_inlined_catalog_when_errored() {
        // An `mcp_list_tools` item carrying `error` is not a usable
        // catalog; the rewrite must fall through to a fresh fetch.
        // Since we point at an unreachable host, that fetch fails — we
        // verify the rewrite *attempted* the fetch by checking for the
        // ListToolsFailed error rather than success.
        use crate::api_types::responses::{
            McpListToolsItem, McpListToolsItemType, ResponsesInput, ResponsesInputItem,
        };

        let service = McpService::new();
        let payload_json = serde_json::json!({
            "tools": [{
                "type": "mcp",
                "server_label": "atlassian",
                "server_url": "http://127.0.0.1:1/never-reachable",
            }],
        });
        let mut payload: CreateResponsesPayload = serde_json::from_value(payload_json).unwrap();
        payload.input = Some(ResponsesInput::Items(vec![
            ResponsesInputItem::McpListTools(McpListToolsItem {
                type_: McpListToolsItemType::McpListTools,
                id: "mcpl_abc".into(),
                server_label: "atlassian".into(),
                tools: vec![],
                error: Some("upstream down".into()),
            }),
        ]));

        let err = rewrite_mcp_tools(&mut payload, &service, McpProviderKind::Test)
            .await
            .expect_err("errored catalog should not be used; upstream fetch must run");
        assert!(matches!(err, McpRewriteError::ListToolsFailed { .. }));
    }
}
