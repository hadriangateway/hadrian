//! Shell tool runtime configuration.
//!
//! Mirrors the `SecretsConfig` pattern: an enum tagged by `type` selects
//! which backend runs the model's `shell` tool calls. Backends behind
//! cargo features are conditionally compiled in.

use serde::{Deserialize, Serialize};

/// Top-level configuration for the `shell` tool runtime.
///
/// Defaults to `None`, which disables the shell tool entirely (any
/// shell tool definition in a request is ignored, same as if the
/// feature were not configured).
///
/// # Examples
///
/// Pass-through to OpenAI (works with GPT-5.2+ that has built-in shell):
///
/// ```toml
/// [features.shell]
/// type = "passthrough_openai"
/// ```
///
/// Client fulfills shell calls itself (provider-agnostic equivalent of
/// OpenAI's "local shell" mode — works behind Anthropic, Bedrock,
/// Vertex, etc. as well as OpenAI):
///
/// ```toml
/// [features.shell]
/// type = "client_passthrough"
/// ```
///
/// Local microVM via microsandbox:
///
/// ```toml
/// [features.shell]
/// type = "microsandbox"
/// endpoint = "http://localhost:5555"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellRuntimeConfig {
    /// Shell tool is not configured. Requests with shell tools fail
    /// with a clear error so it's obvious the gateway isn't set up.
    #[default]
    None,

    /// Forward `shell` tool specs to the upstream provider unchanged.
    /// Only meaningful when the upstream is OpenAI (the model itself
    /// runs the shell call in OpenAI's hosted container).
    PassthroughOpenAI,

    /// The API client fulfills shell calls itself — Hadrian validates
    /// the request, keeps OpenAI's native `shell` spec intact, rewrites
    /// it to a function tool for non-OpenAI providers, and skips
    /// server-side execution. Wire format the client sees:
    ///
    /// - OpenAI: `shell_call` / `shell_call_output` (native).
    /// - Anthropic / Bedrock / Vertex: `function_call` with
    ///   `name="shell"` / `function_call_output` (because those
    ///   providers have no native shell tool type).
    ///
    /// Equivalent to OpenAI's "local shell" mode but works behind any
    /// supported provider.
    ClientPassthrough,

    /// Local microVMs via microsandbox
    /// (<https://github.com/superradcompany/microsandbox>).
    #[cfg(feature = "runtime-microsandbox")]
    Microsandbox(MicrosandboxConfig),

    /// Alibaba OpenSandbox Protocol over HTTP.
    /// (<https://github.com/alibaba/OpenSandbox>)
    #[cfg(feature = "runtime-opensandbox")]
    OpenSandbox(OpenSandboxConfig),
}

impl ShellRuntimeConfig {
    /// Whether any shell runtime is configured. When false the shell
    /// tool is effectively disabled and the orchestrator skips
    /// registering it.
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether this mode keeps OpenAI's native `shell` tool spec intact
    /// rather than rewriting it to a function tool. True for both
    /// passthrough modes (the OpenAI hosted container and the API
    /// client both want native `shell_call` items); false for the
    /// Hadrian-hosted sandbox modes (those run the call themselves and
    /// need the function-mode rewrite so the executor can intercept).
    pub fn keeps_openai_native_shell(&self) -> bool {
        matches!(self, Self::PassthroughOpenAI | Self::ClientPassthrough)
    }

    /// Human-readable name of the configured runtime, for logging.
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PassthroughOpenAI => "passthrough_openai",
            Self::ClientPassthrough => "client_passthrough",
            #[cfg(feature = "runtime-microsandbox")]
            Self::Microsandbox(_) => "microsandbox",
            #[cfg(feature = "runtime-opensandbox")]
            Self::OpenSandbox(_) => "opensandbox",
        }
    }
}

/// Configuration for the microsandbox runtime.
///
/// Microsandbox is an in-process Rust SDK (no daemon, no endpoint) — Hadrian
/// links it directly and constructs `Sandbox` instances per session.
#[cfg(feature = "runtime-microsandbox")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MicrosandboxConfig {
    /// Container image to boot for each session. Default `alpine`.
    #[serde(default = "default_microsandbox_image")]
    pub image: String,

    /// vCPUs per session. Default 1. Range 1..=255; values above 255
    /// are clamped at the SDK boundary.
    #[serde(default = "default_microsandbox_cpus")]
    pub cpus: u32,

    /// Memory per session in MB. Default 512.
    #[serde(default = "default_microsandbox_memory_mb")]
    pub memory_mb: u32,
}

#[cfg(feature = "runtime-microsandbox")]
fn default_microsandbox_image() -> String {
    "alpine".to_string()
}

#[cfg(feature = "runtime-microsandbox")]
fn default_microsandbox_cpus() -> u32 {
    1
}

#[cfg(feature = "runtime-microsandbox")]
fn default_microsandbox_memory_mb() -> u32 {
    512
}

/// Configuration for the OpenSandbox runtime.
#[cfg(feature = "runtime-opensandbox")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OpenSandboxConfig {
    /// HTTP endpoint of the OpenSandbox Lifecycle API (e.g.
    /// `http://localhost:8080/v1`).
    pub endpoint: String,

    /// Optional API key sent in the `OPEN-SANDBOX-API-KEY` header.
    #[serde(default)]
    pub auth_token: Option<String>,

    /// Container image used when starting a sandbox. Default
    /// `python:3.11-slim`.
    #[serde(default)]
    pub default_image: Option<String>,

    /// Maximum seconds to wait for a newly created sandbox to reach
    /// `Running` state. Default 60.
    #[serde(default = "default_opensandbox_start_timeout")]
    pub start_timeout_secs: u64,
}

#[cfg(feature = "runtime-opensandbox")]
fn default_opensandbox_start_timeout() -> u64 {
    60
}
