//! Executor for background responses claimed by the worker.
//!
//! The worker (`jobs::background_responses`) calls
//! [`execute_persisted_response`] once it claims a queued row. The
//! function reconstructs the request, routes it through the same
//! provider plumbing the synchronous handler uses, then wraps the
//! streaming response with the shared `apply_streaming_pipeline`
//! (output guardrails + server-executed tool loop + persister) and
//! drains the body. Skills referenced on the request are resolved at
//! claim time from the persisted `org_id` so each shell session boots
//! with the same mounts the foreground caller asked for.

#![cfg(not(target_arch = "wasm32"))]

use chrono::Utc;
use futures_util::StreamExt;
use thiserror::Error;
use tracing::{error, info, warn};

use crate::{
    AppState,
    api_types::CreateResponsesPayload,
    db::repos::{ResponseCompletion, ResponseRecord, ResponseStatus},
    routes::execution::{ExecutionResult, ResponsesExecutor, execute_with_fallback},
    routing::{resolver, route_models_extended},
    services::{
        ResponsesStore,
        responses_pipeline::{
            PipelinePrincipal, apply_streaming_pipeline, resolve_and_inject_skills,
        },
    },
};

#[derive(Debug, Error)]
pub enum BackgroundExecuteError {
    #[error("payload deserialization failed: {0}")]
    BadPayload(String),
    #[error("model routing failed: {0}")]
    Routing(String),
    #[error("provider resolution failed: {0}")]
    Resolution(String),
    #[error("provider execution failed: {0}")]
    Execution(String),
    #[error("response store missing — background mode requires persistence")]
    NoStore,
}

impl BackgroundExecuteError {
    /// Classify whether this failure could plausibly succeed on a
    /// retry. Configuration-shaped failures (bad payload, missing
    /// provider config) won't get better; transport / provider /
    /// network problems often will.
    pub fn is_transient(&self) -> bool {
        match self {
            // Provider / network / streaming — the same row may
            // succeed if we wait and try again.
            Self::Execution(_) => true,
            // Everything else is a config / structural issue: the
            // payload won't deserialise differently, the routing
            // won't resolve a missing provider, etc.
            Self::BadPayload(_) | Self::Routing(_) | Self::Resolution(_) | Self::NoStore => false,
        }
    }
}

/// True when the request carries a `shell` tool (either as the native
/// `{"type": "shell"}` form or the post-`preprocess_shell_tools`
/// function-mode rewrite). Used to gate `input_file` staging — staging
/// files into `/mnt/data` for a request that can't run shell commands
/// is just wasted I/O.
/// Locate the first `ShellTool` in the request payload and return its
/// (optional) `environment` block. Returns `None` when the payload
/// doesn't carry a typed `shell` tool — `Function`-rewritten tools
/// have no `environment` field by construction.
fn first_shell_environment(
    payload: &crate::api_types::CreateResponsesPayload,
) -> Option<&crate::api_types::responses::ShellEnvironment> {
    payload
        .tools
        .as_ref()?
        .iter()
        .find_map(|t| t.as_shell())
        .and_then(|s| s.environment.as_ref())
}

fn shell_tool_requested(payload: &crate::api_types::CreateResponsesPayload) -> bool {
    let Some(tools) = payload.tools.as_ref() else {
        return false;
    };
    tools.iter().any(|t| {
        t.is_shell()
            || matches!(
                t,
                crate::api_types::responses::ResponsesToolDefinition::Function(v)
                    if v.get("name").and_then(|n| n.as_str()) == Some("shell")
            )
    })
}

/// Run a claimed response to completion.
///
/// `record` must already be in `in_progress` status (claimed via
/// `ResponsesRepo::claim_queued`). The function returns once the
/// streaming response has been fully consumed; the persister updates
/// the row to its terminal status in its own spawned task before this
/// function exits.
pub async fn execute_persisted_response(
    state: AppState,
    record: ResponseRecord,
) -> Result<(), BackgroundExecuteError> {
    let store = state
        .responses_store
        .clone()
        .ok_or(BackgroundExecuteError::NoStore)?;

    info!(
        response_id = %record.id,
        model = %record.model,
        "Background worker executing claimed response"
    );

    // Reconstruct the payload. We force `stream = true` so the
    // persister captures events; the client tails them via
    // GET /v1/responses/{id}?stream=true (matching OpenAI's spec for
    // resuming a Responses-API stream).
    let mut payload: CreateResponsesPayload =
        serde_json::from_value(record.request_payload.clone()).map_err(|e| {
            BackgroundExecuteError::BadPayload(format!("invalid request_payload: {e}"))
        })?;
    payload.stream = true;
    // `background` flag stays — the executor inspects it nowhere in
    // the inner pipeline, but downstream tooling can read it.

    // Route the model.
    let routed = route_models_extended(
        payload.model.as_deref(),
        payload.models.as_deref(),
        &state.config.providers,
    )
    .map_err(|e| BackgroundExecuteError::Routing(e.to_string()))?;

    let resolved = resolver::resolve_to_provider(
        routed,
        state.db.as_ref(),
        state.cache.as_ref(),
        state.secrets.as_ref(),
        None, // background runs without an auth extension; principal already on the row
    )
    .await
    .map_err(|e| BackgroundExecuteError::Resolution(e.to_string()))?;

    let provider_name = resolved.provider_name;
    let provider_config = resolved.provider_config;
    let model_name = resolved.model;
    payload.model = Some(model_name.clone());

    // Re-validate the request's shell `environment` overrides against
    // the current operator limits. Foreground already checked at
    // admission time, but the operator config may have tightened
    // between admission and execution — re-checking ensures we never
    // launch a session that exceeds the current envelope. Misconfig
    // is permanent for this row (`BadPayload`), not retried.
    //
    // We resolve this before skills so container-bound skills can be
    // merged into `payload.skills` ahead of skill resolution.
    let resolved_shell_env_pre = {
        let request_env = first_shell_environment(&payload);
        crate::services::shell_tool::resolve_shell_environment(
            request_env,
            &state.config.features.server_tools.shell_limits,
            &state.config.features.containers,
        )
        .map_err(|e| {
            BackgroundExecuteError::BadPayload(format!("shell environment rejected: {e}"))
        })?
    };

    // Union container-bound skills into `payload.skills` (matches
    // OpenAI's spec where skills bind to the container). We consider
    // both explicit `container_reference` and implicit
    // `previous_response_id` chaining.
    if let Some(svc) = state.containers_service.as_ref() {
        let candidate_container_id: Option<String> = match (
            resolved_shell_env_pre.referenced_container_id.as_deref(),
            payload.previous_response_id.as_deref(),
        ) {
            (Some(referenced), _) => Some(referenced.to_string()),
            (None, Some(prev)) => store
                .get(prev, record.org_id)
                .await
                .ok()
                .and_then(|r| r.container_id),
            _ => None,
        };
        if let Some(cid) = candidate_container_id
            && let Ok(c) = svc.get_container(&cid, record.org_id).await
            && matches!(c.status, crate::db::repos::ContainerStatus::Active)
            && let Some(json) = c.skill_ids_json.as_deref()
            && let Ok(bound) = serde_json::from_str::<Vec<String>>(json)
            && !bound.is_empty()
        {
            let mut merged = payload.skills.clone().unwrap_or_default();
            for s in bound {
                if !merged.contains(&s) {
                    merged.push(s);
                }
            }
            payload.skills = Some(merged);
        }
    }

    // Resolve skills using the org from the persisted row. Mirrors the
    // foreground path: SKILL.md is prepended to instructions and the
    // returned mounts are threaded into apply_streaming_pipeline so
    // the shell runtime materializes the files when a shell call boots
    // a session.
    let mounted_skills = resolve_and_inject_skills(&state, &mut payload, Some(record.org_id))
        .await
        .map_err(|e| BackgroundExecuteError::BadPayload(format!("skill resolution failed: {e}")))?;

    // Final resolved env (re-do after potential payload changes).
    let resolved_shell_env = {
        let request_env = first_shell_environment(&payload);
        crate::services::shell_tool::resolve_shell_environment(
            request_env,
            &state.config.features.server_tools.shell_limits,
            &state.config.features.containers,
        )
        .map_err(|e| {
            BackgroundExecuteError::BadPayload(format!("shell environment rejected: {e}"))
        })?
    };

    // Resolve input_file parts the request asked us to stage into
    // /mnt/data. We only do the work if the request actually carries
    // a shell tool — files for non-shell requests would just sit in
    // memory unused. Errors bubble up as `BadPayload` so the row
    // ends in `failed` with the resolver's diagnostic preserved.
    let staged_input_files = if shell_tool_requested(&payload) {
        crate::services::input_file_staging::stage_input_files(
            &state,
            &payload,
            &state.config.features.containers,
        )
        .await
        .map_err(|e| {
            BackgroundExecuteError::BadPayload(format!("input_file staging failed: {e}"))
        })?
    } else {
        Vec::new()
    };

    // Gateway-side compaction for non-OpenAI providers. Best-effort:
    // an error here means the original payload still flows through.
    if let Err(e) =
        crate::services::compactor::apply_gateway_compaction(&state, &provider_config, &mut payload)
            .await
    {
        tracing::warn!(error = %e, "Background gateway compaction failed; continuing with original payload");
    }

    // Sovereignty requirements are checked at request-creation time
    // for the foreground path; in the background we trust the row.
    let exec_result = execute_with_fallback::<ResponsesExecutor>(
        &state,
        provider_name.clone(),
        provider_config.clone(),
        model_name.clone(),
        payload.clone(),
        None,
    )
    .await
    .map_err(|e| BackgroundExecuteError::Execution(format!("{e:?}")))?;

    let ExecutionResult { response, .. } = exec_result;

    // Cancellation: use the row's in-process watch channel directly.
    // The cross-replica case is handled by a single replica-wide
    // poller (`jobs::responses_cancel_poller`) that periodically
    // queries `WHERE status='cancelled' AND id IN (active set)` and
    // trips the matching sender — one DB round-trip per poll cycle
    // no matter how many backgrounds are in-flight. We use
    // `register_external_execution` so the row's id appears in the
    // active set even when the row was created on a different
    // replica (before a restart, for instance) and there's no local
    // `create()` sender to inherit.
    let cancel_rx = store.register_external_execution(&record.id).await;

    // Reconstruct principal from the persisted row so the shared
    // pipeline applies guardrails / file_search ACLs / shell usage
    // attribution using the same identity that submitted the request.
    let principal = PipelinePrincipal {
        api_key_id: record.api_key_id,
        user_id: record.user_id,
        org_id: Some(record.org_id),
        project_id: record.project_id,
        team_id: None, // teams aren't currently stored on the row
        service_account_id: record.service_account_id,
    };

    // Reconstruct the response owner from the persisted row so the
    // container-persistence layer attributes files to the same scope
    // the foreground caller used.
    let containers_owner = match record.owner_type {
        crate::db::repos::ResponseOwnerType::Organization => {
            crate::db::repos::ResponseOwner::Organization(record.owner_id)
        }
        crate::db::repos::ResponseOwnerType::Team => {
            crate::db::repos::ResponseOwner::Team(record.owner_id)
        }
        crate::db::repos::ResponseOwnerType::Project => {
            crate::db::repos::ResponseOwner::Project(record.owner_id)
        }
        crate::db::repos::ResponseOwnerType::User => {
            crate::db::repos::ResponseOwner::User(record.owner_id)
        }
        crate::db::repos::ResponseOwnerType::ServiceAccount => {
            crate::db::repos::ResponseOwner::ServiceAccount(record.owner_id)
        }
    };

    // Explicit `container_reference` wins over implicit chaining.
    // Foreground validates at admission and returns 400; the row that
    // reached here has already been admitted, so an explicit reference
    // failing now is fatal for this run (`BadPayload`, not retried).
    let container_id_hint = if let Some(ref referenced) = resolved_shell_env.referenced_container_id
    {
        let svc = state.containers_service.as_ref().ok_or_else(|| {
            BackgroundExecuteError::BadPayload(
                "container_reference requires persistence to be enabled".into(),
            )
        })?;
        match svc.get_container(referenced, record.org_id).await {
            Ok(c) if matches!(c.status, crate::db::repos::ContainerStatus::Active) => {
                Some(referenced.clone())
            }
            Ok(c) => {
                return Err(BackgroundExecuteError::BadPayload(format!(
                    "container '{}' is in status '{}' and cannot be referenced",
                    referenced,
                    c.status.as_str()
                )));
            }
            Err(_) => {
                return Err(BackgroundExecuteError::BadPayload(format!(
                    "container '{}' was not found in this organization",
                    referenced
                )));
            }
        }
    } else {
        // Implicit chaining via `previous_response_id`. Any
        // non-active prior container silently falls back to a fresh
        // one rather than erroring.
        match (
            payload.previous_response_id.as_deref(),
            state.containers_service.as_ref(),
        ) {
            (Some(prev_id), Some(containers_svc)) => {
                let prev = store
                    .get(prev_id, record.org_id)
                    .await
                    .ok()
                    .and_then(|r| r.container_id);
                match prev {
                    Some(cid) => match containers_svc.get_container(&cid, record.org_id).await {
                        Ok(c) if matches!(c.status, crate::db::repos::ContainerStatus::Active) => {
                            Some(cid)
                        }
                        _ => None,
                    },
                    None => None,
                }
            }
            _ => None,
        }
    };

    // Build a `UsageLogEntry` so model-token usage from this
    // background run lands in the per-principal usage ledger. Without
    // this the row's `usage` field gets persisted but no
    // `usage_records` row is written — which means cost reports and
    // budget enforcement diverge from reality for background-mode
    // workloads. Mirrors what `build_streaming_usage_entry` does for
    // the foreground path, but sources principal fields from the
    // persisted row instead of the live auth extension.
    let usage_entry = crate::models::UsageLogEntry {
        request_id: uuid::Uuid::new_v4().to_string(),
        api_key_id: record.api_key_id,
        user_id: record.user_id,
        org_id: Some(record.org_id),
        project_id: record.project_id,
        team_id: None,
        service_account_id: record.service_account_id,
        model: model_name.clone(),
        provider: provider_name.clone(),
        input_tokens: 0,
        output_tokens: 0,
        cost_microcents: None,
        http_referer: None,
        request_at: chrono::Utc::now(),
        streamed: true,
        cached_tokens: 0,
        reasoning_tokens: 0,
        finish_reason: None,
        latency_ms: None,
        cancelled: false,
        status_code: None,
        pricing_source: crate::pricing::CostPricingSource::None,
        image_count: None,
        audio_seconds: None,
        character_count: None,
        provider_source: None,
        record_type: "model".to_string(),
        tool_name: None,
        tool_query: None,
        tool_url: None,
        tool_bytes_fetched: None,
        tool_results_count: None,
        tool_runtime_seconds: None,
        tool_exit_code: None,
    };

    let provider_name_clone = provider_name.clone();
    let model_name_clone = model_name.clone();
    let wrapped = apply_streaming_pipeline(
        &state,
        &payload,
        provider_name,
        provider_config,
        model_name,
        principal,
        mounted_skills,
        staged_input_files,
        Some(containers_owner),
        container_id_hint,
        resolved_shell_env,
        // Background has no HTTP request_id; use the response_id for
        // audit-log correlation so events tied to this run can be
        // grouped consistently.
        Some(record.id.clone()),
        response,
        Some(crate::services::responses_pipeline::PersistenceHandle {
            response_id: record.id.clone(),
            org_id: record.org_id,
            initial_sequence_number: record.last_sequence_number,
            cancel_rx,
        }),
    );

    // Wrap the response with the usage-tracking stream so token deltas
    // flow into `state.usage_buffer` exactly the way they do for
    // foreground streaming. The wrap also injects calculated cost into
    // the SSE chunks, but in background mode no client is reading them
    // — the persister is the only downstream consumer, and it already
    // captures the upstream's verbatim `usage` payload.
    let wrapped =
        crate::providers::inject_cost_into_response(crate::providers::CostInjectionParams {
            response: wrapped,
            provider: &provider_name_clone,
            model: &model_name_clone,
            pricing: &state.pricing,
            db: state.db.as_ref(),
            usage_entry: Some(usage_entry),
            #[cfg(feature = "server")]
            task_tracker: Some(&state.task_tracker),
            #[cfg(feature = "server")]
            usage_drain: Some(&state.usage_drain),
            max_response_body_bytes: state.config.server.max_response_body_bytes,
            streaming_idle_timeout_secs: state.config.server.streaming_idle_timeout_secs,
            validation_config: &state.config.observability.response_validation,
            response_type: crate::validation::ResponseType::ResponseStream,
        })
        .await;

    // Drain the body silently. The persister's internal spawned task
    // handles event log writes + the terminal row update.
    let (_parts, body) = wrapped.into_parts();
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        if let Err(e) = chunk {
            warn!(
                response_id = %record.id,
                error = %e,
                "Stream error during background drain"
            );
            // Persister still owns the final-state update; if it
            // received zero terminal events it'll mark the row
            // `incomplete`. Best-effort patch to `failed` here:
            let _ = store
                .update_within_org(
                    &record.id,
                    record.org_id,
                    ResponseCompletion {
                        status: Some(ResponseStatus::Failed),
                        completed_at: Some(Utc::now()),
                        error: Some(serde_json::json!({
                            "code": "stream_error",
                            "message": e.to_string(),
                        })),
                        ..Default::default()
                    },
                )
                .await;
            return Err(BackgroundExecuteError::Execution(e.to_string()));
        }
    }

    info!(response_id = %record.id, "Background response drain complete");
    Ok(())
}

/// Mark a claimed row as `failed` with a structured error payload.
/// Called by the worker when execute_persisted_response returns Err.
pub async fn mark_background_failure(
    store: &ResponsesStore,
    response_id: &str,
    org_id: uuid::Uuid,
    err: &BackgroundExecuteError,
) {
    let error_payload = serde_json::json!({
        "code": match err {
            BackgroundExecuteError::BadPayload(_) => "bad_payload",
            BackgroundExecuteError::Routing(_) => "routing_failed",
            BackgroundExecuteError::Resolution(_) => "provider_resolution_failed",
            BackgroundExecuteError::Execution(_) => "execution_failed",
            BackgroundExecuteError::NoStore => "internal_error",
        },
        "message": err.to_string(),
    });
    if let Err(e) = store
        .update_within_org(
            response_id,
            org_id,
            ResponseCompletion {
                status: Some(ResponseStatus::Failed),
                completed_at: Some(Utc::now()),
                error: Some(error_payload),
                ..Default::default()
            },
        )
        .await
    {
        error!(error = %e, response_id, "Failed to mark background row as failed");
    }
}
