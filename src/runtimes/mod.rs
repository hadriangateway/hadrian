//! Pluggable shell-tool runtimes for the Responses API.
//!
//! A "runtime" is the backend that executes the model's `shell` tool
//! calls — anything from forwarding the request verbatim to OpenAI's
//! hosted container, to spawning a local microVM via microsandbox, to
//! delegating to a managed sandbox service like E2B.
//!
//! Each runtime implements the [`ShellRuntime`] trait. The orchestrator
//! in `services::server_tools` picks one based on admin config + the
//! request's upstream provider, then drives sessions through the
//! [`ShellSession`] interface.
//!
//! # Backends
//!
//! - **`passthrough_openai`** — forward the shell tool spec to OpenAI
//!   unchanged so OpenAI's hosted container executes the call.
//! - **`client_passthrough`** — the API client fulfills shell calls
//!   itself (OpenAI's "local shell" mode, generalized to all providers).
//!   Hadrian validates the request, keeps OpenAI's native `shell` spec
//!   intact, rewrites it to a function tool for non-OpenAI providers,
//!   and does not register a server-side executor.
//! - **`microsandbox`** — local microVMs via
//!   <https://github.com/superradcompany/microsandbox>. Behind the
//!   `runtime-microsandbox` feature flag.
//! - **`opensandbox`** — Alibaba's [Sandbox
//!   Protocol](https://github.com/alibaba/OpenSandbox) over HTTP. Behind
//!   the `runtime-opensandbox` feature flag.

#![cfg(not(target_arch = "wasm32"))]

use std::{pin::Pin, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::Stream;
use thiserror::Error;

#[cfg(feature = "runtime-microsandbox")]
pub mod microsandbox;
#[cfg(feature = "runtime-opensandbox")]
pub mod opensandbox;
pub mod passthrough;

#[cfg(feature = "runtime-microsandbox")]
pub use microsandbox::MicrosandboxRuntime;
#[cfg(feature = "runtime-opensandbox")]
pub use opensandbox::OpenSandboxRuntime;
pub use passthrough::PassthroughRuntime;

/// Errors returned by runtime adapters.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The requested capability is not supported by this backend
    /// (e.g. per-domain secret injection on a backend that only does
    /// allow-all/deny-all egress).
    #[error("runtime does not support: {0}")]
    Unsupported(&'static str),

    /// The backend was reachable but returned an error.
    #[error("runtime error: {0}")]
    Backend(String),

    /// Could not reach the backend (network, auth, etc.).
    #[error("runtime unreachable: {0}")]
    Unreachable(String),

    /// Session timed out (max_session_duration exceeded).
    #[error("session exceeded max duration")]
    SessionTimeout,

    /// The orchestrator should not invoke local execution — the request
    /// is being forwarded to the upstream provider. Returned by the
    /// passthrough adapter from `start_session`.
    #[error("runtime is in passthrough mode; defer to upstream provider")]
    Passthrough,
}

pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// Network isolation policy advertised by a runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkMode {
    /// Container has no outbound network access.
    None,
    /// Container can reach hosts on the allowlist only.
    AllowList,
    /// Container has unrestricted outbound network access.
    Full,
}

/// What a runtime can and cannot do.
///
/// Inspected before starting a session; if the request demands a
/// capability the runtime can't deliver (e.g. per-domain secret
/// injection on a backend that doesn't support it), the orchestrator
/// fails the request with a clear capability-mismatch error rather than
/// silently degrading.
#[derive(Debug, Clone)]
pub struct RuntimeCapabilities {
    /// True if the runtime defers shell execution rather than running it
    /// in a Hadrian-hosted environment. When set, the orchestrator does
    /// not register a [`ShellExecutor`](crate::services::shell_tool::ShellExecutor)
    /// for the request. Combine with [`client_executes`] to distinguish
    /// where execution actually happens (`false` = upstream provider's
    /// hosted container; `true` = the API client).
    pub passthrough_only: bool,
    /// True iff the shell call is fulfilled by the API client itself
    /// (rather than the upstream provider). Drives the preprocessing
    /// decision in `routes/execution.rs`: when this is set, the OpenAI
    /// native `shell` tool spec is left intact so the model emits
    /// `shell_call` items the OpenAI SDK's local-shell loop can
    /// recognize, while non-OpenAI providers still receive the
    /// function-mode rewrite so they emit `function_call` items the
    /// client can interpret. Meaningless without `passthrough_only`.
    pub client_executes: bool,
    /// Can the runtime substitute placeholder strings with secret
    /// values at egress (so the model never sees raw secret values)?
    pub secret_injection: bool,
    /// Can the runtime enforce a per-domain egress allowlist?
    pub egress_allowlist: bool,
    /// Can the runtime mount skill bundles into the session?
    pub skill_mount: bool,
    /// Can the runtime stage files in/out of the session?
    pub file_io: bool,
    /// Network isolation modes the runtime supports.
    pub network_isolation_modes: Vec<NetworkMode>,
    /// Hard limit on session duration, if any.
    pub max_session_duration: Option<Duration>,
}

impl RuntimeCapabilities {
    /// Capabilities of a passthrough runtime where the upstream provider's
    /// hosted environment executes the call (e.g. OpenAI's Responses API
    /// container).
    pub fn passthrough_upstream() -> Self {
        Self {
            passthrough_only: true,
            client_executes: false,
            secret_injection: false,
            egress_allowlist: false,
            skill_mount: false,
            file_io: false,
            network_isolation_modes: Vec::new(),
            max_session_duration: None,
        }
    }

    /// Capabilities of a passthrough runtime where the API client fulfills
    /// shell calls itself (OpenAI's local-shell mode, generalized to all
    /// providers). The orchestrator still skips registering an executor;
    /// the difference from [`passthrough_upstream`](Self::passthrough_upstream)
    /// is purely in the preprocessing decision documented on
    /// [`client_executes`](RuntimeCapabilities::client_executes).
    pub fn passthrough_client() -> Self {
        Self {
            passthrough_only: true,
            client_executes: true,
            ..Self::passthrough_upstream()
        }
    }
}

/// Egress policy applied to a session.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    /// Allowed hostnames (or hostname patterns). Empty = deny-all.
    pub allow_hosts: Vec<String>,
    /// Map of placeholder name → secret value. The runtime substitutes
    /// `${PLACEHOLDER}` in outbound request headers/bodies with the
    /// real value when the destination matches `allow_hosts`. The model
    /// only ever sees the placeholders.
    pub secrets: Vec<SecretMount>,
}

#[derive(Debug, Clone)]
pub struct SecretMount {
    /// The placeholder the model will see, e.g. `GITHUB_TOKEN`.
    pub placeholder: String,
    /// The actual secret value. Never logged. Cleared on session
    /// termination.
    pub value: String,
    /// Hostnames this secret may be sent to (if empty, applies
    /// everywhere in `allow_hosts`).
    pub allowed_hosts: Vec<String>,
}

/// Skill bundle to mount into a session.
///
/// The gateway resolves `skill_id` to file contents from `SkillService`
/// and passes the materialized files in `files`. Adapters write them
/// to `mount_path` inside the sandbox via whatever native filesystem
/// API the runtime exposes (microsandbox: `sandbox.fs().write()`;
/// OpenSandbox: execd `/files/upload`).
#[derive(Debug, Clone)]
pub struct SkillMount {
    pub skill_id: String,
    /// Mount path inside the container (e.g. `/skills/<name>-<version>`).
    pub mount_path: String,
    /// Files to write under `mount_path`. Paths are relative to the
    /// mount point; the adapter is responsible for creating
    /// intermediate directories.
    pub files: Vec<MountedFile>,
}

/// One file to write into a sandbox during skill mount.
#[derive(Debug, Clone)]
pub struct MountedFile {
    /// Path relative to `SkillMount::mount_path`.
    pub relative_path: String,
    /// File contents.
    pub content: bytes::Bytes,
}

/// Parameters for starting a new session.
#[derive(Debug, Clone, Default)]
pub struct SessionSpec {
    pub network_mode: Option<NetworkMode>,
    pub egress_policy: EgressPolicy,
    pub mounted_skills: Vec<SkillMount>,
    pub cpu_limit: Option<f64>,
    pub mem_limit_bytes: Option<u64>,
    pub session_id_hint: Option<String>,
}

/// A request to execute one shell command in an existing session.
#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub command: String,
    /// Optional stdin to pipe to the command.
    pub stdin: Option<Bytes>,
    /// Wall-clock timeout for this command.
    pub timeout: Option<Duration>,
}

/// One event from a running command.
#[derive(Debug, Clone)]
pub enum ExecEvent {
    /// Stdout chunk.
    Stdout(Bytes),
    /// Stderr chunk.
    Stderr(Bytes),
    /// Final exit signal. Always the last event emitted for a command.
    Exit { code: i32, signal: Option<i32> },
}

/// Handle to a running command — streams output until exit.
pub struct ExecHandle {
    pub output: Pin<Box<dyn Stream<Item = ExecEvent> + Send>>,
}

/// Top-level trait every runtime adapter implements.
///
/// Mirrors the `SecretManager` trait shape (`async_trait` + per-backend
/// implementors gated behind feature flags).
#[async_trait]
pub trait ShellRuntime: Send + Sync {
    /// What this runtime can do. Inspected once at startup and per
    /// session-start to fail fast on capability mismatches.
    fn capabilities(&self) -> RuntimeCapabilities;

    /// Start a new session.
    ///
    /// Passthrough adapters return `Err(RuntimeError::Passthrough)` so
    /// the orchestrator knows to defer to the upstream provider.
    async fn start_session(&self, spec: SessionSpec) -> RuntimeResult<SessionHandle>;

    /// Cheap readiness check (e.g. ping the backend, verify auth).
    async fn health_check(&self) -> RuntimeResult<()> {
        Ok(())
    }
}

/// Owned handle to a live session.
///
/// Owns the underlying `ShellSession` implementation and tears it down
/// in `Drop` (best-effort; explicit `terminate()` is preferred).
pub struct SessionHandle {
    pub session_id: String,
    inner: Box<dyn ShellSession>,
}

impl SessionHandle {
    pub fn new(session_id: String, inner: Box<dyn ShellSession>) -> Self {
        Self { session_id, inner }
    }

    pub async fn exec(&self, cmd: ExecRequest) -> RuntimeResult<ExecHandle> {
        self.inner.exec(cmd).await
    }

    pub async fn write_file(&self, path: &str, bytes: Bytes) -> RuntimeResult<()> {
        self.inner.write_file(path, bytes).await
    }

    pub async fn read_file(&self, path: &str) -> RuntimeResult<Bytes> {
        self.inner.read_file(path).await
    }

    /// Tear down the session. Always prefer calling this explicitly so
    /// errors are surfaced; `Drop` is best-effort.
    pub async fn terminate(self) -> RuntimeResult<()> {
        self.inner.terminate().await
    }
}

/// Per-session API exposed by the runtime adapter.
#[async_trait]
pub trait ShellSession: Send + Sync {
    /// Run one command. Returns a handle that streams output until
    /// exit.
    async fn exec(&self, cmd: ExecRequest) -> RuntimeResult<ExecHandle>;

    /// Stage a file into the session's filesystem.
    async fn write_file(&self, _path: &str, _bytes: Bytes) -> RuntimeResult<()> {
        Err(RuntimeError::Unsupported("file_io"))
    }

    /// Read a file out of the session's filesystem.
    async fn read_file(&self, _path: &str) -> RuntimeResult<Bytes> {
        Err(RuntimeError::Unsupported("file_io"))
    }

    /// Tear down the session. Called exactly once.
    async fn terminate(&self) -> RuntimeResult<()>;
}
