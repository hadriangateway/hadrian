//! Passthrough runtime: defer shell execution off the Hadrian process.
//!
//! Two modes share this adapter because both skip server-side execution
//! and differ only in which party fulfills the shell call:
//!
//! - [`PassthroughMode::OpenAiContainer`] (config `passthrough_openai`) —
//!   forward the native `shell` tool spec to OpenAI; the model runs the
//!   call in OpenAI's hosted container. Only meaningful when the
//!   upstream provider is OpenAI (GPT-5.2+ with the built-in `shell`
//!   tool type).
//! - [`PassthroughMode::ApiClient`] (config `client_passthrough`) — the
//!   API client fulfills shell calls itself. Models with native shell
//!   support (OpenAI) emit `shell_call` items; non-OpenAI providers get
//!   the function-mode rewrite and emit `function_call` items with
//!   `name="shell"`. Either way the orchestrator skips server-side
//!   execution and the call passes through.
//!
//! In both modes `start_session` returns [`RuntimeError::Passthrough`]
//! so the orchestrator knows not to invoke local execution. The mode
//! choice is exposed via [`RuntimeCapabilities::client_executes`] so
//! the preprocessing layer (`routes/execution.rs`) can keep OpenAI's
//! native `shell` spec intact for `ApiClient` mode the same way it does
//! for `OpenAiContainer` mode.

use async_trait::async_trait;

use super::{
    RuntimeCapabilities, RuntimeError, RuntimeResult, SessionHandle, SessionSpec, ShellRuntime,
};

/// Which party fulfills shell calls when execution is deferred off
/// Hadrian. See the module docs for the rationale behind each variant.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PassthroughMode {
    /// OpenAI's Responses-API hosted container runs the call.
    #[default]
    OpenAiContainer,
    /// The API client fulfills the call locally (OpenAI's "local shell"
    /// mode, generalized to all providers).
    ApiClient,
}

/// Pass-through runtime for both `passthrough_openai` and
/// `client_passthrough` config modes. The mode it was constructed with
/// only affects [`capabilities()`](ShellRuntime::capabilities); the
/// execution surface is identical (start_session is a no-op that asks
/// the orchestrator to skip local execution).
#[derive(Debug, Default, Clone, Copy)]
pub struct PassthroughRuntime {
    mode: PassthroughMode,
}

impl PassthroughRuntime {
    /// Construct a passthrough runtime for OpenAI's hosted container.
    pub const fn for_openai_container() -> Self {
        Self {
            mode: PassthroughMode::OpenAiContainer,
        }
    }

    /// Construct a passthrough runtime where the API client fulfills
    /// shell calls itself.
    pub const fn for_api_client() -> Self {
        Self {
            mode: PassthroughMode::ApiClient,
        }
    }

    pub fn mode(&self) -> PassthroughMode {
        self.mode
    }
}

#[async_trait]
impl ShellRuntime for PassthroughRuntime {
    fn capabilities(&self) -> RuntimeCapabilities {
        match self.mode {
            PassthroughMode::OpenAiContainer => RuntimeCapabilities::passthrough_upstream(),
            PassthroughMode::ApiClient => RuntimeCapabilities::passthrough_client(),
        }
    }

    async fn start_session(&self, _spec: SessionSpec) -> RuntimeResult<SessionHandle> {
        Err(RuntimeError::Passthrough)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_mode_advertises_upstream_execution() {
        let rt = PassthroughRuntime::for_openai_container();
        let caps = rt.capabilities();
        assert!(caps.passthrough_only);
        assert!(!caps.client_executes);
    }

    #[test]
    fn client_mode_advertises_client_execution() {
        let rt = PassthroughRuntime::for_api_client();
        let caps = rt.capabilities();
        assert!(caps.passthrough_only);
        assert!(caps.client_executes);
    }
}
