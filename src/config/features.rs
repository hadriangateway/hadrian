use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{CircuitBreakerConfig, RetryConfig};

/// Feature flags for optional capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FeaturesConfig {
    /// File search configuration for the Responses API.
    /// Enables server-side file_search tool execution for RAG.
    #[serde(default)]
    pub file_search: Option<FileSearchConfig>,

    /// Guardrails for content filtering, PII detection, and safety.
    /// Supports multiple providers, execution modes, and fine-grained actions.
    #[serde(default)]
    pub guardrails: Option<GuardrailsConfig>,

    /// Response caching.
    #[serde(default)]
    pub response_caching: Option<ResponseCachingConfig>,

    /// HTTP image URL fetching configuration.
    /// Controls how non-OpenAI providers (Anthropic, Bedrock, Vertex) handle
    /// HTTP image URLs in chat completion requests.
    #[serde(default)]
    pub image_fetching: ImageFetchingConfig,

    /// WebSocket configuration for real-time event subscriptions.
    /// Enables clients to subscribe to server events via `/ws/events`.
    #[serde(default)]
    pub websocket: WebSocketConfig,

    /// Vector store cleanup job configuration.
    /// Cleans up soft-deleted vector stores, their chunks, and orphaned files.
    #[serde(default)]
    pub vector_store_cleanup: VectorStoreCleanupConfig,

    /// File processing configuration for RAG document ingestion.
    /// Controls how uploaded files are chunked and embedded into vector stores.
    #[serde(default)]
    pub file_processing: FileProcessingConfig,

    /// Model catalog configuration for enriching API responses with model metadata.
    /// Provides per-model capabilities, pricing, context limits, and modalities
    /// from the models.dev catalog.
    #[serde(default)]
    pub model_catalog: ModelCatalogConfig,

    /// Web search configuration for backend-proxied web search tool.
    /// Requires a search provider API key (Tavily or Exa).
    #[serde(default)]
    pub web_search: Option<WebSearchConfig>,

    /// Web fetch configuration for backend-proxied URL fetching tool.
    /// Validates URLs with SSRF protection and enforces size limits.
    #[serde(default)]
    pub web_fetch: Option<WebFetchConfig>,

    /// Static models cache configuration.
    /// Caches model lists from config-file providers to avoid per-request latency.
    #[serde(default)]
    pub static_models_cache: StaticModelsCacheConfig,

    /// Shared configuration for server-executed tools (file_search, web_search,
    /// future: shell). Controls the global per-request iteration budget across
    /// all such tools. Replaces the per-tool `max_iterations` fields on
    /// `[features.file_search]` and `[features.web_search]`, which are
    /// deprecated and emit a warning at startup if set to a non-default value.
    #[serde(default)]
    pub server_tools: ServerToolsConfig,

    /// Shell tool runtime configuration. Selects which backend executes
    /// `shell` tool calls (passthrough_openai, microsandbox, etc.).
    /// Defaults to `None` — shell tool disabled.
    #[serde(default)]
    pub shell: super::ShellRuntimeConfig,

    /// Container / `/mnt/data` artifact capture settings. Controls how
    /// files written by the shell tool are persisted and surfaced back
    /// to the conversation as `container_file_citation` annotations.
    #[serde(default)]
    pub containers: ContainersConfig,

    /// Persistence settings for the Responses API.
    #[serde(default)]
    pub responses: ResponsesPersistenceConfig,
}

/// Persistence and retention settings for the Responses API.
///
/// When `store=true` (the OpenAI default) is set on a request, Hadrian
/// writes a row to the `responses` table that can later be retrieved
/// via `GET /v1/responses/{id}` or cancelled via
/// `POST /v1/responses/{id}/cancel`. Records past
/// `retention_secs` are removed by the cleanup worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResponsesPersistenceConfig {
    /// How long a terminal response is kept before pruning.
    /// Default 86400 (24h). Must be > 0.
    #[serde(default = "default_responses_retention_secs")]
    pub retention_secs: u64,
    /// Interval at which the retention worker scans for expired
    /// records. Default 3600 (1h). Must be > 0.
    #[serde(default = "default_responses_cleanup_interval_secs")]
    pub cleanup_interval_secs: u64,
    /// Max concurrent in-flight background-mode responses **per
    /// replica**. Each `background=true` request claims a row and runs
    /// the LLM stream in its own task; without a cap a burst can pin
    /// the whole replica. Default 8.
    #[serde(default = "default_responses_worker_concurrency")]
    pub worker_concurrency: usize,
    /// Maximum wall-clock time a response is allowed to remain in
    /// `status='in_progress'`. The retention worker reaps rows that
    /// exceed this (mark Failed with `code="worker_lost"`), covering
    /// the case where a worker crashed mid-execution. Should be
    /// generously larger than the longest expected execution — a
    /// real workload should finish well within this. Default 3600
    /// (1h). Must be > 0.
    #[serde(default = "default_responses_max_in_progress_secs")]
    pub max_in_progress_secs: u64,
    /// Retry policy for background-mode responses. Foreground
    /// streaming requests don't use this — the client owns the retry.
    #[serde(default)]
    pub retry: ResponsesRetryConfig,
    /// Optional webhook fired on terminal-state transitions
    /// (`completed`, `failed`, `cancelled`, `incomplete`). Disabled
    /// when unset.
    #[serde(default)]
    pub webhook: Option<ResponsesWebhookConfig>,

    /// Gateway-side context compaction for non-OpenAI providers (the
    /// canonical OpenAI compaction directive is otherwise forwarded
    /// verbatim). Disabled by default.
    #[serde(default)]
    pub compaction: ResponsesCompactionConfig,
}

/// Operator-side defaults for the gateway compactor. When a request
/// includes `context_management = [{type: "compaction", ...}]` and the
/// upstream provider does not natively support server-side compaction
/// (i.e. anything other than OpenAI / Azure OpenAI), Hadrian runs the
/// compactor before dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResponsesCompactionConfig {
    /// Master switch. When false, gateway-side compaction never runs
    /// regardless of request directives; non-OpenAI providers still
    /// see the unedited payload (matching pre-compactor behaviour).
    #[serde(default = "default_compaction_enabled")]
    pub enabled: bool,
    /// Strategy used when the request didn't specify one. Picks
    /// `truncate` so the default behaviour is deterministic + free.
    #[serde(default = "default_compaction_default_strategy")]
    pub default_strategy: ResponsesCompactionStrategy,
    /// Fallback `compact_threshold` (in tokens) when the request
    /// didn't specify one. The default 12_000 leaves headroom under
    /// the smaller non-OpenAI context windows (Bedrock claude-haiku
    /// ~200k, but typical app prompts stay well below the limit).
    #[serde(default = "default_compaction_threshold")]
    pub default_threshold_tokens: u32,
    /// Number of most-recent items the compactor must keep intact.
    /// Older items are dropped (truncate) or replaced by a summary
    /// (llm). Default 6.
    #[serde(default = "default_compaction_keep_recent")]
    pub keep_recent_items: usize,
    /// Default prompt the `llm` strategy uses to summarise dropped
    /// items. Overridden per-request via
    /// `context_management.compaction.prompt`. Includes a placeholder
    /// describing what the summary will be inserted as.
    #[serde(default = "default_compaction_prompt")]
    pub default_prompt: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ResponsesCompactionStrategy {
    /// Summarise dropped items via the active provider.
    Llm,
    /// Drop oldest non-system items until the rolling token estimate
    /// falls under the threshold.
    #[default]
    Truncate,
}

impl Default for ResponsesCompactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_compaction_enabled(),
            default_strategy: default_compaction_default_strategy(),
            default_threshold_tokens: default_compaction_threshold(),
            keep_recent_items: default_compaction_keep_recent(),
            default_prompt: default_compaction_prompt(),
        }
    }
}

fn default_compaction_enabled() -> bool {
    false
}

fn default_compaction_default_strategy() -> ResponsesCompactionStrategy {
    ResponsesCompactionStrategy::Truncate
}

fn default_compaction_threshold() -> u32 {
    12_000
}

fn default_compaction_keep_recent() -> usize {
    6
}

fn default_compaction_prompt() -> String {
    "Summarize the prior conversation in <= 250 words. Preserve concrete decisions, \
     user-stated constraints, file paths and IDs, and unresolved questions. Drop \
     redundant pleasantries. Output a single paragraph; the result will be inserted \
     as system context for the model to consult in the remaining turns."
        .to_string()
}

impl Default for ResponsesPersistenceConfig {
    fn default() -> Self {
        Self {
            retention_secs: default_responses_retention_secs(),
            cleanup_interval_secs: default_responses_cleanup_interval_secs(),
            worker_concurrency: default_responses_worker_concurrency(),
            max_in_progress_secs: default_responses_max_in_progress_secs(),
            retry: ResponsesRetryConfig::default(),
            webhook: None,
            compaction: ResponsesCompactionConfig::default(),
        }
    }
}

impl ResponsesPersistenceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.retention_secs == 0 {
            return Err("[features.responses] retention_secs must be > 0".into());
        }
        if self.cleanup_interval_secs == 0 {
            return Err("[features.responses] cleanup_interval_secs must be > 0".into());
        }
        if self.max_in_progress_secs == 0 {
            return Err("[features.responses] max_in_progress_secs must be > 0".into());
        }
        self.retry.validate()?;
        Ok(())
    }
}

fn default_responses_worker_concurrency() -> usize {
    8
}

fn default_responses_max_in_progress_secs() -> u64 {
    3_600
}

/// Retry policy for background-mode responses.
///
/// Foreground `/v1/responses` requests are streamed back to the
/// client — retries are the client's responsibility. Background
/// requests have no client connected during execution, so the
/// worker handles transient failures (provider 5xx, network blips,
/// transient DB errors) by re-running the row with exponential
/// backoff until either the call succeeds or `max_attempts` is hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResponsesRetryConfig {
    /// Master switch. Default `true`.
    #[serde(default = "default_responses_retry_enabled")]
    pub enabled: bool,
    /// Maximum number of execution attempts, including the first.
    /// `1` means no retry. Default 3.
    #[serde(default = "default_responses_retry_max_attempts")]
    pub max_attempts: u32,
    /// Initial backoff before the second attempt, in milliseconds.
    /// Default 500ms.
    #[serde(default = "default_responses_retry_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    /// Backoff multiplier between attempts. Default 2.0.
    #[serde(default = "default_responses_retry_multiplier")]
    pub multiplier: f64,
    /// Cap on a single backoff interval, in milliseconds. Default
    /// 30000 (30s).
    #[serde(default = "default_responses_retry_max_backoff_ms")]
    pub max_backoff_ms: u64,
}

impl Default for ResponsesRetryConfig {
    fn default() -> Self {
        Self {
            enabled: default_responses_retry_enabled(),
            max_attempts: default_responses_retry_max_attempts(),
            initial_backoff_ms: default_responses_retry_initial_backoff_ms(),
            multiplier: default_responses_retry_multiplier(),
            max_backoff_ms: default_responses_retry_max_backoff_ms(),
        }
    }
}

impl ResponsesRetryConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_attempts == 0 {
            return Err("[features.responses.retry] max_attempts must be >= 1".into());
        }
        if self.multiplier < 1.0 {
            return Err("[features.responses.retry] multiplier must be >= 1.0".into());
        }
        if self.initial_backoff_ms == 0 {
            return Err("[features.responses.retry] initial_backoff_ms must be > 0".into());
        }
        if self.max_backoff_ms < self.initial_backoff_ms {
            return Err(
                "[features.responses.retry] max_backoff_ms must be >= initial_backoff_ms".into(),
            );
        }
        Ok(())
    }

    /// Compute the next backoff interval for `attempt` (1-indexed
    /// after the first try). Caps at `max_backoff_ms`.
    pub fn backoff_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let exp = attempt.saturating_sub(1).min(31) as i32;
        let ms = (self.initial_backoff_ms as f64) * self.multiplier.powi(exp);
        let clamped = ms.min(self.max_backoff_ms as f64).max(0.0) as u64;
        std::time::Duration::from_millis(clamped)
    }
}

fn default_responses_retry_enabled() -> bool {
    true
}

fn default_responses_retry_max_attempts() -> u32 {
    3
}

fn default_responses_retry_initial_backoff_ms() -> u64 {
    500
}

fn default_responses_retry_multiplier() -> f64 {
    2.0
}

fn default_responses_retry_max_backoff_ms() -> u64 {
    30_000
}

/// Webhook delivery settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResponsesWebhookConfig {
    /// Target URL. Validated for SSRF at config-load time via
    /// `validate_base_url` (no private/loopback/metadata addresses by
    /// default).
    pub url: String,
    /// Optional bearer token sent in the `Authorization` header. As
    /// with other secret-bearing config fields, treat this value as
    /// sensitive: don't log it and prefer routing through the secrets
    /// manager URI scheme rather than embedding the literal token.
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// Optional HMAC signing secret. When set, every delivery carries
    /// an `X-Hadrian-Signature: t=<unix>,v1=<hex>` header where the
    /// hex digest is `HMAC-SHA256(secret, "<unix>.<body>")`. Receivers
    /// reject requests whose timestamp is too old (defends against
    /// replay) and recompute the digest to verify the body.
    #[serde(default)]
    pub signing_secret: Option<String>,
    /// Per-request timeout in seconds. Default 10s.
    #[serde(default = "default_webhook_timeout_secs")]
    pub timeout_secs: u64,
    /// Maximum concurrent in-flight deliveries. A slow target won't
    /// hold up new deliveries beyond this cap; additional events
    /// queue in the bounded retry channel. Default 32.
    #[serde(default = "default_webhook_max_concurrent")]
    pub max_concurrent_deliveries: usize,
    /// Bounded retry queue capacity. When full, new events are
    /// dropped (with a `webhook_dropped_total` counter increment) so
    /// the gateway doesn't grow memory unboundedly when the target
    /// is wedged. Default 1000.
    #[serde(default = "default_webhook_retry_capacity")]
    pub retry_queue_capacity: usize,
}

impl ResponsesWebhookConfig {
    /// Validate the webhook URL against Hadrian's SSRF rules. Called
    /// from `FeaturesConfig::validate()` at startup so misconfigured
    /// hooks fail loudly instead of silently sending traffic to
    /// internal endpoints.
    pub fn validate(&self, allow_loopback: bool) -> Result<(), String> {
        crate::validation::validate_base_url(&self.url, allow_loopback).map_err(|e| {
            format!(
                "[features.responses.webhook] url failed SSRF validation: {}",
                e
            )
        })
    }
}

fn default_webhook_timeout_secs() -> u64 {
    10
}

fn default_webhook_max_concurrent() -> usize {
    32
}

fn default_webhook_retry_capacity() -> usize {
    1000
}

fn default_responses_retention_secs() -> u64 {
    86_400
}

fn default_responses_cleanup_interval_secs() -> u64 {
    3_600
}

impl FeaturesConfig {
    /// Validate all feature configurations.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(ref file_search) = self.file_search {
            file_search.validate()?;
        }
        // Surface deprecation warnings for per-tool iteration limits.
        if let Some(ref file_search) = self.file_search
            && file_search.max_iterations != default_file_search_max_iterations()
        {
            tracing::warn!(
                "[features.file_search] max_iterations is deprecated; \
                 use [features.server_tools] max_iterations instead. \
                 The global value ({}) will be used.",
                self.server_tools.max_iterations
            );
        }
        if let Some(ref web_search) = self.web_search
            && web_search.max_iterations != default_web_search_max_iterations()
        {
            tracing::warn!(
                "[features.web_search] max_iterations is deprecated; \
                 use [features.server_tools] max_iterations instead. \
                 The global value ({}) will be used.",
                self.server_tools.max_iterations
            );
        }
        self.responses.validate()?;
        Ok(())
    }
}

/// Configuration shared by all server-executed tools.
///
/// Server-executed tools (`file_search`, `web_search`, etc.) run inside the
/// gateway in a multi-turn loop. This config gates the total number of
/// iterations a single request can drive, across **all** tools — preventing
/// runaway sessions where the model keeps requesting new tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ServerToolsConfig {
    /// Maximum number of provider continuation requests in one response.
    ///
    /// Counts all server-executed tool iterations together (file_search +
    /// web_search + shell + …). On the final iteration the tools strip
    /// their own definitions from the continuation payload so the model
    /// is forced to produce a text response.
    ///
    /// Default: 10.
    #[serde(default = "default_server_tools_max_iterations")]
    pub max_iterations: usize,

    /// Pricing for runtime time consumed by the shell tool.
    ///
    /// Local runtimes (microsandbox) are billed by wall-clock seconds.
    /// Passthrough mode (where the upstream provider runs the container)
    /// is billed by that provider and remains 0 here.
    #[serde(default)]
    pub pricing: ServerToolsPricingConfig,

    /// Shell-tool execution limits.
    #[serde(default)]
    pub shell_limits: ShellLimitsConfig,
}

impl Default for ServerToolsConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_server_tools_max_iterations(),
            pricing: ServerToolsPricingConfig::default(),
            shell_limits: ShellLimitsConfig::default(),
        }
    }
}

fn default_server_tools_max_iterations() -> usize {
    10
}

/// Limits enforced on every shell-tool invocation. Sets soft ceilings
/// on wall-clock time and resource use so a runaway model can't pin
/// VM resources indefinitely, and the upper bound for what a
/// per-request `environment` block may ask for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ShellLimitsConfig {
    /// Per-command timeout in seconds. The runtime aborts the exec
    /// once this elapses; the persister records the partial output.
    /// Default 300 (5 min).
    #[serde(default = "default_shell_command_timeout_secs")]
    pub command_timeout_secs: u64,
    /// Default vCPU limit applied when the SessionSpec doesn't
    /// specify one. The runtime backend's own default applies when
    /// this is `None`.
    #[serde(default)]
    pub default_cpu_limit: Option<f64>,
    /// Default memory limit (MB) applied when the request didn't
    /// supply a `container_auto.memory_limit`. The runtime backend's
    /// own default applies when this is `None`.
    #[serde(default)]
    pub default_mem_limit_mb: Option<u32>,
    /// Hard ceiling for the per-request `container_auto.memory_limit`
    /// override. Requests asking for more than this are rejected with
    /// `400`. `None` (the default) means requests can ask for any
    /// value the backend supports.
    #[serde(default)]
    pub max_mem_limit_mb: Option<u32>,
    /// Hostnames (or `*.suffix` patterns) requests may put in
    /// `network_policy.domains`. Empty (the default) means requests
    /// cannot widen egress beyond whatever the runtime allows by
    /// default; use `["*"]` to permit any host.
    #[serde(default)]
    pub allowed_egress_hosts: Vec<String>,
    /// Operator-pre-configured secrets the request may reference by
    /// `placeholder` in `domain_secrets`. The raw value never leaves
    /// the gateway — the model sees only the placeholder, and the
    /// runtime substitutes the value at egress to the permitted hosts.
    #[serde(default)]
    pub allowed_domain_secrets: HashMap<String, AllowedDomainSecret>,
    /// Maximum number of characters of stdout/stderr fed back to the
    /// model per shell call. Output longer than this is head + tail
    /// trimmed with a `... N chars truncated ...` marker so the model
    /// still sees both ends. The full stream remains in the response
    /// event log; operators can raise this for token-rich models or
    /// lower it to bound context spend. Default 8000.
    #[serde(default = "default_shell_max_output_chars")]
    pub max_output_chars: usize,
}

impl Default for ShellLimitsConfig {
    fn default() -> Self {
        Self {
            command_timeout_secs: default_shell_command_timeout_secs(),
            default_cpu_limit: None,
            default_mem_limit_mb: None,
            max_mem_limit_mb: None,
            allowed_egress_hosts: Vec::new(),
            allowed_domain_secrets: HashMap::new(),
            max_output_chars: default_shell_max_output_chars(),
        }
    }
}

fn default_shell_max_output_chars() -> usize {
    8_000
}

/// One operator-pinned secret the request may reference via
/// `ShellDomainSecretRef`. The literal value can be a plaintext token
/// or a secrets-manager URI; resolution is the runtime's
/// responsibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AllowedDomainSecret {
    /// Literal value or `secret://…` reference. Never logged. Never
    /// returned in API responses.
    pub value: String,
    /// Hostnames the runtime is permitted to send this secret to. A
    /// request's `allowed_domains` must be a subset; empty in the
    /// request inherits this full list.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

fn default_shell_command_timeout_secs() -> u64 {
    300
}

/// Settings for `/mnt/data` artifact capture from the shell tool.
///
/// When the configured shell runtime supports `file_io`, Hadrian
/// snapshots the container's `/mnt/data` directory before and after
/// every shell command. New or changed files are surfaced as
/// `container_file_citation` annotations on the assistant's reply, with
/// a stable `cfile_<uuid>` identifier that Phase 3's container files
/// API will resolve to downloadable bytes.
///
/// Setting `enabled = false` reverts to the legacy "tear down VM after
/// every command" behaviour with no artifact capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ContainersConfig {
    /// Master switch. When false, the shell tool boots and tears down a
    /// fresh microVM for every command and never captures artifacts —
    /// matches Hadrian behaviour prior to Phase 1.
    #[serde(default = "default_containers_enabled")]
    pub enabled: bool,

    /// Idle time after which an in-memory container session is torn
    /// down. Phase 1 sessions are response-scoped, so this only kicks
    /// in if a response stalls without terminating. Default 1200 (20m,
    /// matching OpenAI's hosted-container idle TTL).
    #[serde(default = "default_containers_idle_ttl_secs")]
    pub default_idle_ttl_secs: u64,

    /// Hard cap on `expires_after.minutes` per request and on
    /// `POST /v1/containers` creation. Requests above this cap reject
    /// with 400. Default 86_400 (60 days), matching OpenAI's
    /// hosted-container maximum.
    #[serde(default = "default_containers_max_idle_ttl_secs")]
    pub max_idle_ttl_secs: u64,

    /// Hard cap on the number of new/changed files captured per shell
    /// exec. Excess files are dropped with a warning. Default 64.
    #[serde(default = "default_containers_max_files_per_exec")]
    pub max_files_per_exec: usize,

    /// Hard cap on the size of any single captured file. Files larger
    /// than this are recorded as metadata only (bytes + filename) with
    /// no content stored. Default 25 MiB.
    #[serde(default = "default_containers_max_bytes_per_file")]
    pub max_bytes_per_file: u64,

    /// Hard cap on the total bytes captured across all files in one
    /// container session. Default 250 MiB.
    #[serde(default = "default_containers_max_bytes_per_session")]
    pub max_bytes_per_session: u64,

    /// Hard cap on the number of `input_file` parts Hadrian will
    /// materialize into `/mnt/data` for one request. Excess parts
    /// cause the request to fail with a 400. Default 32.
    #[serde(default = "default_containers_max_input_files_per_request")]
    pub max_input_files_per_request: usize,

    /// Hard cap on the total bytes across all staged input files in
    /// one request. Default 100 MiB.
    #[serde(default = "default_containers_max_input_bytes_per_request")]
    pub max_input_bytes_per_request: u64,
}

impl Default for ContainersConfig {
    fn default() -> Self {
        Self {
            enabled: default_containers_enabled(),
            default_idle_ttl_secs: default_containers_idle_ttl_secs(),
            max_idle_ttl_secs: default_containers_max_idle_ttl_secs(),
            max_files_per_exec: default_containers_max_files_per_exec(),
            max_bytes_per_file: default_containers_max_bytes_per_file(),
            max_bytes_per_session: default_containers_max_bytes_per_session(),
            max_input_files_per_request: default_containers_max_input_files_per_request(),
            max_input_bytes_per_request: default_containers_max_input_bytes_per_request(),
        }
    }
}

fn default_containers_enabled() -> bool {
    true
}

fn default_containers_idle_ttl_secs() -> u64 {
    1200
}

fn default_containers_max_idle_ttl_secs() -> u64 {
    // 60 days, matching OpenAI's published hosted-container cap.
    60 * 24 * 60 * 60
}

fn default_containers_max_files_per_exec() -> usize {
    64
}

fn default_containers_max_bytes_per_file() -> u64 {
    25 * 1024 * 1024
}

fn default_containers_max_bytes_per_session() -> u64 {
    250 * 1024 * 1024
}

fn default_containers_max_input_files_per_request() -> usize {
    32
}

fn default_containers_max_input_bytes_per_request() -> u64 {
    100 * 1024 * 1024
}

/// Cost rates for billable server-tool runtimes.
///
/// Rates are in **microcents per second** (1/1,000,000 of a dollar per
/// second, matching the precision used elsewhere in `pricing::PricingConfig`).
/// Total cost per shell call is `runtime_seconds * rate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ServerToolsPricingConfig {
    /// Microcents per second of microsandbox VM wall-clock time.
    /// Default 0 (no charge until operator sets a rate).
    #[serde(default)]
    pub microsandbox_microcents_per_second: u64,
}

/// Embedding configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// Provider to use for embeddings.
    #[serde(default = "default_embedding_provider")]
    pub provider: String,

    /// Model to use for embeddings.
    #[serde(default = "default_embedding_model")]
    pub model: String,

    /// Embedding dimensions.
    #[serde(default = "default_embedding_dimensions")]
    pub dimensions: usize,
}

fn default_embedding_provider() -> String {
    "openai".to_string()
}

fn default_embedding_model() -> String {
    "text-embedding-3-small".to_string()
}

fn default_embedding_dimensions() -> usize {
    1536
}

// ─────────────────────────────────────────────────────────────────────────────
// File Search (Responses API RAG)
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the file_search tool in the Responses API.
///
/// When enabled, the gateway intercepts `file_search` tool calls from the LLM
/// and executes them against the local vector store, injecting results back
/// into the conversation without exposing the search process to the client.
///
/// # Example Configuration
///
/// ```toml
/// [features.file_search]
/// enabled = true
/// max_iterations = 5
/// max_results_per_search = 10
/// timeout_secs = 30
/// include_annotations = true
/// score_threshold = 0.7
///
/// # Optional: Configure vector backend independently from semantic caching
/// [features.file_search.vector_backend]
/// type = "pgvector"
/// table_name = "rag_chunks"  # Separate from semantic cache
///
/// # Optional: Configure embeddings (falls back to semantic caching config)
/// [features.file_search.embedding]
/// provider = "openai"
/// model = "text-embedding-3-small"
/// dimensions = 1536
///
/// # Optional: Configure retries for transient failures
/// [features.file_search.retry]
/// enabled = true
/// max_retries = 3
/// initial_delay_ms = 100
/// max_delay_ms = 10000
/// backoff_multiplier = 2.0
/// jitter = 0.1
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileSearchConfig {
    /// Enable file_search tool interception.
    /// When disabled, file_search tools are passed through to the provider
    /// (which may not support them).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum number of tool call iterations before forcing completion.
    /// Prevents infinite loops where the model keeps calling file_search.
    #[serde(default = "default_file_search_max_iterations")]
    pub max_iterations: usize,

    /// Maximum number of search results to return per search call.
    #[serde(default = "default_file_search_max_results")]
    pub max_results_per_search: usize,

    /// Timeout in seconds for each search operation.
    #[serde(default = "default_file_search_timeout_secs")]
    pub timeout_secs: u64,

    /// Include file citation annotations in the response.
    /// When true, responses include metadata about which files were referenced.
    #[serde(default = "default_true")]
    pub include_annotations: bool,

    /// Minimum similarity score threshold for search results (0.0-1.0).
    /// Results below this threshold are excluded.
    #[serde(default = "default_file_search_threshold")]
    pub score_threshold: f64,

    /// Vector database backend configuration for RAG chunk storage.
    ///
    /// When not specified, falls back to:
    /// 1. Semantic caching vector backend (if configured)
    /// 2. Default pgvector with table name "rag_chunks"
    ///
    /// Configuring this separately from semantic caching ensures RAG data
    /// is stored in dedicated tables/collections, avoiding confusion with
    /// semantic cache data.
    #[serde(default)]
    pub vector_backend: Option<RagVectorBackend>,

    /// Embedding configuration for RAG.
    ///
    /// When not specified, falls back to:
    /// 1. Semantic caching embedding config (if configured)
    /// 2. Vector search embedding config (if configured)
    ///
    /// Must be configured if neither semantic caching nor vector search is enabled.
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,

    /// Retry configuration for RAG operations.
    ///
    /// Applies to:
    /// - Embedding API calls (transient 429/5xx errors)
    /// - Vector database writes (network issues, DB overload)
    /// - Vector database searches (connection errors)
    ///
    /// Uses exponential backoff with configurable jitter.
    /// Default: enabled with 3 retries, 100ms initial delay, 2x backoff.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for vector store operations.
    ///
    /// Protects against unhealthy vector store backends by failing fast
    /// after repeated failures. When the circuit is open, requests fail
    /// immediately without attempting the operation.
    ///
    /// Default: enabled with 5 failures in 60s to open, 30s recovery.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Maximum total characters for search results injected into continuation payload.
    ///
    /// Prevents context window overflow when search results are large.
    /// Results are truncated to fit within this limit, preserving complete
    /// result entries where possible (partial results are excluded).
    ///
    /// Default: 50000 characters (~50 chunks of ~1000 chars each)
    #[serde(default = "default_file_search_max_result_chars")]
    pub max_search_result_chars: usize,

    /// LLM-based re-ranking configuration.
    ///
    /// Re-ranking uses a language model to re-score search results based on
    /// semantic relevance to the query. Enable this to use `ranker: "llm"` in
    /// API requests.
    ///
    /// Default: disabled
    #[serde(default)]
    pub rerank: RerankConfig,
}

impl Default for FileSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_iterations: default_file_search_max_iterations(),
            max_results_per_search: default_file_search_max_results(),
            timeout_secs: default_file_search_timeout_secs(),
            include_annotations: true,
            score_threshold: default_file_search_threshold(),
            vector_backend: None,
            embedding: None,
            retry: RetryConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            max_search_result_chars: default_file_search_max_result_chars(),
            rerank: RerankConfig::default(),
        }
    }
}

impl FileSearchConfig {
    /// Validate the file search configuration.
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.score_threshold) {
            return Err(format!(
                "score_threshold must be between 0.0 and 1.0, got {}",
                self.score_threshold
            ));
        }
        self.rerank.validate()?;
        Ok(())
    }
}

/// Vector database backend for RAG chunk storage.
///
/// This is separate from `SemanticVectorBackend` to allow independent configuration
/// of RAG storage vs. semantic caching storage. Using separate tables/collections
/// for each purpose improves clarity and allows different index configurations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum RagVectorBackend {
    /// PostgreSQL with pgvector extension.
    /// Uses the existing database connection pool.
    Pgvector {
        /// Table name for storing RAG document chunks.
        /// Note: A second table "{table_name}_chunks" will NOT be created;
        /// this table name IS the chunks table.
        #[serde(default = "default_rag_table_name")]
        table_name: String,

        /// Index type for vector similarity search.
        #[serde(default)]
        index_type: PgvectorIndexType,

        /// Distance metric for similarity search.
        /// Defaults to cosine, which works best for text embeddings.
        #[serde(default = "default_distance_metric")]
        distance_metric: DistanceMetric,
    },

    /// Qdrant vector database.
    Qdrant {
        /// Qdrant server URL.
        url: String,

        /// API key for authentication (optional).
        #[serde(default)]
        api_key: Option<String>,

        /// VectorStore name for storing RAG document chunks.
        #[serde(default = "default_rag_vector_store_name")]
        qdrant_collection_name: String,

        /// Distance metric for similarity search.
        /// Defaults to cosine, which works best for text embeddings.
        #[serde(default = "default_distance_metric")]
        distance_metric: DistanceMetric,
    },
}

fn default_rag_table_name() -> String {
    "rag_chunks".to_string()
}

fn default_rag_vector_store_name() -> String {
    "rag_chunks".to_string()
}

fn default_file_search_max_iterations() -> usize {
    5
}

fn default_file_search_max_results() -> usize {
    10
}

fn default_file_search_timeout_secs() -> u64 {
    30
}

fn default_file_search_threshold() -> f64 {
    0.7
}

fn default_file_search_max_result_chars() -> usize {
    50_000
}

// ─────────────────────────────────────────────────────────────────────────────
// Re-ranking
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for LLM-based re-ranking of search results.
///
/// Re-ranking is a second-stage retrieval technique that takes initial search results
/// (from vector or hybrid search) and re-scores them using a language model based on
/// semantic relevance to the query. This typically improves precision at the cost of
/// additional latency and API calls.
///
/// # When to Use Re-ranking
///
/// - **High-precision requirements**: When result quality matters more than speed
/// - **Complex queries**: When semantic understanding beyond vector similarity is needed
/// - **Small result sets**: Re-ranking 10-20 results is fast; 100+ becomes slow
///
/// # Example Configuration
///
/// ```toml
/// [features.file_search.rerank]
/// enabled = true
/// model = "gpt-4o-mini"          # Optional: uses default model if not set
/// max_results_to_rerank = 20     # Re-rank top 20 from initial search
/// batch_size = 10                # Process 10 results per LLM call
/// timeout_secs = 30              # Timeout for re-ranking operation
/// ```
///
/// # API Usage
///
/// Enable re-ranking per-request using the `ranker` field:
///
/// ```json
/// {
///   "ranking_options": {
///     "ranker": "llm",
///     "score_threshold": 0.5
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RerankConfig {
    /// Enable LLM-based re-ranking.
    ///
    /// When false, requests with `ranker: "llm"` will fall back to vector search.
    /// Default: false
    #[serde(default)]
    pub enabled: bool,

    /// LLM model to use for re-ranking.
    ///
    /// If not specified, uses the gateway's default model.
    /// Recommended: A fast, capable model like `gpt-4o-mini` or `claude-3-haiku`.
    #[serde(default)]
    pub model: Option<String>,

    /// Maximum number of results to pass to the re-ranker.
    ///
    /// The re-ranker receives the top N results from the initial search.
    /// Higher values may improve recall but increase latency and cost.
    /// Default: 20
    #[serde(default = "default_rerank_max_results")]
    pub max_results_to_rerank: usize,

    /// Number of results to process per LLM call.
    ///
    /// Results are processed in batches to balance latency and context usage.
    /// Smaller batches have lower per-call latency but more total calls.
    /// Default: 10
    #[serde(default = "default_rerank_batch_size")]
    pub batch_size: usize,

    /// Timeout in seconds for the entire re-ranking operation.
    ///
    /// If re-ranking exceeds this timeout, returns the original search results
    /// without re-ranking (graceful degradation).
    /// Default: 30
    #[serde(default = "default_rerank_timeout_secs")]
    pub timeout_secs: u64,

    /// Whether to fall back to original vector scores when re-ranking fails.
    ///
    /// When true (default), if the LLM re-ranking call fails (network error,
    /// rate limit, parse error, etc.), the search returns the original vector
    /// search results instead of failing the entire request.
    ///
    /// When false, re-ranking failures propagate as errors, allowing callers
    /// to handle them explicitly.
    /// Default: true
    #[serde(default = "default_fallback_on_error")]
    pub fallback_on_error: bool,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: None,
            max_results_to_rerank: default_rerank_max_results(),
            batch_size: default_rerank_batch_size(),
            timeout_secs: default_rerank_timeout_secs(),
            fallback_on_error: default_fallback_on_error(),
        }
    }
}

impl RerankConfig {
    /// Validate the re-rank configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_results_to_rerank == 0 {
            return Err("max_results_to_rerank must be greater than 0".to_string());
        }
        if self.batch_size == 0 {
            return Err("batch_size must be greater than 0".to_string());
        }
        if self.batch_size > self.max_results_to_rerank {
            return Err(format!(
                "batch_size ({}) should not exceed max_results_to_rerank ({})",
                self.batch_size, self.max_results_to_rerank
            ));
        }
        if self.timeout_secs == 0 {
            return Err("timeout_secs must be greater than 0".to_string());
        }
        Ok(())
    }
}

fn default_rerank_max_results() -> usize {
    20
}

fn default_rerank_batch_size() -> usize {
    10
}

fn default_rerank_timeout_secs() -> u64 {
    30
}

fn default_fallback_on_error() -> bool {
    true
}

// ─────────────────────────────────────────────────────────────────────────────
// File Processing
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for RAG file processing (chunking and embedding).
///
/// Controls how uploaded files are processed when added to vector stores.
/// Supports two processing modes:
///
/// - **Inline**: Process files synchronously within the gateway process.
///   Simpler setup, but may timeout for large files (>10MB).
///
/// - **Queue**: Publish processing jobs to an external queue for worker processes.
///   Better for production deployments with large files or high volume.
///
/// # Example Configuration
///
/// ```toml
/// [features.file_processing]
/// mode = "inline"
/// max_file_size_mb = 10
/// max_concurrent_tasks = 4
/// default_max_chunk_tokens = 800
/// default_overlap_tokens = 200
/// ```
///
/// For queue mode:
///
/// ```toml
/// [features.file_processing]
/// mode = "queue"
///
/// [features.file_processing.queue]
/// backend = "redis"
/// url = "redis://localhost:6379"
/// queue_name = "file_processing"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileProcessingConfig {
    /// Processing mode: inline or queue.
    #[serde(default)]
    pub mode: FileProcessingMode,

    /// Maximum file size in megabytes.
    /// Files larger than this will be rejected.
    /// Default: 10 MB
    #[serde(default = "default_file_processing_max_size_mb")]
    pub max_file_size_mb: u64,

    /// Maximum concurrent file processing tasks (inline mode only).
    /// Controls how many files can be processed simultaneously.
    /// Default: 4
    #[serde(default = "default_file_processing_max_concurrent")]
    pub max_concurrent_tasks: usize,

    /// Default maximum chunk size in tokens when using auto chunking strategy.
    /// Default: 800
    #[serde(default = "default_file_processing_max_chunk_tokens")]
    pub default_max_chunk_tokens: i32,

    /// Default chunk overlap in tokens when using auto chunking strategy.
    /// Overlap provides context continuity between chunks.
    /// Default: 200
    #[serde(default = "default_file_processing_overlap_tokens")]
    pub default_overlap_tokens: i32,

    /// Queue configuration (required when mode = "queue").
    #[serde(default)]
    pub queue: Option<FileProcessingQueueConfig>,

    /// Callback URL for queue workers to report completion.
    /// Workers POST to this URL when file processing completes.
    /// Only used in queue mode.
    #[serde(default)]
    pub callback_url: Option<String>,

    /// Virus scanning configuration.
    /// When enabled, uploaded files are scanned before being stored.
    #[serde(default)]
    pub virus_scan: VirusScanConfig,

    /// Document extraction configuration.
    /// Controls OCR and PDF-specific processing options for rich documents.
    #[serde(default)]
    pub document_extraction: DocumentExtractionConfig,

    /// Retry configuration for vector store operations during file processing.
    ///
    /// Applies to:
    /// - Storing chunks to vector database (transient DB errors)
    /// - Deleting chunks (connection issues)
    ///
    /// Note: Embedding API retries are handled separately at the provider level.
    /// Default: enabled with 3 retries, 100ms initial delay, 2x backoff.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for vector store operations.
    ///
    /// Protects against unhealthy vector store backends by failing fast
    /// after repeated failures. When the circuit is open, requests fail
    /// immediately without attempting the operation.
    ///
    /// Default: enabled with 5 failures in 60s to open, 30s recovery.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Timeout in seconds for detecting stale in-progress files.
    ///
    /// When a file has been in `in_progress` status longer than this timeout,
    /// it's considered stale (e.g., worker crashed mid-processing). Re-adding
    /// the file will reset it for re-processing.
    ///
    /// Set to 0 to disable stale detection (files stuck in in_progress
    /// will never be automatically re-processed).
    ///
    /// Default: 1800 (30 minutes)
    #[serde(default = "default_stale_processing_timeout_secs")]
    pub stale_processing_timeout_secs: u64,
}

impl Default for FileProcessingConfig {
    fn default() -> Self {
        Self {
            mode: FileProcessingMode::default(),
            max_file_size_mb: default_file_processing_max_size_mb(),
            max_concurrent_tasks: default_file_processing_max_concurrent(),
            default_max_chunk_tokens: default_file_processing_max_chunk_tokens(),
            default_overlap_tokens: default_file_processing_overlap_tokens(),
            queue: None,
            callback_url: None,
            virus_scan: VirusScanConfig::default(),
            document_extraction: DocumentExtractionConfig::default(),
            retry: RetryConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            stale_processing_timeout_secs: default_stale_processing_timeout_secs(),
        }
    }
}

impl FileProcessingConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.mode == FileProcessingMode::Queue && self.queue.is_none() {
            return Err(
                "Queue mode requires [features.file_processing.queue] configuration".to_string(),
            );
        }
        if let Some(ref queue) = self.queue {
            queue.validate()?;
        }
        self.virus_scan.validate()?;
        Ok(())
    }

    /// Get max file size in bytes.
    pub fn max_file_size_bytes(&self) -> i64 {
        (self.max_file_size_mb * 1024 * 1024) as i64
    }
}

/// File processing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileProcessingMode {
    /// Process files inline within the gateway process.
    /// Simplest setup, good for small deployments and small files.
    /// May timeout for large files (>10MB) or high concurrency.
    #[default]
    Inline,

    /// Publish processing jobs to an external queue.
    /// Workers consume jobs and process files asynchronously.
    /// Better for production with large files or high volume.
    Queue,
}

/// Queue backend configuration for file processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileProcessingQueueConfig {
    /// Queue backend type.
    pub backend: FileProcessingQueueBackend,

    /// Connection URL for the queue backend.
    /// Example: "redis://localhost:6379"
    pub url: String,

    /// Queue/topic name for processing jobs.
    #[serde(default = "default_file_processing_queue_name")]
    pub queue_name: String,

    /// Consumer group name (for Redis Streams).
    #[serde(default = "default_file_processing_consumer_group")]
    pub consumer_group: String,
}

impl FileProcessingQueueConfig {
    /// Validate queue configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.url.is_empty() {
            return Err("Queue URL cannot be empty".to_string());
        }
        if self.queue_name.is_empty() {
            return Err("Queue name cannot be empty".to_string());
        }
        Ok(())
    }
}

/// Queue backend type for file processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileProcessingQueueBackend {
    /// Redis Streams.
    /// Good for simple deployments, supports consumer groups.
    Redis,
}

// ─────────────────────────────────────────────────────────────────────────────
// Document Extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Document extraction configuration for processing rich documents.
///
/// Controls how PDF, Office, and other document formats are processed,
/// including OCR settings for scanned documents and images.
///
/// Uses [Kreuzberg](https://github.com/Goldziher/kreuzberg) for document extraction.
///
/// # Example Configuration
///
/// ```toml
/// [features.file_processing.document_extraction]
/// enable_ocr = true
/// ocr_language = "eng"
/// force_ocr = false
/// pdf_extract_images = true
/// pdf_image_dpi = 300
/// ```
///
/// # OCR Requirements
///
/// OCR requires Tesseract to be installed on the system:
/// - **Linux**: `apt install tesseract-ocr tesseract-ocr-eng`
/// - **macOS**: `brew install tesseract`
/// - **Windows**: Install from <https://github.com/UB-Mannheim/tesseract/wiki>
///
/// Additional language packs can be installed for non-English documents:
/// - `tesseract-ocr-fra` (French)
/// - `tesseract-ocr-deu` (German)
/// - `tesseract-ocr-spa` (Spanish)
/// - etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DocumentExtractionConfig {
    /// Enable OCR (Optical Character Recognition) for scanned documents and images.
    ///
    /// When enabled, Kreuzberg will use Tesseract to extract text from:
    /// - Scanned PDF documents (no embedded text layer)
    /// - Images embedded in documents
    /// - Image files (PNG, JPG, TIFF, etc.) if supported
    ///
    /// Requires Tesseract to be installed on the system.
    /// Default: false
    #[serde(default)]
    pub enable_ocr: bool,

    /// Force OCR processing even for documents that have embedded text.
    ///
    /// Useful when:
    /// - The embedded text is known to be unreliable or incomplete
    /// - Documents were generated from scanned images with poor OCR
    /// - You want consistent processing regardless of text layer presence
    ///
    /// Has no effect if `enable_ocr` is false.
    /// Default: false
    #[serde(default)]
    pub force_ocr: bool,

    /// Language code for OCR processing (ISO 639-3 format).
    ///
    /// Common values:
    /// - `eng` - English (default)
    /// - `fra` - French
    /// - `deu` - German
    /// - `spa` - Spanish
    /// - `chi_sim` - Simplified Chinese
    /// - `jpn` - Japanese
    ///
    /// The corresponding Tesseract language pack must be installed.
    /// Default: "eng"
    #[serde(default = "default_ocr_language")]
    pub ocr_language: String,

    /// Extract images from PDF documents for OCR processing.
    ///
    /// When enabled, embedded images in PDFs will be extracted and
    /// processed with OCR to capture text that may only exist in images.
    ///
    /// This increases processing time but improves text extraction for
    /// documents with charts, diagrams, or embedded scanned pages.
    ///
    /// Has no effect if `enable_ocr` is false.
    /// Default: false
    #[serde(default)]
    pub pdf_extract_images: bool,

    /// DPI (dots per inch) for image extraction from PDFs.
    ///
    /// Higher values produce better OCR quality but increase processing
    /// time and memory usage.
    ///
    /// Recommended values:
    /// - 150: Fast processing, acceptable quality
    /// - 300: Good balance (default)
    /// - 600: High quality for small text
    ///
    /// Default: 300
    #[serde(default = "default_pdf_image_dpi")]
    pub pdf_image_dpi: u32,

    /// Maximum time (in seconds) a single document extraction is allowed to
    /// run. Set to 0 to disable the timeout.
    ///
    /// A malicious or pathological document (e.g. an OCR job on a 5,000-page
    /// PDF) can otherwise tie up an extraction worker indefinitely.
    /// Default: 120 seconds (2 minutes)
    #[serde(default = "default_extraction_timeout_secs")]
    pub extraction_timeout_secs: u64,
}

impl Default for DocumentExtractionConfig {
    fn default() -> Self {
        Self {
            enable_ocr: false,
            force_ocr: false,
            ocr_language: default_ocr_language(),
            pdf_extract_images: false,
            pdf_image_dpi: default_pdf_image_dpi(),
            extraction_timeout_secs: default_extraction_timeout_secs(),
        }
    }
}

fn default_extraction_timeout_secs() -> u64 {
    120
}

fn default_ocr_language() -> String {
    "eng".to_string()
}

fn default_pdf_image_dpi() -> u32 {
    300
}

// ─────────────────────────────────────────────────────────────────────────────
// Virus Scanning
// ─────────────────────────────────────────────────────────────────────────────

/// Virus scanning configuration for file uploads.
///
/// When enabled, files are scanned for malware before being stored.
/// Currently supports ClamAV via the clamd daemon.
///
/// # Example Configuration
///
/// ```toml
/// [features.file_processing.virus_scan]
/// enabled = true
/// backend = "clamav"
///
/// [features.file_processing.virus_scan.clamav]
/// host = "localhost"
/// port = 3310
/// timeout_ms = 30000
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct VirusScanConfig {
    /// Enable virus scanning.
    /// When false, files are not scanned before storage.
    /// Default: false
    #[serde(default)]
    pub enabled: bool,

    /// Virus scanning backend.
    /// Currently only "clamav" is supported.
    #[serde(default)]
    pub backend: VirusScanBackend,

    /// ClamAV-specific configuration.
    /// Required when backend = "clamav" and enabled = true.
    #[serde(default)]
    pub clamav: Option<ClamAvConfig>,
}

impl VirusScanConfig {
    /// Validate virus scan configuration.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        match self.backend {
            VirusScanBackend::ClamAv => {
                if self.clamav.is_none() {
                    return Err(
                        "ClamAV backend requires [features.file_processing.virus_scan.clamav] configuration".to_string()
                    );
                }
                if let Some(ref clamav) = self.clamav {
                    clamav.validate()?;
                }
            }
        }

        Ok(())
    }
}

/// Virus scanning backend type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum VirusScanBackend {
    /// ClamAV via clamd daemon.
    /// Open-source antivirus with regularly updated signatures.
    #[default]
    #[serde(rename = "clamav")]
    ClamAv,
}

/// ClamAV daemon (clamd) configuration.
///
/// Clamd must be running and accessible at the configured host:port.
/// The gateway connects via TCP to scan file contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ClamAvConfig {
    /// Host where clamd is running.
    /// Default: "localhost"
    #[serde(default = "default_clamav_host")]
    pub host: String,

    /// Port where clamd is listening.
    /// Default: 3310 (standard clamd port)
    #[serde(default = "default_clamav_port")]
    pub port: u16,

    /// Timeout for scan operations in milliseconds.
    /// Large files may need longer timeouts.
    /// Default: 30000 (30 seconds)
    #[serde(default = "default_clamav_timeout_ms")]
    pub timeout_ms: u64,

    /// Maximum file size to scan in megabytes.
    /// Files larger than this are rejected without scanning.
    /// Should match or be less than clamd's StreamMaxLength setting.
    /// Default: 25 MB (ClamAV default)
    #[serde(default = "default_clamav_max_file_size_mb")]
    pub max_file_size_mb: u64,

    /// Use Unix socket instead of TCP.
    /// When set, host and port are ignored.
    #[serde(default)]
    pub socket_path: Option<String>,
}

impl Default for ClamAvConfig {
    fn default() -> Self {
        Self {
            host: default_clamav_host(),
            port: default_clamav_port(),
            timeout_ms: default_clamav_timeout_ms(),
            max_file_size_mb: default_clamav_max_file_size_mb(),
            socket_path: None,
        }
    }
}

impl ClamAvConfig {
    /// Validate ClamAV configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.socket_path.is_none() && self.host.is_empty() {
            return Err("ClamAV host cannot be empty when socket_path is not set".to_string());
        }
        if self.timeout_ms == 0 {
            return Err("ClamAV timeout_ms must be greater than 0".to_string());
        }
        Ok(())
    }

    /// Get the connection address string for TCP connections.
    pub fn tcp_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Get max file size in bytes.
    pub fn max_file_size_bytes(&self) -> i64 {
        (self.max_file_size_mb * 1024 * 1024) as i64
    }
}

fn default_clamav_host() -> String {
    "localhost".to_string()
}

fn default_clamav_port() -> u16 {
    3310
}

fn default_clamav_timeout_ms() -> u64 {
    30000
}

fn default_clamav_max_file_size_mb() -> u64 {
    25
}

fn default_file_processing_max_size_mb() -> u64 {
    10
}

fn default_file_processing_max_concurrent() -> usize {
    4
}

fn default_file_processing_max_chunk_tokens() -> i32 {
    800
}

fn default_file_processing_overlap_tokens() -> i32 {
    200
}

fn default_stale_processing_timeout_secs() -> u64 {
    1800 // 30 minutes
}

fn default_file_processing_queue_name() -> String {
    "hadrian_file_processing".to_string()
}

fn default_file_processing_consumer_group() -> String {
    "hadrian_workers".to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Guardrails
// ─────────────────────────────────────────────────────────────────────────────

/// Comprehensive guardrails configuration for content filtering, PII detection,
/// and safety enforcement.
///
/// Guardrails can be applied to:
/// - **Input (pre-request)**: Evaluate user messages before sending to LLM
/// - **Output (post-response)**: Evaluate LLM responses before returning to user
///
/// Each stage can use a different provider and have different action policies.
///
/// # Example Configuration
///
/// ```toml
/// [features.guardrails]
/// enabled = true
///
/// [features.guardrails.input]
/// enabled = true
/// mode = "blocking"
///
/// [features.guardrails.input.provider]
/// type = "bedrock"
/// guardrail_id = "abc123"
/// guardrail_version = "1"
///
/// [features.guardrails.input.actions]
/// HATE = "block"
/// PROMPT_ATTACK = "block"
/// VIOLENCE = "warn"
///
/// [features.guardrails.output]
/// enabled = true
///
/// [features.guardrails.output.provider]
/// type = "openai_moderation"
///
/// [features.guardrails.pii]
/// enabled = true
/// action = "redact"
/// types = ["EMAIL", "PHONE", "SSN"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GuardrailsConfig {
    /// Enable guardrails globally.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Input (pre-request) guardrails configuration.
    /// Evaluates user input before sending to the LLM.
    #[serde(default)]
    pub input: Option<InputGuardrailsConfig>,

    /// Output (post-response) guardrails configuration.
    /// Evaluates LLM output before returning to the user.
    #[serde(default)]
    pub output: Option<OutputGuardrailsConfig>,

    /// PII detection and handling configuration.
    /// Can work independently or in combination with content guardrails.
    #[serde(default)]
    pub pii: Option<PiiGuardrailsConfig>,

    /// Custom guardrails via external webhook.
    /// Use this for bring-your-own guardrails implementations.
    #[serde(default)]
    pub custom: Option<CustomGuardrailsConfig>,

    /// Audit logging configuration for guardrails events.
    /// Controls what guardrails events are logged to the audit log.
    #[serde(default)]
    pub audit: GuardrailsAuditConfig,
}

/// Audit logging configuration for guardrails events.
///
/// Controls what guardrails events are logged to the audit log table.
/// Events are logged asynchronously in a fire-and-forget pattern to avoid
/// impacting request latency.
///
/// # Example
///
/// ```toml
/// [features.guardrails.audit]
/// enabled = true
/// log_all_evaluations = false  # Only log violations, not all evaluations
/// log_blocked = true           # Log blocked requests/responses
/// log_violations = true        # Log policy violations
/// log_redacted = true          # Log redaction events (hashes, not content)
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GuardrailsAuditConfig {
    /// Enable audit logging for guardrails events.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Log all evaluations, not just violations.
    /// When false, only violations/blocks are logged.
    /// When true, every guardrails evaluation is logged (high volume).
    #[serde(default)]
    pub log_all_evaluations: bool,

    /// Log blocked requests/responses.
    #[serde(default = "default_true")]
    pub log_blocked: bool,

    /// Log policy violations (even if not blocked).
    #[serde(default = "default_true")]
    pub log_violations: bool,

    /// Log redaction events.
    /// Includes content hashes (not actual content) for audit trail.
    #[serde(default = "default_true")]
    pub log_redacted: bool,
}

impl Default for GuardrailsAuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_all_evaluations: false,
            log_blocked: true,
            log_violations: true,
            log_redacted: true,
        }
    }
}

/// Input guardrails configuration (pre-request evaluation).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InputGuardrailsConfig {
    /// Enable input guardrails.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Execution mode for input guardrails.
    #[serde(default)]
    pub mode: GuardrailsExecutionMode,

    /// Timeout for guardrails evaluation in milliseconds.
    /// Only applies to concurrent mode - if evaluation takes longer,
    /// the request proceeds based on `on_timeout` setting.
    #[serde(default = "default_guardrails_timeout_ms")]
    pub timeout_ms: u64,

    /// Behavior when guardrails evaluation times out (concurrent mode only).
    #[serde(default)]
    pub on_timeout: GuardrailsTimeoutAction,

    /// Behavior when guardrails provider fails (network error, etc.).
    #[serde(default)]
    pub on_error: GuardrailsErrorAction,

    /// Guardrails provider to use for input evaluation.
    pub provider: GuardrailsProvider,

    /// Per-category action configuration.
    /// Maps category names to actions. Unknown categories use `default_action`.
    #[serde(default)]
    pub actions: std::collections::HashMap<String, GuardrailsAction>,

    /// Default action for categories not specified in `actions`.
    #[serde(default)]
    pub default_action: GuardrailsAction,
}

/// Output guardrails configuration (post-response evaluation).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OutputGuardrailsConfig {
    /// Enable output guardrails.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Timeout for guardrails evaluation in milliseconds.
    #[serde(default = "default_guardrails_timeout_ms")]
    pub timeout_ms: u64,

    /// Behavior when guardrails provider fails.
    #[serde(default)]
    pub on_error: GuardrailsErrorAction,

    /// Guardrails provider to use for output evaluation.
    pub provider: GuardrailsProvider,

    /// Per-category action configuration.
    #[serde(default)]
    pub actions: std::collections::HashMap<String, GuardrailsAction>,

    /// Default action for categories not specified in `actions`.
    #[serde(default)]
    pub default_action: GuardrailsAction,

    /// Streaming evaluation mode.
    /// Controls how output is evaluated during streaming responses.
    #[serde(default)]
    pub streaming_mode: StreamingGuardrailsMode,
}

/// PII detection and handling configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PiiGuardrailsConfig {
    /// Enable PII detection.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// PII types to detect.
    /// Common types: EMAIL, PHONE, SSN, CREDIT_CARD, ADDRESS, NAME, DATE_OF_BIRTH
    #[serde(default = "default_pii_types")]
    pub types: Vec<PiiType>,

    /// Action to take when PII is detected.
    #[serde(default)]
    pub action: PiiAction,

    /// Custom replacement text for redaction.
    /// Only used when action is `redact`.
    #[serde(default = "default_pii_replacement")]
    pub replacement: String,

    /// Apply to input, output, or both.
    #[serde(default)]
    pub apply_to: PiiApplyTo,

    /// Provider for PII detection (if not using the main guardrails provider).
    /// If not specified, uses the provider from input/output guardrails config.
    #[serde(default)]
    pub provider: Option<PiiProvider>,
}

/// Custom guardrails configuration for external webhook-based evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CustomGuardrailsConfig {
    /// Enable custom guardrails.
    #[serde(default)]
    pub enabled: bool,

    /// Custom guardrails provider configuration.
    pub provider: CustomGuardrailsProvider,

    /// Apply to input, output, or both.
    #[serde(default)]
    pub apply_to: GuardrailsApplyTo,

    /// Timeout for custom guardrails evaluation in milliseconds.
    #[serde(default = "default_guardrails_timeout_ms")]
    pub timeout_ms: u64,

    /// Behavior when custom guardrails provider fails.
    #[serde(default)]
    pub on_error: GuardrailsErrorAction,
}

/// Guardrails execution mode for input evaluation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuardrailsExecutionMode {
    /// Blocking mode: wait for guardrails evaluation before sending to LLM.
    /// This is the safest mode but adds latency.
    #[default]
    Blocking,

    /// Concurrent mode: start guardrails evaluation and LLM call simultaneously.
    /// If guardrails fail before LLM responds, cancel the LLM request.
    /// If LLM responds first, wait for guardrails result before returning.
    /// Reduces perceived latency while maintaining safety.
    Concurrent,
}

/// Action when guardrails evaluation times out.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuardrailsTimeoutAction {
    /// Block the request on timeout (fail-closed).
    #[default]
    Block,

    /// Allow the request on timeout (fail-open).
    /// Use only when availability is more important than safety.
    Allow,
}

/// Action when guardrails provider encounters an error.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuardrailsErrorAction {
    /// Block the request on error (fail-closed).
    #[default]
    Block,

    /// Allow the request on error (fail-open).
    Allow,

    /// Log the error and allow the request.
    LogAndAllow,
}

/// Guardrails provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum GuardrailsProvider {
    /// OpenAI Moderation API.
    /// Free, fast, good for general content moderation.
    OpenaiModeration {
        /// OpenAI API key. If not provided, uses the default OpenAI provider key.
        #[serde(default)]
        api_key: Option<String>,

        /// Base URL for the moderation API (default: https://api.openai.com/v1).
        /// Useful for proxies or OpenAI-compatible endpoints.
        #[serde(default = "default_openai_moderation_base_url")]
        base_url: String,

        /// Model to use (default: text-moderation-latest).
        #[serde(default = "default_openai_moderation_model")]
        model: String,
    },

    /// AWS Bedrock Guardrails.
    /// Enterprise-grade with configurable policies, PII detection, and word filters.
    #[cfg(feature = "provider-bedrock")]
    Bedrock {
        /// Guardrail identifier.
        guardrail_id: String,

        /// Guardrail version.
        guardrail_version: String,

        /// AWS region. If not specified, uses default region from environment.
        #[serde(default)]
        region: Option<String>,

        /// AWS access key ID. If not specified, uses default credentials.
        #[serde(default)]
        access_key_id: Option<String>,

        /// AWS secret access key. If not specified, uses default credentials.
        #[serde(default)]
        secret_access_key: Option<String>,

        /// Enable trace for debugging.
        #[serde(default)]
        trace_enabled: bool,
    },

    /// Azure AI Content Safety.
    /// Enterprise-grade with configurable severity levels.
    AzureContentSafety {
        /// Azure Content Safety endpoint URL.
        endpoint: String,

        /// Azure API key.
        api_key: String,

        /// API version (default: 2024-09-01).
        #[serde(default = "default_azure_content_safety_version")]
        api_version: String,

        /// Severity thresholds per category (0-6, content above threshold is flagged).
        /// If not specified, uses Azure defaults.
        #[serde(default)]
        thresholds: std::collections::HashMap<String, u8>,

        /// Enable blocklist checking.
        #[serde(default)]
        blocklist_names: Vec<String>,
    },

    /// Built-in blocklist provider.
    /// Fast, local pattern matching with no external dependencies.
    Blocklist {
        /// List of patterns to match against content.
        patterns: Vec<BlocklistPattern>,

        /// Whether to match patterns case-insensitively (default: true).
        #[serde(default = "default_blocklist_case_insensitive")]
        case_insensitive: bool,
    },

    /// Built-in regex-based PII detection provider.
    /// Fast, local detection of common PII types with no external dependencies.
    PiiRegex {
        /// Detect email addresses.
        #[serde(default = "default_true")]
        email: bool,

        /// Detect phone numbers (US and international formats).
        #[serde(default = "default_true")]
        phone: bool,

        /// Detect Social Security Numbers.
        #[serde(default = "default_true")]
        ssn: bool,

        /// Detect credit card numbers (with Luhn validation).
        #[serde(default = "default_true")]
        credit_card: bool,

        /// Detect IP addresses (IPv4 and IPv6).
        #[serde(default = "default_true")]
        ip_address: bool,

        /// Detect dates that may be dates of birth.
        #[serde(default = "default_true")]
        date_of_birth: bool,
    },

    /// Built-in content limits provider.
    /// Enforces size constraints on content with no external dependencies.
    ContentLimits {
        /// Maximum number of characters allowed.
        #[serde(default)]
        max_characters: Option<usize>,

        /// Maximum number of words allowed.
        #[serde(default)]
        max_words: Option<usize>,

        /// Maximum number of lines allowed.
        #[serde(default)]
        max_lines: Option<usize>,
    },

    /// Custom HTTP guardrails provider.
    /// For bring-your-own guardrails implementations.
    Custom(CustomGuardrailsProvider),
}

/// A pattern for the blocklist guardrails provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BlocklistPattern {
    /// The pattern to match. Can be a literal string or regex (if `is_regex` is true).
    pub pattern: String,

    /// Whether the pattern is a regex (default: false, treated as literal string).
    #[serde(default)]
    pub is_regex: bool,

    /// Category to assign when this pattern matches.
    #[serde(default = "default_blocklist_category")]
    pub category: String,

    /// Severity level for matches (default: high).
    #[serde(default = "default_blocklist_severity")]
    pub severity: String,

    /// Human-readable description of why this pattern is blocked.
    #[serde(default)]
    pub message: Option<String>,
}

fn default_blocklist_case_insensitive() -> bool {
    true
}

fn default_blocklist_category() -> String {
    "blocked_content".to_string()
}

fn default_blocklist_severity() -> String {
    "high".to_string()
}

/// Custom HTTP guardrails provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CustomGuardrailsProvider {
    /// Guardrails service URL.
    pub url: String,

    /// API key for authentication.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Custom headers to include in requests.
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,

    /// Request timeout in milliseconds.
    #[serde(default = "default_guardrails_timeout_ms")]
    pub timeout_ms: u64,

    /// Enable retry on failure.
    #[serde(default)]
    pub retry_enabled: bool,

    /// Maximum number of retries.
    #[serde(default = "default_guardrails_max_retries")]
    pub max_retries: u32,
}

/// Action to take when content is flagged by guardrails.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum GuardrailsAction {
    /// Block the request/response and return an error.
    #[default]
    Block,

    /// Allow but add warning headers to the response.
    Warn,

    /// Allow silently but log the violation.
    Log,

    /// Replace flagged content with a placeholder.
    Redact {
        /// Replacement text for redacted content.
        #[serde(default = "default_redaction_text")]
        replacement: String,
    },

    /// Transform/modify the content (provider-specific).
    Modify,
}

/// Streaming output evaluation mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StreamingGuardrailsMode {
    /// Only evaluate the complete response after streaming finishes.
    /// Lowest latency, but harmful content may be partially streamed.
    FinalOnly,

    /// Buffer chunks and evaluate periodically.
    /// Balance between latency and safety. This is the default mode.
    Buffered {
        /// Number of tokens to buffer before evaluation.
        #[serde(default = "default_streaming_buffer_tokens")]
        buffer_tokens: u32,
    },

    /// Evaluate each chunk individually.
    /// Highest safety but significantly increases latency.
    PerChunk,
}

impl Default for StreamingGuardrailsMode {
    fn default() -> Self {
        Self::Buffered {
            buffer_tokens: default_streaming_buffer_tokens(),
        }
    }
}

/// PII types for detection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PiiType {
    Email,
    Phone,
    Ssn,
    CreditCard,
    Address,
    Name,
    DateOfBirth,
    DriversLicense,
    Passport,
    BankAccount,
    IpAddress,
    MacAddress,
    Url,
    Username,
    Password,
    AwsAccessKey,
    AwsSecretKey,
    ApiKey,
    /// Custom PII type (provider-specific).
    #[serde(untagged)]
    Custom(String),
}

/// Action to take when PII is detected.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PiiAction {
    /// Block the request/response containing PII.
    Block,

    /// Redact PII with placeholder text.
    #[default]
    Redact,

    /// Anonymize PII (replace with consistent fake values).
    Anonymize,

    /// Log PII detection but allow the content through.
    Log,
}

/// Where to apply PII detection.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PiiApplyTo {
    /// Apply to input only.
    Input,
    /// Apply to output only.
    Output,
    /// Apply to both input and output.
    #[default]
    Both,
}

/// Where to apply custom guardrails.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuardrailsApplyTo {
    /// Apply to input only.
    Input,
    /// Apply to output only.
    Output,
    /// Apply to both input and output.
    #[default]
    Both,
}

/// PII detection provider (if not using main guardrails provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum PiiProvider {
    /// Use AWS Bedrock Guardrails for PII detection.
    Bedrock {
        guardrail_id: String,
        guardrail_version: String,
        #[serde(default)]
        region: Option<String>,
    },

    /// Use a regex-based local PII detector.
    Regex,

    /// Use a custom PII detection service.
    Custom {
        url: String,
        #[serde(default)]
        api_key: Option<String>,
    },
}

// Default value functions for guardrails

fn default_guardrails_timeout_ms() -> u64 {
    5000 // 5 seconds
}

fn default_guardrails_max_retries() -> u32 {
    2
}

fn default_openai_moderation_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_openai_moderation_model() -> String {
    "text-moderation-latest".to_string()
}

fn default_azure_content_safety_version() -> String {
    "2024-09-01".to_string()
}

fn default_redaction_text() -> String {
    "[REDACTED]".to_string()
}

fn default_streaming_buffer_tokens() -> u32 {
    100
}

fn default_pii_types() -> Vec<PiiType> {
    vec![
        PiiType::Email,
        PiiType::Phone,
        PiiType::Ssn,
        PiiType::CreditCard,
    ]
}

fn default_pii_replacement() -> String {
    "[PII REDACTED]".to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Caching
// ─────────────────────────────────────────────────────────────────────────────

/// Response caching configuration (gateway-level caching).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResponseCachingConfig {
    /// Enable response caching.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Cache TTL in seconds.
    #[serde(default = "default_cache_ttl")]
    pub ttl_secs: u64,

    /// Only cache responses with temperature = 0.
    #[serde(default = "default_true")]
    pub only_deterministic: bool,

    /// Maximum response size to cache in bytes.
    #[serde(default = "default_max_cache_size")]
    pub max_size_bytes: usize,

    /// Cache key components.
    #[serde(default)]
    pub key_components: CacheKeyComponents,

    /// Semantic caching configuration.
    /// When enabled, requests are matched based on semantic similarity
    /// in addition to exact hash matching.
    #[serde(default)]
    pub semantic: Option<SemanticCachingConfig>,
}

/// Semantic caching configuration for similarity-based cache matching.
///
/// When enabled, the cache will also look up semantically similar requests
/// using vector embeddings, allowing cache hits for requests that are
/// different in wording but semantically equivalent.
///
/// # Configuration Example
///
/// ```toml
/// [features.response_caching.semantic]
/// enabled = true
/// similarity_threshold = 0.95  # Minimum cosine similarity for cache hit
///
/// [features.response_caching.semantic.embedding]
/// provider = "openai"
/// model = "text-embedding-3-small"
/// dimensions = 1536
///
/// [features.response_caching.semantic.vector_backend]
/// type = "pgvector"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SemanticCachingConfig {
    /// Enable semantic caching.
    /// When false, only exact-match caching is used.
    #[serde(default)]
    pub enabled: bool,

    /// Minimum cosine similarity threshold for a semantic cache hit (0.0-1.0).
    /// Higher values require closer semantic matches.
    /// Recommended: 0.92-0.98 depending on use case.
    #[serde(default = "default_semantic_similarity_threshold")]
    pub similarity_threshold: f64,

    /// Maximum number of similar results to consider when looking up.
    /// The closest match above the threshold is used.
    #[serde(default = "default_semantic_top_k")]
    pub top_k: usize,

    /// Embedding configuration for generating request embeddings.
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    /// Vector database backend for storing and querying embeddings.
    pub vector_backend: SemanticVectorBackend,
}

fn default_semantic_similarity_threshold() -> f64 {
    0.95
}

fn default_semantic_top_k() -> usize {
    1
}

/// Vector database backend for semantic caching.
///
/// Unlike the general `VectorBackend` for RAG, semantic caching only
/// supports backends that can be efficiently queried for single-vector
/// similarity lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum SemanticVectorBackend {
    /// PostgreSQL with pgvector extension.
    /// Uses the existing database connection pool.
    Pgvector {
        /// Table name for storing cache embeddings.
        #[serde(default = "default_semantic_table_name")]
        table_name: String,

        /// Index type for vector similarity search.
        #[serde(default)]
        index_type: PgvectorIndexType,

        /// Distance metric for similarity search.
        /// Defaults to cosine, which works best for text embeddings.
        #[serde(default = "default_distance_metric")]
        distance_metric: DistanceMetric,
    },

    /// Qdrant vector database.
    Qdrant {
        /// Qdrant server URL.
        url: String,

        /// API key for authentication (optional).
        #[serde(default)]
        api_key: Option<String>,

        /// VectorStore name for storing cache embeddings.
        #[serde(default = "default_semantic_vector_store_name")]
        qdrant_collection_name: String,

        /// Distance metric for similarity search.
        /// Defaults to cosine, which works best for text embeddings.
        #[serde(default = "default_distance_metric")]
        distance_metric: DistanceMetric,
    },
}

fn default_semantic_table_name() -> String {
    "semantic_cache_embeddings".to_string()
}

fn default_semantic_vector_store_name() -> String {
    "semantic_cache".to_string()
}

/// Index type for pgvector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PgvectorIndexType {
    /// IVFFlat index - faster to build, good for moderate dataset sizes.
    #[default]
    IvfFlat,
    /// HNSW index - better query performance, slower to build.
    Hnsw,
}

/// Distance metric for vector similarity search.
///
/// Different metrics are suited for different embedding models and use cases:
///
/// - **Cosine**: Measures the angle between vectors. Best for normalized embeddings
///   (most text embedding models). Score range: 0.0-1.0 (higher = more similar).
///   This is the default and recommended for most use cases.
///
/// - **DotProduct** (Inner Product): Measures the projection of one vector onto another.
///   Best for embeddings where magnitude carries meaning. Requires normalized vectors
///   to produce bounded scores. Score range: varies by implementation.
///
/// - **Euclidean** (L2): Measures the straight-line distance between vectors.
///   Best for embeddings in metric spaces. Score range: 0.0-∞ (lower = more similar,
///   converted to similarity score internally).
///
/// # When to Use Each Metric
///
/// | Metric      | Best For | Embedding Models |
/// |-------------|----------|------------------|
/// | **Cosine** (default) | Text similarity, semantic search | OpenAI `text-embedding-3-*`, Cohere `embed-v3`, Voyage, most text models |
/// | **DotProduct** | Maximum inner product search (MIPS), retrieval-augmented generation | Models trained with contrastive loss, some custom models |
/// | **Euclidean** | Clustering, when absolute distances matter | Image embeddings, some scientific/domain-specific models |
///
/// **Recommendation:** Use **Cosine** unless you have a specific reason not to. Most text
/// embedding models produce normalized vectors optimized for cosine similarity.
///
/// # Backend Support
///
/// | Metric      | pgvector Operator | Qdrant Distance |
/// |-------------|-------------------|-----------------|
/// | Cosine      | `<=>` (cosine)    | `Cosine`        |
/// | DotProduct  | `<#>` (neg. IP)   | `Dot`           |
/// | Euclidean   | `<->` (L2)        | `Euclid`        |
///
/// # Score Normalization
///
/// All metrics are normalized to return similarity scores in the 0.0-1.0 range
/// where higher values indicate more similar vectors. The conversion formulas:
///
/// - Cosine: `similarity = 1.0 - cosine_distance` (pgvector returns distance)
/// - DotProduct: `similarity = (1.0 + dot_product) / 2.0` (normalized embeddings)
/// - Euclidean: `similarity = 1.0 / (1.0 + euclidean_distance)`
///
/// # Caveats
///
/// - **DotProduct requires normalized embeddings**: The score normalization formula
///   `(1 + score) / 2` assumes unit vectors. Non-normalized embeddings may produce
///   scores outside the 0.0-1.0 range (clamped for safety but semantically incorrect).
///
/// - **Changing metrics requires re-indexing**: If you change the distance metric after
///   data has been indexed, you must recreate the vector index for correct results.
///
/// # Configuration Example
///
/// ```toml
/// # RAG vector backend with explicit distance metric
/// [features.file_search.vector_backend]
/// type = "pgvector"
/// table_name = "rag_chunks"
/// distance_metric = "cosine"  # or "dot_product", "euclidean"
///
/// # Semantic caching with Qdrant
/// [features.response_caching.semantic.vector_backend]
/// type = "qdrant"
/// url = "http://localhost:6333"
/// qdrant_collection_name = "semantic_cache"
/// distance_metric = "cosine"
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    /// Cosine similarity - best for text embeddings (default).
    /// Measures the angle between vectors, ignoring magnitude.
    #[default]
    Cosine,

    /// Dot product (inner product) - for embeddings where magnitude matters.
    /// Requires normalized vectors for bounded scores.
    DotProduct,

    /// Euclidean distance (L2) - for metric space embeddings.
    /// Measures straight-line distance between vector endpoints.
    Euclidean,
}

impl DistanceMetric {
    /// Returns the pgvector operator class name for index creation.
    pub fn pgvector_ops_class(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "vector_cosine_ops",
            DistanceMetric::DotProduct => "vector_ip_ops",
            DistanceMetric::Euclidean => "vector_l2_ops",
        }
    }

    /// Returns the pgvector distance operator for queries.
    pub fn pgvector_operator(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "<=>",
            DistanceMetric::DotProduct => "<#>",
            DistanceMetric::Euclidean => "<->",
        }
    }

    /// Returns the Qdrant distance type string.
    pub fn qdrant_distance(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "Cosine",
            DistanceMetric::DotProduct => "Dot",
            DistanceMetric::Euclidean => "Euclid",
        }
    }
}

fn default_distance_metric() -> DistanceMetric {
    DistanceMetric::Cosine
}

fn default_cache_ttl() -> u64 {
    3600 // 1 hour
}

fn default_max_cache_size() -> usize {
    1024 * 1024 // 1 MB
}

/// Components to include in the cache key.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CacheKeyComponents {
    /// Include model name in cache key.
    #[serde(default = "default_true")]
    pub model: bool,

    /// Include temperature in cache key.
    #[serde(default = "default_true")]
    pub temperature: bool,

    /// Include system prompt in cache key.
    #[serde(default = "default_true")]
    pub system_prompt: bool,

    /// Include tools in cache key.
    #[serde(default = "default_true")]
    pub tools: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Image Fetching
// ─────────────────────────────────────────────────────────────────────────────

/// HTTP image URL fetching configuration.
///
/// Non-OpenAI providers (Anthropic, Bedrock, Vertex) only support base64 data URLs
/// for images. When this feature is enabled, HTTP image URLs are automatically
/// fetched and converted to base64 data URLs before being sent to the provider.
///
/// # Example Configuration
///
/// ```toml
/// [features.image_fetching]
/// enabled = true
/// max_size_mb = 20
/// timeout_secs = 30
/// allowed_content_types = ["image/png", "image/jpeg", "image/gif", "image/webp"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ImageFetchingConfig {
    /// Enable HTTP image URL fetching.
    /// When disabled, HTTP image URLs will be passed through unchanged
    /// (and likely rejected by non-OpenAI providers).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum image size in megabytes.
    /// Images larger than this will not be fetched and will cause an error.
    #[serde(default = "default_image_max_size_mb")]
    pub max_size_mb: usize,

    /// Timeout for fetching images in seconds.
    #[serde(default = "default_image_timeout_secs")]
    pub timeout_secs: u64,

    /// Allowed MIME types for fetched images.
    /// Empty list means allow all image/* types.
    #[serde(default = "default_image_content_types")]
    pub allowed_content_types: Vec<String>,
}

impl Default for ImageFetchingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_size_mb: default_image_max_size_mb(),
            timeout_secs: default_image_timeout_secs(),
            allowed_content_types: default_image_content_types(),
        }
    }
}

impl ImageFetchingConfig {
    /// Convert to the runtime `ImageFetchConfig` used by providers.
    pub fn to_runtime_config(&self) -> crate::providers::image::ImageFetchConfig {
        crate::providers::image::ImageFetchConfig {
            enabled: self.enabled,
            max_size_bytes: self.max_size_mb * 1024 * 1024,
            timeout: std::time::Duration::from_secs(self.timeout_secs),
            allowed_content_types: self.allowed_content_types.clone(),
            // Per-provider; Anthropic's constructor sets this on its own copy.
            pass_through_https: false,
        }
    }
}

fn default_image_max_size_mb() -> usize {
    20
}

fn default_image_timeout_secs() -> u64 {
    30
}

fn default_image_content_types() -> Vec<String> {
    vec![
        "image/png".to_string(),
        "image/jpeg".to_string(),
        "image/gif".to_string(),
        "image/webp".to_string(),
    ]
}

fn default_true() -> bool {
    true
}

// ─────────────────────────────────────────────────────────────────────────────
// Web Search
// ─────────────────────────────────────────────────────────────────────────────

/// Web search configuration for backend-proxied search.
///
/// # Example Configuration
///
/// ```toml
/// [features.web_search]
/// provider = "tavily"
/// api_key = "${TAVILY_API_KEY}"
/// max_results = 10
/// timeout_secs = 30
/// ```
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct WebSearchConfig {
    /// Search provider backend.
    pub provider: WebSearchProvider,

    /// API key for the search provider. Supports `${ENV_VAR}` interpolation.
    #[serde(skip_serializing)]
    pub api_key: String,

    /// Maximum number of results to return per search.
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,

    /// Timeout in seconds for search requests.
    #[serde(default = "default_web_search_timeout_secs")]
    pub timeout_secs: u64,

    /// Cost per search request in microcents (1/1,000,000 of a dollar).
    /// Default: 10000 = $0.01
    #[serde(default = "default_web_search_cost")]
    pub cost_microcents_per_request: i64,

    /// Maximum web search tool call iterations before forcing text completion.
    /// Lower than file_search since web search rarely needs multiple rounds.
    #[serde(default = "default_web_search_max_iterations")]
    pub max_iterations: usize,

    /// Maximum characters of content text per search result.
    /// Applies to Exa's `text.maxCharacters` parameter. Tavily returns concise
    /// summaries by default so this is not needed there.
    /// Set to 0 to disable (return full text). Default: 2000.
    #[serde(default = "default_web_search_max_content_chars")]
    pub max_content_chars: usize,
}

impl std::fmt::Debug for WebSearchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSearchConfig")
            .field("provider", &self.provider)
            .field("api_key", &"****")
            .field("max_results", &self.max_results)
            .field("timeout_secs", &self.timeout_secs)
            .field(
                "cost_microcents_per_request",
                &self.cost_microcents_per_request,
            )
            .field("max_iterations", &self.max_iterations)
            .field("max_content_chars", &self.max_content_chars)
            .finish()
    }
}

/// Supported web search providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    Tavily,
    Exa,
}

fn default_web_search_max_results() -> usize {
    10
}

fn default_web_search_timeout_secs() -> u64 {
    30
}

fn default_web_search_cost() -> i64 {
    10000
}

fn default_web_search_max_iterations() -> usize {
    3
}

fn default_web_search_max_content_chars() -> usize {
    2000
}

// ─────────────────────────────────────────────────────────────────────────────
// Web Fetch
// ─────────────────────────────────────────────────────────────────────────────

/// Web fetch configuration for backend-proxied URL fetching.
///
/// # Example Configuration
///
/// ```toml
/// [features.web_fetch]
/// max_response_bytes = 1048576
/// timeout_secs = 30
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct WebFetchConfig {
    /// Enable web fetch tool.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum response body size in bytes.
    #[serde(default = "default_web_fetch_max_bytes")]
    pub max_response_bytes: usize,

    /// Timeout in seconds for fetch requests.
    #[serde(default = "default_web_fetch_timeout_secs")]
    pub timeout_secs: u64,

    /// Allowed response content types.
    #[serde(default = "default_web_fetch_content_types")]
    pub allowed_content_types: Vec<String>,

    /// Cost per fetch request in microcents (1/1,000,000 of a dollar).
    /// Default: 0 (free)
    #[serde(default)]
    pub cost_microcents_per_request: i64,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_response_bytes: default_web_fetch_max_bytes(),
            timeout_secs: default_web_fetch_timeout_secs(),
            allowed_content_types: default_web_fetch_content_types(),
            cost_microcents_per_request: 0,
        }
    }
}

fn default_web_fetch_max_bytes() -> usize {
    1_048_576 // 1 MB
}

fn default_web_fetch_timeout_secs() -> u64 {
    30
}

fn default_web_fetch_content_types() -> Vec<String> {
    vec![
        "text/html".to_string(),
        "text/plain".to_string(),
        "application/json".to_string(),
        "application/xml".to_string(),
        "text/xml".to_string(),
        "text/csv".to_string(),
        "text/markdown".to_string(),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// WebSocket
// ─────────────────────────────────────────────────────────────────────────────

/// WebSocket configuration for real-time event subscriptions.
///
/// When enabled, clients can connect to `/ws/events` to receive real-time
/// notifications about server events such as audit logs, usage tracking,
/// circuit breaker state changes, and budget alerts.
///
/// # Authentication
///
/// WebSocket connections can be authenticated via:
/// - Query parameter `token` - API key for programmatic access
/// - Session cookie - For browser-based access (requires prior OIDC login)
///
/// If `require_auth` is true, unauthenticated connections will be rejected.
///
/// # Example Configuration
///
/// ```toml
/// [features.websocket]
/// enabled = true
/// require_auth = true
/// ping_interval_secs = 30
/// pong_timeout_secs = 60
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct WebSocketConfig {
    /// Enable WebSocket event subscriptions.
    /// When disabled, the `/ws/events` endpoint is not registered.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Require authentication for WebSocket connections.
    /// When true, connections without valid API key or session are rejected.
    /// When false, unauthenticated connections are allowed (useful for development).
    #[serde(default)]
    pub require_auth: bool,

    /// Ping interval for keepalive in seconds.
    /// The server sends ping frames at this interval to detect dead connections.
    #[serde(default = "default_ws_ping_interval_secs")]
    pub ping_interval_secs: u64,

    /// Pong timeout in seconds.
    /// If no pong response is received within this time after a ping,
    /// the connection is terminated.
    #[serde(default = "default_ws_pong_timeout_secs")]
    pub pong_timeout_secs: u64,

    /// Maximum number of concurrent WebSocket connections.
    /// Set to 0 for unlimited connections (not recommended in production).
    #[serde(default = "default_ws_max_connections")]
    pub max_connections: usize,

    /// Event bus channel capacity.
    /// Determines how many events can be buffered before slow subscribers
    /// start missing events (lagging).
    #[serde(default = "default_ws_channel_capacity")]
    pub channel_capacity: usize,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_auth: false,
            ping_interval_secs: default_ws_ping_interval_secs(),
            pong_timeout_secs: default_ws_pong_timeout_secs(),
            max_connections: default_ws_max_connections(),
            channel_capacity: default_ws_channel_capacity(),
        }
    }
}

fn default_ws_ping_interval_secs() -> u64 {
    30
}

fn default_ws_pong_timeout_secs() -> u64 {
    60
}

fn default_ws_max_connections() -> usize {
    1000
}

fn default_ws_channel_capacity() -> usize {
    1024
}

// ─────────────────────────────────────────────────────────────────────────────
// Vector Store Cleanup
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the vector store cleanup background job.
///
/// The cleanup job periodically removes:
/// 1. Soft-deleted vector stores that have passed the cleanup delay
/// 2. Chunks associated with deleted stores from the vector database
/// 3. Files that are no longer referenced by any vector store
///
/// # Example Configuration
///
/// ```toml
/// [features.vector_store_cleanup]
/// enabled = true
/// interval_secs = 300
/// cleanup_delay_secs = 3600
/// batch_size = 100
/// max_duration_secs = 60
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct VectorStoreCleanupConfig {
    /// Enable the cleanup job.
    /// When disabled, soft-deleted stores remain in the database indefinitely.
    #[serde(default)]
    pub enabled: bool,

    /// How often to run the cleanup job (in seconds).
    /// Default: 300 (5 minutes)
    #[serde(default = "default_cleanup_interval_secs")]
    pub interval_secs: u64,

    /// Time to wait after soft deletion before hard deleting (in seconds).
    /// This gives users time to recover accidentally deleted stores.
    /// Default: 3600 (1 hour)
    #[serde(default = "default_cleanup_delay_secs")]
    pub cleanup_delay_secs: u64,

    /// Maximum number of stores to clean up per run.
    /// Prevents long-running cleanup operations.
    /// Default: 100
    #[serde(default = "default_cleanup_batch_size")]
    pub batch_size: u32,

    /// Maximum duration for a single cleanup run (in seconds).
    /// If exceeded, cleanup stops gracefully and continues next run.
    /// Set to 0 for unlimited.
    /// Default: 60
    #[serde(default = "default_cleanup_max_duration_secs")]
    pub max_duration_secs: u64,

    /// Dry run mode - log what would be deleted without actually deleting.
    /// Useful for testing cleanup configuration.
    /// Default: false
    #[serde(default)]
    pub dry_run: bool,
}

impl Default for VectorStoreCleanupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_cleanup_interval_secs(),
            cleanup_delay_secs: default_cleanup_delay_secs(),
            batch_size: default_cleanup_batch_size(),
            max_duration_secs: default_cleanup_max_duration_secs(),
            dry_run: false,
        }
    }
}

impl VectorStoreCleanupConfig {
    /// Get the interval as a Duration.
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_secs)
    }

    /// Get the cleanup delay as a Duration.
    pub fn cleanup_delay(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.cleanup_delay_secs)
    }

    /// Get the max duration as a Duration, or None if unlimited.
    pub fn max_duration(&self) -> Option<std::time::Duration> {
        if self.max_duration_secs == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(self.max_duration_secs))
        }
    }
}

fn default_cleanup_interval_secs() -> u64 {
    300 // 5 minutes
}

fn default_cleanup_delay_secs() -> u64 {
    3600 // 1 hour
}

fn default_cleanup_batch_size() -> u32 {
    100
}

fn default_cleanup_max_duration_secs() -> u64 {
    60
}

// ─────────────────────────────────────────────────────────────────────────────
// Model Catalog
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the models.dev model catalog.
///
/// The catalog provides per-model metadata including capabilities, pricing,
/// context limits, and modalities. Data is embedded at build time and
/// optionally synced at runtime via a background job.
///
/// # Example
///
/// ```toml
/// [features.model_catalog]
/// enabled = true
/// sync_interval_secs = 1800
/// api_url = "https://models.dev/api.json"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogConfig {
    /// Whether to enable runtime catalog sync.
    /// The embedded catalog is always loaded regardless of this setting.
    /// This only controls whether the background sync job runs.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Interval between sync attempts in seconds.
    #[serde(default = "default_catalog_sync_interval_secs")]
    pub sync_interval_secs: u64,

    /// URL to fetch the catalog from.
    #[serde(default = "default_catalog_api_url")]
    pub api_url: String,
}

impl Default for ModelCatalogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sync_interval_secs: default_catalog_sync_interval_secs(),
            api_url: default_catalog_api_url(),
        }
    }
}

fn default_catalog_sync_interval_secs() -> u64 {
    1800 // 30 minutes
}

fn default_catalog_api_url() -> String {
    "https://models.dev/api.json".to_string()
}

/// Configuration for the static models cache.
///
/// Model lists from config-file providers are cached in memory and refreshed
/// periodically so that `/v1/models` does not make upstream HTTP calls on every
/// request.
///
/// ```toml
/// [features.static_models_cache]
/// refresh_interval_secs = 300
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StaticModelsCacheConfig {
    /// How often to refresh the cached model lists, in seconds.
    /// Set to 0 to disable caching (every request will query providers directly).
    /// Default: 300 (5 minutes).
    #[serde(default = "default_static_models_refresh_interval_secs")]
    pub refresh_interval_secs: u64,
}

impl Default for StaticModelsCacheConfig {
    fn default() -> Self {
        Self {
            refresh_interval_secs: default_static_models_refresh_interval_secs(),
        }
    }
}

impl StaticModelsCacheConfig {
    /// Whether caching is enabled (interval > 0).
    pub fn enabled(&self) -> bool {
        self.refresh_interval_secs > 0
    }

    /// Refresh interval as a `Duration`.
    pub fn refresh_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.refresh_interval_secs)
    }
}

fn default_static_models_refresh_interval_secs() -> u64 {
    300 // 5 minutes
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────────────────────────────────────────────────────────────
    // Distance Metric Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_distance_metric_default() {
        let metric: DistanceMetric = Default::default();
        assert_eq!(metric, DistanceMetric::Cosine);
    }

    #[test]
    fn test_distance_metric_pgvector_ops_class() {
        assert_eq!(
            DistanceMetric::Cosine.pgvector_ops_class(),
            "vector_cosine_ops"
        );
        assert_eq!(
            DistanceMetric::DotProduct.pgvector_ops_class(),
            "vector_ip_ops"
        );
        assert_eq!(
            DistanceMetric::Euclidean.pgvector_ops_class(),
            "vector_l2_ops"
        );
    }

    #[test]
    fn test_distance_metric_pgvector_operator() {
        assert_eq!(DistanceMetric::Cosine.pgvector_operator(), "<=>");
        assert_eq!(DistanceMetric::DotProduct.pgvector_operator(), "<#>");
        assert_eq!(DistanceMetric::Euclidean.pgvector_operator(), "<->");
    }

    #[test]
    fn test_distance_metric_qdrant_distance() {
        assert_eq!(DistanceMetric::Cosine.qdrant_distance(), "Cosine");
        assert_eq!(DistanceMetric::DotProduct.qdrant_distance(), "Dot");
        assert_eq!(DistanceMetric::Euclidean.qdrant_distance(), "Euclid");
    }

    #[test]
    fn test_distance_metric_serialization() {
        assert_eq!(
            serde_json::to_string(&DistanceMetric::Cosine).unwrap(),
            "\"cosine\""
        );
        assert_eq!(
            serde_json::to_string(&DistanceMetric::DotProduct).unwrap(),
            "\"dot_product\""
        );
        assert_eq!(
            serde_json::to_string(&DistanceMetric::Euclidean).unwrap(),
            "\"euclidean\""
        );
    }

    #[test]
    fn test_distance_metric_deserialization() {
        assert_eq!(
            serde_json::from_str::<DistanceMetric>("\"cosine\"").unwrap(),
            DistanceMetric::Cosine
        );
        assert_eq!(
            serde_json::from_str::<DistanceMetric>("\"dot_product\"").unwrap(),
            DistanceMetric::DotProduct
        );
        assert_eq!(
            serde_json::from_str::<DistanceMetric>("\"euclidean\"").unwrap(),
            DistanceMetric::Euclidean
        );
    }

    #[test]
    fn test_rag_vector_backend_with_distance_metric() {
        let config: RagVectorBackend = toml::from_str(
            r#"
            type = "pgvector"
            table_name = "my_chunks"
            index_type = "hnsw"
            distance_metric = "euclidean"
            "#,
        )
        .unwrap();

        match config {
            RagVectorBackend::Pgvector {
                table_name,
                index_type,
                distance_metric,
            } => {
                assert_eq!(table_name, "my_chunks");
                assert!(matches!(index_type, PgvectorIndexType::Hnsw));
                assert_eq!(distance_metric, DistanceMetric::Euclidean);
            }
            _ => panic!("Expected Pgvector backend"),
        }
    }

    #[test]
    fn test_rag_vector_backend_distance_metric_default() {
        let config: RagVectorBackend = toml::from_str(
            r#"
            type = "pgvector"
            "#,
        )
        .unwrap();

        match config {
            RagVectorBackend::Pgvector {
                distance_metric, ..
            } => {
                assert_eq!(distance_metric, DistanceMetric::Cosine);
            }
            _ => panic!("Expected Pgvector backend"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Semantic Caching Config Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_semantic_caching_config_pgvector() {
        let config: SemanticCachingConfig = toml::from_str(
            r#"
            enabled = true
            similarity_threshold = 0.92

            [embedding]
            provider = "openai"
            model = "text-embedding-3-small"
            dimensions = 1536

            [vector_backend]
            type = "pgvector"
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert!((config.similarity_threshold - 0.92).abs() < 0.001);
        assert_eq!(config.top_k, 1); // default
        assert_eq!(config.embedding.provider, "openai");
        assert_eq!(config.embedding.model, "text-embedding-3-small");
        assert_eq!(config.embedding.dimensions, 1536);

        match config.vector_backend {
            SemanticVectorBackend::Pgvector {
                table_name,
                index_type,
                ..
            } => {
                assert_eq!(table_name, "semantic_cache_embeddings"); // default
                assert!(matches!(index_type, PgvectorIndexType::IvfFlat)); // default
            }
            _ => panic!("Expected Pgvector backend"),
        }
    }

    #[test]
    fn test_semantic_caching_config_qdrant() {
        let config: SemanticCachingConfig = toml::from_str(
            r#"
            enabled = true
            similarity_threshold = 0.95
            top_k = 3

            [embedding]
            provider = "openai"
            model = "text-embedding-3-large"
            dimensions = 3072

            [vector_backend]
            type = "qdrant"
            url = "http://localhost:6333"
            api_key = "secret"
            qdrant_collection_name = "my_cache"
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert!((config.similarity_threshold - 0.95).abs() < 0.001);
        assert_eq!(config.top_k, 3);

        match config.vector_backend {
            SemanticVectorBackend::Qdrant {
                url,
                api_key,
                qdrant_collection_name,
                ..
            } => {
                assert_eq!(url, "http://localhost:6333");
                assert_eq!(api_key, Some("secret".to_string()));
                assert_eq!(qdrant_collection_name, "my_cache");
            }
            _ => panic!("Expected Qdrant backend"),
        }
    }

    #[test]
    fn test_semantic_caching_config_with_hnsw_index() {
        let config: SemanticCachingConfig = toml::from_str(
            r#"
            enabled = true

            [vector_backend]
            type = "pgvector"
            table_name = "custom_table"
            index_type = "hnsw"
            "#,
        )
        .unwrap();

        match config.vector_backend {
            SemanticVectorBackend::Pgvector {
                table_name,
                index_type,
                ..
            } => {
                assert_eq!(table_name, "custom_table");
                assert!(matches!(index_type, PgvectorIndexType::Hnsw));
            }
            _ => panic!("Expected Pgvector backend"),
        }
    }

    #[test]
    fn test_response_caching_with_semantic() {
        let config: ResponseCachingConfig = toml::from_str(
            r#"
            enabled = true
            ttl_secs = 7200
            only_deterministic = false

            [semantic]
            enabled = true
            similarity_threshold = 0.93

            [semantic.vector_backend]
            type = "pgvector"
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.ttl_secs, 7200);
        assert!(!config.only_deterministic);

        let semantic = config.semantic.unwrap();
        assert!(semantic.enabled);
        assert!((semantic.similarity_threshold - 0.93).abs() < 0.001);
    }

    #[test]
    fn test_response_caching_without_semantic() {
        let config: ResponseCachingConfig = toml::from_str(
            r#"
            enabled = true
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert!(config.semantic.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Guardrails Configuration Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_guardrails_config_openai_moderation() {
        let config: GuardrailsConfig = toml::from_str(
            r#"
            enabled = true

            [input]
            enabled = true
            mode = "blocking"

            [input.provider]
            type = "openai_moderation"
            api_key = "sk-test"

            [input.actions]
            hate = "block"
            violence = "warn"
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        let input = config.input.unwrap();
        assert!(input.enabled);
        assert_eq!(input.mode, GuardrailsExecutionMode::Blocking);
        assert_eq!(input.timeout_ms, 5000); // default

        match input.provider {
            GuardrailsProvider::OpenaiModeration {
                api_key,
                base_url,
                model,
            } => {
                assert_eq!(api_key, Some("sk-test".to_string()));
                assert_eq!(base_url, "https://api.openai.com/v1");
                assert_eq!(model, "text-moderation-latest");
            }
            _ => panic!("Expected OpenAI Moderation provider"),
        }

        assert_eq!(input.actions.get("hate"), Some(&GuardrailsAction::Block));
        assert_eq!(input.actions.get("violence"), Some(&GuardrailsAction::Warn));
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_guardrails_config_bedrock() {
        let config: GuardrailsConfig = toml::from_str(
            r#"
            enabled = true

            [input]
            enabled = true
            mode = "concurrent"
            timeout_ms = 3000
            on_timeout = "allow"
            on_error = "log_and_allow"

            [input.provider]
            type = "bedrock"
            guardrail_id = "abc123"
            guardrail_version = "1"
            region = "us-east-1"
            trace_enabled = true

            [input.actions]
            HATE = "block"
            PROMPT_ATTACK = "block"
            VIOLENCE = { redact = { replacement = "[removed]" } }
            "#,
        )
        .unwrap();

        let input = config.input.unwrap();
        assert_eq!(input.mode, GuardrailsExecutionMode::Concurrent);
        assert_eq!(input.timeout_ms, 3000);
        assert_eq!(input.on_timeout, GuardrailsTimeoutAction::Allow);
        assert_eq!(input.on_error, GuardrailsErrorAction::LogAndAllow);

        match input.provider {
            GuardrailsProvider::Bedrock {
                guardrail_id,
                guardrail_version,
                region,
                trace_enabled,
                ..
            } => {
                assert_eq!(guardrail_id, "abc123");
                assert_eq!(guardrail_version, "1");
                assert_eq!(region, Some("us-east-1".to_string()));
                assert!(trace_enabled);
            }
            _ => panic!("Expected Bedrock provider"),
        }

        assert_eq!(
            input.actions.get("VIOLENCE"),
            Some(&GuardrailsAction::Redact {
                replacement: "[removed]".to_string()
            })
        );
    }

    #[test]
    fn test_guardrails_config_azure_content_safety() {
        let config: GuardrailsConfig = toml::from_str(
            r#"
            enabled = true

            [output]
            enabled = true

            [output.provider]
            type = "azure_content_safety"
            endpoint = "https://my-service.cognitiveservices.azure.com"
            api_key = "azure-key"

            [output.provider.thresholds]
            Hate = 2
            Violence = 4

            [output.actions]
            Hate = "block"
            "#,
        )
        .unwrap();

        let output = config.output.unwrap();
        assert!(output.enabled);

        match output.provider {
            GuardrailsProvider::AzureContentSafety {
                endpoint,
                api_key,
                api_version,
                thresholds,
                ..
            } => {
                assert_eq!(endpoint, "https://my-service.cognitiveservices.azure.com");
                assert_eq!(api_key, "azure-key");
                assert_eq!(api_version, "2024-09-01");
                assert_eq!(thresholds.get("Hate"), Some(&2));
                assert_eq!(thresholds.get("Violence"), Some(&4));
            }
            _ => panic!("Expected Azure Content Safety provider"),
        }
    }

    #[test]
    fn test_guardrails_config_custom_provider() {
        let config: GuardrailsConfig = toml::from_str(
            r#"
            enabled = true

            [custom]
            enabled = true
            apply_to = "both"
            timeout_ms = 2000
            on_error = "allow"

            [custom.provider]
            url = "https://my-guardrails.example.com/evaluate"
            api_key = "custom-key"
            retry_enabled = true
            max_retries = 3

            [custom.provider.headers]
            X-Custom-Header = "value"
            "#,
        )
        .unwrap();

        let custom = config.custom.unwrap();
        assert!(custom.enabled);
        assert_eq!(custom.apply_to, GuardrailsApplyTo::Both);
        assert_eq!(custom.timeout_ms, 2000);
        assert_eq!(custom.on_error, GuardrailsErrorAction::Allow);

        assert_eq!(
            custom.provider.url,
            "https://my-guardrails.example.com/evaluate"
        );
        assert_eq!(custom.provider.api_key, Some("custom-key".to_string()));
        assert!(custom.provider.retry_enabled);
        assert_eq!(custom.provider.max_retries, 3);
        assert_eq!(
            custom.provider.headers.get("X-Custom-Header"),
            Some(&"value".to_string())
        );
    }

    #[test]
    fn test_guardrails_config_pii() {
        let config: GuardrailsConfig = toml::from_str(
            r#"
            enabled = true

            [pii]
            enabled = true
            types = ["EMAIL", "PHONE", "SSN", "CREDIT_CARD", "ADDRESS"]
            action = "redact"
            replacement = "[PERSONAL INFO]"
            apply_to = "input"
            "#,
        )
        .unwrap();

        let pii = config.pii.unwrap();
        assert!(pii.enabled);
        assert_eq!(pii.types.len(), 5);
        assert!(pii.types.contains(&PiiType::Email));
        assert!(pii.types.contains(&PiiType::Address));
        assert_eq!(pii.action, PiiAction::Redact);
        assert_eq!(pii.replacement, "[PERSONAL INFO]");
        assert_eq!(pii.apply_to, PiiApplyTo::Input);
    }

    #[test]
    fn test_guardrails_config_pii_with_bedrock_provider() {
        let config: GuardrailsConfig = toml::from_str(
            r#"
            enabled = true

            [pii]
            enabled = true
            action = "anonymize"

            [pii.provider]
            type = "bedrock"
            guardrail_id = "pii-guard-123"
            guardrail_version = "2"
            region = "us-west-2"
            "#,
        )
        .unwrap();

        let pii = config.pii.unwrap();
        assert_eq!(pii.action, PiiAction::Anonymize);

        match pii.provider.unwrap() {
            PiiProvider::Bedrock {
                guardrail_id,
                guardrail_version,
                region,
            } => {
                assert_eq!(guardrail_id, "pii-guard-123");
                assert_eq!(guardrail_version, "2");
                assert_eq!(region, Some("us-west-2".to_string()));
            }
            _ => panic!("Expected Bedrock PII provider"),
        }
    }

    #[test]
    fn test_guardrails_config_output_streaming_modes() {
        // Test default (buffered with default buffer tokens)
        let config: OutputGuardrailsConfig = toml::from_str(
            r#"
            enabled = true
            [provider]
            type = "openai_moderation"
            "#,
        )
        .unwrap();
        assert!(matches!(
            config.streaming_mode,
            StreamingGuardrailsMode::Buffered { buffer_tokens: 100 }
        ));

        // Test buffered mode
        let config: OutputGuardrailsConfig = toml::from_str(
            r#"
            enabled = true
            streaming_mode = { buffered = { buffer_tokens = 50 } }
            [provider]
            type = "openai_moderation"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.streaming_mode,
            StreamingGuardrailsMode::Buffered { buffer_tokens: 50 }
        );

        // Test per_chunk mode
        let config: OutputGuardrailsConfig = toml::from_str(
            r#"
            enabled = true
            streaming_mode = "per_chunk"
            [provider]
            type = "openai_moderation"
            "#,
        )
        .unwrap();
        assert_eq!(config.streaming_mode, StreamingGuardrailsMode::PerChunk);
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_guardrails_config_full_example() {
        // Test the full config example from the documentation
        let config: GuardrailsConfig = toml::from_str(
            r#"
            enabled = true

            [input]
            enabled = true
            mode = "blocking"

            [input.provider]
            type = "bedrock"
            guardrail_id = "abc123"
            guardrail_version = "1"

            [input.actions]
            HATE = "block"
            PROMPT_ATTACK = "block"
            VIOLENCE = "warn"

            [output]
            enabled = true

            [output.provider]
            type = "openai_moderation"

            [pii]
            enabled = true
            action = "redact"
            types = ["EMAIL", "PHONE", "SSN"]
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert!(config.input.is_some());
        assert!(config.output.is_some());
        assert!(config.pii.is_some());
    }

    #[test]
    fn test_guardrails_action_redact_with_default_replacement() {
        let action: GuardrailsAction = toml::from_str(
            r#"
            redact = {}
            "#,
        )
        .unwrap();

        match action {
            GuardrailsAction::Redact { replacement } => {
                assert_eq!(replacement, "[REDACTED]");
            }
            _ => panic!("Expected Redact action"),
        }
    }

    #[test]
    fn test_guardrails_defaults() {
        let config: InputGuardrailsConfig = toml::from_str(
            r#"
            [provider]
            type = "openai_moderation"
            "#,
        )
        .unwrap();

        assert!(config.enabled); // default true
        assert_eq!(config.mode, GuardrailsExecutionMode::Blocking);
        assert_eq!(config.timeout_ms, 5000);
        assert_eq!(config.on_timeout, GuardrailsTimeoutAction::Block);
        assert_eq!(config.on_error, GuardrailsErrorAction::Block);
        assert_eq!(config.default_action, GuardrailsAction::Block);
    }

    // ───────────────────────────────────────────────────────────────────────────
    // Image Fetching Config Tests
    // ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_image_fetching_config_defaults() {
        let config: ImageFetchingConfig = toml::from_str("").unwrap();

        assert!(config.enabled);
        assert_eq!(config.max_size_mb, 20);
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(
            config.allowed_content_types,
            vec![
                "image/png".to_string(),
                "image/jpeg".to_string(),
                "image/gif".to_string(),
                "image/webp".to_string()
            ]
        );
    }

    #[test]
    fn test_image_fetching_config_custom_values() {
        let config: ImageFetchingConfig = toml::from_str(
            r#"
            enabled = false
            max_size_mb = 50
            timeout_secs = 120
            allowed_content_types = ["image/png", "image/jpeg"]
            "#,
        )
        .unwrap();

        assert!(!config.enabled);
        assert_eq!(config.max_size_mb, 50);
        assert_eq!(config.timeout_secs, 120);
        assert_eq!(
            config.allowed_content_types,
            vec!["image/png".to_string(), "image/jpeg".to_string()]
        );
    }

    #[test]
    fn test_image_fetching_config_to_runtime() {
        let config = ImageFetchingConfig {
            enabled: true,
            max_size_mb: 10,
            timeout_secs: 30,
            allowed_content_types: vec!["image/png".to_string()],
        };

        let runtime = config.to_runtime_config();
        assert!(runtime.enabled);
        assert_eq!(runtime.max_size_bytes, 10 * 1024 * 1024);
        assert_eq!(runtime.timeout, std::time::Duration::from_secs(30));
        assert_eq!(runtime.allowed_content_types, vec!["image/png".to_string()]);
    }

    #[test]
    fn test_features_config_with_image_fetching() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [image_fetching]
            enabled = true
            max_size_mb = 25
            timeout_secs = 45
            "#,
        )
        .unwrap();

        assert!(config.image_fetching.enabled);
        assert_eq!(config.image_fetching.max_size_mb, 25);
        assert_eq!(config.image_fetching.timeout_secs, 45);
    }

    // ───────────────────────────────────────────────────────────────────────────
    // File Search Config Tests
    // ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_file_search_config_defaults() {
        let config: FileSearchConfig = toml::from_str("").unwrap();

        assert!(config.enabled);
        assert_eq!(config.max_iterations, 5);
        assert_eq!(config.max_results_per_search, 10);
        assert_eq!(config.timeout_secs, 30);
        assert!(config.include_annotations);
        assert!((config.score_threshold - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_file_search_config_custom_values() {
        let config: FileSearchConfig = toml::from_str(
            r#"
            enabled = true
            max_iterations = 3
            max_results_per_search = 20
            timeout_secs = 60
            include_annotations = false
            score_threshold = 0.8
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.max_iterations, 3);
        assert_eq!(config.max_results_per_search, 20);
        assert_eq!(config.timeout_secs, 60);
        assert!(!config.include_annotations);
        assert!((config.score_threshold - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_file_search_config_disabled() {
        let config: FileSearchConfig = toml::from_str(
            r#"
            enabled = false
            "#,
        )
        .unwrap();

        assert!(!config.enabled);
        // Defaults should still apply
        assert_eq!(config.max_iterations, 5);
    }

    #[test]
    fn test_file_search_config_validate_score_threshold_valid() {
        let config: FileSearchConfig = toml::from_str(
            r#"
            score_threshold = 0.5
            "#,
        )
        .unwrap();
        assert!(config.validate().is_ok());

        // Test boundary values
        let config_min: FileSearchConfig = toml::from_str(
            r#"
            score_threshold = 0.0
            "#,
        )
        .unwrap();
        assert!(config_min.validate().is_ok());

        let config_max: FileSearchConfig = toml::from_str(
            r#"
            score_threshold = 1.0
            "#,
        )
        .unwrap();
        assert!(config_max.validate().is_ok());
    }

    #[test]
    fn test_file_search_config_validate_score_threshold_invalid() {
        let config: FileSearchConfig = toml::from_str(
            r#"
            score_threshold = 1.5
            "#,
        )
        .unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("score_threshold must be between 0.0 and 1.0")
        );

        let config_negative: FileSearchConfig = toml::from_str(
            r#"
            score_threshold = -0.1
            "#,
        )
        .unwrap();
        assert!(config_negative.validate().is_err());
    }

    #[test]
    fn test_features_config_validate_with_invalid_file_search() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [file_search]
            score_threshold = 2.0
            "#,
        )
        .unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("score_threshold"));
    }

    #[test]
    fn test_features_config_with_file_search() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [file_search]
            enabled = true
            max_iterations = 10
            score_threshold = 0.9
            "#,
        )
        .unwrap();

        let fs = config.file_search.expect("file_search should be set");
        assert!(fs.enabled);
        assert_eq!(fs.max_iterations, 10);
        assert!((fs.score_threshold - 0.9).abs() < 0.001);
    }

    // ───────────────────────────────────────────────────────────────────────────
    // RerankConfig Tests
    // ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_rerank_config_defaults() {
        let config: RerankConfig = toml::from_str("").unwrap();

        assert!(!config.enabled);
        assert!(config.model.is_none());
        assert_eq!(config.max_results_to_rerank, 20);
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.timeout_secs, 30);
    }

    #[test]
    fn test_rerank_config_custom_values() {
        let config: RerankConfig = toml::from_str(
            r#"
            enabled = true
            model = "gpt-4o-mini"
            max_results_to_rerank = 50
            batch_size = 25
            timeout_secs = 60
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.model, Some("gpt-4o-mini".to_string()));
        assert_eq!(config.max_results_to_rerank, 50);
        assert_eq!(config.batch_size, 25);
        assert_eq!(config.timeout_secs, 60);
    }

    #[test]
    fn test_rerank_config_partial() {
        let config: RerankConfig = toml::from_str(
            r#"
            enabled = true
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert!(config.model.is_none());
        assert_eq!(config.max_results_to_rerank, 20);
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.timeout_secs, 30);
    }

    #[test]
    fn test_rerank_config_validate_success() {
        let config = RerankConfig::default();
        assert!(config.validate().is_ok());

        let config: RerankConfig = toml::from_str(
            r#"
            enabled = true
            max_results_to_rerank = 100
            batch_size = 50
            "#,
        )
        .unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_rerank_config_validate_zero_max_results() {
        let config: RerankConfig = toml::from_str(
            r#"
            max_results_to_rerank = 0
            "#,
        )
        .unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max_results_to_rerank"));
    }

    #[test]
    fn test_rerank_config_validate_zero_batch_size() {
        let config: RerankConfig = toml::from_str(
            r#"
            batch_size = 0
            "#,
        )
        .unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("batch_size"));
    }

    #[test]
    fn test_rerank_config_validate_batch_exceeds_max() {
        let config: RerankConfig = toml::from_str(
            r#"
            max_results_to_rerank = 10
            batch_size = 20
            "#,
        )
        .unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("should not exceed"));
    }

    #[test]
    fn test_rerank_config_validate_zero_timeout() {
        let config: RerankConfig = toml::from_str(
            r#"
            timeout_secs = 0
            "#,
        )
        .unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("timeout_secs"));
    }

    #[test]
    fn test_file_search_config_with_rerank() {
        let config: FileSearchConfig = toml::from_str(
            r#"
            [rerank]
            enabled = true
            model = "claude-3-haiku"
            max_results_to_rerank = 30
            "#,
        )
        .unwrap();

        assert!(config.rerank.enabled);
        assert_eq!(config.rerank.model, Some("claude-3-haiku".to_string()));
        assert_eq!(config.rerank.max_results_to_rerank, 30);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_file_search_config_validate_with_invalid_rerank() {
        let config: FileSearchConfig = toml::from_str(
            r#"
            [rerank]
            enabled = true
            max_results_to_rerank = 0
            "#,
        )
        .unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max_results_to_rerank"));
    }

    #[test]
    fn test_features_config_with_file_search_rerank() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [file_search]
            enabled = true

            [file_search.rerank]
            enabled = true
            model = "gpt-4o-mini"
            "#,
        )
        .unwrap();

        let fs = config.file_search.expect("file_search should be set");
        assert!(fs.enabled);
        assert!(fs.rerank.enabled);
        assert_eq!(fs.rerank.model, Some("gpt-4o-mini".to_string()));
    }

    // ───────────────────────────────────────────────────────────────────────────
    // WebSocket Config Tests
    // ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_websocket_config_defaults() {
        let config: WebSocketConfig = toml::from_str("").unwrap();

        assert!(config.enabled);
        assert!(!config.require_auth);
        assert_eq!(config.ping_interval_secs, 30);
        assert_eq!(config.pong_timeout_secs, 60);
        assert_eq!(config.max_connections, 1000);
        assert_eq!(config.channel_capacity, 1024);
    }

    #[test]
    fn test_websocket_config_custom_values() {
        let config: WebSocketConfig = toml::from_str(
            r#"
            enabled = false
            require_auth = true
            ping_interval_secs = 15
            pong_timeout_secs = 30
            max_connections = 500
            channel_capacity = 2048
            "#,
        )
        .unwrap();

        assert!(!config.enabled);
        assert!(config.require_auth);
        assert_eq!(config.ping_interval_secs, 15);
        assert_eq!(config.pong_timeout_secs, 30);
        assert_eq!(config.max_connections, 500);
        assert_eq!(config.channel_capacity, 2048);
    }

    #[test]
    fn test_websocket_config_partial() {
        let config: WebSocketConfig = toml::from_str(
            r#"
            require_auth = true
            "#,
        )
        .unwrap();

        // Only require_auth was set, rest should be defaults
        assert!(config.enabled);
        assert!(config.require_auth);
        assert_eq!(config.ping_interval_secs, 30);
        assert_eq!(config.pong_timeout_secs, 60);
    }

    #[test]
    fn test_features_config_with_websocket() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [websocket]
            enabled = true
            require_auth = true
            ping_interval_secs = 20
            "#,
        )
        .unwrap();

        assert!(config.websocket.enabled);
        assert!(config.websocket.require_auth);
        assert_eq!(config.websocket.ping_interval_secs, 20);
        // Defaults for unset values
        assert_eq!(config.websocket.pong_timeout_secs, 60);
    }

    #[test]
    fn test_websocket_config_disabled() {
        let config: WebSocketConfig = toml::from_str(
            r#"
            enabled = false
            "#,
        )
        .unwrap();

        assert!(!config.enabled);
        // Other values should still have defaults
        assert!(!config.require_auth);
        assert_eq!(config.ping_interval_secs, 30);
    }

    // ───────────────────────────────────────────────────────────────────────────
    // Vector Store Cleanup Config Tests
    // ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_vector_store_cleanup_config_defaults() {
        let config: VectorStoreCleanupConfig = toml::from_str("").unwrap();

        assert!(!config.enabled);
        assert_eq!(config.interval_secs, 300);
        assert_eq!(config.cleanup_delay_secs, 3600);
        assert_eq!(config.batch_size, 100);
        assert_eq!(config.max_duration_secs, 60);
        assert!(!config.dry_run);
    }

    #[test]
    fn test_vector_store_cleanup_config_custom_values() {
        let config: VectorStoreCleanupConfig = toml::from_str(
            r#"
            enabled = true
            interval_secs = 600
            cleanup_delay_secs = 7200
            batch_size = 50
            max_duration_secs = 120
            dry_run = true
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.interval_secs, 600);
        assert_eq!(config.cleanup_delay_secs, 7200);
        assert_eq!(config.batch_size, 50);
        assert_eq!(config.max_duration_secs, 120);
        assert!(config.dry_run);
    }

    #[test]
    fn test_vector_store_cleanup_config_durations() {
        let config = VectorStoreCleanupConfig {
            enabled: true,
            interval_secs: 300,
            cleanup_delay_secs: 3600,
            batch_size: 100,
            max_duration_secs: 60,
            dry_run: false,
        };

        assert_eq!(config.interval(), std::time::Duration::from_secs(300));
        assert_eq!(config.cleanup_delay(), std::time::Duration::from_secs(3600));
        assert_eq!(
            config.max_duration(),
            Some(std::time::Duration::from_secs(60))
        );
    }

    #[test]
    fn test_vector_store_cleanup_config_unlimited_duration() {
        let config = VectorStoreCleanupConfig {
            max_duration_secs: 0,
            ..Default::default()
        };

        assert!(config.max_duration().is_none());
    }

    #[test]
    fn test_features_config_with_vector_store_cleanup() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [vector_store_cleanup]
            enabled = true
            interval_secs = 180
            cleanup_delay_secs = 1800
            "#,
        )
        .unwrap();

        assert!(config.vector_store_cleanup.enabled);
        assert_eq!(config.vector_store_cleanup.interval_secs, 180);
        assert_eq!(config.vector_store_cleanup.cleanup_delay_secs, 1800);
        // Defaults for unset values
        assert_eq!(config.vector_store_cleanup.batch_size, 100);
        assert_eq!(config.vector_store_cleanup.max_duration_secs, 60);
    }

    // ───────────────────────────────────────────────────────────────────────────
    // File Processing Config Tests
    // ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_file_processing_config_defaults() {
        let config: FileProcessingConfig = toml::from_str("").unwrap();

        assert_eq!(config.mode, FileProcessingMode::Inline);
        assert_eq!(config.max_file_size_mb, 10);
        assert_eq!(config.max_concurrent_tasks, 4);
        assert_eq!(config.default_max_chunk_tokens, 800);
        assert_eq!(config.default_overlap_tokens, 200);
        assert!(config.queue.is_none());
        assert!(config.callback_url.is_none());
    }

    #[test]
    fn test_file_processing_config_inline_mode() {
        let config: FileProcessingConfig = toml::from_str(
            r#"
            mode = "inline"
            max_file_size_mb = 20
            max_concurrent_tasks = 8
            default_max_chunk_tokens = 1000
            default_overlap_tokens = 100
            "#,
        )
        .unwrap();

        assert_eq!(config.mode, FileProcessingMode::Inline);
        assert_eq!(config.max_file_size_mb, 20);
        assert_eq!(config.max_concurrent_tasks, 8);
        assert_eq!(config.default_max_chunk_tokens, 1000);
        assert_eq!(config.default_overlap_tokens, 100);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_file_processing_config_queue_mode_redis() {
        let config: FileProcessingConfig = toml::from_str(
            r#"
            mode = "queue"

            [queue]
            backend = "redis"
            url = "redis://localhost:6379"
            queue_name = "my_processing_queue"
            consumer_group = "my_workers"
            "#,
        )
        .unwrap();

        assert_eq!(config.mode, FileProcessingMode::Queue);
        assert!(config.validate().is_ok());

        let queue = config.queue.unwrap();
        assert_eq!(queue.backend, FileProcessingQueueBackend::Redis);
        assert_eq!(queue.url, "redis://localhost:6379");
        assert_eq!(queue.queue_name, "my_processing_queue");
        assert_eq!(queue.consumer_group, "my_workers");
    }

    #[test]
    fn test_file_processing_config_queue_mode_missing_config() {
        let config: FileProcessingConfig = toml::from_str(
            r#"
            mode = "queue"
            "#,
        )
        .unwrap();

        assert_eq!(config.mode, FileProcessingMode::Queue);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_file_processing_config_max_size_bytes() {
        let config = FileProcessingConfig {
            max_file_size_mb: 25,
            ..Default::default()
        };

        assert_eq!(config.max_file_size_bytes(), 25 * 1024 * 1024);
    }

    #[test]
    fn test_file_processing_config_with_callback() {
        let config: FileProcessingConfig = toml::from_str(
            r#"
            mode = "queue"
            callback_url = "https://my-service.example.com/callback"

            [queue]
            backend = "redis"
            url = "redis://localhost:6379"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.callback_url,
            Some("https://my-service.example.com/callback".to_string())
        );
    }

    #[test]
    fn test_features_config_with_file_processing() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [file_processing]
            mode = "inline"
            max_file_size_mb = 15
            max_concurrent_tasks = 2
            "#,
        )
        .unwrap();

        assert_eq!(config.file_processing.mode, FileProcessingMode::Inline);
        assert_eq!(config.file_processing.max_file_size_mb, 15);
        assert_eq!(config.file_processing.max_concurrent_tasks, 2);
        // Defaults for unset values
        assert_eq!(config.file_processing.default_max_chunk_tokens, 800);
        assert_eq!(config.file_processing.default_overlap_tokens, 200);
    }

    #[test]
    fn test_features_config_with_file_processing_queue() {
        let config: FeaturesConfig = toml::from_str(
            r#"
            [file_processing]
            mode = "queue"

            [file_processing.queue]
            backend = "redis"
            url = "redis://localhost:6379"
            "#,
        )
        .unwrap();

        assert_eq!(config.file_processing.mode, FileProcessingMode::Queue);
        assert!(config.file_processing.queue.is_some());
        assert!(config.file_processing.validate().is_ok());
    }

    // ───────────────────────────────────────────────────────────────────────────
    // Virus Scan Config Tests
    // ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_virus_scan_config_defaults() {
        let config: VirusScanConfig = toml::from_str("").unwrap();

        assert!(!config.enabled);
        assert_eq!(config.backend, VirusScanBackend::ClamAv);
        assert!(config.clamav.is_none());
    }

    #[test]
    fn test_virus_scan_config_disabled_validates() {
        let config: VirusScanConfig = toml::from_str(
            r#"
            enabled = false
            "#,
        )
        .unwrap();

        // Disabled config should validate even without clamav section
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_virus_scan_config_enabled_requires_clamav() {
        let config: VirusScanConfig = toml::from_str(
            r#"
            enabled = true
            backend = "clamav"
            "#,
        )
        .unwrap();

        // Enabled without clamav config should fail validation
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_virus_scan_config_enabled_with_clamav() {
        let config: VirusScanConfig = toml::from_str(
            r#"
            enabled = true
            backend = "clamav"

            [clamav]
            host = "scanner.local"
            port = 3311
            timeout_ms = 60000
            max_file_size_mb = 50
            "#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.backend, VirusScanBackend::ClamAv);
        assert!(config.validate().is_ok());

        let clamav = config.clamav.unwrap();
        assert_eq!(clamav.host, "scanner.local");
        assert_eq!(clamav.port, 3311);
        assert_eq!(clamav.timeout_ms, 60000);
        assert_eq!(clamav.max_file_size_mb, 50);
        assert!(clamav.socket_path.is_none());
    }

    #[test]
    fn test_virus_scan_config_with_socket() {
        let config: VirusScanConfig = toml::from_str(
            r#"
            enabled = true

            [clamav]
            socket_path = "/var/run/clamav/clamd.sock"
            "#,
        )
        .unwrap();

        assert!(config.validate().is_ok());

        let clamav = config.clamav.unwrap();
        assert_eq!(
            clamav.socket_path,
            Some("/var/run/clamav/clamd.sock".to_string())
        );
    }

    #[test]
    fn test_clamav_config_defaults() {
        let config: ClamAvConfig = toml::from_str("").unwrap();

        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 3310);
        assert_eq!(config.timeout_ms, 30000);
        assert_eq!(config.max_file_size_mb, 25);
        assert!(config.socket_path.is_none());
    }

    #[test]
    fn test_clamav_config_tcp_address() {
        let config = ClamAvConfig {
            host: "scanner.example.com".to_string(),
            port: 3312,
            ..Default::default()
        };

        assert_eq!(config.tcp_address(), "scanner.example.com:3312");
    }

    #[test]
    fn test_clamav_config_max_size_bytes() {
        let config = ClamAvConfig {
            max_file_size_mb: 100,
            ..Default::default()
        };

        assert_eq!(config.max_file_size_bytes(), 100 * 1024 * 1024);
    }

    #[test]
    fn test_clamav_config_validation_empty_host() {
        let config = ClamAvConfig {
            host: "".to_string(),
            socket_path: None,
            ..Default::default()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_clamav_config_validation_zero_timeout() {
        let config = ClamAvConfig {
            timeout_ms: 0,
            ..Default::default()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_file_processing_with_virus_scan() {
        let config: FileProcessingConfig = toml::from_str(
            r#"
            mode = "inline"
            max_file_size_mb = 20

            [virus_scan]
            enabled = true

            [virus_scan.clamav]
            host = "clamav.local"
            port = 3310
            "#,
        )
        .unwrap();

        assert!(config.validate().is_ok());
        assert!(config.virus_scan.enabled);
        assert!(config.virus_scan.clamav.is_some());
    }
}
