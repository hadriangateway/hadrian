# Adding a Shell Runtime Backend

A "runtime" is the backend that executes the model's `shell` tool calls. Today: `passthrough_openai`,
`client_passthrough`, `microsandbox`, `opensandbox`. This doc walks through adding another
(e.g. E2B, Daytona, a local Docker exec, AgentCore Code Interpreter).

Read `containers.md` first for the lifecycle and `responses_pipeline.md` for how runtimes plug
into the streaming pipeline.

## The trait

`src/runtimes/mod.rs` defines `ShellRuntime`:

```rust
#[async_trait]
pub trait ShellRuntime: Send + Sync {
    fn capabilities(&self) -> RuntimeCapabilities;
    async fn start_session(&self, spec: SessionSpec) -> RuntimeResult<SessionHandle>;
    async fn health_check(&self) -> RuntimeResult<()> { Ok(()) }
}
```

`SessionHandle` wraps a `Box<dyn ShellSession>` which exposes `exec`, `write_file`,
`read_file`, `terminate`.

## Steps

1. **Add a feature flag** in `Cargo.toml` (`runtime-<name>`) — match the existing pattern of
   gating the dep tree per backend so `tiny` / `minimal` profiles don't pull it in.
2. **Add config struct** in `src/config/runtimes.rs`: a `<Name>Config` with `serde(deny_unknown_fields)`
   plus a new variant `ShellRuntimeConfig::<Name>(<Name>Config)`. Update `name()`,
   `keeps_openai_native_shell()` (almost certainly returns `false` for a real sandbox), and
   `ShellRuntimeConfig` doc examples.
3. **Add the runtime module** at `src/runtimes/<name>.rs` behind `#[cfg(feature = "runtime-<name>")]`.
   Implement `ShellRuntime` + `ShellSession`. Honor `SessionSpec` (`mem_limit_bytes`, `cpu_limit`,
   `egress_policy`, `mounted_skills`) — surface anything you can't honor as
   `RuntimeCapabilities::<flag> = false` so the orchestrator can fail-fast instead of silently
   degrading.
4. **Re-export** in `src/runtimes/mod.rs` under matching `#[cfg]` and the module declaration.
5. **Construct in `src/app.rs`** alongside the other runtimes. Add an `info!` log line that
   names the backend so operators see the choice on boot.
6. **Update `src/services/responses_pipeline.rs`** match (the one that resolves pricing /
   label per runtime). It's behind a `passthrough_only` guard so the new arm should usually be
   `(price_per_second, "<name>")`.
7. **Pricing**: add a field on `[features.server_tools.pricing]` for the per-second microcents
   rate so the usage tracker can attribute cost. See `microsandbox_microcents_per_second`.
8. **Tests** — at minimum:
   - `capabilities()` returns the values you intend (test in the same file).
   - A round-trip integration test if the backend has a stub mode; otherwise mark
     `#[ignore]` and exercise from `deploy/tests` (see `testing.md`).

## Capability checklist

Each `RuntimeCapabilities` field controls a real behavior; lying breaks something:

| Field                       | If false                                                                                  |
|-----------------------------|-------------------------------------------------------------------------------------------|
| `passthrough_only`          | Orchestrator registers `ShellExecutor` and calls `start_session`. Pretty much always false. |
| `client_executes`           | Only meaningful with `passthrough_only=true`. Drives whether OpenAI native `shell` is kept. |
| `secret_injection`          | Requests with `domain_secrets` are rejected with a capability-mismatch error.             |
| `egress_allowlist`          | Requests with `network_policy.domains` are rejected.                                      |
| `skill_mount`               | `payload.skills` are silently dropped (warn-log only) for this runtime.                   |
| `file_io`                   | `input_file` staging into `/mnt/data` and file capture both fail open with a warning.     |
| `network_isolation_modes`   | Per-request `network_policy` modes outside this list are rejected.                        |
| `max_session_duration`      | Sessions older than this are terminated by the reaper regardless of activity.             |

## Passthrough vs. hosted

The trait was designed to model both. If your backend is really a passthrough (the call
happens elsewhere), implement it like `src/runtimes/passthrough.rs`: `start_session` returns
`Err(RuntimeError::Passthrough)`, capabilities advertise `passthrough_only: true`, and the
chat.rs preprocess decision is updated to keep the native `shell` spec for the right
upstream providers (currently OpenAI / Azure).
