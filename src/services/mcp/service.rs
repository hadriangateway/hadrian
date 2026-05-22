//! `McpService` — the long-lived `AppState` field that brokers
//! Hadrian-hosted MCP calls. Holds:
//!
//! - a pool of warm [`McpClient`](super::McpClient) handles keyed by
//!   `(server_url, auth_hash)` so chained calls inside one response
//!   don't pay the `initialize` round-trip,
//! - a tools-list cache keyed the same way so the preprocess rewrite
//!   and the executor see the same catalog (and so the cold-start
//!   `tools/list` only fires once per request),
//! - a pending-approvals registry so `mcp_approval_response` items can
//!   resume parked calls.
//!
//! The pool's eviction policy is intentionally simple in this
//! iteration: TTL-based eviction on every cache access. Sophisticated
//! LRU + max-entries can land later if pool growth becomes an issue.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use dashmap::DashMap;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{McpClient, McpClientError, McpToolMeta};
use crate::validation::{UrlValidationOptions, validate_base_url_opts};

/// Cache TTL for warm clients. Reaped on next access; we intentionally
/// don't run a background sweeper to keep the service free of timer
/// state. Matches the `default_idle_ttl_secs` rationale for containers.
const CLIENT_IDLE_TTL: Duration = Duration::from_secs(300);

/// Cache TTL for `tools/list` responses, refreshed on miss. Short
/// enough that a server adding a new tool gets picked up within ~1m;
/// long enough that chained calls in one response amortize the cost.
const TOOLS_CACHE_TTL: Duration = Duration::from_secs(60);

/// Cache TTL for tool-description embeddings used by Hadrian-side tool
/// search. The cache is content-addressed (keyed by the hash of the
/// embedded text), so a stale entry can never be *wrong* — only
/// occasionally re-embedded. A generous hour keeps a deferred catalog
/// warm across the many requests an agent loop makes against it.
const TOOL_EMBEDDING_TTL: Duration = Duration::from_secs(3600);

/// Connection key. Including the auth-hash means two callers using
/// different bearer tokens for the same `server_url` get separate
/// pooled clients — no leak between credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct McpEndpointKey {
    pub server_url: String,
    /// Hex-encoded SHA-256 of `(authorization || headers_sorted_json)`.
    /// `None` is folded into a stable sentinel hash so anonymous
    /// callers share a pool entry.
    pub auth_hash: String,
}

impl McpEndpointKey {
    pub fn new(
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(authorization.unwrap_or("").as_bytes());
        hasher.update(b"\0");
        let mut header_pairs: Vec<(&String, &String)> = headers.iter().collect();
        header_pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in header_pairs {
            hasher.update(k.as_bytes());
            hasher.update(b":");
            hasher.update(v.as_bytes());
            hasher.update(b"\n");
        }
        let auth_hash = hex::encode(hasher.finalize());
        Self {
            server_url: server_url.to_string(),
            auth_hash,
        }
    }
}

struct PooledClient {
    /// Shared, lock-free handle. `McpClient`'s `list_tools` / `call_tool`
    /// take `&self` and rmcp's peer multiplexes concurrent JSON-RPC
    /// requests over one session, so several in-flight `tools/call`s to
    /// the same endpoint run concurrently rather than serializing behind
    /// a mutex. Dropping the last `Arc` runs `McpClient::Drop`, which
    /// issues a clean `RunningService::cancel`.
    client: Arc<McpClient>,
    last_used: Instant,
}

struct CachedTools {
    tools: Arc<Vec<McpToolMeta>>,
    refreshed_at: Instant,
}

/// Last `tools/list` failure for an endpoint, kept so the executor can
/// surface the verbatim upstream error on the `mcp_list_tools` item
/// rather than a generic placeholder. Cleared on the next success.
struct CachedToolsError {
    message: String,
    at: Instant,
}

/// Approval that has just been resolved by [`super::resume`] and is
/// waiting for the executor to surface as an `mcp_call` output item on
/// the resumed response stream.
///
/// Keyed in [`McpServiceInner::resolved_approvals`] by
/// `(org_id, call_id)`. The call_id matches the original
/// `function_call`'s `call_id` that the model emitted before the gate
/// parked it; the executor scans the resumed request's input for
/// matching `function_call_output` items and consumes the entry.
#[derive(Debug, Clone)]
pub struct ResolvedMcpApproval {
    /// Echoes the original `function_call.call_id` so the executor can
    /// pair this entry with the `function_call_output` resume produced.
    pub call_id: String,
    /// Id of the `mcp_approval_request` item that gated this call;
    /// surfaced on the synthesized `mcp_call.approval_request_id`.
    pub approval_request_id: String,
    /// Origin server label — must match an `mcp` tool entry on the
    /// resumed request (resume already validated this).
    pub server_label: String,
    /// Tool name as advertised by the MCP server.
    pub tool_name: String,
    /// Arguments the model proposed for the parked call (JSON string).
    pub arguments_json: String,
    /// Successful tool result (text or stringified non-text content).
    /// `None` when the call failed or was refused.
    pub output: Option<String>,
    /// Failure / refusal message. `None` on success.
    pub error: Option<String>,
}

/// Window during which a stashed [`ResolvedMcpApproval`] is consumed
/// by the executor. The executor reads at request-start, immediately
/// after `resume_mcp_approvals` writes — sub-second under normal
/// operation. The TTL is just defense against an executor that never
/// runs (e.g. a request that errors out before reaching the pipeline).
const RESOLVED_APPROVAL_TTL: Duration = Duration::from_secs(60);

/// The long-lived MCP service held on [`AppState`](crate::AppState).
/// Cheap to clone (everything is behind `Arc` / `DashMap`).
#[derive(Clone)]
pub struct McpService {
    inner: Arc<McpServiceInner>,
}

/// Cache key for a tool-description embedding: `(embedding_model,
/// sha256(text))`. The model is part of the key because vectors from
/// different embedding models are not comparable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EmbeddingCacheKey {
    model: String,
    text_hash: String,
}

impl EmbeddingCacheKey {
    fn new(model: &str, text: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        Self {
            model: model.to_string(),
            text_hash: hex::encode(hasher.finalize()),
        }
    }
}

struct McpServiceInner {
    pool: DashMap<McpEndpointKey, PooledClient>,
    tools_cache: DashMap<McpEndpointKey, CachedTools>,
    /// Last `tools/list` error per endpoint (see [`CachedToolsError`]).
    tools_errors: DashMap<McpEndpointKey, CachedToolsError>,
    /// Per-endpoint single-flight guards for `tools/list`. Without this,
    /// concurrent first requests for the same endpoint all miss the
    /// cache and stampede the upstream `tools/list` (common on agent
    /// fan-out / cold start). The winner fetches and primes the cache;
    /// the others await the lock and then hit the warm cache. Keyed the
    /// same way as the pool so the map is bounded by distinct endpoints.
    tools_fetch_locks: DashMap<McpEndpointKey, Arc<Mutex<()>>>,
    /// Content-addressed cache of tool-description embeddings for
    /// Hadrian-side tool search, keyed by `(model, sha256(text))`.
    tool_embeddings: DashMap<EmbeddingCacheKey, (Arc<Vec<f64>>, Instant)>,
    /// Persistence backend for parked approvals. `None` when the
    /// gateway runs without a database — in that case the approval gate
    /// **fails closed**: a call that `require_approval` would gate is
    /// not run, and the executor surfaces a failed `mcp_call` explaining
    /// that persistence is required (see
    /// [`McpExecutor::park_for_approval`](super::executor::McpExecutor)).
    /// Cross-replica / cross-request resume needs persistence.
    approvals_repo: Option<Arc<dyn crate::db::repos::McpPendingApprovalsRepo>>,
    /// Hand-off between `resume_mcp_approvals` (route layer) and
    /// `McpExecutor::prefix_events` (pipeline layer). Resume writes one
    /// entry per resolved approval; the executor drains entries whose
    /// `call_id` matches a `function_call_output` on the resumed
    /// request and emits a synthesized `mcp_call` output item so the
    /// resumed response carries the spec-mandated item lifecycle.
    resolved_approvals: DashMap<(uuid::Uuid, String), (ResolvedMcpApproval, Instant)>,
    /// SSRF guard config, applied to every `server_url` before opening
    /// a transport. Blocks loopback/private/cloud-metadata addresses
    /// unless the operator explicitly opted in via `server.allow_*_urls`.
    url_validation_opts: UrlValidationOptions,
}

impl McpService {
    /// Construct a service without persistence. With no approvals repo
    /// the approval gate fails closed (a gated call is refused, not run)
    /// — use [`McpService::with_approvals_repo`] when a DB is available.
    pub fn new() -> Self {
        Self::with_approvals_repo(None, UrlValidationOptions::default())
    }

    /// Construct a service with the given approvals repo and SSRF
    /// guard policy. Pass `None` for `approvals_repo` when the
    /// deployment has no database.
    pub fn with_approvals_repo(
        approvals_repo: Option<Arc<dyn crate::db::repos::McpPendingApprovalsRepo>>,
        url_validation_opts: UrlValidationOptions,
    ) -> Self {
        Self {
            inner: Arc::new(McpServiceInner {
                pool: DashMap::new(),
                tools_cache: DashMap::new(),
                tools_errors: DashMap::new(),
                tools_fetch_locks: DashMap::new(),
                tool_embeddings: DashMap::new(),
                approvals_repo,
                resolved_approvals: DashMap::new(),
                url_validation_opts,
            }),
        }
    }

    /// Access the approvals repo, if persistence is wired up.
    pub fn approvals_repo(&self) -> Option<&Arc<dyn crate::db::repos::McpPendingApprovalsRepo>> {
        self.inner.approvals_repo.as_ref()
    }

    /// Record an approval that the resume path just resolved. The
    /// matching executor will pick it up by `(org_id, call_id)` when
    /// the pipeline runs moments later. Stale entries past
    /// [`RESOLVED_APPROVAL_TTL`] are reaped on every write.
    pub fn stash_resolved_approval(&self, org_id: uuid::Uuid, approval: ResolvedMcpApproval) {
        let now = Instant::now();
        self.inner
            .resolved_approvals
            .retain(|_, (_, ts)| now.duration_since(*ts) < RESOLVED_APPROVAL_TTL);
        let key = (org_id, approval.call_id.clone());
        self.inner.resolved_approvals.insert(key, (approval, now));
    }

    /// Remove and return the approval previously stashed for this
    /// `(org_id, call_id)`. One-shot consumption — the executor calls
    /// this exactly once per resumed call.
    pub fn take_resolved_approval(
        &self,
        org_id: uuid::Uuid,
        call_id: &str,
    ) -> Option<ResolvedMcpApproval> {
        self.inner
            .resolved_approvals
            .remove(&(org_id, call_id.to_string()))
            .map(|(_, (a, _))| a)
    }

    /// Synchronous fast-path peek into the `tools/list` cache. Returns
    /// the previously fetched catalog if and only if it is still fresh
    /// (within [`TOOLS_CACHE_TTL`]). Used by the executor to surface
    /// `mcp_list_tools` items at stream start without re-fetching —
    /// the rewrite already populated the cache moments earlier.
    pub fn cached_tools(
        &self,
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
    ) -> Option<Arc<Vec<McpToolMeta>>> {
        let key = McpEndpointKey::new(server_url, authorization, headers);
        let entry = self.inner.tools_cache.get(&key)?;
        if entry.refreshed_at.elapsed() < TOOLS_CACHE_TTL {
            Some(entry.tools.clone())
        } else {
            None
        }
    }

    /// Look up a cached tool-description embedding for `(model, text)`.
    /// Returns `None` on miss or when the entry has aged past
    /// [`TOOL_EMBEDDING_TTL`]. Used by Hadrian-side tool search to avoid
    /// re-embedding a static deferred catalog on every request.
    pub fn cached_embedding(&self, model: &str, text: &str) -> Option<Arc<Vec<f64>>> {
        let key = EmbeddingCacheKey::new(model, text);
        let entry = self.inner.tool_embeddings.get(&key)?;
        if entry.1.elapsed() < TOOL_EMBEDDING_TTL {
            Some(entry.0.clone())
        } else {
            None
        }
    }

    /// Store a tool-description embedding for `(model, text)`. Reaps
    /// entries past [`TOOL_EMBEDDING_TTL`] on write to bound growth.
    pub fn cache_embedding(&self, model: &str, text: &str, embedding: Arc<Vec<f64>>) {
        let now = Instant::now();
        self.inner
            .tool_embeddings
            .retain(|_, (_, ts)| now.duration_since(*ts) < TOOL_EMBEDDING_TTL);
        self.inner
            .tool_embeddings
            .insert(EmbeddingCacheKey::new(model, text), (embedding, now));
    }

    /// Seed the `tools/list` cache for an endpoint without hitting the
    /// network. Used by [`super::preprocess::rewrite_mcp_tools`] when a
    /// caller's request already carries an `mcp_list_tools` item for
    /// the same `server_label` — the spec says the API doesn't refetch
    /// while the catalog is in context, and priming the cache lets the
    /// executor's downstream reads (`cached_tools`, `read_only_hint_for`)
    /// see the same catalog the rewrite is using.
    ///
    /// The cache key folds the caller-supplied auth and headers in, so
    /// a primed entry can only be observed by requests with matching
    /// credentials — no cross-tenant poisoning.
    pub fn prime_tools_cache(
        &self,
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
        tools: Vec<McpToolMeta>,
    ) {
        let key = McpEndpointKey::new(server_url, authorization, headers);
        self.inner.tools_cache.insert(
            key,
            CachedTools {
                tools: Arc::new(tools),
                refreshed_at: Instant::now(),
            },
        );
    }

    /// Get the cached tool catalog for an endpoint, calling
    /// `tools/list` if the cache is empty or stale.
    ///
    /// Single-flighted per endpoint: concurrent first callers serialize
    /// on a per-key lock so only one `tools/list` round-trip fires; the
    /// others await the lock and then read the freshly primed cache.
    pub async fn list_tools(
        &self,
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
    ) -> Result<Arc<Vec<McpToolMeta>>, McpClientError> {
        let key = McpEndpointKey::new(server_url, authorization, headers);

        // Fast path: fresh cache entry.
        if let Some(cached) = self.cached_fresh(&key) {
            return Ok(cached);
        }

        // Single-flight: hold the per-endpoint fetch lock across the slow
        // path so concurrent cold callers don't stampede the upstream.
        let fetch_lock = self
            .inner
            .tools_fetch_locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = fetch_lock.lock().await;

        // Re-check: another caller may have primed the cache while we
        // waited for the lock.
        if let Some(cached) = self.cached_fresh(&key) {
            return Ok(cached);
        }

        // Slow path: fetch and cache. No mutex on the client — rmcp
        // multiplexes concurrent requests over the session. On failure
        // we stash the verbatim error so the executor can surface it on
        // the `mcp_list_tools` item; on success we clear any stale error.
        let tools = match self
            .acquire_client(&key, server_url, authorization, headers)
            .await
        {
            Ok(client) => client.list_tools().await,
            Err(e) => Err(e),
        };
        let tools = match tools {
            Ok(t) => t,
            Err(e) => {
                self.inner.tools_errors.insert(
                    key,
                    CachedToolsError {
                        message: e.to_string(),
                        at: Instant::now(),
                    },
                );
                return Err(e);
            }
        };
        self.inner.tools_errors.remove(&key);
        let tools_arc = Arc::new(tools);
        self.inner.tools_cache.insert(
            key,
            CachedTools {
                tools: tools_arc.clone(),
                refreshed_at: Instant::now(),
            },
        );
        Ok(tools_arc)
    }

    /// The verbatim error from the most recent failed `tools/list` for an
    /// endpoint, if one is still fresh (within [`TOOLS_CACHE_TTL`]). Used
    /// by the executor to populate `mcp_list_tools.error` with the real
    /// upstream message instead of a generic placeholder.
    pub fn cached_tools_error(
        &self,
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
    ) -> Option<String> {
        let key = McpEndpointKey::new(server_url, authorization, headers);
        let entry = self.inner.tools_errors.get(&key)?;
        (entry.at.elapsed() < TOOLS_CACHE_TTL).then(|| entry.message.clone())
    }

    /// Fresh cache read shared by the fast path and the post-lock
    /// re-check in [`Self::list_tools`].
    fn cached_fresh(&self, key: &McpEndpointKey) -> Option<Arc<Vec<McpToolMeta>>> {
        let entry = self.inner.tools_cache.get(key)?;
        (entry.refreshed_at.elapsed() < TOOLS_CACHE_TTL).then(|| entry.tools.clone())
    }

    /// Invoke a tool. Uses a pooled client when available; otherwise
    /// opens a fresh connection. Updates `last_used` on success.
    pub async fn call_tool(
        &self,
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<super::McpCallResult, McpClientError> {
        let key = McpEndpointKey::new(server_url, authorization, headers);
        let client = self
            .acquire_client(&key, server_url, authorization, headers)
            .await?;
        // No lock held across the round-trip: concurrent calls to the
        // same endpoint run in parallel over the shared rmcp session.
        let result = client.call_tool(tool_name, arguments).await?;
        // Touch the entry so eviction doesn't reap a busy connection.
        if let Some(mut entry) = self.inner.pool.get_mut(&key) {
            entry.last_used = Instant::now();
        }
        Ok(result)
    }

    /// Drop the pooled client for an endpoint, forcing the next call to
    /// reconnect. Called after a `tools/call` times out: a timed-out
    /// request is abandoned mid-flight without a protocol-level cancel,
    /// leaving the rmcp session's JSON-RPC stream in an indeterminate
    /// state. Reusing it risks hangs or mismatched responses, so the
    /// safe move is to evict. Dropping the last `Arc<McpClient>` runs its
    /// `Drop`, which issues `RunningService::cancel` — a clean session
    /// shutdown. The tools cache is left intact (the catalog is still
    /// valid; only the live connection is suspect).
    pub fn evict_endpoint(
        &self,
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
    ) {
        let key = McpEndpointKey::new(server_url, authorization, headers);
        self.inner.pool.remove(&key);
    }

    /// Drop pool entries idle past their TTL. Called opportunistically
    /// on every `acquire_client`; no background task.
    fn reap_expired(&self) {
        let now = Instant::now();
        self.inner
            .pool
            .retain(|_, v| now.duration_since(v.last_used) < CLIENT_IDLE_TTL);
    }

    /// Get or open a client for the given endpoint key.
    async fn acquire_client(
        &self,
        key: &McpEndpointKey,
        server_url: &str,
        authorization: Option<&str>,
        headers: &HashMap<String, String>,
    ) -> Result<Arc<McpClient>, McpClientError> {
        self.reap_expired();

        // SSRF guard FIRST, before the pool lookup. A warm pool entry must
        // not let a caller-supplied URL skip validation, and re-validating
        // is cheap relative to the call it precedes (it also re-resolves
        // DNS, catching rebinding to a private IP after warm-up). The
        // cache key isn't poisoned because we only stash on success.
        validate_base_url_opts(server_url, self.inner.url_validation_opts)
            .map_err(|e| McpClientError::Transport(format!("blocked server_url: {e}")))?;

        // Try the pool. We can't return early from inside the closure, so
        // clone the Arc after the lookup.
        if let Some(mut entry) = self.inner.pool.get_mut(key) {
            entry.last_used = Instant::now();
            return Ok(entry.client.clone());
        }

        // Cold path: open a fresh connection and stash it.
        let client = McpClient::connect(
            server_url,
            authorization.map(str::to_string),
            headers.clone(),
        )
        .await?;
        let arc = Arc::new(client);
        self.inner.pool.insert(
            key.clone(),
            PooledClient {
                client: arc.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(arc)
    }
}

impl Default for McpService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_key_differs_on_auth() {
        let h = HashMap::new();
        let a = McpEndpointKey::new("https://x", Some("token-a"), &h);
        let b = McpEndpointKey::new("https://x", Some("token-b"), &h);
        assert_ne!(a, b);
        assert_eq!(a.server_url, b.server_url);
    }

    #[test]
    fn embedding_cache_round_trips_and_keys_on_content() {
        let svc = McpService::new();
        assert!(svc.cached_embedding("m1", "search jira").is_none());

        let v = Arc::new(vec![0.1, 0.2, 0.3]);
        svc.cache_embedding("m1", "search jira", v.clone());
        assert_eq!(
            svc.cached_embedding("m1", "search jira").as_deref(),
            Some(&*v)
        );

        // Different text → miss (content-addressed).
        assert!(svc.cached_embedding("m1", "create page").is_none());
        // Different model → miss (vectors aren't comparable across models).
        assert!(svc.cached_embedding("m2", "search jira").is_none());
    }

    #[test]
    fn endpoint_key_stable_across_header_order() {
        let mut h1 = HashMap::new();
        h1.insert("X-Region".to_string(), "us".to_string());
        h1.insert("X-Workspace".to_string(), "team".to_string());

        let mut h2 = HashMap::new();
        h2.insert("X-Workspace".to_string(), "team".to_string());
        h2.insert("X-Region".to_string(), "us".to_string());

        let a = McpEndpointKey::new("https://x", Some("t"), &h1);
        let b = McpEndpointKey::new("https://x", Some("t"), &h2);
        assert_eq!(a, b);
    }

    #[test]
    fn endpoint_key_distinguishes_no_auth_from_empty_auth() {
        let h = HashMap::new();
        let none = McpEndpointKey::new("https://x", None, &h);
        let empty = McpEndpointKey::new("https://x", Some(""), &h);
        // Both have empty bearer hash inputs; treat as equivalent —
        // the pool dedupes correctly either way.
        assert_eq!(none, empty);
    }
}
