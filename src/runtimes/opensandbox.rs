//! OpenSandbox runtime adapter.
//!
//! Speaks the Alibaba OpenSandbox Lifecycle API (`POST /v1/sandboxes`,
//! `DELETE /v1/sandboxes/{id}`) to provision/tear down sandboxes, then
//! the execd API (`POST /command`) inside each sandbox to run shell
//! commands. Hadrian sees only the Lifecycle endpoint; the OpenSandbox
//! control plane (Docker or Kubernetes) handles the actual container
//! orchestration.
//!
//! Spec: <https://github.com/alibaba/OpenSandbox/blob/main/specs/sandbox-lifecycle.yml>
//! Execd spec: <https://github.com/alibaba/OpenSandbox/blob/main/specs/execd-api.yaml>

#![cfg(feature = "runtime-opensandbox")]

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    config::OpenSandboxConfig,
    runtimes::{
        ExecEvent, ExecHandle, ExecRequest, NetworkMode, RuntimeCapabilities, RuntimeError,
        RuntimeResult, SessionHandle, SessionSpec, ShellRuntime, ShellSession,
    },
    streaming::SseBuffer,
};

/// Default container image when the request doesn't specify one.
const DEFAULT_IMAGE: &str = "python:3.11-slim";

/// Port that OpenSandbox's execd listens on inside every sandbox.
const EXECD_PORT: u16 = 44772;

/// Status states from the Lifecycle API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum SandboxState {
    Pending,
    Running,
    Pausing,
    Paused,
    Resuming,
    Stopping,
    Terminated,
    Failed,
}

#[derive(Debug, Deserialize)]
struct SandboxStatusObj {
    state: SandboxState,
}

#[derive(Debug, Deserialize)]
struct SandboxResponse {
    id: String,
    status: SandboxStatusObj,
}

#[derive(Debug, Serialize)]
struct CreateSandboxRequest<'a> {
    image: ImageSpec<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "resourceLimits")]
    resource_limits: Option<ResourceLimits<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entrypoint: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "networkPolicy")]
    network_policy: Option<NetworkPolicySpec<'a>>,
}

#[derive(Debug, Serialize)]
struct ImageSpec<'a> {
    uri: &'a str,
}

#[derive(Debug, Serialize)]
struct ResourceLimits<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct NetworkPolicySpec<'a> {
    #[serde(rename = "defaultAction")]
    default_action: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    egress: Vec<EgressRule<'a>>,
}

#[derive(Debug, Serialize)]
struct EgressRule<'a> {
    action: &'a str,
    target: &'a str,
}

#[derive(Debug, Serialize)]
struct RunCommandRequest<'a> {
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    #[serde(default)]
    background: bool,
}

#[derive(Debug, Deserialize)]
struct EndpointResponse {
    url: String,
}

/// `ShellRuntime` implementation that delegates to an OpenSandbox
/// control plane.
pub struct OpenSandboxRuntime {
    config: OpenSandboxConfig,
    http: Client,
}

impl OpenSandboxRuntime {
    pub fn new(config: OpenSandboxConfig, http: Client) -> Self {
        Self { config, http }
    }

    fn base_url(&self) -> &str {
        self.config.endpoint.trim_end_matches('/')
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.config.auth_token {
            req.header("OPEN-SANDBOX-API-KEY", key)
        } else {
            req
        }
    }

    async fn create_sandbox(&self, body: CreateSandboxRequest<'_>) -> RuntimeResult<String> {
        let url = format!("{}/sandboxes", self.base_url());
        let resp = self
            .apply_auth(self.http.post(&url).json(&body))
            .send()
            .await
            .map_err(|e| RuntimeError::Unreachable(format!("create sandbox: {e}")))?;
        if !resp.status().is_success() {
            return Err(RuntimeError::Backend(format!(
                "create sandbox failed: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )));
        }
        let sandbox: SandboxResponse = resp
            .json()
            .await
            .map_err(|e| RuntimeError::Backend(format!("decode create response: {e}")))?;
        Ok(sandbox.id)
    }

    /// Poll the sandbox until it reaches Running (or terminal failure)
    /// within the configured timeout. Uses exponential backoff so a
    /// slow control plane gets fewer polls per second over time.
    async fn wait_for_running(&self, id: &str) -> RuntimeResult<()> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.config.start_timeout_secs);
        let mut backoff = Duration::from_millis(100);
        let max_backoff = Duration::from_secs(2);
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(RuntimeError::SessionTimeout);
            }
            let url = format!("{}/sandboxes/{}", self.base_url(), id);
            let resp = self
                .apply_auth(self.http.get(&url))
                .send()
                .await
                .map_err(|e| RuntimeError::Unreachable(format!("get sandbox: {e}")))?;
            if !resp.status().is_success() {
                return Err(RuntimeError::Backend(format!(
                    "get sandbox failed: {}",
                    resp.status()
                )));
            }
            let sandbox: SandboxResponse = resp
                .json()
                .await
                .map_err(|e| RuntimeError::Backend(format!("decode get response: {e}")))?;
            match sandbox.status.state {
                SandboxState::Running => return Ok(()),
                SandboxState::Failed | SandboxState::Terminated => {
                    return Err(RuntimeError::Backend(format!(
                        "sandbox entered terminal state {:?} before running",
                        sandbox.status.state
                    )));
                }
                _ => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }

    async fn fetch_execd_url(&self, id: &str) -> RuntimeResult<String> {
        let url = format!(
            "{}/sandboxes/{}/endpoints/{}",
            self.base_url(),
            id,
            EXECD_PORT
        );
        let resp = self
            .apply_auth(self.http.get(&url))
            .send()
            .await
            .map_err(|e| RuntimeError::Unreachable(format!("get endpoint: {e}")))?;
        if !resp.status().is_success() {
            return Err(RuntimeError::Backend(format!(
                "get endpoint failed: {}",
                resp.status()
            )));
        }
        let ep: EndpointResponse = resp
            .json()
            .await
            .map_err(|e| RuntimeError::Backend(format!("decode endpoint response: {e}")))?;
        Ok(ep.url)
    }

    async fn delete_sandbox(&self, id: &str) -> RuntimeResult<()> {
        let url = format!("{}/sandboxes/{}", self.base_url(), id);
        let resp = self
            .apply_auth(self.http.delete(&url))
            .send()
            .await
            .map_err(|e| RuntimeError::Unreachable(format!("delete sandbox: {e}")))?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(RuntimeError::Backend(format!(
                "delete sandbox failed: {}",
                resp.status()
            )));
        }
        Ok(())
    }

    /// Best-effort cleanup of a sandbox we created but couldn't fully
    /// hand off to a session. Logs the failure (with the rollback
    /// stage) instead of silently swallowing it — orphaned sandboxes
    /// burn operator budget and are easy to lose visibility on.
    async fn cleanup_orphan(&self, id: &str, stage: &'static str) {
        if let Err(e) = self.delete_sandbox(id).await {
            warn!(
                sandbox_id = %id,
                stage,
                error = %e,
                "Failed to delete orphaned opensandbox during rollback"
            );
        }
    }
}

#[async_trait]
impl ShellRuntime for OpenSandboxRuntime {
    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            passthrough_only: false,
            // Per-domain secret injection isn't natively supported by
            // execd; secrets get injected via `envs` which makes them
            // visible in the env block but not destination-scoped.
            // Mark unsupported so the orchestrator fails fast.
            secret_injection: false,
            // OpenSandbox supports network policies natively
            // (default_action + egress allowlist / denylist).
            egress_allowlist: true,
            skill_mount: true,
            // Backed by execd's `GET /files/download?path=` and
            // `POST /files/upload` (the same upload endpoint used by
            // `mount_skill_via_execd`).
            file_io: true,
            network_isolation_modes: vec![NetworkMode::Full, NetworkMode::AllowList],
            max_session_duration: None,
        }
    }

    async fn start_session(&self, spec: SessionSpec) -> RuntimeResult<SessionHandle> {
        if !spec.egress_policy.secrets.is_empty() {
            return Err(RuntimeError::Unsupported("secret_injection"));
        }

        let session_id = spec
            .session_id_hint
            .unwrap_or_else(|| format!("hadrian-{}", Uuid::new_v4()));

        // Build network policy from egress allowlist if present.
        let allow_hosts = spec.egress_policy.allow_hosts.clone();
        let network_policy = if allow_hosts.is_empty() {
            None
        } else {
            Some(NetworkPolicySpec {
                default_action: "deny",
                egress: allow_hosts
                    .iter()
                    .map(|h| EgressRule {
                        action: "allow",
                        target: h.as_str(),
                    })
                    .collect(),
            })
        };

        let cpu_str = spec.cpu_limit.map(|c| format!("{}m", (c * 1000.0) as u64));
        let mem_str = spec
            .mem_limit_bytes
            .map(|b| format!("{}Mi", b / (1024 * 1024)));
        let resource_limits = match (&cpu_str, &mem_str) {
            (None, None) => None,
            (cpu, mem) => Some(ResourceLimits {
                cpu: cpu.as_deref(),
                memory: mem.as_deref(),
            }),
        };

        let image = self
            .config
            .default_image
            .as_deref()
            .unwrap_or(DEFAULT_IMAGE);

        info!(
            stage = "opensandbox_create",
            session_id = %session_id,
            image,
            "Creating OpenSandbox sandbox"
        );

        let body = CreateSandboxRequest {
            image: ImageSpec { uri: image },
            timeout: Some(3600),
            resource_limits,
            entrypoint: vec!["tail", "-f", "/dev/null"],
            network_policy,
        };

        let id = self.create_sandbox(body).await?;
        if let Err(e) = self.wait_for_running(&id).await {
            self.cleanup_orphan(&id, "wait_for_running").await;
            return Err(e);
        }
        let execd_url = match self.fetch_execd_url(&id).await {
            Ok(u) => u,
            Err(e) => {
                self.cleanup_orphan(&id, "fetch_execd_url").await;
                return Err(e);
            }
        };

        // Mount requested skills via execd /files/upload. Each file is
        // a separate multipart POST; small bundles fit comfortably,
        // larger ones get serialized writes.
        for skill in &spec.mounted_skills {
            if let Err(e) = mount_skill_via_execd(
                &self.http,
                &execd_url,
                self.config.auth_token.as_deref(),
                skill,
            )
            .await
            {
                self.cleanup_orphan(&id, "mount_skill_via_execd").await;
                return Err(e);
            }
            debug!(
                stage = "opensandbox_skill_mounted",
                skill_id = %skill.skill_id,
                mount_path = %skill.mount_path,
                file_count = skill.files.len(),
                "Mounted skill bundle"
            );
        }

        let session = OpenSandboxSession {
            http: self.http.clone(),
            execd_url,
            sandbox_id: id.clone(),
            base_url: self.base_url().to_string(),
            api_key: self.config.auth_token.clone(),
            terminated: Arc::new(Mutex::new(false)),
        };

        Ok(SessionHandle::new(session_id, Box::new(session)))
    }
}

/// Upload a skill bundle to OpenSandbox via execd `/files/upload`.
/// Each file is one multipart POST; the `metadata` part is a JSON
/// `FileMetadata` followed by the binary `file` part.
async fn mount_skill_via_execd(
    http: &Client,
    execd_url: &str,
    api_key: Option<&str>,
    skill: &crate::runtimes::SkillMount,
) -> RuntimeResult<()> {
    let upload_url = format!("{}/files/upload", execd_url.trim_end_matches('/'));
    for file in &skill.files {
        // Reject path traversal and absolute paths. Skills come from
        // Hadrian's own SkillService, but treat their `relative_path`
        // as untrusted — a single malicious entry of `../etc/passwd`
        // would otherwise let one skill clobber arbitrary files inside
        // the sandbox (and across tenants if a skill is shared).
        let safe_rel = sanitize_skill_relative_path(&file.relative_path)?;
        let full_path = format!("{}/{}", skill.mount_path.trim_end_matches('/'), safe_rel);
        let metadata = serde_json::json!({
            "path": full_path,
            "mode": 0o644,
        });
        let form = reqwest::multipart::Form::new()
            .text("metadata", metadata.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(file.content.to_vec())
                    .mime_str("application/octet-stream")
                    .unwrap_or_else(|_| reqwest::multipart::Part::bytes(file.content.to_vec())),
            );
        let mut req = http.post(&upload_url).multipart(form);
        if let Some(key) = api_key {
            req = req.header("OPEN-SANDBOX-API-KEY", key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| RuntimeError::Unreachable(format!("skill upload: {e}")))?;
        if !resp.status().is_success() {
            return Err(RuntimeError::Backend(format!(
                "skill upload {} failed: {}",
                full_path,
                resp.status()
            )));
        }
    }
    Ok(())
}

/// Reject path traversal in skill-file relative paths. Returns the
/// cleaned path on success (leading slashes stripped) or a
/// `RuntimeError::Backend` on any `..` segment, absolute prefix, or
/// non-normal path component (e.g. `.`, prefix verbatim on Windows).
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
            Component::CurDir => {
                // `./foo` is harmless; just skip the component.
            }
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

struct OpenSandboxSession {
    http: Client,
    execd_url: String,
    sandbox_id: String,
    base_url: String,
    api_key: Option<String>,
    terminated: Arc<Mutex<bool>>,
}

impl OpenSandboxSession {
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.api_key {
            req.header("OPEN-SANDBOX-API-KEY", key)
        } else {
            req
        }
    }
}

#[async_trait]
impl ShellSession for OpenSandboxSession {
    async fn exec(&self, cmd: ExecRequest) -> RuntimeResult<ExecHandle> {
        let url = format!("{}/command", self.execd_url.trim_end_matches('/'));
        let timeout_ms = cmd.timeout.map(|d| d.as_millis() as u64);
        let body = RunCommandRequest {
            command: &cmd.command,
            timeout: timeout_ms,
            cwd: None,
            background: false,
        };
        let resp = self
            .auth(
                self.http
                    .post(&url)
                    .header(header::ACCEPT, "text/event-stream")
                    .json(&body),
            )
            .send()
            .await
            .map_err(|e| RuntimeError::Unreachable(format!("exec: {e}")))?;
        if !resp.status().is_success() {
            return Err(RuntimeError::Backend(format!(
                "exec failed: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )));
        }

        // Bridge the byte stream into an ExecEvent stream by parsing SSE.
        let byte_stream = resp.bytes_stream();
        let output = bridge_sse_to_exec_events(byte_stream);
        Ok(ExecHandle { output })
    }

    async fn write_file(&self, path: &str, bytes: Bytes) -> RuntimeResult<()> {
        let upload_url = format!("{}/files/upload", self.execd_url.trim_end_matches('/'));
        let metadata = serde_json::json!({
            "path": path,
            "mode": 0o644,
        });
        let form = reqwest::multipart::Form::new()
            .text("metadata", metadata.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes.to_vec())
                    .mime_str("application/octet-stream")
                    .unwrap_or_else(|_| reqwest::multipart::Part::bytes(bytes.to_vec())),
            );
        let resp = self
            .auth(self.http.post(&upload_url).multipart(form))
            .send()
            .await
            .map_err(|e| RuntimeError::Unreachable(format!("write_file {path}: {e}")))?;
        if !resp.status().is_success() {
            return Err(RuntimeError::Backend(format!(
                "write_file {path} failed: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn read_file(&self, path: &str) -> RuntimeResult<Bytes> {
        let download_url = format!("{}/files/download", self.execd_url.trim_end_matches('/'));
        let resp = self
            .auth(self.http.get(&download_url).query(&[("path", path)]))
            .send()
            .await
            .map_err(|e| RuntimeError::Unreachable(format!("read_file {path}: {e}")))?;
        if !resp.status().is_success() {
            return Err(RuntimeError::Backend(format!(
                "read_file {path} failed: {}",
                resp.status()
            )));
        }
        resp.bytes()
            .await
            .map_err(|e| RuntimeError::Backend(format!("read_file {path} body: {e}")))
    }

    async fn terminate(&self) -> RuntimeResult<()> {
        let mut t = self.terminated.lock().await;
        if *t {
            return Ok(());
        }
        let url = format!("{}/sandboxes/{}", self.base_url, self.sandbox_id);
        let req = self.http.delete(&url);
        let req = match &self.api_key {
            Some(k) => req.header("OPEN-SANDBOX-API-KEY", k),
            None => req,
        };
        match req.send().await {
            Ok(resp)
                if resp.status().is_success()
                    || resp.status() == reqwest::StatusCode::NOT_FOUND =>
            {
                *t = true;
                debug!(
                    stage = "opensandbox_terminated",
                    sandbox_id = %self.sandbox_id,
                    "OpenSandbox sandbox terminated"
                );
                Ok(())
            }
            Ok(resp) => Err(RuntimeError::Backend(format!(
                "delete sandbox failed: {}",
                resp.status()
            ))),
            Err(e) => Err(RuntimeError::Unreachable(format!("delete sandbox: {e}"))),
        }
    }
}

/// Parse the execd `/command` SSE stream into [`ExecEvent`]s.
///
/// Recognised event types per the execd spec: `stdout`, `stderr`,
/// `execution_complete`, `error`. Others are ignored. The stream
/// always ends with an `Exit` event so the caller's loop terminates;
/// if the upstream stream closes without one, we synthesize `Exit
/// { code: -1 }`.
fn bridge_sse_to_exec_events(
    mut byte_stream: impl Stream<Item = reqwest::Result<Bytes>> + Send + Unpin + 'static,
) -> std::pin::Pin<Box<dyn Stream<Item = ExecEvent> + Send>> {
    let (tx, rx) = mpsc::channel::<ExecEvent>(32);
    crate::compat::spawn_detached(async move {
        let mut sse = SseBuffer::new();
        let mut exit_emitted = false;
        while let Some(chunk_result) = byte_stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "OpenSandbox SSE stream error");
                    break;
                }
            };
            sse.extend(&chunk);
            for event in sse.extract_complete_events() {
                let Some((event_type, text, exit_code)) = parse_execd_event(&event) else {
                    continue;
                };
                let mapped = match event_type.as_str() {
                    "stdout" => ExecEvent::Stdout(Bytes::from(text.unwrap_or_default())),
                    "stderr" => ExecEvent::Stderr(Bytes::from(text.unwrap_or_default())),
                    "execution_complete" => {
                        exit_emitted = true;
                        ExecEvent::Exit {
                            code: exit_code.unwrap_or(0),
                            signal: None,
                        }
                    }
                    "error" => {
                        exit_emitted = true;
                        error!(
                            stage = "opensandbox_exec_error",
                            text = ?text,
                            "Execd reported error"
                        );
                        ExecEvent::Exit {
                            code: -1,
                            signal: None,
                        }
                    }
                    _ => continue,
                };
                if tx.send(mapped).await.is_err() {
                    return;
                }
            }
        }
        if !exit_emitted {
            let _ = tx
                .send(ExecEvent::Exit {
                    code: -1,
                    signal: None,
                })
                .await;
        }
    });
    let mut rx = rx;
    Box::pin(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)))
}

/// Best-effort parser for one SSE event payload. Returns
/// (event_type, optional text, optional exit_code) when the payload
/// is a JSON object with a `type` field.
fn parse_execd_event(event: &[u8]) -> Option<(String, Option<String>, Option<i32>)> {
    let s = std::str::from_utf8(event).ok()?;
    for line in s.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        let json: serde_json::Value = serde_json::from_str(data).ok()?;
        let event_type = json.get("type")?.as_str()?.to_string();
        let text = json.get("text").and_then(|v| v.as_str()).map(String::from);
        // execd doesn't always carry exit code; check `data.code` or
        // `exitCode` if present.
        let exit_code = json
            .get("data")
            .and_then(|d| d.get("code"))
            .and_then(|v| v.as_i64())
            .or_else(|| json.get("exitCode").and_then(|v| v.as_i64()))
            .map(|v| v as i32);
        return Some((event_type, text, exit_code));
    }
    None
}
