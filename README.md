<p align="center">
  <img src="ui/public/icons/icon.svg" alt="Hadrian" width="128" height="128" />
</p>

<h1 align="center">Hadrian Gateway</h1>

<p align="center">
  An open-source AI Gateway that provides a unified OpenAI-compatible API for routing requests to multiple LLM providers. All enterprise features included. Dual-licensed under Apache 2.0 and MIT.
</p>

<p align="center">
  <a href="https://hadriangateway.com/docs"><strong>Documentation</strong></a> | <a href="https://app.hadriangateway.com"><strong>Try in Browser</strong></a> | <a href="https://hadriangateway.com/docs/api"><strong>API Reference</strong></a> | <a href="https://openrouter.ai/apps?url=https%3A%2F%2Fhadriangateway.com"><strong>OpenRouter</strong></a> | <a href="https://openresponses.org"><strong>OpenResponses</strong></a>
</p>

<p align="center">
  <video width="100%" poster="https://github.com/user-attachments/assets/ab0768fd-859b-4cf7-8be7-c77cb20018d5" src="https://github.com/user-attachments/assets/6bb8d08b-17c2-484a-a2d2-c701564ad1e8" />
</p>


> [!WARNING]
> Hadrian is experimental, alpha, vibe-coded software and is not ready for production use. The API, configuration format, and database schema are subject to breaking changes that will lead to data loss. Hadrian has not undergone a security audit. Do not expose it to untrusted networks or use it to handle sensitive data. We are not accepting pull requests at this time, but [issues](https://github.com/hadriangateway/hadrian/issues) and [discussions](https://github.com/hadriangateway/hadrian/discussions) are welcome.

## Why Hadrian?

- **Single binary, single config.** No complex deployments. Works on a Raspberry Pi or global cloud infrastructure.
- **All features included.** Multi-tenancy, SSO, RBAC, guardrails, semantic caching, cost forecasting. Everything is free.
- **Production ready.** Budget enforcement, rate limiting, circuit breakers, fallback chains, observability.
- **Multi-model chat UI.** Compare responses from multiple models side-by-side with 15 interaction modes.
- **Built-in RAG.** OpenAI-compatible Vector Stores API with document processing, chunking, and search.
- **Studio.** Image generation, TTS, transcription, and translation with multi-model execution.

## Quick Start

See the [Getting Started](https://hadriangateway.com/docs/getting-started) guide for more details. Otherwise:

Download the latest binary from [GitHub Releases](https://github.com/hadriangateway/hadrian/releases/latest) and run it:

```bash
./hadrian
```

Or use Docker:

```bash
docker run -p 8080:8080 \
  -v hadrian-data:/app/data \
  ghcr.io/hadriangateway/hadrian
```

To customize the configuration, create a `hadrian.toml` and mount it:

```bash
cat <<'EOF' > hadrian.toml
[server]
host = "0.0.0.0"
port = 8080

[database]
type = "sqlite"
path = "/app/data/hadrian.db"

[cache]
type = "memory"

[ui]
enabled = true

# Add a provider (uncomment and set your API key)
# [providers.openai]
# type = "open_ai"
# api_key = "${OPENAI_API_KEY}"
EOF

docker run -p 8080:8080 \
  -v ./hadrian.toml:/app/config/hadrian.toml:ro \
  -v hadrian-data:/app/data \
  ghcr.io/hadriangateway/hadrian
```

Or build from source ([just](https://just.systems) required):

```bash
git clone https://github.com/hadriangateway/hadrian.git
cd hadrian && just init && just build
./target/release/hadrian
```

Or install from crates.io (find the latest version with `cargo search hadrian`):

```bash
cargo install hadrian@VERSION
```

The gateway starts at `http://localhost:8080` with the chat UI. No database required for basic use. Running without arguments creates `~/.config/hadrian/hadrian.toml` with sensible defaults, uses SQLite, and opens the browser.

## Configuration

```toml
# Minimal: just add a provider
[providers.openai]
type = "open_ai"
api_key = "${OPENAI_API_KEY}"
```

```toml
# Multiple providers with fallback
[providers.anthropic]
type = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
fallback_providers = ["openai"]

[providers.openai]
type = "open_ai"
api_key = "${OPENAI_API_KEY}"
```

Supports OpenAI, Anthropic, AWS Bedrock, Google Vertex AI, Azure OpenAI, and any OpenAI-compatible API (OpenRouter, Ollama, etc). See the [provider docs](https://hadriangateway.com/docs/configuration/providers) for details.

## Features

- **Providers:** OpenAI, Anthropic, Bedrock, Vertex, Azure, plus any OpenAI-compatible API. Fallback chains, circuit breakers, health checks.
- **Multi-tenancy:** Organizations, teams, projects, users. Scoped providers, budgets, and rate limits at every level.
- **Auth:** API keys, OIDC/OAuth, per-org SSO, SAML, SCIM, reverse proxy auth, CEL-based RBAC, Sovereignty enforcement for data residency.
- **Guardrails:** Blocklist, PII detection, content moderation (OpenAI, Bedrock, Azure). Blocking, concurrent, and post-response modes.
- **Caching:** Exact match and semantic similarity caching with pgvector or Qdrant.
- **Knowledge Bases:** File upload, text extraction, OCR, chunking, vector search, re-ranking. OpenAI-compatible Vector Stores API.
- **Cost tracking:** Microcent precision, time-series forecasting, budget enforcement with atomic reservation.
- **Observability:** Prometheus metrics, OTLP tracing, structured logging, usage export.
- **Web UI:** Multi-model chat with 15 modes, web search, frontend tools (Python, JS, SQL, charts), MCP support, admin panel.
- **Agents:** Server-side shell tool in persistent containers, server-side MCP, and Skills, via the Responses API.
- **Studio:** Image generation, text-to-speech, transcription, and translation across providers.
- **Secrets:** External secrets managers (AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, HashiCorp Vault) for credential storage.

## API

OpenAI-compatible. Point any OpenAI SDK at Hadrian:

```bash
curl http://localhost:8080/api/v1/responses \
  -H "Content-Type: application/json" \
  -H "X-API-Key: gw_live_..." \
  -d '{"model": "anthropic/claude-opus-4-6", "input": "Hello!"}'
```

Interactive API reference available at `/api/docs` when running.

## Deployment

Available as a single binary, Docker image, or Helm chart.

```bash
# Docker Compose (production)
cd deploy && docker compose -f docker-compose.postgres.yml up -d

# Kubernetes (from source)
cd helm/hadrian && helm dependency update && helm install my-gateway .
```

See the [deployment docs](https://hadriangateway.com/docs/deployment) for Docker Compose configurations, Helm chart options, and production recommendations.

## Development

```bash
# Backend
cargo build && cargo test && cargo clippy && cargo +nightly fmt

# Frontend
cd ui && pnpm install && pnpm dev

# E2E tests
cd deploy/tests && pnpm test
```

## License

Dual-licensed under [Apache 2.0](LICENSE-APACHE) and [MIT](LICENSE-MIT).
