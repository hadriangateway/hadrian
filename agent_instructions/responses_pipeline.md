# Responses API Streaming Pipeline

`/v1/responses` requests run through `services::responses_pipeline::apply_streaming_pipeline`,
which wraps the upstream provider's SSE stream with input-file staging, guardrails, server-tool
execution, and DB persistence. This doc covers the order, the actors, and how
background requests re-enter the same pipeline.

For the broader Responses API + Containers feature, start with `containers.md`. For adding a
new tool, mirror `services/web_search_tool.rs` or `services/shell_tool.rs`. For the runtime
side, see `adding_runtime.md`.

## Where it lives

- **Entry point**: `routes/api/responses.rs::handle_create_response` for foreground;
  `services/background_executor.rs::run_background` for background requests pulled off the
  queue by the `background_responses` job.
- **Pipeline body**: `services/responses_pipeline.rs::apply_streaming_pipeline`.
- **Tool loop**: `services/server_tools/runner.rs::ToolLoopRunner`.

## Order of operations (foreground)

1. **Admission** (`routes/api/responses.rs`): auth, RBAC, budget, sovereignty checks. Resolve
   the request's shell `environment` against `[features.server_tools].shell_limits` and fail
   fast on cap overruns (`resolve_shell_environment` in `services/shell_tool.rs`).
2. **Input-file staging** (`services/input_file_staging.rs`): walk `payload.input` for
   `input_file` parts and resolve each via Files API lookup / base64 / HTTP fetch. Returns
   `Vec<StagedFile>` ready to write into `/mnt/data`.
3. **Provider dispatch**: `ResponsesExecutor::execute` runs `preprocess_file_search_tools`,
   `preprocess_web_search_tools`, and (per-provider) `preprocess_shell_tools` with a
   `ShellToolHint` built from the resolved environment, then calls the upstream provider for
   a streaming response.
4. **Output guardrails** wrap the stream (block / redact per
   `[features.guardrails].output`).
5. **Server tool loop** (`ToolLoopRunner`): for each SSE event, every registered tool runs
   `detect`. Detected calls run in parallel via `execute()`; their `events` stream interleaves
   into the client stream; their `ToolCallResult` continuation items go into the next
   provider request. The loop iterates up to `[features.server_tools].max_iterations` (final
   iteration strips that tool from `payload.tools` so the model has to produce text).
6. **Response persister** (`services/response_persister.rs`): when persistence is enabled,
   buffer the full SSE event stream and upsert into `responses` + `response_events` so
   `/v1/responses/{id}` can replay.
7. **Webhook fan-out** (`services/responses_webhook.rs`): terminal events fire optional
   per-org webhooks.

## Background variant

`POST /v1/responses { "background": true }` skips the provider call inline. Instead:

1. The route persists a `responses` row in `queued` state.
2. `jobs/background_responses.rs` (leader-locked) pulls queued rows and hands them to
   `services/background_executor.rs::run_background`.
3. The executor reconstructs the request (auth context comes from the persisted `owner` and
   `org_id` — see `db/repos/responses.rs`), then enters the *same* `apply_streaming_pipeline`
   path. SSE events are captured into the `response_events` table rather than streamed to a
   live client.
4. `services/response_event_buffer.rs` lets clients stream the response via
   `GET /v1/responses/{id}?stream=true`, replaying buffered events and tailing new ones.

The skills used at background-execution time are resolved from the persisted owner, NOT the
request-time principal — this matters when the principal rotates (e.g. API key revoked
between queue and dispatch).

## Adding a tool to the pipeline

A `ServerExecutedTool` is the unit of work. To add one:

1. Implement the trait (`services/server_tools/mod.rs`). Be careful in `detect()` — it runs
   for every SSE event, must be cheap, and must use the **canonical detection event** for
   that call type (e.g. `response.output_item.done`) to avoid duplicate fires on partial
   events.
2. Register it in `apply_streaming_pipeline` under whatever feature gate makes sense.
3. Hook a `format_*` helper for any new SSE event types you emit; mirror existing
   `response.<tool>.in_progress` / `.completed` shapes.
4. Update `[features.server_tools].pricing` if there's a per-call cost dimension.

## Common edits

- **Reorder pipeline stages**: only do this with a real reason. Guardrails-before-tools is
  intentional (the guardrail can short-circuit before any tool spends money). Persister-last
  is intentional (we persist what the client actually saw).
- **Add an SSE transformer that changes existing events**: implement `transform_event` on the
  tool. The runner calls it for every outgoing event. Use interior mutability for stateful
  transforms (see `ShellExecutor::transform_event` for citation injection).
- **Skip the pipeline entirely**: `has_enabled_tools()` returns false → the runner falls
  through with the original stream unchanged. That's the right behavior when nothing applies.
