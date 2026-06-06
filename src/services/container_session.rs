//! Container session: a wrapper around `SessionHandle` that snapshots
//! `/mnt/data` between shell commands and captures any
//! new/changed files as `ContainerFileRef`s.
//!
//! One session per `ShellExecutor` (i.e. per response). File content
//! is committed to the `container_files` table for durability and
//! shadowed in process memory for fast access during the active
//! response.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::{
    api_types::responses::{ContainerFileRef, ContainerFileSource},
    config::ContainersConfig,
    db::repos::ResponseOwner,
    runtimes::{
        ExecRequest, RuntimeError, RuntimeResult, SessionHandle, SessionSpec, ShellRuntime,
    },
    services::{
        containers::{ContainersService, PersistFileInput},
        input_file_staging::StagedFile,
    },
};

/// Root of the writable workspace inside the container.
pub const MNT_DATA: &str = "/mnt/data";

/// Generate a `cntr_<simple-uuid>` identifier.
pub fn new_container_id() -> String {
    format!("cntr_{}", Uuid::new_v4().simple())
}

/// Generate a `cfile_<simple-uuid>` identifier.
pub fn new_file_id() -> String {
    format!("cfile_{}", Uuid::new_v4().simple())
}

/// One file currently being tracked by the session.
///
/// `content` is the in-memory copy used to build the persist payload
/// for `container_files`; `hash` mirrors the snapshot entry so the
/// persist payload can carry the canonical content-hash without
/// re-digesting the bytes.
#[derive(Debug, Clone)]
struct TrackedFile {
    file_id: String,
    path: String,
    filename: String,
    bytes: u64,
    content_type: Option<String>,
    source: ContainerFileSource,
    content: Bytes,
    hash: [u8; 32],
}

impl TrackedFile {
    fn to_ref(&self, container_id: &str) -> ContainerFileRef {
        ContainerFileRef {
            container_id: container_id.to_string(),
            file_id: self.file_id.clone(),
            filename: self.filename.clone(),
            path: self.path.clone(),
            bytes: self.bytes,
            content_type: self.content_type.clone(),
            source: self.source,
        }
    }
}

/// Mutable state inside a `ContainerSession`.
///
/// Held behind a `Mutex` because parallel shell tool calls dispatched
/// by the runner share one session and must serialize access to the
/// snapshot map and byte counters.
#[derive(Default)]
struct SessionState {
    /// path -> sha256 of every file under `/mnt/data` as of the last
    /// snapshot. Drives the new/changed diff after each exec.
    snapshot: HashMap<String, [u8; 32]>,
    /// Files captured during this session, indexed by path. Latest
    /// version per path wins (overwrites replace the prior entry).
    captured: HashMap<String, TrackedFile>,
    /// Sum of `bytes` across all current `captured` entries, used to
    /// enforce `max_bytes_per_session`.
    bytes_total: u64,
    /// Wall-clock of the last successful `exec()`. Drives idle TTL.
    last_active: Option<Instant>,
}

/// Process-wide cache of live [`ContainerSession`]s keyed by their
/// container id. Lets multiple `/v1/responses` requests share one
/// VM across the lifetime of a container.
///
/// Each entry holds an `Arc<Mutex<ContainerSession>>`. Shell commands
/// from concurrent requests serialize through the mutex; the registry
/// itself is lock-free for lookups. When the reaper or
/// `DELETE /v1/containers/{id}` evicts a row, dropping the registry's
/// Arc lets `ContainerSession::drop` tear the VM down once the last
/// outstanding request finishes with it.
#[derive(Default)]
pub struct ContainerSessionRegistry {
    sessions: dashmap::DashMap<String, Arc<Mutex<ContainerSession>>>,
}

impl ContainerSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a live session by id. Cheap; lock-free.
    pub fn get(&self, container_id: &str) -> Option<Arc<Mutex<ContainerSession>>> {
        self.sessions.get(container_id).map(|e| e.clone())
    }

    /// Insert a freshly-started session. If an entry already exists
    /// for this id (e.g. two requests for the same `previous_response_id`
    /// raced to create), the new entry wins and the previous one is
    /// returned so the caller can terminate it explicitly.
    pub fn insert(
        &self,
        container_id: String,
        session: ContainerSession,
    ) -> (
        Arc<Mutex<ContainerSession>>,
        Option<Arc<Mutex<ContainerSession>>>,
    ) {
        let arc = Arc::new(Mutex::new(session));
        let prior = self.sessions.insert(container_id, arc.clone());
        (arc, prior)
    }

    /// Atomic get-or-insert: returns the existing session if one is
    /// already registered for `container_id`, otherwise inserts the
    /// session produced by `build()` and returns it. The booted
    /// session — including the underlying VM — is only constructed
    /// when no prior entry exists, so a race between two requests for
    /// the same `previous_response_id` boots exactly one VM instead
    /// of two (with the loser then being torn down).
    ///
    /// Returns `(arc, inserted)` where `inserted` is `false` when the
    /// existing entry won the race. On `build` failure the slot is
    /// left empty and the error bubbles up.
    pub async fn get_or_try_insert_with<F, Fut, E>(
        &self,
        container_id: String,
        build: F,
    ) -> Result<(Arc<Mutex<ContainerSession>>, bool), E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<ContainerSession, E>>,
    {
        // `DashMap::entry` blocks any other writer on the same shard
        // for the duration the `Entry` is held. We do NOT hold it
        // across the `build().await` (it's a sync mutex internally —
        // holding across await would block all other shard writers
        // including unrelated containers). Instead, fast-path a
        // get(), boot, then re-check under entry().
        if let Some(existing) = self.sessions.get(&container_id) {
            return Ok((existing.clone(), false));
        }
        let session = build().await?;
        let arc_new = Arc::new(Mutex::new(session));
        // CAS: only insert if still vacant. If a parallel request
        // raced us and registered first, drop our freshly-booted VM
        // (its `Drop` impl detaches a terminate task).
        let entry = self.sessions.entry(container_id);
        match entry {
            dashmap::Entry::Occupied(o) => Ok((o.get().clone(), false)),
            dashmap::Entry::Vacant(v) => {
                v.insert(arc_new.clone());
                Ok((arc_new, true))
            }
        }
    }

    /// Drop the registry's reference to this session. Returns the
    /// removed entry so callers can terminate it explicitly when
    /// they need ordered cleanup; ignoring the return value relies on
    /// `ContainerSession::drop` to detach the terminate task.
    pub fn remove(&self, container_id: &str) -> Option<Arc<Mutex<ContainerSession>>> {
        self.sessions.remove(container_id).map(|(_, v)| v)
    }

    /// Snapshot of the ids of all live sessions. Used by the reaper to
    /// reconcile this replica's local registry against expired DB rows
    /// (the registry is process-local, so every replica must evict its
    /// own sessions even when a different replica flipped the row).
    pub fn ids(&self) -> Vec<String> {
        self.sessions.iter().map(|e| e.key().clone()).collect()
    }

    /// Number of live sessions. Used by tests and the
    /// `hadrian_container_session_count` gauge.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Persistence wiring for a session. Optional — when `None`, files
/// captured under `/mnt/data` live only in memory and never make it
/// to the database (no `GET /v1/containers/{id}/files/...` route
/// resolves). Populated whenever a database is configured and the
/// pipeline can derive a [`ResponseOwner`] for the request.
#[derive(Clone)]
pub struct ContainerPersistence {
    pub service: Arc<ContainersService>,
    pub org_id: uuid::Uuid,
    pub owner: ResponseOwner,
    pub source_response_id: Option<String>,
}

/// A persistent shell-tool session backed by a `SessionHandle`.
///
/// Owns the underlying VM for the lifetime of one response. Future
/// work may extend ownership to a `Container` resource that spans
/// multiple responses. Implements `Drop` to detach a terminate task
/// so the VM is cleaned up even on client disconnect.
pub struct ContainerSession {
    pub container_id: String,
    pub runtime_label: &'static str,
    pub created_at: Instant,
    pub idle_ttl: Duration,
    file_io: bool,
    session: Option<SessionHandle>,
    config: ContainersConfig,
    state: Mutex<SessionState>,
    persistence: Option<ContainerPersistence>,
}

impl ContainerSession {
    /// Boot a fresh VM and provision a new `containers` row. The
    /// caller is responsible for inserting the returned session into
    /// a `ContainerSessionRegistry` (the registry doesn't try to
    /// auto-register so the caller controls the keying and can react
    /// to insert races).
    pub async fn start_new(
        runtime: Arc<dyn ShellRuntime>,
        runtime_label: &'static str,
        spec: SessionSpec,
        config: ContainersConfig,
        persistence: Option<ContainerPersistence>,
        idle_ttl_secs_override: Option<i64>,
    ) -> RuntimeResult<Self> {
        let container_id = new_container_id();
        Self::boot_inner(
            container_id,
            runtime,
            runtime_label,
            spec,
            config,
            persistence,
            None,
            idle_ttl_secs_override,
        )
        .await
    }

    /// Boot a VM bound to a specific container id. If the
    /// `containers` row for `container_id` exists and is active, its
    /// persisted files are replayed into `/mnt/data` so the model
    /// picks up where it left off. If the row is missing, this method
    /// also inserts it.
    ///
    /// Returns [`RuntimeError::Backend`] when the row exists but is
    /// no longer reusable (`expired` / `deleted`). Callers translate
    /// to a 410-style error for explicit reuse or silently fall back
    /// to `start_new` for implicit chaining.
    pub async fn start_attached(
        container_id: String,
        runtime: Arc<dyn ShellRuntime>,
        runtime_label: &'static str,
        spec: SessionSpec,
        config: ContainersConfig,
        persistence: ContainerPersistence,
        idle_ttl_secs_override: Option<i64>,
    ) -> RuntimeResult<Self> {
        // Materialise the row before booting the VM so a failure
        // (expired container, DB outage) costs us nothing. Reattach
        // does *not* extend the TTL — the container row's
        // `idle_ttl_secs` is whatever was set at creation time and
        // stays put across reuse.
        let initial_ttl = idle_ttl_secs_override.unwrap_or(config.default_idle_ttl_secs as i64);
        persistence
            .service
            .ensure_container(
                container_id.clone(),
                persistence.org_id,
                persistence.owner,
                runtime_label,
                persistence.source_response_id.clone(),
                initial_ttl,
            )
            .await
            .map_err(|e| RuntimeError::Backend(format!("ensure_container: {e}")))?;

        Self::boot_inner(
            container_id,
            runtime,
            runtime_label,
            spec,
            config,
            Some(persistence.clone()),
            Some(persistence),
            idle_ttl_secs_override,
        )
        .await
    }

    /// Shared boot path. When `replay` is `Some`, skips provisioning
    /// (the row already exists) and replays persisted files into
    /// `/mnt/data` after the baseline `mkdir`.
    #[allow(clippy::too_many_arguments)] // each arg is load-bearing; bundling doesn't help
    async fn boot_inner(
        container_id: String,
        runtime: Arc<dyn ShellRuntime>,
        runtime_label: &'static str,
        spec: SessionSpec,
        config: ContainersConfig,
        persistence: Option<ContainerPersistence>,
        replay: Option<ContainerPersistence>,
        idle_ttl_secs_override: Option<i64>,
    ) -> RuntimeResult<Self> {
        let file_io = runtime.capabilities().file_io;
        let session = runtime.start_session(spec).await?;

        let initial_ttl = idle_ttl_secs_override.unwrap_or(config.default_idle_ttl_secs as i64);

        // For fresh containers, insert the row up front so a
        // simultaneous chain-reuse request can find it. For reattach,
        // skip — the row was created in a previous response.
        let persistence = if replay.is_some() {
            persistence
        } else {
            match persistence {
                Some(p) => match p
                    .service
                    .provision(
                        container_id.clone(),
                        p.org_id,
                        p.owner,
                        runtime_label,
                        p.source_response_id.clone(),
                        initial_ttl,
                    )
                    .await
                {
                    Ok(_) => Some(p),
                    Err(e) => {
                        warn!(
                            stage = "container_provision_failed",
                            container_id = %container_id,
                            error = %e,
                            "Failed to insert containers row; continuing in-memory only"
                        );
                        None
                    }
                },
                None => None,
            }
        };

        // Once the container row exists (whether freshly provisioned
        // or reattached), stamp the current response's
        // `container_id` column. Lets the next chained request find
        // this container via `previous_response_id`.
        if let Some(ref p) = persistence
            && let Some(ref resp_id) = p.source_response_id
            && let Err(e) = p
                .service
                .link_response_to_container(resp_id, &container_id, p.org_id)
                .await
        {
            warn!(
                stage = "container_link_response_failed",
                container_id = %container_id,
                response_id = %resp_id,
                error = %e,
                "Failed to stamp responses.container_id; chain reuse from this response won't work"
            );
        }

        let effective_ttl_secs = if initial_ttl > 0 {
            initial_ttl as u64
        } else {
            config.default_idle_ttl_secs
        };
        let session_obj = Self {
            container_id,
            runtime_label,
            created_at: Instant::now(),
            idle_ttl: Duration::from_secs(effective_ttl_secs.max(1)),
            file_io,
            session: Some(session),
            config,
            state: Mutex::new(SessionState::default()),
            persistence,
        };

        if file_io && session_obj.config.enabled {
            // Ensure /mnt/data exists. `mkdir -p` is a no-op if it
            // already does; either way it's cheap.
            if let Err(e) = session_obj
                .exec_blocking("mkdir -p /mnt/data".to_string(), Duration::from_secs(5))
                .await
            {
                warn!(
                    stage = "container_mkdir_failed",
                    container_id = %session_obj.container_id,
                    error = %e,
                    "Failed to create /mnt/data — artifact capture disabled for this session"
                );
            } else {
                if let Some(replay_ctx) = replay {
                    // Replay persisted files into the fresh VM so the
                    // model sees the same `/mnt/data` it left in the
                    // prior response. Files are restored as
                    // `source: assistant|user` from the DB rows; the
                    // snapshot map is seeded so capture_changes only
                    // reports genuinely new edits.
                    if let Err(e) = session_obj.replay_from_db(&replay_ctx).await {
                        warn!(
                            stage = "container_replay_failed",
                            container_id = %session_obj.container_id,
                            error = %e,
                            "Failed to replay persisted files into /mnt/data — \
                             continuing with empty workspace"
                        );
                    }
                }

                // Baseline snapshot. Errors here are logged but not
                // fatal — the model can still run commands; we just
                // won't know about pre-existing files.
                if let Err(e) = session_obj.refresh_snapshot().await {
                    warn!(
                        stage = "container_baseline_snapshot_failed",
                        container_id = %session_obj.container_id,
                        error = %e,
                        "Failed to seed /mnt/data baseline snapshot"
                    );
                }
            }
        }

        Ok(session_obj)
    }

    /// Restore persisted `container_files` rows into the live VM's
    /// `/mnt/data`. Called by [`Self::start_attached`].
    async fn replay_from_db(&self, persistence: &ContainerPersistence) -> RuntimeResult<()> {
        let svc = &persistence.service;
        let records = svc
            .list_files_for_replay(&self.container_id)
            .await
            .map_err(|e| RuntimeError::Backend(format!("replay list: {e}")))?;
        if records.is_empty() {
            return Ok(());
        }

        let session = self.session()?;
        let mut state = self.state.lock().await;
        let mut replayed = 0usize;
        let mut bytes_total: u64 = 0;
        for rec in records.into_iter().rev() {
            // Reverse so an overwrite history applies in time order.
            let bytes = match svc.read_content_for_replay(&rec).await {
                Ok(Some(b)) => Bytes::from(b),
                Ok(None) => {
                    // Metadata-only row (oversized; content was
                    // dropped). Restore the snapshot entry without
                    // bytes; the model sees the file is "gone" until
                    // it writes again.
                    let hash = sha256_bytes(&[]);
                    state.snapshot.insert(rec.path.clone(), hash);
                    continue;
                }
                Err(e) => {
                    warn!(
                        stage = "container_replay_read_failed",
                        container_id = %self.container_id,
                        path = %rec.path,
                        error = %e,
                        "Failed to read persisted bytes during replay; skipping"
                    );
                    continue;
                }
            };

            if let Err(e) = session.write_file(&rec.path, bytes.clone()).await {
                warn!(
                    stage = "container_replay_write_failed",
                    container_id = %self.container_id,
                    path = %rec.path,
                    error = %e,
                    "Failed to write file into /mnt/data during replay; skipping"
                );
                continue;
            }

            let hash = sha256_bytes(&bytes);
            let source = match rec.source {
                crate::db::repos::ContainerFileSourceKind::User => ContainerFileSource::User,
                crate::db::repos::ContainerFileSourceKind::Assistant => {
                    ContainerFileSource::Assistant
                }
            };
            let tracked = TrackedFile {
                file_id: rec.id.clone(),
                path: rec.path.clone(),
                filename: rec.filename.clone(),
                bytes: bytes.len() as u64,
                content_type: rec.content_type.clone(),
                source,
                content: bytes,
                hash,
            };
            bytes_total = bytes_total.saturating_add(tracked.bytes);
            state.snapshot.insert(rec.path.clone(), hash);
            state.captured.insert(rec.path.clone(), tracked);
            replayed += 1;
        }
        state.bytes_total = bytes_total;
        debug!(
            stage = "container_replay",
            container_id = %self.container_id,
            files = replayed,
            bytes_total,
            "Replayed persisted files into /mnt/data"
        );
        Ok(())
    }

    pub fn file_io_enabled(&self) -> bool {
        self.file_io && self.config.enabled
    }

    fn session(&self) -> RuntimeResult<&SessionHandle> {
        self.session
            .as_ref()
            .ok_or_else(|| RuntimeError::Backend("container session already terminated".into()))
    }

    /// Run one command. On success and when artifact capture is
    /// enabled, snapshot `/mnt/data` afterwards and return the diff
    /// (new/changed files) so the caller can fold them into the
    /// terminal `shell_call_output` item's `output_files` array.
    pub async fn exec(&self, req: ExecRequest) -> RuntimeResult<ExecOutcome> {
        let exec = self.session()?.exec(req).await?;
        {
            let mut state = self.state.lock().await;
            state.last_active = Some(Instant::now());
        }
        // Persist the activity ping so the idle reaper doesn't expire a busy
        // container and the containers API shows a moving last_active_at /
        // expires_at. The in-memory `last_active` above only guards the
        // process-local registry. Best-effort: a failed write must not fail
        // the command.
        if let Some(persistence) = &self.persistence
            && let Err(e) = persistence
                .service
                .touch_last_active(&self.container_id, persistence.org_id)
                .await
        {
            debug!(
                stage = "container_touch_last_active_failed",
                container_id = %self.container_id,
                error = %e,
                "Failed to persist container last_active_at"
            );
        }
        Ok(ExecOutcome { handle: exec })
    }

    /// Helper for one-shot internal commands (mkdir, sha256sum) where
    /// we don't care about streaming output. Captures stdout+stderr
    /// into a single `String` and returns the exit code alongside.
    async fn exec_blocking(&self, command: String, timeout: Duration) -> RuntimeResult<String> {
        use futures_util::StreamExt;

        use crate::runtimes::ExecEvent;

        let exec = self
            .session()?
            .exec(ExecRequest {
                command,
                stdin: None,
                timeout: Some(timeout),
            })
            .await?;
        let mut stdout = Vec::new();
        let mut stream = exec.output;
        while let Some(ev) = stream.next().await {
            match ev {
                ExecEvent::Stdout(b) => stdout.extend_from_slice(&b),
                ExecEvent::Stderr(_) => {}
                ExecEvent::Exit { .. } => break,
            }
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    /// Diff `/mnt/data` after a shell command. Returns one entry per
    /// new or changed file (caller decides what to do with them —
    /// typically attach to the shell_call_output and emit
    /// `file_created` SSE events).
    ///
    /// No-op when `file_io` is unavailable or `[features.containers]
    /// enabled = false`.
    pub async fn capture_changes(&self) -> RuntimeResult<Vec<ContainerFileRef>> {
        if !self.file_io_enabled() {
            return Ok(Vec::new());
        }

        let listing = self.list_mnt_data().await?;

        let mut state = self.state.lock().await;
        let mut new_refs = Vec::new();
        let mut files_this_exec: usize = 0;

        for (path, hash) in &listing {
            let prior = state.snapshot.get(path);
            let changed = match prior {
                None => true,
                Some(prev) => prev != hash,
            };
            if !changed {
                continue;
            }

            if files_this_exec >= self.config.max_files_per_exec {
                warn!(
                    stage = "container_capture_truncated",
                    container_id = %self.container_id,
                    limit = self.config.max_files_per_exec,
                    "Hit max_files_per_exec; remaining changes for this command dropped"
                );
                break;
            }
            files_this_exec += 1;

            // Pull the bytes out of the VM. Errors are logged and the
            // file is skipped rather than aborting the whole capture.
            let bytes = match self.session()?.read_file(path).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        stage = "container_read_failed",
                        container_id = %self.container_id,
                        path = %path,
                        error = %e,
                        "Failed to read captured file"
                    );
                    continue;
                }
            };

            // Preserve the existing TrackedFile.file_id when
            // overwriting a known path. The repo's UPSERT keeps the
            // row's PK stable across overwrites, so any annotation
            // already cited for this path keeps resolving — the
            // in-memory id must match.
            let stable_id = state
                .captured
                .get(path)
                .map(|f| f.file_id.clone())
                .unwrap_or_else(new_file_id);

            if bytes.len() as u64 > self.config.max_bytes_per_file {
                warn!(
                    stage = "container_file_too_large",
                    container_id = %self.container_id,
                    path = %path,
                    bytes = bytes.len(),
                    limit = self.config.max_bytes_per_file,
                    "Captured file exceeds max_bytes_per_file; recording metadata only"
                );
                // Insert a metadata-only entry: no content, but the
                // model and client still see that the file exists.
                let prior_bytes = state.captured.get(path).map(|f| f.bytes).unwrap_or(0);
                state.bytes_total = state.bytes_total.saturating_sub(prior_bytes);
                let filename = filename_from_path(path);
                let entry = TrackedFile {
                    file_id: stable_id,
                    path: path.clone(),
                    filename: filename.clone(),
                    bytes: bytes.len() as u64,
                    content_type: guess_content_type(&filename),
                    source: ContainerFileSource::Assistant,
                    content: Bytes::new(),
                    hash: *hash,
                };
                let r = entry.to_ref(&self.container_id);
                state.captured.insert(path.clone(), entry);
                state.snapshot.insert(path.clone(), *hash);
                new_refs.push(r);
                continue;
            }

            // Enforce the cumulative session cap. If adding this file
            // would put us over, skip it — but still advance the
            // snapshot to this version's hash so we treat it as
            // accounted-for. Without this, the file stays absent from
            // (or stale in) `snapshot`, so every later command re-flags
            // it as changed, re-reads its bytes out of the VM, and
            // re-emits the same warning. Advancing the snapshot means we
            // only warn again if the file's content actually changes.
            let prior_bytes = state.captured.get(path).map(|f| f.bytes).unwrap_or(0);
            let projected = state.bytes_total.saturating_sub(prior_bytes) + bytes.len() as u64;
            if projected > self.config.max_bytes_per_session {
                warn!(
                    stage = "container_session_bytes_limit",
                    container_id = %self.container_id,
                    path = %path,
                    projected,
                    limit = self.config.max_bytes_per_session,
                    "Captured file would exceed max_bytes_per_session; skipping"
                );
                state.snapshot.insert(path.clone(), *hash);
                continue;
            }

            let filename = filename_from_path(path);
            let content_type = guess_content_type(&filename);
            let entry = TrackedFile {
                file_id: stable_id,
                path: path.clone(),
                filename: filename.clone(),
                bytes: bytes.len() as u64,
                content_type,
                source: ContainerFileSource::Assistant,
                content: bytes,
                hash: *hash,
            };
            let r = entry.to_ref(&self.container_id);
            state.bytes_total = projected;
            state.captured.insert(path.clone(), entry);
            state.snapshot.insert(path.clone(), *hash);
            new_refs.push(r);
        }

        // Files that disappeared from /mnt/data: drop their tracking
        // info. We don't emit deletion events — only creations and
        // modifications surface as annotations.
        let removed_paths: Vec<String> = state
            .snapshot
            .keys()
            .filter(|p| !listing.contains_key(p.as_str()))
            .cloned()
            .collect();
        for p in removed_paths {
            if let Some(entry) = state.captured.remove(&p) {
                state.bytes_total = state.bytes_total.saturating_sub(entry.bytes);
            }
            state.snapshot.remove(&p);
        }

        debug!(
            stage = "container_capture",
            container_id = %self.container_id,
            new_or_changed = new_refs.len(),
            total_tracked = state.captured.len(),
            bytes_total = state.bytes_total,
            "Captured /mnt/data changes"
        );

        // Snapshot the newly captured entries while we still hold the
        // state lock so the persist payload sees the same bytes as
        // the in-memory view. Then drop the lock before doing async DB
        // I/O so concurrent capture_changes / read_file calls don't
        // queue behind the persister.
        let persist_payload: Vec<PersistFileInput> = if self.persistence.is_some() {
            new_refs
                .iter()
                .filter_map(|r| state.captured.get(&r.path).map(|t| (r, t)))
                .map(|(_r, t)| PersistFileInput {
                    file_id: t.file_id.clone(),
                    path: t.path.clone(),
                    filename: t.filename.clone(),
                    content_type: t.content_type.clone(),
                    source: t.source,
                    content: t.content.clone(),
                    content_hash_hex: hex::encode(t.hash),
                    source_response_id: self
                        .persistence
                        .as_ref()
                        .and_then(|p| p.source_response_id.clone()),
                    source_call_id: None,
                })
                .collect()
        } else {
            Vec::new()
        };
        drop(state);

        if !persist_payload.is_empty()
            && let Some(p) = self.persistence.as_ref()
            && let Err(e) = p
                .service
                .persist_files(&self.container_id, p.org_id, persist_payload)
                .await
        {
            warn!(
                stage = "container_persist_failed",
                container_id = %self.container_id,
                error = %e,
                "Failed to persist captured files; in-memory annotations remain valid for this response"
            );
        }

        Ok(new_refs)
    }

    /// Write a batch of user-supplied files into `/mnt/data` and
    /// register them as captured artifacts with `source = User`.
    ///
    /// The responses pipeline resolves `input_file` parts into
    /// `StagedFile`s and feeds them in before the first shell command
    /// runs. The snapshot is updated as we go so the next
    /// `capture_changes()` call doesn't re-report these as new/changed.
    ///
    /// No-ops with `Ok(vec![])` when artifact capture is disabled or
    /// the runtime doesn't expose `file_io`. Returns the per-file
    /// references in input order so the caller can fold them into the
    /// terminal `shell_call_output` item's `output_files` array or
    /// surface them as `container_file_citation` annotations.
    pub async fn ingest_user_files(
        &self,
        files: Vec<StagedFile>,
    ) -> RuntimeResult<Vec<ContainerFileRef>> {
        if !self.file_io_enabled() || files.is_empty() {
            return Ok(Vec::new());
        }

        let session = self.session()?;
        let mut out = Vec::with_capacity(files.len());
        let mut state = self.state.lock().await;

        for file in files {
            let path = format!("{MNT_DATA}/{}", file.filename);
            let len = file.bytes.len() as u64;

            // Enforce the session-wide byte budget. We treat user
            // files the same way as assistant-produced ones — they
            // share `bytes_total`. Hitting the cap is logged and the
            // remaining files are skipped, but the staged file is
            // still written so the model can see it; the only thing
            // we drop is the in-memory cached copy used for download.
            let over_budget =
                state.bytes_total.saturating_add(len) > self.config.max_bytes_per_session;
            if over_budget {
                warn!(
                    stage = "ingest_user_file_over_budget",
                    container_id = %self.container_id,
                    filename = %file.filename,
                    bytes = len,
                    bytes_total = state.bytes_total,
                    limit = self.config.max_bytes_per_session,
                    "Staging file would exceed max_bytes_per_session; recording metadata only"
                );
            }

            if let Err(e) = session.write_file(&path, file.bytes.clone()).await {
                error!(
                    stage = "ingest_user_file_write_failed",
                    container_id = %self.container_id,
                    filename = %file.filename,
                    error = %e,
                    "Failed to stage user input file into /mnt/data"
                );
                return Err(e);
            }

            let hash = sha256_bytes(&file.bytes);
            let stored_content = if over_budget {
                Bytes::new()
            } else {
                file.bytes
            };
            let tracked = TrackedFile {
                file_id: new_file_id(),
                path: path.clone(),
                filename: file.filename.clone(),
                bytes: len,
                content_type: file.content_type,
                source: ContainerFileSource::User,
                content: stored_content,
                hash,
            };
            let r = tracked.to_ref(&self.container_id);
            if !over_budget {
                state.bytes_total = state.bytes_total.saturating_add(len);
            }
            state.captured.insert(path.clone(), tracked);
            state.snapshot.insert(path, hash);
            out.push(r);
        }

        state.last_active = Some(Instant::now());

        debug!(
            stage = "ingest_user_files",
            container_id = %self.container_id,
            count = out.len(),
            bytes_total = state.bytes_total,
            "Staged user input files into /mnt/data"
        );

        // Build the persist payload from the freshly-inserted tracked
        // files (still inside the lock so bytes match). Drop the lock
        // before the async DB call.
        let persist_payload: Vec<PersistFileInput> = if self.persistence.is_some() {
            out.iter()
                .filter_map(|r| state.captured.get(&r.path).map(|t| (r, t)))
                .map(|(_, t)| PersistFileInput {
                    file_id: t.file_id.clone(),
                    path: t.path.clone(),
                    filename: t.filename.clone(),
                    content_type: t.content_type.clone(),
                    source: t.source,
                    content: t.content.clone(),
                    content_hash_hex: hex::encode(t.hash),
                    source_response_id: self
                        .persistence
                        .as_ref()
                        .and_then(|p| p.source_response_id.clone()),
                    source_call_id: None,
                })
                .collect()
        } else {
            Vec::new()
        };
        drop(state);

        if !persist_payload.is_empty()
            && let Some(p) = self.persistence.as_ref()
            && let Err(e) = p
                .service
                .persist_files(&self.container_id, p.org_id, persist_payload)
                .await
        {
            warn!(
                stage = "container_persist_failed",
                container_id = %self.container_id,
                error = %e,
                "Failed to persist staged input files"
            );
        }

        Ok(out)
    }

    /// All container files currently tracked, regardless of when they
    /// were first captured. Callers attach an annotation per file to
    /// the assistant's final `output_text`.
    pub async fn list_captured(&self) -> Vec<ContainerFileRef> {
        let state = self.state.lock().await;
        state
            .captured
            .values()
            .map(|f| f.to_ref(&self.container_id))
            .collect()
    }

    /// Re-seed `snapshot` from the current `/mnt/data` state without
    /// emitting any change events. Used at session-start.
    async fn refresh_snapshot(&self) -> RuntimeResult<()> {
        let listing = self.list_mnt_data().await?;
        let mut state = self.state.lock().await;
        state.snapshot = listing;
        Ok(())
    }

    /// List every regular file under `/mnt/data` with its sha256.
    ///
    /// Uses `sha256sum` which is present on the alpine base image
    /// (busybox) and on every glibc Linux distro we ship images for.
    /// The trailing `|| true` keeps the command's exit code at 0 even
    /// when `/mnt/data` is empty (so we don't surface a confusing
    /// `exit_code=1` to callers via the orchestrator's plumbing — this
    /// is an internal command).
    async fn list_mnt_data(&self) -> RuntimeResult<HashMap<String, [u8; 32]>> {
        let command = format!(
            "find {MNT_DATA} -type f -print0 2>/dev/null | xargs -0 -r sha256sum 2>/dev/null || true"
        );
        let output = self.exec_blocking(command, Duration::from_secs(30)).await?;
        Ok(parse_sha256sum_output(&output))
    }

    /// Tear down the underlying VM. Idempotent; safe to call from
    /// `Drop` via a detached task.
    pub async fn terminate(mut self) -> RuntimeResult<()> {
        if let Some(session) = self.session.take() {
            session.terminate().await
        } else {
            Ok(())
        }
    }
}

impl Drop for ContainerSession {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let cid = self.container_id.clone();
            crate::compat::spawn_detached(async move {
                if let Err(e) = session.terminate().await {
                    warn!(
                        stage = "container_drop_terminate_failed",
                        container_id = %cid,
                        error = %e,
                        "Detached terminate after Drop failed"
                    );
                }
            });
        }
    }
}

/// Outcome of a single `exec()` call. Mirrors the runtime's
/// `ExecHandle` for now; future revisions may add per-exec annotation
/// hints or a captured-files preview here.
pub struct ExecOutcome {
    pub handle: crate::runtimes::ExecHandle,
}

fn filename_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Map a small set of common extensions to MIME types. Returns
/// `None` for unknowns rather than guessing — clients are free to
/// sniff if they need finer detail.
fn guess_content_type(filename: &str) -> Option<String> {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    let mime = match ext.as_str() {
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "jsonl" | "ndjson" => "application/x-ndjson",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "py" => "text/x-python",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        "css" => "text/css",
        _ => return None,
    };
    Some(mime.to_string())
}

/// Parse `sha256sum`'s output format: `<64-hex>  <path>` per line.
/// Tolerates the busybox/coreutils binary-flag variants (`*` prefix).
fn parse_sha256sum_output(output: &str) -> HashMap<String, [u8; 32]> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // Hash, two spaces (or space + `*` for binary), then path.
        let Some((hash_str, rest)) = line.split_once(' ') else {
            continue;
        };
        if hash_str.len() != 64 {
            continue;
        }
        let path = rest.trim_start_matches([' ', '*']);
        let mut bytes = [0u8; 32];
        let Ok(()) = hex::decode_to_slice(hash_str, &mut bytes) else {
            continue;
        };
        map.insert(path.to_string(), bytes);
    }
    map
}

/// Convenience: compute sha256 of a byte slice. Used by
/// `ingest_user_files` to seed the snapshot map for staged inputs.
pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_coreutils_sha256sum_lines() {
        let raw = "\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  /mnt/data/empty.txt\n\
abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd  /mnt/data/sub/foo.bin\n";
        let map = parse_sha256sum_output(raw);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("/mnt/data/empty.txt"));
        assert!(map.contains_key("/mnt/data/sub/foo.bin"));
    }

    #[test]
    fn parses_busybox_binary_flag_lines() {
        // busybox sha256sum emits a `*` before the path for binary mode.
        let raw =
            "1111111111111111111111111111111111111111111111111111111111111111 */mnt/data/a.bin\n";
        let map = parse_sha256sum_output(raw);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("/mnt/data/a.bin"));
    }

    #[test]
    fn skips_malformed_lines() {
        let raw = "garbage\n\nshorthash  /mnt/data/x\nffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff  /mnt/data/ok\n";
        let map = parse_sha256sum_output(raw);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("/mnt/data/ok"));
    }

    #[test]
    fn guesses_common_content_types() {
        assert_eq!(guess_content_type("foo.csv"), Some("text/csv".to_string()));
        assert_eq!(guess_content_type("a.PNG"), Some("image/png".to_string()));
        assert_eq!(guess_content_type("README"), None);
        assert_eq!(guess_content_type("a.unknown"), None);
    }

    #[test]
    fn filename_extraction_handles_trailing_slash() {
        assert_eq!(filename_from_path("/mnt/data/sub/foo.txt"), "foo.txt");
        assert_eq!(filename_from_path("/mnt/data/foo.txt"), "foo.txt");
    }

    #[test]
    fn ids_have_expected_prefixes() {
        let c = new_container_id();
        let f = new_file_id();
        assert!(c.starts_with("cntr_"));
        assert!(f.starts_with("cfile_"));
        assert_eq!(c.len(), 5 + 32);
        assert_eq!(f.len(), 6 + 32);
    }

    // ---- capture_changes byte-budget regression coverage ----

    use async_trait::async_trait;
    use futures_util::stream;

    use crate::runtimes::{ExecEvent, ExecHandle, ExecRequest, RuntimeResult, ShellSession};

    /// In-memory `/mnt/data` backing a fake session. `exec` answers the
    /// `find … sha256sum` listing command; `read_file` serves stored
    /// bytes and counts reads per path so a test can assert a file is
    /// not re-read out of the VM on a subsequent capture.
    struct FakeSession {
        files: std::sync::Mutex<HashMap<String, Bytes>>,
        reads: std::sync::Mutex<HashMap<String, usize>>,
    }

    impl FakeSession {
        fn new(files: HashMap<String, Bytes>) -> Self {
            Self {
                files: std::sync::Mutex::new(files),
                reads: std::sync::Mutex::new(HashMap::new()),
            }
        }

        fn read_count(&self, path: &str) -> usize {
            self.reads.lock().unwrap().get(path).copied().unwrap_or(0)
        }
    }

    #[async_trait]
    impl ShellSession for FakeSession {
        async fn exec(&self, cmd: ExecRequest) -> RuntimeResult<ExecHandle> {
            // Only the `find … sha256sum` listing produces file output; any
            // other command returns empty stdout so a future second `exec`
            // call in `capture_changes` can't be silently mis-parsed as a
            // listing.
            if !cmd.command.contains("sha256sum") {
                let events = vec![ExecEvent::Exit {
                    code: 0,
                    signal: None,
                }];
                return Ok(ExecHandle {
                    output: Box::pin(stream::iter(events)),
                });
            }
            // Emulate the `find … sha256sum` listing the session uses to
            // snapshot /mnt/data.
            let mut out = String::new();
            for (path, bytes) in self.files.lock().unwrap().iter() {
                out.push_str(&hex::encode(sha256_bytes(bytes)));
                out.push_str("  ");
                out.push_str(path);
                out.push('\n');
            }
            let events = vec![
                ExecEvent::Stdout(Bytes::from(out)),
                ExecEvent::Exit {
                    code: 0,
                    signal: None,
                },
            ];
            Ok(ExecHandle {
                output: Box::pin(stream::iter(events)),
            })
        }

        async fn read_file(&self, path: &str) -> RuntimeResult<Bytes> {
            *self
                .reads
                .lock()
                .unwrap()
                .entry(path.to_string())
                .or_insert(0) += 1;
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| RuntimeError::Backend(format!("no such file: {path}")))
        }

        async fn terminate(&self) -> RuntimeResult<()> {
            Ok(())
        }
    }

    fn test_session(handle: SessionHandle, config: ContainersConfig) -> ContainerSession {
        ContainerSession {
            container_id: new_container_id(),
            runtime_label: "test",
            created_at: Instant::now(),
            idle_ttl: Duration::from_secs(60),
            file_io: true,
            session: Some(handle),
            config,
            state: Mutex::new(SessionState::default()),
            persistence: None,
        }
    }

    /// A file skipped by the session byte budget must advance the
    /// snapshot so the next `capture_changes` doesn't re-detect it as
    /// changed, re-read its bytes out of the VM, and re-warn. Without the
    /// fix, the second capture re-reads the over-budget file.
    #[tokio::test]
    async fn over_budget_file_advances_snapshot_and_is_not_reread() {
        let big = Bytes::from(vec![7u8; 4096]);
        let path = format!("{MNT_DATA}/big.bin");
        let mut files = HashMap::new();
        files.insert(path.clone(), big);
        let fake = Arc::new(FakeSession::new(files));
        let handle = SessionHandle::new("sess".into(), Box::new(FakeSessionProxy(fake.clone())));

        // Cap below the file size so it's always over budget.
        let config = ContainersConfig {
            max_bytes_per_session: 1024,
            ..ContainersConfig::default()
        };
        let session = test_session(handle, config);

        // First capture: detects the change, reads it once, finds it over
        // budget, skips it (no ref returned) but records the snapshot.
        let refs = session.capture_changes().await.unwrap();
        assert!(refs.is_empty(), "over-budget file must not be captured");
        assert_eq!(fake.read_count(&path), 1, "expected exactly one read");

        // Second capture with the file unchanged: must NOT re-read it.
        let refs = session.capture_changes().await.unwrap();
        assert!(refs.is_empty());
        assert_eq!(
            fake.read_count(&path),
            1,
            "snapshot was not advanced: over-budget file re-read on the next command"
        );

        // Third capture after the file's content changes: the snapshot was
        // advanced to the *old* hash, so the new content must be re-detected
        // and re-read (and re-warned, as it's still over budget). This guards
        // against a regression that advances the snapshot unconditionally.
        fake.files
            .lock()
            .unwrap()
            .insert(path.clone(), Bytes::from(vec![9u8; 4096]));
        let refs = session.capture_changes().await.unwrap();
        assert!(refs.is_empty(), "still over budget after content change");
        assert_eq!(
            fake.read_count(&path),
            2,
            "content change after a budget-skip must trigger a re-read"
        );
    }

    /// Thin wrapper so the test can keep an `Arc` to the fake for
    /// assertions while still handing an owned `Box<dyn ShellSession>` to
    /// `SessionHandle`.
    struct FakeSessionProxy(Arc<FakeSession>);

    #[async_trait]
    impl ShellSession for FakeSessionProxy {
        async fn exec(&self, cmd: ExecRequest) -> RuntimeResult<ExecHandle> {
            self.0.exec(cmd).await
        }
        async fn read_file(&self, path: &str) -> RuntimeResult<Bytes> {
            self.0.read_file(path).await
        }
        async fn write_file(&self, path: &str, bytes: Bytes) -> RuntimeResult<()> {
            self.0.write_file(path, bytes).await
        }
        async fn terminate(&self) -> RuntimeResult<()> {
            self.0.terminate().await
        }
    }
}
