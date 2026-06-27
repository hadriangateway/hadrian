use std::{net::IpAddr, time::Duration};

use http::{HeaderName, Method};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

/// HTTP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Host address to bind to.
    #[serde(default = "default_host")]
    pub host: IpAddr,

    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Request body size limit in bytes (the *global* cap, applied to every
    /// request that doesn't have a more specific override). The audio and file
    /// upload routes get a higher per-route limit because their payloads are
    /// inherently larger than chat completions.
    #[serde(default = "default_body_limit")]
    pub body_limit_bytes: usize,

    /// Request body size limit in bytes for audio routes
    /// (`/v1/audio/transcriptions`, `/v1/audio/translations`).
    /// Whisper-style transcription requests can carry tens of megabytes of
    /// audio. Defaults to 100 MB.
    #[serde(default = "default_audio_body_limit")]
    pub audio_body_limit_bytes: usize,

    /// Request body size limit in bytes for `/v1/files` uploads.
    /// Defaults to 512 MB so multi-document RAG ingest works without manual
    /// tuning. Operators that don't use file uploads should drop this.
    #[serde(default = "default_files_body_limit")]
    pub files_body_limit_bytes: usize,

    /// Maximum request body size for skill uploads (`POST /v1/skills` and
    /// version creates) in bytes. Covers zip bundles / multipart directories.
    /// Defaults to 64 MB.
    #[serde(default = "default_skills_body_limit")]
    pub skills_body_limit_bytes: usize,

    /// Maximum response body size for buffering provider responses (in bytes).
    /// This prevents OOM from malicious or malformed provider responses.
    #[serde(default = "default_max_response_body")]
    pub max_response_body_bytes: usize,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Streaming response idle timeout in seconds.
    ///
    /// This is the maximum time allowed between chunks in a streaming response.
    /// If no chunk is received from the upstream provider within this timeout,
    /// the stream is terminated.
    ///
    /// This protects against:
    /// - Stalled upstream providers that stop sending data
    /// - Connection pool exhaustion from hung streams
    ///
    /// Set to 0 to disable idle timeout (not recommended).
    /// Default: 120 seconds (2 minutes)
    #[serde(default = "default_streaming_idle_timeout")]
    pub streaming_idle_timeout_secs: u64,

    /// TLS configuration. If omitted, serves plain HTTP.
    /// In production, TLS is typically terminated at the load balancer.
    #[serde(default)]
    pub tls: Option<TlsConfig>,

    /// Trusted proxy configuration for extracting real client IPs.
    #[serde(default)]
    pub trusted_proxies: TrustedProxiesConfig,

    /// CORS configuration.
    #[serde(default)]
    pub cors: CorsConfig,

    /// Security headers configuration.
    #[serde(default)]
    pub security_headers: SecurityHeadersConfig,

    /// HTTP client configuration for outbound requests to LLM providers.
    #[serde(default)]
    pub http_client: HttpClientConfig,

    /// Graceful shutdown timing.
    #[serde(default)]
    pub shutdown: ShutdownConfig,

    /// Maximum number of per-issuer JWKS endpoints fetched in parallel when
    /// warming the gateway JWT validator registry on startup. Higher values
    /// speed up startup but risk overwhelming individual IdPs.
    #[serde(default = "default_jwt_loader_concurrency")]
    pub jwt_loader_concurrency: usize,

    /// Allow loopback addresses (127.0.0.1, ::1, localhost) in user-supplied URLs.
    ///
    /// When false (default), URLs targeting loopback addresses are blocked to prevent SSRF.
    /// Enable for development only. Private ranges and cloud metadata endpoints
    /// are always blocked regardless of this setting.
    #[serde(default)]
    pub allow_loopback_urls: bool,

    /// Allow private/internal IP ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16) in
    /// user-supplied URLs.
    ///
    /// When false (default), URLs resolving to private IPs are blocked to prevent SSRF.
    /// Enable for Docker, Kubernetes, or other environments where services communicate
    /// over private networks (e.g., Keycloak at `http://keycloak:8080`).
    /// Cloud metadata endpoints (169.254.169.254) are always blocked.
    #[serde(default)]
    pub allow_private_urls: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            body_limit_bytes: default_body_limit(),
            audio_body_limit_bytes: default_audio_body_limit(),
            files_body_limit_bytes: default_files_body_limit(),
            skills_body_limit_bytes: default_skills_body_limit(),
            max_response_body_bytes: default_max_response_body(),
            timeout_secs: default_timeout(),
            streaming_idle_timeout_secs: default_streaming_idle_timeout(),
            tls: None,
            trusted_proxies: TrustedProxiesConfig::default(),
            cors: CorsConfig::default(),
            security_headers: SecurityHeadersConfig::default(),
            http_client: HttpClientConfig::default(),
            shutdown: ShutdownConfig::default(),
            jwt_loader_concurrency: default_jwt_loader_concurrency(),
            allow_loopback_urls: false,
            allow_private_urls: false,
        }
    }
}

fn default_host() -> IpAddr {
    "0.0.0.0".parse().unwrap()
}

fn default_port() -> u16 {
    8080
}

fn default_body_limit() -> usize {
    10 * 1024 * 1024 // 10 MB
}

fn default_audio_body_limit() -> usize {
    100 * 1024 * 1024 // 100 MB — enough for ~1h of compressed audio
}

fn default_files_body_limit() -> usize {
    512 * 1024 * 1024 // 512 MB — multi-document RAG ingest
}

fn default_skills_body_limit() -> usize {
    64 * 1024 * 1024 // 64 MB — skill zip bundles / multipart directories
}

fn default_max_response_body() -> usize {
    100 * 1024 * 1024 // 100 MB
}

fn default_timeout() -> u64 {
    300 // 5 minutes (for long-running completions)
}

fn default_streaming_idle_timeout() -> u64 {
    120 // 2 minutes between chunks
}

/// Graceful shutdown timing.
///
/// These values were previously hardcoded constants. They control how long the
/// server waits for in-flight work to drain before exiting. The defaults match
/// the prior hardcoded values; deployments with longer-running tasks (or with
/// shorter `terminationGracePeriodSeconds`) should override them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ShutdownConfig {
    /// Seconds to wait for the usage-buffer worker to flush its final batch.
    #[serde(default = "default_usage_buffer_flush_secs")]
    pub usage_buffer_flush_secs: u64,

    /// Seconds to wait for outstanding background tasks (request handlers,
    /// usage logging, etc.) to complete after the close signal.
    #[serde(default = "default_drain_secs")]
    pub drain_secs: u64,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            usage_buffer_flush_secs: default_usage_buffer_flush_secs(),
            drain_secs: default_drain_secs(),
        }
    }
}

fn default_usage_buffer_flush_secs() -> u64 {
    5
}

fn default_drain_secs() -> u64 {
    30
}

fn default_jwt_loader_concurrency() -> usize {
    10
}

/// TLS configuration.
///
/// Native TLS termination is not yet implemented. Until it is, the gateway
/// listens on plain HTTP and operators must terminate TLS upstream (reverse
/// proxy / load balancer). Setting `[server.tls]` without
/// `acknowledge_unsupported = true` is treated as a misconfiguration and
/// refuses startup, so an operator following stale documentation can't
/// silently expose plaintext.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Path to the certificate file (PEM format).
    pub cert_path: String,

    /// Path to the private key file (PEM format).
    pub key_path: String,

    /// Set to `true` to acknowledge that native TLS termination is not yet
    /// implemented and the gateway will continue to listen on plain HTTP.
    /// When unset, the gateway refuses to start to avoid an operator
    /// accidentally exposing plaintext after copying TLS config from
    /// stale documentation.
    #[serde(default)]
    pub acknowledge_unsupported: bool,
}

/// Configuration for trusted reverse proxies.
///
/// **Security Note:** Proxy header spoofing is a serious vulnerability. Only trust
/// proxy headers when the connecting client is from a known proxy IP/CIDR range.
///
/// - `dangerously_trust_all: true` - **DANGEROUS**: Trusts proxy headers from ANY source.
///   Only use in controlled environments where the gateway is not directly accessible
///   from the internet (e.g., behind a load balancer that strips/rewrites headers).
///
/// - `cidrs: ["10.0.0.0/8"]` - Trust proxy headers only when the connecting IP is
///   within one of the specified CIDR ranges. This is the recommended approach.
///
/// When proxy headers are trusted, X-Forwarded-For is parsed right-to-left, skipping
/// IPs that are within trusted CIDRs, to find the first untrusted (client) IP.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TrustedProxiesConfig {
    /// Trust all proxies (use X-Forwarded-For as-is).
    ///
    /// **WARNING: This is a security risk!** Only enable this if the gateway is
    /// completely isolated behind a trusted load balancer that:
    /// 1. Is the only way to reach the gateway
    /// 2. Properly sets/overwrites the X-Forwarded-For header
    ///
    /// If attackers can connect directly to the gateway, they can spoof any IP
    /// and bypass IP-based rate limiting entirely.
    #[serde(default)]
    pub dangerously_trust_all: bool,

    /// List of trusted proxy CIDR ranges (e.g., ["10.0.0.0/8", "172.16.0.0/12"]).
    ///
    /// Proxy headers are only trusted when the connecting IP is within one of
    /// these ranges. This prevents IP spoofing from untrusted sources.
    #[serde(default)]
    pub cidrs: Vec<String>,

    /// Header to use for the real client IP.
    /// Common values: "X-Forwarded-For", "X-Real-IP", "CF-Connecting-IP"
    #[serde(default = "default_real_ip_header")]
    pub real_ip_header: String,
}

impl TrustedProxiesConfig {
    /// Parse the CIDR strings into IpNet objects.
    ///
    /// Invalid CIDRs are logged as warnings and skipped.
    pub fn parsed_cidrs(&self) -> Vec<IpNet> {
        self.cidrs
            .iter()
            .filter_map(|cidr_str| {
                cidr_str.parse::<IpNet>().ok().or_else(|| {
                    tracing::warn!(cidr = %cidr_str, "Invalid CIDR in trusted_proxies config, skipping");
                    None
                })
            })
            .collect()
    }

    /// Check if an IP address is within any of the trusted CIDR ranges.
    pub fn is_trusted_ip(&self, ip: IpAddr, parsed_cidrs: &[IpNet]) -> bool {
        if self.dangerously_trust_all {
            return true;
        }
        parsed_cidrs.iter().any(|cidr| cidr.contains(&ip))
    }

    /// Returns true if proxy headers should potentially be trusted.
    ///
    /// This doesn't mean headers ARE trusted - the connecting IP must still
    /// be validated against the CIDRs (unless dangerously_trust_all is set).
    pub fn is_configured(&self) -> bool {
        self.dangerously_trust_all || !self.cidrs.is_empty()
    }
}

fn default_real_ip_header() -> String {
    "X-Forwarded-For".to_string()
}

/// CORS configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CorsConfig {
    /// Enable CORS.
    #[serde(default = "default_cors_enabled")]
    pub enabled: bool,

    /// Allowed origins. Use ["*"] for any origin (not recommended for production).
    #[serde(default)]
    pub allowed_origins: Vec<String>,

    /// Allowed HTTP methods.
    #[serde(default = "default_cors_methods")]
    pub allowed_methods: Vec<String>,

    /// Allowed headers.
    #[serde(default = "default_cors_headers")]
    pub allowed_headers: Vec<String>,

    /// Whether to allow credentials.
    #[serde(default)]
    pub allow_credentials: bool,

    /// Max age for preflight cache in seconds.
    #[serde(default = "default_cors_max_age")]
    pub max_age_secs: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            enabled: default_cors_enabled(),
            allowed_origins: vec![],
            allowed_methods: default_cors_methods(),
            allowed_headers: default_cors_headers(),
            allow_credentials: false,
            max_age_secs: default_cors_max_age(),
        }
    }
}

impl CorsConfig {
    /// Build a CorsLayer from the configuration.
    ///
    /// Returns None if CORS is disabled.
    ///
    /// Behavior:
    /// - If `allowed_origins` is empty, no cross-origin requests are allowed (restrictive default)
    /// - If `allowed_origins` contains "*", any origin is allowed (logs a warning)
    /// - Otherwise, only the specified origins are allowed
    pub fn into_layer(self) -> Option<CorsLayer> {
        if !self.enabled {
            tracing::debug!("CORS is disabled");
            return None;
        }

        // Build allow_origin based on configuration
        let allow_origin = if self.allowed_origins.is_empty() {
            tracing::info!(
                "CORS: No allowed_origins configured - cross-origin requests will be rejected. \
                 Configure [server.cors.allowed_origins] to allow specific origins."
            );
            // Empty list means no origins allowed (restrictive default)
            AllowOrigin::list(std::iter::empty::<http::HeaderValue>())
        } else if self.allowed_origins.len() == 1 && self.allowed_origins[0] == "*" {
            tracing::warn!(
                "CORS: Allowing any origin (allowed_origins = [\"*\"]). \
                 This is NOT recommended for production - specify allowed origins explicitly."
            );
            AllowOrigin::any()
        } else {
            let origins: Vec<http::HeaderValue> = self
                .allowed_origins
                .iter()
                .filter_map(|origin| {
                    origin.parse().ok().or_else(|| {
                        tracing::warn!(origin = %origin, "Invalid CORS origin, skipping");
                        None
                    })
                })
                .collect();

            if origins.is_empty() {
                tracing::warn!(
                    "CORS: All configured origins were invalid - cross-origin requests will be rejected"
                );
            } else {
                tracing::info!(origins = ?self.allowed_origins, "CORS: Allowing specific origins");
            }

            AllowOrigin::list(origins)
        };

        // Build allow_methods
        let methods: Vec<Method> = self
            .allowed_methods
            .iter()
            .filter_map(|m| {
                m.parse().ok().or_else(|| {
                    tracing::warn!(method = %m, "Invalid CORS method, skipping");
                    None
                })
            })
            .collect();
        let allow_methods = AllowMethods::list(methods);

        // Build allow_headers
        let headers: Vec<HeaderName> = self
            .allowed_headers
            .iter()
            .filter_map(|h| {
                h.parse().ok().or_else(|| {
                    tracing::warn!(header = %h, "Invalid CORS header, skipping");
                    None
                })
            })
            .collect();
        let allow_headers = AllowHeaders::list(headers);

        let mut layer = CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_methods(allow_methods)
            .allow_headers(allow_headers)
            .max_age(Duration::from_secs(self.max_age_secs));

        if self.allow_credentials {
            layer = layer.allow_credentials(true);
        }

        Some(layer)
    }
}

fn default_cors_enabled() -> bool {
    true
}

fn default_cors_methods() -> Vec<String> {
    vec!["GET", "POST", "PUT", "DELETE", "OPTIONS"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn default_cors_headers() -> Vec<String> {
    vec!["Content-Type", "Authorization", "X-API-Key"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn default_cors_max_age() -> u64 {
    86400 // 24 hours
}

/// Security headers configuration.
///
/// These headers protect against common web vulnerabilities like clickjacking,
/// MIME-sniffing, and protocol downgrade attacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SecurityHeadersConfig {
    /// Enable security headers.
    #[serde(default = "default_security_headers_enabled")]
    pub enabled: bool,

    /// X-Content-Type-Options header value.
    /// Prevents MIME-sniffing attacks. Default: "nosniff"
    #[serde(default = "default_content_type_options")]
    pub content_type_options: String,

    /// X-Frame-Options header value.
    /// Prevents clickjacking attacks. Options: "DENY", "SAMEORIGIN", or omit.
    /// Default: "DENY"
    #[serde(default = "default_frame_options")]
    pub frame_options: Option<String>,

    /// Strict-Transport-Security header configuration.
    /// Forces HTTPS connections. Only sent over HTTPS connections.
    #[serde(default)]
    pub hsts: HstsConfig,

    /// Content-Security-Policy header value.
    /// Controls resource loading to prevent XSS attacks.
    ///
    /// When unset, the policy is rendered from `csp_preset`. Setting an explicit
    /// string here always wins.
    #[serde(default)]
    pub content_security_policy: Option<String>,

    /// Built-in CSP preset to use when `content_security_policy` is not set.
    ///
    /// - `strict` (default): no `'unsafe-eval'`, `connect-src 'self'`. Suitable
    ///   for headless gateway deployments and any deployment that does not serve
    ///   the bundled UI's in-browser WASM tools (Pyodide / DuckDB / Vega charts).
    /// - `self_hosted`: enables `'unsafe-eval'` + blob workers for the bundled
    ///   UI's WASM tools (served same-origin), but keeps `connect-src 'self'` —
    ///   no third-party origins. Use for UI deployments that do NOT use MCP or
    ///   the external browser tools (web search / Wikipedia).
    /// - `permissive`: like `self_hosted` but also widens `connect-src`
    ///   (`https: http: wss: ws:`) for runtime-configured MCP servers and
    ///   external browser tools. Required when the UI talks to MCP or fetches
    ///   external URLs.
    #[serde(default)]
    pub csp_preset: CspPreset,

    /// X-XSS-Protection header value.
    /// Legacy header for older browsers. Disabled by default as CSP provides protection.
    /// Enable for legacy browser compatibility.
    #[serde(default = "default_xss_protection")]
    pub xss_protection: Option<String>,

    /// Referrer-Policy header value.
    /// Controls referrer information sent in requests.
    /// Default: "strict-origin-when-cross-origin"
    #[serde(default = "default_referrer_policy")]
    pub referrer_policy: Option<String>,

    /// Permissions-Policy header value.
    /// Controls browser features available to the page.
    /// Default: None (not set)
    #[serde(default)]
    pub permissions_policy: Option<String>,
}

impl Default for SecurityHeadersConfig {
    fn default() -> Self {
        Self {
            enabled: default_security_headers_enabled(),
            content_type_options: default_content_type_options(),
            frame_options: default_frame_options(),
            hsts: HstsConfig::default(),
            content_security_policy: None,
            csp_preset: CspPreset::default(),
            xss_protection: default_xss_protection(),
            referrer_policy: default_referrer_policy(),
            permissions_policy: None,
        }
    }
}

impl SecurityHeadersConfig {
    /// Resolve the effective CSP header value.
    ///
    /// An explicit `content_security_policy` string always wins; otherwise the
    /// `csp_preset` is rendered. Returns `None` to disable the header entirely.
    pub fn resolved_csp(&self) -> Option<String> {
        if self.content_security_policy.is_some() {
            return self.content_security_policy.clone();
        }
        Some(self.csp_preset.render())
    }
}

/// Built-in CSP presets selectable via `[server.security_headers].csp_preset`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CspPreset {
    /// Locked-down CSP. No `'unsafe-eval'`, `connect-src 'self'`. Default.
    #[default]
    Strict,
    /// Same-origin WASM runtimes (self-hosted Pyodide/DuckDB). Enables
    /// `'unsafe-eval'` + blob workers for the bundled UI's WASM tools but keeps
    /// `connect-src 'self'` — no third-party origins at all. Use for UI
    /// deployments that do NOT use MCP or the external browser tools (web search
    /// / Wikipedia), which need outbound connections.
    SelfHosted,
    /// Like `self_hosted` but also widens `connect-src` for runtime-configured
    /// MCP server URLs and external browser tools.
    Permissive,
}

impl CspPreset {
    fn render(self) -> String {
        match self {
            CspPreset::Strict => default_csp_strict(),
            CspPreset::SelfHosted => default_csp_self_hosted(),
            CspPreset::Permissive => default_csp_permissive(),
        }
    }
}

/// Strict CSP — safe default for API-only / headless deployments.
fn default_csp_strict() -> String {
    "default-src 'self'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; \
     font-src 'self' data:; \
     media-src 'self'; \
     connect-src 'self'; \
     worker-src 'self'; \
     frame-src 'self'; \
     object-src 'none'; \
     base-uri 'self'; \
     form-action 'self'; \
     frame-ancestors 'none'"
        .to_string()
}

/// Permissive CSP for deployments serving the bundled UI's WASM features.
fn default_csp_permissive() -> String {
    default_csp().expect("permissive CSP is always Some")
}

/// CSP for deployments that serve the bundled UI's WASM tools from the gateway's
/// own origin (self-hosted Pyodide/DuckDB). Identical to `permissive` except
/// `connect-src` is narrowed to `'self'` — no third-party origins. This breaks
/// MCP and the external browser tools (web search / Wikipedia), which need
/// outbound connections; use `permissive` for those.
///
/// `'unsafe-eval'` is still required: Pyodide executes Python bytecode and Vega
/// compiles expressions via `Function()`. Self-hosting removes the CDN, not eval.
fn default_csp_self_hosted() -> String {
    "default-src 'self'; \
     script-src 'self' blob: 'unsafe-eval'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; \
     font-src 'self' data:; \
     media-src 'self' blob:; \
     connect-src 'self'; \
     worker-src 'self' blob:; \
     frame-src 'self' blob:; \
     object-src 'none'; \
     base-uri 'self'; \
     form-action 'self'; \
     frame-ancestors 'none'"
        .to_string()
}

fn default_security_headers_enabled() -> bool {
    true
}

fn default_content_type_options() -> String {
    "nosniff".to_string()
}

fn default_frame_options() -> Option<String> {
    Some("DENY".to_string())
}

/// Default Content-Security-Policy for the web UI.
///
/// Directives:
/// - `script-src 'self' blob: 'unsafe-eval'` — WASM workers loaded as blob URLs; `unsafe-eval`
///   required by Pyodide for Python bytecode execution and Vega `Function()` evaluation. The
///   WASM runtimes (Pyodide/DuckDB) are served same-origin, so no CDN origin is needed
/// - `style-src 'unsafe-inline'` — Tailwind CSS dynamic styling
/// - `worker-src blob:` — Web Worker sandboxed execution
/// - `frame-src blob:` — HTML artifact preview iframes
/// - `img-src data: blob:` — Generated charts/images and inline assets
/// - `media-src blob:` — Audio playback from generated TTS blob URLs
/// - `connect-src https: http: wss: ws:` — MCP servers are user-configured at arbitrary URLs
///   and discovered at runtime (stored in localStorage), so connect-src must allow all schemes.
///   Also covers Wikipedia/Wikidata tool queries. (Pyodide/DuckDB assets are served same-origin.)
/// - `object-src 'none'` — Blocks plugins (Flash, Java applets)
/// - `base-uri 'self'` — Prevents `<base>` tag injection
/// - `form-action 'self'` — Restricts form submission targets (not covered by `default-src`)
/// - `frame-ancestors 'none'` — Blocks cross-origin framing (takes precedence over `X-Frame-Options`)
fn default_csp() -> Option<String> {
    Some("default-src 'self'; script-src 'self' blob: 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self' https: http: wss: ws:; worker-src 'self' blob:; frame-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'".to_string())
}

fn default_xss_protection() -> Option<String> {
    None
}

fn default_referrer_policy() -> Option<String> {
    Some("strict-origin-when-cross-origin".to_string())
}

/// HTTP Strict Transport Security (HSTS) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HstsConfig {
    /// Enable HSTS header.
    #[serde(default = "default_hsts_enabled")]
    pub enabled: bool,

    /// Max age in seconds browsers should remember to only use HTTPS.
    /// Default: 31536000 (1 year)
    #[serde(default = "default_hsts_max_age")]
    pub max_age_secs: u64,

    /// Include all subdomains in the HSTS policy.
    #[serde(default = "default_hsts_include_subdomains")]
    pub include_subdomains: bool,

    /// Allow preloading into browser HSTS lists.
    /// Only enable if you're ready to commit to HTTPS permanently.
    #[serde(default)]
    pub preload: bool,
}

impl Default for HstsConfig {
    fn default() -> Self {
        Self {
            enabled: default_hsts_enabled(),
            max_age_secs: default_hsts_max_age(),
            include_subdomains: default_hsts_include_subdomains(),
            preload: false,
        }
    }
}

fn default_hsts_enabled() -> bool {
    true
}

fn default_hsts_max_age() -> u64 {
    31536000 // 1 year
}

fn default_hsts_include_subdomains() -> bool {
    true
}

/// HTTP client configuration for outbound requests.
///
/// Controls connection pooling, timeouts, and HTTP/2 settings for
/// requests to LLM providers and other external services.
///
/// # Architecture: Single Shared Client
///
/// The gateway uses a single `reqwest::Client` instance shared across all providers.
/// This is efficient because:
///
/// - **Per-host connection pooling**: reqwest maintains separate connection pools for each
///   host (api.openai.com, api.anthropic.com, etc.), so providers don't compete for connections.
///
/// - **HTTP/2 multiplexing**: With `http2_adaptive_window` enabled, each connection can handle
///   hundreds of concurrent request streams. At 32 idle connections per host, this supports
///   thousands of concurrent requests per provider.
///
/// - **Low overhead**: A single client shares DNS cache, TLS session cache, and connection
///   pools, reducing memory and CPU overhead compared to per-provider clients.
///
/// For extreme workloads (10K+ RPS to a single provider), increase `pool_max_idle_per_host`.
/// Per-provider clients would only help if you need different timeout settings per provider
/// or complete resource isolation between providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HttpClientConfig {
    /// Request timeout in seconds.
    /// This is the total time allowed for a request, including connection and response.
    /// Set high enough for long-running LLM completions (streaming responses may take minutes).
    #[serde(default = "default_http_client_timeout")]
    pub timeout_secs: u64,

    /// Enable verbose connection logging for debugging.
    /// Logs connection establishment details to help diagnose network issues.
    #[serde(default)]
    pub verbose: bool,

    /// Connection timeout in seconds.
    /// Time allowed to establish a connection to the remote server.
    #[serde(default = "default_http_client_connect_timeout")]
    pub connect_timeout_secs: u64,

    /// Maximum idle connections to keep per host.
    /// Higher values reduce connection establishment latency for frequently-used providers.
    /// Lower values reduce memory usage when connecting to many different hosts.
    #[serde(default = "default_pool_max_idle_per_host")]
    pub pool_max_idle_per_host: usize,

    /// Idle connection timeout in seconds.
    /// Connections idle longer than this are closed.
    #[serde(default = "default_pool_idle_timeout")]
    pub pool_idle_timeout_secs: u64,

    /// Enable HTTP/2 with prior knowledge (h2c or h2 without ALPN negotiation).
    /// Only enable if you know the target servers support HTTP/2.
    /// When false (default), HTTP version is negotiated automatically via ALPN.
    #[serde(default)]
    pub http2_prior_knowledge: bool,

    /// Enable HTTP/2 adaptive window sizing.
    /// Allows the receive window to grow dynamically based on throughput,
    /// improving performance for high-bandwidth connections.
    #[serde(default = "default_http2_adaptive_window")]
    pub http2_adaptive_window: bool,

    /// TCP keepalive interval in seconds.
    /// Sends periodic probes to detect dead connections.
    /// Set to 0 to disable TCP keepalive.
    #[serde(default = "default_tcp_keepalive")]
    pub tcp_keepalive_secs: u64,

    /// Enable TCP_NODELAY (disable Nagle's algorithm).
    /// Reduces latency for small writes at the cost of slightly higher bandwidth usage.
    #[serde(default = "default_tcp_nodelay")]
    pub tcp_nodelay: bool,

    /// User-Agent header to send with requests.
    /// Some providers require or prefer specific User-Agent values.
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_http_client_timeout(),
            verbose: false,
            connect_timeout_secs: default_http_client_connect_timeout(),
            pool_max_idle_per_host: default_pool_max_idle_per_host(),
            pool_idle_timeout_secs: default_pool_idle_timeout(),
            http2_prior_knowledge: false,
            http2_adaptive_window: default_http2_adaptive_window(),
            tcp_keepalive_secs: default_tcp_keepalive(),
            tcp_nodelay: default_tcp_nodelay(),
            user_agent: default_user_agent(),
        }
    }
}

impl HttpClientConfig {
    /// Build a reqwest Client from this configuration.
    pub fn build_client(&self) -> Result<reqwest::Client, reqwest::Error> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut builder = reqwest::Client::builder()
                .timeout(Duration::from_secs(self.timeout_secs))
                .connection_verbose(self.verbose)
                .connect_timeout(Duration::from_secs(self.connect_timeout_secs))
                .pool_max_idle_per_host(self.pool_max_idle_per_host)
                .pool_idle_timeout(Duration::from_secs(self.pool_idle_timeout_secs))
                .tcp_nodelay(self.tcp_nodelay)
                .user_agent(&self.user_agent);

            // HTTP/2 configuration
            if self.http2_prior_knowledge {
                builder = builder.http2_prior_knowledge();
            }
            if self.http2_adaptive_window {
                builder = builder.http2_adaptive_window(true);
            }

            // TCP keepalive (0 means disabled)
            if self.tcp_keepalive_secs > 0 {
                builder = builder.tcp_keepalive(Duration::from_secs(self.tcp_keepalive_secs));
            }

            builder.build()
        }
        #[cfg(target_arch = "wasm32")]
        {
            reqwest::Client::builder().build()
        }
    }
}

// Default: 5 minutes for long-running completions
fn default_http_client_timeout() -> u64 {
    300
}

// Default: 10 seconds to establish connection
fn default_http_client_connect_timeout() -> u64 {
    10
}

// Default: 32 idle connections per host (good balance for multi-provider setups)
fn default_pool_max_idle_per_host() -> usize {
    32
}

// Default: 90 seconds idle timeout
fn default_pool_idle_timeout() -> u64 {
    90
}

// Default: enable adaptive window for better throughput
fn default_http2_adaptive_window() -> bool {
    true
}

// Default: 60 seconds TCP keepalive
fn default_tcp_keepalive() -> u64 {
    60
}

// Default: enable TCP_NODELAY for lower latency
fn default_tcp_nodelay() -> bool {
    true
}

fn default_user_agent() -> String {
    format!("hadrian/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csp_preset_self_hosted_no_cdn_narrow_connect() {
        let csp = default_csp_self_hosted();
        assert!(csp.contains("'unsafe-eval'"));
        assert!(csp.contains("worker-src 'self' blob:"));
        assert!(!csp.contains("jsdelivr"));
        // connect-src is narrowed to 'self' — no wide schemes for MCP/external tools.
        assert!(csp.contains("connect-src 'self';"));
        assert!(!csp.contains("connect-src 'self' https:"));
    }

    #[test]
    fn test_csp_preset_permissive_drops_cdn_keeps_wide_connect() {
        let csp = default_csp_permissive();
        assert!(!csp.contains("jsdelivr"));
        assert!(csp.contains("'unsafe-eval'"));
        // MCP servers and external browser tools still need a wide connect-src.
        assert!(csp.contains("connect-src 'self' https: http: wss: ws:"));
    }

    #[test]
    fn test_csp_preset_strict_has_no_eval() {
        let csp = default_csp_strict();
        assert!(!csp.contains("'unsafe-eval'"));
        assert!(!csp.contains("jsdelivr"));
        assert!(csp.contains("connect-src 'self'"));
    }

    #[test]
    fn test_csp_preset_serde_snake_case_roundtrip() {
        #[derive(serde::Deserialize)]
        struct W {
            p: CspPreset,
        }
        assert_eq!(
            toml::from_str::<W>("p = \"strict\"").unwrap().p,
            CspPreset::Strict
        );
        assert_eq!(
            toml::from_str::<W>("p = \"self_hosted\"").unwrap().p,
            CspPreset::SelfHosted
        );
        assert_eq!(
            toml::from_str::<W>("p = \"permissive\"").unwrap().p,
            CspPreset::Permissive
        );
    }

    #[test]
    fn test_resolved_csp_explicit_string_overrides_preset() {
        let config = SecurityHeadersConfig {
            content_security_policy: Some("default-src 'none'".to_string()),
            csp_preset: CspPreset::Permissive,
            ..Default::default()
        };
        assert_eq!(config.resolved_csp().as_deref(), Some("default-src 'none'"));
    }

    #[test]
    fn test_http_client_config_defaults() {
        let config = HttpClientConfig::default();
        assert_eq!(config.timeout_secs, 300);
        assert_eq!(config.connect_timeout_secs, 10);
        assert_eq!(config.pool_max_idle_per_host, 32);
        assert_eq!(config.pool_idle_timeout_secs, 90);
        assert!(!config.http2_prior_knowledge);
        assert!(config.http2_adaptive_window);
        assert_eq!(config.tcp_keepalive_secs, 60);
        assert!(config.tcp_nodelay);
        assert!(config.user_agent.starts_with("hadrian/"));
    }

    #[test]
    fn test_http_client_config_build() {
        let config = HttpClientConfig::default();
        let client = config.build_client();
        assert!(client.is_ok());
    }

    #[test]
    fn test_http_client_config_custom() {
        let config = HttpClientConfig {
            timeout_secs: 60,
            verbose: false,
            connect_timeout_secs: 5,
            pool_max_idle_per_host: 16,
            pool_idle_timeout_secs: 30,
            http2_prior_knowledge: true,
            http2_adaptive_window: false,
            tcp_keepalive_secs: 0, // Disabled
            tcp_nodelay: false,
            user_agent: "custom-agent/1.0".to_string(),
        };
        let client = config.build_client();
        assert!(client.is_ok());
    }

    #[test]
    fn test_http_client_config_parse() {
        let toml = r#"
            timeout_secs = 120
            connect_timeout_secs = 5
            pool_max_idle_per_host = 64
            http2_prior_knowledge = true
        "#;
        let config: HttpClientConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.timeout_secs, 120);
        assert_eq!(config.connect_timeout_secs, 5);
        assert_eq!(config.pool_max_idle_per_host, 64);
        assert!(config.http2_prior_knowledge);
        // Defaults for unspecified fields
        assert!(config.http2_adaptive_window);
        assert_eq!(config.tcp_keepalive_secs, 60);
    }
}
