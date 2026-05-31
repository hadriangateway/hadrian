//! Shell tool interception service for the Responses API.
//!
//! Detects `shell` tool calls in upstream responses, dispatches them to
//! the configured `ShellRuntime`, emits the spec-canonical
//! `response.output_item.added` / `.done` events carrying `shell_call`
//! and `shell_call_output` items, and folds the final result into the
//! next provider continuation request.
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
        FunctionTool, KnownShellNetworkPolicyType, OutputItemFunctionCall,
        OutputItemFunctionCallType, ResponsesAnnotation, ResponsesInput, ResponsesInputItem,
        ResponsesToolDefinition, ShellCall, ShellCallOutcome, ShellCallOutputItem,
        ShellDomainSecret, ShellEnvironment, ShellNetworkPolicyType,
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
    /// Per-request idle TTL override (in seconds). `None` falls back to
    /// `[features.containers].default_idle_ttl_secs` at attach time.
    pub idle_ttl_secs: Option<i64>,
    /// `container_id` the caller explicitly referenced via
    /// `environment.type = "container_reference"`. The executor must
    /// attach to this exact id; mismatches between this and any
    /// pipeline-derived hint surface as 400.
    pub referenced_container_id: Option<String>,
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
    #[error(
        "inline domain secret '{name}': host '{host}' is not in operator's allowed_egress_hosts"
    )]
    InlineSecretHostNotAllowed { name: String, host: String },
    #[error("container_reference.container_id is empty")]
    EmptyContainerReferenceId,
    #[error(
        "network_policy.type = 'disabled' does not allow allowed_domains \
         or domain_secrets to be set"
    )]
    DisabledPolicyWithFields,
    #[error(
        "expires_after.minutes {requested} exceeds operator cap of {max} minutes \
         ([features.containers].max_idle_ttl_secs)"
    )]
    ExpiresAfterExceedsCap { requested: u32, max: u32 },
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
    containers: &ContainersConfig,
) -> Result<ResolvedShellEnvironment, ShellEnvironmentError> {
    let default_mem_bytes = operator
        .default_mem_limit_mb
        .map(|mb| u64::from(mb) * 1024 * 1024);

    let Some(env) = request_env else {
        return Ok(ResolvedShellEnvironment {
            mem_limit_bytes: default_mem_bytes,
            egress_policy: EgressPolicy::default(),
            idle_ttl_secs: None,
            referenced_container_id: None,
        });
    };

    // ── memory + expires_after (auto-only) ──
    // OpenAI's `ContainerAutoParam` includes `expires_after`; the
    // resolver honors it here so a per-request idle TTL flows into the
    // container row at provision time. Inline requests with no
    // `expires_after` fall back to the container's persisted TTL (when
    // chained) or `[features.containers].default_idle_ttl_secs` (for a
    // fresh auto-provisioned session).
    let max_idle_minutes = (containers.max_idle_ttl_secs / 60) as u32;
    let (mem_limit_bytes, idle_ttl_secs) = match env {
        ShellEnvironment::ContainerAuto(auto) => {
            let mem = match auto.memory_limit.as_deref() {
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
            let ttl = match auto.expires_after.as_ref() {
                Some(exp) => {
                    if exp.minutes > max_idle_minutes {
                        return Err(ShellEnvironmentError::ExpiresAfterExceedsCap {
                            requested: exp.minutes,
                            max: max_idle_minutes,
                        });
                    }
                    Some(i64::from(exp.minutes) * 60)
                }
                None => None,
            };
            (mem, ttl)
        }
        ShellEnvironment::ContainerReference(_) | ShellEnvironment::Local(_) => {
            (default_mem_bytes, None)
        }
    };

    let referenced_container_id = match env {
        ShellEnvironment::ContainerReference(r) => {
            let trimmed = r.container_id.trim();
            if trimmed.is_empty() {
                return Err(ShellEnvironmentError::EmptyContainerReferenceId);
            }
            Some(trimmed.to_string())
        }
        ShellEnvironment::ContainerAuto(_) | ShellEnvironment::Local(_) => None,
    };

    // ── egress hosts + domain secrets ──
    let mut allow_hosts: Vec<String> = Vec::new();
    let mut secrets: Vec<SecretMount> = Vec::new();
    if let Some(policy) = env.network_policy() {
        // `disabled` ⇒ deny-all egress: no allowed_domains, no
        // domain_secrets. We treat the disabled policy as authoritative
        // and refuse to silently ignore allowlist entries the caller
        // also sent.
        if matches!(
            policy.type_,
            ShellNetworkPolicyType::Known(KnownShellNetworkPolicyType::Disabled)
        ) {
            if !policy.allowed_domains.is_empty() || !policy.domain_secrets.is_empty() {
                return Err(ShellEnvironmentError::DisabledPolicyWithFields);
            }
            // allow_hosts stays empty → runtime applies deny-all.
            return Ok(ResolvedShellEnvironment {
                mem_limit_bytes,
                egress_policy: EgressPolicy {
                    allow_hosts: Vec::new(),
                    secrets: Vec::new(),
                },
                idle_ttl_secs,
                referenced_container_id,
            });
        }
        for host in &policy.allowed_domains {
            if !host_matches_any(host, &operator.allowed_egress_hosts) {
                return Err(ShellEnvironmentError::HostNotAllowed(host.clone()));
            }
        }
        allow_hosts = policy.allowed_domains.clone();

        for entry in &policy.domain_secrets {
            match entry {
                ShellDomainSecret::Reference(r) => {
                    let allowed = operator
                        .allowed_domain_secrets
                        .get(&r.placeholder)
                        .ok_or_else(|| {
                            ShellEnvironmentError::UnknownSecret(r.placeholder.clone())
                        })?;
                    for host in &r.allowed_domains {
                        if !host_matches_any(host, &allowed.allowed_hosts) {
                            return Err(ShellEnvironmentError::SecretHostNotAllowed {
                                placeholder: r.placeholder.clone(),
                                host: host.clone(),
                            });
                        }
                    }
                    // Empty `allowed_domains` in the request inherits the
                    // operator's full list — same convention as omitting the
                    // field in OpenAI's spec.
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
                ShellDomainSecret::Inline(inline) => {
                    // Inline form puts the raw value on the wire (OpenAI
                    // shape). Hadrian still enforces the operator's host
                    // allowlist: the secret may only flow to a host the
                    // operator permits, not anywhere the caller wants.
                    if !host_matches_any(&inline.domain, &operator.allowed_egress_hosts) {
                        return Err(ShellEnvironmentError::InlineSecretHostNotAllowed {
                            name: inline.name.clone(),
                            host: inline.domain.clone(),
                        });
                    }
                    secrets.push(SecretMount {
                        placeholder: inline.name.clone(),
                        value: inline.value.clone(),
                        allowed_hosts: vec![inline.domain.clone()],
                    });
                }
            }
        }
    }

    Ok(ResolvedShellEnvironment {
        mem_limit_bytes,
        egress_policy: EgressPolicy {
            allow_hosts,
            secrets,
        },
        idle_ttl_secs,
        referenced_container_id,
    })
}

/// Public façade for [`parse_memory_limit`] used by
/// `POST /v1/containers` to pre-validate the request body before
/// persisting it.
pub fn parse_memory_limit_pub(raw: &str) -> Option<u64> {
    parse_memory_limit(raw)
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
    // Normalize a trailing dot on the host — `example.com.` and
    // `example.com` are the same FQDN and operators don't write the
    // trailing form into allowlists. Cheap to strip; avoids a surprise
    // miss when the model echoes a fully-qualified name back through
    // curl or DNS tooling.
    let host = host.trim_end_matches('.');
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // `*.example.com` matches any subdomain (`a.example.com`,
        // `a.b.example.com`) but not the bare apex.
        let suffix = suffix.trim_end_matches('.');
        return host.len() > suffix.len() + 1
            && host.ends_with(suffix)
            && host.as_bytes()[host.len() - suffix.len() - 1] == b'.';
    }
    host.eq_ignore_ascii_case(pattern.trim_end_matches('.'))
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

/// Where shell execution actually happens. Drives the tone of the
/// dynamic tool description emitted to non-OpenAI providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellExecutionLocation {
    /// Hadrian-hosted sandbox VM (microsandbox / opensandbox).
    HadrianSandbox,
    /// The API client fulfills the call itself (`client_passthrough`).
    /// Hadrian can't promise anything about the environment.
    ApiClient,
}

/// Everything the model should be told about its shell tool for a given
/// request. Built once per request from the resolved runtime, container
/// config, and effective shell environment, then folded into the
/// function-mode description rewritten in [`preprocess_shell_tools`].
///
/// The default value produces a minimal, conservative description
/// suitable when no concrete sandbox is wired up.
#[derive(Debug, Clone)]
pub struct ShellToolHint {
    pub location: ShellExecutionLocation,
    /// Workdir inside the sandbox where input files are staged and
    /// captured output files are read from. Always `/mnt/data` today.
    pub workdir: &'static str,
    /// Idle / capture behavior is enabled (containers feature on).
    pub container_persistence: bool,
    /// Network access from inside the sandbox.
    pub network_summary: ShellNetworkSummary,
    /// Memory limit applied to the sandbox, in MiB.
    pub mem_limit_mb: Option<u64>,
    /// Per-command wall-clock cap, in seconds.
    pub command_timeout_secs: u64,
    /// stdout/stderr fed back to the model are truncated past this many
    /// characters with a head+tail keep.
    pub max_output_chars: usize,
    /// Names of skill bundles mounted under `/skills/<id>` for this
    /// request.
    pub mounted_skill_ids: Vec<String>,
    /// Operator-supplied description of the container environment
    /// (`[features.server_tools.shell_limits].environment_description`):
    /// pre-installed toolchains, how to install packages, etc. Appended
    /// verbatim to the tool description. `None` appends nothing.
    pub environment_description: Option<String>,
}

/// Network access summary for the model. Tone matches OpenAI's
/// `network_policy` field.
#[derive(Debug, Clone)]
pub enum ShellNetworkSummary {
    /// No outbound network from the sandbox.
    NoNetwork,
    /// Egress restricted to specific hosts.
    Allowlist(Vec<String>),
    /// Unrestricted outbound network.
    Unrestricted,
    /// Hadrian can't tell (e.g. client-fulfilled mode).
    Unknown,
}

impl Default for ShellToolHint {
    fn default() -> Self {
        Self {
            location: ShellExecutionLocation::HadrianSandbox,
            workdir: crate::services::container_session::MNT_DATA,
            container_persistence: false,
            network_summary: ShellNetworkSummary::Unknown,
            mem_limit_mb: None,
            command_timeout_secs: 300,
            max_output_chars: DEFAULT_MAX_OUTPUT_CHARS,
            mounted_skill_ids: Vec::new(),
            environment_description: None,
        }
    }
}

impl ShellToolHint {
    /// Render the hint into the `description` string the model sees on
    /// the function-mode shell tool. The wording is deliberately
    /// prescriptive about workdir, file capture, persistence, network,
    /// and truncation — model providers' built-in shell tools embed
    /// similar guidance (Anthropic's `bash_20250124` adds ~245 tokens
    /// of it) and without it models guess at paths and lose output.
    pub fn render_description(&self) -> String {
        let mut s = String::with_capacity(512);
        s.push_str(
            "Execute a shell command and return its stdout, stderr, and exit code. \
             Use this for running scripts, processing data, or any task that benefits \
             from a shell.\n\n",
        );

        match self.location {
            ShellExecutionLocation::HadrianSandbox => {
                s.push_str(&format!(
                    "Runs in a sandboxed Linux environment hosted by the gateway. \
                     Working directory is `{}`.\n",
                    self.workdir
                ));
                if self.container_persistence {
                    s.push_str(&format!(
                        "- Files you write under `{}` are captured and returned to the \
                         caller as `container_file_citation` annotations. The caller can \
                         download them via the containers API.\n",
                        self.workdir
                    ));
                    s.push_str(
                        "- State under that directory **persists across turns** in the \
                         same response and across responses that chain via \
                         `previous_response_id`. Files you wrote earlier are still there.\n",
                    );
                } else {
                    s.push_str(
                        "- Files written during this call are not persisted between \
                         turns; recreate any intermediate state you need.\n",
                    );
                }
                if !self.mounted_skill_ids.is_empty() {
                    s.push_str("- Skill bundles mounted for this request: ");
                    for (i, id) in self.mounted_skill_ids.iter().enumerate() {
                        if i > 0 {
                            s.push_str(", ");
                        }
                        s.push_str(&format!("`/skills/{id}`"));
                    }
                    s.push_str(
                        ". Inspect those directories for tools or data the \
                                caller wants you to use.\n",
                    );
                }
                match &self.network_summary {
                    ShellNetworkSummary::NoNetwork => {
                        s.push_str(
                            "- **No outbound network.** Do not attempt to fetch \
                                    packages, call APIs, or resolve hostnames.\n",
                        );
                    }
                    ShellNetworkSummary::Allowlist(hosts) if !hosts.is_empty() => {
                        s.push_str("- Outbound network is restricted to these hosts only: ");
                        for (i, h) in hosts.iter().enumerate() {
                            if i > 0 {
                                s.push_str(", ");
                            }
                            s.push_str(&format!("`{h}`"));
                        }
                        s.push_str(". Reaching anything else will fail.\n");
                    }
                    ShellNetworkSummary::Allowlist(_) => {
                        s.push_str("- Outbound network uses an operator-defined allowlist.\n");
                    }
                    ShellNetworkSummary::Unrestricted => {
                        s.push_str(
                            "- Outbound network is unrestricted, but prefer minimal \
                             egress and never exfiltrate secrets the caller didn't share.\n",
                        );
                    }
                    ShellNetworkSummary::Unknown => {}
                }
                if let Some(mb) = self.mem_limit_mb {
                    s.push_str(&format!(
                        "- Memory limit: {mb} MiB. Stream large datasets through pipes \
                         rather than loading them all in memory.\n"
                    ));
                }
                s.push_str(&format!(
                    "- Per-command wall-clock limit: {}s. Long-running jobs must be \
                     broken into steps.\n",
                    self.command_timeout_secs
                ));
            }
            ShellExecutionLocation::ApiClient => {
                s.push_str(
                    "Runs on the **API client**, not the gateway. The client decides \
                     the working directory, available tools, and network policy — assume \
                     a generic POSIX shell with whatever the caller's environment \
                     provides. Do not assume packages, files, or services are present \
                     unless the caller said so.\n",
                );
            }
        }

        s.push_str(&format!(
            "\nstdout and stderr fed back to you are truncated past {} characters \
             (head + tail kept). For long output, redirect to a file (e.g. \
             `cmd > /mnt/data/log.txt`) and grep / tail it on a follow-up call.",
            self.max_output_chars
        ));

        // Operator-supplied environment notes (pre-installed toolchains, how to
        // install packages, etc.) so the model doesn't waste turns probing.
        if let Some(desc) = self.environment_description.as_deref() {
            let desc = desc.trim();
            if !desc.is_empty() {
                s.push_str("\n\nEnvironment:\n");
                s.push_str(desc);
            }
        }

        s
    }
}

/// Arguments the model emits when invoking the function-mode shell
/// tool. Mirrors OpenAI's `shell_call.action` object: `commands` is a
/// sequence of shell strings executed in order in the same session,
/// with optional `working_directory`, `env`, `timeout_ms`, and
/// `max_output_length` overrides.
///
/// ```json
/// {"action": {"commands": ["cd src", "ls -la"], "timeout_ms": 5000}}
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShellToolArguments {
    /// Nested OpenAI `action` object carrying the commands and
    /// per-call overrides.
    #[serde(default)]
    pub action: Option<ShellToolAction>,
    /// Optional stdin piped to the joined command script. Hadrian
    /// extension — kept because the spec doesn't carry stdin and some
    /// useful flows (`base64 -d > out`) need it.
    #[serde(default)]
    pub stdin: Option<String>,
}

/// Per-call action object (OpenAI's `shell_call.action` shape).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShellToolAction {
    /// Shell command lines to execute, in order, in the same session.
    /// Joined with newlines and run as a single script — exit_code is
    /// the script's final exit status. Use explicit `&&` chains inside
    /// one entry when the model wants short-circuit semantics.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Per-call timeout in milliseconds. Clamped to the operator's
    /// `command_timeout_secs * 1000` cap; values larger than the cap
    /// are silently shortened rather than rejected, mirroring
    /// OpenAI's behaviour.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Per-call cap on stdout+stderr characters fed back to the
    /// model. Clamped to the operator's `max_output_chars` cap.
    #[serde(default)]
    pub max_output_length: Option<usize>,
    /// Optional environment variables for this call. Names must match
    /// `[A-Za-z_][A-Za-z0-9_]*`; values are passed through to the
    /// shell verbatim.
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// Optional working directory for this call. When unset, the
    /// runtime's default (`/mnt/data`) is used.
    #[serde(default)]
    pub working_directory: Option<String>,
}

/// One concrete shell call after parsing the `action` shape.
#[derive(Debug, Clone)]
pub struct ResolvedShellArgs {
    /// Spec-shaped command list — joined into a script for execution.
    pub commands: Vec<String>,
    pub stdin: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_output_length: Option<usize>,
    pub env: Option<HashMap<String, String>>,
    pub working_directory: Option<String>,
}

impl ResolvedShellArgs {
    /// Join `commands` into a single shell script. Empty / whitespace
    /// entries are dropped — they'd otherwise produce confusing
    /// "command not found" lines.
    pub fn joined_script(&self) -> String {
        self.commands
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl ShellToolArguments {
    pub const FUNCTION_NAME: &'static str = "shell";

    pub fn parse(arguments_json: &str) -> Option<Self> {
        serde_json::from_str(arguments_json).ok()
    }

    /// Resolve the parsed arguments into a flat call shape. Returns
    /// `None` when no non-empty command line was supplied.
    pub fn resolve(self) -> Option<ResolvedShellArgs> {
        let action = self.action?;
        let commands: Vec<String> = action
            .commands
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if commands.is_empty() {
            return None;
        }
        Some(ResolvedShellArgs {
            commands,
            stdin: self.stdin,
            timeout_ms: action.timeout_ms,
            max_output_length: action.max_output_length,
            env: action.env,
            working_directory: action.working_directory,
        })
    }

    pub fn function_parameters_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "object",
                    "description": "Spec-compliant action object (matches OpenAI's shell_call.action shape).",
                    "properties": {
                        "commands": {
                            "type": "array",
                            "items": {"type": "string"},
                            "minItems": 1,
                            "description": "Shell command lines, executed in order in the same session. Joined into a script — use explicit `&&` for short-circuit semantics."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Per-call timeout in milliseconds. Clamped to the operator's cap."
                        },
                        "max_output_length": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Per-call cap on stdout+stderr characters fed back to the model. Clamped to the operator's cap."
                        },
                        "env": {
                            "type": "object",
                            "additionalProperties": {"type": "string"},
                            "description": "Extra environment variables exported for this call only."
                        },
                        "working_directory": {
                            "type": "string",
                            "description": "Override working directory for this call (defaults to /mnt/data)."
                        }
                    },
                    "required": ["commands"],
                    "additionalProperties": false
                },
                "stdin": {
                    "type": "string",
                    "description": "Optional stdin piped to the joined command script."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    /// Build the function-tool JSON the model sees, embedding the
    /// rendered hint as the description.
    pub fn function_tool_definition(hint: &ShellToolHint) -> Value {
        serde_json::json!({
            "type": "function",
            "name": Self::FUNCTION_NAME,
            "description": hint.render_description(),
            "parameters": Self::function_parameters_schema(),
            "strict": false,
        })
    }
}

/// Rewrite `shell` tool definitions in the payload to function tools so
/// non-OpenAI models can invoke them. The hint describes the effective
/// sandbox so the model sees workdir, persistence, network, and
/// truncation rules accurate for *this* request.
///
/// Called by `routes/execution.rs` for every non-passthrough provider
/// path. In OpenAI passthrough modes the native spec is left intact and
/// this is skipped.
pub fn preprocess_shell_tools(payload: &mut CreateResponsesPayload, hint: &ShellToolHint) {
    // History items reconstructed from a `previous_response_id` chain carry
    // the client-facing hosted-shell shapes (`shell_call` /
    // `shell_call_output`, the latter with an *array* `output`). In function
    // mode — the exact set of provider paths that call this — the model
    // originally exchanged `function_call` / `function_call_output`, so
    // replay those shapes instead. Forwarded verbatim the array `output`
    // makes OpenAI-compatible upstreams reject the turn (`output`: expected
    // string, received array) and Anthropic / Bedrock / Vertex silently drop
    // the items, erasing the tool result from history. Runs before the tools
    // early-return so a continuation that no longer re-declares the shell
    // tool still gets its history normalized.
    rewrite_shell_history_to_function_calls(payload);

    let Some(tools) = payload.tools.as_mut() else {
        return;
    };
    let rewrite = ResponsesToolDefinition::Function(
        FunctionTool::from_json(ShellToolArguments::function_tool_definition(hint))
            .expect("shell function-tool definition is well-formed"),
    );
    for tool in tools.iter_mut() {
        if tool.is_shell() {
            *tool = rewrite.clone();
            debug!(
                stage = "tool_preprocessed",
                location = ?hint.location,
                "Preprocessed shell tool to function definition"
            );
        }
    }
}

/// Rewrite reconstructed hosted-shell history items (`shell_call` /
/// `shell_call_output`) in `payload.input` into the `function_call` /
/// `function_call_output` pair the model exchanged in function mode. See
/// [`preprocess_shell_tools`] for why. The two items stay paired by their
/// shared `call_id`, so the upstream still matches the call to its result.
fn rewrite_shell_history_to_function_calls(payload: &mut CreateResponsesPayload) {
    let Some(ResponsesInput::Items(items)) = payload.input.as_mut() else {
        return;
    };
    for item in items.iter_mut() {
        let rewritten = match item {
            ResponsesInputItem::ShellCall(call) => Some(ResponsesInputItem::OutputFunctionCall(
                shell_call_to_function_call(call),
            )),
            ResponsesInputItem::ShellCallOutput(output) => Some(
                ResponsesInputItem::FunctionCallOutput(shell_output_to_function_output(output)),
            ),
            _ => None,
        };
        if let Some(rewritten) = rewritten {
            *item = rewritten;
        }
    }
}

/// Reconstruct the model-emitted `function_call` from a stored `shell_call`.
/// The arguments mirror the `shell` function-tool schema
/// (`{ "action": { … } }`). `id` is left unset: the stored `shell_call`
/// reuses the model's `call_id` as its `id` (not a valid `fc_…` output id),
/// and the API assigns item ids on output anyway — pairing is by `call_id`.
fn shell_call_to_function_call(call: &ShellCall) -> OutputItemFunctionCall {
    let arguments = serde_json::to_string(&serde_json::json!({ "action": call.action }))
        .unwrap_or_else(|_| "{}".to_string());
    OutputItemFunctionCall {
        type_: OutputItemFunctionCallType::FunctionCall,
        id: None,
        name: ShellToolArguments::FUNCTION_NAME.to_string(),
        arguments,
        call_id: call.call_id.clone(),
        status: None,
    }
}

/// Reconstruct the `function_call_output` from a stored `shell_call_output`,
/// flattening the array `output` into the plain-text `exit_code/stdout/stderr`
/// blob the live tool loop feeds back to the model (see the continuation item
/// built in `ShellExecutor::execute`).
fn shell_output_to_function_output(output: &ShellCallOutputItem) -> FunctionCallOutput {
    FunctionCallOutput {
        type_: FunctionCallOutputType::FunctionCallOutput,
        id: None,
        call_id: output.call_id.clone(),
        output: render_shell_output_text(output),
        status: None,
    }
}

/// Flatten a `shell_call_output`'s content chunks (and any captured
/// `output_files` manifest) into the text result the model saw on the
/// original turn.
fn render_shell_output_text(item: &ShellCallOutputItem) -> String {
    let mut combined = item
        .output
        .iter()
        .map(|chunk| {
            let exit_code = match &chunk.outcome {
                ShellCallOutcome::Exit { exit_code } => *exit_code,
                // Canonical `timeout(1)` exit code; matches how the live
                // loop reports a killed call.
                ShellCallOutcome::Timeout => 124,
            };
            format!(
                "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                exit_code, chunk.stdout, chunk.stderr
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !item.output_files.is_empty() {
        // Only separate from the chunk text when there is some; an empty
        // `output` array (no chunks) would otherwise leave a leading newline.
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("output_files:\n");
        for f in &item.output_files {
            combined.push_str(&format!("- {} ({} bytes)\n", f.path, f.bytes));
        }
    }
    combined
}

// ─────────────────────────────────────────────────────────────────────────────
// Detection
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ShellToolCall {
    id: String,
    args: ResolvedShellArgs,
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
    let args = ShellToolArguments::parse(arguments_str)?.resolve()?;
    Some(ShellToolCall { id, args })
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

/// Item lifecycle stage. Drives the `response.output_item.<verb>` event
/// type Hadrian emits and the corresponding `status` on the carried item.
/// Spec lifecycle is `added → done`; SDK consumers hook the typed event
/// stream off these names.
#[derive(Debug, Clone, Copy)]
enum ItemLifecycle {
    Added,
    Done,
}

impl ItemLifecycle {
    fn event_type(self) -> &'static str {
        match self {
            ItemLifecycle::Added => "response.output_item.added",
            ItemLifecycle::Done => "response.output_item.done",
        }
    }
}

/// Build the `shell_call` item JSON, shared between `output_item.added`
/// (status `in_progress`) and `output_item.done` (status
/// `completed` / `incomplete`). Per OpenAI's `FunctionShellCall` shape:
/// `{id, call_id, action: {commands, timeout_ms?, max_output_length?},
/// status, environment}`.
#[allow(clippy::too_many_arguments)]
fn build_shell_call_item(
    item_id: &str,
    call_id: &str,
    commands: &[String],
    timeout_ms: Option<u64>,
    max_output_length: Option<usize>,
    env: Option<&HashMap<String, String>>,
    working_directory: Option<&str>,
    status: &str,
    environment: Option<&serde_json::Value>,
    created_by: Option<&str>,
) -> serde_json::Value {
    let mut action = serde_json::json!({
        "commands": commands,
    });
    if let Some(ms) = timeout_ms {
        action["timeout_ms"] = serde_json::Value::from(ms);
    }
    if let Some(n) = max_output_length {
        action["max_output_length"] = serde_json::Value::from(n);
    }
    if let Some(env) = env
        && !env.is_empty()
    {
        action["env"] = serde_json::to_value(env).unwrap_or(serde_json::Value::Null);
    }
    if let Some(wd) = working_directory {
        action["working_directory"] = serde_json::Value::from(wd);
    }
    let mut item = serde_json::json!({
        "type": "shell_call",
        "id": item_id,
        "call_id": call_id,
        "action": action,
        "status": status,
    });
    if let Some(env_val) = environment {
        item["environment"] = env_val.clone();
    }
    if let Some(c) = created_by {
        item["created_by"] = serde_json::Value::from(c);
    }
    item
}

/// Emit a structured `response.output_item.<lifecycle>` event carrying
/// a spec-compliant `shell_call` item.
///
/// Hadrian synthesizes both `added` (status `in_progress`) and `done`
/// (terminal status) events so non-passthrough runtimes (`microsandbox`
/// / `opensandbox`) match the wire contract OpenAI's hosted shell emits
/// natively. For `passthrough_openai` the upstream already emits these
/// items and we don't double-emit.
#[allow(clippy::too_many_arguments)]
fn format_shell_call_item(
    lifecycle: ItemLifecycle,
    item_id: &str,
    call_id: &str,
    output_index: usize,
    commands: &[String],
    timeout_ms: Option<u64>,
    max_output_length: Option<usize>,
    env: Option<&HashMap<String, String>>,
    working_directory: Option<&str>,
    status: &str,
    environment: Option<&serde_json::Value>,
    created_by: Option<&str>,
) -> Bytes {
    let item = build_shell_call_item(
        item_id,
        call_id,
        commands,
        timeout_ms,
        max_output_length,
        env,
        working_directory,
        status,
        environment,
        created_by,
    );
    sse_event(serde_json::json!({
        "type": lifecycle.event_type(),
        "output_index": output_index,
        "item": item,
    }))
}

/// Emit a structured `response.output_item.done` event carrying a
/// spec-compliant `shell_call_output` item. Per OpenAI's
/// `FunctionShellCallOutput` shape: `{id, call_id, status,
/// output: [{stdout, stderr, outcome: {type: "exit"|"timeout", exit_code?}}],
/// max_output_length?}`. The Hadrian-extension `output_files` array
/// rides alongside.
///
/// The standard `function_call_output` continuation item still flows
/// through `apply_to_continuation` for the model's next-turn input —
/// this event is purely additive on the client-facing SSE stream.
#[allow(clippy::too_many_arguments)]
fn format_shell_call_output_item(
    lifecycle: ItemLifecycle,
    item_id: &str,
    call_id: &str,
    output_index: usize,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    files: &[ContainerFileRef],
    killed: bool,
    max_output_length: Option<usize>,
    created_by: Option<&str>,
) -> Bytes {
    // `incomplete` whenever we never observed a normal exit — i.e. the
    // call was killed by timeout or the runtime didn't report. Spec
    // uses `incomplete` for "abandoned mid-flight" outcomes. The added
    // lifecycle always carries `in_progress` per the spec.
    let status_str = match lifecycle {
        ItemLifecycle::Added => "in_progress",
        ItemLifecycle::Done if killed => "incomplete",
        ItemLifecycle::Done => "completed",
    };
    // Outcome discriminator: `timeout` for killed calls (we don't get
    // a real exit code from the runtime), `exit` otherwise.
    let outcome = if killed {
        serde_json::json!({ "type": "timeout" })
    } else {
        serde_json::json!({ "type": "exit", "exit_code": exit_code })
    };
    let mut content = serde_json::json!({
        "stdout": stdout,
        "stderr": stderr,
        "outcome": outcome,
    });
    if let Some(c) = created_by {
        content["created_by"] = serde_json::Value::from(c);
    }
    let mut item = serde_json::json!({
        "type": "shell_call_output",
        "id": item_id,
        "call_id": call_id,
        "status": status_str,
        "output": [content],
        // Hadrian Extension: captured file manifest. Spec leaves the
        // shell_call_output content array open for additive properties.
        "output_files": files,
    });
    if let Some(n) = max_output_length {
        item["max_output_length"] = serde_json::Value::from(n);
    }
    if let Some(c) = created_by {
        item["created_by"] = serde_json::Value::from(c);
    }
    sse_event(serde_json::json!({
        "type": lifecycle.event_type(),
        "output_index": output_index,
        "item": item,
    }))
}

/// Emit the spec-canonical `output_item.done` events for both the
/// `shell_call` and `shell_call_output` items when a shell call fails
/// before producing real output (boot failure, passthrough misconfig,
/// exec error). Both items carry `status: "incomplete"`; the output
/// item's stderr surfaces the error message so the model can react.
#[allow(clippy::too_many_arguments)]
async fn emit_failure_done(
    event_tx: &mpsc::Sender<Bytes>,
    id: &str,
    commands: &[String],
    timeout_ms: Option<u64>,
    max_output_length: Option<usize>,
    env: Option<&HashMap<String, String>>,
    working_directory: Option<&str>,
    stderr: &str,
) {
    let _ = event_tx
        .send(format_shell_call_item(
            ItemLifecycle::Done,
            id,
            id,
            0,
            commands,
            timeout_ms,
            max_output_length,
            env,
            working_directory,
            "incomplete",
            None,
            Some("model"),
        ))
        .await;
    let _ = event_tx
        .send(format_shell_call_output_item(
            ItemLifecycle::Done,
            id,
            id,
            0,
            -1,
            "",
            stderr,
            &[],
            true,
            max_output_length,
            Some("gateway"),
        ))
        .await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-call env / working_directory threading
// ─────────────────────────────────────────────────────────────────────────────

/// POSIX-ish environment variable name check: starts with a letter or
/// underscore, followed by letters, digits, or underscores. Rejects names
/// that could break out of the synthesized `export` statement.
fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Single-quotes a string for safe inclusion in a POSIX shell command,
/// escaping embedded single quotes (`'` → `'\''`).
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Builds the effective shell script honoring the model's per-call
/// `working_directory` and `env`. Runtimes accept only a single command
/// string and expose no separate cwd/env channel, so we prepend a
/// validated, shell-escaped `cd`/`export` preamble to `command`. A
/// missing working directory fails the call loudly (`|| exit 1`) rather
/// than silently running in the default cwd. Returns `InvalidCall` for a
/// malformed env name.
fn build_effective_command(
    command: &str,
    env: Option<&HashMap<String, String>>,
    working_directory: Option<&str>,
) -> Result<String, ToolError> {
    let mut preamble = String::new();
    if let Some(wd) = working_directory.map(str::trim).filter(|s| !s.is_empty()) {
        preamble.push_str(&format!("cd {} || exit 1\n", shell_single_quote(wd)));
    }
    if let Some(env) = env {
        // Deterministic order so the synthesized script is stable.
        let mut names: Vec<&String> = env.keys().collect();
        names.sort();
        for name in names {
            if !is_valid_env_name(name) {
                return Err(ToolError::InvalidCall(format!(
                    "invalid environment variable name: {name}"
                )));
            }
            preamble.push_str(&format!(
                "export {}={}\n",
                name,
                shell_single_quote(&env[name])
            ));
        }
    }
    if preamble.is_empty() {
        Ok(command.to_string())
    } else {
        Ok(format!("{preamble}{command}"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Output trimming for continuation payload
// ─────────────────────────────────────────────────────────────────────────────

/// Default operator cap for `[features.server_tools.shell_limits].max_output_chars`.
/// Used as the fallback the `ShellToolHint::default()` description embeds; the
/// configured value flows in via [`ShellExecutor`] at execute time.
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 8_000;

/// Streaming UTF-8 decoder that buffers bytes split across chunk
/// boundaries instead of emitting a `U+FFFD` per partial sequence.
/// Callers feed in raw stdout/stderr bytes via [`Self::push`] and read
/// out fully-decoded strings; whatever's left over (≤ 3 trailing bytes
/// of an incomplete code point) is carried into the next call.
struct Utf8ChunkDecoder {
    /// Bytes that belong to an in-progress UTF-8 sequence at the tail
    /// of the previous push. Always < 4 bytes.
    leftover: Vec<u8>,
}

impl Utf8ChunkDecoder {
    fn new() -> Self {
        Self {
            leftover: Vec::with_capacity(4),
        }
    }

    /// Append `bytes` to the pending buffer and return everything we
    /// can prove is a complete UTF-8 sequence. Invalid sequences are
    /// turned into replacement chars (consistent with
    /// `String::from_utf8_lossy`); the very last possibly-partial
    /// sequence is held back for the next call.
    fn push(&mut self, bytes: &[u8]) -> String {
        if bytes.is_empty() && self.leftover.is_empty() {
            return String::new();
        }
        let combined: Vec<u8> = if self.leftover.is_empty() {
            bytes.to_vec()
        } else {
            let mut v = std::mem::take(&mut self.leftover);
            v.extend_from_slice(bytes);
            v
        };

        // Find the largest prefix of `combined` that's complete. A
        // trailing UTF-8 sequence is incomplete if its leading byte
        // promises more continuation bytes than we've seen.
        let mut boundary = combined.len();
        let max_lookback = boundary.min(3);
        for back in 1..=max_lookback {
            let i = boundary - back;
            let b = combined[i];
            if b < 0x80 {
                break; // ASCII — nothing partial above this
            }
            // Continuation byte (10xxxxxx): keep scanning back.
            if b & 0b1100_0000 == 0b1000_0000 {
                continue;
            }
            // Leading byte: figure out how many bytes the sequence
            // should occupy and see whether we have them all.
            let needed = if b & 0b1110_0000 == 0b1100_0000 {
                2
            } else if b & 0b1111_0000 == 0b1110_0000 {
                3
            } else if b & 0b1111_1000 == 0b1111_0000 {
                4
            } else {
                // Invalid leading byte — let the lossy decoder mark
                // it `U+FFFD` rather than holding back forever.
                break;
            };
            if combined.len() - i < needed {
                boundary = i;
            }
            break;
        }

        self.leftover.clear();
        if boundary < combined.len() {
            self.leftover.extend_from_slice(&combined[boundary..]);
        }
        String::from_utf8_lossy(&combined[..boundary]).into_owned()
    }

    /// Flush whatever's left, treating any straggling partial sequence
    /// as malformed (renders as `U+FFFD`).
    fn flush(&mut self) -> String {
        if self.leftover.is_empty() {
            return String::new();
        }
        let s = String::from_utf8_lossy(&self.leftover).into_owned();
        self.leftover.clear();
        s
    }
}

/// Bounded buffer that head + tail trims as bytes arrive so a runaway
/// stdout doesn't pin gigabytes of memory before the post-exec trim
/// runs. Total visible characters are capped at `max_chars`: the first
/// `max_chars / 2` chars land in `head`, the last `max_chars / 2` chars
/// in a rotating `tail` ring, and everything in between is counted but
/// discarded.
///
/// Final rendering via [`Self::into_trimmed`] emits `head` + a
/// `... N chars truncated ...` marker + `tail` when the stream
/// overflowed `max_chars`; otherwise the captured prefix is returned
/// verbatim. Matches the contract the function-mode shell tool
/// description embeds ("truncated past {N} chars").
struct BoundedHeadTail {
    head: String,
    head_chars: usize,
    /// Ring of the most recent tail chars sized at `half`. Below the
    /// head fill we accumulate into `head` only.
    tail: std::collections::VecDeque<char>,
    /// Total chars seen (used to compute the truncation count and
    /// avoid mixing bytes and chars).
    total_chars: usize,
    /// Per-side cap. Head takes the first `half` chars, tail rotates
    /// the most recent `half` chars after that.
    half: usize,
    max_chars: usize,
}

impl BoundedHeadTail {
    fn new(max_chars: usize) -> Self {
        let half = max_chars / 2;
        Self {
            head: String::with_capacity(half.min(8 * 1024)),
            head_chars: 0,
            tail: std::collections::VecDeque::new(),
            total_chars: 0,
            half,
            max_chars,
        }
    }

    fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            self.total_chars += 1;
            if self.head_chars < self.half {
                self.head.push(c);
                self.head_chars += 1;
                continue;
            }
            // Past the head fill: rotate into a `half`-sized ring.
            if self.tail.len() == self.half {
                self.tail.pop_front();
            }
            self.tail.push_back(c);
        }
    }

    fn into_trimmed(self) -> String {
        if self.total_chars <= self.max_chars {
            // Head + tail combined still fit under the cap; just glue
            // them (no truncation marker).
            let head = self.head;
            let tail: String = self.tail.into_iter().collect();
            return format!("{head}{tail}");
        }
        let head = self.head;
        let tail: String = self.tail.into_iter().collect();
        let dropped = self.total_chars - self.max_chars;
        format!("{head}\n... [{dropped} chars truncated] ...\n{tail}")
    }

    /// True when the stream exceeded `max_chars` and the rendered
    /// output had to insert a truncation marker.
    fn was_truncated(&self) -> bool {
        self.total_chars > self.max_chars
    }
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
    /// generates a fresh id on first use (in-memory-only deployments).
    container_id_hint: Option<String>,
    /// Files captured across every shell call in this response,
    /// keyed by path so an overwrite replaces the prior entry. Read
    /// synchronously by `transform_event` to populate
    /// `container_file_citation` annotations on output_text events.
    captured_files: Arc<std::sync::Mutex<HashMap<String, ContainerFileRef>>>,
    /// Hides the rewritten `shell` function-call plumbing from the
    /// client stream; the executor emits the spec-shaped `shell_call` /
    /// `shell_call_output` items itself.
    suppressor: crate::services::server_tools::FunctionCallSuppressor,
    /// `input_file` parts the request asked us to stage into
    /// `/mnt/data` before the first shell command. Drained on the
    /// first `execute()` call; subsequent calls see `None`.
    pending_input_files: Arc<Mutex<Option<Vec<StagedFile>>>>,
    /// Optional database write-through. When `Some`, a `containers`
    /// row is inserted on session start and every captured file is
    /// upserted into `container_files`; when `None`, the session
    /// runs entirely in-memory.
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
            suppressor: crate::services::server_tools::FunctionCallSuppressor::new(),
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
                            ResponsesToolDefinition::Function(f)
                                if f.name == ShellToolArguments::FUNCTION_NAME
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
                    "commands": tc.args.commands,
                    "stdin": tc.args.stdin,
                    "timeout_ms": tc.args.timeout_ms,
                    "max_output_length": tc.args.max_output_length,
                    "env": tc.args.env,
                    "working_directory": tc.args.working_directory,
                }),
            })
            .collect()
    }

    async fn execute(
        &self,
        call: DetectedToolCall,
        _ctx: &ToolContext,
    ) -> Result<ToolExecutionHandle, ToolError> {
        let commands: Vec<String> = call
            .arguments
            .get("commands")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // Joined script — runtimes only accept a single string today,
        // so multi-command actions are concatenated and run as a shell
        // script. Final exit code is the script's.
        let command = commands.join("\n");
        let stdin = call
            .arguments
            .get("stdin")
            .and_then(|v| v.as_str())
            .map(|s| Bytes::from(s.to_string()));
        let model_timeout_ms = call.arguments.get("timeout_ms").and_then(|v| v.as_u64());
        let model_max_output_length = call
            .arguments
            .get("max_output_length")
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok());
        let model_env: Option<HashMap<String, String>> = call
            .arguments
            .get("env")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let model_workdir = call
            .arguments
            .get("working_directory")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // Honor the model's per-call working_directory/env by synthesizing a
        // validated `cd`/`export` preamble — runtimes accept only a single
        // command string with no separate cwd/env channel. The echoed
        // shell_call item still reports the raw `model_env`/`model_workdir`.
        let command =
            build_effective_command(&command, model_env.as_ref(), model_workdir.as_deref())?;
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

        // Emit the spec-canonical `output_item.added` lifecycle events
        // for the `shell_call` and (placeholder) `shell_call_output`
        // items before any I/O. SDKs typed against the Responses API
        // stream pick the items up here; the matching `output_item.done`
        // events fire from the exec-complete branch below with the
        // final status and content.
        //
        // `environment` is omitted on the `added` event because the
        // resolved container_id isn't known until boot completes; the
        // `done` event carries it.
        let added_shell_call = format_shell_call_item(
            ItemLifecycle::Added,
            &id,
            &id,
            0,
            &commands,
            model_timeout_ms,
            model_max_output_length,
            model_env.as_ref(),
            model_workdir.as_deref(),
            "in_progress",
            None,
            Some("model"),
        );
        let _ = event_tx.send(added_shell_call).await;
        let added_shell_output = format_shell_call_output_item(
            ItemLifecycle::Added,
            &id,
            &id,
            0,
            0,
            "",
            "",
            &[],
            false,
            model_max_output_length,
            Some("gateway"),
        );
        let _ = event_tx.send(added_shell_output).await;

        // Spawn the actual session work so the orchestrator can start
        // consuming events while we boot the container.
        let id_for_task = id.clone();
        let command_for_task = command.clone();
        let commands_for_task = commands.clone();
        let working_directory_for_task = model_workdir.clone();
        // Capture model-side action fields so we can emit a spec-shaped
        // `shell_call` item after exec completes (`action.timeout_ms`,
        // `action.env`, `action.max_output_length`).
        let action_timeout_ms_for_task = model_timeout_ms;
        let action_env_for_task = model_env.clone();
        // Apply the model's per-call timeout when given; clamp to the
        // operator cap. `timeout_ms < 1000` rounds up to one second.
        let op_timeout_ms = self
            .limits
            .command_timeout_secs
            .saturating_mul(1000)
            .max(1000);
        let effective_timeout_ms = match model_timeout_ms {
            Some(ms) if ms > 0 => ms.min(op_timeout_ms),
            _ => op_timeout_ms,
        };
        let exec_timeout = Duration::from_millis(effective_timeout_ms.max(1));
        let default_cpu = self.limits.default_cpu_limit;
        // Per-call cap on stdout+stderr fed back to the model. Clamped
        // to the operator's `max_output_chars` cap.
        let op_max_chars = self.limits.max_output_chars.max(64);
        let max_output_chars = match model_max_output_length {
            Some(n) if n > 0 => n.min(op_max_chars).max(64),
            _ => op_max_chars,
        };
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
            //
            // When `container_id_hint` is set, the get-or-insert path
            // is atomic via `ContainerSessionRegistry::get_or_try_insert_with`
            // so two concurrent requests racing on the same hint boot
            // exactly one VM rather than booting two and terminating
            // the loser mid-exec.
            let session_arc: Arc<Mutex<ContainerSession>> = {
                let mut handle_slot = session_handle_slot.lock().await;
                if let Some(existing) = handle_slot.as_ref() {
                    existing.clone()
                } else {
                    let spec_template = || SessionSpec {
                        mounted_skills: mounted_skills.clone(),
                        cpu_limit: default_cpu,
                        mem_limit_bytes: resolved_env.mem_limit_bytes,
                        egress_policy: resolved_env.egress_policy.clone(),
                        ..SessionSpec::default()
                    };

                    // Async boot helper kept inline so it can close
                    // over `runtime`, `containers_config`, etc.
                    let runtime_for_boot = runtime.clone();
                    let runtime_label_for_boot = runtime_label;
                    let containers_config_for_boot = containers_config.clone();
                    let persistence_for_boot = persistence.clone();
                    let hint_for_boot = container_id_hint.clone();

                    let idle_ttl_override = resolved_env.idle_ttl_secs;
                    let boot_session = move || async move {
                        let spec = spec_template();
                        match (hint_for_boot, persistence_for_boot.clone()) {
                            (Some(cid), Some(p)) => {
                                ContainerSession::start_attached(
                                    cid,
                                    runtime_for_boot,
                                    runtime_label_for_boot,
                                    spec,
                                    containers_config_for_boot,
                                    p,
                                    idle_ttl_override,
                                )
                                .await
                            }
                            _ => {
                                ContainerSession::start_new(
                                    runtime_for_boot,
                                    runtime_label_for_boot,
                                    spec,
                                    containers_config_for_boot,
                                    persistence_for_boot,
                                    idle_ttl_override,
                                )
                                .await
                            }
                        }
                    };

                    let resolved: Arc<Mutex<ContainerSession>> = match container_id_hint.as_deref()
                    {
                        Some(hint) => {
                            // Atomic get-or-create under the hint id.
                            // The registry's CAS guarantees at most one
                            // VM gets booted across racing requests.
                            match registry
                                .get_or_try_insert_with(hint.to_string(), boot_session)
                                .await
                            {
                                Ok((arc, inserted)) => {
                                    debug!(
                                        stage = "container_session_resolved",
                                        call_id = %id_for_task,
                                        container_id = %hint,
                                        booted = inserted,
                                        "Resolved container session via registry CAS"
                                    );
                                    arc
                                }
                                Err(RuntimeError::Passthrough) => {
                                    warn!(
                                        stage = "passthrough_invoked",
                                        call_id = %id_for_task,
                                        "Passthrough runtime received an execute() call; \
                                         this indicates a misconfiguration in chat.rs registration"
                                    );
                                    emit_failure_done(
                                        &event_tx,
                                        &id_for_task,
                                        &commands_for_task,
                                        action_timeout_ms_for_task,
                                        model_max_output_length,
                                        action_env_for_task.as_ref(),
                                        working_directory_for_task.as_deref(),
                                        "shell runtime is configured for passthrough but \
                                         executor was invoked",
                                    )
                                    .await;
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
                                    let err_text = e.to_string();
                                    emit_failure_done(
                                        &event_tx,
                                        &id_for_task,
                                        &commands_for_task,
                                        action_timeout_ms_for_task,
                                        model_max_output_length,
                                        action_env_for_task.as_ref(),
                                        working_directory_for_task.as_deref(),
                                        &err_text,
                                    )
                                    .await;
                                    let _ =
                                        result_tx.send(Err(ToolError::ExecutionFailed(err_text)));
                                    return;
                                }
                            }
                        }
                        None => {
                            // No hint: each request boots its own
                            // fresh container. We still register so
                            // subsequent chains can reattach.
                            let session = match boot_session().await {
                                Ok(s) => {
                                    debug!(
                                        stage = "container_session_started",
                                        call_id = %id_for_task,
                                        container_id = %s.container_id,
                                        file_io = s.file_io_enabled(),
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
                                    emit_failure_done(
                                        &event_tx,
                                        &id_for_task,
                                        &commands_for_task,
                                        action_timeout_ms_for_task,
                                        model_max_output_length,
                                        action_env_for_task.as_ref(),
                                        working_directory_for_task.as_deref(),
                                        "shell runtime is configured for passthrough but \
                                         executor was invoked",
                                    )
                                    .await;
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
                                    let err_text = e.to_string();
                                    emit_failure_done(
                                        &event_tx,
                                        &id_for_task,
                                        &commands_for_task,
                                        action_timeout_ms_for_task,
                                        model_max_output_length,
                                        action_env_for_task.as_ref(),
                                        working_directory_for_task.as_deref(),
                                        &err_text,
                                    )
                                    .await;
                                    let _ =
                                        result_tx.send(Err(ToolError::ExecutionFailed(err_text)));
                                    return;
                                }
                            };
                            let cid = session.container_id.clone();
                            let (arc, _displaced) = registry.insert(cid, session);
                            // No `container_id_hint` ⇒ the id we just
                            // picked is fresh, so a `displaced` here
                            // would only happen against an unrelated
                            // session under the same brand-new UUID —
                            // statistically impossible (32 hex chars).
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
            // Snapshot the container_id now so we can build the
            // `shell_call.environment.container_reference` after exec
            // completes — by then `session_guard` has been dropped.
            let container_id_for_items = session.container_id.clone();

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
                        // Mirror into captured_files so the very first
                        // assistant message picks them up in
                        // annotations, even if the shell command
                        // itself produces no further output.
                        // Poison recovery: see `transform_event`; the
                        // map contents are still consistent after a
                        // panic in another holder.
                        let mut guard = match captured_files.lock() {
                            Ok(g) => g,
                            Err(poisoned) => poisoned.into_inner(),
                        };
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
                    let _ = start.elapsed();
                    let err_text = e.to_string();
                    emit_failure_done(
                        &event_tx,
                        &id_for_task,
                        &commands_for_task,
                        action_timeout_ms_for_task,
                        model_max_output_length,
                        action_env_for_task.as_ref(),
                        working_directory_for_task.as_deref(),
                        &err_text,
                    )
                    .await;
                    let _ = result_tx.send(Err(ToolError::ExecutionFailed(err_text)));
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
            //
            // Output handling: we feed bytes through a `Utf8ChunkDecoder`
            // so multi-byte sequences split across stdout chunks don't
            // emit a `U+FFFD` per chunk boundary, and into a
            // `BoundedHeadTail` so an `echo "$(head -c 1G /dev/urandom)"`
            // can't pin gigabytes of memory before the post-exec trim
            // runs. The SSE chunk is still emitted from the original
            // bytes so observers downstream see the raw stream.
            let max_chars = max_output_chars;
            let mut stdout_decoder = Utf8ChunkDecoder::new();
            let mut stderr_decoder = Utf8ChunkDecoder::new();
            let mut stdout_buf = BoundedHeadTail::new(max_chars);
            let mut stderr_buf = BoundedHeadTail::new(max_chars);
            let mut final_exit: Option<i32> = None;
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
                        // OpenAI's Responses API spec has no per-delta
                        // streaming event for `shell_call`; stdout/stderr
                        // surface in a single `shell_call_output` item via
                        // `response.output_item.done` after exec. We
                        // accumulate into bounded head/tail buffers here
                        // and rely on the `event_tx.closed()` arm above
                        // for client-disconnect detection.
                        match ev {
                            ExecEvent::Stdout(bytes) => {
                                let decoded = stdout_decoder.push(&bytes);
                                if !decoded.is_empty() {
                                    stdout_buf.push_str(&decoded);
                                }
                            }
                            ExecEvent::Stderr(bytes) => {
                                let decoded = stderr_decoder.push(&bytes);
                                if !decoded.is_empty() {
                                    stderr_buf.push_str(&decoded);
                                }
                            }
                            ExecEvent::Exit { code, .. } => {
                                final_exit = Some(code);
                            }
                        }
                    }
                }
            }
            // Flush any straggling bytes from a multi-byte sequence
            // that wasn't completed before the runtime closed the stream.
            let stdout_tail = stdout_decoder.flush();
            if !stdout_tail.is_empty() {
                stdout_buf.push_str(&stdout_tail);
            }
            let stderr_tail = stderr_decoder.flush();
            if !stderr_tail.is_empty() {
                stderr_buf.push_str(&stderr_tail);
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
                let _ = &new_files;
            }

            // Replace the global captured_files map for this response
            // with the session's full tracked set (handles overwrites
            // and deletions consistently with what the model sees).
            let all_tracked = session.list_captured().await;
            {
                let mut guard = match captured_files.lock() {
                    Ok(g) => g,
                    Err(poisoned) => poisoned.into_inner(),
                };
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
            // Resolve the exit code for downstream reporting. `final_exit
            // == None` means the runtime stream closed without an `Exit`
            // event — surface that as `-1` to metrics (the sentinel
            // `record_shell_execution` uses for error exits) but preserve
            // `None` on the usage row so a later audit can tell "process
            // exited 0" from "process never reported an exit." Clients
            // see this case as `status: "incomplete"` with a `timeout`
            // outcome on the `shell_call_output` item.
            let exit_for_report = final_exit.unwrap_or(-1);

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
                        match (client_disconnected, final_exit) {
                            (true, _) => "client_disconnected",
                            (false, None) => "no_exit_event",
                            (false, Some(_)) => "completed",
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
                    tool_exit_code: final_exit,
                });
            }
            #[cfg(not(feature = "concurrency"))]
            let _ = (&principal, command_for_task.clone());

            if client_disconnected {
                crate::observability::metrics::record_shell_execution(
                    duration_secs,
                    exit_for_report,
                    "client_disconnected",
                    runtime_label,
                    cost_microcents,
                );
                // Drop both channels without sending — the orchestrator
                // is gone, no one is listening.
                return;
            }

            // `killed` ≈ runtime returned the timeout sentinel (124 is
            // the canonical `timeout(1)` exit code; we also surface
            // `None` exits — the process didn't report — as killed.
            let killed = final_exit == Some(124) || final_exit.is_none();
            let _ = stdout_buf.was_truncated() || stderr_buf.was_truncated();

            // Emit the spec-shaped `shell_call` and `shell_call_output`
            // items so clients built against the OpenAI Responses-API
            // spec see the same wire contract regardless of which
            // runtime executed the call. Hadrian's additive bits
            // (`output_files` etc.) ride alongside as extra properties.
            //
            // Snapshot the trimmed stdout/stderr for this event before
            // building anything — `into_trimmed` consumes the buffers
            // and we want them both for the SSE event and the
            // continuation text.
            let stdout_render =
                std::mem::replace(&mut stdout_buf, BoundedHeadTail::new(max_chars)).into_trimmed();
            let stderr_render =
                std::mem::replace(&mut stderr_buf, BoundedHeadTail::new(max_chars)).into_trimmed();
            // Spec status: `incomplete` when we killed/timed out,
            // `completed` otherwise (a non-zero exit code still counts
            // as `completed` per spec).
            let shell_call_status = if killed { "incomplete" } else { "completed" };
            let environment_val = serde_json::json!({
                "type": "container_reference",
                "container_id": container_id_for_items,
            });
            let _ = event_tx
                .send(format_shell_call_item(
                    ItemLifecycle::Done,
                    &id_for_task,
                    &id_for_task,
                    0,
                    &commands_for_task,
                    action_timeout_ms_for_task,
                    model_max_output_length,
                    action_env_for_task.as_ref(),
                    working_directory_for_task.as_deref(),
                    shell_call_status,
                    Some(&environment_val),
                    Some("model"),
                ))
                .await;
            let _ = event_tx
                .send(format_shell_call_output_item(
                    ItemLifecycle::Done,
                    &id_for_task,
                    &id_for_task,
                    0,
                    exit_for_report,
                    &stdout_render,
                    &stderr_render,
                    &new_files,
                    killed,
                    model_max_output_length,
                    Some("gateway"),
                ))
                .await;
            info!(
                stage = "shell_completed",
                call_id = %id_for_task,
                exit_code = exit_for_report,
                exit_observed = final_exit.is_some(),
                duration_ms = (duration_secs * 1000.0) as u64,
                cost_microcents,
                runtime = runtime_label,
                "Shell command completed"
            );
            crate::observability::metrics::record_shell_execution(
                duration_secs,
                exit_for_report,
                if final_exit.is_some() {
                    "completed"
                } else {
                    "no_exit_event"
                },
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
                exit_for_report, stdout_render, stderr_render, files_section,
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
                if let ResponsesToolDefinition::Function(f) = t {
                    f.name != ShellToolArguments::FUNCTION_NAME
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
        // Hide the rewritten `shell` function-call plumbing first; the
        // executor emits the spec-shaped `shell_call` / `shell_call_output`
        // items itself.
        let event = self
            .suppressor
            .suppress(event, |name| name == ShellToolArguments::FUNCTION_NAME);
        if event.is_empty() {
            return event;
        }
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
            ContainerExpiresAfter, ContainerExpiresAfterAnchor, ShellContainerAuto,
            ShellContainerReference, ShellDomainSecret, ShellDomainSecretInline,
            ShellDomainSecretRef, ShellEnvironment, ShellNetworkPolicy, ShellNetworkPolicyType,
        },
        config::AllowedDomainSecret,
    };

    fn op_limits_with(
        max_mb: Option<u32>,
        hosts: &[&str],
        secrets: &[(&str, &str, &[&str])],
    ) -> ShellLimitsConfig {
        let mut limits = ShellLimitsConfig {
            max_mem_limit_mb: max_mb,
            allowed_egress_hosts: hosts.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        };
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

    fn default_containers() -> ContainersConfig {
        ContainersConfig::default()
    }

    fn auto_env(memory_limit: Option<&str>, net: Option<ShellNetworkPolicy>) -> ShellEnvironment {
        ShellEnvironment::ContainerAuto(ShellContainerAuto {
            memory_limit: memory_limit.map(str::to_string),
            network_policy: net,
            file_ids: None,
            skills: None,
            expires_after: None,
        })
    }

    fn net_policy(domains: &[&str], secrets: Vec<ShellDomainSecret>) -> ShellNetworkPolicy {
        ShellNetworkPolicy {
            type_: ShellNetworkPolicyType::Known(
                crate::api_types::responses::KnownShellNetworkPolicyType::Allowlist,
            ),
            allowed_domains: domains.iter().map(|s| (*s).to_string()).collect(),
            domain_secrets: secrets,
        }
    }

    #[test]
    fn resolver_none_inherits_operator_default_memory() {
        let limits = ShellLimitsConfig {
            default_mem_limit_mb: Some(512),
            ..Default::default()
        };
        let r = resolve_shell_environment(None, &limits, &default_containers()).unwrap();
        assert_eq!(r.mem_limit_bytes, Some(512 * 1024 * 1024));
        assert!(r.egress_policy.allow_hosts.is_empty());
        assert!(r.egress_policy.secrets.is_empty());
        assert!(r.referenced_container_id.is_none());
        assert!(r.idle_ttl_secs.is_none());
    }

    #[test]
    fn resolver_memory_request_within_cap() {
        let limits = op_limits_with(Some(2048), &[], &[]);
        let env = auto_env(Some("1g"), None);
        let r = resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap();
        assert_eq!(r.mem_limit_bytes, Some(1024 * 1024 * 1024));
    }

    #[test]
    fn resolver_memory_request_exceeds_cap_rejected() {
        let limits = op_limits_with(Some(512), &[], &[]);
        let env = auto_env(Some("1g"), None);
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
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
        let env = auto_env(Some("64g"), None);
        let r = resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap();
        assert_eq!(r.mem_limit_bytes, Some(64 * 1024 * 1024 * 1024));
    }

    #[test]
    fn resolver_expires_after_within_cap_flows_through() {
        let limits = op_limits_with(None, &[], &[]);
        let containers = default_containers();
        let env = ShellEnvironment::ContainerAuto(ShellContainerAuto {
            expires_after: Some(ContainerExpiresAfter {
                anchor: ContainerExpiresAfterAnchor::LastActiveAt,
                minutes: 30,
            }),
            ..Default::default()
        });
        let r = resolve_shell_environment(Some(&env), &limits, &containers).unwrap();
        assert_eq!(r.idle_ttl_secs, Some(30 * 60));
    }

    #[test]
    fn resolver_expires_after_exceeds_cap_rejected() {
        let limits = op_limits_with(None, &[], &[]);
        let mut containers = default_containers();
        containers.max_idle_ttl_secs = 600; // 10 min cap
        let env = ShellEnvironment::ContainerAuto(ShellContainerAuto {
            expires_after: Some(ContainerExpiresAfter {
                anchor: ContainerExpiresAfterAnchor::LastActiveAt,
                minutes: 30,
            }),
            ..Default::default()
        });
        let err = resolve_shell_environment(Some(&env), &limits, &containers).unwrap_err();
        assert!(
            matches!(
                err,
                ShellEnvironmentError::ExpiresAfterExceedsCap {
                    requested: 30,
                    max: 10,
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn resolver_memory_unparseable_rejected() {
        let limits = ShellLimitsConfig::default();
        let env = auto_env(Some("huge"), None);
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::BadMemoryLimit(_)));
    }

    #[test]
    fn resolver_egress_subset_accepted() {
        let limits = op_limits_with(None, &["api.openai.com", "*.example.com"], &[]);
        let env = auto_env(
            None,
            Some(net_policy(
                &["api.openai.com", "foo.example.com"],
                Vec::new(),
            )),
        );
        let r = resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap();
        assert_eq!(r.egress_policy.allow_hosts.len(), 2);
    }

    #[test]
    fn resolver_egress_apex_does_not_match_wildcard() {
        let limits = op_limits_with(None, &["*.example.com"], &[]);
        let env = auto_env(None, Some(net_policy(&["example.com"], Vec::new())));
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::HostNotAllowed(h) if h == "example.com"));
    }

    #[test]
    fn resolver_egress_host_outside_allowlist_rejected() {
        let limits = op_limits_with(None, &["api.openai.com"], &[]);
        let env = auto_env(None, Some(net_policy(&["evil.example.com"], Vec::new())));
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::HostNotAllowed(h) if h == "evil.example.com"));
    }

    #[test]
    fn resolver_wildcard_star_allows_everything() {
        let limits = op_limits_with(None, &["*"], &[]);
        let env = auto_env(None, Some(net_policy(&["anything.example"], Vec::new())));
        assert!(resolve_shell_environment(Some(&env), &limits, &default_containers()).is_ok());
    }

    #[test]
    fn resolver_unknown_secret_rejected() {
        let limits = op_limits_with(None, &[], &[]);
        let env = auto_env(
            None,
            Some(net_policy(
                &[],
                vec![ShellDomainSecret::Reference(ShellDomainSecretRef {
                    placeholder: "GITHUB_TOKEN".into(),
                    allowed_domains: vec![],
                })],
            )),
        );
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
        assert!(matches!(err, ShellEnvironmentError::UnknownSecret(p) if p == "GITHUB_TOKEN"));
    }

    #[test]
    fn resolver_secret_subset_accepted_inherits_full_allowlist_when_empty() {
        let limits = op_limits_with(
            None,
            &[],
            &[("GH", "ghp_xxx", &["api.github.com", "uploads.github.com"])],
        );
        let env = auto_env(
            None,
            Some(net_policy(
                &[],
                vec![ShellDomainSecret::Reference(ShellDomainSecretRef {
                    placeholder: "GH".into(),
                    allowed_domains: vec![],
                })],
            )),
        );
        let r = resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap();
        assert_eq!(r.egress_policy.secrets.len(), 1);
        assert_eq!(r.egress_policy.secrets[0].value, "ghp_xxx");
        assert_eq!(r.egress_policy.secrets[0].allowed_hosts.len(), 2);
    }

    #[test]
    fn resolver_secret_host_outside_allowed_rejected() {
        let limits = op_limits_with(None, &[], &[("GH", "v", &["api.github.com"])]);
        let env = auto_env(
            None,
            Some(net_policy(
                &[],
                vec![ShellDomainSecret::Reference(ShellDomainSecretRef {
                    placeholder: "GH".into(),
                    allowed_domains: vec!["evil.example.com".into()],
                })],
            )),
        );
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
        assert!(matches!(
            err,
            ShellEnvironmentError::SecretHostNotAllowed { placeholder, host }
                if placeholder == "GH" && host == "evil.example.com"
        ));
    }

    #[test]
    fn resolver_inline_secret_uses_host_envelope_and_propagates_value() {
        let limits = op_limits_with(None, &["api.github.com"], &[]);
        let env = auto_env(
            None,
            Some(net_policy(
                &["api.github.com"],
                vec![ShellDomainSecret::Inline(ShellDomainSecretInline {
                    domain: "api.github.com".into(),
                    name: "GH_TOKEN".into(),
                    value: "ghp_inline".into(),
                })],
            )),
        );
        let r = resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap();
        assert_eq!(r.egress_policy.secrets.len(), 1);
        assert_eq!(r.egress_policy.secrets[0].placeholder, "GH_TOKEN");
        assert_eq!(r.egress_policy.secrets[0].value, "ghp_inline");
        assert_eq!(
            r.egress_policy.secrets[0].allowed_hosts,
            vec!["api.github.com".to_string()]
        );
    }

    #[test]
    fn resolver_inline_secret_host_outside_operator_envelope_rejected() {
        let limits = op_limits_with(None, &["api.github.com"], &[]);
        let env = auto_env(
            None,
            Some(ShellNetworkPolicy {
                type_: ShellNetworkPolicyType::Known(
                    crate::api_types::responses::KnownShellNetworkPolicyType::Allowlist,
                ),
                allowed_domains: vec![],
                domain_secrets: vec![ShellDomainSecret::Inline(ShellDomainSecretInline {
                    domain: "evil.example.com".into(),
                    name: "X".into(),
                    value: "y".into(),
                })],
            }),
        );
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
        assert!(matches!(
            err,
            ShellEnvironmentError::InlineSecretHostNotAllowed { name, host }
                if name == "X" && host == "evil.example.com"
        ));
    }

    #[test]
    fn resolver_container_reference_returns_id() {
        let limits = op_limits_with(None, &[], &[]);
        let env = ShellEnvironment::ContainerReference(ShellContainerReference {
            container_id: "cntr_abc".into(),
            network_policy: None,
        });
        let r = resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap();
        assert_eq!(r.referenced_container_id.as_deref(), Some("cntr_abc"));
    }

    #[test]
    fn resolver_container_reference_rejects_empty_id() {
        let limits = op_limits_with(None, &[], &[]);
        let env = ShellEnvironment::ContainerReference(ShellContainerReference {
            container_id: "   ".into(),
            network_policy: None,
        });
        let err =
            resolve_shell_environment(Some(&env), &limits, &default_containers()).unwrap_err();
        assert!(matches!(
            err,
            ShellEnvironmentError::EmptyContainerReferenceId
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
    fn host_match_normalizes_trailing_dot() {
        // FQDN form (RFC 1035) with explicit root-domain dot.
        assert!(host_matches("api.openai.com.", "api.openai.com"));
        assert!(host_matches("api.openai.com", "api.openai.com."));
        assert!(host_matches("a.example.com.", "*.example.com"));
        assert!(host_matches("a.example.com", "*.example.com."));
    }

    #[test]
    fn parses_single_command_action() {
        let v = serde_json::json!({
            "type": "function_call",
            "name": "shell",
            "call_id": "call_abc",
            "arguments": "{\"action\": {\"commands\": [\"echo hi\"]}}"
        });
        let tc = parse_shell_tool_call(&v).unwrap();
        assert_eq!(tc.id, "call_abc");
        assert_eq!(tc.args.commands, vec!["echo hi".to_string()]);
        assert!(tc.args.stdin.is_none());
        assert!(tc.args.timeout_ms.is_none());
    }

    #[test]
    fn parses_multi_command_action() {
        let v = serde_json::json!({
            "type": "function_call",
            "name": "shell",
            "call_id": "call_xyz",
            "arguments": "{\"action\": {\"commands\": [\"cd /tmp\", \"ls /\"], \"timeout_ms\": 1500, \"max_output_length\": 2000, \"env\": {\"FOO\": \"bar\"}, \"working_directory\": \"/tmp\"}}"
        });
        let tc = parse_shell_tool_call(&v).unwrap();
        assert_eq!(
            tc.args.commands,
            vec!["cd /tmp".to_string(), "ls /".to_string()]
        );
        assert_eq!(tc.args.timeout_ms, Some(1500));
        assert_eq!(tc.args.max_output_length, Some(2000));
        assert_eq!(
            tc.args.env.as_ref().unwrap().get("FOO").map(|s| s.as_str()),
            Some("bar")
        );
        assert_eq!(tc.args.working_directory.as_deref(), Some("/tmp"));
        // Script form joins with newlines.
        assert_eq!(tc.args.joined_script(), "cd /tmp\nls /");
    }

    #[test]
    fn empty_commands_resolves_none() {
        let args =
            ShellToolArguments::parse("{\"action\": {\"commands\": [\"   \", \"\"]}}").unwrap();
        assert!(args.resolve().is_none());
    }

    #[test]
    fn build_effective_command_prepends_cd_and_exports() {
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        env.insert("BAZ".to_string(), "with 'quote'".to_string());
        let out = build_effective_command("ls /", Some(&env), Some("/tmp")).unwrap();
        // cd preamble first, then exports in sorted order, then command.
        assert_eq!(
            out,
            "cd '/tmp' || exit 1\nexport BAZ='with '\\''quote'\\'''\nexport FOO='bar'\nls /"
        );
    }

    #[test]
    fn build_effective_command_noop_without_env_or_cwd() {
        assert_eq!(build_effective_command("ls /", None, None).unwrap(), "ls /");
        // Blank working_directory is ignored.
        assert_eq!(
            build_effective_command("ls /", None, Some("   ")).unwrap(),
            "ls /"
        );
    }

    #[test]
    fn build_effective_command_rejects_bad_env_name() {
        let mut env = HashMap::new();
        env.insert("BAD NAME".to_string(), "x".to_string());
        assert!(matches!(
            build_effective_command("ls", Some(&env), None),
            Err(ToolError::InvalidCall(_))
        ));
        let mut env2 = HashMap::new();
        env2.insert("1FOO".to_string(), "x".to_string());
        assert!(build_effective_command("ls", Some(&env2), None).is_err());
    }

    #[test]
    fn env_name_validation() {
        assert!(is_valid_env_name("FOO"));
        assert!(is_valid_env_name("_foo_BAR9"));
        assert!(!is_valid_env_name(""));
        assert!(!is_valid_env_name("9foo"));
        assert!(!is_valid_env_name("FO O"));
        assert!(!is_valid_env_name("FOO=BAR"));
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
        preprocess_shell_tools(&mut payload, &ShellToolHint::default());
        let tools = payload.tools.unwrap();
        assert_eq!(tools.len(), 1);
        let ResponsesToolDefinition::Function(func) = &tools[0] else {
            panic!("expected function tool");
        };
        // Default hint advertises Hadrian-hosted sandbox and the workdir.
        let desc = func.description.as_deref().unwrap();
        assert!(
            desc.contains("/mnt/data"),
            "description should mention workdir: {desc}"
        );
        assert!(
            desc.contains("truncated"),
            "description should warn about truncation: {desc}"
        );
    }

    #[test]
    fn preprocess_rewrites_shell_history_to_function_calls() {
        // A continuation reconstructed from `previous_response_id` replays
        // the prior turn's hosted-shell items (`shell_call` /
        // `shell_call_output`, the latter with an *array* `output`). In
        // function mode these must become `function_call` /
        // `function_call_output` so upstreams that demand a string `output`
        // don't reject the turn.
        let payload_json = serde_json::json!({
            "tools": [{"type": "shell"}],
            "stream": false,
            "input": [
                {"role": "user", "content": "Run `echo hi`"},
                {"type": "shell_call", "id": "call_abc", "call_id": "call_abc",
                 "status": "completed",
                 "action": {"commands": ["echo hi"]}},
                {"type": "shell_call_output", "id": "call_abc", "call_id": "call_abc",
                 "status": "completed",
                 "output": [{"stdout": "hi\n", "stderr": "",
                             "outcome": {"type": "exit", "exit_code": 0}}]},
            ],
        });
        let mut payload: CreateResponsesPayload = serde_json::from_value(payload_json).unwrap();
        preprocess_shell_tools(&mut payload, &ShellToolHint::default());

        let Some(ResponsesInput::Items(items)) = payload.input.as_ref() else {
            panic!("expected items input");
        };
        assert_eq!(items.len(), 3);
        // The user message is preserved as-is. A bare-string `content`
        // with no `type` deserializes to the `EasyMessage` variant.
        assert!(
            matches!(items[0], ResponsesInputItem::EasyMessage(_)),
            "expected user message at index 0, got {:?}",
            items[0]
        );
        // shell_call → function_call with reconstructed `shell` arguments,
        // pairing preserved via call_id.
        let ResponsesInputItem::OutputFunctionCall(call) = &items[1] else {
            panic!(
                "expected shell_call rewritten to function_call: {:?}",
                items[1]
            );
        };
        assert_eq!(call.name, "shell");
        assert_eq!(call.call_id, "call_abc");
        assert!(call.id.is_none(), "must omit the non-fc_ shell id");
        assert!(
            call.arguments.contains("echo hi"),
            "arguments should carry the action: {}",
            call.arguments
        );
        // shell_call_output → function_call_output with a *string* output.
        let ResponsesInputItem::FunctionCallOutput(out) = &items[2] else {
            panic!(
                "expected shell_call_output rewritten to function_call_output: {:?}",
                items[2]
            );
        };
        assert_eq!(out.call_id, "call_abc");
        assert!(
            out.output.contains("exit_code: 0") && out.output.contains("hi"),
            "output should flatten to text: {}",
            out.output
        );
        // No stray hosted-shell items survive to trip the upstream validator.
        assert!(!items.iter().any(|i| matches!(
            i,
            ResponsesInputItem::ShellCall(_) | ResponsesInputItem::ShellCallOutput(_)
        )));
    }

    #[test]
    fn shell_hint_describes_client_passthrough() {
        let hint = ShellToolHint {
            location: ShellExecutionLocation::ApiClient,
            ..ShellToolHint::default()
        };
        let desc = hint.render_description();
        assert!(
            desc.contains("API client"),
            "should call out client execution: {desc}"
        );
        // Workdir guidance is for Hadrian-hosted sandboxes; client-mode
        // models shouldn't be told to write to /mnt/data.
        assert!(
            !desc.contains("written under `/mnt/data`"),
            "client mode should not promise /mnt/data: {desc}"
        );
    }

    #[test]
    fn shell_hint_describes_allowlist() {
        let hint = ShellToolHint {
            network_summary: ShellNetworkSummary::Allowlist(vec![
                "api.example.com".into(),
                "cdn.example.org".into(),
            ]),
            ..ShellToolHint::default()
        };
        let desc = hint.render_description();
        assert!(
            desc.contains("api.example.com"),
            "should list allowed host: {desc}"
        );
        assert!(desc.contains("cdn.example.org"));
    }

    #[test]
    fn utf8_chunk_decoder_buffers_partial_sequences() {
        let mut dec = Utf8ChunkDecoder::new();
        // "é" is 0xC3 0xA9 in UTF-8. Feed the lead byte alone — should
        // emit nothing yet and hold the byte for the next push.
        assert_eq!(dec.push(&[0xC3]), "");
        // Now feed the continuation byte — should emit the full char.
        assert_eq!(dec.push(&[0xA9]), "é");
        // Mixed: ASCII then a partial sequence at the tail.
        assert_eq!(dec.push(b"abc\xC3"), "abc");
        assert_eq!(dec.push(&[0xA9]), "é");
    }

    #[test]
    fn utf8_chunk_decoder_handles_three_and_four_byte_sequences() {
        let mut dec = Utf8ChunkDecoder::new();
        // Snowman: 0xE2 0x98 0x83 (3 bytes).
        assert_eq!(dec.push(&[0xE2, 0x98]), "");
        assert_eq!(dec.push(&[0x83]), "☃");
        // 4-byte: 𝄞 = 0xF0 0x9D 0x84 0x9E.
        assert_eq!(dec.push(&[0xF0, 0x9D, 0x84]), "");
        assert_eq!(dec.push(&[0x9E]), "𝄞");
    }

    #[test]
    fn bounded_head_tail_under_cap_passes_through() {
        let mut b = BoundedHeadTail::new(20);
        b.push_str("hello world");
        assert_eq!(b.into_trimmed(), "hello world");
    }

    #[test]
    fn bounded_head_tail_emits_truncation_marker() {
        let mut b = BoundedHeadTail::new(10);
        // 5 head + 5 tail = 10 kept; the rest is the truncated middle.
        b.push_str("AAAAA");
        b.push_str("BBBBBBBBBB");
        b.push_str("CCCCC");
        let out = b.into_trimmed();
        assert!(out.contains("chars truncated"), "got: {out}");
        assert!(out.starts_with("AAAAA"));
        assert!(out.ends_with("CCCCC"));
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
    fn shell_call_done_carries_container_reference_environment() {
        let env = serde_json::json!({
            "type": "container_reference",
            "container_id": "cntr_abc123",
        });
        let bytes = format_shell_call_item(
            ItemLifecycle::Done,
            "call_1",
            "call_1",
            0,
            &["echo hi".to_string()],
            None,
            None,
            None,
            None,
            "completed",
            Some(&env),
            Some("model"),
        );
        let s = std::str::from_utf8(&bytes).unwrap();
        let json_str = s.trim().strip_prefix("data: ").unwrap();
        let v: Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(v["type"], "response.output_item.done");
        let item = &v["item"];
        assert_eq!(item["type"], "shell_call");
        assert_eq!(item["status"], "completed");
        assert_eq!(item["environment"]["type"], "container_reference");
        assert_eq!(item["environment"]["container_id"], "cntr_abc123");
    }

    #[test]
    fn shell_call_added_omits_environment_when_unknown() {
        let bytes = format_shell_call_item(
            ItemLifecycle::Added,
            "call_1",
            "call_1",
            0,
            &["echo hi".to_string()],
            None,
            None,
            None,
            None,
            "in_progress",
            None,
            Some("model"),
        );
        let s = std::str::from_utf8(&bytes).unwrap();
        let json_str = s.trim().strip_prefix("data: ").unwrap();
        let v: Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(v["type"], "response.output_item.added");
        let item = &v["item"];
        assert_eq!(item["status"], "in_progress");
        assert!(item.get("environment").is_none());
    }

    #[test]
    fn shell_call_output_done_includes_captured_files() {
        let f = sample_file("cfile_abc", "out.csv", "/mnt/data/out.csv");
        let bytes = format_shell_call_output_item(
            ItemLifecycle::Done,
            "call_1",
            "call_1",
            0,
            0,
            "",
            "",
            std::slice::from_ref(&f),
            false,
            None,
            Some("gateway"),
        );
        let s = std::str::from_utf8(&bytes).unwrap();
        let json_str = s.trim().strip_prefix("data: ").unwrap();
        let v: Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(v["type"], "response.output_item.done");
        let item = &v["item"];
        assert_eq!(item["type"], "shell_call_output");
        assert_eq!(item["status"], "completed");
        let files = item["output_files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["file_id"], "cfile_abc");
        assert_eq!(files[0]["container_id"], "cntr_test");
    }
}
