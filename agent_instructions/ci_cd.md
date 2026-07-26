# CI/CD Pipelines

## CI Pipeline

GitHub Actions workflow (`.github/workflows/ci.yml`) runs:
- Backend: format, clippy, build, test, security audit (`cargo audit`, `cargo-deny`)
- Frontend: lint, format, type check, build, security audit (`pnpm audit`)
- Cross-platform builds (Linux, macOS Intel/ARM, Windows)
- Docker build (shared image used by E2E tests)
- E2E tests (TypeScript/Playwright with testcontainers, needs Docker build)
- OpenAPI conformance check
- Documentation build
- WASM build (compile to `wasm32-unknown-unknown` via `wasm-pack`, build frontend with `VITE_WASM_MODE=true`)

### CI can break without code changes

Several jobs track live external state, so a red run on an untouched branch (or main) may be
environmental drift rather than a regression — compare against main's latest run first:

- The Rust toolchain is unpinned `stable`: a new Rust release ships new clippy lints that fail
  `-D warnings` feature checks.
- The OpenAPI conformance job downloads the *latest* OpenAI spec (`fetch-openapi-specs.sh`), so
  new OpenAI endpoints/fields fail conformance until documented in `DOCUMENTED_MISSING_FIELDS`
  (or, for beta `?beta=true` path keys, skipped as out of scope).
- `cargo audit` / `cargo-deny` / `pnpm audit` pull fresh advisory databases. Fix by updating the
  dependency when a patched version is in range; otherwise add a scoped override
  (`ui/pnpm-workspace.yaml`) or a documented ignore (`deny.toml`, `auditConfig.ignoreGhsas`).
  Note `cargo-deny` treats `unsound` advisories as errors while `cargo audit` only warns.

## Release Pipeline

GitHub Actions workflow (`.github/workflows/release.yml`) triggers on version tags (`v*`) or manual dispatch (with dry-run option):
- Builds frontend assets (UI, Storybook, docs) in a shared job
- Builds release binaries for each target/feature combination:
  - `x86_64-unknown-linux-gnu` (full, standard, minimal, tiny)
  - `x86_64-unknown-linux-musl` (standard, minimal, tiny)
  - `aarch64-unknown-linux-gnu` (standard, minimal, tiny)
  - `aarch64-apple-darwin` (full, standard, minimal, tiny)
  - `x86_64-pc-windows-msvc` (standard, minimal, tiny)
- Creates GitHub Release with archives and SHA256 checksums (tag push only)
- Dry-run mode builds artifacts and prints a summary without creating a release

## WASM Deploy

Workflow (`.github/workflows/deploy-wasm.yml`):
- Triggers on pushes to `main` touching `src/**`, `ui/**`, `Cargo.toml`, `Cargo.lock`, or `scripts/build-wasm.sh`
- Builds WASM module + frontend with `VITE_WASM_MODE=true`
- Deploys to Cloudflare Pages (app.hadriangateway.com)
- Sets `Service-Worker-Allowed: /` and `Cache-Control: no-cache` headers on `sw.js`

## Docs Deploy

Workflow (`.github/workflows/docs.yml`):
- Triggers on pushes to `main` touching `docs/**`, `openapi/hadrian.openapi.json`
- Build job: installs UI deps, builds Storybook, builds docs (output: `docs/out/`)
- Uploads both a GitHub Pages artifact and a generic artifact for reuse
- Deploys in parallel to:
  - **GitHub Pages** at hadriangateway.com (with CNAME file)
  - **Cloudflare Pages** at docs.hadriangateway.com (project: `hadrian-docs`)
- Uses same `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` secrets as WASM deploy

## Helm Chart

Workflow (`.github/workflows/helm.yml`) runs:
- `helm lint` (standard and strict mode)
- `helm template` with matrix of configurations (PostgreSQL, Redis, Ingress, etc.)
- Schema validation with `ajv-cli`
- Integration tests in ephemeral kind cluster
