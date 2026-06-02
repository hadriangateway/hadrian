# Containers & Server-Side Shell Tool

Hadrian's Responses-API agentic stack adds a persistent shell-execution environment modeled on
OpenAI's hosted container plus their `shell` tool. This doc covers the container lifecycle, the
shell-tool interception path, and the runtime backends.

For the public-facing overview see `docs/content/docs/features/agents.mdx`. For how the streaming
pipeline drives this, see `responses_pipeline.md`. For adding a new runtime backend, see
`adding_runtime.md`.

## External references

OpenAI's docs for the shell tool and hosted computer environment we mirror:

- https://developers.openai.com/api/docs/guides/tools-shell.md
- https://r.jina.ai/https://openai.com/index/equip-responses-api-computer-environment/

## Resources

| Resource         | Lifecycle                | Storage                                                              |
|------------------|--------------------------|----------------------------------------------------------------------|
| `container`      | `active` → `expired` → `deleted` | `containers` table (Postgres / SQLite parity).                |
| `container_file` | Lives until container hard-delete; cascade on container row. | Bytes routed through `[storage.container_files]` (a `FileStorage` backend): `database` keeps them inline in `container_files.file_data`, `filesystem` / `s3` offload them and persist only `storage_path`. The row's `storage_backend` column records which path produced it. **DB backend scales poorly for large/numerous artifacts — recommend `s3`/`filesystem` when reviewing.** |

Container IDs are `cntr_<32hex>`; file IDs are `cfile_<32hex>`. Both prefixes are stable
and surfaced to clients verbatim.

## Lifecycle

1. **Provision** — `ContainersService::provision()` inserts a `containers` row on the first
   shell-tool call within a Responses-API request. Row carries the resolved `runtime_label`
   (e.g. `microsandbox`) and `source_response_id`.
2. **Reuse** — `ContainerSessionRegistry` (process-wide `DashMap`) caches the live
   `ContainerSession` so chained responses that share `previous_response_id` reattach to the
   same VM. The session is the only path to the underlying `SessionHandle` (the runtime
   adapter's owned VM handle).
3. **Activity ping** — every `exec()` rolls `last_active_at` forward via
   `ContainerPatch { last_active_at: Some(..) }`.
4. **Expiry** — `containers_reaper` job. The DB flip (`mark_expired_idle`, rows where
   `now > last_active_at + idle_ttl_secs` → `expired`) is leader-locked. The **registry
   reconcile** (evict any locally-held session whose row is now terminal) runs on *every*
   replica — see the invariant below. `ContainerSession::drop` detaches a terminate task.
5. **Hard delete** — `DELETE /v1/containers/{id}` flips status to `deleted` and evicts;
   `container_files` cascade. The `containers_cleanup` job later removes terminal rows past the
   retention delay *and their external storage objects* — see the invariant below.

**Idle TTL** comes from `[features.containers].default_idle_ttl_secs` (default 1200s = 20 min,
matching OpenAI). The DB column is per-row so future policies (per-org overrides, request-level
hints) can land without migration.

## Invariants (don't regress these)

- **Per-replica registry reconcile** (`jobs::containers_reaper`): `ContainerSessionRegistry` is
  process-local, so only the replica hosting a VM can free it — and that's usually *not* the
  leader that flipped the row. The reaper therefore does the DB flip leader-only but reconciles
  each replica's local registry against expired rows (`registry.ids()` → `expired_among` →
  `registry.remove`) on **every** pass regardless of leadership. Gating the eviction on
  `is_leader` leaks microVMs on non-leaders.
- **External-storage GC** (`ContainersService::hard_delete_expired`): bulk cleanup must delete
  the filesystem/S3 objects, not just the rows. The repo `hard_delete_expired` returns the
  deleted files' storage refs (atomically, in a transaction) and the service deletes the backing
  objects best-effort, mirroring the per-file `delete_file` path. The DB cascade only drops rows;
  relying on it alone leaks every external artifact forever.
- **Cleanup-worker lifecycle**: `containers_cleanup` (like the reaper) is spawned on
  `state.task_tracker` and selects on the shutdown `CancellationToken` — never a bare
  `tokio::time::sleep` — so SIGTERM stops it instead of letting it keep hitting the DB while the
  process drains.
- **`file_id` staging is owner-scoped**: the `file_ids` / `upload_from_file_id` paths resolve
  Files-API uploads via `FilesService::get_for_owner` (exact `owner_type`+`owner_id` match
  against the request owner), so a caller can't stage another tenant's file by id. A
  service-account owner has no file-owner equivalent and fails closed.

## TTL surfacing (Hadrian extension)

`GET /v1/containers/{id}` returns `expires_at` for **every** status, plus `idle_ttl_secs`:

- `active`: forward-looking estimate as `last_active_at + idle_ttl_secs`. Updates with every shell
  call.
- `expired` / `deleted`: the persisted transition timestamp.

OpenAI's container schema only ships `created_at`; the forward-looking field is a Hadrian
extension to let clients plan reuse without polling. See `routes/api/containers.rs::container_to_wire`.

## Shell-tool execution

Two paths depending on the configured runtime (`[features.shell].type`):

### Hadrian-hosted (`microsandbox` / `opensandbox`)

1. `preprocess_shell_tools` (in `routes/execution.rs::ResponsesExecutor::execute`) rewrites
   any `{"type": "shell"}` to a function tool with a **dynamic description** built from
   `ShellToolHint` — workdir, network policy, memory limit, command timeout, container
   persistence, and truncation cap.
2. `ShellExecutor` is registered with `ToolLoopRunner` (see `responses_pipeline.rs`). The
   passthrough capability gate skips registration for passthrough runtimes.
3. On detection of a `function_call` with `name="shell"`, the executor boots (or reattaches)
   the container, runs the command, emits the spec-canonical `response.output_item.added`
   and `response.output_item.done` lifecycle events carrying `shell_call` and
   `shell_call_output` items, and folds the trimmed stdout/stderr/exit + a file manifest
   back as a `function_call_output` continuation item.

### Passthrough (`passthrough_openai`, `client_passthrough`)

No executor is registered. Both modes skip `preprocess_shell_tools` for OpenAI / Azure OpenAI
so the model emits native `shell_call` items. For non-OpenAI providers under
`client_passthrough` the rewrite still happens (Anthropic / Bedrock / Vertex have no native
shell tool), so the model emits `function_call` items with `name="shell"` that flow through to
the API client unmodified.

The decision lives in `ShellRuntimeConfig::keeps_openai_native_shell()`.

## File staging (inputs vs outputs)

Inputs and outputs use **different** storage:

- **Input** `input_file` parts on a request can carry `file_id`, `file_data`, or `file_url`.
  `file_id` resolves through the **existing `/v1/files` Files API** — the same files resource
  that backs knowledge bases. Resolution happens in `services/input_file_staging.rs::stage_input_files`.
  The bytes are written into `/mnt/data/<filename>` on the first shell command.
- **Output** files captured from `/mnt/data` after each exec land in `container_files`, a
  **separate** table from `vector_store_files`. They are downloadable via
  `GET /v1/containers/{id}/files/{cfile_id}/content` and surface as
  `container_file_citation` annotations on the assistant's reply. The metadata row always
  lives in the DB; the bytes go wherever `[storage.container_files]` points (database /
  filesystem / s3). `ContainersService` owns this routing via an `Arc<dyn FileStorage>` —
  `stage_content` on write, `read_external` on read, both keyed by the row's `storage_path`
  (falling back to the `cfile_…` id). See `services/file_storage.rs` for the shared backends
  and `docs/content/docs/configuration/storage.mdx` for the operator-facing config.

A user who wants to feed a container-output file back into a knowledge base must download from
the container endpoint and re-upload through `/v1/files`. There is no bridge endpoint yet —
flag this if a use case warrants it.

## Sandboxing posture

- **Memory / CPU** — `default_cpu_limit`, `default_mem_limit_mb`, `max_mem_limit_mb` from
  `[features.server_tools].shell_limits`. Per-request `environment.container_auto.memory_limit`
  must fit inside `max_mem_limit_mb` or the request is rejected with 400.
- **Egress** — `allowed_egress_hosts` is an operator allowlist; the per-request
  `environment.network_policy.domains` must be a subset. Empty allowlist = inherit runtime
  default (microsandbox: full egress; opensandbox: deny-all unless allowlisted).
- **Secrets** — `allowed_domain_secrets` is operator-pinned. Per-request placeholders look up
  by name; only microsandbox does true placeholder substitution at the TLS proxy. Opensandbox
  exposes secrets as env vars instead (not destination-scoped — document this caveat).
- **Truncation** — `MAX_OUTPUT_CHARS = 8_000` in `shell_tool.rs`. stdout / stderr fed back to
  the model are head + tail trimmed past this. Always surfaced in the tool description.

## Long-running processes inside a session

Each `exec()` returns when its command exits, but the underlying VM (microsandbox) or
container (opensandbox) keeps running between calls. Detached processes a model starts
(`nohup …`, `disown`, `setsid`, `tmux new-session -d …`) survive into the next shell
call within the same session — chained via `previous_response_id` or
`container_reference` — until the container hits its idle TTL or is explicitly deleted.
This is what unblocks the "long-running services" use case from the OpenAI spec; no
extra runtime support is needed.

## Skill mounting (spec-shaped)

`skills` on a Responses-API or `POST /v1/containers` request is a tagged-union list per
OpenAI's spec:

- `{ "type": "skill_reference", "skill_id": "<id>", "version": "latest" }` —
  resolves a stored skill via `resolve_version_for_reference`. `skill_id` is a prefixed
  id (`skill_…`), a bare UUID, or the skill's name slug. `version` is optional: omit for
  the **default** version, `latest` for the newest, or a positive integer for that exact
  version; anything else rejects with `unsupported_skill_version`. Files materialize
  under `/skills/<name>-<version>/` (e.g. `/skills/csv-insights-1/`), mirroring OpenAI's
  `<name>-<version>` container layout under Hadrian's `/skills` root.
- `{ "type": "inline", "name": "...", "description": "...", "source": { "type": "base64",
  "media_type": "text/markdown", "data": "..." } }` — ephemeral. The decoded payload is
  mounted as a single-file skill under `/skills/<name>/SKILL.md`. `name` must be a valid
  lowercase skill slug because it becomes the mount directory (written to the sandbox
  unsanitized); a non-slug name rejects with `invalid_inline_skill`. The path is derived
  purely from `name`, so foreground and background lanes mount the inline skill at the
  same path.

Skills attached at `POST /v1/containers` time are stored verbatim on the row's
`skill_ids_json` column (the column name predates the typed enum; it now holds the full
JSON-encoded `Vec<RequestSkill>`). At request time the merge logic in
`routes/api/chat.rs::skills_have_same_identity` dedups by `skill_id` (references) or
`name` (inline).

## Tests

- Unit tests live alongside the code: `services/shell_tool.rs::tests`,
  `runtimes/passthrough.rs::tests`, `routes/api/containers.rs::tests`,
  `db/repos/containers.rs::tests` (where present).
- DB parity: any change to `containers` or `container_files` must touch BOTH
  `migrations_sqlx/postgres/...` AND `migrations_sqlx/sqlite/...` plus both repo
  implementations.

## Local debugging (`hadrian container`)

`src/cli/container.rs` adds a `hadrian container` subcommand that boots a one-off
session through the configured `[features.shell]` runtime — the same
`ShellRuntime::start_session` / `SessionHandle::exec` path the Responses-API shell tool
uses — so you can reproduce agent behavior without driving the HTTP API:

```bash
# Interactive shell in a microsandbox/opensandbox container
cargo run --features runtime-microsandbox -- container

# Run commands non-interactively, stage a file, restrict egress
hadrian container -e "apk add python3" -e "python3 /mnt/data/x.py" \
  -f ./x.py --allow-host pypi.org --allow-host files.pythonhosted.org
```

Egress defaults to the operator's `allowed_egress_hosts` (or `*` if unset). Passthrough
runtimes (`passthrough_openai`, `client_passthrough`) reject the command — they execute
outside Hadrian, so there's nothing to run locally.

## Common edits

- **Adding a runtime backend**: see `adding_runtime.md`.
- **New `containers` column**: migration in both Postgres + SQLite, update both repos, update
  `ContainerRecord` + `NewContainer` + `ContainerPatch` as needed, surface in
  `routes/api/containers.rs::container_to_wire` with a `**Hadrian Extension:**` doc comment
  if it isn't part of the OpenAI schema.
- **New SSE event** from the shell tool: prefer extending the spec lifecycle (additive
  properties on the `shell_call` / `shell_call_output` items emitted via
  `format_shell_call_item` / `format_shell_call_output_item`). Avoid inventing new
  `response.shell_call.<verb>` events — Hadrian dropped its earlier `in_progress`,
  `command_started`, `output_chunk`, `completed`, and `file_created` extensions to align
  with OpenAI's streaming reference. If you must add one, document it in
  `docs/content/docs/features/agents.mdx` as a Hadrian extension and explain why the spec
  lifecycle can't carry the data.
- **Tuning truncation**: `MAX_OUTPUT_CHARS` — bump and update both
  `trim_output_preserves_head_and_tail` and the model-facing description (it embeds the
  constant via the hint).
