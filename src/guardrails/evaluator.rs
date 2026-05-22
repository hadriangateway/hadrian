//! Guardrails evaluator for content filtering.
//!
//! This module provides evaluators for input and output guardrails:
//!
//! - `InputGuardrails`: Evaluates user input before sending to the LLM
//! - `OutputGuardrails`: Evaluates LLM responses before returning to the user
//!
//! Both evaluators share common infrastructure:
//! - Creating guardrails providers from configuration
//! - Extracting text content from requests/responses
//! - Evaluating content against guardrails policies
//! - Applying configured actions (block, warn, log, redact)

use std::{sync::Arc, time::Duration};

use reqwest::Client;
use tracing::instrument;

#[cfg(feature = "provider-bedrock")]
use super::BedrockGuardrailsProvider;
use super::{
    ActionExecutor, AzureContentSafetyProvider, BlocklistProvider, CustomHttpProvider,
    GuardrailsError, GuardrailsProvider, GuardrailsRequest, GuardrailsResponse,
    GuardrailsRetryConfig, OpenAIModerationProvider, ResolvedAction, Violation,
};
use crate::{
    api_types::{
        CreateChatCompletionPayload, Message, MessageContent,
        chat_completion::ContentPart,
        completions::{CompletionPrompt, CreateCompletionPayload},
        responses::{
            CreateResponsesPayload, EasyInputMessage, EasyInputMessageContent,
            OutputMessageContentItem, ResponseInputContentItem, ResponsesInput, ResponsesInputItem,
        },
    },
    config::{
        GuardrailsConfig, GuardrailsProvider as GuardrailsProviderConfig, InputGuardrailsConfig,
        OutputGuardrailsConfig,
    },
    observability::metrics::{
        record_guardrails_concurrent_race, record_guardrails_error, record_guardrails_evaluation,
        record_guardrails_timeout, record_guardrails_violation,
    },
};

/// Input guardrails evaluator.
///
/// Evaluates user input content against guardrails policies before sending to the LLM.
/// Supports two execution modes:
/// - **Blocking**: Wait for guardrails evaluation to complete before calling the LLM
/// - **Concurrent**: Start guardrails and LLM calls simultaneously, canceling if guardrails fail
pub struct InputGuardrails {
    /// The guardrails provider to use for evaluation.
    provider: Arc<dyn GuardrailsProvider>,
    /// Action executor for applying configured actions.
    action_executor: ActionExecutor,
    /// Retry configuration.
    retry_config: GuardrailsRetryConfig,
    /// Timeout for evaluation.
    timeout: Duration,
    /// Execution mode (blocking or concurrent).
    mode: crate::config::GuardrailsExecutionMode,
    /// Action to take on timeout (for concurrent mode).
    on_timeout: crate::config::GuardrailsTimeoutAction,
    /// Action to take on provider error.
    on_error: crate::config::GuardrailsErrorAction,
}

impl InputGuardrails {
    /// Creates a new input guardrails evaluator from configuration.
    ///
    /// Returns `None` if guardrails are disabled or not configured.
    pub fn from_config(
        config: &GuardrailsConfig,
        http_client: &Client,
    ) -> Result<Option<Self>, GuardrailsError> {
        if !config.enabled {
            return Ok(None);
        }

        let Some(input_config) = &config.input else {
            return Ok(None);
        };

        if !input_config.enabled {
            return Ok(None);
        }

        let provider = create_provider(&input_config.provider, http_client)?;
        let action_executor = ActionExecutor::from_input_config(input_config);
        let retry_config = GuardrailsRetryConfig::default();

        Ok(Some(Self {
            provider,
            action_executor,
            retry_config,
            timeout: Duration::from_millis(input_config.timeout_ms),
            mode: input_config.mode.clone(),
            on_timeout: input_config.on_timeout.clone(),
            on_error: input_config.on_error.clone(),
        }))
    }

    /// Creates input guardrails from a specific input config (for testing).
    pub fn from_input_config(
        config: &InputGuardrailsConfig,
        http_client: &Client,
    ) -> Result<Self, GuardrailsError> {
        let provider = create_provider(&config.provider, http_client)?;
        let action_executor = ActionExecutor::from_input_config(config);
        let retry_config = GuardrailsRetryConfig::default();

        Ok(Self {
            provider,
            action_executor,
            retry_config,
            timeout: Duration::from_millis(config.timeout_ms),
            mode: config.mode.clone(),
            on_timeout: config.on_timeout.clone(),
            on_error: config.on_error.clone(),
        })
    }

    /// Evaluates a chat completion payload against guardrails.
    ///
    /// Extracts all text content from messages and evaluates them.
    /// Returns the resolved action to take based on the evaluation result.
    #[instrument(skip(self, payload), fields(provider = %self.provider.name()))]
    pub async fn evaluate_payload(
        &self,
        payload: &CreateChatCompletionPayload,
        request_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<InputGuardrailsResult, GuardrailsError> {
        // Extract all text content from messages
        let text = extract_text_from_messages(&payload.messages);

        if text.is_empty() {
            tracing::debug!("No text content to evaluate");
            return Ok(InputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: text,
            });
        }

        // Build the guardrails request
        let mut request = GuardrailsRequest::user_input(&text);
        if let Some(id) = request_id {
            request = request.with_request_id(id);
        }
        if let Some(id) = user_id {
            request = request.with_user_id(id);
        }

        // Evaluate with timeout
        // Note: Retry is handled internally by individual providers or can be added
        // at a higher level. The with_retry function requires 'static closures which
        // doesn't work well with the async trait pattern.
        let evaluation_result =
            tokio::time::timeout(self.timeout, self.evaluate_with_retry(&request)).await;

        let response = match evaluation_result {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                // Provider error
                return self.handle_provider_error(e, &text);
            }
            Err(_) => {
                // Timeout
                return self.handle_timeout(&text);
            }
        };

        // Apply configured actions
        let action = self.action_executor.resolve_action(&response, &text);

        tracing::debug!(
            passed = response.passed,
            violations = response.violations.len(),
            action = ?action,
            "Guardrails evaluation complete"
        );

        Ok(InputGuardrailsResult {
            action,
            response,
            evaluated_text: text,
        })
    }

    /// Evaluates a completion payload against guardrails.
    ///
    /// Extracts text content from the prompt and evaluates it.
    /// Returns the resolved action to take based on the evaluation result.
    #[instrument(skip(self, payload), fields(provider = %self.provider.name()))]
    pub async fn evaluate_completion_payload(
        &self,
        payload: &CreateCompletionPayload,
        request_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<InputGuardrailsResult, GuardrailsError> {
        // Extract text content from prompt
        let text = extract_text_from_completion_payload(payload);

        self.evaluate_text(&text, request_id, user_id).await
    }

    /// Evaluates a responses payload against guardrails.
    ///
    /// Extracts text content from the input and instructions, and evaluates them.
    /// Returns the resolved action to take based on the evaluation result.
    #[instrument(skip(self, payload), fields(provider = %self.provider.name()))]
    pub async fn evaluate_responses_payload(
        &self,
        payload: &CreateResponsesPayload,
        request_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<InputGuardrailsResult, GuardrailsError> {
        // Extract text content from input and instructions
        let text = extract_text_from_responses_payload(payload);

        self.evaluate_text(&text, request_id, user_id).await
    }

    /// Evaluates raw text content against guardrails.
    ///
    /// This is the common evaluation logic used by all payload types.
    async fn evaluate_text(
        &self,
        text: &str,
        request_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<InputGuardrailsResult, GuardrailsError> {
        if text.is_empty() {
            tracing::debug!("No text content to evaluate");
            return Ok(InputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: text.to_string(),
            });
        }

        // Build the guardrails request
        let mut request = GuardrailsRequest::user_input(text);
        if let Some(id) = request_id {
            request = request.with_request_id(id);
        }
        if let Some(id) = user_id {
            request = request.with_user_id(id);
        }

        let start = std::time::Instant::now();

        // Evaluate with timeout
        let evaluation_result =
            tokio::time::timeout(self.timeout, self.evaluate_with_retry(&request)).await;

        let latency_ms = start.elapsed().as_millis() as u64;

        let response = match evaluation_result {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                // Provider error - record error metrics
                record_guardrails_error(self.provider.name(), "input", e.error_type_for_metrics());
                record_guardrails_evaluation(
                    self.provider.name(),
                    "input",
                    "error",
                    latency_ms as f64 / 1000.0,
                );
                return self.handle_provider_error(e, text);
            }
            Err(_) => {
                // Timeout - record timeout metrics
                record_guardrails_timeout(self.provider.name(), "input");
                record_guardrails_evaluation(
                    self.provider.name(),
                    "input",
                    "timeout",
                    latency_ms as f64 / 1000.0,
                );
                return self.handle_timeout(text);
            }
        };

        // Apply configured actions
        let action = self.action_executor.resolve_action(&response, text);
        let result_label = resolved_action_label(&action);

        // Record evaluation metrics
        record_evaluation_metrics(
            self.provider.name(),
            "input",
            result_label,
            Some(latency_ms),
            &response.violations,
        );

        // Record span events for each violation for distributed tracing
        for violation in &response.violations {
            tracing::event!(
                tracing::Level::INFO,
                category = %violation.category,
                severity = %violation.severity,
                confidence = violation.confidence,
                message = violation.message.as_deref().unwrap_or(""),
                "Guardrails violation detected"
            );
        }

        tracing::debug!(
            passed = response.passed,
            violations = response.violations.len(),
            action = ?action,
            "Guardrails evaluation complete"
        );

        Ok(InputGuardrailsResult {
            action,
            response,
            evaluated_text: text.to_string(),
        })
    }

    /// Evaluates the request with retry logic.
    async fn evaluate_with_retry(
        &self,
        request: &GuardrailsRequest,
    ) -> Result<GuardrailsResponse, GuardrailsError> {
        let max_attempts = if self.retry_config.enabled {
            self.retry_config.max_retries + 1
        } else {
            1
        };

        let mut last_error = None;

        for attempt in 0..max_attempts {
            match self.provider.evaluate(request).await {
                Ok(response) => {
                    if attempt > 0 {
                        tracing::debug!(
                            provider = self.provider.name(),
                            attempt = attempt + 1,
                            "Guardrails evaluation succeeded after retry"
                        );
                    }
                    return Ok(response);
                }
                Err(error) => {
                    if error.is_retryable() && attempt < max_attempts - 1 {
                        let delay = self.retry_config.delay_for_attempt(attempt);
                        tracing::warn!(
                            provider = self.provider.name(),
                            error = %error,
                            attempt = attempt + 1,
                            max_attempts = max_attempts,
                            delay_ms = delay.as_millis(),
                            "Retryable guardrails error, will retry after delay"
                        );
                        tokio::time::sleep(delay).await;
                        last_error = Some(error);
                        continue;
                    }

                    if attempt > 0 {
                        tracing::warn!(
                            provider = self.provider.name(),
                            error = %error,
                            attempts = attempt + 1,
                            "Guardrails evaluation failed after all retry attempts"
                        );
                    }
                    return Err(error);
                }
            }
        }

        // This should only be reached if max_attempts is 0, which shouldn't happen
        Err(last_error.unwrap_or_else(|| GuardrailsError::internal("No evaluation attempts made")))
    }

    /// Handles a provider error based on configuration.
    fn handle_provider_error(
        &self,
        error: GuardrailsError,
        text: &str,
    ) -> Result<InputGuardrailsResult, GuardrailsError> {
        use crate::config::GuardrailsErrorAction;

        tracing::warn!(
            error = %error,
            on_error = ?self.on_error,
            "Guardrails provider error"
        );

        match self.on_error {
            GuardrailsErrorAction::Block => Err(error),
            GuardrailsErrorAction::Allow => Ok(InputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: text.to_string(),
            }),
            GuardrailsErrorAction::LogAndAllow => {
                tracing::error!(
                    error = %error,
                    "Guardrails provider error - allowing request (log_and_allow)"
                );
                Ok(InputGuardrailsResult {
                    action: ResolvedAction::Allow,
                    response: GuardrailsResponse::passed(),
                    evaluated_text: text.to_string(),
                })
            }
        }
    }

    /// Handles a timeout based on configuration.
    fn handle_timeout(&self, text: &str) -> Result<InputGuardrailsResult, GuardrailsError> {
        use crate::config::GuardrailsTimeoutAction;

        tracing::warn!(
            timeout_ms = %self.timeout.as_millis(),
            on_timeout = ?self.on_timeout,
            "Guardrails evaluation timed out"
        );

        match self.on_timeout {
            GuardrailsTimeoutAction::Block => Err(GuardrailsError::timeout(
                self.provider.name(),
                self.timeout.as_millis() as u64,
            )),
            GuardrailsTimeoutAction::Allow => Ok(InputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: text.to_string(),
            }),
        }
    }

    /// Returns the provider name.
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// Returns true if the guardrails are configured for concurrent execution mode.
    pub fn is_concurrent(&self) -> bool {
        self.mode == crate::config::GuardrailsExecutionMode::Concurrent
    }

    /// Returns the execution mode.
    pub fn mode(&self) -> &crate::config::GuardrailsExecutionMode {
        &self.mode
    }

    /// Returns the timeout duration for concurrent mode.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns the on_timeout action for concurrent mode.
    pub fn on_timeout(&self) -> &crate::config::GuardrailsTimeoutAction {
        &self.on_timeout
    }

    /// Creates an InputGuardrails for testing with a mock provider.
    #[cfg(test)]
    pub(crate) fn for_testing(
        provider: Arc<dyn GuardrailsProvider>,
        timeout: Duration,
        on_timeout: crate::config::GuardrailsTimeoutAction,
    ) -> Self {
        Self {
            provider,
            action_executor: ActionExecutor::new(
                std::collections::HashMap::new(),
                crate::config::GuardrailsAction::Block,
            ),
            retry_config: GuardrailsRetryConfig::default(),
            timeout,
            mode: crate::config::GuardrailsExecutionMode::Concurrent,
            on_timeout,
            on_error: crate::config::GuardrailsErrorAction::Block,
        }
    }
}

/// Result of input guardrails evaluation.
#[derive(Debug)]
pub struct InputGuardrailsResult {
    /// The resolved action to take.
    pub action: ResolvedAction,
    /// The raw guardrails response.
    pub response: GuardrailsResponse,
    /// The text that was evaluated.
    pub evaluated_text: String,
}

impl InputGuardrailsResult {
    /// Returns true if the content should be blocked.
    pub fn is_blocked(&self) -> bool {
        self.action.is_blocked()
    }

    /// Returns the violations found during evaluation.
    pub fn violations(&self) -> &[Violation] {
        self.action.violations()
    }

    /// Returns a string label for the action result (for metrics).
    pub fn result_label(&self) -> &'static str {
        resolved_action_label(&self.action)
    }

    /// Creates response headers for the guardrails result.
    pub fn to_headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = Vec::new();

        // Add result header
        let result = match &self.action {
            ResolvedAction::Allow => "passed",
            ResolvedAction::Block { .. } => "blocked",
            ResolvedAction::Warn { .. } => "warned",
            ResolvedAction::Log { .. } => "logged",
            ResolvedAction::Redact { .. } => "redacted",
        };
        headers.push(("X-Guardrails-Input-Result", result.to_string()));

        // Add violations header if any
        if !self.response.violations.is_empty() {
            let violations: Vec<String> = self
                .response
                .violations
                .iter()
                .map(|v| format!("{}:{}", v.category, v.severity))
                .collect();
            headers.push(("X-Guardrails-Violations", violations.join(",")));
        }

        // Add latency header if available
        if let Some(latency_ms) = self.response.latency_ms {
            headers.push(("X-Guardrails-Latency-Ms", latency_ms.to_string()));
        }

        headers
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Concurrent Execution
// ─────────────────────────────────────────────────────────────────────────────

/// Tracks which operation completed first in concurrent mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrentRaceWinner {
    /// Guardrails evaluation completed first.
    Guardrails,
    /// LLM provider call completed first.
    Llm,
    /// Guardrails timed out (LLM result used based on on_timeout config).
    GuardrailsTimedOut,
}

impl std::fmt::Display for ConcurrentRaceWinner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Guardrails => write!(f, "guardrails_first"),
            Self::Llm => write!(f, "llm_first"),
            Self::GuardrailsTimedOut => write!(f, "guardrails_timed_out"),
        }
    }
}

/// Result of a concurrent guardrails + LLM evaluation.
///
/// In concurrent mode, the guardrails and LLM are started simultaneously:
/// - If guardrails finish first and fail → cancel/discard LLM result, return error
/// - If guardrails finish first and pass → wait for LLM, return LLM result
/// - If LLM finishes first → wait for guardrails, then decide
/// - If guardrails timeout → behavior depends on `on_timeout` config
#[derive(Debug)]
pub struct ConcurrentEvaluationOutcome<T> {
    /// Which operation completed first.
    pub winner: ConcurrentRaceWinner,
    /// The guardrails result (if available).
    pub guardrails_result: Option<InputGuardrailsResult>,
    /// The LLM result (if available and not cancelled).
    pub llm_result: Option<T>,
    /// Whether the request was blocked by guardrails.
    #[allow(dead_code)] // Guardrail response field
    pub blocked: bool,
    /// Guardrails latency in milliseconds (if completed).
    pub guardrails_latency_ms: Option<u64>,
    /// LLM latency in milliseconds (if completed).
    pub llm_latency_ms: Option<u64>,
}

impl<T> ConcurrentEvaluationOutcome<T> {
    /// Creates headers for the concurrent evaluation result.
    pub fn to_headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = Vec::new();

        // Add concurrent mode header
        headers.push(("X-Guardrails-Mode", "concurrent".to_string()));
        headers.push(("X-Guardrails-Race-Winner", self.winner.to_string()));

        // Add guardrails headers if available
        if let Some(ref result) = self.guardrails_result {
            headers.extend(result.to_headers());
        }

        // Add latency headers
        if let Some(latency) = self.guardrails_latency_ms {
            headers.push(("X-Guardrails-Latency-Ms", latency.to_string()));
        }
        if let Some(latency) = self.llm_latency_ms {
            headers.push(("X-LLM-Latency-Ms", latency.to_string()));
        }

        headers
    }
}

/// Runs guardrails evaluation and an LLM call concurrently.
///
/// This function handles the race between guardrails and the LLM:
/// 1. If guardrails fail (block action) before LLM completes → return blocked error
/// 2. If LLM completes first → wait for guardrails result, then decide
/// 3. If guardrails timeout → behavior depends on `on_timeout` config
///
/// # Type Parameters
/// - `T`: The LLM result type
/// - `E`: The LLM error type (must be convertible to String for logging)
///
/// # Arguments
/// - `guardrails`: The input guardrails evaluator
/// - `guardrails_future`: The guardrails evaluation future
/// - `llm_future`: The LLM provider call future
///
/// # Returns
/// - `Ok(outcome)` with the evaluation outcome containing both results
/// - `Err(guardrails_error)` if guardrails blocked the request
#[instrument(
    name = "guardrails.concurrent",
    skip(guardrails, guardrails_future, llm_future),
    fields(
        provider = %guardrails.provider_name(),
        timeout_ms = guardrails.timeout().as_millis() as u64,
        winner = tracing::field::Empty,
        blocked = tracing::field::Empty,
        guardrails_latency_ms = tracing::field::Empty,
        llm_latency_ms = tracing::field::Empty,
    )
)]
pub async fn run_concurrent_evaluation<T, E, GF, LF>(
    guardrails: &InputGuardrails,
    guardrails_future: GF,
    llm_future: LF,
) -> Result<ConcurrentEvaluationOutcome<T>, GuardrailsError>
where
    GF: std::future::Future<Output = Result<InputGuardrailsResult, GuardrailsError>>,
    LF: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    use std::time::Instant;

    use tokio::select;

    let timeout = guardrails.timeout();
    let on_timeout = guardrails.on_timeout().clone();

    let start = Instant::now();

    // Pin both futures so we can poll them
    tokio::pin!(guardrails_future);
    tokio::pin!(llm_future);

    // Race the guardrails with a timeout against the LLM call
    let guardrails_with_timeout = tokio::time::timeout(timeout, &mut guardrails_future);

    // First race: see which completes first
    select! {
        // Guardrails completed (with or without timeout)
        guardrails_result = guardrails_with_timeout => {
            let guardrails_latency_ms = start.elapsed().as_millis() as u64;

            match guardrails_result {
                Ok(Ok(result)) => {
                    // Guardrails completed successfully
                    record_guardrails_concurrent_race("guardrails_first");
                    let span = tracing::Span::current();
                    span.record("winner", "guardrails_first");
                    span.record("guardrails_latency_ms", guardrails_latency_ms);

                    if result.is_blocked() {
                        // Guardrails blocked - cancel LLM and return error
                        span.record("blocked", true);
                        tracing::info!(
                            winner = "guardrails_first",
                            action = "blocked",
                            latency_ms = guardrails_latency_ms,
                            "Concurrent guardrails blocked content before LLM completed"
                        );
                        return Err(GuardrailsError::blocked_with_violations(
                            crate::guardrails::ContentSource::UserInput,
                            "Content blocked by input guardrails (concurrent mode)",
                            result.violations().to_vec(),
                        ));
                    }
                    span.record("blocked", false);

                    // Guardrails passed - now wait for LLM
                    tracing::debug!(
                        winner = "guardrails_first",
                        action = "passed",
                        latency_ms = guardrails_latency_ms,
                        "Concurrent guardrails passed, waiting for LLM"
                    );

                    match llm_future.await {
                        Ok(llm_result) => {
                            let llm_latency_ms = start.elapsed().as_millis() as u64;
                            span.record("llm_latency_ms", llm_latency_ms);
                            Ok(ConcurrentEvaluationOutcome {
                                winner: ConcurrentRaceWinner::Guardrails,
                                guardrails_result: Some(result),
                                llm_result: Some(llm_result),
                                blocked: false,
                                guardrails_latency_ms: Some(guardrails_latency_ms),
                                llm_latency_ms: Some(llm_latency_ms),
                            })
                        }
                        Err(e) => {
                            // LLM failed but guardrails passed - return LLM error
                            let llm_latency_ms = start.elapsed().as_millis() as u64;
                            span.record("llm_latency_ms", llm_latency_ms);
                            tracing::warn!(
                                llm_error = %e,
                                "LLM failed after guardrails passed in concurrent mode"
                            );
                            Ok(ConcurrentEvaluationOutcome {
                                winner: ConcurrentRaceWinner::Guardrails,
                                guardrails_result: Some(result),
                                llm_result: None,
                                blocked: false,
                                guardrails_latency_ms: Some(guardrails_latency_ms),
                                llm_latency_ms: Some(llm_latency_ms),
                            })
                        }
                    }
                }
                Ok(Err(e)) => {
                    // Guardrails provider error
                    let span = tracing::Span::current();
                    span.record("winner", "guardrails_first");
                    span.record("guardrails_latency_ms", guardrails_latency_ms);
                    tracing::warn!(
                        error = %e,
                        latency_ms = guardrails_latency_ms,
                        "Guardrails provider error in concurrent mode"
                    );
                    return Err(e);
                }
                Err(_timeout) => {
                    // Guardrails timed out
                    let guardrails_latency_ms = timeout.as_millis() as u64;
                    record_guardrails_concurrent_race("guardrails_timed_out");
                    let span = tracing::Span::current();
                    span.record("winner", "guardrails_timed_out");
                    span.record("guardrails_latency_ms", guardrails_latency_ms);

                    match on_timeout {
                        crate::config::GuardrailsTimeoutAction::Block => {
                            span.record("blocked", true);
                            tracing::warn!(
                                timeout_ms = guardrails_latency_ms,
                                "Concurrent guardrails timed out, blocking request"
                            );
                            return Err(GuardrailsError::timeout(
                                guardrails.provider_name(),
                                guardrails_latency_ms,
                            ));
                        }
                        crate::config::GuardrailsTimeoutAction::Allow => {
                            span.record("blocked", false);
                            // Timeout with allow - wait for LLM
                            tracing::info!(
                                timeout_ms = guardrails_latency_ms,
                                "Concurrent guardrails timed out, allowing request (on_timeout=allow)"
                            );

                            match llm_future.await {
                                Ok(llm_result) => {
                                    let llm_latency_ms = start.elapsed().as_millis() as u64;
                                    span.record("llm_latency_ms", llm_latency_ms);
                                    Ok(ConcurrentEvaluationOutcome {
                                        winner: ConcurrentRaceWinner::GuardrailsTimedOut,
                                        guardrails_result: None,
                                        llm_result: Some(llm_result),
                                        blocked: false,
                                        guardrails_latency_ms: Some(guardrails_latency_ms),
                                        llm_latency_ms: Some(llm_latency_ms),
                                    })
                                }
                                Err(e) => {
                                    let llm_latency_ms = start.elapsed().as_millis() as u64;
                                    span.record("llm_latency_ms", llm_latency_ms);
                                    tracing::warn!(
                                        llm_error = %e,
                                        "LLM failed after guardrails timed out in concurrent mode"
                                    );
                                    Ok(ConcurrentEvaluationOutcome {
                                        winner: ConcurrentRaceWinner::GuardrailsTimedOut,
                                        guardrails_result: None,
                                        llm_result: None,
                                        blocked: false,
                                        guardrails_latency_ms: Some(guardrails_latency_ms),
                                        llm_latency_ms: Some(llm_latency_ms),
                                    })
                                }
                            }
                        }
                    }
                }
            }
        }

        // LLM completed first
        llm_result = &mut llm_future => {
            let llm_latency_ms = start.elapsed().as_millis() as u64;
            record_guardrails_concurrent_race("llm_first");
            let span = tracing::Span::current();
            span.record("winner", "llm_first");
            span.record("llm_latency_ms", llm_latency_ms);

            tracing::debug!(
                winner = "llm_first",
                latency_ms = llm_latency_ms,
                "LLM completed first in concurrent mode, waiting for guardrails"
            );

            // LLM completed first - now we must wait for guardrails to complete
            // Apply timeout to remaining guardrails wait time
            let remaining_timeout = timeout.saturating_sub(start.elapsed());
            let guardrails_result = tokio::time::timeout(remaining_timeout, guardrails_future).await;

            let guardrails_latency_ms = start.elapsed().as_millis() as u64;

            match guardrails_result {
                Ok(Ok(result)) => {
                    span.record("guardrails_latency_ms", guardrails_latency_ms);
                    if result.is_blocked() {
                        // Guardrails blocked - discard LLM result
                        span.record("blocked", true);
                        tracing::info!(
                            winner = "llm_first",
                            action = "blocked",
                            guardrails_latency_ms,
                            llm_latency_ms,
                            "Guardrails blocked content after LLM completed"
                        );
                        return Err(GuardrailsError::blocked_with_violations(
                            crate::guardrails::ContentSource::UserInput,
                            "Content blocked by input guardrails (concurrent mode, LLM completed first)",
                            result.violations().to_vec(),
                        ));
                    }
                    span.record("blocked", false);

                    // Both completed, guardrails passed
                    match llm_result {
                        Ok(llm) => Ok(ConcurrentEvaluationOutcome {
                            winner: ConcurrentRaceWinner::Llm,
                            guardrails_result: Some(result),
                            llm_result: Some(llm),
                            blocked: false,
                            guardrails_latency_ms: Some(guardrails_latency_ms),
                            llm_latency_ms: Some(llm_latency_ms),
                        }),
                        Err(e) => {
                            tracing::warn!(
                                llm_error = %e,
                                "LLM result was an error in concurrent mode"
                            );
                            Ok(ConcurrentEvaluationOutcome {
                                winner: ConcurrentRaceWinner::Llm,
                                guardrails_result: Some(result),
                                llm_result: None,
                                blocked: false,
                                guardrails_latency_ms: Some(guardrails_latency_ms),
                                llm_latency_ms: Some(llm_latency_ms),
                            })
                        }
                    }
                }
                Ok(Err(e)) => {
                    // Guardrails provider error after LLM completed
                    span.record("guardrails_latency_ms", guardrails_latency_ms);
                    tracing::warn!(
                        error = %e,
                        "Guardrails provider error after LLM completed in concurrent mode"
                    );
                    Err(e)
                }
                Err(_timeout) => {
                    // Guardrails timed out after LLM completed
                    span.record("guardrails_latency_ms", guardrails_latency_ms);
                    match on_timeout {
                        crate::config::GuardrailsTimeoutAction::Block => {
                            span.record("blocked", true);
                            tracing::warn!(
                                timeout_ms = timeout.as_millis(),
                                "Guardrails timed out after LLM completed, blocking request"
                            );
                            return Err(GuardrailsError::timeout(
                                guardrails.provider_name(),
                                timeout.as_millis() as u64,
                            ));
                        }
                        crate::config::GuardrailsTimeoutAction::Allow => {
                            span.record("blocked", false);
                            tracing::info!(
                                timeout_ms = timeout.as_millis(),
                                "Guardrails timed out after LLM completed, allowing (on_timeout=allow)"
                            );
                            match llm_result {
                                Ok(llm) => Ok(ConcurrentEvaluationOutcome {
                                    winner: ConcurrentRaceWinner::Llm,
                                    guardrails_result: None,
                                    llm_result: Some(llm),
                                    blocked: false,
                                    guardrails_latency_ms: Some(guardrails_latency_ms),
                                    llm_latency_ms: Some(llm_latency_ms),
                                }),
                                Err(e) => {
                                    tracing::warn!(
                                        llm_error = %e,
                                        "LLM result was an error in concurrent mode (guardrails timed out)"
                                    );
                                    Ok(ConcurrentEvaluationOutcome {
                                        winner: ConcurrentRaceWinner::Llm,
                                        guardrails_result: None,
                                        llm_result: None,
                                        blocked: false,
                                        guardrails_latency_ms: Some(guardrails_latency_ms),
                                        llm_latency_ms: Some(llm_latency_ms),
                                    })
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Output Guardrails
// ─────────────────────────────────────────────────────────────────────────────

/// Output guardrails evaluator.
///
/// Evaluates LLM output content against guardrails policies before returning to the user.
/// This is used for post-response evaluation in non-streaming mode.
pub struct OutputGuardrails {
    /// The guardrails provider to use for evaluation.
    provider: Arc<dyn GuardrailsProvider>,
    /// Action executor for applying configured actions.
    action_executor: ActionExecutor,
    /// Retry configuration.
    retry_config: GuardrailsRetryConfig,
    /// Timeout for evaluation.
    timeout: Duration,
    /// Action to take on provider error.
    on_error: crate::config::GuardrailsErrorAction,
    /// Streaming evaluation mode.
    streaming_mode: crate::config::StreamingGuardrailsMode,
}

impl OutputGuardrails {
    /// Creates a new output guardrails evaluator from configuration.
    ///
    /// Returns `None` if guardrails are disabled or not configured.
    pub fn from_config(
        config: &GuardrailsConfig,
        http_client: &Client,
    ) -> Result<Option<Self>, GuardrailsError> {
        if !config.enabled {
            return Ok(None);
        }

        let Some(output_config) = &config.output else {
            return Ok(None);
        };

        if !output_config.enabled {
            return Ok(None);
        }

        let provider = create_provider(&output_config.provider, http_client)?;
        let action_executor = ActionExecutor::from_output_config(output_config);
        let retry_config = GuardrailsRetryConfig::default();

        Ok(Some(Self {
            provider,
            action_executor,
            retry_config,
            timeout: Duration::from_millis(output_config.timeout_ms),
            on_error: output_config.on_error.clone(),
            streaming_mode: output_config.streaming_mode.clone(),
        }))
    }

    /// Creates output guardrails from a specific output config (for testing).
    pub fn from_output_config(
        config: &OutputGuardrailsConfig,
        http_client: &Client,
    ) -> Result<Self, GuardrailsError> {
        let provider = create_provider(&config.provider, http_client)?;
        let action_executor = ActionExecutor::from_output_config(config);
        let retry_config = GuardrailsRetryConfig::default();

        Ok(Self {
            provider,
            action_executor,
            retry_config,
            timeout: Duration::from_millis(config.timeout_ms),
            on_error: config.on_error.clone(),
            streaming_mode: config.streaming_mode.clone(),
        })
    }

    /// Evaluates LLM response content against guardrails.
    ///
    /// Extracts text from the assistant's response and evaluates it.
    /// Returns the resolved action to take based on the evaluation result.
    #[instrument(skip(self, content), fields(provider = %self.provider.name()))]
    pub async fn evaluate_response(
        &self,
        content: &str,
        request_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<OutputGuardrailsResult, GuardrailsError> {
        if content.is_empty() {
            tracing::debug!("No text content to evaluate");
            return Ok(OutputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: content.to_string(),
            });
        }

        // Build the guardrails request for LLM output
        let mut request = GuardrailsRequest::llm_output(content);
        if let Some(id) = request_id {
            request = request.with_request_id(id);
        }
        if let Some(id) = user_id {
            request = request.with_user_id(id);
        }

        let start = std::time::Instant::now();

        // Evaluate with timeout
        let evaluation_result =
            tokio::time::timeout(self.timeout, self.evaluate_with_retry(&request)).await;

        let latency_ms = start.elapsed().as_millis() as u64;

        let response = match evaluation_result {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                // Provider error - record error metrics
                record_guardrails_error(self.provider.name(), "output", e.error_type_for_metrics());
                record_guardrails_evaluation(
                    self.provider.name(),
                    "output",
                    "error",
                    latency_ms as f64 / 1000.0,
                );
                return self.handle_provider_error(e, content);
            }
            Err(_) => {
                // Timeout - record timeout metrics
                record_guardrails_timeout(self.provider.name(), "output");
                record_guardrails_evaluation(
                    self.provider.name(),
                    "output",
                    "timeout",
                    latency_ms as f64 / 1000.0,
                );
                return self.handle_timeout(content);
            }
        };

        // Apply configured actions
        let action = self.action_executor.resolve_action(&response, content);
        let result_label = resolved_action_label(&action);

        // Record evaluation metrics
        record_evaluation_metrics(
            self.provider.name(),
            "output",
            result_label,
            Some(latency_ms),
            &response.violations,
        );

        // Record span events for each violation for distributed tracing
        for violation in &response.violations {
            tracing::event!(
                tracing::Level::INFO,
                category = %violation.category,
                severity = %violation.severity,
                confidence = violation.confidence,
                message = violation.message.as_deref().unwrap_or(""),
                "Output guardrails violation detected"
            );
        }

        tracing::debug!(
            passed = response.passed,
            violations = response.violations.len(),
            action = ?action,
            "Output guardrails evaluation complete"
        );

        Ok(OutputGuardrailsResult {
            action,
            response,
            evaluated_text: content.to_string(),
        })
    }

    /// Evaluates the request with retry logic.
    async fn evaluate_with_retry(
        &self,
        request: &GuardrailsRequest,
    ) -> Result<GuardrailsResponse, GuardrailsError> {
        let max_attempts = if self.retry_config.enabled {
            self.retry_config.max_retries + 1
        } else {
            1
        };

        let mut last_error = None;

        for attempt in 0..max_attempts {
            match self.provider.evaluate(request).await {
                Ok(response) => {
                    if attempt > 0 {
                        tracing::debug!(
                            provider = self.provider.name(),
                            attempt = attempt + 1,
                            "Output guardrails evaluation succeeded after retry"
                        );
                    }
                    return Ok(response);
                }
                Err(error) => {
                    if error.is_retryable() && attempt < max_attempts - 1 {
                        let delay = self.retry_config.delay_for_attempt(attempt);
                        tracing::warn!(
                            provider = self.provider.name(),
                            error = %error,
                            attempt = attempt + 1,
                            max_attempts = max_attempts,
                            delay_ms = delay.as_millis(),
                            "Retryable output guardrails error, will retry after delay"
                        );
                        tokio::time::sleep(delay).await;
                        last_error = Some(error);
                        continue;
                    }

                    if attempt > 0 {
                        tracing::warn!(
                            provider = self.provider.name(),
                            error = %error,
                            attempts = attempt + 1,
                            "Output guardrails evaluation failed after all retry attempts"
                        );
                    }
                    return Err(error);
                }
            }
        }

        // This should only be reached if max_attempts is 0, which shouldn't happen
        Err(last_error.unwrap_or_else(|| GuardrailsError::internal("No evaluation attempts made")))
    }

    /// Handles a provider error based on configuration.
    fn handle_provider_error(
        &self,
        error: GuardrailsError,
        text: &str,
    ) -> Result<OutputGuardrailsResult, GuardrailsError> {
        use crate::config::GuardrailsErrorAction;

        tracing::warn!(
            error = %error,
            on_error = ?self.on_error,
            "Output guardrails provider error"
        );

        match self.on_error {
            GuardrailsErrorAction::Block => Err(error),
            GuardrailsErrorAction::Allow => Ok(OutputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: text.to_string(),
            }),
            GuardrailsErrorAction::LogAndAllow => {
                tracing::error!(
                    error = %error,
                    "Output guardrails provider error - allowing response (log_and_allow)"
                );
                Ok(OutputGuardrailsResult {
                    action: ResolvedAction::Allow,
                    response: GuardrailsResponse::passed(),
                    evaluated_text: text.to_string(),
                })
            }
        }
    }

    /// Handles a timeout based on configuration.
    fn handle_timeout(&self, text: &str) -> Result<OutputGuardrailsResult, GuardrailsError> {
        use crate::config::GuardrailsErrorAction;

        tracing::warn!(
            timeout_ms = %self.timeout.as_millis(),
            on_error = ?self.on_error,
            "Output guardrails evaluation timed out"
        );

        // For output guardrails, timeout is handled via on_error (there's no on_timeout config)
        match self.on_error {
            GuardrailsErrorAction::Block => Err(GuardrailsError::timeout(
                self.provider.name(),
                self.timeout.as_millis() as u64,
            )),
            GuardrailsErrorAction::Allow => Ok(OutputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: text.to_string(),
            }),
            GuardrailsErrorAction::LogAndAllow => {
                tracing::error!(
                    timeout_ms = %self.timeout.as_millis(),
                    "Output guardrails evaluation timed out - allowing response (log_and_allow)"
                );
                Ok(OutputGuardrailsResult {
                    action: ResolvedAction::Allow,
                    response: GuardrailsResponse::passed(),
                    evaluated_text: text.to_string(),
                })
            }
        }
    }

    /// Returns the provider name.
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// Returns the provider for streaming use.
    pub fn provider(&self) -> Arc<dyn GuardrailsProvider> {
        self.provider.clone()
    }

    /// Returns the action executor for streaming use.
    pub fn action_executor(&self) -> ActionExecutor {
        self.action_executor.clone()
    }

    /// Returns the on_error action for streaming use.
    pub fn on_error(&self) -> crate::config::GuardrailsErrorAction {
        self.on_error.clone()
    }

    /// Returns the streaming evaluation mode.
    pub fn streaming_mode(&self) -> crate::config::StreamingGuardrailsMode {
        self.streaming_mode.clone()
    }
}

/// Result of output guardrails evaluation.
#[derive(Debug, Clone)]
pub struct OutputGuardrailsResult {
    /// The resolved action to take.
    pub action: ResolvedAction,
    /// The raw guardrails response.
    pub response: GuardrailsResponse,
    /// The text that was evaluated.
    pub evaluated_text: String,
}

impl OutputGuardrailsResult {
    /// Returns true if the content should be blocked.
    pub fn is_blocked(&self) -> bool {
        self.action.is_blocked()
    }

    /// Returns true if the content was modified (redacted).
    pub fn is_modified(&self) -> bool {
        self.action.is_modified()
    }

    /// Returns the modified content if redaction was applied.
    pub fn modified_content(&self) -> Option<&str> {
        match &self.action {
            ResolvedAction::Redact {
                modified_content, ..
            } => Some(modified_content),
            _ => None,
        }
    }

    /// Returns the violations found during evaluation.
    pub fn violations(&self) -> &[Violation] {
        self.action.violations()
    }

    /// Returns a string label for the action result (for metrics).
    pub fn result_label(&self) -> &'static str {
        resolved_action_label(&self.action)
    }

    /// Creates response headers for the output guardrails result.
    pub fn to_headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = Vec::new();

        // Add result header
        let result = match &self.action {
            ResolvedAction::Allow => "passed",
            ResolvedAction::Block { .. } => "blocked",
            ResolvedAction::Warn { .. } => "warned",
            ResolvedAction::Log { .. } => "logged",
            ResolvedAction::Redact { .. } => "redacted",
        };
        headers.push(("X-Guardrails-Output-Result", result.to_string()));

        // Add violations header if any
        if !self.response.violations.is_empty() {
            let violations: Vec<String> = self
                .response
                .violations
                .iter()
                .map(|v| format!("{}:{}", v.category, v.severity))
                .collect();
            headers.push(("X-Guardrails-Output-Violations", violations.join(",")));
        }

        // Add latency header if available
        if let Some(latency_ms) = self.response.latency_ms {
            headers.push(("X-Guardrails-Output-Latency-Ms", latency_ms.to_string()));
        }

        headers
    }
}

/// Extracts assistant content from a chat completion response JSON.
///
/// This function parses the response body and extracts the content from the
/// first choice's message. Returns an empty string if the content cannot be extracted.
pub fn extract_assistant_content_from_response(body: &[u8]) -> String {
    // Parse the response body as JSON
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_slice(body) else {
        return String::new();
    };

    // Extract content from choices[0].message.content
    if let Some(choices) = json.get("choices").and_then(|c| c.as_array())
        && let Some(first_choice) = choices.first()
        && let Some(message) = first_choice.get("message")
        && let Some(content) = message.get("content").and_then(|c| c.as_str())
    {
        return content.to_string();
    }

    String::new()
}

/// Extracts all text content from chat completion messages.
///
/// Concatenates text from:
/// - System messages
/// - User messages (text content or text parts)
/// - Assistant messages (content, reasoning)
/// - Tool messages
/// - Developer messages
pub fn extract_text_from_messages(messages: &[Message]) -> String {
    let mut parts = Vec::new();

    for message in messages {
        match message {
            Message::System { content, .. } => {
                if let Some(text) = extract_text_from_content(content) {
                    parts.push(text);
                }
            }
            Message::User { content, .. } => {
                if let Some(text) = extract_text_from_content(content) {
                    parts.push(text);
                }
            }
            Message::Assistant {
                content, reasoning, ..
            } => {
                if let Some(c) = content
                    && let Some(text) = extract_text_from_content(c)
                {
                    parts.push(text);
                }
                if let Some(r) = reasoning {
                    parts.push(r.clone());
                }
            }
            Message::Tool { content, .. } => {
                if let Some(text) = extract_text_from_content(content) {
                    parts.push(text);
                }
            }
            Message::Developer { content, .. } => {
                if let Some(text) = extract_text_from_content(content) {
                    parts.push(text);
                }
            }
        }
    }

    parts.join("\n\n")
}

/// Extracts text from message content.
fn extract_text_from_content(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Text(text) => {
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        MessageContent::Parts(parts) => {
            let text_parts: Vec<String> = parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text, .. } => {
                        if text.is_empty() {
                            None
                        } else {
                            Some(text.clone())
                        }
                    }
                    _ => None, // Skip image, audio, video parts
                })
                .collect();

            if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Completions API Text Extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extracts text content from a completion prompt.
///
/// Handles all prompt formats:
/// - Single text string
/// - Array of text strings
/// - Token arrays (skipped, not textual)
pub fn extract_text_from_completion_prompt(prompt: &CompletionPrompt) -> String {
    match prompt {
        CompletionPrompt::Text(text) => text.clone(),
        CompletionPrompt::TextArray(texts) => texts.join("\n"),
        // Token arrays don't have meaningful text representation
        CompletionPrompt::Tokens(_) | CompletionPrompt::TokenArrays(_) => String::new(),
    }
}

/// Extracts text content from a completion payload.
///
/// Extracts text from the prompt field.
pub fn extract_text_from_completion_payload(payload: &CreateCompletionPayload) -> String {
    extract_text_from_completion_prompt(&payload.prompt)
}

/// Extracts text content from a completion response JSON.
///
/// Extracts text from `choices[].text` fields.
pub fn extract_text_from_completion_response(body: &[u8]) -> String {
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_slice(body) else {
        return String::new();
    };

    let Some(choices) = json.get("choices").and_then(|c| c.as_array()) else {
        return String::new();
    };

    let texts: Vec<&str> = choices
        .iter()
        .filter_map(|choice| choice.get("text").and_then(|t| t.as_str()))
        .collect();

    texts.join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────
// Responses API Text Extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extracts text content from a responses payload input.
///
/// Handles all input formats:
/// - Simple text string
/// - Array of input items (messages, function calls, etc.)
pub fn extract_text_from_responses_input(input: &ResponsesInput) -> String {
    match input {
        ResponsesInput::Text(text) => text.clone(),
        ResponsesInput::Items(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(extract_text_from_input_item)
                .collect();
            parts.join("\n\n")
        }
    }
}

/// Extracts text from a single responses input item.
fn extract_text_from_input_item(item: &ResponsesInputItem) -> Option<String> {
    match item {
        ResponsesInputItem::EasyMessage(msg) => extract_text_from_easy_message(msg),
        ResponsesInputItem::MessageItem(msg) => {
            let texts: Vec<String> = msg
                .content
                .iter()
                .filter_map(extract_text_from_input_content_item)
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        ResponsesInputItem::Reasoning(reasoning) => {
            // Extract text from reasoning content
            let mut parts = Vec::new();
            if let Some(content) = &reasoning.content {
                for item in content {
                    parts.push(item.text.clone());
                }
            }
            for summary in &reasoning.summary {
                parts.push(summary.text.clone());
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        ResponsesInputItem::FunctionCall(fc) => Some(format!("{}({})", fc.name, fc.arguments)),
        ResponsesInputItem::FunctionCallOutput(output) => Some(output.output.clone()),
        ResponsesInputItem::OutputMessage(msg) => {
            let texts: Vec<String> = msg
                .content
                .iter()
                .filter_map(extract_text_from_output_content_item)
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        ResponsesInputItem::OutputFunctionCall(fc) => {
            Some(format!("{}({})", fc.name, fc.arguments))
        }
        // Tool calls don't have meaningful text content for guardrails
        ResponsesInputItem::WebSearchCall(_)
        | ResponsesInputItem::FileSearchCall(_)
        | ResponsesInputItem::ShellCall(_)
        | ResponsesInputItem::ShellCallOutput(_)
        | ResponsesInputItem::McpListTools(_)
        | ResponsesInputItem::McpCall(_)
        | ResponsesInputItem::McpApprovalRequest(_)
        | ResponsesInputItem::McpApprovalResponse(_)
        | ResponsesInputItem::ToolSearchCall(_)
        | ResponsesInputItem::ToolSearchOutput(_)
        | ResponsesInputItem::Compaction(_)
        | ResponsesInputItem::ImageGeneration(_) => None,
    }
}

/// Extracts text from an easy input message.
fn extract_text_from_easy_message(msg: &EasyInputMessage) -> Option<String> {
    match &msg.content {
        EasyInputMessageContent::Text(text) => {
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        EasyInputMessageContent::Parts(parts) => {
            let texts: Vec<String> = parts
                .iter()
                .filter_map(extract_text_from_input_content_item)
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
    }
}

/// Extracts text from an input content item.
fn extract_text_from_input_content_item(item: &ResponseInputContentItem) -> Option<String> {
    match item {
        ResponseInputContentItem::InputText { text, .. } => {
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        // Skip non-text content
        ResponseInputContentItem::InputImage { .. }
        | ResponseInputContentItem::InputFile { .. }
        | ResponseInputContentItem::InputAudio { .. } => None,
    }
}

/// Extracts text from an output content item.
fn extract_text_from_output_content_item(item: &OutputMessageContentItem) -> Option<String> {
    match item {
        OutputMessageContentItem::OutputText { text, .. } => {
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        OutputMessageContentItem::Refusal { refusal } => {
            if refusal.is_empty() {
                None
            } else {
                Some(refusal.clone())
            }
        }
    }
}

/// Extracts text content from a responses payload.
///
/// Extracts text from the input field and instructions.
pub fn extract_text_from_responses_payload(payload: &CreateResponsesPayload) -> String {
    let mut parts = Vec::new();

    // Extract from instructions (system prompt)
    if let Some(instructions) = &payload.instructions
        && !instructions.is_empty()
    {
        parts.push(instructions.clone());
    }

    // Extract from input
    if let Some(input) = &payload.input {
        let input_text = extract_text_from_responses_input(input);
        if !input_text.is_empty() {
            parts.push(input_text);
        }
    }

    parts.join("\n\n")
}

/// Extracts text content from a responses API response JSON.
///
/// Extracts text from `output[]` items (messages, reasoning).
pub fn extract_text_from_responses_response(body: &[u8]) -> String {
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_slice(body) else {
        return String::new();
    };

    let mut parts = Vec::new();

    // Extract from output_text field if present
    if let Some(output_text) = json.get("output_text").and_then(|t| t.as_str())
        && !output_text.is_empty()
    {
        return output_text.to_string();
    }

    // Fall back to extracting from output array
    if let Some(output) = json.get("output").and_then(|o| o.as_array()) {
        for item in output {
            if let Some(text) = extract_text_from_output_json(item) {
                parts.push(text);
            }
        }
    }

    parts.join("\n\n")
}

/// Extracts text from an output item JSON.
fn extract_text_from_output_json(item: &serde_json::Value) -> Option<String> {
    let item_type = item.get("type").and_then(|t| t.as_str())?;

    match item_type {
        "message" => {
            // Extract from content array
            let content = item.get("content").and_then(|c| c.as_array())?;
            let texts: Vec<&str> = content
                .iter()
                .filter_map(|c| {
                    let content_type = c.get("type").and_then(|t| t.as_str())?;
                    match content_type {
                        "output_text" => c.get("text").and_then(|t| t.as_str()),
                        "refusal" => c.get("refusal").and_then(|r| r.as_str()),
                        _ => None,
                    }
                })
                .filter(|s| !s.is_empty())
                .collect();

            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        "reasoning" => {
            let mut parts = Vec::new();

            // Extract from content
            if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                for c in content {
                    if let Some(text) = c.get("text").and_then(|t| t.as_str())
                        && !text.is_empty()
                    {
                        parts.push(text);
                    }
                }
            }

            // Extract from summary
            if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                for s in summary {
                    if let Some(text) = s.get("text").and_then(|t| t.as_str())
                        && !text.is_empty()
                    {
                        parts.push(text);
                    }
                }
            }

            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        "function_call" => {
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = item.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
            if name.is_empty() && args.is_empty() {
                None
            } else {
                Some(format!("{}({})", name, args))
            }
        }
        _ => None,
    }
}

/// Creates a guardrails provider from configuration.
fn create_provider(
    config: &GuardrailsProviderConfig,
    http_client: &Client,
) -> Result<Arc<dyn GuardrailsProvider>, GuardrailsError> {
    match config {
        GuardrailsProviderConfig::OpenaiModeration {
            api_key,
            base_url,
            model,
        } => {
            let api_key = api_key.clone().ok_or_else(|| {
                GuardrailsError::config_error("OpenAI Moderation requires an API key")
            })?;
            let provider = OpenAIModerationProvider::with_base_url(
                http_client.clone(),
                api_key,
                base_url,
                model,
            );
            Ok(Arc::new(provider))
        }

        #[cfg(feature = "provider-bedrock")]
        GuardrailsProviderConfig::Bedrock {
            guardrail_id,
            guardrail_version,
            region,
            access_key_id,
            secret_access_key,
            trace_enabled,
        } => {
            // Get region from config or environment
            let region = region
                .clone()
                .or_else(|| std::env::var("AWS_REGION").ok())
                .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
                .ok_or_else(|| {
                    GuardrailsError::config_error(
                        "Bedrock Guardrails requires a region. Set region in config or AWS_REGION environment variable."
                    )
                })?;

            // Determine credential source
            let credential_source = match (access_key_id.clone(), secret_access_key.clone()) {
                (Some(key_id), Some(secret)) => crate::config::AwsCredentials::Static {
                    access_key_id: key_id,
                    secret_access_key: secret,
                    session_token: None,
                },
                _ => crate::config::AwsCredentials::Default,
            };

            let provider = BedrockGuardrailsProvider::new(
                http_client.clone(),
                guardrail_id.clone(),
                guardrail_version.clone(),
                region,
                credential_source,
                *trace_enabled,
            );
            Ok(Arc::new(provider))
        }

        GuardrailsProviderConfig::AzureContentSafety {
            endpoint,
            api_key,
            api_version,
            thresholds,
            blocklist_names,
        } => {
            let mut provider = AzureContentSafetyProvider::new(
                http_client.clone(),
                endpoint,
                api_key,
                api_version,
            );

            if !thresholds.is_empty() {
                provider = provider.with_thresholds(thresholds.clone());
            }
            if !blocklist_names.is_empty() {
                provider = provider.with_blocklists(blocklist_names.clone());
            }

            Ok(Arc::new(provider))
        }

        GuardrailsProviderConfig::Blocklist {
            patterns,
            case_insensitive,
        } => {
            let provider = BlocklistProvider::new(patterns.clone(), *case_insensitive)?;
            Ok(Arc::new(provider))
        }

        GuardrailsProviderConfig::PiiRegex {
            email,
            phone,
            ssn,
            credit_card,
            ip_address,
            date_of_birth,
        } => {
            use super::pii_regex::{PiiRegexConfig, PiiRegexProvider};
            let config = PiiRegexConfig {
                email: *email,
                phone: *phone,
                ssn: *ssn,
                credit_card: *credit_card,
                ip_address: *ip_address,
                date_of_birth: *date_of_birth,
            };
            let provider = PiiRegexProvider::new(config)?;
            Ok(Arc::new(provider))
        }

        GuardrailsProviderConfig::ContentLimits {
            max_characters,
            max_words,
            max_lines,
        } => {
            use super::content_limits::{ContentLimitsConfig, ContentLimitsProvider};
            let config = ContentLimitsConfig {
                max_characters: *max_characters,
                max_words: *max_words,
                max_lines: *max_lines,
            };
            let provider = ContentLimitsProvider::new(config);
            Ok(Arc::new(provider))
        }

        GuardrailsProviderConfig::Custom(custom_config) => {
            let provider = CustomHttpProvider::from_config(http_client.clone(), custom_config)?;
            Ok(Arc::new(provider))
        }
    }
}

/// Returns a string label for a resolved action (for metrics).
fn resolved_action_label(action: &ResolvedAction) -> &'static str {
    match action {
        ResolvedAction::Allow => "passed",
        ResolvedAction::Block { .. } => "blocked",
        ResolvedAction::Warn { .. } => "warned",
        ResolvedAction::Log { .. } => "logged",
        ResolvedAction::Redact { .. } => "redacted",
    }
}

/// Records metrics for a guardrails evaluation result.
fn record_evaluation_metrics(
    provider: &str,
    stage: &str,
    result: &str,
    latency_ms: Option<u64>,
    violations: &[Violation],
) {
    let latency_secs = latency_ms.map(|ms| ms as f64 / 1000.0).unwrap_or(0.0);
    record_guardrails_evaluation(provider, stage, result, latency_secs);

    // Record individual violations
    for violation in violations {
        record_guardrails_violation(
            provider,
            &violation.category.to_string(),
            &violation.severity.to_string(),
            result,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{
        Message, MessageContent,
        chat_completion::{ContentPart, ImageUrl},
    };

    #[test]
    fn test_extract_text_from_simple_user_message() {
        let messages = vec![Message::User {
            content: MessageContent::Text("Hello, how are you?".to_string()),
            name: None,
        }];

        let text = extract_text_from_messages(&messages);
        assert_eq!(text, "Hello, how are you?");
    }

    #[test]
    fn test_extract_text_from_multiple_messages() {
        let messages = vec![
            Message::System {
                content: MessageContent::Text("You are a helpful assistant.".to_string()),
                name: None,
            },
            Message::User {
                content: MessageContent::Text("What is the weather?".to_string()),
                name: None,
            },
            Message::Assistant {
                content: Some(MessageContent::Text("The weather is sunny.".to_string())),
                name: None,
                tool_calls: None,
                refusal: None,
                reasoning: None,
            },
            Message::User {
                content: MessageContent::Text("Thanks!".to_string()),
                name: None,
            },
        ];

        let text = extract_text_from_messages(&messages);
        assert!(text.contains("You are a helpful assistant."));
        assert!(text.contains("What is the weather?"));
        assert!(text.contains("The weather is sunny."));
        assert!(text.contains("Thanks!"));
    }

    #[test]
    fn test_extract_text_from_multipart_message() {
        let messages = vec![Message::User {
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "What is in this image?".to_string(),
                    cache_control: None,
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "https://example.com/image.jpg".to_string(),
                        detail: None,
                    },
                    cache_control: None,
                },
                ContentPart::Text {
                    text: "Please describe it.".to_string(),
                    cache_control: None,
                },
            ]),
            name: None,
        }];

        let text = extract_text_from_messages(&messages);
        assert!(text.contains("What is in this image?"));
        assert!(text.contains("Please describe it."));
        assert!(!text.contains("https://")); // Image URL should not be included
    }

    #[test]
    fn test_extract_text_from_assistant_with_reasoning() {
        let messages = vec![Message::Assistant {
            content: Some(MessageContent::Text("The answer is 42.".to_string())),
            name: None,
            tool_calls: None,
            refusal: None,
            reasoning: Some("I need to think about this carefully...".to_string()),
        }];

        let text = extract_text_from_messages(&messages);
        assert!(text.contains("The answer is 42."));
        assert!(text.contains("I need to think about this carefully..."));
    }

    #[test]
    fn test_extract_text_empty_messages() {
        let messages: Vec<Message> = vec![];
        let text = extract_text_from_messages(&messages);
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_text_empty_content() {
        let messages = vec![Message::User {
            content: MessageContent::Text(String::new()),
            name: None,
        }];

        let text = extract_text_from_messages(&messages);
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_text_tool_message() {
        let messages = vec![Message::Tool {
            content: MessageContent::Text(r#"{"result": "success"}"#.to_string()),
            tool_call_id: "call_123".to_string(),
        }];

        let text = extract_text_from_messages(&messages);
        assert!(text.contains("success"));
    }

    #[test]
    fn test_input_guardrails_result_headers_passed() {
        let result = InputGuardrailsResult {
            action: ResolvedAction::Allow,
            response: GuardrailsResponse::passed().with_latency(50),
            evaluated_text: "test".to_string(),
        };

        let headers = result.to_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Input-Result" && v == "passed")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Latency-Ms" && v == "50")
        );
        assert!(!headers.iter().any(|(k, _)| *k == "X-Guardrails-Violations"));
    }

    #[test]
    fn test_input_guardrails_result_headers_blocked() {
        use super::super::{Category, Severity, Violation};

        let violations = vec![Violation::new(Category::Hate, Severity::High, 0.95)];
        let result = InputGuardrailsResult {
            action: ResolvedAction::Block {
                reason: "Content blocked".to_string(),
                violations: violations.clone(),
            },
            response: GuardrailsResponse::with_violations(violations),
            evaluated_text: "test".to_string(),
        };

        let headers = result.to_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Input-Result" && v == "blocked")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Violations" && v.contains("hate"))
        );
    }

    #[test]
    fn test_input_guardrails_result_is_blocked() {
        let blocked_result = InputGuardrailsResult {
            action: ResolvedAction::Block {
                reason: "test".to_string(),
                violations: vec![],
            },
            response: GuardrailsResponse::passed(),
            evaluated_text: "test".to_string(),
        };
        assert!(blocked_result.is_blocked());

        let allowed_result = InputGuardrailsResult {
            action: ResolvedAction::Allow,
            response: GuardrailsResponse::passed(),
            evaluated_text: "test".to_string(),
        };
        assert!(!allowed_result.is_blocked());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Output Guardrails Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_extract_assistant_content_from_response_valid() {
        let response_body = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "gpt-4",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello! How can I help you today?"
                    },
                    "finish_reason": "stop"
                }
            ]
        }"#;

        let content = extract_assistant_content_from_response(response_body.as_bytes());
        assert_eq!(content, "Hello! How can I help you today?");
    }

    #[test]
    fn test_extract_assistant_content_from_response_empty_choices() {
        let response_body = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": []
        }"#;

        let content = extract_assistant_content_from_response(response_body.as_bytes());
        assert!(content.is_empty());
    }

    #[test]
    fn test_extract_assistant_content_from_response_null_content() {
        let response_body = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null
                    },
                    "finish_reason": "stop"
                }
            ]
        }"#;

        let content = extract_assistant_content_from_response(response_body.as_bytes());
        assert!(content.is_empty());
    }

    #[test]
    fn test_extract_assistant_content_from_response_invalid_json() {
        let response_body = b"not valid json";
        let content = extract_assistant_content_from_response(response_body);
        assert!(content.is_empty());
    }

    #[test]
    fn test_extract_assistant_content_from_response_missing_message() {
        let response_body = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [
                {
                    "index": 0,
                    "finish_reason": "stop"
                }
            ]
        }"#;

        let content = extract_assistant_content_from_response(response_body.as_bytes());
        assert!(content.is_empty());
    }

    #[test]
    fn test_output_guardrails_result_headers_passed() {
        let result = OutputGuardrailsResult {
            action: ResolvedAction::Allow,
            response: GuardrailsResponse::passed().with_latency(75),
            evaluated_text: "test output".to_string(),
        };

        let headers = result.to_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Output-Result" && v == "passed")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Output-Latency-Ms" && v == "75")
        );
        assert!(
            !headers
                .iter()
                .any(|(k, _)| *k == "X-Guardrails-Output-Violations")
        );
    }

    #[test]
    fn test_output_guardrails_result_headers_blocked() {
        use super::super::{Category, Severity, Violation};

        let violations = vec![Violation::new(Category::Violence, Severity::Critical, 0.98)];
        let result = OutputGuardrailsResult {
            action: ResolvedAction::Block {
                reason: "Violent content".to_string(),
                violations: violations.clone(),
            },
            response: GuardrailsResponse::with_violations(violations),
            evaluated_text: "violent content here".to_string(),
        };

        let headers = result.to_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Output-Result" && v == "blocked")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Output-Violations" && v.contains("violence"))
        );
    }

    #[test]
    fn test_output_guardrails_result_headers_redacted() {
        use super::super::{Category, Severity, Violation};

        let violations = vec![Violation::new(Category::PiiEmail, Severity::Medium, 0.85)];
        let result = OutputGuardrailsResult {
            action: ResolvedAction::Redact {
                original_content: "Contact me at test@example.com".to_string(),
                modified_content: "Contact me at [REDACTED]".to_string(),
                violations: violations.clone(),
            },
            response: GuardrailsResponse::with_violations(violations),
            evaluated_text: "Contact me at test@example.com".to_string(),
        };

        let headers = result.to_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Output-Result" && v == "redacted")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Output-Violations" && v.contains("pii_email"))
        );
    }

    #[test]
    fn test_output_guardrails_result_is_blocked() {
        let blocked_result = OutputGuardrailsResult {
            action: ResolvedAction::Block {
                reason: "test".to_string(),
                violations: vec![],
            },
            response: GuardrailsResponse::passed(),
            evaluated_text: "test".to_string(),
        };
        assert!(blocked_result.is_blocked());

        let allowed_result = OutputGuardrailsResult {
            action: ResolvedAction::Allow,
            response: GuardrailsResponse::passed(),
            evaluated_text: "test".to_string(),
        };
        assert!(!allowed_result.is_blocked());
    }

    #[test]
    fn test_output_guardrails_result_is_modified() {
        let redacted_result = OutputGuardrailsResult {
            action: ResolvedAction::Redact {
                original_content: "original".to_string(),
                modified_content: "[REDACTED]".to_string(),
                violations: vec![],
            },
            response: GuardrailsResponse::passed(),
            evaluated_text: "original".to_string(),
        };
        assert!(redacted_result.is_modified());
        assert_eq!(redacted_result.modified_content(), Some("[REDACTED]"));

        let allowed_result = OutputGuardrailsResult {
            action: ResolvedAction::Allow,
            response: GuardrailsResponse::passed(),
            evaluated_text: "test".to_string(),
        };
        assert!(!allowed_result.is_modified());
        assert!(allowed_result.modified_content().is_none());
    }

    #[test]
    fn test_output_guardrails_result_violations() {
        use super::super::{Category, Severity, Violation};

        let violations = vec![
            Violation::new(Category::Hate, Severity::High, 0.9),
            Violation::new(Category::Violence, Severity::Medium, 0.7),
        ];

        let result = OutputGuardrailsResult {
            action: ResolvedAction::Warn {
                violations: violations.clone(),
            },
            response: GuardrailsResponse::with_violations(violations.clone()),
            evaluated_text: "test".to_string(),
        };

        assert_eq!(result.violations().len(), 2);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Completions API Extraction Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_extract_text_from_completion_prompt_single_text() {
        let prompt = CompletionPrompt::Text("Hello, world!".to_string());
        let text = extract_text_from_completion_prompt(&prompt);
        assert_eq!(text, "Hello, world!");
    }

    #[test]
    fn test_extract_text_from_completion_prompt_text_array() {
        let prompt = CompletionPrompt::TextArray(vec![
            "First prompt.".to_string(),
            "Second prompt.".to_string(),
            "Third prompt.".to_string(),
        ]);
        let text = extract_text_from_completion_prompt(&prompt);
        assert_eq!(text, "First prompt.\nSecond prompt.\nThird prompt.");
    }

    #[test]
    fn test_extract_text_from_completion_prompt_tokens() {
        let prompt = CompletionPrompt::Tokens(vec![1.0, 2.0, 3.0]);
        let text = extract_text_from_completion_prompt(&prompt);
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_text_from_completion_prompt_token_arrays() {
        let prompt = CompletionPrompt::TokenArrays(vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
        let text = extract_text_from_completion_prompt(&prompt);
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_text_from_completion_response_valid() {
        let response = r#"{
            "id": "cmpl-123",
            "object": "text_completion",
            "created": 1234567890,
            "model": "gpt-3.5-turbo-instruct",
            "choices": [
                {
                    "text": "This is the completed text.",
                    "index": 0,
                    "logprobs": null,
                    "finish_reason": "stop"
                }
            ]
        }"#;

        let text = extract_text_from_completion_response(response.as_bytes());
        assert_eq!(text, "This is the completed text.");
    }

    #[test]
    fn test_extract_text_from_completion_response_multiple_choices() {
        let response = r#"{
            "id": "cmpl-123",
            "object": "text_completion",
            "choices": [
                {"text": "First completion.", "index": 0, "finish_reason": "stop"},
                {"text": "Second completion.", "index": 1, "finish_reason": "stop"}
            ]
        }"#;

        let text = extract_text_from_completion_response(response.as_bytes());
        assert_eq!(text, "First completion.\nSecond completion.");
    }

    #[test]
    fn test_extract_text_from_completion_response_empty_choices() {
        let response = r#"{"id": "cmpl-123", "choices": []}"#;
        let text = extract_text_from_completion_response(response.as_bytes());
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_text_from_completion_response_invalid_json() {
        let response = b"not valid json";
        let text = extract_text_from_completion_response(response);
        assert!(text.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Responses API Extraction Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_extract_text_from_responses_input_simple_text() {
        let input = ResponsesInput::Text("What is the capital of France?".to_string());
        let text = extract_text_from_responses_input(&input);
        assert_eq!(text, "What is the capital of France?");
    }

    #[test]
    fn test_extract_text_from_responses_input_easy_message() {
        use crate::api_types::responses::EasyInputMessageRole;

        let input =
            ResponsesInput::Items(vec![ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: None,
                role: EasyInputMessageRole::User,
                content: EasyInputMessageContent::Text("Hello!".to_string()),
            })]);

        let text = extract_text_from_responses_input(&input);
        assert_eq!(text, "Hello!");
    }

    #[test]
    fn test_extract_text_from_responses_input_multipart() {
        use crate::api_types::responses::{EasyInputMessageRole, ResponseInputImageDetail};

        let input =
            ResponsesInput::Items(vec![ResponsesInputItem::EasyMessage(EasyInputMessage {
                type_: None,
                role: EasyInputMessageRole::User,
                content: EasyInputMessageContent::Parts(vec![
                    ResponseInputContentItem::InputText {
                        text: "What is in this image?".to_string(),
                        cache_control: None,
                    },
                    ResponseInputContentItem::InputImage {
                        detail: ResponseInputImageDetail::Auto,
                        image_url: Some("https://example.com/image.jpg".to_string()),
                        cache_control: None,
                    },
                ]),
            })]);

        let text = extract_text_from_responses_input(&input);
        assert_eq!(text, "What is in this image?");
    }

    #[test]
    fn test_extract_text_from_responses_response_with_output_text() {
        let response = r#"{
            "id": "resp-123",
            "object": "response",
            "output_text": "Paris is the capital of France.",
            "output": []
        }"#;

        let text = extract_text_from_responses_response(response.as_bytes());
        assert_eq!(text, "Paris is the capital of France.");
    }

    #[test]
    fn test_extract_text_from_responses_response_from_output_array() {
        let response = r#"{
            "id": "resp-123",
            "object": "response",
            "output": [
                {
                    "type": "message",
                    "id": "msg-123",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "The capital of France is Paris."
                        }
                    ]
                }
            ]
        }"#;

        let text = extract_text_from_responses_response(response.as_bytes());
        assert_eq!(text, "The capital of France is Paris.");
    }

    #[test]
    fn test_extract_text_from_responses_response_with_reasoning() {
        let response = r#"{
            "id": "resp-123",
            "output": [
                {
                    "type": "reasoning",
                    "id": "reason-123",
                    "content": [
                        {"type": "reasoning_text", "text": "Let me think about this..."}
                    ],
                    "summary": [
                        {"type": "summary_text", "text": "After considering..."}
                    ]
                },
                {
                    "type": "message",
                    "id": "msg-123",
                    "content": [
                        {"type": "output_text", "text": "The answer is 42."}
                    ]
                }
            ]
        }"#;

        let text = extract_text_from_responses_response(response.as_bytes());
        assert!(text.contains("Let me think about this..."));
        assert!(text.contains("After considering..."));
        assert!(text.contains("The answer is 42."));
    }

    #[test]
    fn test_extract_text_from_responses_response_with_refusal() {
        let response = r#"{
            "id": "resp-123",
            "output": [
                {
                    "type": "message",
                    "id": "msg-123",
                    "content": [
                        {"type": "refusal", "refusal": "I cannot help with that request."}
                    ]
                }
            ]
        }"#;

        let text = extract_text_from_responses_response(response.as_bytes());
        assert_eq!(text, "I cannot help with that request.");
    }

    #[test]
    fn test_extract_text_from_responses_response_invalid_json() {
        let text = extract_text_from_responses_response(b"not valid json");
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_text_from_responses_response_empty_output() {
        let response = r#"{"id": "resp-123", "output": []}"#;
        let text = extract_text_from_responses_response(response.as_bytes());
        assert!(text.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Concurrent Execution Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_concurrent_race_winner_display() {
        assert_eq!(
            ConcurrentRaceWinner::Guardrails.to_string(),
            "guardrails_first"
        );
        assert_eq!(ConcurrentRaceWinner::Llm.to_string(), "llm_first");
        assert_eq!(
            ConcurrentRaceWinner::GuardrailsTimedOut.to_string(),
            "guardrails_timed_out"
        );
    }

    #[test]
    fn test_concurrent_evaluation_outcome_headers() {
        let outcome: ConcurrentEvaluationOutcome<String> = ConcurrentEvaluationOutcome {
            winner: ConcurrentRaceWinner::Guardrails,
            guardrails_result: Some(InputGuardrailsResult {
                action: ResolvedAction::Allow,
                response: GuardrailsResponse::passed(),
                evaluated_text: "test".to_string(),
            }),
            llm_result: Some("result".to_string()),
            blocked: false,
            guardrails_latency_ms: Some(100),
            llm_latency_ms: Some(200),
        };

        let headers = outcome.to_headers();

        // Check mode header
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Mode" && v == "concurrent")
        );

        // Check winner header
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Race-Winner" && v == "guardrails_first")
        );

        // Check latency headers
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Latency-Ms" && v == "100")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-LLM-Latency-Ms" && v == "200")
        );
    }

    #[test]
    fn test_concurrent_evaluation_outcome_headers_llm_first() {
        let outcome: ConcurrentEvaluationOutcome<String> = ConcurrentEvaluationOutcome {
            winner: ConcurrentRaceWinner::Llm,
            guardrails_result: None,
            llm_result: Some("result".to_string()),
            blocked: false,
            guardrails_latency_ms: Some(500),
            llm_latency_ms: Some(300),
        };

        let headers = outcome.to_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Race-Winner" && v == "llm_first")
        );
    }

    #[test]
    fn test_concurrent_evaluation_outcome_headers_timeout() {
        let outcome: ConcurrentEvaluationOutcome<String> = ConcurrentEvaluationOutcome {
            winner: ConcurrentRaceWinner::GuardrailsTimedOut,
            guardrails_result: None,
            llm_result: Some("result".to_string()),
            blocked: false,
            guardrails_latency_ms: Some(5000),
            llm_latency_ms: Some(1000),
        };

        let headers = outcome.to_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-Guardrails-Race-Winner" && v == "guardrails_timed_out")
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Concurrent Execution Async Tests
    // ─────────────────────────────────────────────────────────────────────────────

    /// Mock guardrails provider for testing.
    struct MockGuardrailsProvider {
        name: String,
    }

    impl MockGuardrailsProvider {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl super::super::GuardrailsProvider for MockGuardrailsProvider {
        fn name(&self) -> &str {
            &self.name
        }

        async fn evaluate(
            &self,
            _request: &GuardrailsRequest,
        ) -> super::super::GuardrailsResult<GuardrailsResponse> {
            // Not used directly in concurrent tests - we pass in futures
            Ok(GuardrailsResponse::passed())
        }
    }

    fn create_test_guardrails(
        timeout_ms: u64,
        on_timeout: crate::config::GuardrailsTimeoutAction,
    ) -> InputGuardrails {
        let provider = Arc::new(MockGuardrailsProvider::new("mock-test"));
        InputGuardrails::for_testing(provider, Duration::from_millis(timeout_ms), on_timeout)
    }

    fn create_passing_result() -> InputGuardrailsResult {
        InputGuardrailsResult {
            action: ResolvedAction::Allow,
            response: GuardrailsResponse::passed(),
            evaluated_text: "test".to_string(),
        }
    }

    fn create_blocking_result() -> InputGuardrailsResult {
        use super::super::{Category, Severity};

        InputGuardrailsResult {
            action: ResolvedAction::Block {
                reason: "blocked by guardrails".to_string(),
                violations: vec![Violation::new(Category::Hate, Severity::High, 0.95)],
            },
            response: GuardrailsResponse::with_violations(vec![Violation::new(
                Category::Hate,
                Severity::High,
                0.95,
            )]),
            evaluated_text: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn test_concurrent_guardrails_pass_first_llm_success() {
        use crate::config::GuardrailsTimeoutAction;

        let guardrails = create_test_guardrails(5000, GuardrailsTimeoutAction::Block);

        // Guardrails completes in 10ms, LLM in 50ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok::<_, GuardrailsError>(create_passing_result())
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, String>("LLM response".to_string())
        };

        let outcome = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future)
            .await
            .expect("should succeed");

        assert_eq!(outcome.winner, ConcurrentRaceWinner::Guardrails);
        assert!(outcome.guardrails_result.is_some());
        assert!(!outcome.guardrails_result.as_ref().unwrap().is_blocked());
        assert_eq!(outcome.llm_result, Some("LLM response".to_string()));
        assert!(!outcome.blocked);
        assert!(outcome.guardrails_latency_ms.is_some());
        assert!(outcome.llm_latency_ms.is_some());
        // Guardrails should complete before LLM
        assert!(outcome.guardrails_latency_ms.unwrap() < outcome.llm_latency_ms.unwrap());
    }

    #[tokio::test]
    async fn test_concurrent_guardrails_block_first() {
        use crate::config::GuardrailsTimeoutAction;

        let guardrails = create_test_guardrails(5000, GuardrailsTimeoutAction::Block);

        // Guardrails blocks in 10ms, LLM would take 100ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok::<_, GuardrailsError>(create_blocking_result())
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok::<_, String>("LLM response".to_string())
        };

        let result = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future).await;

        // Should return an error because guardrails blocked
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_blocking());
    }

    #[tokio::test]
    async fn test_concurrent_llm_first_guardrails_pass() {
        use crate::config::GuardrailsTimeoutAction;

        let guardrails = create_test_guardrails(5000, GuardrailsTimeoutAction::Block);

        // LLM completes in 10ms, guardrails in 50ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, GuardrailsError>(create_passing_result())
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok::<_, String>("LLM response".to_string())
        };

        let outcome = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future)
            .await
            .expect("should succeed");

        assert_eq!(outcome.winner, ConcurrentRaceWinner::Llm);
        assert!(outcome.guardrails_result.is_some());
        assert!(!outcome.guardrails_result.as_ref().unwrap().is_blocked());
        assert_eq!(outcome.llm_result, Some("LLM response".to_string()));
        assert!(!outcome.blocked);
        // LLM should complete before guardrails
        assert!(outcome.llm_latency_ms.unwrap() < outcome.guardrails_latency_ms.unwrap());
    }

    #[tokio::test]
    async fn test_concurrent_llm_first_guardrails_block() {
        use crate::config::GuardrailsTimeoutAction;

        let guardrails = create_test_guardrails(5000, GuardrailsTimeoutAction::Block);

        // LLM completes in 10ms, guardrails blocks in 50ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, GuardrailsError>(create_blocking_result())
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok::<_, String>("LLM response".to_string())
        };

        let result = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future).await;

        // Should return an error because guardrails blocked (even though LLM finished first)
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_blocking());
    }

    #[tokio::test]
    async fn test_concurrent_guardrails_timeout_with_block_action() {
        use crate::config::GuardrailsTimeoutAction;

        // 50ms timeout, guardrails will take 100ms
        let guardrails = create_test_guardrails(50, GuardrailsTimeoutAction::Block);

        // Guardrails takes 100ms (will timeout), LLM takes 200ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok::<_, GuardrailsError>(create_passing_result())
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<_, String>("LLM response".to_string())
        };

        let result = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future).await;

        // Should return a timeout error because on_timeout is Block
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            GuardrailsError::Timeout { .. } => {}
            _ => panic!("Expected Timeout error, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_concurrent_guardrails_timeout_with_allow_action() {
        use crate::config::GuardrailsTimeoutAction;

        // 50ms timeout, guardrails will take 100ms
        let guardrails = create_test_guardrails(50, GuardrailsTimeoutAction::Allow);

        // Guardrails takes 100ms (will timeout), LLM takes 80ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok::<_, GuardrailsError>(create_passing_result())
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok::<_, String>("LLM response".to_string())
        };

        let outcome = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future)
            .await
            .expect("should succeed with allow on timeout");

        assert_eq!(outcome.winner, ConcurrentRaceWinner::GuardrailsTimedOut);
        // Guardrails result should be None because it timed out
        assert!(outcome.guardrails_result.is_none());
        // LLM result should be present
        assert_eq!(outcome.llm_result, Some("LLM response".to_string()));
        assert!(!outcome.blocked);
    }

    #[tokio::test]
    async fn test_concurrent_llm_error_after_guardrails_pass() {
        use crate::config::GuardrailsTimeoutAction;

        let guardrails = create_test_guardrails(5000, GuardrailsTimeoutAction::Block);

        // Guardrails pass in 10ms, LLM fails in 50ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok::<_, GuardrailsError>(create_passing_result())
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err::<String, _>("LLM provider error".to_string())
        };

        let outcome = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future)
            .await
            .expect("should succeed even with LLM error");

        assert_eq!(outcome.winner, ConcurrentRaceWinner::Guardrails);
        assert!(outcome.guardrails_result.is_some());
        // LLM result should be None because it failed
        assert!(outcome.llm_result.is_none());
        assert!(!outcome.blocked);
    }

    #[tokio::test]
    async fn test_concurrent_guardrails_provider_error() {
        use crate::config::GuardrailsTimeoutAction;

        let guardrails = create_test_guardrails(5000, GuardrailsTimeoutAction::Block);

        // Guardrails fails with provider error in 10ms
        let guardrails_future = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Err::<InputGuardrailsResult, _>(GuardrailsError::provider_error(
                "mock-test",
                "Provider unavailable",
            ))
        };

        let llm_future = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, String>("LLM response".to_string())
        };

        let result = run_concurrent_evaluation(&guardrails, guardrails_future, llm_future).await;

        // Should return provider error
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            GuardrailsError::ProviderError { .. } => {}
            _ => panic!("Expected ProviderError, got {:?}", err),
        }
    }
}
