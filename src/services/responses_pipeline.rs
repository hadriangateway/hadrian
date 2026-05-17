//! Shared streaming pipeline for `/v1/responses`.
//!
//! Both the foreground handler (`api_v1_responses`) and the background
//! worker need to apply the same wraps around the upstream LLM stream:
//! output guardrails, the server-executed-tool loop runner
//! (`file_search`, `web_search`, `shell`), and the persister. This
//! module factors that pipeline out so both call sites stay in sync.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use axum::body::Body;
use bytes::Bytes;
use http::Response;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    AppState,
    api_types::CreateResponsesPayload,
    auth::AuthenticatedRequest,
    config::ProviderConfig,
    db::repos::ResponseOwner,
    models::{ApiKeyOwner, SKILL_MAIN_FILE},
    routes::{
        api::wrap_streaming_with_guardrails,
        execution::{ProviderExecutor, ResponsesExecutor},
    },
    runtimes::{MountedFile, SkillMount},
    services::{
        FileSearchAuthContext, FileSearchContext, WebSearchContext,
        file_search_tool::FileSearchExecutor,
        server_tools::{ProviderCallback, ServerExecutedTool, ToolLoopRunner},
        shell_tool::ShellExecutor,
        web_search_tool::WebSearchExecutor,
    },
};

/// Identity fields used by the shell tool's usage attribution.
///
/// Foreground builds this from the `AuthenticatedRequest`; background
/// builds it from the persisted `ResponseRecord`. Either way the
/// downstream wrap code looks the same.
#[derive(Debug, Clone, Default)]
pub struct PipelinePrincipal {
    pub api_key_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
}

impl PipelinePrincipal {
    /// Build from an HTTP auth context with sensible fallbacks to the
    /// gateway's default user/org for unauthenticated requests.
    pub fn from_auth(state: &AppState, auth: Option<&AuthenticatedRequest>) -> Self {
        let api_key = auth.and_then(|a| a.api_key());
        Self {
            api_key_id: api_key.map(|k| k.key.id),
            user_id: auth.and_then(|a| a.user_id()).or(state.default_user_id),
            org_id: api_key
                .and_then(|k| k.org_id)
                .or_else(|| auth.and_then(|a| a.principal().org_id()))
                .or(state.default_org_id),
            project_id: api_key.and_then(|k| k.project_id),
            team_id: api_key.and_then(|k| k.team_id),
            service_account_id: api_key.and_then(|k| k.service_account_id),
        }
    }
}

/// Derive the canonical owner for a persisted response from the
/// calling principal. Mirrors the ownership semantics used by other
/// Hadrian resources (skills, templates, conversations): the response
/// is owned by whatever scope its API key is bound to, falling back
/// to the user for identity-based auth or the organization for
/// anonymous / default-principal deployments.
///
/// Returns `None` only when no auth context is available and no
/// `default_org_id` is configured — i.e., the gateway can't safely
/// place the row anywhere. Callers turn that into a 401 or skip
/// persistence, depending on the surface.
pub fn derive_response_owner(
    state: &AppState,
    auth: Option<&AuthenticatedRequest>,
) -> Option<ResponseOwner> {
    if let Some(auth) = auth {
        if let Some(api_key) = auth.api_key() {
            return Some(match &api_key.key.owner {
                ApiKeyOwner::Organization { org_id } => ResponseOwner::Organization(*org_id),
                ApiKeyOwner::Team { team_id } => ResponseOwner::Team(*team_id),
                ApiKeyOwner::Project { project_id } => ResponseOwner::Project(*project_id),
                ApiKeyOwner::User { user_id } => ResponseOwner::User(*user_id),
                ApiKeyOwner::ServiceAccount { service_account_id } => {
                    ResponseOwner::ServiceAccount(*service_account_id)
                }
            });
        }
        if let Some(user_id) = auth.user_id() {
            return Some(ResponseOwner::User(user_id));
        }
    }
    state.default_org_id.map(ResponseOwner::Organization)
}

impl From<PipelinePrincipal> for crate::services::shell_tool::ShellPrincipal {
    fn from(p: PipelinePrincipal) -> Self {
        Self {
            api_key_id: p.api_key_id,
            user_id: p.user_id,
            org_id: p.org_id,
            project_id: p.project_id,
            team_id: p.team_id,
            service_account_id: p.service_account_id,
        }
    }
}

/// Errors from resolving the `skills` field on a Responses-API request.
#[derive(Debug, Error)]
pub enum SkillResolutionError {
    #[error("skill id '{0}' is not a valid UUID")]
    InvalidId(String),
    #[error("skill '{0}' not found")]
    NotFound(String),
    #[error("skills require an org context; the request has no resolvable organization")]
    MissingOrg,
    #[error("skill service is not configured on this gateway")]
    NoService,
    #[error("skill lookup failed: {0}")]
    Db(String),
}

/// Resolve `payload.skills` to mountable bundles and prepend each
/// skill's `SKILL.md` to `payload.instructions` so the model knows the
/// skill is available.
///
/// Mutates `payload.instructions` in place when skills are present.
/// Returns an empty `Vec` if `payload.skills` is `None` or empty.
///
/// Called from both the foreground handler and the background executor
/// before the upstream LLM call; the returned mounts are threaded into
/// `apply_streaming_pipeline` so the shell runtime materializes them
/// when a shell-tool call boots a session.
pub async fn resolve_and_inject_skills(
    state: &AppState,
    payload: &mut CreateResponsesPayload,
    org_id: Option<Uuid>,
) -> Result<Vec<SkillMount>, SkillResolutionError> {
    let Some(ref skill_ids) = payload.skills else {
        return Ok(Vec::new());
    };
    if skill_ids.is_empty() {
        return Ok(Vec::new());
    }
    let services = state
        .services
        .as_ref()
        .ok_or(SkillResolutionError::NoService)?;
    let org = org_id.ok_or(SkillResolutionError::MissingOrg)?;

    let mut mounts = Vec::with_capacity(skill_ids.len());
    let mut preamble_sections = Vec::with_capacity(skill_ids.len());

    for raw in skill_ids {
        let id = Uuid::parse_str(raw).map_err(|_| SkillResolutionError::InvalidId(raw.clone()))?;
        let skill = services
            .skills
            .get_by_id_and_org(id, org)
            .await
            .map_err(|e| SkillResolutionError::Db(e.to_string()))?
            .ok_or_else(|| SkillResolutionError::NotFound(raw.clone()))?;

        let mount_path = format!("/skills/{}", skill.id);
        let files = skill
            .files
            .iter()
            .map(|f| MountedFile {
                relative_path: f.path.clone(),
                content: Bytes::from(f.content.clone().into_bytes()),
            })
            .collect();
        if let Some(main) = skill.files.iter().find(|f| f.path == SKILL_MAIN_FILE) {
            preamble_sections.push(format!(
                "## Skill: {name}\n{description}\n\nFiles available under: {mount_path}/\n\n{content}",
                name = skill.name,
                description = skill.description,
                mount_path = mount_path,
                content = main.content,
            ));
        } else {
            preamble_sections.push(format!(
                "## Skill: {name}\n{description}\n\nFiles available under: {mount_path}/",
                name = skill.name,
                description = skill.description,
                mount_path = mount_path,
            ));
        }
        mounts.push(SkillMount {
            skill_id: skill.id.to_string(),
            mount_path,
            files,
        });
    }

    if !preamble_sections.is_empty() {
        let preamble = format!(
            "The following skills are mounted in the sandbox. Use the files at the indicated paths when relevant.\n\n{}",
            preamble_sections.join("\n\n---\n\n")
        );
        payload.instructions = Some(match payload.instructions.take() {
            Some(existing) if !existing.trim().is_empty() => format!("{preamble}\n\n{existing}"),
            _ => preamble,
        });
    }

    Ok(mounts)
}

/// Wrap a streaming Responses-API response with the full server-side
/// pipeline: output guardrails, the server-executed tool loop
/// (`file_search` / `web_search` / `shell`), and the persister.
///
/// Wrap order matches the foreground handler: guardrails first so the
/// tool loop and persister only ever see content that has passed the
/// filter.
///
/// `request_id` is forwarded to guardrails for audit-log correlation.
/// Background callers pass the response_id; foreground passes the
/// HTTP request_id. Either way it's only used as a tracing tag.
///
/// `persistence`: when `Some`, the response is also wrapped with
/// `wrap_streaming_with_persistence`. Background callers always supply
/// this (they pre-created the row); foreground also supplies it when
/// `store=true`. Carries the row's `org_id` so persistence writes are
/// tenant-scoped — a stale or wrong id can't punch into another org.
#[allow(clippy::too_many_arguments)] // each arg is load-bearing; bundling adds no clarity
pub fn apply_streaming_pipeline(
    state: &AppState,
    payload: &CreateResponsesPayload,
    provider_name: String,
    provider_config: ProviderConfig,
    model_name: String,
    principal: PipelinePrincipal,
    mounted_skills: Vec<SkillMount>,
    request_id: Option<String>,
    response: Response<Body>,
    persistence: Option<PersistenceHandle>,
) -> Response<Body> {
    if !response.status().is_success() {
        return response;
    }

    // ── Output guardrails ───────────────────────────────────────
    // Wrap first so the tool loop and persister only see content
    // that's already passed the filter. `wrap_streaming_with_guardrails`
    // is a no-op when the response isn't actually a streaming SSE
    // body, so it's safe to call unconditionally.
    let response = if let Some(ref output_guardrails) = state.output_guardrails {
        let user_id_str = principal.user_id.map(|id| id.to_string());
        wrap_streaming_with_guardrails(response, output_guardrails, user_id_str, request_id)
    } else {
        response
    };

    // ── Tool runner ─────────────────────────────────────────────
    let mut tools: Vec<Arc<dyn ServerExecutedTool>> = Vec::new();

    if let Some(ref file_search_service) = state.file_search_service
        && let Some(ref file_search_config) = state.config.features.file_search
        && file_search_config.enabled
    {
        let file_search_tools: Vec<_> = payload
            .tools
            .as_ref()
            .map(|t| {
                t.iter()
                    .filter_map(|tt| tt.as_file_search().cloned())
                    .collect()
            })
            .unwrap_or_default();
        if !file_search_tools.is_empty() {
            // Build a synthetic FileSearchAuthContext from the
            // principal so the same ACL scoping works for foreground
            // and background calls.
            let file_search_auth = FileSearchAuthContext {
                user_id: principal.user_id,
                org_id: principal.org_id,
                project_id: principal.project_id,
                identity_org_ids: Vec::new(),
                identity_project_ids: Vec::new(),
            };
            let context = FileSearchContext::new(
                file_search_service.clone(),
                file_search_config.clone(),
                Some(file_search_auth),
                file_search_tools,
                payload.clone(),
            );
            tools.push(Arc::new(FileSearchExecutor::new(context)));
        }
    }

    if let Some(ref web_search_config) = state.config.features.web_search {
        let has_web_search = payload
            .tools
            .as_ref()
            .map(|t| t.iter().any(|tt| tt.is_web_search()))
            .unwrap_or(false);
        if has_web_search {
            let context = WebSearchContext::new(
                state.http_client.clone(),
                web_search_config.clone(),
                state.config.features.server_tools.max_iterations,
            );
            tools.push(Arc::new(WebSearchExecutor::new(context)));
        }
    }

    if let Some(ref shell_runtime) = state.shell_runtime {
        let has_shell = payload
            .tools
            .as_ref()
            .map(|t| t.iter().any(|tt| tt.is_shell()))
            .unwrap_or(false);
        if has_shell && !shell_runtime.capabilities().passthrough_only {
            let (rate, label) = match &state.config.features.shell {
                crate::config::ShellRuntimeConfig::None
                | crate::config::ShellRuntimeConfig::PassthroughOpenAI => (0, "unknown"),
                #[cfg(feature = "runtime-microsandbox")]
                crate::config::ShellRuntimeConfig::Microsandbox(_) => (
                    state
                        .config
                        .features
                        .server_tools
                        .pricing
                        .microsandbox_microcents_per_second,
                    "microsandbox",
                ),
                #[cfg(feature = "runtime-opensandbox")]
                crate::config::ShellRuntimeConfig::OpenSandbox(_) => (0, "opensandbox"),
            };
            tools.push(Arc::new(ShellExecutor::new(
                shell_runtime.clone(),
                rate,
                label,
                principal.clone().into(),
                mounted_skills,
                state.config.features.server_tools.shell_limits.clone(),
                #[cfg(feature = "concurrency")]
                state.usage_buffer.clone(),
            )));
        }
    }

    let after_tools = if tools.is_empty() {
        response
    } else {
        let callback_state = state.clone();
        let callback_provider_name = provider_name.clone();
        let callback_provider_config = provider_config.clone();
        let callback_model_name = model_name.clone();
        let provider_callback: ProviderCallback = Arc::new(move |payload| {
            let state = callback_state.clone();
            let provider_name = callback_provider_name.clone();
            let provider_config = callback_provider_config.clone();
            let model_name = callback_model_name.clone();
            Box::pin(async move {
                let mut payload = payload;
                payload.model = Some(model_name);
                ResponsesExecutor::execute(&state, &provider_name, &provider_config, payload).await
            })
        });
        let max_iterations = state.config.features.server_tools.max_iterations;
        let mut runner = ToolLoopRunner::new(payload.clone(), max_iterations)
            .with_provider_callback(provider_callback);
        for tool in tools {
            runner = runner.register(tool);
        }
        runner.wrap_streaming(response)
    };

    // ── Persistence wrap ───────────────────────────────────────
    if let (Some(handle), Some(store)) = (persistence, state.responses_store.as_ref()) {
        crate::services::response_persister::wrap_streaming_with_persistence(
            after_tools,
            store.clone(),
            handle.response_id,
            handle.org_id,
            handle.initial_sequence_number,
            handle.cancel_rx,
            state.response_event_buffer.clone(),
        )
    } else {
        after_tools
    }
}

/// Plumbing handle for the persister wrap. Bundles the row's id and
/// `org_id` together so the persister can issue tenant-scoped writes
/// without re-loading the row.
pub struct PersistenceHandle {
    pub response_id: String,
    pub org_id: Uuid,
    /// Last sequence number already in the event log for this
    /// response. The persister starts incrementing from this so a
    /// re-attach can't collide on the `(response_id, sequence_number)`
    /// primary key.
    pub initial_sequence_number: i64,
    pub cancel_rx: crate::services::CancelSignal,
}
