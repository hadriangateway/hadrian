# Configuration

Config file: `hadrian.toml` (TOML format). Environment variables: use `${VAR_NAME}` syntax for interpolation. Secrets are automatically redacted in logs and API responses. See `src/config/` for all configuration options.

## Top-Level Config Sections

| Section | Description |
|---------|-------------|
| `[server]` | HTTP server (host, port, TLS, CORS, trusted proxies, security headers) |
| `[database]` | SQLite or PostgreSQL connection, pool settings, read replicas |
| `[cache]` | In-memory or Redis cache for sessions, rate limits, API key lookups |
| `[auth]` | Authentication mode (`none`, `api_key`, `idp`, `iap`), API key settings, per-org SSO, RBAC (CEL policies), session config |
| `[providers]` | LLM providers (OpenAI, Anthropic, Bedrock, Vertex, Azure), retries, fallbacks, health checks |
| `[limits]` | Rate limits, budget enforcement, request size limits |
| `[features]` | Feature flags (see below) |
| `[observability]` | Logging, tracing (OTLP), metrics (Prometheus), usage tracking, response validation |
| `[ui]` | Web UI settings, branding, file upload limits, admin panel |
| `[pricing]` | Model pricing for cost calculation and budget enforcement |
| `[secrets]` | External secrets managers (Vault, AWS Secrets Manager, Azure Key Vault, GCP) |
| `[retention]` | Data retention policies for automatic purging |
| `[storage]` | File storage backend (local filesystem, S3-compatible) |

## Key Provider Options

- `[providers.<name>]` — Define providers (openai, anthropic, bedrock, vertex, azure_openai, test)
- `fallback_providers` — List of providers to try on 5xx errors
- `retries` — Per-provider retry settings (max_attempts, delays, backoff)
- `health_check` — Background health monitoring
- `circuit_breaker` — Automatic provider disabling on repeated failures
- `streaming_buffer` — Buffer size for SSE streaming

## Feature Flags

- `[features.file_search]` — Knowledge Bases / RAG / vector search (embedding model, vector backend, chunking, reranking)
- `[features.file_processing]` — RAG document ingestion (text extraction, OCR, chunking)
- `[features.guardrails]` — Input/output guardrails (blocklist, PII detection, moderation APIs)
- `[features.response_caching]` — Response caching with optional semantic similarity matching
- `[features.image_fetching]` — Fetch images from URLs for vision models
- `[features.model_catalog]` — Model metadata enrichment from models.dev
- `[features.websocket]` — WebSocket for real-time events
- `[features.vector_store_cleanup]` — Background cleanup for soft-deleted vector stores
- `[features.shell]` — Shell tool runtime (`passthrough_openai`, `client_passthrough`, `microsandbox`, `opensandbox`). See `containers.md` and `adding_runtime.md`. Cargo features `runtime-microsandbox` / `runtime-opensandbox` gate the local backends.
- `[features.containers]` — Container persistence + artifact capture (idle TTL, per-file / per-session byte caps, max input files per request). Defaults match OpenAI's hosted-container behavior.
- `[features.server_tools]` — Server-executed tool framework: `max_iterations` (tool-loop budget), `pricing` (per-runtime microcents/sec), `shell_limits` (default & max memory, command timeout, egress allowlist, domain secrets).
