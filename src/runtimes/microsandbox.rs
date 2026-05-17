//! Microsandbox runtime adapter.
//!
//! Wraps the [`microsandbox`] SDK so each Hadrian session corresponds to
//! one local microVM. Microsandbox runs in-process (no daemon, no
//! endpoint) — booting a VM, streaming command I/O, and tearing it down
//! all happen via direct Rust calls.

#![cfg(feature = "runtime-microsandbox")]

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::Stream;
use microsandbox::{ExecEvent as MsExecEvent, NetworkPolicy, Sandbox};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    config::MicrosandboxConfig,
    runtimes::{
        ExecEvent, ExecHandle, ExecRequest, NetworkMode, RuntimeCapabilities, RuntimeError,
        RuntimeResult, SessionHandle, SessionSpec, ShellRuntime, ShellSession,
    },
};

/// `ShellRuntime` implementation backed by microsandbox microVMs.
///
/// Each `start_session` boots a fresh VM — the previous pre-warm pool
/// was removed because pooled VMs were reused across tenants without a
/// filesystem reset, opening a cross-tenant data leak window for any
/// session whose `SessionSpec` didn't force a fresh VM. Cold-start
/// cost is paid per request; revisit pooling later only if we add
/// per-tenant keying or snapshot-restore.
pub struct MicrosandboxRuntime {
    config: MicrosandboxConfig,
}

impl MicrosandboxRuntime {
    pub fn new(config: MicrosandboxConfig) -> Self {
        Self { config }
    }
}

fn cpus_for_sdk(cpus: u32) -> u8 {
    // SDK takes u8 (0..=255). Anything beyond 255 vCPUs is operator
    // error; clamp rather than silently wrapping.
    cpus.min(u8::MAX as u32) as u8
}

#[async_trait]
impl ShellRuntime for MicrosandboxRuntime {
    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            passthrough_only: false,
            client_executes: false,
            // Slice 1D enables secret injection via microsandbox's
            // SecretBuilder (placeholder substitution at the TLS proxy).
            secret_injection: true,
            // Hostname-based egress allowlist without an accompanying
            // secret isn't natively supported by microsandbox's
            // NetworkPolicy (which is IP-based). Adding it would
            // require DNS-resolution + per-IP rules; punt for now and
            // recommend operators scope egress via secret mounts.
            egress_allowlist: false,
            skill_mount: true,
            file_io: true,
            network_isolation_modes: vec![NetworkMode::Full],
            max_session_duration: None,
        }
    }

    async fn start_session(&self, spec: SessionSpec) -> RuntimeResult<SessionHandle> {
        // Validate capability requirements before spinning up a VM.
        if !spec.egress_policy.allow_hosts.is_empty() {
            return Err(RuntimeError::Unsupported("egress_allowlist"));
        }

        let session_id = spec
            .session_id_hint
            .unwrap_or_else(|| format!("hadrian-{}", Uuid::new_v4()));

        let cpus: u8 = spec
            .cpu_limit
            .map(|c| c.ceil().clamp(1.0, u8::MAX as f64) as u8)
            .unwrap_or_else(|| cpus_for_sdk(self.config.cpus));
        let memory_mb: u32 = spec
            .mem_limit_bytes
            .map(|b| (b / (1024 * 1024)) as u32)
            .unwrap_or(self.config.memory_mb);

        info!(
            stage = "microsandbox_starting",
            session_id = %session_id,
            image = %self.config.image,
            cpus,
            memory_mb,
            "Creating microsandbox VM"
        );

        // Pin an explicit `public_only` network policy: egress allowed
        // to public internet ranges, denied for private/loopback/
        // link-local/metadata (the IMDS endpoint at 169.254.169.254
        // and friends). This makes the network posture independent of
        // whatever `NetworkConfig::default()` happens to be in the SDK
        // version we're built against — a future SDK that flipped the
        // default to allow-all would silently regress us otherwise.
        // Per-secret allow-host rules still apply on top via
        // `secret().allow_host()` below.
        let mut builder = Sandbox::builder(session_id.clone())
            .image(self.config.image.clone())
            .cpus(cpus)
            .memory(memory_mb)
            .replace()
            .network(|n| n.policy(NetworkPolicy::public_only()));

        // Wire each requested SecretMount into microsandbox's
        // SecretBuilder. Each mount gives the guest a placeholder env
        // var (e.g. `$MSB_GITHUB_TOKEN`); the TLS-intercepting proxy
        // substitutes the real value when outbound requests are
        // destined for one of the allowed hosts. The model never sees
        // the raw secret value.
        for mount in &spec.egress_policy.secrets {
            if mount.allowed_hosts.is_empty() {
                return Err(RuntimeError::Backend(format!(
                    "secret mount {:?} has no allowed_hosts",
                    mount.placeholder
                )));
            }
            let placeholder = mount.placeholder.clone();
            let value = mount.value.clone();
            let hosts = mount.allowed_hosts.clone();
            builder = builder.secret(move |s| {
                let mut sb = s.env(&placeholder).value(&value);
                for h in &hosts {
                    sb = if h.contains('*') {
                        sb.allow_host_pattern(h)
                    } else {
                        sb.allow_host(h)
                    };
                }
                sb
            });
        }

        let sandbox = builder
            .create()
            .await
            .map_err(|e| RuntimeError::Backend(format!("microsandbox create: {e}")))?;

        // Mount any requested skill bundles via the VM's filesystem
        // API. Each skill's files are written under its `mount_path`;
        // intermediate directories are created on demand.
        for skill in &spec.mounted_skills {
            mount_skill_into_sandbox(&sandbox, skill).await?;
            debug!(
                stage = "microsandbox_skill_mounted",
                skill_id = %skill.skill_id,
                mount_path = %skill.mount_path,
                file_count = skill.files.len(),
                "Mounted skill bundle"
            );
        }

        Ok(SessionHandle::new(
            session_id,
            Box::new(MicrosandboxSession {
                sandbox: Arc::new(Mutex::new(Some(sandbox))),
            }),
        ))
    }
}

/// Write a skill's files into a freshly-booted sandbox. Creates the
/// mount directory and any subdirectories implied by file paths.
async fn mount_skill_into_sandbox(
    sandbox: &Sandbox,
    skill: &crate::runtimes::SkillMount,
) -> RuntimeResult<()> {
    let fs = sandbox.fs();
    // Root directory first.
    fs.mkdir(&skill.mount_path)
        .await
        .map_err(|e| RuntimeError::Backend(format!("mkdir {}: {e}", skill.mount_path)))?;
    for file in &skill.files {
        // Reject path traversal: treat the SkillService output as
        // untrusted on this code path. `..` or absolute prefixes
        // would let one skill clobber arbitrary paths inside the VM.
        let safe_rel = sanitize_skill_relative_path(&file.relative_path)?;
        let full_path = join_paths(&skill.mount_path, &safe_rel);
        // Ensure parent directory exists.
        if let Some(parent) = parent_of(&full_path)
            && parent != skill.mount_path
        {
            // microsandbox returns an AlreadyExists-style error if
            // the directory is already there; log other failures so
            // we don't silently swallow real problems on `write`
            // later. Per CLAUDE.md memory: prefer correctness + debug
            // logs over silent error swallowing.
            if let Err(e) = fs.mkdir(&parent).await {
                let msg = e.to_string();
                let already_exists = msg.contains("AlreadyExists")
                    || msg.contains("already exists")
                    || msg.contains("EEXIST");
                if !already_exists {
                    debug!(
                        path = %parent,
                        error = %msg,
                        "Non-AlreadyExists mkdir error inside sandbox (continuing)"
                    );
                }
            }
        }
        fs.write(&full_path, file.content.as_ref())
            .await
            .map_err(|e| RuntimeError::Backend(format!("write {full_path}: {e}")))?;
    }
    Ok(())
}

/// Reject path traversal in skill-file relative paths. Returns the
/// cleaned path (leading slashes stripped) or a `RuntimeError::Backend`
/// on any `..` segment, absolute prefix, or non-normal component.
fn sanitize_skill_relative_path(rel: &str) -> RuntimeResult<String> {
    use std::path::{Component, Path};
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(RuntimeError::Backend(format!(
            "skill relative_path must not be absolute: {rel}"
        )));
    }
    let mut cleaned = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => cleaned.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RuntimeError::Backend(format!(
                    "skill relative_path must not contain traversal: {rel}"
                )));
            }
        }
    }
    cleaned.to_str().map(str::to_string).ok_or_else(|| {
        RuntimeError::Backend(format!("skill relative_path is not valid UTF-8: {rel}"))
    })
}

fn join_paths(base: &str, rel: &str) -> String {
    let base = base.trim_end_matches('/');
    let rel = rel.trim_start_matches('/');
    format!("{base}/{rel}")
}

fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    if idx == 0 {
        return Some("/".to_string());
    }
    Some(trimmed[..idx].to_string())
}

/// One live microsandbox session.
///
/// The inner `Option<Sandbox>` lets `terminate()` consume the Sandbox
/// without requiring `&mut self` (which the trait doesn't provide).
struct MicrosandboxSession {
    sandbox: Arc<Mutex<Option<Sandbox>>>,
}

#[async_trait]
impl ShellSession for MicrosandboxSession {
    async fn exec(&self, cmd: ExecRequest) -> RuntimeResult<ExecHandle> {
        let guard = self.sandbox.lock().await;
        let sandbox = guard
            .as_ref()
            .ok_or_else(|| RuntimeError::Backend("session already terminated".into()))?;

        // stdin: SDK supports a Pipe mode for streaming; for a one-shot
        // bytes payload from the model we'd ideally use `stdin_bytes`
        // before exec, but `shell_stream` is the simplest path and
        // doesn't accept stdin. Fall back to redirecting via heredoc in
        // the command when stdin is provided.
        //
        // Per-call random terminator: a fixed marker is forge-able
        // (an adversarial model could embed the literal string in
        // stdin to escape into shell). The UUID makes collision
        // probability ~2^-122. Single-quoting prevents variable
        // expansion inside the body.
        let script = match cmd.stdin {
            Some(bytes) => {
                let stdin_text = String::from_utf8_lossy(&bytes);
                let terminator = format!("__HADRIAN_STDIN_{}__", uuid::Uuid::new_v4().simple());
                format!(
                    "{} <<'{terminator}'\n{stdin_text}\n{terminator}",
                    cmd.command
                )
            }
            None => cmd.command.clone(),
        };

        let mut handle = sandbox
            .shell_stream(script)
            .await
            .map_err(|e| RuntimeError::Backend(format!("microsandbox shell_stream: {e}")))?;

        // Bridge the SDK's UnboundedReceiver<MsExecEvent> into our
        // Stream<ExecEvent>. We can't move `handle` directly across a
        // .await in a stream::unfold without owning it, so we drain it
        // into our own channel via a detached task.
        let (tx, rx) = mpsc::channel::<ExecEvent>(32);
        crate::compat::spawn_detached(async move {
            while let Some(ev) = handle.recv().await {
                let mapped = match ev {
                    MsExecEvent::Started { .. } => continue, // skip
                    MsExecEvent::Stdout(b) => ExecEvent::Stdout(b),
                    MsExecEvent::Stderr(b) => ExecEvent::Stderr(b),
                    MsExecEvent::Exited { code } => ExecEvent::Exit { code, signal: None },
                    MsExecEvent::Failed(f) => {
                        warn!(
                            stage = "exec_failed",
                            error = ?f,
                            "microsandbox exec failed to start"
                        );
                        ExecEvent::Exit {
                            code: -1,
                            signal: None,
                        }
                    }
                };
                if tx.send(mapped).await.is_err() {
                    return;
                }
            }
        });

        Ok(ExecHandle {
            output: Box::pin(receiver_stream(rx)),
        })
    }

    async fn write_file(&self, path: &str, bytes: Bytes) -> RuntimeResult<()> {
        let guard = self.sandbox.lock().await;
        let sandbox = guard
            .as_ref()
            .ok_or_else(|| RuntimeError::Backend("session already terminated".into()))?;
        sandbox
            .fs()
            .write(path, bytes.as_ref())
            .await
            .map_err(|e| RuntimeError::Backend(format!("write_file {path}: {e}")))
    }

    async fn read_file(&self, path: &str) -> RuntimeResult<Bytes> {
        let guard = self.sandbox.lock().await;
        let sandbox = guard
            .as_ref()
            .ok_or_else(|| RuntimeError::Backend("session already terminated".into()))?;
        sandbox
            .fs()
            .read(path)
            .await
            .map_err(|e| RuntimeError::Backend(format!("read_file {path}: {e}")))
    }

    async fn terminate(&self) -> RuntimeResult<()> {
        let mut guard = self.sandbox.lock().await;
        let Some(sandbox) = guard.take() else {
            return Ok(());
        };
        match sandbox.stop_and_wait().await {
            Ok(status) => {
                info!(
                    stage = "microsandbox_stopped",
                    exit_code = status.code().unwrap_or(-1),
                    "microsandbox VM stopped cleanly"
                );
                Ok(())
            }
            Err(e) => {
                error!(
                    stage = "microsandbox_stop_failed",
                    error = %e,
                    "Failed to stop microsandbox VM"
                );
                Err(RuntimeError::Backend(format!("stop_and_wait: {e}")))
            }
        }
    }
}

fn receiver_stream<T>(mut rx: mpsc::Receiver<T>) -> impl Stream<Item = T> + Send
where
    T: Send + 'static,
{
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}
