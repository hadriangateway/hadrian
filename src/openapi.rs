use serde::{Deserialize, Serialize};
#[cfg(feature = "utoipa")]
use utoipa::OpenApi;

#[cfg(feature = "utoipa")]
use crate::{
    api_types, models,
    routes::{admin, api, health},
};

#[cfg(feature = "utoipa")]
/// OpenAPI documentation for Hadrian Gateway
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Hadrian Gateway API",
        version = env!("CARGO_PKG_VERSION"),
        description = r#"**Hadrian Gateway** is an AI Gateway providing a unified OpenAI-compatible API for routing requests to multiple LLM providers.

## Overview

The gateway provides two main API surfaces:

- **Public API** (`/api/v1/*`) - OpenAI-compatible endpoints for LLM inference. Use these endpoints to create chat completions, text completions, embeddings, and list available models. Authentication depends on the configured `auth.mode` (API key, IdP, IAP, or none).

- **Admin API** (`/admin/v1/*`) - RESTful management endpoints for multi-tenant configuration. Manage organizations, projects, users, API keys, dynamic providers, usage tracking, and model pricing.

## Authentication

The gateway supports multiple authentication methods for API access.

### API Key Authentication

API keys are the primary authentication method for programmatic access. Keys are created via the Admin API and scoped to organizations, projects, or users.

**Using the Authorization header (recommended):**
```
Authorization: Bearer gw_live_abc123def456...
```

**Using the X-API-Key header:**
```
X-API-Key: gw_live_abc123def456...
```

Both headers are supported. The `Authorization: Bearer` format is recommended for compatibility with OpenAI client libraries.

**Example request:**
```bash
curl https://gateway.example.com/api/v1/chat/completions \
  -H \"Authorization: Bearer gw_live_abc123def456...\" \
  -H \"Content-Type: application/json\" \
  -d '{\"model\": \"openai/gpt-4\", \"messages\": [{\"role\": \"user\", \"content\": \"Hello\"}]}'
```

### JWT Authentication

When JWT authentication is enabled, requests can be authenticated using a JWT token from your identity provider.

```
Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9...
```

The gateway validates the JWT against the configured JWKS endpoint and extracts the identity from the token claims.

**Example request:**
```bash
curl https://gateway.example.com/api/v1/chat/completions \
  -H \"Authorization: Bearer eyJhbGciOiJSUzI1NiIs...\" \
  -H \"Content-Type: application/json\" \
  -d '{\"model\": \"openai/gpt-4\", \"messages\": [{\"role\": \"user\", \"content\": \"Hello\"}]}'
```

### Multi-Auth Mode

When configured for multi-auth, the gateway accepts both API keys and JWTs using **format-based detection**:

- **X-API-Key header**: Always validated as an API key
- **Authorization: Bearer header**: Uses format-based detection:
  - Tokens starting with the configured API key prefix (default: `gw_`) are validated as API keys
  - All other tokens are validated as JWTs

**Important:** Providing both `X-API-Key` and `Authorization` headers simultaneously results in a 400 error (ambiguous credentials). Choose one authentication method per request.

**Examples:**
```bash
# API key in X-API-Key header
curl -H \"X-API-Key: gw_live_abc123...\" https://gateway.example.com/v1/chat/completions

# API key in Authorization: Bearer header (format-based detection)
curl -H \"Authorization: Bearer gw_live_abc123...\" https://gateway.example.com/v1/chat/completions

# JWT in Authorization: Bearer header
curl -H \"Authorization: Bearer eyJhbGciOiJSUzI1NiIs...\" https://gateway.example.com/v1/chat/completions
```

### Authentication Errors

| Error Code | HTTP Status | Description | Example Response |
|------------|-------------|-------------|------------------|
| `unauthorized` | 401 | No authentication credentials provided | `{\"error\": {\"code\": \"unauthorized\", \"message\": \"Authentication required\"}}` |
| `ambiguous_credentials` | 400 | Both X-API-Key and Authorization headers provided | `{\"error\": {\"code\": \"ambiguous_credentials\", \"message\": \"Ambiguous credentials: provide either X-API-Key or Authorization header, not both\"}}` |
| `invalid_api_key` | 401 | API key is invalid, malformed, or revoked | `{\"error\": {\"code\": \"invalid_api_key\", \"message\": \"Invalid API key\"}}` |
| `not_authenticated` | 401 | JWT validation failed | `{\"error\": {\"code\": \"not_authenticated\", \"message\": \"Token validation failed\"}}` |
| `forbidden` | 403 | Valid credentials but insufficient permissions | `{\"error\": {\"code\": \"forbidden\", \"message\": \"Insufficient permissions\"}}` |

### Configuration Examples

**API Key Authentication:**
```toml
[auth.mode]
type = \"api_key\"

[auth.api_key]
header_name = \"X-API-Key\"    # Header to read API key from
key_prefix = \"gw_\"           # Valid key prefix
cache_ttl_secs = 60           # Cache key lookups for 60 seconds
```

**IdP Authentication (SSO + API keys + JWT):**
```toml
[auth.mode]
type = \"idp\"

[auth.api_key]
header_name = \"X-API-Key\"
key_prefix = \"gw_\"

[auth.session]
secure = true
```

**Identity-Aware Proxy (IAP):**
```toml
[auth.mode]
type = \"iap\"
identity_header = \"X-Forwarded-User\"
email_header = \"X-Forwarded-Email\"
```

## Pagination

All Admin API list endpoints use **cursor-based pagination** for stable, performant navigation.

**Query Parameters:**
- `limit` (optional): Maximum records per page (default: 100, max: 1000)
- `cursor` (optional): Opaque cursor from previous response's `next_cursor` or `prev_cursor`
- `direction` (optional): `forward` (default) or `backward`

**Response:**
```json
{
  \"data\": [...],
  \"pagination\": {
    \"limit\": 100,
    \"has_more\": true,
    \"next_cursor\": \"MTczMzU4MDgwMDAwMDphYmMxMjM0...\",
    \"prev_cursor\": null
  }
}
```

## Model Routing

Models can be addressed in several ways:

- **Static routing**: `provider-name/model-name` routes to config-defined providers
- **Dynamic routing**: `:org/{ORG}/{PROVIDER}/{MODEL}` routes to database-backed providers
- **Default**: When no prefix is specified, routes to the default provider

## Error Codes

All errors follow a consistent JSON format:

```json
{
  \"error\": {
    \"code\": \"error_code\",
    \"message\": \"Human-readable error message\",
    \"details\": { ... }  // Optional additional context
  }
}
```

### Authentication & Authorization Errors

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `unauthorized` | 401 | Missing or invalid API key/token |
| `invalid_api_key` | 401 | API key is invalid, expired, or revoked |
| `forbidden` | 403 | Valid credentials but insufficient permissions |
| `not_authenticated` | 401 | Authentication required for this operation |

### Rate Limiting & Budget Errors

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `rate_limit_exceeded` | 429 | Request rate limit exceeded. Check `Retry-After` header. |
| `budget_exceeded` | 402 | Budget limit exceeded for the configured period. Details include `limit_cents`, `current_spend_cents`, and `period`. |
| `cache_required` | 503 | Budget enforcement requires cache to be configured |

### Request Validation Errors

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `validation_error` | 400 | Request body validation failed |
| `bad_request` | 400 | Malformed request |
| `routing_error` | 400 | Model routing failed (invalid model string or provider not found) |
| `not_found` | 404 | Requested resource not found |
| `conflict` | 409 | Resource already exists or conflicts with existing state |

### Provider & Gateway Errors

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `provider_error` | 502 | Upstream LLM provider returned an error |
| `request_failed` | 502 | Failed to communicate with upstream provider |
| `circuit_breaker_open` | 503 | Provider circuit breaker is open due to repeated failures |
| `response_read_error` | 500 | Failed to read provider response |
| `response_builder` | 500 | Failed to build response from provider data |
| `internal_error` | 500 | Internal server error |

### Guardrails Errors

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `guardrails_blocked` | 400 | Content blocked by guardrails policy. Response includes `violations` array. |
| `guardrails_timeout` | 504 | Guardrails evaluation timed out |
| `guardrails_provider_error` | 502 | Error communicating with guardrails provider |
| `guardrails_auth_error` | 502 | Authentication failed with guardrails provider |
| `guardrails_rate_limited` | 429 | Guardrails provider rate limit exceeded |
| `guardrails_config_error` | 500 | Invalid guardrails configuration |
| `guardrails_parse_error` | 400 | Failed to parse content for guardrails evaluation |

### Admin API Errors

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `database_required` | 503 | Database not configured (required for admin operations) |
| `services_required` | 503 | Required services not initialized |
| `not_configured` | 503 | Required feature or service not configured |
| `database_error` | 500 | Database operation failed |

## Rate Limiting

The gateway implements multiple layers of rate limiting to protect against abuse and ensure fair usage.

### Rate Limit Types

| Type | Scope | Default | Description |
|------|-------|---------|-------------|
| **Requests per minute** | API Key | 60 | Maximum requests per minute per API key |
| **Requests per day** | API Key | Unlimited | Optional daily request limit per API key |
| **Tokens per minute** | API Key | 100,000 | Maximum tokens processed per minute |
| **Tokens per day** | API Key | Unlimited | Optional daily token limit |
| **Concurrent requests** | API Key | 10 | Maximum simultaneous in-flight requests |
| **IP requests per minute** | IP Address | 120 | Rate limit for unauthenticated requests |

### Rate Limit Headers

All API responses include rate limit information in HTTP headers.

#### Request Rate Limit Headers

| Header | Description | Example |
|--------|-------------|---------|
| `X-RateLimit-Limit` | Maximum requests allowed in the current window | `60` |
| `X-RateLimit-Remaining` | Requests remaining in the current window | `45` |
| `X-RateLimit-Reset` | Seconds until the rate limit window resets | `42` |

#### Token Rate Limit Headers

| Header | Description | Example |
|--------|-------------|---------|
| `X-TokenRateLimit-Limit` | Maximum tokens allowed per minute | `100000` |
| `X-TokenRateLimit-Remaining` | Tokens remaining in the current minute | `85000` |
| `X-TokenRateLimit-Used` | Tokens used in the current minute | `15000` |
| `X-TokenRateLimit-Day-Limit` | Maximum tokens allowed per day (if configured) | `1000000` |
| `X-TokenRateLimit-Day-Remaining` | Tokens remaining today (if configured) | `950000` |

#### Rate Limit Exceeded Response

When a rate limit is exceeded, the API returns HTTP 429 with:

```json
{
  \"error\": {
    \"code\": \"rate_limit_exceeded\",
    \"message\": \"Rate limit exceeded: 60 requests per minute\",
    \"details\": {
      \"limit\": 60,
      \"window\": \"minute\",
      \"retry_after_secs\": 42
    }
  }
}
```

The `Retry-After` header indicates seconds to wait before retrying:

```
HTTP/1.1 429 Too Many Requests
Retry-After: 42
X-RateLimit-Limit: 60
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 42
```

### IP-Based Rate Limiting

Unauthenticated requests (requests without a valid API key) are rate limited by IP address. This protects public endpoints like `/health` from abuse.

- **Default:** 120 requests per minute per IP
- **Client IP Detection:** Respects `X-Forwarded-For` and `X-Real-IP` headers when trusted proxies are configured
- **Configuration:** Can be disabled or adjusted via `limits.rate_limits.ip_rate_limits` in config

### Rate Limit Configuration

Rate limits are configured hierarchically:

1. **Global defaults** (in `hadrian.toml`):
```toml
[limits.rate_limits]
requests_per_minute = 60
tokens_per_minute = 100000
concurrent_requests = 10

[limits.rate_limits.ip_rate_limits]
enabled = true
requests_per_minute = 120
```

2. **Per-API key** limits can override global defaults (when creating API keys via Admin API)

### Best Practices

- **Implement exponential backoff**: When receiving 429 responses, wait the `Retry-After` duration before retrying
- **Monitor rate limit headers**: Track `X-RateLimit-Remaining` to proactively throttle requests
- **Use streaming for long responses**: Streaming responses don't hold connections during generation
- **Batch requests when possible**: Combine multiple small requests into larger batches
"#,
        license(name = "Apache-2.0 OR MIT", url = "https://github.com/hadriangateway/hadrian/blob/main/LICENSE-APACHE"),
    ),
    servers(
        (url = "/", description = "Default server")
    ),
    tags(
        // Public API tags
        (name = "chat", description = "Create chat completions using conversational message format. Supports streaming, tool use, vision, and reasoning models. OpenAI-compatible."),
        (name = "completions", description = "Create text completions from a prompt. Legacy API for non-chat models. OpenAI-compatible."),
        (name = "embeddings", description = "Generate vector embeddings for text input. Use for semantic search, clustering, and similarity comparisons. OpenAI-compatible."),
        (name = "models", description = "List all available models from configured providers. Model IDs are prefixed with provider name."),
        (name = "me", description = "Self-service endpoints for authenticated users. Export personal data for GDPR compliance."),
        (name = "oauth", description = "OAuth-style PKCE flow for issuing user-scoped API keys to external apps. The user grants consent in the Hadrian UI; the external app exchanges the resulting code at `/oauth/token` for an API key bound to that user."),
        (name = "Images", description = "Generate, edit, and create variations of images using DALL-E models. OpenAI-compatible."),
        (name = "Videos", description = "Asynchronous video generation, remix, edit, extend, and characters using Sora models. OpenAI-compatible."),
        (name = "Audio", description = "Text-to-speech, speech-to-text transcription, and audio translation using TTS and Whisper models. OpenAI-compatible."),
        // Admin API tags
        (name = "organizations", description = "Organizations are the top-level entity for multi-tenancy. Each organization can have multiple projects, users, API keys, and provider configurations."),
        (name = "projects", description = "Projects belong to organizations and provide a way to separate workloads, budgets, and API keys within an organization."),
        (name = "users", description = "Users can be members of organizations and projects. Users can have their own API keys and provider configurations."),
        (name = "api-keys", description = "API keys authenticate requests to the Public API. Keys can be scoped to organizations, projects, or users with optional budget limits and expiration."),
        (name = "dynamic-providers", description = "Dynamic providers allow runtime configuration of LLM backends without restarting the gateway. Useful for BYOK (bring-your-own-key) scenarios."),
        (name = "usage", description = "Query usage statistics for API keys including token counts, costs, and breakdowns by date, model, or referer."),
        (name = "model-pricing", description = "Configure per-model pricing for cost tracking. Pricing can be set globally, per-provider, per-organization, per-project, or per-user."),
        (name = "conversations", description = "Store and manage chat conversation history. Conversations can be owned by users or projects and support multiple models."),
        (name = "templates", description = "Manage reusable prompt templates. Templates can be owned by organizations, teams, projects, or users and include metadata for configuration."),
        (name = "skills", description = "Manage Skills (OpenAI-compatible `/v1/skills`). A skill packages a SKILL.md instruction file plus optional bundled scripts, references, and assets, published as immutable versions with a `default_version`/`latest_version` pointer. Upload as a JSON file array, a multipart directory, or a zip bundle; download a version as zip via `/content`.\n\n## Hadrian Extensions\n- `owner_type`/`owner_id` for organization/team/project/user ownership (OpenAI is project-scoped)\n- JSON `files` array (`{path, content}`) alongside the spec's zip/multipart upload\n- `files`/`files_manifest`, `total_bytes`, and frontmatter flags on responses\n- `skill_reference` accepts a prefixed/bare id or a name slug, plus a specific `version`"),
        (name = "audit-logs", description = "Query audit logs for admin operations. All sensitive operations like API key creation, user permission changes, and resource modifications are logged."),
        (name = "teams", description = "Teams group users within an organization for easier permission management. Users can belong to multiple teams, and projects can be assigned to a team."),
        (name = "service_accounts", description = "Service accounts are machine identities that can own API keys and carry roles for RBAC evaluation. They enable unified authorization across human users and automated systems."),
        (name = "access-reviews", description = "Access review reports for compliance requirements (SOC 2, ISO 27001). View user access across organizations, projects, and API keys."),
        (name = "sso", description = "SSO connection configuration (read-only from config). View OIDC and proxy auth settings for JIT user provisioning."),
        (name = "files", description = "Upload and manage files for use with vector stores. Files are uploaded via multipart form data and can be added to vector stores for RAG."),
        (name = "vector-stores", description = "Create and manage vector stores for RAG (Retrieval Augmented Generation). Vector stores contain files that are chunked and embedded for semantic search.\n\n## Hadrian Extensions\n\nThe Vector Stores API is based on OpenAI's Vector Stores API with the following extensions:\n\n### Multi-Tenancy\n- `owner_type`, `owner_id` fields for organization/project/user ownership\n- Required in create requests and included in responses\n\n### Additional Fields\n- `description`: Human-readable description for vector stores\n- `embedding_model`: Configurable embedding model (default: text-embedding-3-small)\n- `embedding_dimensions`: Configurable vector dimensions (default: 1536)\n- `updated_at`: Modification timestamp\n- `file_id`: Reference to Files API in vector store files\n\n### Extension Endpoints\n- `GET /v1/vector_stores/{id}/files/{file_id}/chunks`: List chunks for debugging\n\n### Search Extensions\n- Request: `threshold` (similarity threshold), `file_ids` (file filter)\n- Response: `chunk_id`, `vector_store_id`, `chunk_index` for debugging\n\n### Schema Differences\n- Timestamps use ISO 8601 format (OpenAI uses Unix timestamps)\n- List responses use `pagination` object (OpenAI uses root-level `first_id`, `last_id`, `has_more`)\n- Search `content` is a string (OpenAI uses `[{type, text}]` array)"),
        // Health & Infrastructure
        (name = "health", description = "Health check endpoints for monitoring and Kubernetes probes. Use `/health` for detailed status, `/health/live` for liveness probes, and `/health/ready` for readiness probes."),
        (name = "auth", description = "Browser-facing authentication endpoints (OIDC / SAML). The frontend calls `/auth/discover` to find the right SSO provider for an email domain, then `/auth/login` to redirect to the IdP; `/auth/me` returns the authenticated identity for whatever session cookie or bearer token is presented."),
    ),
    paths(
        // Health check routes
        health::health_check,
        health::liveness,
        health::readiness,
        // Browser auth routes
        crate::routes::auth::discover,
        crate::routes::auth::me,
        // Public API routes
        api::api_v1_chat_completions,
        api::api_v1_responses,
        api::api_v1_responses_compact,
        api::responses_lookup::api_v1_responses_get,
        api::responses_lookup::api_v1_responses_cancel,
        api::responses_lookup::api_v1_responses_delete,
        api::containers::api_v1_containers_create,
        api::containers::api_v1_containers_list,
        api::containers::api_v1_containers_get,
        api::containers::api_v1_containers_delete,
        api::containers::api_v1_containers_list_files,
        api::containers::api_v1_containers_file_upload,
        api::containers::api_v1_containers_file_get,
        api::containers::api_v1_containers_file_delete,
        api::containers::api_v1_containers_file_content,
        // Videos API (OpenAI-compatible)
        api::videos::api_v1_videos_create,
        api::videos::api_v1_videos_list,
        api::videos::api_v1_videos_retrieve,
        api::videos::api_v1_videos_delete,
        api::videos::api_v1_videos_content,
        api::videos::api_v1_videos_remix,
        api::videos::api_v1_videos_edits,
        api::videos::api_v1_videos_extensions,
        api::videos::api_v1_videos_characters_create,
        api::videos::api_v1_videos_characters_retrieve,
        // Public API - Skills
        api::skills::api_v1_skills_create,
        api::skills::api_v1_skills_list,
        api::skills::api_v1_skills_get,
        api::skills::api_v1_skills_set_default,
        api::skills::api_v1_skills_delete,
        api::skills::api_v1_skills_get_content,
        api::skills::api_v1_skills_create_version,
        api::skills::api_v1_skills_list_versions,
        api::skills::api_v1_skills_get_version,
        api::skills::api_v1_skills_delete_version,
        api::skills::api_v1_skills_get_version_content,
        api::api_v1_completions,
        api::api_v1_embeddings,
        api::api_v1_models,
        // Self-service endpoints (current user)
        admin::me::export,
        admin::me::delete,
        admin::me::eligible_owners,
        admin::me_providers::list,
        admin::me_providers::create,
        admin::me_providers::get,
        admin::me_providers::update,
        admin::me_providers::delete,
        admin::me_providers::test_connectivity,
        admin::me_providers::test_credentials,
        admin::me_providers::built_in_providers,
        // Self-service endpoints - API Keys
        admin::me_api_keys::get,
        admin::me_api_keys::list,
        admin::me_api_keys::create,
        admin::me_api_keys::revoke,
        admin::me_api_keys::rotate,
        // OAuth-style PKCE flow
        admin::oauth::authorize,
        admin::oauth::preflight,
        crate::routes::oauth_public::token,
        crate::routes::oauth_public::authorization_server_metadata,
        // Self-service endpoints - Sessions
        admin::me_sessions::list,
        admin::me_sessions::delete_one,
        // Admin routes - Organizations
        admin::organizations::create,
        admin::organizations::get,
        admin::organizations::list,
        admin::organizations::update,
        admin::organizations::delete,
        // Admin routes - Projects
        admin::projects::create,
        admin::projects::get,
        admin::projects::list,
        admin::projects::update,
        admin::projects::delete,
        // Admin routes - Users
        admin::users::create,
        admin::users::get,
        admin::users::list,
        admin::users::update,
        admin::users::delete,
        admin::users::export,
        // Admin routes - User Sessions
        admin::sessions::list,
        admin::sessions::delete_all,
        admin::sessions::delete_one,
        admin::users::list_org_members,
        admin::users::add_org_member,
        admin::users::remove_org_member,
        admin::users::list_project_members,
        admin::users::add_project_member,
        admin::users::remove_project_member,
        // Admin routes - API Keys
        admin::api_keys::create,
        admin::api_keys::list_by_org,
        admin::api_keys::list_by_project,
        admin::api_keys::list_by_user,
        admin::api_keys::list_by_service_account,
        admin::api_keys::revoke,
        admin::api_keys::rotate,
        // Admin routes - Dynamic Providers
        admin::dynamic_providers::create,
        admin::dynamic_providers::get,
        admin::dynamic_providers::update,
        admin::dynamic_providers::delete,
        admin::dynamic_providers::list_by_org,
        admin::dynamic_providers::list_by_project,
        admin::dynamic_providers::list_by_user,
        admin::dynamic_providers::test_connectivity,
        admin::dynamic_providers::test_credentials,
        // Admin routes - Usage (API Key level)
        admin::usage::get_summary,
        admin::usage::get_by_date,
        admin::usage::get_by_model,
        admin::usage::get_by_referer,
        admin::usage::get_forecast,
        // Admin routes - Usage (Organization level)
        admin::usage::get_org_summary,
        admin::usage::get_org_by_date,
        admin::usage::get_org_by_model,
        admin::usage::get_org_by_provider,
        admin::usage::get_org_forecast,
        // Admin routes - Usage (API Key by-provider and time series)
        admin::usage::get_by_provider,
        admin::usage::get_by_date_model,
        admin::usage::get_by_date_provider,
        admin::usage::get_by_pricing_source,
        admin::usage::get_by_date_pricing_source,
        // Admin routes - Usage (Project level)
        admin::usage::get_project_summary,
        admin::usage::get_project_by_date,
        admin::usage::get_project_by_model,
        admin::usage::get_project_by_provider,
        admin::usage::get_project_by_date_model,
        admin::usage::get_project_by_date_provider,
        admin::usage::get_project_by_pricing_source,
        admin::usage::get_project_by_date_pricing_source,
        admin::usage::get_project_forecast,
        // Admin routes - Usage (User level)
        admin::usage::get_user_summary,
        admin::usage::get_user_by_date,
        admin::usage::get_user_by_model,
        admin::usage::get_user_by_provider,
        admin::usage::get_user_by_date_model,
        admin::usage::get_user_by_date_provider,
        admin::usage::get_user_by_pricing_source,
        admin::usage::get_user_by_date_pricing_source,
        admin::usage::get_user_forecast,
        // Admin routes - Usage (Team level)
        admin::usage::get_team_summary,
        admin::usage::get_team_by_date,
        admin::usage::get_team_by_model,
        admin::usage::get_team_by_provider,
        admin::usage::get_team_by_date_model,
        admin::usage::get_team_by_date_provider,
        admin::usage::get_team_by_pricing_source,
        admin::usage::get_team_by_date_pricing_source,
        admin::usage::get_team_forecast,
        // Admin routes - Usage (Provider level)
        admin::usage::get_provider_summary,
        admin::usage::get_provider_by_date,
        admin::usage::get_provider_by_model,
        admin::usage::get_provider_forecast,
        // Admin routes - Usage (Org time series)
        admin::usage::get_org_by_date_model,
        admin::usage::get_org_by_date_provider,
        admin::usage::get_org_by_pricing_source,
        admin::usage::get_org_by_date_pricing_source,
        // Admin routes - Usage (Org entity breakdowns)
        admin::usage::get_org_by_user,
        admin::usage::get_org_by_date_user,
        admin::usage::get_org_by_project,
        admin::usage::get_org_by_date_project,
        admin::usage::get_org_by_team,
        admin::usage::get_org_by_date_team,
        // Admin routes - Usage (Project entity breakdowns)
        admin::usage::get_project_by_user,
        admin::usage::get_project_by_date_user,
        // Admin routes - Usage (Team entity breakdowns)
        admin::usage::get_team_by_user,
        admin::usage::get_team_by_date_user,
        admin::usage::get_team_by_project,
        admin::usage::get_team_by_date_project,
        // Admin routes - Usage (Global)
        admin::usage::get_global_summary,
        admin::usage::get_global_by_date,
        admin::usage::get_global_by_model,
        admin::usage::get_global_by_provider,
        admin::usage::get_global_by_pricing_source,
        admin::usage::get_global_by_date_model,
        admin::usage::get_global_by_date_provider,
        admin::usage::get_global_by_date_pricing_source,
        admin::usage::get_global_by_user,
        admin::usage::get_global_by_date_user,
        admin::usage::get_global_by_project,
        admin::usage::get_global_by_date_project,
        admin::usage::get_global_by_team,
        admin::usage::get_global_by_date_team,
        admin::usage::get_global_by_org,
        admin::usage::get_global_by_date_org,
        // Admin routes - Usage (Self-service)
        admin::usage::get_me_summary,
        admin::usage::get_me_by_date,
        admin::usage::get_me_by_model,
        admin::usage::get_me_by_provider,
        admin::usage::get_me_by_date_model,
        admin::usage::get_me_by_date_provider,
        admin::usage::get_me_by_pricing_source,
        admin::usage::get_me_by_date_pricing_source,
        // Admin routes - Usage Logs
        admin::usage::list_logs,
        admin::usage::list_me_logs,
        admin::usage::export_logs,
        admin::usage::export_me_logs,
        // Admin routes - Model Pricing
        admin::model_pricing::create,
        admin::model_pricing::get,
        admin::model_pricing::update,
        admin::model_pricing::delete,
        admin::model_pricing::list_global,
        admin::model_pricing::list_by_provider,
        admin::model_pricing::list_by_org,
        admin::model_pricing::list_by_project,
        admin::model_pricing::list_by_user,
        admin::model_pricing::upsert,
        admin::model_pricing::bulk_upsert,
        // Admin routes - Conversations
        admin::conversations::create,
        admin::conversations::get,
        admin::conversations::update,
        admin::conversations::delete,
        admin::conversations::append_messages,
        admin::conversations::set_pin,
        admin::conversations::list_by_project,
        admin::conversations::list_by_user,
        admin::conversations::list_accessible_for_user,
        // Admin routes - Templates
        admin::templates::create,
        admin::templates::get,
        admin::templates::update,
        admin::templates::delete,
        admin::templates::list_by_org,
        admin::templates::list_by_team,
        admin::templates::list_by_project,
        admin::templates::list_by_user,
        // Admin routes - Provider Management
        admin::providers::list_circuit_breakers,
        admin::providers::get_circuit_breaker,
        admin::providers::list_provider_health,
        admin::providers::get_provider_health,
        admin::providers::list_provider_stats,
        admin::providers::get_provider_stats,
        admin::providers::get_provider_stats_history,
        // Admin routes - Dead Letter Queue
        admin::dlq::list,
        admin::dlq::get,
        admin::dlq::delete,
        admin::dlq::retry,
        admin::dlq::stats,
        admin::dlq::purge,
        admin::dlq::prune,
        // Admin routes - Audit Logs
        admin::audit_logs::list,
        admin::audit_logs::get,
        // Admin routes - Access Reviews
        admin::access_reviews::get_inventory,
        admin::access_reviews::get_stale_access,
        admin::access_reviews::get_org_access_report,
        admin::access_reviews::get_user_access_summary,
        // Admin routes - Teams
        admin::teams::create,
        admin::teams::get,
        admin::teams::list,
        admin::teams::update,
        admin::teams::delete,
        admin::teams::list_members,
        admin::teams::add_member,
        admin::teams::update_member,
        admin::teams::remove_member,
        // Admin routes - Service Accounts
        admin::service_accounts::create,
        admin::service_accounts::get,
        admin::service_accounts::list,
        admin::service_accounts::update,
        admin::service_accounts::delete,
        // Admin routes - SSO Connections (read-only, from config)
        admin::sso_connections::list,
        admin::sso_connections::get,
        // Admin routes - Session Info (debugging)
        admin::session_info::get,
        // Admin routes - SSO Group Mappings
        admin::sso_group_mappings::list,
        admin::sso_group_mappings::create,
        admin::sso_group_mappings::get,
        admin::sso_group_mappings::update,
        admin::sso_group_mappings::delete,
        admin::sso_group_mappings::test,
        admin::sso_group_mappings::export,
        admin::sso_group_mappings::import,
        // Admin routes - Organization SSO Config
        admin::org_sso_configs::get,
        admin::org_sso_configs::create,
        admin::org_sso_configs::update,
        admin::org_sso_configs::delete,
        // SAML metadata endpoints are conditionally added at runtime via merge_saml_openapi()
        // when the saml feature is enabled (parse_saml_metadata, get_sp_metadata)
        // Admin routes - Organization RBAC Policies
        admin::org_rbac_policies::list,
        admin::org_rbac_policies::create,
        admin::org_rbac_policies::get,
        admin::org_rbac_policies::update,
        admin::org_rbac_policies::delete,
        admin::org_rbac_policies::list_versions,
        admin::org_rbac_policies::rollback,
        admin::org_rbac_policies::simulate,
        admin::org_rbac_policies::validate,
        // Admin routes - Domain Verifications
        admin::domain_verifications::list,
        admin::domain_verifications::create,
        admin::domain_verifications::get,
        admin::domain_verifications::get_instructions,
        admin::domain_verifications::delete,
        admin::domain_verifications::verify,
        // Admin routes - Organization SCIM Config
        admin::scim_configs::get,
        admin::scim_configs::create,
        admin::scim_configs::update,
        admin::scim_configs::delete,
        admin::scim_configs::rotate_token,
        // Images API (OpenAI-compatible)
        api::api_v1_images_generations,
        api::api_v1_images_edits,
        api::api_v1_images_variations,
        // Audio API (OpenAI-compatible)
        api::api_v1_audio_speech,
        api::api_v1_audio_transcriptions,
        api::api_v1_audio_translations,
        // Files API (OpenAI-compatible, under /api/v1)
        api::api_v1_files_upload,
        api::api_v1_files_list,
        api::api_v1_files_get,
        api::api_v1_files_get_content,
        api::api_v1_files_delete,
        // API routes - Vector Stores
        api::api_v1_vector_stores_create,
        api::api_v1_vector_stores_list,
        api::api_v1_vector_stores_get,
        api::api_v1_vector_stores_modify,
        api::api_v1_vector_stores_delete,
        // API routes - Vector Store Files
        api::api_v1_vector_stores_create_file,
        api::api_v1_vector_stores_list_files,
        api::api_v1_vector_stores_get_file,
        api::api_v1_vector_stores_delete_file,
        // API routes - Vector Store File Batches
        api::api_v1_vector_stores_create_file_batch,
        api::api_v1_vector_stores_get_file_batch,
        api::api_v1_vector_stores_cancel_file_batch,
        api::api_v1_vector_stores_list_batch_files,
        // API routes - Vector Store Chunks & Search (Hadrian extensions)
        api::api_v1_vector_stores_list_file_chunks,
        api::api_v1_vector_stores_search,
        // API routes - Tools (Hadrian extensions)
        api::web_search,
        api::web_fetch,
    ),
    components(schemas(
        // API types - Chat Completion
        api_types::CreateChatCompletionPayload,
        api_types::Message,
        api_types::MessageContent,
        api_types::chat_completion::ContentPart,
        api_types::chat_completion::ImageUrl,
        api_types::chat_completion::ImageUrlDetail,
        api_types::chat_completion::VideoUrl,
        api_types::chat_completion::InputAudio,
        api_types::chat_completion::InputAudioFormat,
        api_types::chat_completion::ReasoningEffort,
        api_types::chat_completion::ReasoningSummary,
        api_types::chat_completion::CreateChatCompletionReasoning,
        api_types::chat_completion::ResponseFormat,
        api_types::chat_completion::JsonSchemaConfig,
        api_types::chat_completion::Stop,
        api_types::chat_completion::StreamOptions,
        api_types::chat_completion::ToolChoice,
        api_types::chat_completion::ToolChoiceDefaults,
        api_types::chat_completion::NamedToolChoice,
        api_types::chat_completion::NamedToolChoiceFunction,
        api_types::chat_completion::ToolType,
        api_types::chat_completion::ToolDefinition,
        api_types::chat_completion::ToolDefinitionFunction,
        api_types::chat_completion::ToolCall,
        api_types::chat_completion::ToolCallFunction,
        // API types - Completions
        api_types::CreateCompletionPayload,
        // API types - Embeddings
        api_types::CreateEmbeddingPayload,
        api_types::embeddings::EmbeddingInput,
        api_types::embeddings::EncodingFormat,
        // API types - Images
        api_types::CreateImageRequest,
        api_types::CreateImageEditRequest,
        api_types::CreateImageVariationRequest,
        api_types::ImagesResponse,
        api_types::images::Image,
        api_types::images::ImageUsage,
        api_types::images::ImageModel,
        api_types::images::ImageQuality,
        api_types::images::ImageResponseFormat,
        api_types::images::ImageOutputFormat,
        api_types::images::ImageSize,
        api_types::images::ImageStyle,
        api_types::images::ImageBackground,
        api_types::images::ImageModeration,
        // API types - Videos
        api_types::CreateVideoRequest,
        api_types::Video,
        api_types::VideoListResponse,
        api_types::VideoDeleteResponse,
        api_types::RemixVideoRequest,
        api_types::VideoEditRequest,
        api_types::VideoExtensionRequest,
        api_types::videos::CreateCharacterRequest,
        api_types::videos::VideoRef,
        api_types::videos::VideoError,
        api_types::Character,
        api_types::videos::InputReference,
        api_types::videos::VideoModel,
        api_types::videos::VideoStatus,
        api_types::videos::VideoSize,
        api_types::videos::VideoSeconds,
        api_types::videos::VideoVariant,
        // API types - Audio
        api_types::CreateSpeechRequest,
        api_types::CreateTranscriptionRequest,
        api_types::CreateTranslationRequest,
        api_types::audio::Voice,
        api_types::audio::SpeechResponseFormat,
        api_types::audio::SpeechStreamFormat,
        api_types::audio::AudioResponseFormat,
        api_types::audio::TimestampGranularity,
        api_types::audio::TranscriptionInclude,
        api_types::audio::TranscriptionChunkingStrategy,
        api_types::audio::TranscriptionResponse,
        api_types::audio::TranscriptionVerboseResponse,
        api_types::audio::TranscriptionDiarizedResponse,
        api_types::audio::TranscriptionWord,
        api_types::audio::TranscriptionSegment,
        api_types::audio::TranscriptionDiarizedSegment,
        api_types::audio::TranscriptionLogprob,
        api_types::audio::TranscriptionInputTokenDetails,
        api_types::audio::TranscriptionUsageTokens,
        api_types::audio::TranscriptionUsageDuration,
        api_types::audio::TranscriptionUsage,
        api_types::audio::TranslationResponse,
        api_types::audio::TranslationVerboseResponse,
        // API types - Responses
        api_types::CreateResponsesPayload,
        api_types::CompactRequest,
        // API types - Containers
        api::containers::CreateContainerRequest,
        api_types::responses::ContainerExpiresAfter,
        api_types::responses::ContainerExpiresAfterAnchor,
        api_types::responses::ShellNetworkPolicy,
        // API types - Responses tool definitions (`tools[]` variants).
        // Registered so the OpenAPI spec emits a `oneOf` of named tool
        // schemas under the `Tool` slot; the conformance script matches
        // variants against OpenAI's `Tool` union by literal `type` value.
        api_types::responses::ResponsesToolDefinition,
        api_types::responses::FunctionTool,
        api_types::responses::FunctionToolType,
        api_types::responses::McpTool,
        api_types::responses::McpToolType,
        api_types::responses::McpRequireApproval,
        api_types::responses::McpApprovalMode,
        api_types::responses::McpApprovalFilter,
        api_types::responses::McpAllowedTools,
        api_types::responses::McpToolFilter,
        api_types::responses::ToolSearchTool,
        api_types::responses::ToolSearchToolType,
        api_types::responses::ToolSearchExecution,
        api_types::responses::ToolSearchRankerKind,
        api_types::responses::ShellTool,
        api_types::responses::ShellToolType,
        api_types::responses::FileSearchTool,
        api_types::responses::FileSearchToolType,
        api_types::responses::WebSearchPreviewTool,
        api_types::responses::WebSearchPreviewToolType,
        api_types::responses::WebSearchPreview20250311Tool,
        api_types::responses::WebSearchPreview20250311ToolType,
        api_types::responses::WebSearchTool,
        api_types::responses::WebSearchToolType,
        api_types::responses::WebSearch20250826Tool,
        api_types::responses::WebSearch20250826ToolType,
        // Models response
        api::CombinedModelsResponse,
        // Admin models - Organization
        models::Organization,
        models::CreateOrganization,
        models::UpdateOrganization,
        // Admin models - Project
        models::Project,
        models::CreateProject,
        models::UpdateProject,
        // Browser auth response shapes
        crate::routes::auth::MeResponse,
        crate::routes::auth::DiscoverResponse,
        // Admin models - User
        models::User,
        models::CreateUser,
        models::UpdateUser,
        models::UserDeletionResponse,
        // GDPR Export types
        models::UserDataExport,
        models::UserMemberships,
        models::UserOrgMembership,
        models::UserProjectMembership,
        models::ExportedApiKey,
        models::ExportedUsageSummary,
        // Self-service eligible owners (OAuth owner picker)
        admin::me::EligibleOwner,
        admin::me::EligibleOwnersResponse,
        // Admin models - API Key
        models::ApiKey,
        models::ApiKeyScope,
        models::CreateApiKey,
        models::CreatedApiKey,
        models::ApiKeyOwner,
        models::BudgetPeriod,
        admin::api_keys::RotateApiKeyRequest,
        // OAuth PKCE flow
        models::CreateAuthorizationCode,
        models::AuthorizationCodeResponse,
        models::ExchangeCodeForKey,
        models::OAuthKeyOptions,
        models::PkceCodeChallengeMethod,
        admin::oauth::PreflightResponse,
        crate::routes::oauth_public::OAuthTokenResponse,
        crate::routes::oauth_public::AuthorizationServerMetadata,
        // Admin models - Dynamic Provider
        models::DynamicProvider,
        models::DynamicProviderResponse,
        models::CreateDynamicProvider,
        models::CreateSelfServiceProvider,
        models::CreateSelfServiceApiKey,
        models::UpdateDynamicProvider,
        models::ConnectivityTestResponse,
        models::ProviderOwner,
        admin::me_providers::SelfServiceProviderListResponse,
        admin::me_providers::BuiltInProvider,
        admin::me_providers::BuiltInProvidersResponse,
        // Admin models - Model Pricing
        models::DbModelPricing,
        models::CreateModelPricing,
        models::UpdateModelPricing,
        models::PricingOwner,
        models::PricingSource,
        // Admin routes - Usage response types
        admin::usage::UsageQuery,
        admin::usage::UsageSummaryResponse,
        admin::usage::DailySpendResponse,
        admin::usage::ModelSpendResponse,
        admin::usage::RefererSpendResponse,
        admin::usage::ProviderSpendResponse,
        admin::usage::ForecastQuery,
        admin::usage::CostForecastResponse,
        admin::usage::TimeSeriesForecastResponse,
        admin::usage::DailyModelSpendResponse,
        admin::usage::DailyProviderSpendResponse,
        admin::usage::PricingSourceSpendResponse,
        admin::usage::DailyPricingSourceSpendResponse,
        admin::usage::UserSpendResponse,
        admin::usage::DailyUserSpendResponse,
        admin::usage::ProjectSpendResponse,
        admin::usage::DailyProjectSpendResponse,
        admin::usage::TeamSpendResponse,
        admin::usage::DailyTeamSpendResponse,
        admin::usage::OrgSpendResponse,
        admin::usage::DailyOrgSpendResponse,
        admin::usage::UsageLogResponse,
        admin::usage::UsageLogListResponse,
        admin::usage::UsageLogExportFormat,
        // Admin routes - Users
        admin::users::AddMemberRequest,
        admin::users::UserListResponse,
        // Admin routes - User Sessions
        admin::sessions::SessionInfo,
        admin::sessions::SessionListResponse,
        admin::sessions::SessionsRevokedResponse,
        crate::auth::session_store::DeviceInfo,
        // Admin routes - Organizations
        admin::organizations::ListQuery,
        admin::organizations::OrganizationListResponse,
        // Admin routes - Projects
        admin::projects::ProjectListResponse,
        // Admin routes - Model Pricing
        admin::model_pricing::BulkUpsertResponse,
        // Admin models - Conversation
        models::Conversation,
        models::ConversationWithProject,
        models::CreateConversation,
        models::UpdateConversation,
        models::SetPinOrder,
        models::AppendMessages,
        models::ConversationOwner,
        models::ConversationOwnerType,
        models::Message,
        admin::conversations::ConversationListResponse,
        admin::conversations::ConversationWithProjectListResponse,
        admin::conversations::ListAccessibleQuery,
        // Admin models - Template
        models::Template,
        models::CreateTemplate,
        models::UpdateTemplate,
        models::TemplateOwner,
        models::TemplateOwnerType,
        admin::templates::TemplateListResponse,
        // Public API - Skills (OpenAI-compatible, with Hadrian extensions)
        models::SkillId,
        models::SkillVersionId,
        models::SkillFile,
        models::SkillFileInput,
        models::SkillFileManifest,
        models::SkillOwner,
        models::SkillOwnerType,
        api::skills::SkillResource,
        api::skills::SkillVersionResource,
        api::skills::SkillListResource,
        api::skills::SkillVersionListResource,
        api::skills::DeletedSkillResource,
        api::skills::DeletedSkillVersionResource,
        api::skills::CreateSkillBody,
        api::skills::CreateSkillVersionBody,
        api::skills::SetDefaultSkillVersionBody,
        // Admin routes - DLQ
        admin::dlq::DlqListQuery,
        admin::dlq::DlqEntryResponse,
        admin::dlq::DlqStatsResponse,
        admin::dlq::DlqRetryResponse,
        admin::dlq::PruneQuery,
        // Admin routes - Providers
        admin::providers::CircuitBreakersResponse,
        admin::providers::ProviderCircuitBreakerResponse,
        admin::providers::ProviderHealthResponse,
        admin::providers::ProviderStatsResponse,
        admin::providers::ProviderStatsHistoryQuery,
        crate::providers::CircuitBreakerStatus,
        crate::jobs::ProviderHealthState,
        crate::providers::health_check::HealthStatus,
        crate::services::ProviderStats,
        crate::services::ProviderStatsHistorical,
        crate::services::TimeBucketStats,
        crate::services::StatsGranularity,
        // Admin routes - Audit Logs
        admin::audit_logs::AuditLogListResponse,
        models::AuditLog,
        models::AuditLogQuery,
        models::AuditActorType,
        // Access Review types
        models::ExportFormat,
        models::AccessInventoryResponse,
        models::AccessInventorySummary,
        models::AccessInventoryQuery,
        models::UserAccessInventoryEntry,
        models::OrgAccessEntry,
        models::ProjectAccessEntry,
        models::ApiKeySummary,
        // Organization Access Report types
        models::OrgAccessReportResponse,
        models::OrgAccessReportSummary,
        models::OrgAccessReportQuery,
        models::OrgMemberAccessEntry,
        models::OrgMemberProjectAccess,
        models::OrgApiKeyEntry,
        models::AccessGrantHistoryEntry,
        // User Access Summary types
        models::UserAccessSummaryResponse,
        models::UserAccessSummaryQuery,
        models::UserAccessOrgEntry,
        models::UserAccessProjectEntry,
        models::UserAccessApiKeyEntry,
        models::UserAccessSummary,
        // Team types
        models::Team,
        models::CreateTeam,
        models::UpdateTeam,
        models::TeamMembership,
        models::TeamMember,
        models::AddTeamMember,
        models::UpdateTeamMember,
        admin::teams::TeamListResponse,
        admin::teams::TeamMemberListResponse,
        // Service Account types
        models::ServiceAccount,
        models::CreateServiceAccount,
        models::UpdateServiceAccount,
        admin::service_accounts::ServiceAccountListResponse,
        // SSO Connection types
        admin::sso_connections::SsoConnection,
        admin::sso_connections::SsoConnectionsResponse,
        // Session Info types
        admin::session_info::SessionInfoResponse,
        admin::session_info::IdentityInfo,
        admin::session_info::UserInfo,
        admin::session_info::OrgMembershipInfo,
        admin::session_info::TeamMembershipInfo,
        admin::session_info::ProjectMembershipInfo,
        admin::session_info::SsoConnectionInfo,
        // SSO Group Mapping types
        models::SsoGroupMapping,
        models::CreateSsoGroupMapping,
        models::UpdateSsoGroupMapping,
        admin::sso_group_mappings::SsoGroupMappingListResponse,
        admin::sso_group_mappings::TestMappingRequest,
        admin::sso_group_mappings::TestMappingResult,
        admin::sso_group_mappings::TestMappingResponse,
        admin::sso_group_mappings::ExportFormat,
        admin::sso_group_mappings::ExportMappingEntry,
        admin::sso_group_mappings::ExportResponse,
        admin::sso_group_mappings::ImportConflictStrategy,
        admin::sso_group_mappings::ImportMappingEntry,
        admin::sso_group_mappings::ImportRequest,
        admin::sso_group_mappings::ImportError,
        admin::sso_group_mappings::ImportResponse,
        // Organization SSO Config types
        models::OrgSsoConfig,
        models::CreateOrgSsoConfig,
        models::UpdateOrgSsoConfig,
        models::SsoProviderType,
        models::SsoEnforcementMode,
        // Organization RBAC Policy types
        models::OrgRbacPolicy,
        models::OrgRbacPolicyVersion,
        models::CreateOrgRbacPolicy,
        models::UpdateOrgRbacPolicy,
        models::RollbackOrgRbacPolicy,
        models::RbacPolicyEffect,
        admin::org_rbac_policies::OrgRbacPolicyListResponse,
        admin::org_rbac_policies::OrgRbacPolicyVersionListResponse,
        admin::org_rbac_policies::SimulatePolicyRequest,
        admin::org_rbac_policies::SimulatePolicyResponse,
        admin::org_rbac_policies::SimulateSubject,
        admin::org_rbac_policies::SimulateContext,
        admin::org_rbac_policies::PolicyEvaluationResult,
        admin::org_rbac_policies::PolicySource,
        admin::org_rbac_policies::ValidateCelRequest,
        admin::org_rbac_policies::ValidateCelResponse,
        // Domain Verification types
        models::DomainVerification,
        models::CreateDomainVerification,
        models::DomainVerificationStatus,
        models::DomainVerificationInstructions,
        models::VerifyDomainResponse,
        admin::domain_verifications::ListDomainVerificationsResponse,
        // Organization SCIM Config types
        models::OrgScimConfig,
        models::CreateOrgScimConfig,
        models::UpdateOrgScimConfig,
        models::CreatedOrgScimConfig,
        // Stale Access Detection types
        models::StaleAccessQuery,
        models::StaleAccessResponse,
        models::StaleAccessSummary,
        models::StaleUserEntry,
        models::StaleApiKeyEntry,
        models::NeverActiveUserEntry,
        // Files API types
        models::File,
        models::FilePurpose,
        models::FileStatus,
        models::VectorStoreOwnerType,
        api::ListFilesQuery,
        api::FileListResponse,
        api::DeleteFileResponse,
        // Vector Store types
        models::VectorStore,
        models::VectorStoreStatus,
        models::VectorStoreFile,
        models::VectorStoreFileStatus,
        models::VectorStoreOwner,
        models::CreateVectorStore,
        models::UpdateVectorStore,
        models::FileCounts,
        models::ExpiresAfter,
        models::FileError,
        models::ChunkingStrategy,
        api::ListVectorStoresQuery,
        api::VectorStoreListResponse,
        api::DeleteVectorStoreResponse,
        api::CreateVectorStoreFileRequest,
        api::ListVectorStoreFilesQuery,
        api::VectorStoreFileListResponse,
        api::DeleteVectorStoreFileResponse,
        api::FileBatch,
        api::FileBatchCounts,
        api::CreateFileBatchRequest,
        // Vector Store Chunks & Search (Hadrian extensions)
        api::ChunkResponse,
        api::ChunkListResponse,
        api::VectorStoreSearchRequest,
        api::SearchResultItem,
        api::VectorStoreSearchResponse,
        // Attribute filter types (OpenAI-compatible)
        models::AttributeFilter,
        models::ComparisonFilter,
        models::CompoundFilter,
        models::ComparisonOperator,
        models::LogicalOperator,
        models::FilterValue,
        models::FilterValueItem,
        // Ranking options (OpenAI-compatible)
        models::FileSearchRankingOptions,
        models::FileSearchRanker,
        // Tools types (Hadrian extensions)
        api::WebSearchRequest,
        api::WebSearchResponse,
        api::WebSearchResult,
        api::WebFetchRequest,
        api::WebFetchResponse,
        // Error response
        ErrorResponse,
        ErrorInfo,
        // Pagination
        PaginationMeta,
        // Health check types
        health::HealthStatus,
        health::SubsystemStatus,
        health::ComponentStatus,
    )),
    security(
        ("api_key" = [])
    ),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;

#[cfg(all(feature = "utoipa", feature = "saml"))]
#[derive(OpenApi)]
#[openapi(
    paths(
        admin::org_sso_configs::parse_saml_metadata,
        admin::org_sso_configs::get_sp_metadata,
    ),
    components(schemas(
        admin::org_sso_configs::ParseSamlMetadataRequest,
        admin::org_sso_configs::ParsedSamlIdpConfig,
    ))
)]
struct SamlApiDoc;

#[cfg(feature = "utoipa")]
impl ApiDoc {
    /// Build the full OpenAPI spec, conditionally including SAML endpoints.
    #[allow(unused_mut)]
    pub fn build() -> utoipa::openapi::OpenApi {
        let mut spec = Self::openapi();
        #[cfg(feature = "saml")]
        {
            let saml_spec = SamlApiDoc::openapi();
            spec.merge(saml_spec);
        }
        spec
    }
}

/// Standard error response body
#[derive(serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ErrorResponse {
    /// Error information
    pub error: ErrorInfo,
}

/// Error information matching OpenAI's error schema.
///
/// OpenAI error format: `{"error": {"type": "...", "message": "...", "param": ..., "code": ...}}`
#[derive(serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ErrorInfo {
    /// Error type classification (e.g., "invalid_request_error", "authentication_error")
    #[cfg_attr(feature = "utoipa", schema(example = "invalid_request_error"))]
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-readable error message
    #[cfg_attr(
        feature = "utoipa",
        schema(example = "Budget limit exceeded for monthly period")
    )]
    pub message: String,
    /// Parameter that caused the error (null if not applicable)
    #[cfg_attr(feature = "utoipa", schema(example = json!(null)))]
    pub param: Option<String>,
    /// Machine-readable error code (null if not applicable)
    #[cfg_attr(feature = "utoipa", schema(example = "budget_exceeded"))]
    pub code: Option<String>,
    /// **Hadrian Extension:** Request ID for correlating errors with logs.
    /// This field is automatically populated by the gateway middleware.
    #[cfg_attr(
        feature = "utoipa",
        schema(example = "550e8400-e29b-41d4-a716-446655440000")
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ErrorResponse {
    /// Create a new error response (OpenAI-compatible format).
    ///
    /// Uses "invalid_request_error" as the default error type.
    /// The `request_id` field is automatically populated by the gateway middleware.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ErrorInfo {
                error_type: "invalid_request_error".to_string(),
                message: message.into(),
                param: None,
                code: Some(code.into()),
                request_id: None,
            },
        }
    }

    /// Create a new error response with a parameter reference.
    ///
    /// Use this when the error is caused by a specific request parameter.
    /// The `request_id` field is automatically populated by the gateway middleware.
    pub fn with_param(
        code: impl Into<String>,
        message: impl Into<String>,
        param: impl Into<String>,
    ) -> Self {
        Self {
            error: ErrorInfo {
                error_type: "invalid_request_error".to_string(),
                message: message.into(),
                param: Some(param.into()),
                code: Some(code.into()),
                request_id: None,
            },
        }
    }

    /// Create a new error response with explicit error type.
    ///
    /// Common error types:
    /// - "invalid_request_error" - Invalid parameters or malformed request
    /// - "authentication_error" - Invalid API key or unauthorized
    /// - "permission_error" - Valid API key but lacking permissions
    /// - "not_found_error" - Resource not found
    /// - "rate_limit_error" - Rate limit exceeded
    /// - "server_error" - Internal server error
    ///
    /// The `request_id` field is automatically populated by the gateway middleware.
    pub fn with_type(
        error_type: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            error: ErrorInfo {
                error_type: error_type.into(),
                message: message.into(),
                param: None,
                code: Some(code.into()),
                request_id: None,
            },
        }
    }

    /// Create a new error response with details (Hadrian extension).
    ///
    /// Note: OpenAI's API does not have a `details` field. This serializes
    /// details into the message for compatibility.
    #[deprecated(note = "Use with_param or with_type for OpenAI compatibility")]
    pub fn with_details(
        code: impl Into<String>,
        message: impl Into<String>,
        _details: serde_json::Value,
    ) -> Self {
        // For backwards compatibility, just ignore details since OpenAI doesn't support them
        Self::new(code, message)
    }
}

/// Pagination metadata for list responses using cursor-based pagination.
///
/// Cursor-based pagination provides stable, performant navigation for large datasets.
/// Use `next_cursor` and `prev_cursor` to navigate between pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct PaginationMeta {
    /// Maximum number of records returned per page.
    #[cfg_attr(feature = "utoipa", schema(example = 100))]
    pub limit: i64,
    /// Whether there are more records available after this page.
    #[cfg_attr(feature = "utoipa", schema(example = true))]
    pub has_more: bool,
    /// Cursor for fetching the next page.
    #[cfg_attr(
        feature = "utoipa",
        schema(example = "MTczMzU4MDgwMDAwMDphYmMxMjM0NS02Nzg5LTAxMjMtNDU2Ny0wMTIzNDU2Nzg5YWI")
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Cursor for fetching the previous page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<String>,
}

impl PaginationMeta {
    /// Create pagination metadata for cursor-based pagination.
    ///
    /// # Arguments
    /// * `limit` - Maximum number of records per page
    /// * `has_more` - Whether there are more records after this page
    /// * `next_cursor` - Cursor for the next page (if has_more is true)
    /// * `prev_cursor` - Cursor for the previous page (if not on first page)
    pub fn with_cursors(
        limit: i64,
        has_more: bool,
        next_cursor: Option<String>,
        prev_cursor: Option<String>,
    ) -> Self {
        Self {
            limit,
            has_more,
            next_cursor,
            prev_cursor,
        }
    }
}

#[cfg(feature = "utoipa")]
/// Security scheme and tag groups modifier
struct SecurityAddon;

#[cfg(feature = "utoipa")]
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // Add security scheme
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            utoipa::openapi::security::SecurityScheme::Http(
                utoipa::openapi::security::HttpBuilder::new()
                    .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                    .bearer_format("API Key")
                    .description(Some("API key authentication using Bearer token format"))
                    .build(),
            ),
        );

        // Add x-tagGroups extension for Scalar/Redocly sidebar organization
        let tag_groups = serde_json::json!([
            {
                "name": "Health & Infrastructure",
                "tags": ["health"]
            },
            {
                "name": "Public API",
                "tags": ["chat", "completions", "embeddings", "models", "skills"]
            },
            {
                "name": "Admin API",
                "tags": ["organizations", "projects", "teams", "users", "api-keys", "dynamic-providers", "usage", "model-pricing", "conversations", "dlq", "audit-logs", "access-reviews", "sso", "files", "vector-stores"]
            }
        ]);

        let extensions = openapi.extensions.get_or_insert_with(Default::default);
        extensions.insert("x-tagGroups".to_string(), tag_groups);
    }
}
