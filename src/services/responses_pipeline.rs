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
        container_session::ContainerPersistence,
        file_search_tool::FileSearchExecutor,
        input_file_staging::StagedFile,
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
    #[error("only `version = \"latest\"` is supported (got `{0}`)")]
    UnsupportedVersion(String),
    #[error("inline skill `{name}` has invalid base64 data: {detail}")]
    InvalidBase64 { name: String, detail: String },
    #[error(
        "inline skill `{name}` uses media_type `{media_type}` — only `text/markdown` is supported today"
    )]
    UnsupportedMediaType { name: String, media_type: String },
    #[error("inline skill `{name}` payload is not valid UTF-8")]
    InvalidUtf8 { name: String },
    #[error("inline skill name must be non-empty")]
    EmptyInlineName,
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
    let Some(ref skills) = payload.skills else {
        return Ok(Vec::new());
    };
    if skills.is_empty() {
        return Ok(Vec::new());
    }

    let mut mounts = Vec::with_capacity(skills.len());
    let mut preamble_sections = Vec::with_capacity(skills.len());

    for entry in skills {
        let mount = resolve_one_skill(state, org_id, entry).await?;
        let preamble = mount.build_preamble();
        preamble_sections.push(preamble);
        mounts.push(mount.into_skill_mount());
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

/// Intermediate form between a parsed `RequestSkill` and a runtime
/// `SkillMount`. Carries the display-name + description so we can
/// build a consistent preamble section regardless of whether the
/// skill came from the database or an inline bundle.
#[derive(Debug)]
struct ResolvedSkill {
    skill_id: String,
    name: String,
    description: String,
    mount_path: String,
    files: Vec<MountedFile>,
    /// `SKILL.md` content when present — used to embed in the preamble.
    main_content: Option<String>,
}

impl ResolvedSkill {
    fn build_preamble(&self) -> String {
        match &self.main_content {
            Some(content) => format!(
                "## Skill: {name}\n{description}\n\nFiles available under: {mount_path}/\n\n{content}",
                name = self.name,
                description = self.description,
                mount_path = self.mount_path,
                content = content,
            ),
            None => format!(
                "## Skill: {name}\n{description}\n\nFiles available under: {mount_path}/",
                name = self.name,
                description = self.description,
                mount_path = self.mount_path,
            ),
        }
    }

    fn into_skill_mount(self) -> SkillMount {
        SkillMount {
            skill_id: self.skill_id,
            mount_path: self.mount_path,
            files: self.files,
        }
    }
}

async fn resolve_one_skill(
    state: &AppState,
    org_id: Option<Uuid>,
    entry: &crate::api_types::RequestSkill,
) -> Result<ResolvedSkill, SkillResolutionError> {
    match entry {
        crate::api_types::RequestSkill::SkillReference(reference) => {
            // Version pin: only `latest` (and the absence of a value)
            // are honored today. Reject anything else loudly so callers
            // discover the gap instead of silently getting `latest`.
            if let Some(v) = reference.version.as_deref()
                && v != "latest"
            {
                return Err(SkillResolutionError::UnsupportedVersion(v.to_string()));
            }
            let services = state
                .services
                .as_ref()
                .ok_or(SkillResolutionError::NoService)?;
            let org = org_id.ok_or(SkillResolutionError::MissingOrg)?;
            let id = Uuid::parse_str(&reference.skill_id)
                .map_err(|_| SkillResolutionError::InvalidId(reference.skill_id.clone()))?;
            let skill = services
                .skills
                .get_by_id_and_org(id, org)
                .await
                .map_err(|e| SkillResolutionError::Db(e.to_string()))?
                .ok_or_else(|| SkillResolutionError::NotFound(reference.skill_id.clone()))?;

            let mount_path = format!("/skills/{}", skill.id);
            let main_content = skill
                .files
                .iter()
                .find(|f| f.path == SKILL_MAIN_FILE)
                .map(|f| f.content.clone());
            let files = skill
                .files
                .iter()
                .map(|f| MountedFile {
                    relative_path: f.path.clone(),
                    content: Bytes::from(f.content.clone().into_bytes()),
                })
                .collect();
            Ok(ResolvedSkill {
                skill_id: skill.id.to_string(),
                name: skill.name,
                description: skill.description,
                mount_path,
                files,
                main_content,
            })
        }
        crate::api_types::RequestSkill::Inline(inline) => resolve_inline_skill(inline),
    }
}

fn resolve_inline_skill(
    inline: &crate::api_types::InlineSkill,
) -> Result<ResolvedSkill, SkillResolutionError> {
    if inline.name.trim().is_empty() {
        return Err(SkillResolutionError::EmptyInlineName);
    }
    let crate::api_types::InlineSkillSource::Base64 { media_type, data } = &inline.source;
    if media_type != "text/markdown" {
        return Err(SkillResolutionError::UnsupportedMediaType {
            name: inline.name.clone(),
            media_type: media_type.clone(),
        });
    }
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| SkillResolutionError::InvalidBase64 {
            name: inline.name.clone(),
            detail: e.to_string(),
        })?;
    let content = String::from_utf8(bytes).map_err(|_| SkillResolutionError::InvalidUtf8 {
        name: inline.name.clone(),
    })?;
    // Synthetic per-request id derived from the payload hash so the
    // mount path is stable across foreground/background dispatch but
    // doesn't collide with stored skill UUIDs.
    let synthetic_id = inline_skill_synthetic_id(&inline.name, &content);
    let mount_path = format!("/skills/{synthetic_id}");
    let files = vec![MountedFile {
        relative_path: SKILL_MAIN_FILE.to_string(),
        content: Bytes::from(content.clone().into_bytes()),
    }];
    Ok(ResolvedSkill {
        skill_id: synthetic_id,
        name: inline.name.clone(),
        description: inline.description.clone(),
        mount_path,
        files,
        main_content: Some(content),
    })
}

/// Synthetic id used as the mount-path segment for inline skills.
/// Hash-based so the same payload reuses the same path across the
/// foreground / background lanes (otherwise the response that resumes
/// from a background tick would mount the skill at a different path).
fn inline_skill_synthetic_id(name: &str, content: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    content.hash(&mut hasher);
    format!("skill_inline_{:016x}", hasher.finish())
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
    staged_input_files: Vec<StagedFile>,
    response_owner: Option<ResponseOwner>,
    // `container_id_hint`: pre-resolved container id this response
    // should attach to (or create under). Derived from
    // `previous_response_id` chaining upstream; `None` falls back to
    // the executor allocating a fresh id on first use.
    container_id_hint: Option<String>,
    // `resolved_shell_env`: per-request shell `environment` block
    // already intersected with `[features.server_tools.shell_limits]`
    // upstream. Foreground and background callers both validate at
    // request-acceptance time so an out-of-bounds request can't even
    // be queued — by the time we get here it's known-safe.
    resolved_shell_env: crate::services::shell_tool::ResolvedShellEnvironment,
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
            // The two passthrough variants are filtered out by the
            // `passthrough_only` guard above; the `None` arm is unreachable
            // because `shell_runtime` would have been `None` too. They're
            // listed for match exhaustiveness only.
            let (rate, label) = match &state.config.features.shell {
                crate::config::ShellRuntimeConfig::None
                | crate::config::ShellRuntimeConfig::PassthroughOpenAI
                | crate::config::ShellRuntimeConfig::ClientPassthrough => (0, "unknown"),
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
            // Build the optional ContainerPersistence handle. All three
            // ingredients must be present: the gateway has a DB (so a
            // ContainersService was constructed), the caller derived a
            // ResponseOwner from auth, and the principal carries an
            // org id. When any is missing we run in-memory only — the
            // live response still works, but `/v1/containers/*`
            // returns 404 for files captured under this session.
            let persistence = match (
                state.containers_service.as_ref(),
                response_owner,
                principal.org_id,
            ) {
                (Some(svc), Some(owner), Some(org_id)) => Some(ContainerPersistence {
                    service: svc.clone(),
                    org_id,
                    owner,
                    source_response_id: persistence.as_ref().map(|h| h.response_id.clone()),
                }),
                _ => None,
            };
            tools.push(Arc::new(ShellExecutor::new(
                shell_runtime.clone(),
                rate,
                label,
                principal.clone().into(),
                mounted_skills,
                state.config.features.server_tools.shell_limits.clone(),
                resolved_shell_env.clone(),
                state.config.features.containers.clone(),
                staged_input_files,
                persistence,
                state.container_session_registry.clone(),
                container_id_hint,
                #[cfg(feature = "concurrency")]
                state.usage_buffer.clone(),
            )));
        }
    }

    // MCP tool executor — engages when `[features.mcp].mode =
    // hadrian_hosted` is configured and the request carries any
    // `mcp` tool entries. Under `passthrough_openai` the upstream
    // (OpenAI/Azure) runs the MCP loop and we don't register here.
    #[cfg(feature = "mcp")]
    {
        if let (Some(mcp_cfg), Some(mcp_service)) = (
            state.config.features.mcp.as_ref(),
            state.mcp_service.as_ref(),
        ) && mcp_cfg.is_hadrian_hosted()
        {
            // Thread persistence context through so the approval gate
            // can park calls. Both response_id and org_id must be
            // present for parking to work; the executor degrades to
            // warn-and-run otherwise.
            let response_id = persistence.as_ref().map(|h| h.response_id.clone());
            let org_id = principal.org_id;
            let executor = crate::services::mcp::McpExecutor::with_persistence(
                mcp_service.clone(),
                payload,
                response_id,
                org_id,
                mcp_cfg.call_timeout_secs,
            );
            if executor.has_bindings() {
                tools.push(Arc::new(executor));
            }

            // Hadrian-side tool search for any `defer_loading` servers.
            // Registered after the MCP executor so its continuation pass
            // (which appends function-call outputs) runs first.
            let tool_search = crate::services::mcp::ToolSearchExecutor::new(
                mcp_service.clone(),
                payload,
                &mcp_cfg.tool_search,
                state.tool_search_embeddings.clone(),
            );
            if tool_search.has_deferred() {
                tools.push(Arc::new(tool_search));
            }
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
        // Every server tool synthesizes its own spec-shaped output items
        // (web_search_call / file_search_call / shell_call / mcp_call …)
        // and suppresses the rewritten function-call plumbing via
        // `transform_event`. Turning on the stream rewriter makes the
        // runner own a single monotonic sequence_number / output_index
        // space and reconstruct the terminal `response.output` from the
        // items it actually forwards — so the persisted/retrieved
        // response carries the hosted-tool items the client saw, not the
        // provider's last-turn view or the internal function calls.
        let mut runner = ToolLoopRunner::new(payload.clone(), max_iterations)
            .with_provider_callback(provider_callback)
            .rewrite_output(true);
        // Restore the caller's original `mcp` tool entries on the echoed
        // `response.tools`. The `hadrian_hosted` rewrite expanded each `mcp`
        // entry into N `mcp_<label>__<tool>` function tools before the
        // provider call; without this the provider echoes those internal
        // functions instead of the `mcp` tool the caller sent.
        #[cfg(feature = "mcp")]
        {
            let echo = build_mcp_tool_echo(payload);
            if !echo.is_empty() {
                runner = runner.with_mcp_tool_echo(echo);
            }
        }
        // Stamp the persisted response id onto lifecycle events so the
        // streamed id is stable across turns and matches what's
        // retrievable via GET /v1/responses/{id}.
        if let Some(handle) = persistence.as_ref() {
            runner = runner.with_response_id(handle.response_id.clone());
        }
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

/// Build the `(function-name prefix, original tool JSON)` pairs the runner
/// uses to collapse rewritten MCP function tools back into the caller's
/// original `mcp` entry on the echoed `response.tools`. The `authorization`
/// bearer is stripped — it must never be echoed back (and thereby persisted)
/// on the stored response. Returns empty when the payload carries no `mcp`
/// tools.
#[cfg(feature = "mcp")]
fn build_mcp_tool_echo(payload: &CreateResponsesPayload) -> Vec<(String, serde_json::Value)> {
    use crate::services::mcp::synthesize_function_name;
    let Some(tools) = payload.tools.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tool in tools {
        let Some(mcp) = tool.as_mcp() else {
            continue;
        };
        let Ok(mut value) = serde_json::to_value(mcp) else {
            continue;
        };
        if let Some(obj) = value.as_object_mut() {
            obj.remove("authorization");
        }
        out.push((synthesize_function_name(&mcp.server_label, ""), value));
    }
    out
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

// ─────────────────────────────────────────────────────────────────────────────
// Non-streaming bridge
// ─────────────────────────────────────────────────────────────────────────────

/// Collect a streaming Responses-API SSE body into a single JSON object
/// matching the OpenAI non-streaming response shape.
///
/// Walks each `data:` event and:
///   - pushes every `response.output_item.done` `item` onto `output[]`,
///     so the caller sees the full transcript across all server-tool
///     iterations (function_call + shell_call_output + final message);
///   - takes the terminal `response.completed` / `response.failed` /
///     `response.incomplete` event's `response` object as the metadata
///     envelope (id, model, status, error, etc.);
///   - sums `usage` across every terminal event observed so the
///     reported tokens and cost reflect the entire agent loop, not the
///     final turn alone.
///
/// Non-success or non-SSE responses pass through unchanged.
pub async fn collect_streaming_response_to_json(response: Response<Body>) -> Response<Body> {
    if !response.status().is_success() {
        return response;
    }
    let is_sse = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    if !is_sse {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    // The pipeline produces bounded SSE bodies (the runner terminates
    // when the model stops calling tools or the iteration budget is
    // hit), so `usize::MAX` is fine here — we already trust the
    // upstream's per-call output size limits.
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(
                stage = "collect_buffered_failed",
                error = %e,
                "Failed to read streaming body for non-streaming bridge"
            );
            return Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()));
        }
    };

    let mut output_items: Vec<serde_json::Value> = Vec::new();
    let mut terminal_response: Option<serde_json::Value> = None;
    let mut usage_sum: Option<crate::api_types::responses::ResponsesUsage> = None;

    for line in bytes.split(|b| *b == b'\n') {
        let Ok(line_str) = std::str::from_utf8(line) else {
            continue;
        };
        let Some(data) = line_str.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let event_type = value.get("type").and_then(|t| t.as_str());
        match event_type {
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item").cloned() {
                    output_items.push(item);
                }
            }
            Some("response.completed" | "response.failed" | "response.incomplete") => {
                if let Some(resp) = value.get("response").cloned() {
                    if let Some(usage_val) = resp.get("usage")
                        && let Ok(usage) = serde_json::from_value::<
                            crate::api_types::responses::ResponsesUsage,
                        >(usage_val.clone())
                    {
                        match usage_sum.as_mut() {
                            None => usage_sum = Some(usage),
                            Some(acc) => add_usage(acc, &usage),
                        }
                    }
                    terminal_response = Some(resp);
                }
            }
            _ => {}
        }
    }

    let mut response_obj = match terminal_response {
        Some(r) => r,
        None => {
            tracing::warn!(
                stage = "collect_buffered_no_terminal",
                "No response.completed event in stream; falling back to passthrough"
            );
            return Response::from_parts(parts, Body::from(bytes));
        }
    };
    response_obj["output"] = serde_json::Value::Array(output_items);
    if let Some(usage) = usage_sum
        && let Ok(usage_val) = serde_json::to_value(&usage)
    {
        response_obj["usage"] = usage_val;
    }
    let body_bytes = match serde_json::to_vec(&response_obj) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(
                stage = "collect_buffered_serialize_failed",
                error = %e,
                "Failed to serialize collected response JSON"
            );
            return Response::builder()
                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()));
        }
    };

    parts.headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    // Strip every header that says "this body is being streamed" — once
    // we've buffered the SSE stream into a fixed JSON blob, hyper will
    // emit the correct Content-Length itself, and leaving any of these
    // in place produces a malformed HTTP frame that hyper closes the
    // connection on (manifests as "Empty reply from server" on the
    // client).
    parts.headers.remove(http::header::CONTENT_LENGTH);
    parts.headers.remove(http::header::TRANSFER_ENCODING);
    parts.headers.remove(http::header::CACHE_CONTROL);
    parts.headers.remove("x-accel-buffering");
    Response::from_parts(parts, Body::from(body_bytes))
}

fn add_usage(
    acc: &mut crate::api_types::responses::ResponsesUsage,
    add: &crate::api_types::responses::ResponsesUsage,
) {
    acc.accumulate(add);
}

#[cfg(test)]
mod skill_tests {
    use base64::Engine as _;

    use super::*;
    use crate::api_types::{InlineSkill, InlineSkillSource};

    #[test]
    fn inline_markdown_skill_resolves_to_skill_md() {
        let payload = "# Useful skill\n\nDo a thing.";
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload.as_bytes());
        let inline = InlineSkill {
            name: "useful".into(),
            description: "Does a thing".into(),
            source: InlineSkillSource::Base64 {
                media_type: "text/markdown".into(),
                data: encoded,
            },
        };
        let mount = resolve_inline_skill(&inline).expect("should resolve");
        assert!(mount.skill_id.starts_with("skill_inline_"));
        assert_eq!(mount.mount_path, format!("/skills/{}", mount.skill_id));
        assert_eq!(mount.files.len(), 1);
        assert_eq!(mount.files[0].relative_path, SKILL_MAIN_FILE);
        assert_eq!(
            std::str::from_utf8(&mount.files[0].content).unwrap(),
            payload
        );
        let preamble = mount.build_preamble();
        assert!(preamble.contains("## Skill: useful"));
        assert!(preamble.contains(payload));
    }

    #[test]
    fn inline_rejects_unsupported_media_type() {
        let inline = InlineSkill {
            name: "useful".into(),
            description: "x".into(),
            source: InlineSkillSource::Base64 {
                media_type: "application/zip".into(),
                data: "AAAA".into(),
            },
        };
        let err = resolve_inline_skill(&inline).expect_err("should fail");
        assert!(
            matches!(err, SkillResolutionError::UnsupportedMediaType { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn inline_rejects_invalid_base64() {
        let inline = InlineSkill {
            name: "x".into(),
            description: "x".into(),
            source: InlineSkillSource::Base64 {
                media_type: "text/markdown".into(),
                data: "not valid base64 !!!".into(),
            },
        };
        let err = resolve_inline_skill(&inline).expect_err("should fail");
        assert!(matches!(err, SkillResolutionError::InvalidBase64 { .. }));
    }

    #[test]
    fn inline_synthetic_id_stable_for_same_payload() {
        let a = inline_skill_synthetic_id("foo", "bar");
        let b = inline_skill_synthetic_id("foo", "bar");
        assert_eq!(a, b);
        let c = inline_skill_synthetic_id("foo", "different");
        assert_ne!(a, c);
    }
}
