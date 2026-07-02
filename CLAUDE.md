# CLAUDE.md

Hadrian is an AI Gateway providing a unified OpenAI-compatible API for routing requests to multiple LLM providers. Fully
open source, no restrictions — runs on anything from a Raspberry Pi to globally distributed cloud infrastructure.

Backend: Rust with Axum. Frontend: React 19, TypeScript, TailwindCSS. Config: `hadrian.toml`.

## General Guidelines

- Write high-quality, idiomatic code using modern language features — terse, not verbose
- Rely on linting, formatting, and type checking; aim for high test coverage
- No backwards compatibility concerns yet — modify migrations, schemas, APIs, config as needed (keep sqlite and postgres
  in sync)
- No unused imports, `todo!`s, or dead code — implement or explain why not

## Agent Instructions

Read files in `agent_instructions/` for detailed guidance on specific tasks:

- `adding_admin_endpoint.md` — Admin endpoints and pagination patterns
- `adding_frontend_tool.md` — Frontend tools
- `adding_provider.md` — LLM providers (and how server-side tools rewrite)
- `adding_runtime.md` — Shell-tool runtime backends (passthrough, microsandbox, opensandbox, …)
- `architecture.md` — Multi-tenancy, auth, RBAC, SSO, request flow, RAG, chat modes, caching, server-side tools
- `ci_cd.md` — CI, release, and deploy pipelines
- `configuration.md` — Config sections, feature flags, provider options
- `containers.md` — Responses-API containers, shell tool, lifecycle, file staging
- `database_changes.md` — Database migrations and schema changes
- `documentation.md` — Documentation site, writing guidelines, Storybook embeds
- `frontend_conventions.md` — Frontend conventions, accessibility (WCAG 2.1 AA)
- `key_files.md` — Comprehensive file listing by subsystem
- `modifying_chat_ui.md` — Chat UI performance (stores, selectors, memoization)
- `responses_pipeline.md` — Foreground/background streaming pipeline, server-tool loop
- `testing.md` — Provider e2e tests (wiremock), university E2E tests
- `wasm.md` — WASM build architecture and frontend development

If you encounter issues, unusual behavior, etc. during a session, you MUST document these quirks by updating this
CLAUDE.md file and the agent instructions.

## Backend

### Build & Development

```bash
cargo build                     # Build (default: full features)
cargo build --release           # Release build
cargo test                      # Unit tests
cargo test -- --ignored         # Integration tests
cargo clippy                    # Lint
cargo +nightly fmt              # Format (requires nightly)
cargo run                       # Run with default config (hadrian.toml)
cargo run -- --config path.toml # Run with custom config
docker build -t hadrian:local . # Build gateway image first (compose files use hadrian:local)
cd deploy/tests && pnpm test    # E2E tests with testcontainers
```

### Cargo Features

Hierarchical profiles (default: `full`):

- **`tiny`** — OpenAI + Test providers, no DB, no embedded assets
- **`minimal`** — tiny + all providers, SQLite, embedded UI/catalog, wizard
- **`standard`** — minimal + Postgres, Redis, OTLP, Prometheus, SSO, CEL, S3, secrets managers
- **`full`** — standard + SAML, xberg, ClamAV
- **`headless`** — full without embedded assets
- **`wasm`** — Browser-only build (see `agent_instructions/wasm.md`)

Shell-tool runtime backends are gated by their own feature flags (off by default in every
profile so the heavy SDKs are opt-in):

- **`runtime-microsandbox`** — pulls in the microsandbox SDK + microVM dependencies for the
  in-process `microsandbox` runtime.
- **`runtime-opensandbox`** — pulls in the HTTP client glue for the Alibaba OpenSandbox
  Lifecycle API. No new system dependencies.

`passthrough_openai` and `client_passthrough` runtimes are always available; they require no
extra cargo features. See `agent_instructions/containers.md`.

```bash
cargo build --no-default-features --features tiny       # Smallest binary
cargo build --no-default-features --features minimal    # Fast compile
cargo build --no-default-features --features standard   # Typical deployment
cargo build --no-default-features --features headless   # No embedded assets
```

Server runs on `http://0.0.0.0:8080` by default.

### After Backend Changes

1. `cargo check` — compile errors
2. `cargo clippy` — lint
3. `cargo +nightly fmt` — format
4. `cargo test` — tests

## Frontend

The UI is in `ui/` — React 19, TypeScript, TailwindCSS, Storybook, @tanstack/react-query, hey-api.

pnpm 11 no longer reads the `"pnpm"` field in package.json: settings (`overrides`, `auditConfig`) and
build-script allowances (`allowBuilds` — pnpm 11 blocks dependency build scripts by default) live in
`pnpm-workspace.yaml` next to each package.json (`ui/`, `docs/`, `deploy/tests/`).

```bash
cd ui
pnpm install           # Install dependencies
pnpm dev               # Dev server
pnpm build             # Production build
pnpm lint:fix          # Fix lint errors
pnpm format            # Format code
pnpm storybook         # Component development
pnpm test-storybook    # Storybook tests
pnpm openapi-ts        # Regenerate API client from /api/openapi.json
```

### After Frontend Changes

1. `pnpm lint:fix` — fix lint errors
2. `pnpm format` — format
3. `pnpm test-storybook` — Storybook tests
4. `pnpm build` — production build

Lint, formatting, and a11y errors must be resolved. If ignoring, prompt the user to explain why.

See `agent_instructions/frontend_conventions.md` for conventions and accessibility requirements.

## Architecture Overview

**Multi-tenancy:** Organization → Teams → Users → Projects. Resources (conversations, providers, API keys, vector
stores, files) owned by teams, users, or projects.

**Principals:** User (human identity), ServiceAccount (machine with roles), Machine (shared credential, no roles).

**Request flow:** Client → Middleware (auth → budget) → Route Handler (resolve provider) → LLM Provider (stream) → Usage
Tracking (async).

See `agent_instructions/architecture.md` for details on RBAC, SSO, RAG, chat modes, and more.

## API Conventions

- Admin endpoints: `/admin/v1/` — OpenAI-compatible endpoints: `/v1/`
- OpenAI spec conformance with `**Hadrian Extension:**` doc comments; verify with `./scripts/openapi-conformance.py`
- Spec generates client for the frontend with `./scripts/generate-openapi.sh`
- Reference specs in `openapi/` directory
- Plural nouns for resources, consistent JSON error shapes
- Cursor-based pagination on all list endpoints (see `agent_instructions/adding_admin_endpoint.md`)

## Testing

- Unit tests: same file as code (`#[cfg(test)]`)
- E2E tests: `docker build -t hadrian:local .` (first), then `cd deploy/tests && pnpm test`
- Test both SQLite and PostgreSQL paths
- See `agent_instructions/testing.md` for provider and university E2E tests

## Documentation

Docs site in `docs/` using Fumadocs (Next.js). Keep docs up-to-date when code changes affect user-facing behavior.

```bash
cd docs && pnpm build   # Build static site
cd docs && pnpm dev     # Dev server at localhost:3000
```

Read https://www.fumadocs.dev/llms.txt before updating docs (fetch with curl). See `agent_instructions/documentation.md`
for writing guidelines.

## Security Rules

### Authorization enforcement

Every admin endpoint **must** extract `Extension(authz): Extension<AuthzContext>` and call
`authz.require(resource, action)` before any operation. Reference `routes/admin/teams.rs`.

### Database scoping

Admin handler `get_by_id()` calls with org context **must** use org-scoped variants (`get_by_id_and_org()`).

### URL validation

User-supplied URLs the server fetches **must** go through `validate_base_url()` to block SSRF.

### Error messages

Never expose internal paths, UUIDs, infrastructure details, or secret manager references to clients.

### Credential handling

Never return provider credentials in API responses. Never treat a secret reference as a literal value.

### Cursor pagination timestamps

SQLite repos that use cursor pagination **must** call `truncate_to_millis(Utc::now())` when creating
or updating timestamps. Cursors encode at millisecond precision; without truncation, SQLite TEXT
comparisons fail. See `src/db/repos/cursor.rs` for details.

### Security defaults

Fail-closed: invalid credentials = 401, `fail_on_evaluation_error` = true, IAP auth requires explicit `trusted_proxies`.

## Shell Quirks

- Use `/usr/bin/ls` instead of `ls` (aliased to exa)
- Use `sleep 5s` not `sleep -s 5`

## Debugging

- `RUST_LOG=debug` for verbose logging
- `observability.logging.format = "pretty"` for readable logs
- `/health` for DB connectivity, `/docs` for docs, `/api/docs` for Scalar API reference
