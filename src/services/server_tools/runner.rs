//! Streaming orchestrator that runs registered `ServerExecutedTool`s in a
//! shared multi-turn loop.

use std::{collections::HashMap, sync::Arc};

use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures_util::{StreamExt, stream::FuturesUnordered};
use http::Response;
use tokio::sync::mpsc;
use tracing::{Instrument, debug, error, info, info_span, warn};

use super::{DetectedToolCall, ProviderCallback, ServerExecutedTool, ToolCallResult, ToolContext};
use crate::{
    api_types::responses::{
        CreateResponsesPayload, EasyInputMessage, EasyInputMessageContent, EasyInputMessageRole,
        OutputItemFunctionCall, OutputMessage, ResponsesIncludable, ResponsesInput,
        ResponsesInputItem, ResponsesReasoning, ResponsesUsage,
    },
    observability::metrics::record_server_tool_iteration,
    streaming::SseBuffer,
};

/// Multi-tool orchestrator for streaming Responses API output.
///
/// Wraps the upstream response body, reads SSE events, dispatches detection
/// across all registered tools, executes detected calls, and continues the
/// conversation with the provider until either the model stops calling
/// tools or the global iteration budget is exhausted.
pub struct ToolLoopRunner {
    tools: Vec<Arc<dyn ServerExecutedTool>>,
    provider_callback: Option<ProviderCallback>,
    original_payload: CreateResponsesPayload,
    max_iterations: usize,
    rewrite_output: bool,
    response_id: Option<String>,
    /// `(function-name prefix, original tool JSON)` pairs used to collapse
    /// rewritten MCP function tools back into the caller's original `mcp`
    /// entry on the echoed `response.tools`. Empty when no MCP rewrite ran.
    /// See [`ToolLoopRunner::with_mcp_tool_echo`].
    mcp_tool_echo: Vec<(String, serde_json::Value)>,
}

impl ToolLoopRunner {
    /// Create a new runner.
    ///
    /// `max_iterations` is the maximum number of provider continuation
    /// requests the runner will dispatch — i.e., the total number of
    /// times the loop body executes. Counted globally across all tools.
    pub fn new(original_payload: CreateResponsesPayload, max_iterations: usize) -> Self {
        Self {
            tools: Vec::new(),
            provider_callback: None,
            original_payload,
            max_iterations,
            rewrite_output: false,
            response_id: None,
            mcp_tool_echo: Vec::new(),
        }
    }

    /// Restore the caller's original `mcp` tool entries on the echoed
    /// `response.tools`. Under `hadrian_hosted` the request's `mcp` tool is
    /// rewritten into N `mcp_<label>__<tool>` function tools before it hits
    /// the provider, so the provider echoes those internal function tools
    /// instead of the `mcp` entry the caller sent. Each pair is
    /// `(function-name prefix, original tool JSON)`: on every lifecycle
    /// event the rewriter collapses a run of function tools whose `name`
    /// starts with the prefix back into the single original `mcp` entry.
    /// Only takes effect when [`Self::rewrite_output`] is enabled.
    pub fn with_mcp_tool_echo(mut self, echo: Vec<(String, serde_json::Value)>) -> Self {
        self.mcp_tool_echo = echo;
        self
    }

    /// Set the stable Hadrian `resp_…` id to stamp onto lifecycle events,
    /// replacing the provider's per-turn id. Only takes effect when
    /// [`Self::rewrite_output`] is enabled. Pass the persisted response id
    /// so the streamed id matches what's retrievable / chainable.
    pub fn with_response_id(mut self, response_id: String) -> Self {
        self.response_id = Some(response_id);
        self
    }

    /// Enable single-stream normalization of the forwarded events.
    ///
    /// When on, the runner owns one monotonic `sequence_number` /
    /// `output_index` space across the prefix events, every provider
    /// turn, and the tool-synthesized events, and reconstructs the
    /// terminal `response.output` from the `output_item.done` items it
    /// actually forwards. This is what lets a tool that synthesizes its
    /// own output items (the MCP executor: `mcp_list_tools` / `mcp_call`
    /// / `mcp_approval_request`) and suppresses the underlying
    /// function-call plumbing produce a spec-shaped, collision-free
    /// stream *and* a persisted output that matches it. Off by default
    /// so tools that don't synthesize items keep the provider's stream
    /// verbatim.
    pub fn rewrite_output(mut self, on: bool) -> Self {
        self.rewrite_output = on;
        self
    }

    /// Register a tool. Tools are dispatched in registration order; first
    /// `detect()` match wins for a given event.
    pub fn register(mut self, tool: Arc<dyn ServerExecutedTool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Set the provider callback used for continuation requests.
    ///
    /// Without a callback the runner only detects + emits in-progress
    /// events; it doesn't actually execute multi-turn. Most callers should
    /// always set this.
    pub fn with_provider_callback(mut self, callback: ProviderCallback) -> Self {
        self.provider_callback = Some(callback);
        self
    }

    /// Are any registered tools enabled for the original payload?
    pub fn has_enabled_tools(&self) -> bool {
        self.tools
            .iter()
            .any(|t| t.is_enabled_for(&self.original_payload))
    }

    /// Wrap a streaming HTTP response, intercepting and executing tool
    /// calls along the way.
    ///
    /// If no registered tool is enabled for the request, returns the
    /// response unchanged.
    pub fn wrap_streaming(self, response: Response<Body>) -> Response<Body> {
        // Filter to enabled tools up-front so detection loops are tight.
        let enabled_tools: Vec<Arc<dyn ServerExecutedTool>> = self
            .tools
            .into_iter()
            .filter(|t| t.is_enabled_for(&self.original_payload))
            .collect();

        if enabled_tools.is_empty() {
            return response;
        }

        let (parts, body) = response.into_parts();
        let max_iterations = self.max_iterations;
        let has_callback = self.provider_callback.is_some();
        let provider_callback = self.provider_callback;
        let original_payload = self.original_payload;
        // **Hadrian Extension:** `include: ["usage.incremental"]` opts into
        // cumulative `response.usage.updated` events at turn boundaries.
        let emit_usage_updates = original_payload
            .include
            .as_deref()
            .is_some_and(|i| i.contains(&ResponsesIncludable::UsageIncremental));
        let rewrite_output = self.rewrite_output;
        let response_id = self.response_id;
        let mcp_tool_echo = self.mcp_tool_echo;

        let span = info_span!(
            "tool_loop_runner",
            tool_count = enabled_tools.len(),
            max_iterations = max_iterations,
            has_callback = has_callback,
        );

        let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(32);

        crate::compat::spawn_detached(
            async move {
                let ctx = ToolContext {
                    original_payload: original_payload.clone(),
                };
                let tool_by_name: HashMap<&'static str, Arc<dyn ServerExecutedTool>> =
                    enabled_tools
                        .iter()
                        .map(|t| (t.name(), t.clone()))
                        .collect();
                let tool_names: Vec<&'static str> =
                    enabled_tools.iter().map(|t| t.name()).collect();

                // Single-stream normalizer (sequence_number / output_index /
                // terminal-output reconstruction). `None` keeps the provider
                // stream verbatim — see `ToolLoopRunner::rewrite_output`.
                let mut rewriter = rewrite_output
                    .then(|| StreamRewriter::new(response_id.clone(), mcp_tool_echo.clone()));

                // Collect any one-shot prefix events the tools want to
                // surface (the `mcp` `mcp_list_tools` catalog snapshot).
                // With the rewriter on, they're deferred until after the
                // lifecycle start events so `response.created` leads the
                // stream; without it, they're forwarded verbatim up front.
                for tool in &enabled_tools {
                    for event in tool.prefix_events() {
                        match rewriter.as_mut() {
                            Some(r) => r.defer_prefix(event),
                            None => {
                                if tx.send(Ok(event)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }

                let mut iteration: usize = 0;
                let mut current_body = body;
                // Set when an error/fallback path forwards a turn's raw
                // accumulated bytes (which still carry that turn's `[DONE]`
                // sentinel). The normal path swallows every per-turn `[DONE]`
                // and emits a single terminal one after the loop; this flag
                // keeps the fallback paths from producing a second `[DONE]`.
                let mut raw_tail_forwarded = false;
                // Continuation payload carried across iterations. Each
                // turn appends the assistant items the upstream emitted
                // plus the tool function-call outputs, so the model
                // sees its own prior tool_use/tool_result pairs on
                // subsequent turns. Without this, providers that
                // translate Responses items into native pairwise
                // formats (e.g. Anthropic via OpenRouter) drop the
                // orphan tool outputs on the floor and the model loops
                // forever as if it had never run anything.
                let mut continuation_payload = original_payload.clone();

                loop {
                    iteration += 1;
                    let at_iteration_limit = iteration > max_iterations;

                    let iter_span = info_span!(
                        "tool_loop_iteration",
                        iteration = iteration,
                        at_limit = at_iteration_limit,
                    );
                    let _iter_guard = iter_span.enter();

                    let mut body_stream = current_body.into_data_stream();
                    let mut accumulated = BytesMut::new();
                    let mut detected: Vec<DetectedToolCall> = Vec::new();
                    let mut sse_buffer = SseBuffer::new();
                    // The turn's suppressed *final* terminal event
                    // (`response.completed` / `failed` / `incomplete`), kept so
                    // a failure/abort path can re-emit it through the rewriter
                    // instead of dumping the provider's raw last-turn bytes.
                    let mut suppressed_terminal: Option<Bytes> = None;
                    // Assistant items the upstream emitted this turn.
                    // Threaded into the continuation payload below so
                    // the function-call outputs from this iteration
                    // have matching function_call items to anchor to.
                    let mut captured_assistant_items: Vec<ResponsesInputItem> = Vec::new();

                    // Read the current response stream, forwarding events
                    // until we've finished consuming or detected calls.
                    while let Some(chunk_result) = body_stream.next().await {
                        match chunk_result {
                            Ok(chunk) => {
                                accumulated.extend_from_slice(&chunk);
                                sse_buffer.extend(&chunk);

                                for event in sse_buffer.extract_complete_events() {
                                    // Each provider turn ends with its own
                                    // `[DONE]` sentinel. Swallow them all here
                                    // and emit exactly one terminal `[DONE]`
                                    // after the loop — otherwise a spec-
                                    // compliant SSE client treats the first
                                    // mid-loop `[DONE]` as end-of-stream and
                                    // never sees the remaining turns.
                                    if is_done_sentinel(&event) {
                                        continue;
                                    }
                                    if !at_iteration_limit {
                                        for tool in &enabled_tools {
                                            let calls = tool.detect(&event, &ctx);
                                            for call in calls {
                                                info!(
                                                    stage = "tool_call_detected",
                                                    tool = call.tool_name,
                                                    call_id = %call.call_id,
                                                    iteration = iteration,
                                                    "Detected tool call"
                                                );
                                                detected.push(call);
                                            }
                                        }

                                        if let Some(item) = parse_assistant_item(&event) {
                                            captured_assistant_items.push(item);
                                        }

                                        // Once a tool call has been detected for
                                        // this iteration, hold back only the
                                        // iteration-terminator events
                                        // (`response.created`,
                                        // `response.in_progress`,
                                        // `response.completed`, ...) — they would
                                        // confuse a client into thinking the
                                        // upstream is finished when in fact we're
                                        // about to continue the loop. Item-level
                                        // events (`output_item.done`,
                                        // `content_part.done`, etc.) are
                                        // informational and must be forwarded so
                                        // both streaming clients and the
                                        // non-streaming bridge can reconstruct
                                        // the full transcript.
                                        if !detected.is_empty()
                                            && has_callback
                                            && is_iteration_terminator(&event)
                                        {
                                            // Capture this suppressed turn's
                                            // usage before dropping it, so the
                                            // final terminal can report the
                                            // whole loop's tokens/cost.
                                            if let Some(r) = rewriter.as_mut() {
                                                r.accumulate_suppressed_usage(&event);
                                            }
                                            if is_terminal_lifecycle(&event) {
                                                // Keep the final terminal so an
                                                // error/abort path below can
                                                // re-emit it through the rewriter
                                                // rather than forwarding raw
                                                // provider bytes. Its own usage
                                                // was just folded into the
                                                // carried total, which that path
                                                // folds back in — strip it so
                                                // the turn isn't counted twice.
                                                suppressed_terminal =
                                                    Some(strip_response_usage(event.clone()));
                                                // Surface the running loop total
                                                // at the turn boundary when the
                                                // caller opted in via `include:
                                                // ["usage.incremental"]`.
                                                if emit_usage_updates
                                                    && let Some(update) = rewriter
                                                        .as_ref()
                                                        .and_then(|r| r.carried_usage.as_ref())
                                                        .map(format_usage_updated_event)
                                                    && let Some(out) =
                                                        finalize_event(&mut rewriter, update)
                                                    && tx.send(Ok(out)).await.is_err()
                                                {
                                                    return; // client disconnected
                                                }
                                            }
                                            continue;
                                        }
                                    }

                                    let to_send = apply_transforms(&enabled_tools, event);
                                    if let Some(out) = finalize_event(&mut rewriter, to_send)
                                        && tx.send(Ok(out)).await.is_err()
                                    {
                                        return; // client disconnected
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(
                                    stage = "stream_error",
                                    error = %e,
                                    iteration = iteration,
                                    "Error reading stream chunk"
                                );
                                let _ = tx.send(Err(std::io::Error::other(e))).await;
                                return;
                            }
                        }
                    }

                    // Flush any trailing partial event.
                    if !sse_buffer.is_empty() {
                        let remaining = sse_buffer.take_remaining();
                        if !remaining.is_empty()
                            && !is_done_sentinel(&remaining)
                            && (detected.is_empty() || !has_callback)
                        {
                            let to_send = apply_transforms(&enabled_tools, remaining);
                            if let Some(out) = finalize_event(&mut rewriter, to_send)
                                && tx.send(Ok(out)).await.is_err()
                            {
                                return;
                            }
                        }
                    }

                    if at_iteration_limit {
                        warn!(
                            stage = "iteration_limit_reached",
                            iteration = iteration,
                            max_iterations = max_iterations,
                            "Maximum server-tool iterations exceeded; forwarding final response"
                        );
                        record_server_tool_iteration(
                            iteration as u32,
                            true,
                            "limit_reached",
                            &tool_names,
                        );
                        break;
                    }

                    if detected.is_empty() {
                        debug!(
                            stage = "stream_completed",
                            iteration = iteration,
                            "No tool calls detected; stream complete"
                        );
                        record_server_tool_iteration(
                            iteration as u32,
                            true,
                            "completed",
                            &tool_names,
                        );
                        break;
                    }

                    // Execute all detected calls in parallel, interleaving
                    // their progress events into the client stream.
                    let mut exec_handles = FuturesUnordered::new();
                    for call in detected.drain(..) {
                        let Some(tool) = tool_by_name.get(call.tool_name).cloned() else {
                            error!(
                                stage = "unknown_tool",
                                tool = call.tool_name,
                                "Detected call references unregistered tool; skipping"
                            );
                            continue;
                        };
                        let ctx = ctx.clone();
                        let call_id = call.call_id.clone();
                        let tool_name = call.tool_name;
                        exec_handles.push(async move {
                            let handle = tool.execute(call, &ctx).await;
                            (tool_name, call_id, handle)
                        });
                    }

                    // results_by_tool[tool_name] = Vec<ToolCallResult>
                    let mut results_by_tool: HashMap<&'static str, Vec<ToolCallResult>> =
                        HashMap::new();
                    let mut had_failure = false;

                    while let Some((tool_name, call_id, handle)) = exec_handles.next().await {
                        let handle = match handle {
                            Ok(h) => h,
                            Err(e) => {
                                error!(
                                    stage = "execute_failed",
                                    tool = tool_name,
                                    call_id = %call_id,
                                    error = %e,
                                    "Tool execute() returned error"
                                );
                                had_failure = true;
                                continue;
                            }
                        };

                        let mut events = handle.events;
                        while let Some(event) = events.next().await {
                            let to_send = apply_transforms(&enabled_tools, event);
                            if let Some(out) = finalize_event(&mut rewriter, to_send)
                                && tx.send(Ok(out)).await.is_err()
                            {
                                return;
                            }
                        }

                        match handle.result.await {
                            Ok(result) => {
                                results_by_tool.entry(tool_name).or_default().push(result);
                            }
                            Err(e) => {
                                error!(
                                    stage = "result_failed",
                                    tool = tool_name,
                                    call_id = %call_id,
                                    error = %e,
                                    "Tool result returned error"
                                );
                                had_failure = true;
                            }
                        }
                    }

                    if had_failure {
                        // A tool call failed; stop the loop and emit a final
                        // terminal. With the rewriter on we re-emit the turn's
                        // suppressed terminal through it — so the client gets a
                        // normalized terminal with the reconstructed
                        // `response.output` (synthesized `mcp_*` items included),
                        // stable id, collapsed `tools`, and folded usage —
                        // instead of the provider's raw, un-normalized bytes.
                        // With the rewriter off (file_search/web_search/shell)
                        // we forward the raw accumulated tail as before.
                        match emit_final_terminal(
                            &mut rewriter,
                            &tx,
                            suppressed_terminal.take(),
                            accumulated,
                        )
                        .await
                        {
                            Ok(forwarded) => raw_tail_forwarded = forwarded,
                            Err(()) => return,
                        }
                        record_server_tool_iteration(iteration as u32, true, "error", &tool_names);
                        break;
                    }

                    // A tool asked to end the turn here (the MCP approval gate
                    // sets `stop_loop` when it parks a call). Don't send a
                    // continuation — that would prompt the model for another
                    // turn after the `mcp_approval_request`, and the OpenAI
                    // Responses spec stops the turn at the approval request.
                    // Re-emit the suppressed terminal through the rewriter so
                    // `response.output` ends at the synthesized items the client
                    // saw (e.g. `mcp_approval_request`), exactly as the
                    // failure/abort paths do. See `ToolCallResult::stop_loop`.
                    let stop_requested = results_by_tool.values().flatten().any(|r| r.stop_loop);
                    if stop_requested {
                        match emit_final_terminal(
                            &mut rewriter,
                            &tx,
                            suppressed_terminal.take(),
                            accumulated,
                        )
                        .await
                        {
                            Ok(forwarded) => raw_tail_forwarded = forwarded,
                            Err(()) => return,
                        }
                        record_server_tool_iteration(
                            iteration as u32,
                            true,
                            "stop_requested",
                            &tool_names,
                        );
                        break;
                    }

                    // Build the continuation payload by letting each tool
                    // fold its results in.
                    let Some(ref callback) = provider_callback else {
                        // No callback: forward what we have and stop. The raw
                        // tail still carries this turn's `[DONE]`.
                        if tx.send(Ok(accumulated.freeze())).await.is_err() {
                            return;
                        }
                        raw_tail_forwarded = true;
                        record_server_tool_iteration(
                            iteration as u32,
                            true,
                            "no_callback",
                            &tool_names,
                        );
                        break;
                    };

                    let is_final_iteration = iteration == max_iterations;
                    // The continuation payload accumulates across
                    // iterations: each turn it grows by the assistant
                    // items the upstream emitted plus the function-call
                    // outputs this turn's tools produced. Pairing the
                    // assistant's function_call items with their
                    // corresponding function_call_output items is what
                    // lets non-OpenAI providers (e.g. Anthropic via
                    // OpenRouter) reconstruct valid tool_use/tool_result
                    // pairs on the wire.
                    normalize_input_to_items(&mut continuation_payload);
                    if let Some(ResponsesInput::Items(ref mut items)) = continuation_payload.input {
                        items.append(&mut captured_assistant_items);
                    }
                    for tool in &enabled_tools {
                        if let Some(results) = results_by_tool.get(tool.name()) {
                            tool.apply_to_continuation(
                                &mut continuation_payload,
                                results,
                                is_final_iteration,
                            );
                        }
                    }
                    let mut continuation_payload_for_call = continuation_payload.clone();
                    continuation_payload_for_call.stream = true;

                    info!(
                        stage = "continuation_sent",
                        iteration = iteration,
                        is_final_iteration = is_final_iteration,
                        tools_with_results = results_by_tool.len(),
                        "Sending continuation request to provider"
                    );

                    record_server_tool_iteration(
                        iteration as u32,
                        false,
                        "continuation",
                        &tool_names,
                    );

                    match callback(continuation_payload_for_call).await {
                        Ok(continuation_response) => {
                            let (_, new_body) = continuation_response.into_parts();
                            current_body = new_body;
                            continue;
                        }
                        Err(e) => {
                            error!(
                                stage = "continuation_failed",
                                iteration = iteration,
                                error = %e,
                                "Provider continuation request failed"
                            );
                            // Same as the `had_failure` path: re-emit the prior
                            // turn's terminal through the rewriter (so the
                            // already-streamed synthesized items survive in the
                            // reconstructed output) rather than dumping raw bytes.
                            match emit_final_terminal(
                                &mut rewriter,
                                &tx,
                                suppressed_terminal.take(),
                                accumulated,
                            )
                            .await
                            {
                                Ok(forwarded) => raw_tail_forwarded = forwarded,
                                Err(()) => return,
                            }
                            record_server_tool_iteration(
                                iteration as u32,
                                true,
                                "continuation_error",
                                &tool_names,
                            );
                            break;
                        }
                    }
                }

                // Emit exactly one terminal `[DONE]` for the merged stream.
                // Per-turn sentinels were swallowed above; the fallback paths
                // that dumped raw bytes set `raw_tail_forwarded` since their
                // tail already carries a `[DONE]`.
                if !raw_tail_forwarded {
                    let _ = tx.send(Ok(Bytes::from_static(b"data: [DONE]\n\n"))).await;
                }

                debug!(
                    stage = "processing_completed",
                    "Tool loop processing complete"
                );
            }
            .instrument(span),
        );

        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        let body = Body::from_stream(stream);
        Response::from_parts(parts, body)
    }
}

fn apply_transforms(tools: &[Arc<dyn ServerExecutedTool>], event: Bytes) -> Bytes {
    let mut out = event;
    for t in tools {
        out = t.transform_event(out);
    }
    out
}

/// Drop suppressed (empty) events and run the optional [`StreamRewriter`].
/// A tool's `transform_event` returns empty bytes to suppress an event
/// (the MCP executor hides the rewritten function-call plumbing this
/// way); those are skipped here. Returns `None` when nothing should be
/// forwarded.
fn finalize_event(rewriter: &mut Option<StreamRewriter>, event: Bytes) -> Option<Bytes> {
    if event.is_empty() {
        return None;
    }
    match rewriter {
        // The rewriter returns empty bytes for a suppressed event (a
        // duplicate lifecycle start); drop those rather than forwarding a
        // blank chunk.
        Some(r) => {
            let out = r.rewrite(event);
            (!out.is_empty()).then_some(out)
        }
        None => Some(event),
    }
}

/// Extract the payload `type` of a single SSE event, if it carries a
/// JSON `data:` line with a string `type` field.
fn event_type_of(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data:").map(str::trim))?;
    if data == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    value
        .get("type")
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

/// Single-stream normalizer for the runner's forwarded SSE events.
///
/// The runner multiplexes three event sources into one client stream:
/// tool prefix events, the provider's per-turn streams, and the
/// tool-synthesized execution events. Each source numbers its own
/// `sequence_number` / `output_index` from zero, so without
/// normalization they collide. This rewriter assigns a single monotonic
/// `sequence_number` to every event and a stable `output_index` per
/// item id, then reconstructs the terminal `response.output` from the
/// `output_item.done` items it actually forwarded — so the persisted /
/// retrieved response carries exactly the items the client saw streamed
/// (e.g. `mcp_list_tools` / `mcp_call`), not the provider's last-turn
/// view.
struct StreamRewriter {
    seq: u64,
    next_output_index: u64,
    /// slot key (the item id, or `(type, id)` for the hosted-shell pair) →
    /// assigned `output_index`, so an item's added/done/delta events share
    /// one slot. See [`Self::index_for`] for why the shell pair is qualified.
    item_index: HashMap<String, u64>,
    /// Output items captured in forward order, for terminal-output
    /// reconstruction.
    output_items: Vec<serde_json::Value>,
    /// `(item type, item id)` → position in `output_items`, to dedupe a
    /// re-emitted item. Keyed on type *and* id because a `shell_call` and
    /// its `shell_call_output` deliberately share one id (the model's
    /// `call_id` doubles as the item id); keying on id alone let the
    /// second-emitted item overwrite the first, silently dropping the tool
    /// result from the persisted `response.output` and corrupting
    /// `previous_response_id` replay.
    output_pos: HashMap<(String, String), usize>,
    /// Stable Hadrian `resp_…` id stamped onto every lifecycle event's
    /// `response.id`, replacing the provider's per-turn id (e.g.
    /// OpenRouter's `gen-…`). Matches the persisted/retrievable id so a
    /// streaming client sees one stable id across the whole tool loop.
    /// `None` when the response isn't persisted — the provider id passes
    /// through unchanged.
    response_id: Option<String>,
    /// Whether a `response.created` / `response.in_progress` has already
    /// been forwarded. The server-tool loop concatenates one provider
    /// stream per turn, each opening with its own start events; only the
    /// first of each may reach the client so the loop reads as a single
    /// response.
    seen_created: bool,
    seen_in_progress: bool,
    /// Upstream item id → normalized stable id. Providers may hand back
    /// placeholder ids (e.g. OpenRouter's `rs_tmp_…` / `msg_tmp_…`); these
    /// are rewritten to clean, stable ids consistently across every event
    /// that references the item.
    id_map: HashMap<String, String>,
    /// One-shot tool prefix events (the MCP `mcp_list_tools` snapshot)
    /// held back until after `response.created` / `response.in_progress`
    /// so the stream opens with the lifecycle events the spec requires
    /// first, not the catalog.
    deferred_prefix: Vec<Bytes>,
    prefix_flushed: bool,
    /// `(function-name prefix, original tool JSON)` pairs for collapsing
    /// rewritten MCP function tools back into the caller's original `mcp`
    /// entry on the echoed `response.tools`. Empty when no MCP rewrite ran.
    mcp_tool_echo: Vec<(String, serde_json::Value)>,
    /// Running sum of the `usage` carried on each *suppressed* intermediate
    /// turn's terminal event. The runner drops those events from the client
    /// stream, so without this their tokens/cost would be lost; the total is
    /// folded into the final terminal event's own usage by [`Self::rewrite_one`]
    /// so the streamed/persisted/billed usage reflects the whole loop.
    carried_usage: Option<ResponsesUsage>,
}

impl StreamRewriter {
    fn new(response_id: Option<String>, mcp_tool_echo: Vec<(String, serde_json::Value)>) -> Self {
        Self {
            seq: 0,
            next_output_index: 0,
            item_index: HashMap::new(),
            output_items: Vec::new(),
            output_pos: HashMap::new(),
            response_id,
            seen_created: false,
            seen_in_progress: false,
            id_map: HashMap::new(),
            deferred_prefix: Vec::new(),
            prefix_flushed: false,
            mcp_tool_echo,
            carried_usage: None,
        }
    }

    /// Fold a *suppressed* intermediate turn's `response.usage` into the
    /// carried total. The runner calls this for terminal events it drops from
    /// the client stream (so their usage isn't lost); events without a
    /// `response.usage` (e.g. `response.created` / `response.in_progress`)
    /// no-op. Only effective with [`ToolLoopRunner::rewrite_output`], which the
    /// `/v1/responses` pipeline always enables for multi-turn loops.
    fn accumulate_suppressed_usage(&mut self, event: &[u8]) {
        let Ok(text) = std::str::from_utf8(event) else {
            return;
        };
        let Some(data) = text
            .lines()
            .find_map(|line| line.strip_prefix("data:").map(str::trim))
        else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        let Some(usage_val) = value.get("response").and_then(|r| r.get("usage")) else {
            return;
        };
        let Ok(usage) = serde_json::from_value::<ResponsesUsage>(usage_val.clone()) else {
            return;
        };
        match self.carried_usage.as_mut() {
            Some(acc) => acc.accumulate(&usage),
            None => self.carried_usage = Some(usage),
        }
    }

    /// Collapse rewritten MCP function tools on a lifecycle event's
    /// `response.tools` array back into the caller's original `mcp` entry.
    /// Under `hadrian_hosted` the request's `mcp` tool is expanded into N
    /// `mcp_<label>__<tool>` function tools before reaching the provider, so
    /// the provider echoes those internal functions. This replaces the first
    /// function tool of each label's run with the original `mcp` entry and
    /// drops the rest, leaving every non-MCP tool untouched — so the echoed
    /// `tools` match what the caller sent, per the OpenAI spec.
    fn restore_mcp_tools(&self, tools: &mut serde_json::Value) {
        if self.mcp_tool_echo.is_empty() {
            return;
        }
        let Some(arr) = tools.as_array() else {
            return;
        };
        let mut out: Vec<serde_json::Value> = Vec::with_capacity(arr.len());
        let mut emitted: Vec<&str> = Vec::new();
        for tool in arr {
            let fn_name = (tool.get("type").and_then(|t| t.as_str()) == Some("function"))
                .then(|| tool.get("name").and_then(|n| n.as_str()))
                .flatten();
            let matched = fn_name.and_then(|name| {
                self.mcp_tool_echo
                    .iter()
                    .find(|(prefix, _)| name.starts_with(prefix.as_str()))
            });
            match matched {
                Some((prefix, original)) => {
                    // Emit the original `mcp` entry once per label, at the
                    // position of its first function tool; skip the rest.
                    if !emitted.contains(&prefix.as_str()) {
                        emitted.push(prefix.as_str());
                        out.push(original.clone());
                    }
                }
                None => out.push(tool.clone()),
            }
        }
        *tools = serde_json::Value::Array(out);
    }

    /// Hold a one-shot prefix event back until the lifecycle start events
    /// have been forwarded. Flushed by [`Self::flush_prefix`].
    fn defer_prefix(&mut self, event: Bytes) {
        self.deferred_prefix.push(event);
    }

    /// Normalize an upstream item id to a stable id. Placeholder ids
    /// carrying a `_tmp_` marker are rewritten to `<prefix>_<token>`
    /// (preserving the `rs`/`msg`/… type prefix); already-clean ids pass
    /// through. Mapping is memoized so every event referencing the item
    /// (added / delta / done / content_part) shares one id.
    fn normalize_id(&mut self, raw: &str) -> String {
        if let Some(existing) = self.id_map.get(raw) {
            return existing.clone();
        }
        let normalized = match raw.find("_tmp_") {
            Some(idx) => format!("{}_{}", &raw[..idx], uuid::Uuid::new_v4().simple()),
            None => raw.to_string(),
        };
        self.id_map.insert(raw.to_string(), normalized.clone());
        normalized
    }

    /// Append every deferred prefix event (rewritten through the normal
    /// path) into `buf`, marking the prefix flushed.
    fn flush_prefix(&mut self, buf: &mut Vec<u8>) {
        if self.prefix_flushed {
            return;
        }
        self.prefix_flushed = true;
        for event in std::mem::take(&mut self.deferred_prefix) {
            buf.extend_from_slice(&self.rewrite_one(event));
        }
    }

    /// Rewrite one SSE event, applying lifecycle de-duplication and
    /// prefix ordering on top of the core [`Self::rewrite_one`] transform.
    /// Returns empty bytes for a suppressed event (a duplicate lifecycle
    /// start), or one-or-more concatenated SSE events when deferred prefix
    /// events are flushed alongside this one.
    fn rewrite(&mut self, event: Bytes) -> Bytes {
        let etype = event_type_of(&event);
        // De-dupe the per-turn lifecycle start events: forward only the
        // first `response.created` / `response.in_progress`.
        match etype.as_deref() {
            Some("response.created") => {
                if self.seen_created {
                    return Bytes::new();
                }
                self.seen_created = true;
            }
            Some("response.in_progress") => {
                if self.seen_in_progress {
                    return Bytes::new();
                }
                self.seen_in_progress = true;
            }
            _ => {}
        }

        let is_start = matches!(
            etype.as_deref(),
            Some("response.created" | "response.in_progress")
        );
        // Keep-alive heartbeats can arrive before `response.created` (the
        // pipeline emits them while waiting on the upstream's first byte);
        // they must not trigger the prefix flush, or the catalog would
        // precede `response.created`.
        let is_keep_alive = matches!(etype.as_deref(), Some("response.keep_alive"));
        let mut buf = Vec::new();
        // Fallback flush before the first real (non-start, non-heartbeat)
        // event, but only once `response.created` has been forwarded — so
        // the catalog can never lead the stream even if `in_progress` is
        // absent.
        if !is_start && !is_keep_alive && self.seen_created {
            self.flush_prefix(&mut buf);
        }
        buf.extend_from_slice(&self.rewrite_one(event));
        // Primary: emit the prefix right after `response.in_progress` (the
        // spec's first post-lifecycle slot).
        if matches!(etype.as_deref(), Some("response.in_progress")) {
            self.flush_prefix(&mut buf);
        }
        Bytes::from(buf)
    }

    /// Core transform for one complete SSE event: normalize sequence
    /// number, output index, item ids and `response.id`, accumulate the
    /// terminal output, and re-frame with an `event:` line. Non-JSON
    /// events (`[DONE]`, comments) and unparseable payloads pass through
    /// untouched. Preserves any non-`data:` framing lines already present.
    fn rewrite_one(&mut self, event: Bytes) -> Bytes {
        let Ok(text) = std::str::from_utf8(&event) else {
            return event;
        };
        // Split SSE framing: keep non-`data:` field lines verbatim, join
        // the `data:` payloads (one event may carry several).
        let mut prefix_lines: Vec<&str> = Vec::new();
        let mut data_parts: Vec<&str> = Vec::new();
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                data_parts.push(rest.strip_prefix(' ').unwrap_or(rest));
            } else {
                prefix_lines.push(line);
            }
        }
        if data_parts.is_empty() {
            return event;
        }
        let data = data_parts.join("\n");
        if data.trim() == "[DONE]" {
            return event;
        }
        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&data) else {
            return event;
        };
        let Some(obj) = json.as_object_mut() else {
            return event;
        };
        let Some(event_type) = obj.get("type").and_then(|t| t.as_str()).map(str::to_string) else {
            return event;
        };

        // (1) One monotonic sequence_number across the whole stream.
        obj.insert(
            "sequence_number".into(),
            serde_json::Value::from(self.next_seq()),
        );

        // (1a) Stamp the stable response id onto lifecycle events,
        // replacing the provider's per-turn id so the client sees one id.
        if let Some(ref rid) = self.response_id
            && let Some(resp) = obj.get_mut("response").and_then(|r| r.as_object_mut())
            && resp.contains_key("id")
        {
            resp.insert("id".into(), serde_json::Value::from(rid.clone()));
        }

        // (1c) Collapse rewritten MCP function tools on the echoed
        // `response.tools` back into the caller's original `mcp` entry so the
        // client never sees the internal `mcp_<label>__<tool>` expansion.
        if let Some(tools) = obj
            .get_mut("response")
            .and_then(|r| r.as_object_mut())
            .and_then(|r| r.get_mut("tools"))
        {
            self.restore_mcp_tools(tools);
        }

        // (1b) Normalize placeholder item ids (`rs_tmp_…`, `msg_tmp_…`)
        // to stable ids, consistently across `item.id` and `item_id`.
        if let Some(raw) = obj
            .get("item")
            .and_then(|i| i.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
        {
            let norm = self.normalize_id(&raw);
            if let Some(item) = obj.get_mut("item").and_then(|i| i.as_object_mut()) {
                item.insert("id".into(), serde_json::Value::from(norm));
            }
        }
        if let Some(raw) = obj
            .get("item_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        {
            let norm = self.normalize_id(&raw);
            obj.insert("item_id".into(), serde_json::Value::from(norm));
        }

        // (2) Stable output_index per item id. Only events that already
        // carry an output_index are remapped.
        if obj.contains_key("output_index") {
            let id = obj
                .get("item")
                .and_then(|i| i.get("id"))
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("item_id").and_then(|v| v.as_str()))
                .map(str::to_string);
            if let Some(id) = id {
                let item_type = obj
                    .get("item")
                    .and_then(|i| i.get("type"))
                    .and_then(|v| v.as_str());
                let idx = self.index_for(item_type, &id);
                obj.insert("output_index".into(), serde_json::Value::from(idx));
            }
        }

        // (3) Accumulate output items, and on the terminal event splice
        // the reconstructed array into `response.output`.
        if event_type == "response.output_item.done" {
            if let Some(item) = obj.get("item").cloned() {
                self.record_output_item(item);
            }
        } else if matches!(
            event_type.as_str(),
            "response.completed" | "response.failed" | "response.incomplete"
        ) && !self.output_items.is_empty()
            && let Some(resp) = obj.get_mut("response").and_then(|r| r.as_object_mut())
        {
            resp.insert(
                "output".into(),
                serde_json::Value::Array(self.ordered_output_items()),
            );
        }

        // (4) Fold the usage carried from suppressed intermediate turns into
        // the final terminal event's own usage, so the streamed/persisted/
        // billed usage reflects the whole loop, not just the last turn. Only
        // the final terminal reaches the rewriter (intermediates are
        // suppressed), so this fires at most once.
        if matches!(
            event_type.as_str(),
            "response.completed" | "response.failed" | "response.incomplete"
        ) && let Some(carried) = self.carried_usage.as_ref()
            && let Some(resp) = obj.get_mut("response").and_then(|r| r.as_object_mut())
        {
            let mut total = carried.clone();
            // The final turn's own usage is the larger base; fold it in (or
            // emit the carried total alone when the final event omits usage).
            if let Some(own) = resp
                .get("usage")
                .and_then(|u| serde_json::from_value::<ResponsesUsage>(u.clone()).ok())
            {
                total.accumulate(&own);
            }
            if let Ok(value) = serde_json::to_value(&total) {
                resp.insert("usage".into(), value);
            }
        }

        let body = serde_json::to_string(&json).unwrap_or_default();
        let mut out = String::with_capacity(body.len() + prefix_lines.len() * 8 + 32);
        // Re-frame with an `event:` line matching the payload `type` (the
        // spec emits both `event:` and `data:`). Skip if the upstream
        // already supplied one in the preserved framing lines.
        let has_event_line = prefix_lines.iter().any(|l| l.starts_with("event:"));
        if !has_event_line {
            out.push_str("event: ");
            out.push_str(&event_type);
            out.push('\n');
        }
        for line in prefix_lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("data: ");
        out.push_str(&body);
        out.push_str("\n\n");
        Bytes::from(out)
    }

    fn next_seq(&mut self) -> u64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    /// The `output_index` for an item, stable across its lifecycle events.
    ///
    /// Keyed on the item `id`, which is normally unique — and *must* stay the
    /// key for delta events (`output_text.delta`, `function_call_arguments.delta`,
    /// …) that reference their item by `item_id` alone (no `type`), so they
    /// land on the slot their `output_item.added`/`done` already claimed.
    ///
    /// The sole exception is the hosted-shell pair: a `shell_call` and its
    /// `shell_call_output` deliberately share one id (the model's `call_id`
    /// doubles as the item id), so keying on id alone collapses them onto a
    /// single `output_index` — a streaming client tracking items by slot then
    /// renders only the last-arriving one. Those two types ride exclusively on
    /// full `output_item.added`/`done` events (never deltas), so qualifying
    /// just them by `(type, id)` splits the pair into two slots safely, and
    /// (the call's added arrives first) in call-before-output order to match
    /// the terminal reordering. Mirrors the `(type, id)` keying
    /// [`Self::record_output_item`] uses for `output_pos`.
    fn index_for(&mut self, item_type: Option<&str>, id: &str) -> u64 {
        let key = match item_type {
            Some(t @ ("shell_call" | "shell_call_output")) => format!("{t}\u{0}{id}"),
            _ => id.to_string(),
        };
        if let Some(i) = self.item_index.get(&key) {
            return *i;
        }
        let i = self.next_output_index;
        self.next_output_index += 1;
        self.item_index.insert(key, i);
        i
    }

    fn record_output_item(&mut self, item: serde_json::Value) {
        if let Some(id) = item.get("id").and_then(|v| v.as_str()).map(str::to_string) {
            let kind = item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let key = (kind, id);
            if let Some(&pos) = self.output_pos.get(&key) {
                self.output_items[pos] = item;
                return;
            }
            self.output_pos.insert(key, self.output_items.len());
        }
        self.output_items.push(item);
    }

    /// The captured output items for the persisted `response.output`, with
    /// every `shell_call_output` reordered to follow its paired `shell_call`.
    /// See [`crate::services::shell_tool::order_shell_outputs_after_calls`] for
    /// why this matters for `previous_response_id` replay.
    fn ordered_output_items(&self) -> Vec<serde_json::Value> {
        crate::services::shell_tool::order_shell_outputs_after_calls(self.output_items.clone())
    }
}

/// True iff `event` is the SSE `[DONE]` end-of-stream sentinel
/// (`data: [DONE]`), ignoring framing whitespace.
fn is_done_sentinel(event: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(event) else {
        return false;
    };
    text.lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
        .any(|data| data == "[DONE]")
}

/// True for SSE events that mark a turn boundary: the start
/// (`response.created` / `response.in_progress`) or end
/// (`response.completed` / `response.failed` / `response.incomplete`)
/// of one upstream stream. The runner holds these back across
/// intermediate iterations so the client sees one coherent timeline,
/// not N concatenated mini-streams.
/// True for a *final* response lifecycle event (`response.completed` /
/// `response.failed` / `response.incomplete`) — the subset of
/// [`is_iteration_terminator`] events that close out a turn. Used to
/// stash the suppressed terminal of a tool-calling turn so a later
/// failure/abort path can re-emit it through the [`StreamRewriter`].
fn is_terminal_lifecycle(event: &[u8]) -> bool {
    matches!(
        event_type_of(event).as_deref(),
        Some("response.completed" | "response.failed" | "response.incomplete")
    )
}

/// Serialize the cumulative tool-loop usage as a `response.usage.updated`
/// SSE event — a **Hadrian Extension** emitted at each suppressed turn
/// boundary when the caller opted in via `include: ["usage.incremental"]`,
/// so streaming clients can watch tokens/cost accrue across the loop
/// instead of only seeing the total on the terminal event. The placeholder
/// `sequence_number` and the `event:` framing line are stamped by the
/// [`StreamRewriter`].
fn format_usage_updated_event(usage: &ResponsesUsage) -> Bytes {
    let payload = serde_json::json!({
        "type": "response.usage.updated",
        "sequence_number": 0,
        "usage": usage,
    });
    let s = serde_json::to_string(&payload).unwrap_or_default();
    Bytes::from(format!("data: {s}\n\n"))
}

/// Remove `response.usage` from a suppressed terminal event before it is
/// stashed for the failure/abort re-emission path. The turn's usage has
/// already been folded into the carried total by
/// [`StreamRewriter::accumulate_suppressed_usage`]; the re-emission path
/// folds the carried total into the event's own usage, so keeping the
/// event's copy would count the turn twice. Events without a parseable
/// `response.usage` pass through unchanged.
fn strip_response_usage(event: Bytes) -> Bytes {
    let Ok(text) = std::str::from_utf8(&event) else {
        return event;
    };
    let mut prefix_lines: Vec<&str> = Vec::new();
    let mut data_parts: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        match line.strip_prefix("data:") {
            Some(rest) => data_parts.push(rest.strip_prefix(' ').unwrap_or(rest)),
            None => prefix_lines.push(line),
        }
    }
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&data_parts.join("\n")) else {
        return event;
    };
    let stripped = json
        .get_mut("response")
        .and_then(|r| r.as_object_mut())
        .and_then(|r| r.remove("usage"));
    if stripped.is_none() {
        return event;
    }
    let body = serde_json::to_string(&json).unwrap_or_default();
    let mut out = String::with_capacity(body.len() + 32);
    for line in prefix_lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("data: ");
    out.push_str(&body);
    out.push_str("\n\n");
    Bytes::from(out)
}

/// Emit the final terminal when the loop ends without a fresh provider
/// turn — a failure/abort path, or a tool-requested clean stop (the MCP
/// approval gate's `stop_loop`). In the stop case the suppressed terminal
/// is the provider's `response.completed`, so this emits a completed
/// terminal; on failure paths it carries whatever terminal was suppressed.
///
/// With the rewriter on we re-emit the turn's suppressed terminal
/// through it: this normalizes ids/sequence numbers, reconstructs
/// `response.output` from the items actually forwarded (so the
/// already-streamed synthesized `mcp_*` items survive), collapses the
/// rewritten MCP function tools back to the original `mcp` entry, and
/// folds in the carried usage. Falls back to forwarding the raw
/// `accumulated` tail when the rewriter is off (file_search /
/// web_search / shell keep the provider stream verbatim) or when no
/// terminal was captured (the upstream sent none).
///
/// Returns `Ok(raw_tail_forwarded)` — `true` only on the raw fallback,
/// whose tail already carries a `[DONE]` so the epilogue must not emit a
/// second one — or `Err(())` if the client disconnected.
async fn emit_final_terminal(
    rewriter: &mut Option<StreamRewriter>,
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
    suppressed_terminal: Option<Bytes>,
    accumulated: BytesMut,
) -> Result<bool, ()> {
    if rewriter.is_some()
        && let Some(terminal) = suppressed_terminal
    {
        if let Some(out) = finalize_event(rewriter, terminal)
            && tx.send(Ok(out)).await.is_err()
        {
            return Err(());
        }
        // The terminal went through the rewriter; the single terminal
        // `[DONE]` is emitted by the post-loop epilogue.
        return Ok(false);
    }
    // Fallback: forward the raw accumulated tail (carries its own `[DONE]`).
    if tx.send(Ok(accumulated.freeze())).await.is_err() {
        return Err(());
    }
    Ok(true)
}

fn is_iteration_terminator(event: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(event) else {
        return false;
    };
    let Some(data) = text
        .lines()
        .find_map(|line| line.strip_prefix("data:").map(str::trim))
    else {
        return false;
    };
    if data == "[DONE]" {
        return false;
    }
    let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(data) else {
        return false;
    };
    matches!(
        value.get("type").and_then(|t| t.as_str()),
        Some(
            "response.created"
                | "response.in_progress"
                | "response.completed"
                | "response.failed"
                | "response.incomplete"
        )
    )
}

/// Inspect one SSE event and extract the assistant item it carries,
/// if any. Returns `Some(item)` for `response.output_item.done` events
/// whose `item` is a model-emitted `message`, `function_call`, or
/// `reasoning`. Gateway-synthesized items (`shell_call_output`,
/// `web_search_call`, `file_search_call`) are skipped — tools fold
/// their own continuation items in via `apply_to_continuation`.
fn parse_assistant_item(event: &[u8]) -> Option<ResponsesInputItem> {
    let text = std::str::from_utf8(event).ok()?;
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data:").map(str::trim))?;
    if data == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    if value.get("type").and_then(|t| t.as_str()) != Some("response.output_item.done") {
        return None;
    }
    let item = value.get("item")?;
    match item.get("type").and_then(|t| t.as_str())? {
        "message" => serde_json::from_value::<OutputMessage>(item.clone())
            .ok()
            .map(ResponsesInputItem::OutputMessage),
        "function_call" => serde_json::from_value::<OutputItemFunctionCall>(item.clone())
            .ok()
            .map(ResponsesInputItem::OutputFunctionCall),
        "reasoning" => serde_json::from_value::<ResponsesReasoning>(item.clone())
            .ok()
            .map(ResponsesInputItem::Reasoning),
        _ => None,
    }
}

/// Ensure `payload.input` is `Items` so callers can append to it.
/// A `Text` input is rewrapped as a single user `EasyMessage`; `None`
/// becomes an empty `Items` vec.
fn normalize_input_to_items(payload: &mut CreateResponsesPayload) {
    match payload.input.take() {
        Some(ResponsesInput::Items(items)) => {
            payload.input = Some(ResponsesInput::Items(items));
        }
        Some(ResponsesInput::Text(text)) => {
            payload.input = Some(ResponsesInput::Items(vec![
                ResponsesInputItem::EasyMessage(EasyInputMessage {
                    type_: None,
                    role: EasyInputMessageRole::User,
                    content: EasyInputMessageContent::Text(text),
                }),
            ]));
        }
        None => {
            payload.input = Some(ResponsesInput::Items(Vec::new()));
        }
    }
}

#[cfg(test)]
mod rewriter_tests {
    use super::*;

    fn data(event: &Bytes) -> serde_json::Value {
        let text = std::str::from_utf8(event).unwrap();
        let line = text
            .lines()
            .find_map(|l| l.strip_prefix("data:").map(str::trim))
            .unwrap();
        serde_json::from_str(line).unwrap()
    }

    fn ev(json: serde_json::Value) -> Bytes {
        Bytes::from(format!("data: {}\n\n", json))
    }

    #[test]
    fn rewrites_monotonic_sequence_numbers() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // Three events that each carried their own (colliding) seq 0.
        let a = r.rewrite(ev(
            serde_json::json!({"type":"response.created","sequence_number":0}),
        ));
        let b = r.rewrite(ev(
            serde_json::json!({"type":"response.in_progress","sequence_number":0}),
        ));
        let c = r.rewrite(ev(
            serde_json::json!({"type":"response.output_text.delta","sequence_number":0,"delta":"hi"}),
        ));
        assert_eq!(data(&a)["sequence_number"], 0);
        assert_eq!(data(&b)["sequence_number"], 1);
        assert_eq!(data(&c)["sequence_number"], 2);
    }

    #[test]
    fn assigns_stable_output_index_per_item() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // Two items each claiming output_index 0; the second must get 1,
        // and the matching done/delta events for one item share its slot.
        let added0 = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"mcp_list_tools","id":"mcpl_1"}
        })));
        let added1 = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"message","id":"msg_1"}
        })));
        let delta = r.rewrite(ev(serde_json::json!({
            "type":"response.output_text.delta","output_index":0,"item_id":"msg_1","delta":"x"
        })));
        assert_eq!(data(&added0)["output_index"], 0);
        assert_eq!(data(&added1)["output_index"], 1);
        // The delta references msg_1 by item_id → same slot as added1.
        assert_eq!(data(&delta)["output_index"], 1);
    }

    #[test]
    fn shell_call_and_output_get_distinct_live_output_index() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // The shell_call and its shell_call_output share one id (the model's
        // call_id), but the live stream must still give them two distinct
        // output_index slots — keyed on (type, id) — with the call's the lower
        // one so the live order matches the terminal reordering. A following
        // message's delta must keep sharing its own (id-keyed) slot.
        let added_call = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"shell_call","id":"toolu_1","call_id":"toolu_1"}
        })));
        let added_output = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"shell_call_output","id":"toolu_1","call_id":"toolu_1"}
        })));
        // Output `done` arrives before the call `done` (the shell tool's wire
        // ordering); each must land back on the slot its `added` claimed.
        let done_output = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":0,
            "item":{"type":"shell_call_output","id":"toolu_1","call_id":"toolu_1"}
        })));
        let done_call = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":0,
            "item":{"type":"shell_call","id":"toolu_1","call_id":"toolu_1"}
        })));
        let added_msg = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"message","id":"msg_1"}
        })));
        let msg_delta = r.rewrite(ev(serde_json::json!({
            "type":"response.output_text.delta","output_index":0,"item_id":"msg_1","delta":"x"
        })));
        // Two distinct slots for the pair, call below its output.
        assert_eq!(data(&added_call)["output_index"], 0);
        assert_eq!(data(&added_output)["output_index"], 1);
        assert_eq!(data(&done_output)["output_index"], 1);
        assert_eq!(data(&done_call)["output_index"], 0);
        // The message takes the next slot after the pair, and its delta — which
        // carries only `item_id`, no `type` — still resolves to that same slot.
        assert_eq!(data(&added_msg)["output_index"], 2);
        assert_eq!(data(&msg_delta)["output_index"], 2);
    }

    #[test]
    fn reconstructs_terminal_output_from_done_items() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // A synthesized mcp_list_tools, an mcp_call, and a final message
        // all arrive as output_item.done across the (multi-turn) stream.
        r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":0,
            "item":{"type":"mcp_list_tools","id":"mcpl_1","server_label":"x","tools":[]}
        })));
        r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":1,
            "item":{"type":"mcp_call","id":"mcp_1","name":"t","arguments":"{}","status":"completed"}
        })));
        r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":2,
            "item":{"type":"message","id":"msg_1","role":"assistant"}
        })));
        // The provider's terminal event only knows about the last turn's
        // message; the rewriter splices in the full forwarded history.
        let terminal = r.rewrite(ev(serde_json::json!({
            "type":"response.completed",
            "response":{"id":"resp_1","output":[{"type":"message","id":"msg_1"}]}
        })));
        let out = data(&terminal);
        let items = out["response"]["output"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["type"], "mcp_list_tools");
        assert_eq!(items[1]["type"], "mcp_call");
        assert_eq!(items[2]["type"], "message");
    }

    #[test]
    fn shell_call_and_output_both_persist_in_call_then_output_order() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // A signed reasoning block, then a shell call whose OUTPUT `done` is
        // emitted before the CALL `done` (the shell tool's wire-ordering
        // invariant), then the final message. The shell_call and its
        // shell_call_output share one id (the model's call_id doubles as the
        // item id) — the regression dropped one of the pair via that shared id.
        r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":0,
            "item":{"type":"reasoning","id":"rs_1","summary":[],
                    "content":[{"type":"reasoning_text","text":"think"}],
                    "signature":"sig","format":"anthropic-claude-v1","status":"completed"}
        })));
        // output BEFORE call, both id == call_id == "toolu_1".
        r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":1,
            "item":{"type":"shell_call_output","id":"toolu_1","call_id":"toolu_1",
                    "status":"completed",
                    "output":[{"stdout":"hi\n","stderr":"",
                               "outcome":{"type":"exit","exit_code":0}}],
                    "output_files":[]}
        })));
        r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":1,
            "item":{"type":"shell_call","id":"toolu_1","call_id":"toolu_1",
                    "status":"completed","action":{"commands":["echo hi"]}}
        })));
        r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.done","output_index":2,
            "item":{"type":"message","id":"msg_1","role":"assistant"}
        })));
        let terminal = r.rewrite(ev(serde_json::json!({
            "type":"response.completed",
            "response":{"id":"resp_1","output":[{"type":"message","id":"msg_1"}]}
        })));
        let out = data(&terminal);
        let items = out["response"]["output"].as_array().unwrap();
        // Both the call and its output survive the shared id ...
        assert_eq!(items.len(), 4, "got {items:#?}");
        assert_eq!(items[0]["type"], "reasoning");
        // ... and the call is persisted BEFORE its output for valid replay.
        assert_eq!(items[1]["type"], "shell_call");
        assert_eq!(items[2]["type"], "shell_call_output");
        assert_eq!(items[1]["call_id"], "toolu_1");
        assert_eq!(items[2]["call_id"], "toolu_1");
        assert_eq!(items[3]["type"], "message");
    }

    #[test]
    fn persisted_shell_call_output_deserializes_as_output_item() {
        // `previous_response_id` replay (services/responses_chain.rs) parses
        // the stored `output` array as `Vec<ResponsesOutputItem>`; a shape that
        // fails would turn the (previously silently dropped) tool result into a
        // hard `CorruptRecord` error. Lock the round-trip for the exact JSON
        // `format_shell_call_output_item` emits.
        use crate::api_types::responses::ResponsesOutputItem;
        let item = serde_json::json!({
            "type":"shell_call_output","id":"toolu_1","call_id":"toolu_1",
            "status":"completed",
            "output":[{"stdout":"hi\n","stderr":"",
                       "outcome":{"type":"exit","exit_code":0},"created_by":"gateway"}],
            "output_files":[],
            "max_output_length":1000,
            "created_by":"gateway"
        });
        let parsed: ResponsesOutputItem =
            serde_json::from_value(item).expect("shell_call_output must parse as an output item");
        assert!(matches!(parsed, ResponsesOutputItem::ShellCallOutput(_)));
    }

    #[test]
    fn accumulates_usage_across_suppressed_turns() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // Intermediate turn (suppressed by the runner): its usage must be
        // captured even though the event never reaches the client stream.
        let intermediate = ev(serde_json::json!({
            "type": "response.completed",
            "response": {"usage": {
                "input_tokens": 100, "output_tokens": 50, "total_tokens": 150,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 10},
                "cost": 0.001
            }}
        }));
        r.accumulate_suppressed_usage(&intermediate);
        // Final turn's terminal event reaches the rewriter normally.
        let final_ev = r.rewrite(ev(serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp_1", "usage": {
                "input_tokens": 200, "output_tokens": 30, "total_tokens": 230,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 5},
                "cost": 0.002
            }}
        })));
        let usage = &data(&final_ev)["response"]["usage"];
        assert_eq!(usage["input_tokens"], 300);
        assert_eq!(usage["output_tokens"], 80);
        assert_eq!(usage["total_tokens"], 380);
        assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 15);
        assert!((usage["cost"].as_f64().unwrap() - 0.003).abs() < 1e-9);
    }

    #[test]
    fn usage_fold_emits_carried_total_when_final_omits_usage() {
        let mut r = StreamRewriter::new(None, Vec::new());
        r.accumulate_suppressed_usage(&ev(serde_json::json!({
            "type": "response.completed",
            "response": {"usage": {
                "input_tokens": 7, "output_tokens": 3, "total_tokens": 10,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 0}
            }}
        })));
        // Final terminal with no usage field still surfaces the carried total.
        let final_ev = r.rewrite(ev(serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp_1"}
        })));
        let usage = &data(&final_ev)["response"]["usage"];
        assert_eq!(usage["input_tokens"], 7);
        assert_eq!(usage["total_tokens"], 10);
    }

    #[test]
    fn detects_done_sentinel() {
        assert!(super::is_done_sentinel(b"data: [DONE]\n\n"));
        assert!(super::is_done_sentinel(b"event: done\ndata: [DONE]\n\n"));
        assert!(!super::is_done_sentinel(
            b"data: {\"type\":\"response.completed\"}\n\n"
        ));
        assert!(!super::is_done_sentinel(b": keep-alive\n\n"));
    }

    #[test]
    fn passes_through_done_sentinel_and_non_json() {
        let mut r = StreamRewriter::new(None, Vec::new());
        let done = r.rewrite(Bytes::from("data: [DONE]\n\n"));
        assert_eq!(&done[..], b"data: [DONE]\n\n");
    }

    #[test]
    fn collapses_rewritten_mcp_function_tools_on_echo() {
        // The provider echoes the internal `mcp_<label>__<tool>` function
        // tools; the rewriter must collapse each label's run back into the
        // single original `mcp` entry, leaving non-MCP tools untouched.
        let original = serde_json::json!({
            "type": "mcp",
            "server_label": "platter",
            "server_url": "http://127.0.0.1:3100/mcp",
            "require_approval": "never"
        });
        let echo = vec![("mcp_platter__".to_string(), original.clone())];
        let mut r = StreamRewriter::new(None, echo);
        let out = r.rewrite(ev(serde_json::json!({
            "type": "response.created",
            "response": {
                "id": "resp_1",
                "tools": [
                    {"type": "function", "name": "mcp_platter__read"},
                    {"type": "function", "name": "mcp_platter__bash"},
                    {"type": "function", "name": "get_weather"}
                ]
            }
        })));
        let tools = data(&out)["response"]["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 2, "two mcp functions collapse to one entry");
        assert_eq!(tools[0]["type"], "mcp");
        assert_eq!(tools[0]["server_label"], "platter");
        assert!(tools[0].get("authorization").is_none());
        // The caller's own function tool passes through unchanged.
        assert_eq!(tools[1]["type"], "function");
        assert_eq!(tools[1]["name"], "get_weather");
    }

    #[test]
    fn preserves_event_field_line() {
        let mut r = StreamRewriter::new(None, Vec::new());
        let out = r.rewrite(Bytes::from(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
        ));
        let text = std::str::from_utf8(&out).unwrap();
        assert!(text.contains("event: response.created"));
        assert_eq!(data(&out)["sequence_number"], 0);
    }

    #[test]
    fn does_not_overwrite_output_when_no_items_captured() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // No output_item.done seen → leave the provider's output intact.
        let terminal = r.rewrite(ev(serde_json::json!({
            "type":"response.completed",
            "response":{"id":"r","output":[{"type":"message","id":"m"}]}
        })));
        let items = data(&terminal)["response"]["output"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(items, 1);
    }

    #[test]
    fn dedupes_lifecycle_start_events_across_turns() {
        let mut r = StreamRewriter::new(None, Vec::new());
        // Turn 1 opens the stream.
        assert!(
            !r.rewrite(ev(serde_json::json!({"type":"response.created"})))
                .is_empty()
        );
        assert!(
            !r.rewrite(ev(serde_json::json!({"type":"response.in_progress"})))
                .is_empty()
        );
        // Turn 2 (a server-tool continuation) reopens with its own start
        // events — those must be suppressed so the loop reads as one
        // response.
        assert!(
            r.rewrite(ev(serde_json::json!({"type":"response.created"})))
                .is_empty()
        );
        assert!(
            r.rewrite(ev(serde_json::json!({"type":"response.in_progress"})))
                .is_empty()
        );
    }

    #[test]
    fn defers_prefix_until_after_in_progress() {
        let mut r = StreamRewriter::new(None, Vec::new());
        r.defer_prefix(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"mcp_list_tools","id":"mcpl_1"}
        })));
        // created leads, no prefix yet.
        let created = r.rewrite(ev(serde_json::json!({"type":"response.created"})));
        assert!(
            !std::str::from_utf8(&created)
                .unwrap()
                .contains("mcp_list_tools")
        );
        // in_progress flushes the prefix immediately after it.
        let in_progress = r.rewrite(ev(serde_json::json!({"type":"response.in_progress"})));
        let text = std::str::from_utf8(&in_progress).unwrap();
        let ip = text.find("response.in_progress").unwrap();
        let cat = text.find("mcp_list_tools").unwrap();
        assert!(ip < cat, "in_progress must precede the deferred catalog");
    }

    #[test]
    fn flushes_prefix_before_first_real_event_when_no_in_progress() {
        let mut r = StreamRewriter::new(None, Vec::new());
        r.defer_prefix(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"mcp_list_tools","id":"mcpl_1"}
        })));
        // created leads; in_progress never arrives, so the first real
        // event flushes the held-back catalog.
        r.rewrite(ev(serde_json::json!({"type":"response.created"})));
        let out = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"message","id":"msg_1"}
        })));
        let text = std::str::from_utf8(&out).unwrap();
        assert!(text.find("mcp_list_tools").unwrap() < text.find("\"message\"").unwrap());
    }

    #[test]
    fn keep_alive_before_created_does_not_leak_prefix() {
        let mut r = StreamRewriter::new(None, Vec::new());
        r.defer_prefix(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"mcp_list_tools","id":"mcpl_1"}
        })));
        // A heartbeat ahead of `response.created` must not flush the
        // catalog — `response.created` has to stay ahead of it.
        let ka = r.rewrite(ev(serde_json::json!({"type":"response.keep_alive"})));
        assert!(!std::str::from_utf8(&ka).unwrap().contains("mcp_list_tools"));
        let created = r.rewrite(ev(serde_json::json!({"type":"response.created"})));
        assert!(
            !std::str::from_utf8(&created)
                .unwrap()
                .contains("mcp_list_tools")
        );
        // in_progress finally releases it.
        let ip = r.rewrite(ev(serde_json::json!({"type":"response.in_progress"})));
        assert!(std::str::from_utf8(&ip).unwrap().contains("mcp_list_tools"));
    }

    #[test]
    fn stamps_stable_response_id_onto_lifecycle_events() {
        let mut r = StreamRewriter::new(Some("resp_stable".to_string()), Vec::new());
        let created = r.rewrite(ev(serde_json::json!({
            "type":"response.created","response":{"id":"gen-123","status":"in_progress"}
        })));
        assert_eq!(data(&created)["response"]["id"], "resp_stable");
        let completed = r.rewrite(ev(serde_json::json!({
            "type":"response.completed","response":{"id":"gen-456","status":"completed"}
        })));
        assert_eq!(data(&completed)["response"]["id"], "resp_stable");
    }

    #[test]
    fn normalizes_tmp_item_ids_consistently() {
        let mut r = StreamRewriter::new(None, Vec::new());
        let added = r.rewrite(ev(serde_json::json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"type":"message","id":"msg_tmp_abc"}
        })));
        let new_id = data(&added)["item"]["id"].as_str().unwrap().to_string();
        assert!(new_id.starts_with("msg_"), "preserves the type prefix");
        assert!(!new_id.contains("_tmp_"), "drops the placeholder marker");
        // A later event referencing the same upstream id via item_id must
        // map to the same normalized id.
        let delta = r.rewrite(ev(serde_json::json!({
            "type":"response.output_text.delta","output_index":0,"item_id":"msg_tmp_abc","delta":"x"
        })));
        assert_eq!(data(&delta)["item_id"], new_id);
    }

    #[test]
    fn synthesizes_event_field_line_when_absent() {
        let mut r = StreamRewriter::new(None, Vec::new());
        let out = r.rewrite(ev(serde_json::json!({"type":"response.created"})));
        let text = std::str::from_utf8(&out).unwrap();
        assert!(text.starts_with("event: response.created\n"));
        assert!(text.contains("data: "));
    }
}

/// Shared end-to-end loop test harness: a fake tool triggered by a
/// `faketool` function call, canned provider turns, and stream collection.
#[cfg(test)]
mod loop_test_support {
    use std::sync::{Arc, atomic::AtomicUsize};

    use async_trait::async_trait;
    use axum::{body::Body, response::Response};
    use bytes::Bytes;

    use super::*;
    use crate::services::server_tools::{
        DetectedToolCall, ProviderCallback, ServerExecutedTool, ToolCallResult, ToolContext,
        ToolError, ToolExecutionHandle,
    };

    /// Detects a `function_call` named `faketool` and, on execute, emits one
    /// synthesized output item (standing in for the gate's
    /// `mcp_approval_request`) and resolves with a result whose `stop_loop`
    /// flag is configurable.
    pub(super) struct FakeTool {
        pub(super) stop: bool,
    }

    #[async_trait]
    impl ServerExecutedTool for FakeTool {
        fn name(&self) -> &'static str {
            "faketool"
        }
        fn is_enabled_for(&self, _payload: &CreateResponsesPayload) -> bool {
            true
        }
        fn detect(&self, event: &[u8], _ctx: &ToolContext) -> Vec<DetectedToolCall> {
            let Ok(text) = std::str::from_utf8(event) else {
                return Vec::new();
            };
            let Some(data) = text
                .lines()
                .find_map(|l| l.strip_prefix("data:").map(str::trim))
            else {
                return Vec::new();
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                return Vec::new();
            };
            if v["type"] == "response.output_item.done"
                && v["item"]["type"] == "function_call"
                && v["item"]["name"] == "faketool"
            {
                let call_id = v["item"]["call_id"].as_str().unwrap_or("c1").to_string();
                return vec![DetectedToolCall::new(
                    "faketool",
                    call_id,
                    serde_json::json!({}),
                )];
            }
            Vec::new()
        }
        async fn execute(
            &self,
            call: DetectedToolCall,
            _ctx: &ToolContext,
        ) -> Result<ToolExecutionHandle, ToolError> {
            let stop = self.stop;
            let call_id = call.call_id;
            let item = Bytes::from_static(
                b"data: {\"type\":\"response.output_item.done\",\"output_index\":0,\
                  \"item\":{\"type\":\"mcp_approval_request\",\"id\":\"mcpr_x\",\
                  \"server_label\":\"s\",\"name\":\"bash\",\"arguments\":\"{}\"}}\n\n",
            );
            let events = futures_util::stream::iter(vec![item]);
            let result = ToolCallResult {
                call_id,
                continuation_items: Vec::new(),
                stop_loop: stop,
            };
            Ok(ToolExecutionHandle {
                events: Box::pin(events),
                result: Box::pin(async move { Ok(result) }),
            })
        }
        fn apply_to_continuation(
            &self,
            _payload: &mut CreateResponsesPayload,
            _results: &[ToolCallResult],
            _is_final_iteration: bool,
        ) {
        }
    }

    /// A provider turn that emits one `faketool` call then completes.
    /// `usage` (if given) rides on the terminal `response.completed`.
    pub(super) fn first_turn_body(usage: Option<serde_json::Value>) -> Response<Body> {
        let terminal = serde_json::json!({
            "type": "response.completed",
            "sequence_number": 2,
            "response": usage.map_or(serde_json::json!({}), |u| serde_json::json!({"usage": u})),
        });
        let sse = format!(
            concat!(
                "data: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{}}}}\n\n",
                "data: {{\"type\":\"response.in_progress\",\"sequence_number\":1,\"response\":{{}}}}\n\n",
                "data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{{\"type\":\"function_call\",\"name\":\"faketool\",\"call_id\":\"c1\",\"id\":\"fc_1\",\"arguments\":\"{{}}\"}}}}\n\n",
                "data: {terminal}\n\n",
                "data: [DONE]\n\n",
            ),
            terminal = terminal
        );
        Response::new(Body::from(sse))
    }

    /// Continuation callback that counts invocations and returns a trivial
    /// final turn (a message, no tool call) so a non-stop loop terminates.
    /// `usage` (if given) rides on the final turn's `response.completed`.
    pub(super) fn counting_callback(
        counter: Arc<AtomicUsize>,
        usage: Option<serde_json::Value>,
    ) -> ProviderCallback {
        Arc::new(move |_payload| {
            let counter = counter.clone();
            let usage = usage.clone();
            Box::pin(async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let terminal = serde_json::json!({
                    "type": "response.completed",
                    "sequence_number": 1,
                    "response": usage
                        .map_or(serde_json::json!({}), |u| serde_json::json!({"usage": u})),
                });
                let sse = format!(
                    concat!(
                        "data: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{}}}}\n\n",
                        "data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{{\"type\":\"output_text\",\"text\":\"done\",\"annotations\":[]}}]}}}}\n\n",
                        "data: {terminal}\n\n",
                        "data: [DONE]\n\n",
                    ),
                    terminal = terminal
                );
                Ok(Response::new(Body::from(sse)))
            })
        })
    }

    pub(super) async fn collect(resp: Response<Body>) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    pub(super) fn payload() -> CreateResponsesPayload {
        serde_json::from_value(serde_json::json!({"model":"m","stream":true})).unwrap()
    }

    /// Parse every `data:` payload (excluding `[DONE]`) in stream order.
    pub(super) fn events_of(out: &str) -> Vec<serde_json::Value> {
        out.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|d| *d != "[DONE]")
            .map(|d| serde_json::from_str(d).unwrap())
            .collect()
    }
}

/// End-to-end loop tests for `ToolCallResult::stop_loop` — the signal the
/// MCP approval gate uses to end the turn at the `mcp_approval_request`
/// instead of looping the model into a trailing assistant message.
#[cfg(test)]
mod stop_loop_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::{loop_test_support::*, *};

    #[tokio::test]
    async fn stop_loop_ends_turn_without_continuation() {
        // The fix: a tool result with `stop_loop` ends the turn here. The
        // runner must NOT send a continuation (which is what previously made
        // the model emit a trailing assistant message after the approval
        // request and broke the resume turn).
        let counter = Arc::new(AtomicUsize::new(0));
        let runner = ToolLoopRunner::new(payload(), 8)
            .with_provider_callback(counting_callback(counter.clone(), None))
            .rewrite_output(true)
            .register(Arc::new(FakeTool { stop: true }));
        let out = collect(runner.wrap_streaming(first_turn_body(None))).await;

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "stop_loop must not trigger a continuation request"
        );
        // The turn still terminates cleanly, and the synthesized approval
        // item survives in the streamed output (the spec-correct stopping
        // point), with no trailing assistant message after it.
        assert!(
            out.contains("response.completed"),
            "expected a completed terminal:\n{out}"
        );
        assert!(
            out.contains("mcp_approval_request"),
            "the synthesized approval item should be forwarded:\n{out}"
        );
        assert!(
            !out.contains("output_text"),
            "no trailing assistant message should follow the approval request:\n{out}"
        );
        assert!(out.contains("[DONE]"), "expected a terminal [DONE]:\n{out}");
    }

    #[tokio::test]
    async fn without_stop_loop_the_runner_continues_once() {
        // Control: the identical harness with `stop_loop = false` drives
        // exactly one continuation — proving the stop above is the flag's
        // doing, not a dead callback or a tool that never engaged.
        let counter = Arc::new(AtomicUsize::new(0));
        let runner = ToolLoopRunner::new(payload(), 8)
            .with_provider_callback(counting_callback(counter.clone(), None))
            .rewrite_output(true)
            .register(Arc::new(FakeTool { stop: false }));
        let _ = collect(runner.wrap_streaming(first_turn_body(None))).await;

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "a normal tool result must drive exactly one continuation"
        );
    }
}

/// End-to-end tests for the `include: ["usage.incremental"]` extension —
/// cumulative `response.usage.updated` events at tool-loop turn boundaries.
#[cfg(test)]
mod usage_update_tests {
    use std::sync::{Arc, atomic::AtomicUsize};

    use super::{loop_test_support::*, *};

    fn turn1_usage() -> serde_json::Value {
        serde_json::json!({
            "input_tokens": 100, "output_tokens": 50, "total_tokens": 150,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 5},
            "cost": 0.001
        })
    }

    fn turn2_usage() -> serde_json::Value {
        serde_json::json!({
            "input_tokens": 200, "output_tokens": 30, "total_tokens": 230,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 10},
            "cost": 0.002
        })
    }

    fn payload_with_include() -> CreateResponsesPayload {
        serde_json::from_value(serde_json::json!({
            "model": "m", "stream": true, "include": ["usage.incremental"]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn emits_cumulative_usage_update_at_turn_boundary() {
        let counter = Arc::new(AtomicUsize::new(0));
        let runner = ToolLoopRunner::new(payload_with_include(), 8)
            .with_provider_callback(counting_callback(counter, Some(turn2_usage())))
            .rewrite_output(true)
            .register(Arc::new(FakeTool { stop: false }));
        let out = collect(runner.wrap_streaming(first_turn_body(Some(turn1_usage())))).await;
        let events = events_of(&out);

        // Exactly one update — at the turn-1 boundary — carrying turn 1's
        // usage as the cumulative total so far, with proper SSE framing.
        let updates: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "response.usage.updated")
            .collect();
        assert_eq!(updates.len(), 1, "one suppressed turn, one update:\n{out}");
        assert_eq!(updates[0]["usage"]["total_tokens"], 150);
        assert!((updates[0]["usage"]["cost"].as_f64().unwrap() - 0.001).abs() < 1e-9);
        assert!(out.contains("event: response.usage.updated\n"));

        // The terminal still reports the whole loop's fold.
        let terminal = events
            .iter()
            .find(|e| e["type"] == "response.completed")
            .expect("terminal event");
        let usage = &terminal["response"]["usage"];
        assert_eq!(usage["input_tokens"], 300);
        assert_eq!(usage["output_tokens"], 80);
        assert_eq!(usage["total_tokens"], 380);
        assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 15);
        assert!((usage["cost"].as_f64().unwrap() - 0.003).abs() < 1e-9);

        // One monotonic sequence-number space across the merged stream,
        // update included.
        let seqs: Vec<u64> = events
            .iter()
            .filter_map(|e| e["sequence_number"].as_u64())
            .collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "seqs: {seqs:?}");
    }

    #[tokio::test]
    async fn usage_update_requires_include_opt_in() {
        let counter = Arc::new(AtomicUsize::new(0));
        let runner = ToolLoopRunner::new(payload(), 8)
            .with_provider_callback(counting_callback(counter, Some(turn2_usage())))
            .rewrite_output(true)
            .register(Arc::new(FakeTool { stop: false }));
        let out = collect(runner.wrap_streaming(first_turn_body(Some(turn1_usage())))).await;

        assert!(!out.contains("response.usage.updated"), "{out}");
        // The terminal fold is unaffected by the missing opt-in.
        assert!(out.contains("\"total_tokens\":380"), "{out}");
    }

    #[tokio::test]
    async fn no_usage_update_without_tool_turns() {
        // A single-turn response (no tool call → nothing suppressed) stays
        // update-free even with the opt-in.
        let terminal = serde_json::json!({
            "type": "response.completed",
            "sequence_number": 1,
            "response": {"usage": turn1_usage()},
        });
        let sse = format!(
            concat!(
                "data: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{}}}}\n\n",
                "data: {terminal}\n\n",
                "data: [DONE]\n\n",
            ),
            terminal = terminal
        );
        let counter = Arc::new(AtomicUsize::new(0));
        let runner = ToolLoopRunner::new(payload_with_include(), 8)
            .with_provider_callback(counting_callback(counter.clone(), None))
            .rewrite_output(true)
            .register(Arc::new(FakeTool { stop: false }));
        let out = collect(runner.wrap_streaming(Response::new(Body::from(sse)))).await;

        assert!(!out.contains("response.usage.updated"), "{out}");
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stop_loop_terminal_reports_carried_usage_once() {
        // `stop_loop` re-emits the suppressed terminal through the rewriter,
        // which folds in the carried total. The stash is usage-stripped, so
        // the turn's tokens appear exactly once — and the terminal agrees
        // with the last `usage.updated`.
        let counter = Arc::new(AtomicUsize::new(0));
        let runner = ToolLoopRunner::new(payload_with_include(), 8)
            .with_provider_callback(counting_callback(counter, None))
            .rewrite_output(true)
            .register(Arc::new(FakeTool { stop: true }));
        let out = collect(runner.wrap_streaming(first_turn_body(Some(turn1_usage())))).await;
        let events = events_of(&out);

        let update = events
            .iter()
            .find(|e| e["type"] == "response.usage.updated")
            .expect("update event");
        let terminal = events
            .iter()
            .find(|e| e["type"] == "response.completed")
            .expect("terminal event");
        let usage = &terminal["response"]["usage"];
        assert_eq!(usage["input_tokens"], 100, "not double-counted:\n{out}");
        assert_eq!(usage["total_tokens"], 150);
        assert!((usage["cost"].as_f64().unwrap() - 0.001).abs() < 1e-9);
        assert_eq!(update["usage"], *usage);
    }

    #[test]
    fn format_usage_updated_event_shape() {
        let usage: ResponsesUsage = serde_json::from_value(turn1_usage()).unwrap();
        let ev = format_usage_updated_event(&usage);
        let text = std::str::from_utf8(&ev).unwrap();
        let data = text.strip_prefix("data: ").unwrap().trim_end();
        let v: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(v["type"], "response.usage.updated");
        assert_eq!(v["sequence_number"], 0);
        assert_eq!(v["usage"], turn1_usage());
    }

    #[test]
    fn strip_response_usage_removes_usage_preserving_framing() {
        let event = Bytes::from(format!(
            "event: response.completed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.completed",
                "response": {"status": "completed", "usage": turn1_usage()},
            })
        ));
        let out = strip_response_usage(event);
        let text = std::str::from_utf8(&out).unwrap();
        assert!(text.starts_with("event: response.completed\n"));
        let data = text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
        let v: serde_json::Value = serde_json::from_str(data).unwrap();
        assert!(v["response"].get("usage").is_none());
        assert_eq!(v["response"]["status"], "completed");

        // No usage → unchanged bytes.
        let plain =
            Bytes::from_static(b"data: {\"type\":\"response.completed\",\"response\":{}}\n\n");
        assert_eq!(strip_response_usage(plain.clone()), plain);
    }
}
