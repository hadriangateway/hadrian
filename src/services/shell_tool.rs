//! Shell tool interception service for the Responses API.
//!
//! Detects `shell` tool calls in upstream responses, dispatches them to
//! the configured `ShellRuntime`, streams the runtime's output back to
//! the client as `response.shell_call.*` SSE events, and folds the
//! final result into the next provider continuation request.
//!
//! Passthrough mode is handled at registration time: the orchestrator
//! simply doesn't register a `ShellExecutor` when the configured
//! runtime advertises `passthrough_only`. In that case the upstream
//! provider's shell tool spec flows through unchanged.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use chrono::Utc;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    api_types::responses::{
        ContainerFileRef, CreateResponsesPayload, FunctionCallOutput, FunctionCallOutputType,
        ResponsesAnnotation, ResponsesInput, ResponsesInputItem, ResponsesToolDefinition,
        ShellEnvironment,
    },
    config::{ContainersConfig, ShellLimitsConfig},
    models::UsageLogEntry,
    pricing::CostPricingSource,
    runtimes::{
        EgressPolicy, ExecEvent, ExecRequest, RuntimeError, SecretMount, SessionSpec, ShellRuntime,
        SkillMount,
    },
    services::{
        container_session::{ContainerPersistence, ContainerSession, ContainerSessionRegistry},
        input_file_staging::StagedFile,
        server_tools::{
            DetectedToolCall, ServerExecutedTool, ToolCallResult, ToolContext, ToolError,
            ToolExecutionHandle,
        },
    },
};

// ─────────────────────────────────────────────────────────────────────────────
// Per-request environment resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Result of intersecting a per-request `ShellEnvironment` with the
/// operator's `[features.server_tools.shell_limits]`. Drives the
/// `SessionSpec` the executor hands to the runtime on first call.
#[derive(Debug, Clone, Default)]
pub struct ResolvedShellEnvironment {
    /// Memory limit to apply to this session, in bytes. `None` means
    /// "use the runtime backend's default".
    pub mem_limit_bytes: Option<u64>,
    /// Egress allowlist + secrets to mount. An empty `allow_hosts`
    /// list means the runtime applies its built-in default.
    pub egress_policy: EgressPolicy,
}

/// Rejection reasons surfaced as `400 Bad Request` at the route layer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShellEnvironmentError {
    #[error("invalid memory_limit '{0}': expected '<n>[k|m|g][b]' (e.g. '512m', '1g')")]
    BadMemoryLimit(String),
    #[error("requested memory_limit {requested_mb} MB exceeds operator cap of {max_mb} MB")]
    MemoryExceedsCap { requested_mb: u64, max_mb: u32 },
    #[error("egress host '{0}' is not in the operator's allowed_egress_hosts")]
    HostNotAllowed(String),
    #[error("unknown domain secret placeholder '{0}' (not in allowed_domain_secrets)")]
    UnknownSecret(String),
    #[error("secret '{placeholder}' may not flow to host '{host}' under operator's allowed_hosts")]
    SecretHostNotAllowed { placeholder: String, host: String },
}

/// Intersect the per-request environment with the operator-pinned
/// limits. The request can ask for a **narrower** subset of what the
/// operator permits; anything outside that envelope is rejected.
///
/// Always succeeds for `request_env = None` (the caller didn't ask for
/// anything beyond defaults). Returns an empty `EgressPolicy` when no
/// egress was requested.
pub fn resolve_shell_environment(
    request_env: Option<&ShellEnvironment>,
    operator: &ShellLimitsConfig,
) -> Result<ResolvedShellEnvironment, ShellEnvironmentError> {
    let default_mem_bytes = operator
        .default_mem_limit_mb
        .map(|mb| u64::from(mb) * 1024 * 1024);

    let Some(env) = request_env else {
        return Ok(ResolvedShellEnvironment {
            mem_limit_bytes: default_mem_bytes,
            egress_policy: EgressPolicy::default(),
        });
    };

    // ── memory ──
    let mem_limit_bytes = match env
        .container_auto
        .as_ref()
        .and_then(|c| c.memory_limit.as_deref())
    {
        Some(raw) => {
            let requested_bytes = parse_memory_limit(raw)
                .ok_or_else(|| ShellEnvironmentError::BadMemoryLimit(raw.to_string()))?;
            if let Some(cap_mb) = operator.max_mem_limit_mb {
                let cap_bytes = u64::from(cap_mb) * 1024 * 1024;
                if requested_bytes > cap_bytes {
                    return Err(ShellEnvironmentError::MemoryExceedsCap {
                        requested_mb: requested_bytes / (1024 * 1024),
                        max_mb: cap_mb,
                    });
                }
            }
            Some(requested_bytes)
        }
        None => default_mem_bytes,
    };

    // ── egress hosts ──
    let allow_hosts = match env.network_policy.as_ref() {
        Some(policy) => {
            for host in &policy.domains {
                if !host_matches_any(host, &operator.allowed_egress_hosts) {
                    return Err(ShellEnvironmentError::HostNotAllowed(host.clone()));
                }
            }
            policy.domains.clone()
        }
        None => Vec::new(),
    };

    // ── domain secrets ──
    let mut secrets = Vec::with_capacity(env.domain_secrets.len());
    for r in &env.domain_secrets {
        let allowed = operator
            .allowed_domain_secrets
            .get(&r.placeholder)
            .ok_or_else(|| ShellEnvironmentError::UnknownSecret(r.placeholder.clone()))?;
        for host in &r.allowed_domains {
            if !host_matches_any(host, &allowed.allowed_hosts) {
                return Err(ShellEnvironmentError::SecretHostNotAllowed {
                    placeholder: r.placeholder.clone(),
                    host: host.clone(),
                });
            }
        }
        // Empty `allowed_domains` in the request inherits the
        // operator's full list — same convention as omitting the field
        // in OpenAI's spec.
        let allowed_hosts = if r.allowed_domains.is_empty() {
            allowed.allowed_hosts.clone()
        } else {
            r.allowed_domains.clone()
        };
        secrets.push(SecretMount {
            placeholder: r.placeholder.clone(),
            value: allowed.value.clone(),
            allowed_hosts,
        });
    }

    Ok(ResolvedShellEnvironment {
        mem_limit_bytes,
        egress_policy: EgressPolicy {
            allow_hosts,
            secrets,
        },
    })
}

/// Parse OpenAI-style `memory_limit` strings: `"512m"`, `"1g"`,
/// `"1024MB"`, case-insensitive. Returns the value in bytes, or
/// `None` on any parse failure. A bare integer is treated as bytes.
fn parse_memory_limit(raw: &str) -> Option<u64> {
    let s = raw.trim().to_ascii_lowercase();
    let (digits, suffix) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| (&s[..i], s[i..].trim()))
        .unwrap_or((s.as_str(), ""));
    if digits.is_empty() {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let mult: u64 = match suffix {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// `host` is allowed iff it matches any entry in `patterns`. Entries
/// may be exact hostnames or `*.suffix.example` glob patterns;
/// a single `"*"` matches anything.
fn host_matches_any(host: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| host_matches(host, p))
}

fn host_matches(host: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // `*.example.com` matches any subdomain (`a.example.com`,
        // `a.b.example.com`) but not the bare apex.
        return host.len() > suffix.len() + 1
            && host.ends_with(suffix)
            && host.as_bytes()[host.len() - suffix.len() - 1] == b'.';
    }
    host.eq_ignore_ascii_case(pattern)
}

/// Identity fields captured at request time for shell-tool usage
/// attribution. Mirrors the tuple `extract_identity` returns elsewhere.
#[derive(Debug, Clone, Default)]
pub struct ShellPrincipal {
    pub api_key_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool arguments (function schema the model sees)
// ─────────────────────────────────────────────────────────────────────────────

/// Arguments the model emits when invoking the function-mode shell
/// tool. Non-OpenAI providers (Anthropic, etc.) see the `shell` tool
/// rewritten as a function tool with this schema.
#[derive(Debug, Clone, Deserialize)]
pub struct ShellToolArguments {
    pub command: String,
    /// Optional stdin to pipe to the command. Kept short — for larger
    /// inputs, prefer writing files via the runtime's file_io and
    /// referring to them from the command.
    #[serde(default)]
    pub stdin: Option<String>,
}

impl ShellToolArguments {
    pub const FUNCTION_NAME: &'static str = "shell";

    pub fn parse(arguments_json: &str) -> Option<Self> {
        serde_json::from_str(arguments_json).ok()
    }

    pub fn function_description() -> &'static str {
        "Execute a shell command in a sandboxed environment and return its output. \
         Use this for running scripts, querying tools, processing data, or any task \
         that benefits from a shell."
    }

    pub fn function_parameters_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "stdin": {
                    "type": "string",
                    "description": "Optional stdin to pipe to the command"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    pub fn function_tool_definition() -> Value {
        serde_json::json!({
            "type": "function",
            "name": Self::FUNCTION_NAME,
            "description": Self::function_description(),
            "parameters": Self::function_parameters_schema(),
            "strict": false,
        })
    }
}

/// Rewrite `shell` tool definitions in the payload to function tools so
/// non-OpenAI models can invoke them.
///
/// Called by chat.rs when the configured runtime is **not** passthrough.
/// In passthrough mode, the spec is left intact so OpenAI sees the
/// native tool definition.
pub fn preprocess_shell_tools(payload: &mut CreateResponsesPayload) {
    let Some(tools) = payload.tools.as_mut() else {
        return;
    };
    for tool in tools.iter_mut() {
        if tool.is_shell() {
            *tool =
                ResponsesToolDefinition::Function(ShellToolArguments::function_tool_definition());
            debug!(
                stage = "tool_preprocessed",
                "Preprocessed shell tool to function definition"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Detection
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ShellToolCall {
    id: String,
    command: String,
    stdin: Option<String>,
}

fn parse_shell_tool_call(value: &Value) -> Option<ShellToolCall> {
    let obj = value.as_object()?;
    if obj.get("type").and_then(|t| t.as_str())? != "function_call" {
        return None;
    }
    if obj.get("name").and_then(|n| n.as_str())? != ShellToolArguments::FUNCTION_NAME {
        return None;
    }
    let id = obj
        .get("call_id")
        .or_else(|| obj.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let arguments_str = obj.get("arguments")?.as_str()?;
    let args = ShellToolArguments::parse(arguments_str)?;
    Some(ShellToolCall {
        id,
        command: args.command,
        stdin: args.stdin,
    })
}

fn detect_shell_in_chunk(chunk: &[u8]) -> Vec<ShellToolCall> {
    let Ok(chunk_str) = std::str::from_utf8(chunk) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for line in chunk_str.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        // Same canonical detection as web_search: only emit on
        // response.output_item.done to avoid duplicates.
        if json.get("type").and_then(|t| t.as_str()) == Some("response.output_item.done")
            && let Some(item) = json.get("item")
            && let Some(tc) = parse_shell_tool_call(item)
        {
            found.push(tc);
        }
    }
    found
}

// ─────────────────────────────────────────────────────────────────────────────
// SSE event formatters
// ─────────────────────────────────────────────────────────────────────────────

fn sse_event(payload: Value) -> Bytes {
    let s = serde_json::to_string(&payload).unwrap_or_default();
    Bytes::from(format!("data: {}\n\n", s))
}

fn format_in_progress(item_id: &str, output_index: usize) -> Bytes {
    sse_event(serde_json::json!({
        "type": "response.shell_call.in_progress",
        "output_index": output_index,
        "item_id": item_id,
    }))
}

fn format_command_started(item_id: &str, output_index: usize, command: &str) -> Bytes {
    sse_event(serde_json::json!({
        "type": "response.shell_call.command_started",
        "output_index": output_index,
        "item_id": item_id,
        "command": command,
    }))
}

fn format_output_chunk(item_id: &str, output_index: usize, stream: &str, data: &[u8]) -> Bytes {
    // Encode chunk bytes as UTF-8 with replacement to keep SSE
    // line-safe; binary data should be rare in shell stdout.
    let text = String::from_utf8_lossy(data).to_string();
    sse_event(serde_json::json!({
        "type": "response.shell_call.output_chunk",
        "output_index": output_index,
        "item_id": item_id,
        "stream": stream, // "stdout" | "stderr"
        "data": text,
    }))
}

fn format_completed(item_id: &str, output_index: usize, exit_code: i32) -> Bytes {
    sse_event(serde_json::json!({
        "type": "response.shell_call.completed",
        "output_index": output_index,
        "item_id": item_id,
        "exit_code": exit_code,
    }))
}

fn format_file_created(item_id: &str, output_index: usize, file: &ContainerFileRef) -> Bytes {
    sse_event(serde_json::json!({
        "type": "response.shell_call.file_created",
        "output_index": output_index,
        "item_id": item_id,
        "container_id": file.container_id,
        "file_id": file.file_id,
        "filename": file.filename,
        "path": file.path,
        "bytes": file.bytes,
        "content_type": file.content_type,
        "source": file.source,
    }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Output trimming for continuation payload
// ─────────────────────────────────────────────────────────────────────────────

/// Max characters of stdout/stderr we feed back to the model per call,
/// preserving head + tail like OpenAI's `output_text_truncation`.
const MAX_OUTPUT_CHARS: usize = 8_000;

fn trim_output(s: String) -> String {
    if s.len() <= MAX_OUTPUT_CHARS {
        return s;
    }
    let half = MAX_OUTPUT_CHARS / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(half)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "{head}\n... [{} chars truncated] ...\n{tail}",
        s.len() - MAX_OUTPUT_CHARS
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// ShellExecutor
// ─────────────────────────────────────────────────────────────────────────────

/// `ServerExecutedTool` implementation that runs shell commands against
/// any [`ShellRuntime`] whose `capabilities().passthrough_only` is
/// false.
///
/// **Not registered for passthrough runtimes** — the orchestrator
/// inspects the runtime's capabilities and skips registration entirely
/// when passthrough is in effect.
///
/// Each `ShellExecutor` is request-scoped (created by
/// `apply_streaming_pipeline` per Responses-API request) and owns one
/// lazily-started [`ContainerSession`] that all shell tool calls
/// within that request share. The session — and its underlying
/// sandbox VM — lives until the `ShellExecutor` is dropped at
/// stream-end, at which point [`ContainerSession::drop`] detaches a
/// terminate task. Files written to `/mnt/data` along the way are
/// captured into `captured_files` and replayed as
/// `container_file_citation` annotations on the assistant's reply.
pub struct ShellExecutor {
    runtime: Arc<dyn ShellRuntime>,
    /// Cost per second of runtime time, in microcents. Multiplied by
    /// the wall-clock duration of each shell call to compute the
    /// chargeable cost emitted to metrics and the usage record.
    cost_microcents_per_second: u64,
    /// Label used for the runtime axis of cost metrics
    /// (e.g. `"microsandbox"`, `"passthrough_openai"`).
    runtime_label: &'static str,
    /// Identity context attached to the per-shell-call usage record so
    /// runtime time is attributed to the right principal.
    principal: ShellPrincipal,
    /// Skill bundles to mount into the session started by this
    /// executor. Resolved upstream from the request's `skills` field;
    /// empty when the request didn't ask for any. Cloned into the
    /// `SessionSpec` on first `execute()` call.
    mounted_skills: Vec<SkillMount>,
    /// Per-execution limits (timeouts, default CPU/mem). Loaded from
    /// `[features.server_tools].shell_limits`.
    limits: crate::config::ShellLimitsConfig,
    /// Per-request `environment` overrides, already intersected with
    /// `limits` at request-acceptance time. Drives memory and egress
    /// on the `SessionSpec` we hand the runtime.
    resolved_env: ResolvedShellEnvironment,
    /// Container / artifact-capture settings.
    containers_config: ContainersConfig,
    /// Per-executor cache of the resolved registry entry. `None`
    /// until the first `execute()` call. Once populated, every
    /// subsequent call in this response goes straight to the cached
    /// `Arc<Mutex<ContainerSession>>` without touching the registry.
    session_handle: Arc<Mutex<Option<Arc<Mutex<ContainerSession>>>>>,
    /// Process-wide registry of live container sessions. Used to
    /// share one VM across responses that target the same
    /// `container_id` (e.g. chained via `previous_response_id`).
    registry: Arc<ContainerSessionRegistry>,
    /// Pre-allocated container id from the pipeline. When `Some`, the
    /// executor checks the registry for an existing session and
    /// reattaches (or creates) under this id; when `None`, it
    /// generates a fresh id on first use (Phase 1/2 behaviour for
    /// in-memory-only deployments).
    container_id_hint: Option<String>,
    /// Files captured across every shell call in this response,
    /// keyed by path so an overwrite replaces the prior entry. Read
    /// synchronously by `transform_event` to populate
    /// `container_file_citation` annotations on output_text events.
    captured_files: Arc<std::sync::Mutex<HashMap<String, ContainerFileRef>>>,
    /// `input_file` parts the request asked us to stage into
    /// `/mnt/data` before the first shell command. Drained on the
    /// first `execute()` call; subsequent calls see `None`.
    pending_input_files: Arc<Mutex<Option<Vec<StagedFile>>>>,
    /// Optional database write-through. When `Some`, a `containers`
    /// row is inserted on session start and every captured file is
    /// upserted into `container_files`; when `None`, the session
    /// runs entirely in-memory (Phase 1/2 behaviour).
    persistence: Option<ContainerPersistence>,
    /// Usage log buffer. When set, the executor pushes a `record_type:
    /// "tool"` entry per completed call with `tool_runtime_seconds` set.
    #[cfg(feature = "concurrency")]
    usage_buffer: Option<Arc<crate::usage_buffer::UsageLogBuffer>>,
}

impl ShellExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Arc<dyn ShellRuntime>,
        cost_microcents_per_second: u64,
        runtime_label: &'static str,
        principal: ShellPrincipal,
        mounted_skills: Vec<SkillMount>,
        limits: crate::config::ShellLimitsConfig,
        resolved_env: ResolvedShellEnvironment,
        containers_config: ContainersConfig,
        staged_input_files: Vec<StagedFile>,
        persistence: Option<ContainerPersistence>,
        registry: Arc<ContainerSessionRegistry>,
        container_id_hint: Option<String>,
        #[cfg(feature = "concurrency")] usage_buffer: Option<
            Arc<crate::usage_buffer::UsageLogBuffer>,
        >,
    ) -> Self {
        let pending = if staged_input_files.is_empty() {
            None
        } else {
            Some(staged_input_files)
        };
        Self {
            runtime,
            cost_microcents_per_second,
            runtime_label,
            principal,
            mounted_skills,
            limits,
            resolved_env,
            containers_config,
            session_handle: Arc::new(Mutex::new(None)),
            registry,
            container_id_hint,
            captured_files: Arc::new(std::sync::Mutex::new(HashMap::new())),
            pending_input_files: Arc::new(Mutex::new(pending)),
            persistence,
            #[cfg(feature = "concurrency")]
            usage_buffer,
        }
    }
}

#[async_trait::async_trait]
impl ServerExecutedTool for ShellExecutor {
    fn name(&self) -> &'static str {
        ShellToolArguments::FUNCTION_NAME
    }

    fn is_enabled_for(&self, payload: &CreateResponsesPayload) -> bool {
        // We only engage if there's a shell tool — or a function tool
        // already preprocessed from a shell tool — in the request.
        payload
            .tools
            .as_ref()
            .map(|tools| {
                tools.iter().any(|t| {
                    t.is_shell()
                        || matches!(
                            t,
                            ResponsesToolDefinition::Function(v)
                                if v.get("name").and_then(|n| n.as_str())
                                    == Some(ShellToolArguments::FUNCTION_NAME)
                        )
                })
            })
            .unwrap_or(false)
    }

    fn detect(&self, event: &[u8], _ctx: &ToolContext) -> Vec<DetectedToolCall> {
        detect_shell_in_chunk(event)
            .into_iter()
            .map(|tc| DetectedToolCall {
                tool_name: ShellToolArguments::FUNCTION_NAME,
                call_id: tc.id.clone(),
                arguments: serde_json::json!({
                    "id": tc.id,
                    "command": tc.command,
                    "stdin": tc.stdin,
                }),
            })
            .collect()
    }

    async fn execute(
        &self,
        call: DetectedToolCall,
        _ctx: &ToolContext,
    ) -> Result<ToolExecutionHandle, ToolError> {
        let command = call
            .arguments
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let stdin = call
            .arguments
            .get("stdin")
            .and_then(|v| v.as_str())
            .map(|s| Bytes::from(s.to_string()));
        let id = call.call_id.clone();
        let runtime = self.runtime.clone();
        let cost_per_sec = self.cost_microcents_per_second;
        let runtime_label = self.runtime_label;
        let principal = self.principal.clone();
        let mounted_skills = self.mounted_skills.clone();
        let containers_config = self.containers_config.clone();
        let session_handle_slot = self.session_handle.clone();
        let registry = self.registry.clone();
        let container_id_hint = self.container_id_hint.clone();
        let captured_files = self.captured_files.clone();
        let pending_input_files = self.pending_input_files.clone();
        let persistence = self.persistence.clone();
        #[cfg(feature = "concurrency")]
        let usage_buffer = self.usage_buffer.clone();

        let (event_tx, event_rx) = mpsc::channel::<Bytes>(32);
        let (result_tx, result_rx) =
            tokio::sync::oneshot::channel::<Result<ToolCallResult, ToolError>>();

        // Emit initial progress events before doing any I/O.
        let _ = event_tx.send(format_in_progress(&id, 0)).await;
        let _ = event_tx
            .send(format_command_started(&id, 0, &command))
            .await;

        // Spawn the actual session work so the orchestrator can start
        // consuming events while we boot the container.
        let id_for_task = id.clone();
        let command_for_task = command.clone();
        let exec_timeout = Duration::from_secs(self.limits.command_timeout_secs.max(1));
        let default_cpu = self.limits.default_cpu_limit;
        // Per-request override (already intersected with operator's
        // limits at request-acceptance time) wins; fall back to the
        // operator default. `egress_policy` is taken from the resolved
        // env verbatim — an empty `allow_hosts` means inherit runtime
        // default, which `SessionSpec`'s default already encodes.
        let resolved_env = self.resolved_env.clone();
        crate::compat::spawn_detached(async move {
            let start = Instant::now();

            // Resolve the registry entry. On the first execute() call
            // for this executor we either grab an existing shared
            // session (chained `previous_response_id`) or boot a new
            // VM and register it. Subsequent calls within this
            // response read the cached `Arc<Mutex<ContainerSession>>`
            // without touching the registry.
            let session_arc: Arc<Mutex<ContainerSession>> = {
                let mut handle_slot = session_handle_slot.lock().await;
                if let Some(existing) = handle_slot.as_ref() {
                    existing.clone()
                } else {
                    // First-call path. Try the registry under the
                    // requested id; if missing, boot.
                    let resolved: Arc<Mutex<ContainerSession>> = match container_id_hint
                        .as_deref()
                        .and_then(|cid| registry.get(cid))
                    {
                        Some(cached) => cached,
                        None => {
                            let spec = SessionSpec {
                                mounted_skills,
                                cpu_limit: default_cpu,
                                mem_limit_bytes: resolved_env.mem_limit_bytes,
                                egress_policy: resolved_env.egress_policy,
                                ..SessionSpec::default()
                            };
                            let boot_result = match (container_id_hint.clone(), persistence.clone())
                            {
                                (Some(cid), Some(p)) => {
                                    ContainerSession::start_attached(
                                        cid,
                                        runtime.clone(),
                                        runtime_label,
                                        spec,
                                        containers_config.clone(),
                                        p,
                                    )
                                    .await
                                }
                                _ => {
                                    ContainerSession::start_new(
                                        runtime.clone(),
                                        runtime_label,
                                        spec,
                                        containers_config.clone(),
                                        persistence.clone(),
                                    )
                                    .await
                                }
                            };
                            let session = match boot_result {
                                Ok(s) => {
                                    debug!(
                                        stage = "container_session_started",
                                        call_id = %id_for_task,
                                        container_id = %s.container_id,
                                        file_io = s.file_io_enabled(),
                                        reattached = container_id_hint.is_some(),
                                        "Started persistent container session"
                                    );
                                    s
                                }
                                Err(RuntimeError::Passthrough) => {
                                    warn!(
                                        stage = "passthrough_invoked",
                                        call_id = %id_for_task,
                                        "Passthrough runtime received an execute() call; \
                                         this indicates a misconfiguration in chat.rs registration"
                                    );
                                    let _ =
                                        event_tx.send(format_completed(&id_for_task, 0, -1)).await;
                                    let _ = result_tx.send(Err(ToolError::ExecutionFailed(
                                        "shell runtime is configured for passthrough but \
                                         executor was invoked"
                                            .into(),
                                    )));
                                    return;
                                }
                                Err(e) => {
                                    error!(
                                        stage = "session_start_failed",
                                        call_id = %id_for_task,
                                        error = %e,
                                        "Failed to start shell session"
                                    );
                                    let _ =
                                        event_tx.send(format_completed(&id_for_task, 0, -1)).await;
                                    let _ = result_tx
                                        .send(Err(ToolError::ExecutionFailed(e.to_string())));
                                    return;
                                }
                            };
                            let cid = session.container_id.clone();
                            let (arc, displaced) = registry.insert(cid, session);
                            if let Some(prev) = displaced {
                                warn!(
                                    stage = "container_registry_race",
                                    call_id = %id_for_task,
                                    "Two concurrent requests booted a VM under the same \
                                     container_id; the displaced session will be terminated \
                                     when its Arc is dropped"
                                );
                                // Drop the displaced Arc explicitly so
                                // any in-flight readers finish, then
                                // its `ContainerSession::drop`
                                // detaches the terminate task.
                                drop(prev);
                            }
                            arc
                        }
                    };
                    *handle_slot = Some(resolved.clone());
                    resolved
                }
            };

            // Hold the per-session lock for the duration of:
            // input-file staging → exec → capture. Concurrent shell
            // calls (within this response OR across responses sharing
            // the container) queue behind this; one VM, one in-flight
            // command.
            let session_guard = session_arc.lock().await;
            let session: &ContainerSession = &session_guard;

            // Drain any `input_file` parts the pipeline staged for us
            // and write them into /mnt/data before the model's command
            // runs. Only fires on the first execute() call per
            // executor; subsequent calls see `None`.
            let pending = {
                let mut p = pending_input_files.lock().await;
                p.take()
            };
            if let Some(files) = pending {
                let file_count = files.len();
                match session.ingest_user_files(files).await {
                    Ok(refs) => {
                        if !refs.is_empty() {
                            debug!(
                                stage = "input_files_staged",
                                call_id = %id_for_task,
                                count = refs.len(),
                                "Staged input_file parts into /mnt/data"
                            );
                        }
                        for r in &refs {
                            let _ = event_tx.send(format_file_created(&id_for_task, 0, r)).await;
                        }
                        // Mirror into captured_files so the very first
                        // assistant message picks them up in
                        // annotations, even if the shell command
                        // itself produces no further output.
                        let mut guard =
                            captured_files.lock().expect("captured_files lock poisoned");
                        for r in refs {
                            guard.insert(r.path.clone(), r);
                        }
                    }
                    Err(e) => {
                        warn!(
                            stage = "input_files_staging_failed",
                            call_id = %id_for_task,
                            count = file_count,
                            error = %e,
                            "Failed to stage one or more input_file parts; \
                             continuing with the shell command anyway"
                        );
                    }
                }
            }

            let exec = match session
                .exec(ExecRequest {
                    command: command_for_task.clone(),
                    stdin,
                    timeout: Some(exec_timeout),
                })
                .await
            {
                Ok(e) => e,
                Err(e) => {
                    error!(
                        stage = "exec_failed",
                        call_id = %id_for_task,
                        error = %e,
                        "Failed to exec shell command"
                    );
                    let _ = event_tx.send(format_completed(&id_for_task, 0, -1)).await;
                    let _ = result_tx.send(Err(ToolError::ExecutionFailed(e.to_string())));
                    return;
                }
            };

            // Stream output, accumulating for the continuation payload.
            // We race two futures:
            //   - `output.next()`: the next ExecEvent from the runtime.
            //   - `event_tx.closed()`: resolves when the orchestrator has
            //     dropped its receiver, which happens when the HTTP
            //     client disconnects upstream.
            //
            // This catches disconnect even for commands that produce no
            // output, which the previous send-error-only check missed.
            let mut stdout_buf = String::new();
            let mut stderr_buf = String::new();
            let mut final_exit: i32 = 0;
            let mut output = exec.handle.output;
            let mut client_disconnected = false;
            loop {
                tokio::select! {
                    _ = event_tx.closed() => {
                        warn!(
                            stage = "client_disconnected",
                            call_id = %id_for_task,
                            "Client disconnected (channel closed); terminating session"
                        );
                        client_disconnected = true;
                        break;
                    }
                    maybe_ev = output.next() => {
                        let Some(ev) = maybe_ev else { break };
                        let send_result = match ev {
                            ExecEvent::Stdout(bytes) => {
                                stdout_buf.push_str(&String::from_utf8_lossy(&bytes));
                                event_tx
                                    .send(format_output_chunk(&id_for_task, 0, "stdout", &bytes))
                                    .await
                            }
                            ExecEvent::Stderr(bytes) => {
                                stderr_buf.push_str(&String::from_utf8_lossy(&bytes));
                                event_tx
                                    .send(format_output_chunk(&id_for_task, 0, "stderr", &bytes))
                                    .await
                            }
                            ExecEvent::Exit { code, .. } => {
                                final_exit = code;
                                Ok(())
                            }
                        };
                        if send_result.is_err() {
                            warn!(
                                stage = "client_disconnected",
                                call_id = %id_for_task,
                                "Client disconnected mid-output; terminating session"
                            );
                            client_disconnected = true;
                            break;
                        }
                    }
                }
            }

            // Snapshot /mnt/data to detect any files the command
            // produced. Only runs when the runtime supports file_io
            // and the operator hasn't disabled the feature. Errors
            // are surfaced as warnings — they don't fail the shell
            // call, since the command itself already ran.
            let mut new_files: Vec<ContainerFileRef> = Vec::new();
            if !client_disconnected {
                match session.capture_changes().await {
                    Ok(refs) => new_files = refs,
                    Err(e) => warn!(
                        stage = "capture_failed",
                        call_id = %id_for_task,
                        error = %e,
                        "Failed to snapshot /mnt/data after exec"
                    ),
                }
                for r in &new_files {
                    let _ = event_tx.send(format_file_created(&id_for_task, 0, r)).await;
                }
            }

            // Replace the global captured_files map for this response
            // with the session's full tracked set (handles overwrites
            // and deletions consistently with what the model sees).
            let all_tracked = session.list_captured().await;
            {
                let mut guard = captured_files.lock().expect("captured_files lock poisoned");
                guard.clear();
                for r in all_tracked {
                    guard.insert(r.path.clone(), r);
                }
            }

            // Release the session lock before the cost-accounting and
            // continuation-build work — those don't need the VM.
            drop(session_guard);

            let duration_secs = start.elapsed().as_secs_f64();

            // Cost is billable regardless of how the session ended — we
            // ran the VM, the operator pays for the time.
            let cost_microcents = (duration_secs * cost_per_sec as f64).round() as i64;

            // Push the per-principal usage record. We do this on every
            // exit path (completion + disconnect) so the principal is
            // billed for what they consumed.
            #[cfg(feature = "concurrency")]
            if let Some(ref buf) = usage_buffer {
                buf.push(UsageLogEntry {
                    request_id: Uuid::new_v4().to_string(),
                    api_key_id: principal.api_key_id,
                    user_id: principal.user_id,
                    org_id: principal.org_id,
                    project_id: principal.project_id,
                    team_id: principal.team_id,
                    service_account_id: principal.service_account_id,
                    model: "shell".to_string(),
                    provider: runtime_label.to_string(),
                    http_referer: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    cost_microcents: Some(cost_microcents),
                    request_at: Utc::now(),
                    streamed: true,
                    cached_tokens: 0,
                    reasoning_tokens: 0,
                    finish_reason: Some(
                        if client_disconnected {
                            "client_disconnected"
                        } else {
                            "completed"
                        }
                        .to_string(),
                    ),
                    latency_ms: Some((duration_secs * 1000.0) as i32),
                    cancelled: client_disconnected,
                    status_code: Some(200),
                    pricing_source: CostPricingSource::PricingConfig,
                    image_count: None,
                    audio_seconds: None,
                    character_count: None,
                    provider_source: None,
                    record_type: "tool".to_string(),
                    tool_name: Some("shell".to_string()),
                    tool_query: Some(command_for_task.clone()),
                    tool_url: None,
                    tool_bytes_fetched: None,
                    tool_results_count: None,
                    tool_runtime_seconds: Some(duration_secs),
                });
            }
            #[cfg(not(feature = "concurrency"))]
            let _ = (&principal, command_for_task.clone());

            if client_disconnected {
                crate::observability::metrics::record_shell_execution(
                    duration_secs,
                    final_exit,
                    "client_disconnected",
                    runtime_label,
                    cost_microcents,
                );
                // Drop both channels without sending — the orchestrator
                // is gone, no one is listening.
                return;
            }

            let _ = event_tx
                .send(format_completed(&id_for_task, 0, final_exit))
                .await;
            info!(
                stage = "shell_completed",
                call_id = %id_for_task,
                exit_code = final_exit,
                duration_ms = (duration_secs * 1000.0) as u64,
                cost_microcents,
                runtime = runtime_label,
                "Shell command completed"
            );
            crate::observability::metrics::record_shell_execution(
                duration_secs,
                final_exit,
                "completed",
                runtime_label,
                cost_microcents,
            );

            // Build the continuation item — the model sees a single
            // text blob with combined stdout/stderr summary, head+tail
            // truncated. When this command produced files, append a
            // short manifest so the model can refer to them on its
            // next turn (e.g. "I wrote /mnt/data/foo.csv").
            let files_section = if new_files.is_empty() {
                String::new()
            } else {
                let mut s = String::from("\noutput_files:\n");
                for f in &new_files {
                    s.push_str(&format!("- {} ({} bytes)\n", f.path, f.bytes));
                }
                s
            };
            let combined = format!(
                "exit_code: {}\nstdout:\n{}\nstderr:\n{}{}",
                final_exit,
                trim_output(stdout_buf),
                trim_output(stderr_buf),
                files_section,
            );

            let cont_item = ResponsesInputItem::FunctionCallOutput(FunctionCallOutput {
                type_: FunctionCallOutputType::FunctionCallOutput,
                id: Some(id_for_task.clone()),
                call_id: id_for_task.clone(),
                output: combined,
                status: None,
            });

            let _ = result_tx.send(Ok(ToolCallResult {
                call_id: id_for_task,
                continuation_items: vec![cont_item],
            }));
            drop(event_tx);
        });

        Ok(ToolExecutionHandle {
            events: Box::pin(futures_util::stream::unfold(
                event_rx,
                |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
            )),
            result: Box::pin(async move {
                result_rx
                    .await
                    .map_err(|_| ToolError::ExecutionFailed("shell result channel closed".into()))?
            }),
        })
    }

    fn apply_to_continuation(
        &self,
        payload: &mut CreateResponsesPayload,
        results: &[ToolCallResult],
        is_final_iteration: bool,
    ) {
        let function_outputs: Vec<ResponsesInputItem> = results
            .iter()
            .flat_map(|r| r.continuation_items.clone())
            .collect();
        if function_outputs.is_empty() {
            return;
        }

        match payload.input {
            Some(ResponsesInput::Items(ref mut items)) => {
                items.extend(function_outputs);
            }
            Some(ResponsesInput::Text(ref text)) => {
                let text = text.clone();
                let mut items = vec![ResponsesInputItem::EasyMessage(
                    crate::api_types::responses::EasyInputMessage {
                        type_: None,
                        role: crate::api_types::responses::EasyInputMessageRole::User,
                        content: crate::api_types::responses::EasyInputMessageContent::Text(text),
                    },
                )];
                items.extend(function_outputs);
                payload.input = Some(ResponsesInput::Items(items));
            }
            None => {
                payload.input = Some(ResponsesInput::Items(function_outputs));
            }
        }

        if is_final_iteration && let Some(ref mut tools) = payload.tools {
            let before = tools.len();
            tools.retain(|t| !t.is_shell());
            tools.retain(|t| {
                if let ResponsesToolDefinition::Function(v) = t {
                    v.get("name").and_then(|n| n.as_str())
                        != Some(ShellToolArguments::FUNCTION_NAME)
                } else {
                    true
                }
            });
            if tools.len() < before {
                info!(
                    stage = "tools_removed",
                    removed = before - tools.len(),
                    "Removed shell tools on final iteration to force completion"
                );
            }
            if tools.is_empty() {
                payload.tools = None;
            }
        }
    }

    /// Inject `container_file_citation` annotations into output_text
    /// `response.content_part.done` events using the captured-files
    /// map populated by each `execute()` call.
    fn transform_event(&self, event: Bytes) -> Bytes {
        let captured: Vec<ContainerFileRef> = {
            let guard = match self.captured_files.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if guard.is_empty() {
                return event;
            }
            guard.values().cloned().collect()
        };
        inject_container_file_citations(&event, &captured)
    }
}

/// Append `container_file_citation` annotations to any
/// `response.content_part.done` event whose part is an `output_text`.
/// Existing annotations on the part are preserved; we extend the
/// `annotations` array rather than overwriting it.
///
/// Mirrors the file_search citation injector but uses a fixed file
/// list rather than parsing markers out of the text.
fn inject_container_file_citations(chunk: &[u8], files: &[ContainerFileRef]) -> Bytes {
    if files.is_empty() {
        return Bytes::copy_from_slice(chunk);
    }
    let Ok(chunk_str) = std::str::from_utf8(chunk) else {
        return Bytes::copy_from_slice(chunk);
    };

    let mut output = String::new();
    for line in chunk_str.split_inclusive('\n') {
        if let Some(data) = line.strip_prefix("data:") {
            let data_trimmed = data.trim();
            if data_trimmed.is_empty() || data_trimmed == "[DONE]" {
                output.push_str(line);
                continue;
            }
            if let Ok(mut json) = serde_json::from_str::<Value>(data_trimmed) {
                let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if event_type == "response.content_part.done"
                    && let Some(part) = json.get_mut("part")
                    && let Some(part_obj) = part.as_object_mut()
                    && part_obj.get("type").and_then(|t| t.as_str()) == Some("output_text")
                {
                    let mut existing: Vec<Value> = part_obj
                        .get("annotations")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    for f in files {
                        let ann = ResponsesAnnotation::ContainerFileCitation {
                            container_id: f.container_id.clone(),
                            file_id: f.file_id.clone(),
                            filename: f.filename.clone(),
                            start_index: 0,
                            end_index: 0,
                            index: None,
                        };
                        if let Ok(v) = serde_json::to_value(&ann) {
                            existing.push(v);
                        }
                    }
                    part_obj.insert("annotations".to_string(), Value::Array(existing));
                    debug!(
                        stage = "container_annotations_injected",
                        count = files.len(),
                        "Injected container_file_citation annotations"
                    );
                }
                if let Ok(json_str) = serde_json::to_string(&json) {
                    output.push_str("data: ");
                    output.push_str(&json_str);
                    output.push_str("\n\n");
                    continue;
                }
            }
        }
        output.push_str(line);
    }
    Bytes::from(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api_types::responses::{
            ShellContainerAuto, ShellDomainSecretRef, ShellEnvironment, ShellNetworkPolicy,
        },
        config::AllowedDomainSecret,
    };

    fn op_limits_with(
        max_mb: Option<u32>,
        hosts: &[&str],
        secrets: &[(&str, &str, &[&str])],
    ) -> ShellLimitsConfig {
        let mut limits = ShellLimitsConfig::default();
        limits.max_mem_limit_mb = max_mb;
        limits.allowed_egress_hosts = hosts.iter().map(|s| (*s).to_string()).collect();
        for (name, value, hs) in secrets {
            limits.allowed_domain_secrets.insert(
                (*name).to_string(),
                AllowedDomainSecret {
                    value: (*value).to_string(),
                    allowed_hosts: hs.iter().map(|s| (*s).to_string()).collect(),
                },
            );
        }
        limits
    }

    #[test]
    fn resolver_none_inherits_operator_default_memory() {
        let mut limits = ShellLimitsConfig::default();
        limits.default_mem_limit_mb = Some(512);
        let r = resolve_shell_environment(None, &limits).unwrap();
        assert_eq!(r.mem_limit_bytes, Some(512 * 1024 * 1024));
        assert!(r.egress_policy.allow_hosts.is_empty());
        assert!(r.egress_policy.secrets.is_empty());
    }

    #[test]
    fn resolver_memory_request_within_cap() {
        let limits = op_limits_with(Some(2048), &[], &[]);
        let env = ShellEnvironment {
            container_auto: Some(ShellContainerAuto {
                memory_limit: Some("1g".into()),
            }),
            ..Default::default()
        };
        let r = resolve_shell_environment(Some(&env), &limits).unwrap();
        assert_eq!(r.mem_limit_bytes, Some(1024 * 1024 * 1024));
    }

    #[test]
    fn resolver_memory_request_exceeds_cap_rejected() {
        let limits = op_limits_with(Some(512), &[], &[]);
        let env = ShellEnvironment {
            container_auto: Some(ShellContainerAuto {
                memory_limit: Some("1g".into()),
            }),
            ..Default::default()
        };
        let err = resolve_shell_environment(Some(&env), &limits).unwrap_err();
        assert!(
            matches!(
                err,
                ShellEnvironmentError::MemoryExceedsCap {
                    requested_mb: 1024,
                    max_mb: 512
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn resolver_memory_no_cap_accepts_anything() {
        let limits = op_limits_with(None, &[], &[]);
        let env = ShellEnvironment {
            container_auto: Some(ShellContainerAuto {
                memory_limit: Some("64g".into()),
            }),
            ..Default::default()
        };
        let r = resolve_shell_environment(Some(&env), &limits).unwrap();
        assert_eq!(r.mem_limit_bytes, Some(64 * 1024 * 1024 * 1024));
    }

    #[test]
    fn resolver_memory_unparseable_rejected() {
        let limits = ShellLimitsConfig::default();
        let env = ShellEnvironment {
            container_auto: Some(ShellContainerAuto {
                memory_limit: Some("huge".into()),
            }),
            ..Default::default()
        };
        let err = resolve_shell_environment(Some(&env), &limits).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::BadMemoryLimit(_)));
    }

    #[test]
    fn resolver_egress_subset_accepted() {
        let limits = op_limits_with(None, &["api.openai.com", "*.example.com"], &[]);
        let env = ShellEnvironment {
            network_policy: Some(ShellNetworkPolicy {
                domains: vec!["api.openai.com".into(), "foo.example.com".into()],
            }),
            ..Default::default()
        };
        let r = resolve_shell_environment(Some(&env), &limits).unwrap();
        assert_eq!(r.egress_policy.allow_hosts.len(), 2);
    }

    #[test]
    fn resolver_egress_apex_does_not_match_wildcard() {
        let limits = op_limits_with(None, &["*.example.com"], &[]);
        let env = ShellEnvironment {
            network_policy: Some(ShellNetworkPolicy {
                domains: vec!["example.com".into()],
            }),
            ..Default::default()
        };
        let err = resolve_shell_environment(Some(&env), &limits).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::HostNotAllowed(h) if h == "example.com"));
    }

    #[test]
    fn resolver_egress_host_outside_allowlist_rejected() {
        let limits = op_limits_with(None, &["api.openai.com"], &[]);
        let env = ShellEnvironment {
            network_policy: Some(ShellNetworkPolicy {
                domains: vec!["evil.example.com".into()],
            }),
            ..Default::default()
        };
        let err = resolve_shell_environment(Some(&env), &limits).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::HostNotAllowed(h) if h == "evil.example.com"));
    }

    #[test]
    fn resolver_wildcard_star_allows_everything() {
        let limits = op_limits_with(None, &["*"], &[]);
        let env = ShellEnvironment {
            network_policy: Some(ShellNetworkPolicy {
                domains: vec!["anything.example".into()],
            }),
            ..Default::default()
        };
        assert!(resolve_shell_environment(Some(&env), &limits).is_ok());
    }

    #[test]
    fn resolver_unknown_secret_rejected() {
        let limits = op_limits_with(None, &[], &[]);
        let env = ShellEnvironment {
            domain_secrets: vec![ShellDomainSecretRef {
                placeholder: "GITHUB_TOKEN".into(),
                allowed_domains: vec![],
            }],
            ..Default::default()
        };
        let err = resolve_shell_environment(Some(&env), &limits).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::UnknownSecret(p) if p == "GITHUB_TOKEN"));
    }

    #[test]
    fn resolver_secret_subset_accepted_inherits_full_allowlist_when_empty() {
        let limits = op_limits_with(
            None,
            &[],
            &[("GH", "ghp_xxx", &["api.github.com", "uploads.github.com"])],
        );
        let env = ShellEnvironment {
            domain_secrets: vec![ShellDomainSecretRef {
                placeholder: "GH".into(),
                allowed_domains: vec![],
            }],
            ..Default::default()
        };
        let r = resolve_shell_environment(Some(&env), &limits).unwrap();
        assert_eq!(r.egress_policy.secrets.len(), 1);
        assert_eq!(r.egress_policy.secrets[0].value, "ghp_xxx");
        assert_eq!(r.egress_policy.secrets[0].allowed_hosts.len(), 2);
    }

    #[test]
    fn resolver_secret_host_outside_allowed_rejected() {
        let limits = op_limits_with(None, &[], &[("GH", "v", &["api.github.com"])]);
        let env = ShellEnvironment {
            domain_secrets: vec![ShellDomainSecretRef {
                placeholder: "GH".into(),
                allowed_domains: vec!["evil.example.com".into()],
            }],
            ..Default::default()
        };
        let err = resolve_shell_environment(Some(&env), &limits).unwrap_err();
        assert!(matches!(
            err,
            ShellEnvironmentError::SecretHostNotAllowed { placeholder, host }
                if placeholder == "GH" && host == "evil.example.com"
        ));
    }

    #[test]
    fn parses_memory_limits() {
        assert_eq!(parse_memory_limit("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory_limit("512m"), Some(512 * 1024 * 1024));
        assert_eq!(parse_memory_limit("1024MB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory_limit("2 GB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_memory_limit("4096"), Some(4096));
        assert_eq!(parse_memory_limit("4096b"), Some(4096));
        assert_eq!(parse_memory_limit(""), None);
        assert_eq!(parse_memory_limit("nope"), None);
        assert_eq!(parse_memory_limit("1tb"), None);
    }

    #[test]
    fn host_match_handles_wildcards() {
        assert!(host_matches("a.example.com", "*.example.com"));
        assert!(host_matches("a.b.example.com", "*.example.com"));
        assert!(!host_matches("example.com", "*.example.com"));
        assert!(!host_matches("notexample.com", "*.example.com"));
        assert!(host_matches("Anything.tld", "*"));
        assert!(host_matches("API.OpenAI.com", "api.openai.com"));
    }

    #[test]
    fn parses_function_call_arguments() {
        let v = serde_json::json!({
            "type": "function_call",
            "name": "shell",
            "call_id": "call_abc",
            "arguments": "{\"command\": \"echo hi\"}"
        });
        let tc = parse_shell_tool_call(&v).unwrap();
        assert_eq!(tc.id, "call_abc");
        assert_eq!(tc.command, "echo hi");
        assert!(tc.stdin.is_none());
    }

    #[test]
    fn ignores_non_shell_function_calls() {
        let v = serde_json::json!({
            "type": "function_call",
            "name": "web_search",
            "call_id": "call_xyz",
            "arguments": "{\"query\": \"hi\"}"
        });
        assert!(parse_shell_tool_call(&v).is_none());
    }

    #[test]
    fn preprocess_rewrites_shell_tool_to_function() {
        let payload_json = serde_json::json!({
            "tools": [{"type": "shell"}],
            "stream": false,
        });
        let mut payload: CreateResponsesPayload = serde_json::from_value(payload_json).unwrap();
        preprocess_shell_tools(&mut payload);
        let tools = payload.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert!(matches!(tools[0], ResponsesToolDefinition::Function(_)));
    }

    #[test]
    fn trim_output_preserves_head_and_tail() {
        let big = "a".repeat(MAX_OUTPUT_CHARS + 100);
        let trimmed = trim_output(big);
        assert!(trimmed.contains("chars truncated"));
        assert!(trimmed.starts_with("aaa"));
        assert!(trimmed.ends_with("aaa"));
    }

    fn sample_file(file_id: &str, filename: &str, path: &str) -> ContainerFileRef {
        ContainerFileRef {
            container_id: "cntr_test".to_string(),
            file_id: file_id.to_string(),
            filename: filename.to_string(),
            path: path.to_string(),
            bytes: 42,
            content_type: Some("text/csv".to_string()),
            source: crate::api_types::responses::ContainerFileSource::Assistant,
        }
    }

    #[test]
    fn injects_container_file_citation_on_content_part_done() {
        let files = vec![sample_file("cfile_abc", "out.csv", "/mnt/data/out.csv")];
        let event = b"data: {\"type\":\"response.content_part.done\",\"part\":{\"type\":\"output_text\",\"text\":\"Done\"}}\n\n";
        let out = inject_container_file_citations(event, &files);
        let s = std::str::from_utf8(&out).unwrap();
        // Pull the JSON payload back out and re-parse.
        let json_str = s.trim().strip_prefix("data: ").unwrap();
        let v: Value = serde_json::from_str(json_str).unwrap();
        let anns = v
            .get("part")
            .and_then(|p| p.get("annotations"))
            .and_then(|a| a.as_array())
            .expect("annotations array");
        assert_eq!(anns.len(), 1);
        let ann = &anns[0];
        assert_eq!(
            ann.get("type").and_then(|t| t.as_str()),
            Some("container_file_citation")
        );
        assert_eq!(
            ann.get("file_id").and_then(|f| f.as_str()),
            Some("cfile_abc")
        );
        assert_eq!(
            ann.get("filename").and_then(|f| f.as_str()),
            Some("out.csv")
        );
        assert_eq!(
            ann.get("container_id").and_then(|c| c.as_str()),
            Some("cntr_test")
        );
    }

    #[test]
    fn preserves_existing_annotations_on_content_part_done() {
        let files = vec![sample_file("cfile_a", "a.csv", "/mnt/data/a.csv")];
        let event = b"data: {\"type\":\"response.content_part.done\",\"part\":{\"type\":\"output_text\",\"text\":\"hi\",\"annotations\":[{\"type\":\"file_citation\",\"file_id\":\"file_existing\",\"filename\":\"prior.txt\",\"index\":0}]}}\n\n";
        let out = inject_container_file_citations(event, &files);
        let s = std::str::from_utf8(&out).unwrap();
        let json_str = s.trim().strip_prefix("data: ").unwrap();
        let v: Value = serde_json::from_str(json_str).unwrap();
        let anns = v["part"]["annotations"].as_array().unwrap();
        assert_eq!(anns.len(), 2, "existing annotation should be preserved");
        assert!(
            anns.iter()
                .any(|a| a["type"] == "file_citation" && a["file_id"] == "file_existing")
        );
        assert!(
            anns.iter()
                .any(|a| a["type"] == "container_file_citation" && a["file_id"] == "cfile_a")
        );
    }

    #[test]
    fn leaves_unrelated_events_untouched() {
        let files = vec![sample_file("cfile_x", "x", "/mnt/data/x")];
        let event = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";
        let out = inject_container_file_citations(event, &files);
        // No content_part.done → no annotations injected; passes through
        // semantically. We deserialize + reserialize so byte-equality
        // isn't guaranteed, but the parsed shape must match.
        let s = std::str::from_utf8(&out).unwrap();
        let json_str = s.trim().strip_prefix("data: ").unwrap();
        let v: Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(v["type"], "response.output_text.delta");
        assert_eq!(v["delta"], "hello");
        assert!(v.get("annotations").is_none());
    }

    #[test]
    fn no_op_when_no_captured_files() {
        let event = b"data: {\"type\":\"response.content_part.done\",\"part\":{\"type\":\"output_text\",\"text\":\"hi\"}}\n\n";
        let out = inject_container_file_citations(event, &[]);
        // Pass-through: returned bytes equal the input bytes verbatim.
        assert_eq!(&out[..], &event[..]);
    }

    #[test]
    fn file_created_event_has_expected_fields() {
        let f = sample_file("cfile_abc", "out.csv", "/mnt/data/out.csv");
        let bytes = format_file_created("call_1", 0, &f);
        let s = std::str::from_utf8(&bytes).unwrap();
        let json_str = s.trim().strip_prefix("data: ").unwrap();
        let v: Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(v["type"], "response.shell_call.file_created");
        assert_eq!(v["item_id"], "call_1");
        assert_eq!(v["file_id"], "cfile_abc");
        assert_eq!(v["filename"], "out.csv");
        assert_eq!(v["path"], "/mnt/data/out.csv");
        assert_eq!(v["container_id"], "cntr_test");
        assert_eq!(v["source"], "assistant");
    }
}
