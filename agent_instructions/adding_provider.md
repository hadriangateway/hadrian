# Adding a New Provider

This guide explains how to add a new LLM provider to Hadrian, following established patterns to ensure consistency with circuit breakers, retry logic, error handling, fallbacks, and usage tracking.

## Architecture Overview

### Current Provider Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            Request Flow                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  1. Route Handler (routes/api/)                                             │
│     └── Parses request, extracts model string                               │
│                                                                             │
│  2. Model Routing (providers/routing/)                                      │
│     └── Resolves "provider/model" to (provider_config, model_name)          │
│                                                                             │
│  3. Execution Layer (routes/execution.rs)                                   │
│     ├── execute_with_fallback<E>() - orchestrates the request               │
│     ├── Builds fallback chain from config                                   │
│     ├── Calls ProviderExecutor::execute() for each attempt                  │
│     └── Handles fallback logic based on error classification                │
│                                                                             │
│  4. Provider Executor (routes/execution.rs)                                 │
│     ├── ChatCompletionExecutor, ResponsesExecutor, etc.                     │
│     ├── Matches provider config type (OpenAi, Anthropic, etc.)              │
│     ├── Instantiates provider with circuit breaker registry                 │
│     └── Calls provider.create_chat_completion(), etc.                       │
│                                                                             │
│  5. Provider Implementation (providers/<name>/mod.rs)                       │
│     ├── Converts OpenAI format → Provider format                            │
│     ├── Calls with_circuit_breaker_and_retry()                              │
│     ├── Makes HTTP request with authentication                              │
│     ├── Converts Provider format → OpenAI format                            │
│     └── Returns Response                                                    │
│                                                                             │
│  6. Cost Injection (providers/mod.rs)                                       │
│     └── inject_cost_into_response() wraps response with usage tracking      │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Key Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `Provider` trait | `providers/mod.rs` | Core interface all providers implement |
| `ProviderExecutor` trait | `routes/execution.rs` | Dispatches to correct provider method |
| `with_circuit_breaker_and_retry` | `providers/retry.rs` | Retry + circuit breaker wrapper |
| `CircuitBreakerRegistry` | `providers/registry.rs` | Shared circuit breaker instances |
| `ProviderErrorParser` trait | `providers/error.rs` | Parse provider errors → OpenAI format |
| `json_response` | `providers/response.rs` | Build JSON response with correct headers |
| `streaming_response` | `providers/response.rs` | Build SSE streaming response with correct headers |
| `error_response` | `providers/response.rs` | Build error response using a `ProviderErrorParser` |
| `inject_cost_into_response` | `providers/mod.rs` | Add usage tracking to responses |
| `build_fallback_chain` | `providers/fallback.rs` | Construct fallback provider chain |

## Critical Integration Patterns

Every provider MUST follow these patterns for correct integration with Hadrian's resilience and observability systems.

### 1. Circuit Breaker Integration

Every provider MUST:
- Accept a `CircuitBreakerRegistry` in `from_config_with_registry()`
- Store the circuit breaker instance
- Pass it to `with_circuit_breaker_and_retry()` for all HTTP calls

```rust
pub fn from_config_with_registry(
    config: &MyProviderConfig,
    provider_name: &str,
    registry: &CircuitBreakerRegistry,
) -> Self {
    // Get or create circuit breaker for this provider
    let circuit_breaker = registry.get_or_create(provider_name, &config.circuit_breaker);

    Self {
        // ... other fields ...
        circuit_breaker_config: config.circuit_breaker.clone(),
        circuit_breaker,
    }
}
```

### 2. Retry Logic with Pre-serialization

Every provider MUST:
- Pre-serialize request body BEFORE the retry loop
- Clone the serialized bytes in each retry attempt (cheap)
- NOT re-serialize the struct on each attempt (expensive)

```rust
// CORRECT: Pre-serialize once, clone bytes in retry loop
let body = serde_json::to_vec(&request).unwrap_or_default();

let response = with_circuit_breaker_and_retry(
    self.circuit_breaker.as_deref(),
    &self.circuit_breaker_config,
    &self.retry,
    "my_provider",
    "chat_completion",
    || async {
        client
            .post(&url)
            .header("content-type", "application/json")
            .body(body.clone())  // Clone Vec<u8>, not struct
            .send()
            .await
    },
).await?;

// WRONG: Re-serializing on each retry
let response = with_circuit_breaker_and_retry(
    ...,
    || async {
        client.post(&url).json(&request).send().await  // BAD: re-serializes every time
    },
).await?;
```

For multipart forms (audio/image endpoints), pre-serialize enum values:

```rust
// Pre-serialize before retry loop
let size = request.size.and_then(|s| {
    serde_json::to_string(&s).ok().map(|v| v.trim_matches('"').to_string())
});

let response = with_circuit_breaker_and_retry(
    ...,
    || {
        // Forms must be rebuilt each attempt (consumed on send)
        let mut form = Form::new().part("image", Part::bytes(image.to_vec()));
        if let Some(ref s) = size {
            form = form.text("size", s.clone());
        }
        async move { self.build_multipart_request(client, &url, form).send().await }
    },
).await?;
```

### 3. Error Handling

Every provider MUST:
- Implement a `ProviderErrorParser` for the provider's error format in `providers/error.rs`
- Use the `error_response::<Parser>()` helper to build error responses
- Map provider errors to OpenAI error types for consistent client experience

```rust
// In providers/error.rs, add your parser:

pub struct MyProviderErrorParser;

impl ProviderErrorParser for MyProviderErrorParser {
    fn parse_error(
        status: StatusCode,
        headers: &http::HeaderMap,
        body: &[u8],
    ) -> ProviderErrorInfo {
        let error: serde_json::Value = serde_json::from_slice(body)
            .unwrap_or_else(|_| serde_json::json!({}));

        let provider_code = error["error"]["code"].as_str().unwrap_or("unknown");
        let message = error["error"]["message"].as_str()
            .unwrap_or("Unknown error").to_string();

        // Map to OpenAI error types
        let error_type = match provider_code {
            "invalid_request" => OpenAiErrorType::InvalidRequest,
            "unauthorized" | "forbidden" => OpenAiErrorType::Authentication,
            "rate_limit" => OpenAiErrorType::RateLimit,
            "internal_error" | "timeout" => OpenAiErrorType::Server,
            _ => OpenAiErrorType::Api,
        };

        ProviderErrorInfo::new(error_type, message, provider_code)
    }
}

// In your provider, use the error_response helper (no per-provider function needed):
use crate::providers::response::error_response;

if !response.status().is_success() {
    return error_response::<MyProviderErrorParser>(response).await;
}
```

### 4. Streaming Responses

Every provider with streaming MUST:
- Implement a stream transformer that converts provider SSE → OpenAI SSE
- Handle usage tracking in the final stream chunk
- Use the `streaming_response()` helper for correct SSE headers

```rust
use crate::providers::response::{error_response, streaming_response};

if stream {
    use futures_util::StreamExt;

    let status = response.status();
    if !status.is_success() {
        return error_response::<MyProviderErrorParser>(response).await;
    }

    let byte_stream = response.bytes_stream().map(|result| {
        result.map_err(std::io::Error::other)
    });
    let transformed = MyProviderToOpenAIStream::new(byte_stream, &self.streaming_buffer);

    streaming_response(status, transformed)
}
```

### 5. Configuration

Every provider MUST have a config struct in `config/providers.rs`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MyProviderConfig {
    // Authentication
    pub api_key: String,

    // Endpoint configuration
    #[serde(default = "default_my_provider_base_url")]
    pub base_url: String,

    // Timeout (required for all providers)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    // Retry configuration (required for all providers)
    #[serde(default)]
    pub retry: RetryConfig,

    // Circuit breaker (required for all providers)
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    // Streaming buffer (for streaming providers)
    #[serde(default)]
    pub streaming_buffer: StreamingBufferConfig,

    // Fallback configuration
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    // Model-level fallbacks
    #[serde(default)]
    pub model_fallbacks: HashMap<String, Vec<ModelFallback>>,
}

fn default_my_provider_base_url() -> String {
    "https://api.myprovider.com/v1".to_string()
}
```

Add to `ProviderConfig` enum:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderConfig {
    // ... existing variants ...
    MyProvider(MyProviderConfig),
}
```

### 6. Execution Integration

Add your provider to ALL executor implementations in `routes/execution.rs`:

```rust
impl ProviderExecutor for ChatCompletionExecutor {
    // ...
    async fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> Result<Response, ProviderError> {
        match provider_config {
            // ... existing cases ...
            ProviderConfig::MyProvider(config) => {
                my_provider::MyProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_chat_completion(&state.http_client, payload)
                .await
            }
        }
    }
}
```

Repeat for `ResponsesExecutor`, `CompletionExecutor`, `EmbeddingExecutor`.

## Step-by-Step Implementation

### Step 1: Create Provider Module Structure

```
src/providers/my_provider/
├── mod.rs       # Main provider implementation
├── convert.rs   # Request/response conversion
├── stream.rs    # Streaming transformers (if streaming supported)
└── types.rs     # Provider-specific types
```

### Step 2: Define Types (`types.rs`)

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct MyProviderRequest {
    pub model: String,
    pub messages: Vec<MyProviderMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum MyProviderMessage {
    System { content: String },
    User { content: MyProviderContent },
    Assistant { content: String },
}

#[derive(Debug, Deserialize)]
pub struct MyProviderResponse {
    pub id: String,
    pub model: String,
    pub choices: Vec<MyProviderChoice>,
    pub usage: MyProviderUsage,
}
```

### Step 3: Implement Conversion (`convert.rs`)

```rust
use crate::api_types::{CreateChatCompletionPayload, Message, chat_completion::ChatCompletionResponse};
use super::types::*;

pub fn convert_messages(messages: Vec<Message>) -> Vec<MyProviderMessage> {
    messages.into_iter().filter_map(|msg| {
        match msg {
            Message::System { content, .. } => {
                Some(MyProviderMessage::System { content })
            }
            Message::User { content, .. } => {
                Some(MyProviderMessage::User { content: convert_content(content) })
            }
            Message::Assistant { content, .. } => {
                Some(MyProviderMessage::Assistant {
                    content: content.unwrap_or_default()
                })
            }
            _ => None,
        }
    }).collect()
}

pub fn convert_response(response: MyProviderResponse) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: response.id,
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp() as u64,
        model: response.model,
        choices: response.choices.into_iter().map(convert_choice).collect(),
        usage: Some(convert_usage(response.usage)),
        ..Default::default()
    }
}
```

### Step 4: Implement Stream Transformer (`stream.rs`)

```rust
use bytes::Bytes;
use futures_util::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::config::StreamingBufferConfig;

pub struct MyProviderToOpenAIStream<S> {
    inner: S,
    buffer: String,
    config: StreamingBufferConfig,
}

impl<S> MyProviderToOpenAIStream<S> {
    pub fn new(inner: S, config: &StreamingBufferConfig) -> Self {
        Self {
            inner,
            buffer: String::new(),
            config: config.clone(),
        }
    }
}

impl<S, E> Stream for MyProviderToOpenAIStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::error::Error,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Parse provider SSE events from buffer
        // Convert to OpenAI format: data: {"id":...,"object":"chat.completion.chunk",...}\n\n
        // Handle [DONE] message
        // Include usage in final chunk if available
    }
}
```

### Step 5: Implement Main Provider (`mod.rs`)

```rust
mod convert;
mod stream;
mod types;

use std::{sync::Arc, time::Duration};
use async_trait::async_trait;
use axum::response::Response;

use crate::{
    api_types::*,
    config::{CircuitBreakerConfig, MyProviderConfig, RetryConfig, StreamingBufferConfig},
    providers::{
        CircuitBreakerRegistry, ModelsResponse, Provider, ProviderError,
        circuit_breaker::CircuitBreaker,
        error::MyProviderErrorParser,
        response::{error_response, json_response, streaming_response},
        retry::with_circuit_breaker_and_retry,
    },
};

pub struct MyProvider {
    api_key: String,
    base_url: String,
    timeout: Duration,
    retry: RetryConfig,
    circuit_breaker_config: CircuitBreakerConfig,
    circuit_breaker: Option<Arc<CircuitBreaker>>,
    streaming_buffer: StreamingBufferConfig,
}

impl MyProvider {
    pub fn from_config_with_registry(
        config: &MyProviderConfig,
        provider_name: &str,
        registry: &CircuitBreakerRegistry,
    ) -> Self {
        let circuit_breaker = registry.get_or_create(provider_name, &config.circuit_breaker);

        Self {
            api_key: config.api_key.clone(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            timeout: Duration::from_secs(config.timeout_secs),
            retry: config.retry.clone(),
            circuit_breaker_config: config.circuit_breaker.clone(),
            circuit_breaker,
            streaming_buffer: config.streaming_buffer.clone(),
        }
    }
}

#[async_trait]
impl Provider for MyProvider {
    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "my_provider",
            operation = "chat_completion",
            model = %payload.model.as_deref().unwrap_or("default"),
            stream = payload.stream
        )
    )]
    async fn create_chat_completion(
        &self,
        client: &reqwest::Client,
        payload: CreateChatCompletionPayload,
    ) -> Result<Response, ProviderError> {
        let model = payload.model.clone().unwrap_or_else(|| "default".to_string());
        let stream = payload.stream;

        // Convert request
        let request = types::MyProviderRequest {
            model,
            messages: convert::convert_messages(payload.messages),
            max_tokens: payload.max_tokens,
            temperature: payload.temperature,
            stream: if stream { Some(true) } else { None },
        };

        // Pre-serialize before retry loop
        let body = serde_json::to_vec(&request).unwrap_or_default();
        let url = format!("{}/chat/completions", self.base_url);
        let api_key = self.api_key.clone();
        let timeout = self.timeout;

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "my_provider",
            "chat_completion",
            || async {
                client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json")
                    .timeout(timeout)
                    .body(body.clone())
                    .send()
                    .await
            },
        ).await?;

        let status = response.status();
        if !status.is_success() {
            return error_response::<MyProviderErrorParser>(response).await;
        }

        if stream {
            use futures_util::StreamExt;

            let byte_stream = response.bytes_stream().map(|result| {
                result.map_err(std::io::Error::other)
            });
            let transformed = stream::MyProviderToOpenAIStream::new(
                byte_stream,
                &self.streaming_buffer
            );

            streaming_response(status, transformed)
        } else {
            let provider_response: types::MyProviderResponse = response.json().await?;
            let openai_response = convert::convert_response(provider_response);
            json_response(status, &openai_response)
        }
    }

    async fn create_responses(
        &self,
        _client: &reqwest::Client,
        _payload: CreateResponsesPayload,
    ) -> Result<Response, ProviderError> {
        // Implement similarly to chat_completion, or:
        Err(ProviderError::Internal(
            "This provider does not support the Responses API".to_string(),
        ))
    }

    async fn create_completion(
        &self,
        _client: &reqwest::Client,
        _payload: CreateCompletionPayload,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support legacy completions".to_string(),
        ))
    }

    async fn create_embedding(
        &self,
        _client: &reqwest::Client,
        _payload: CreateEmbeddingPayload,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support embeddings".to_string(),
        ))
    }

    async fn list_models(
        &self,
        _client: &reqwest::Client,
    ) -> Result<ModelsResponse, ProviderError> {
        // Return static list or call provider's models endpoint
        Ok(ModelsResponse {
            data: vec![
                crate::providers::ModelInfo {
                    id: "my-model-1".to_string(),
                    extra: serde_json::json!({}),
                },
            ],
        })
    }
}
```

### Step 6: Register the Provider

1. Export from `providers/mod.rs`:
```rust
pub mod my_provider;
```

2. Update `routes/execution.rs` - add to ALL executor match blocks

3. Add config to `config/providers.rs`

### Step 7: Add Tests

Create test fixtures in `tests/fixtures/providers/my_provider/`:

```
tests/fixtures/providers/my_provider/
├── chat_completion_basic/
│   ├── request.json
│   └── response.json
├── chat_completion_streaming/
│   ├── request.json
│   └── response.txt  # Raw SSE events
└── error_rate_limit/
    ├── request.json
    └── response.json
```

Add test spec to `src/tests/provider_e2e.rs`:

```rust
ProviderTestSpec {
    name: "my_provider",
    fixtures_dir: "my_provider",
    provider_config: r#"
        [my_provider]
        type = "my_provider"
        api_key = "test-key"
    "#,
    test_cases: vec![
        ("chat_completion_basic", "chat_completion", false),
        ("chat_completion_streaming", "chat_completion", true),
    ],
},
```

## Provider Trait Methods

| Method | Purpose | Streaming | Required |
|--------|---------|-----------|----------|
| `create_chat_completion` | Chat completions API | Yes | Yes |
| `create_responses` | OpenAI responses API | Yes | No (return error) |
| `create_completion` | Legacy completions | Yes | No (return error) |
| `create_embedding` | Embedding generation | No | No (return error) |
| `list_models` | List available models | No | Yes (for health checks) |
| `health_check` | Provider health | No | Has default impl |

## Server-side tools (`shell`, `file_search`, `web_search`)

For `create_responses`, the execution layer rewrites server-side tools to function tools for
providers without a native equivalent — your provider doesn't need to know about `shell` or
`web_search` specifically.

- **`shell`** — `routes/execution.rs` calls `preprocess_shell_tools` with a `ShellToolHint`
  before invoking your provider. Anthropic / Bedrock / Vertex always get the rewrite (they
  have no native shell tool); OpenAI / Azure keep the native spec when the runtime mode is
  `passthrough_openai` or `client_passthrough`. If your provider has a native shell-like
  primitive, extend `keeps_openai_native_shell()` in `config/runtimes.rs` and update the
  per-provider branch in `ResponsesExecutor::execute`.
- **`file_search` / `web_search`** — same pattern; rewritten unconditionally for non-OpenAI
  providers. The `ToolLoopRunner` (`services/server_tools/runner.rs`) intercepts the
  resulting `function_call` items and executes them server-side.
- **Function-call shape compatibility** — your `convert.rs` needs to translate OpenAI-style
  `function_call` / `function_call_output` items in `payload.input` to the provider's
  tool-use format and back. The server-tool loop will not work otherwise. See
  `providers/{anthropic,bedrock,vertex}/convert.rs` for working examples.
- **Container files** — when the server tool returns a `container_file_citation` annotation,
  it shows up on a `response.content_part.done` event. You don't need to handle this in your
  convert layer; the shell executor injects it on the way out via `transform_event`.

For containers / shell architecture, see `containers.md` and `responses_pipeline.md`.

## Checklist

- [ ] Provider struct with `circuit_breaker` field
- [ ] `from_config_with_registry()` constructor
- [ ] `from_config_with_registry_and_image_config()` if image URL fetching needed
- [ ] Pre-serialization in ALL methods that use retry
- [ ] Error parser implementing `ProviderErrorParser` in `providers/error.rs`
- [ ] Use `error_response::<Parser>()` helper for error responses
- [ ] Use `json_response()` helper for JSON responses
- [ ] Use `streaming_response()` helper for SSE streaming responses
- [ ] Stream transformer in `stream.rs` (if streaming supported)
- [ ] Configuration struct with ALL required fields
- [ ] Added to `ProviderConfig` enum
- [ ] Added to ALL `ProviderExecutor` implementations in `routes/execution.rs`
- [ ] Tracing instrumentation on all trait methods
- [ ] Unit tests for conversion functions
- [ ] E2E test fixtures
- [ ] Documentation in CLAUDE.md provider features table

## Example Config

```toml
[providers.my-provider]
type = "my_provider"
api_key = "sk-xxx"
base_url = "https://api.myprovider.com/v1"
timeout_secs = 120

[providers.my-provider.retry]
enabled = true
max_retries = 3
initial_delay_ms = 100
max_delay_ms = 10000

[providers.my-provider.circuit_breaker]
enabled = true
failure_threshold = 5
open_timeout_secs = 30
success_threshold = 2
failure_status_codes = [500, 502, 503, 504]

fallback_providers = ["backup-provider"]

[providers.my-provider.model_fallbacks]
"expensive-model" = [
    { model = "cheaper-model" },
    { provider = "backup-provider", model = "fallback-model" }
]
```
