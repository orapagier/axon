# Axon Improvement Plan — Out-engineering n8n

> Scope: the **workflow engine and platform**, not nodes/integrations (those come later).
> Goal: close the handful of reliability/data gaps where n8n is visibly ahead, then win
> decisively on the AI-native + single-binary + concurrent-execution moat that n8n
> structurally cannot match.
>
> Explicitly **out of scope** (deliberate product decision): multi-user, RBAC, projects,
> credential sharing, SSO, GitOps environments. Axon stays single-operator.

---

## 0. Guiding principles

1. **Single binary stays the moat.** No new mandatory external services (no Redis, no
   Postgres requirement). Every feature below works against the existing SQLite + Qdrant
   stack. Scale is vertical + async, not a worker fleet.
2. **n8n parity where users expect it, n8n-beating where it's cheap.** Match the mental
   model (retry, error workflow, sub-workflow, wait-for-webhook) so migrators feel at home;
   exceed it on concurrency, AI nodes, and self-healing.
3. **Engine changes are additive and migration-guarded.** Every schema change is an
   idempotent migration (`0007+`), every new `WorkflowNode`/`Workflow` field is
   `#[serde(default)]` so old rows and old exported JSON keep loading.
4. **Editor and engine must agree.** Any new execution semantic (pinned data, retry,
   branches) gets mirrored in the canvas preview so "test run" matches "production run".

### Key existing symbols (touch points)

| Concern | Symbol / file |
|---|---|
| Whole-run executor | `WorkflowEngine::run_with_trigger` — `crates/axon-agent/src/tools/workflow.rs:1910` |
| Per-node dispatch | `execute_node_by_type` — `workflow.rs:1702` |
| Background spawn | `run_in_background_with_source` — `workflow.rs:2933` |
| Single-node run | `run_node_in_background` — `workflow.rs:2947` |
| Config interpolation + credential inject | `interpolate_config` — `workflow.rs:1274` |
| Durable wait | `tools/workflow/nodes/wait.rs` |
| Agent-triggered run (reuse for sub-workflow) | `handle_run_workflow` — `agent/internal_tools.rs:392` |
| Workflow CRUD API | `upsert_workflow`, `run_workflow`, `get_workflow_runs` — `dashboard/api.rs` |
| Routes | `dashboard/server.rs:166`+ |
| Schema | `db/migrations/0001..0006`, latest is `0006`; **next is `0007`** |
| Secret crypto | `crypto.rs` |

---

## 1. Milestones (build order)

Ordered by leverage ÷ risk. Each milestone is independently shippable.

### Milestone A — "Trustworthy engine" (highest leverage, lowest risk)
- A1. Per-node **retry on fail**
- A2. **Sub-workflow** ("Execute Workflow") node
- A3. **Error workflow** / error trigger
- A4. Finish **pinned data** (engine honors it)
- A5. Workflow **export / import** (JSON)

### Milestone B — "Production data model"
- B1. **Workflow versioning / history** (single-operator undo + restore)
- B2. **Binary / large-payload** offloading
- B3. **Execution concurrency** control + bounded queue

### Milestone C — "Ops & resume"
- C1. **Wait-for-webhook** + **Approval** node (human-in-the-loop)
- C2. **Trigger idempotency / dedup**
- C3. **Observability**: `/metrics` + structured run events

### Milestone D — "Polish that closes the feel gap"
- D1. **Credential hardening** (KDF, no plaintext fallback, credential test)
- D2. **Expression helper library** ($now / $jmespath / item helpers)

---

## 2. Milestone A — Trustworthy engine

### A1. Per-node retry on fail

**Problem.** Nodes have only `continue_on_fail` (`WorkflowNode`, `workflow.rs:71`). A flaky
HTTP call either fails the run or is swallowed — there's no "try 3×, wait 2s between". n8n
has `retryOnFail` + `maxTries` (1–5) + `waitBetweenTries`.

**Target.** Any node can retry N times with a fixed (and optionally exponential) wait, before
either failing the run or, if `continue_on_fail`, passing through the error.

**Approach.**

1. Migration `0007_node_reliability.sql`:
   ```sql
   ALTER TABLE workflow_nodes ADD COLUMN retries        INTEGER DEFAULT 0;
   ALTER TABLE workflow_nodes ADD COLUMN retry_wait_ms  INTEGER DEFAULT 0;
   ALTER TABLE workflow_nodes ADD COLUMN retry_backoff  TEXT    DEFAULT 'fixed'; -- 'fixed'|'exponential'
   ```
2. Extend `WorkflowNode` (`workflow.rs:57`):
   ```rust
   #[serde(default)] pub retries: u32,
   #[serde(default)] pub retry_wait_ms: u64,
   #[serde(default)] pub retry_backoff: String, // "" => "fixed"
   ```
3. Wrap the single-execution call site. The cleanest seam is the **non-iterating** branch and
   each **iteration unit** inside `run_with_trigger` that calls `execute_node_by_type`
   (`workflow.rs:1702`, called at ~`:2428` and `:2462`). Introduce a helper:
   ```rust
   async fn execute_with_retry(node, config, state, …) -> Result<Value, String> {
       let max = node.retries.max(0);
       let mut attempt = 0;
       loop {
           match execute_node_by_type(node, config, …).await {
               Ok(v) => return Ok(v),
               Err(e) if attempt < max => {
                   attempt += 1;
                   let base = node.retry_wait_ms.max(1);
                   let wait = if node.retry_backoff == "exponential" {
                       base * 2u64.pow(attempt - 1)
                   } else { base };
                   // cancellation-aware sleep (reuse the sliced sleep from wait.rs)
                   sleep_cancellable(wait, state, workflow_id, run_id).await?;
                   continue;
               }
               Err(e) => return Err(e),
           }
       }
   }
   ```
   Route **all three** call sites (sequential, parallel `buffered`, and single-node) through it.
   Triggers/`loop`/`wait` should default to `retries = 0` (don't retry a durable suspend).
4. Record attempts on `NodeResult` for the UI: add `#[serde(default)] pub attempts: u32`.
5. UI: in `NodeDetails.vue`, a "Settings" sub-panel — `retries`, `retry_wait_ms`,
   `retry_backoff` (mirrors the existing `continue_on_fail` toggle).

**Edge cases.** Retry must be cancellation-aware (poll `state.workflow_cancellations` during
the wait, like `wait.rs:111`). A retried node must not double-emit incremental `node_results`.
Inside a parallel loop, retry is per-unit; the unit's `attempts` is reported in
`iteration_errors`.

**Test.** `tests.rs`: a node whose executor fails twice then succeeds with `retries=2`
yields success and `attempts=3`; with `retries=1` yields error. Verify exponential timing
with a fake clock or a tolerance assert.

**Effort:** ~0.5 day.

---

### A2. Sub-workflow ("Execute Workflow") node

**Problem.** There is **no** node that runs another workflow (the dispatch at
`workflow.rs:1714` has no such arm). Large automations can't be decomposed — the single
biggest maintainability gap vs n8n.

**Target.** A `subflow` node that runs another workflow by id, passes the current items as
input, waits for completion, and returns its `final_output`/`items` so downstream nodes
consume them. Reuses the proven `WorkflowEngine` path already used by
`handle_run_workflow` (`internal_tools.rs:392`).

**Approach.**

1. New node type `subflow` (alias `workflow`). Config:
   ```json
   { "workflow_id": "...", "input_mode": "items|json|none",
     "wait_for_completion": true, "input": { /* optional explicit payload */ } }
   ```
2. New module `tools/workflow/nodes/subflow.rs`. Dispatch arm in `execute_node_by_type`:
   ```rust
   "subflow" | "workflow" => nodes::subflow::execute(config, state, workflow_id, run_id, depth).await,
   ```
3. Executor:
   - Resolve `workflow_id`, load the target workflow (same loader `run_with_trigger` uses).
   - Build a trigger payload from the caller's items (the interpolated `config.input` or the
     upstream node's `items`). Inject it the same way other triggers do (a dedicated
     `SUBFLOW_TRIGGER_DATA` static keyed by a fresh run id, mirroring
     `EXTERNAL_TRIGGER_DATA`, `workflow.rs:28`).
   - Call `WorkflowEngine::run_with_trigger(target, trigger_source="subflow", …)`.
   - Return `{ "items": <final items>, "run_id": <child>, "status": … }`.
4. **Recursion guard (critical).** Thread a `depth: u32` parameter through
   `run_with_trigger` → `execute_node_by_type` → subflow executor. Refuse when
   `depth >= MAX_SUBFLOW_DEPTH` (e.g. 8) and detect direct cycles by tracking the set of
   ancestor workflow ids in a task-local. Without this, A↔B workflows hang the process.
5. Child run is a **real** `workflow_runs` row (so history/observability sees it) but tagged
   `parent_run_id` (add nullable column in the same `0007` migration). The UI run view can
   render "▶ opened sub-run".
6. UI: node in `lib/nodes.js` with a workflow picker (populate from `/api/workflows`),
   `input_mode` select, and a "wait for completion" toggle.

**Edge cases.** Disabled/empty target → error (respect `continue_on_fail`). A subflow that
itself durably suspends (Wait) — for v1, a suspended child returns `status:"waiting"` and the
parent treats it as success-with-pending; full parent/child suspend chaining is a later
enhancement (note it explicitly). Cancellation of the parent must propagate to the child run
id (add child id to `workflow_cancellations`).

**Test.** Parent passes `[{n:1},{n:2}]`, child doubles each, parent receives `[{n:2},{n:4}]`.
Cycle A→B→A errors at depth guard. Child run row exists with `parent_run_id`.

**Effort:** ~1 day. **Highest single-feature leverage in the plan.**

---

### A3. Error workflow / error trigger

**Problem.** On failure you only fire `send_global_error_notification`
(`error_reporting.rs`). You can't run a *workflow* in response — n8n's Error Trigger pattern
(notify Slack, open a ticket, compensate) is one of its most-used features.

**Target.** A workflow can fail over to a designated **error workflow**, which receives a
structured payload describing what failed.

**Approach.**

1. Migration `0007`: `ALTER TABLE workflows ADD COLUMN error_workflow_id TEXT;`
   Plus a global default setting key `workflow.default_error_workflow_id` in `seed.sql`.
2. New trigger type `error` (`stimulus` subtype). A workflow whose entry trigger is `error`
   is eligible as an error handler and is excluded from manual "run all triggers".
3. In `run_with_trigger`, on terminal failure (the path that sets run status `error`), after
   persisting the failed run, resolve `error_workflow_id` (node-level → workflow-level →
   global default). If present and **not self** (prevent error-loop), fire it via
   `run_in_background_with_source` with an injected payload:
   ```json
   { "workflow": { "id", "name" },
     "run_id": "...", "failed_node": { "id","name","type" },
     "error": "...", "trigger_type": "...", "ts": "..." }
   ```
   Inject through a new `ERROR_TRIGGER_DATA` static (mirror the existing trigger statics).
4. **Loop-guard:** never run an error workflow as a result of an error workflow failing
   (carry a `is_error_run` flag in the trigger source / a task-local), and never let a
   workflow be its own error handler.
5. UI: workflow settings panel gets an "On failure, run workflow…" picker; a starter
   "Error Trigger" node template.

**Edge cases.** Error workflow itself errors → log + notify only (no recursion). The failure
payload must be size-bounded (truncate large node errors).

**Test.** Failing workflow with `error_workflow_id` set spawns exactly one child run whose
trigger payload contains the failing node id and error string; self-reference is refused.

**Effort:** ~0.5 day (rides on A2's trigger-injection plumbing).

---

### A4. Finish pinned data (make the engine honor it)

**Problem / current state.** Half-built:
- `loadHistoryToEditor` (`WorkflowsPage.vue:1579`) loads a *past run* into `lastRunResult`
  for previewing/expression-building — useful, keep it.
- A per-node `pinnedData` field + "Pinned Data" status icon exist
  (`useCanvasNode.js:59`, `CanvasNodeStatusIcons.vue:60`) **but nothing writes them and the
  engine never reads them.** n8n's defining behavior — a pinned node returns its pinned
  output *instead of executing* during manual runs — is missing.

**Target.** Per-node pinned output that, on **editor/manual runs only**, is used as that
node's result without executing it (so downstream nodes see deterministic data and external
side-effects don't fire while building). Production/trigger runs ignore pins.

**Approach.**

1. Migration `0007`: `ALTER TABLE workflow_nodes ADD COLUMN pinned_data TEXT;`
   (`NULL` = not pinned; JSON value = the pinned output object.)
2. `WorkflowNode`: `#[serde(default)] pub pinned_data: Option<Value>`.
3. In `run_with_trigger`, when `trigger_source` indicates a manual/editor run
   (the `None`/`"manual"` path — *not* telegram/gmail/webhook/subflow/scheduled) and
   `node.pinned_data.is_some()`:
   - Skip execution, synthesize a `NodeResult { status:"success", output: pinned, … }`,
     mark it `pinned: true`, route edges normally.
   - This slots in right beside the existing disabled-node short-circuit (`workflow.rs:2241`).
4. Writer: endpoint `POST /api/workflows/:id/nodes/:node_id/pin` (body = a node result or the
   literal value) and `DELETE …/pin` to clear. From the UI: "Pin output" on a node that has
   run data; "Unpin" when pinned. The existing status icon then lights up correctly because
   `pinnedData` is finally populated.
5. Guard rails: pinning is editor convenience — surface a clear badge ("Pinned — using saved
   data, not executing") and never let a scheduled/production run silently use pins.

**Edge cases.** A pinned trigger node feeds a manual run deterministically (great for testing
downstream). Pin size cap (e.g. 256 KB) — larger should be refused with a hint to use a real
run. Export (A5) includes pins optionally.

**Test.** Manual run of a workflow with a pinned middle node does not call that node's
executor (assert via a side-effect counter) yet downstream receives the pinned value; a
`telegram`-sourced run of the same workflow *does* execute it.

**Effort:** ~0.5 day.

---

### A5. Workflow export / import (JSON)

**Problem.** No export/import endpoint exists. Backups, sharing, version-pinning, and any
"templates" story are impossible; everything lives only in SQLite.

**Target.** One self-contained JSON document per workflow: metadata + nodes + edges (+ pins
optionally). Importable into the same or another Axon, with fresh ids and a credential
**reference remap** (never export secret material).

**Approach.**

1. Define a stable bundle schema `AxonWorkflowBundle`:
   ```json
   { "axon_format": 1, "exported_at": "...",
     "workflow": { name, description, trigger_type, trigger_config, error_workflow_ref },
     "nodes": [ { local_id, node_type, name, position_x, position_y, config,
                  enabled, continue_on_fail, retries, retry_wait_ms, pinned_data? } ],
     "edges": [ { source_local_id, target_local_id, source_handle, target_handle } ],
     "credentials_required": [ { ref, service, name } ] }
   ```
   `config.credential_id` values are replaced by a symbolic `ref`; on import the user maps
   each `ref` to an existing credential (or leaves blank). **Secrets never leave the box.**
2. `GET /api/workflows/:id/export` → bundle (download). `POST /api/workflows/import` → bundle,
   returns the new workflow id. Implement id remapping (local_id → fresh UUIDs, rewrite edges
   and any `$node["Name"]` references that are name-based — those survive since they're by
   name).
3. Add routes in `dashboard/server.rs` next to the existing workflow routes (`:166`).
4. UI: "Export" / "Import" buttons on `WorkflowsPage.vue`; import shows the
   `credentials_required` mapping step.

**Edge cases.** Version skew: gate on `axon_format`; unknown fields ignored (forward-compat
via `#[serde(default)]`). Importing a bundle that references an error workflow by name →
resolve if present, else leave unset with a warning.

**Test.** Round-trip: export → import → exported-again is structurally equal (ids aside).
Import with an unknown node_type loads but flags the node invalid rather than 500-ing.

**Effort:** ~1 day.

---

## 3. Milestone B — Production data model

### B1. Workflow versioning / history

**Problem.** `workflows` has no version/`updated_at` and no snapshots
(`0002_agent_tables.sql:38`). A bad edit is unrecoverable. (This is single-operator undo, not
team collaboration — in scope.)

**Target.** Every save snapshots the prior state; operator can list, diff, and restore
versions.

**Approach.**

1. Migration `0008_workflow_versions.sql`:
   ```sql
   CREATE TABLE IF NOT EXISTS workflow_versions (
     id          TEXT PRIMARY KEY,
     workflow_id TEXT NOT NULL,
     version     INTEGER NOT NULL,
     label       TEXT,
     snapshot    TEXT NOT NULL,      -- full AxonWorkflowBundle (reuse A5 schema)
     created_at  TEXT NOT NULL DEFAULT (datetime('now'))
   );
   CREATE INDEX IF NOT EXISTS idx_wv_workflow ON workflow_versions(workflow_id, version DESC);
   ```
2. In `upsert_workflow`, **before** writing changes, serialize the current persisted state
   into a version row (skip if unchanged — compare a content hash to avoid noise on no-op
   autosaves; the editor autosaves frequently, so dedupe is essential).
3. Reuse the A5 bundle serializer for `snapshot` — one format for export and history.
4. Retention: cap versions per workflow (e.g. keep last 50 + any labeled), pruned by
   `maintenance.rs` alongside the existing run retention (`run_retention`).
5. API: `GET /api/workflows/:id/versions`, `GET …/versions/:v` (preview/diff),
   `POST …/versions/:v/restore` (which itself snapshots current first).
6. UI: a "History" drawer (you already have a history panel pattern in `WorkflowsPage.vue`),
   with restore + a simple node/edge/config diff.

**Edge cases.** Autosave storms → content-hash dedupe + min-interval (e.g. ≤1 version/30s).
Restore must re-version, never destroy the current state silently.

**Effort:** ~1 day.

---

### B2. Binary / large-payload offloading

**Problem.** A whole run's `node_results` is one `TEXT` JSON blob in `workflow_runs`
(`0002:83`). Base64 images, file bytes, or big API responses bloat SQLite, slow the
incremental `UPDATE workflow_runs SET node_results=…` (`workflow.rs:2261`), and balloon
history. There's no binary handling like n8n's filesystem/S3 mode.

**Target.** Large/binary outputs are stored as files on disk and referenced by a small
descriptor inside `node_results`; the engine, UI, and downstream nodes pass references; bytes
are loaded only when a node actually needs them.

**Approach.**

1. Reuse the existing file machinery (`tools/file_handler.rs`, `files/` dir,
   `axon-agent/files`). Define a descriptor:
   ```json
   { "_axon_binary": { "id": "...", "mime": "...", "size": 12345,
                       "filename": "report.pdf", "path": "files/<id>" } }
   ```
2. After a node returns, if its output (or a field) exceeds a threshold (e.g. 64 KB) or is
   tagged binary, the engine offloads bytes to `files/<id>` and replaces the value with the
   descriptor before persisting `node_results`. A small helper `offload_large_values(output)`
   walks the JSON.
3. Interpolation/consumers: when a node config references a binary field, `interpolate_config`
   (or the node executor) rehydrates by reading `path`. Add a `binary` reference resolver
   beside the existing `data/output/json` accessors (`workflow.rs:1193`).
4. Retention: when a `workflow_runs` row is pruned (`maintenance.rs`), delete its orphaned
   `files/<id>` blobs. Add a sweep that GCs unreferenced blobs.
5. Settings: `workflow.binary_inline_max_bytes`, `workflow.binary_store_dir`.

**Edge cases.** Pinned data (A4) and exports (A5) must store *references*, not the bytes
(export can optionally bundle small binaries, but never giant ones). Concurrent loop units
writing many blobs — id must be content/uuid based to avoid collisions.

**Effort:** ~1.5 days. Do **after** A5/B1 so the bundle/snapshot format already exists to
hold references.

---

### B3. Execution concurrency control + bounded queue

**Problem.** Triggers spawn runs via `run_in_background_with_source` with no global cap. A
burst (many Telegram messages, a webhook storm, a fan-out of subflows) can spawn unbounded
tokio tasks and exhaust memory/CPU. n8n has concurrency control; Axon currently has none.

**Target.** A global limit on concurrently *executing* workflow runs, with a bounded wait
queue and a clear overflow policy — all in-process, no Redis.

**Approach.**

1. Add to `AppState`: `run_semaphore: Arc<tokio::sync::Semaphore>` sized from setting
   `workflow.max_concurrent_runs` (default e.g. 16), plus an atomic `queue_depth` gauge.
2. `run_in_background_with_source` acquires a permit before executing; if none is immediately
   available it either (a) queues (await the permit) up to a max queue length, or (b) for
   "fire from a trigger" rejects with a logged backpressure event when the queue is full
   (policy per trigger type — interactive chat should queue, high-volume webhooks may shed).
3. Subflows (A2) acquire from the **same** semaphore but with a reserved minimum to avoid
   deadlock (a parent holding a permit while waiting on a child that can't get one). Simplest
   safe rule: subflow runs execute inline within the parent's permit (don't take a second
   permit) — they're already bounded by `MAX_SUBFLOW_DEPTH`.
4. Expose `queue_depth` and `active_runs` for C3's `/metrics`.

**Edge cases.** Durable Wait suspends should **release** the permit while suspended (they're
not consuming CPU) and re-acquire on resume — otherwise long waits starve the pool. Tie this
into the suspend path in `wait.rs`/the engine's `__axon_wait_suspend` handling.

**Effort:** ~1 day. Subtle (deadlock-prone) — write the deadlock test first.

---

## 4. Milestone C — Ops & resume

### C1. Wait-for-webhook + Approval node (human-in-the-loop)

**Problem.** `Wait` is time-only (`wait.rs`: `interval`/`until`). n8n can suspend a run until
an external webhook/form resumes it — the backbone of approvals ("approve this refund"),
callbacks, and async external jobs. Axon can't.

**Target.** A Wait node can suspend durably until a tokenized resume URL is hit; an Approval
node is a thin wrapper that suspends and exposes Approve/Reject URLs feeding two branch
handles.

**Approach.**

1. Reuse the durable-suspend machinery already in place (`SUSPEND_MARKER`, `wait.rs:91`,
   `0005_durable_wait.sql`). Add `mode: "webhook" | "approval"`.
2. Migration `0009_resume_tokens.sql`:
   ```sql
   CREATE TABLE IF NOT EXISTS workflow_resume_tokens (
     token       TEXT PRIMARY KEY,
     run_id      TEXT NOT NULL,
     workflow_id TEXT NOT NULL,
     node_id     TEXT NOT NULL,
     created_at  TEXT NOT NULL DEFAULT (datetime('now')),
     expires_at  TEXT
   );
   ```
3. On suspend in `webhook`/`approval` mode, mint a token, store it, and surface the resume URL
   in the node output (`resume_url`, `approve_url`, `reject_url`).
4. New public endpoint `POST /webhook/resume/:token` (and `/approve`,`/reject`) in the webhook
   layer (`webhook/external.rs`). On hit: validate + consume token, attach the POST body as
   the node's resume payload, and wake the run via the existing resume path (the same one that
   re-enters `run_with_trigger` for durable waits). Approve/Reject set which **branch handle**
   continues (`source_handle` already exists on edges — `workflow.rs:80`).
5. UI: Approval node with two output handles; surfaces the live approve/reject links during a
   run; optional message template.

**Edge cases.** Token expiry → resume to a timeout branch (or fail) rather than hang forever.
Double-submit → idempotent (token consumed once). A resume for an already-finished/cancelled
run is a no-op with a 410. Secure the token (unguessable, single-use); the resume endpoint is
unauthenticated by necessity, so the token *is* the credential.

**Effort:** ~1.5 days. Build on B3 (permit release on suspend) so a fleet of pending approvals
doesn't hold permits.

---

### C2. Trigger idempotency / dedup

**Problem.** Polling triggers (Gmail `check_and_trigger_gmail` `workflow.rs:3291`, webhooks)
can reprocess the same event after a restart or an overlapping poll, double-firing workflows.

**Target.** Each external event is processed at most once.

**Approach.**

1. Migration `0009` (same as C1): 
   ```sql
   CREATE TABLE IF NOT EXISTS trigger_dedup (
     source     TEXT NOT NULL,   -- 'gmail'|'webhook'|'github'|…
     event_key  TEXT NOT NULL,   -- message id / delivery id / hash
     seen_at    TEXT NOT NULL DEFAULT (datetime('now')),
     PRIMARY KEY (source, event_key)
   );
   ```
2. Before firing, `INSERT OR IGNORE` the `(source, event_key)`; if `changes() == 0`, skip.
   Gmail key = message id; GitHub = `X-GitHub-Delivery`; generic webhook = hash of body +
   timestamp window.
3. Prune via `maintenance.rs` retention (keep N days).

**Edge cases.** Don't dedupe interactive chat (every Telegram message is intentionally
distinct) — scope dedup to event-sourced triggers only.

**Effort:** ~0.5 day.

---

### C3. Observability — `/metrics` + structured run events

**Problem.** You have `tracing` but no metrics surface. Operating Axon at volume means
flying blind on run rate, failure rate, latency, queue depth.

**Target.** A Prometheus endpoint and a few high-value structured events. No external
collector required to read it.

**Approach.**

1. Add the `metrics` + `metrics-exporter-prometheus` crates; register a recorder at startup.
2. Instrument the engine:
   - `axon_workflow_runs_total{status}` (counter, on run completion)
   - `axon_workflow_run_duration_seconds` (histogram, from `start.elapsed()` already computed)
   - `axon_node_exec_duration_seconds{node_type}` (histogram, from `NodeResult.duration_ms`)
   - `axon_active_runs` / `axon_run_queue_depth` (gauges, from B3)
   - `axon_node_retries_total` (from A1)
3. Route `GET /metrics` in `dashboard/server.rs` (gate behind the existing bearer auth or a
   separate metrics token).
4. Optional: a compact JSON `GET /api/health` with the same numbers for the dashboard's own
   status widget.

**Effort:** ~0.5 day.

---

## 5. Milestone D — Polish that closes the "feel" gap

### D1. Credential hardening

**Problem (`crypto.rs`).** Three real weaknesses:
- Insecure **default key** when `AXON_MASTER_KEY` unset (`crypto.rs:9`) — silently runs in dev
  mode in prod if the env is missing.
- Key derived by **truncate/pad to 32 bytes** (`:14`) instead of a KDF — short/long keys are
  weak or silently clipped.
- **Plaintext fallback** on decrypt failure (`:47`,`:57`) — a wrong key returns the ciphertext
  string as if it were the secret, which can leak/break silently.

**Target.** Proper key derivation, fail-closed behavior, and a credential "test" action — all
without changing the single-binary model.

**Approach.**

1. Derive the AES key with SHA-256 (or Argon2id for a passphrase) over `AXON_MASTER_KEY` →
   always 32 bytes, any input length. (Re-encrypt path: see migration note below.)
2. **Refuse to boot in production** with the default/empty key (allow it only when an explicit
   `AXON_DEV=1` is set). Loud, fail-closed.
3. Remove the plaintext fallback: on decrypt failure, return an explicit `Err`, never the
   ciphertext. Callers surface "credential needs re-entry (master key changed)".
4. **Migration concern:** changing the KDF invalidates existing ciphertexts. Ship a one-shot
   re-encrypt: on boot, detect old-scheme values (they currently decrypt with the
   truncate/pad key) and re-encrypt under the new KDF. Keep the old `get_key` as
   `get_key_legacy` purely for this migration pass, then drop after a release.
5. Credential **test**: `POST /api/credentials/:id/test` that does a service-specific cheap
   call (e.g. token introspection) and reports validity — closes a real n8n-parity gap and
   prevents "why did my workflow fail" credential mysteries.

**Edge cases.** Migration must be idempotent and reversible-safe (don't double-encrypt). Log
counts, never log secret values.

**Effort:** ~1 day.

---

### D2. Expression helper library

**Problem.** Expressions resolve via `resolve_value_scoped` + the Boa JS node
(10s/64 KB caps), and the condition engine already mirrors n8n operators
(`workflow.rs:1363`). But the day-to-day helper surface (n8n's Luxon `$now`/`$today`,
`$jmespath`, `$items`, string/date helpers) is thinner, which is most of why n8n "feels
nicer" to build in.

**Target.** A small, documented set of built-in expression helpers available both in `{{ }}`
interpolation and inside the Boa context.

**Approach.**

1. In the Boa setup for the `javascript` node (`execute_js_node`, `workflow.rs:191`), register
   globals: `$now` (ISO + helpers via `chrono`), `$today`, `$json` (current item),
   `$items(nodeName)`, `$node`, `$workflow`, `$env` (whitelisted), and a `$jmespath(obj, expr)`
   backed by the `jmespath` crate.
2. In the lightweight `{{ }}` resolver, support the same names so non-JS fields get parity for
   the common cases (`{{$now}}`, `{{$json.foo}}`, `{{$jmespath(...)}}`).
3. String/date helper shims (`.toUpperCase`, date formatting) — lean on Boa's JS stdlib where
   possible; only add Rust-backed helpers for what JS lacks cheaply (jmespath, timezone-aware
   now).
4. Document the full helper set in `USER_GUIDE.md` with a side-by-side "n8n → Axon" cheat
   sheet to ease migration.

**Edge cases.** `$env` must be whitelisted (never expose `AXON_MASTER_KEY` etc.). Keep the
10s/64 KB Boa guards. JMESPath errors return `null` with a log line, not a run failure.

**Effort:** ~1 day.

---

## 6. What we are deliberately NOT doing (and why)

| Skipped | Reason |
|---|---|
| Multi-user, RBAC, projects, credential sharing | Product decision: Axon is single-operator. |
| SSO / SAML / audit-for-compliance | Follows from no multi-user. |
| GitOps environments (dev→prod promotion) | Team workflow; export/import (A5) + versioning (B1) cover the solo backup/restore need. |
| Redis/Bull queue mode, worker fleet | Breaks the single-binary moat. B3 (in-process concurrency control) is the right-sized answer for one operator. |
| Mass node/integration expansion | Explicitly deferred by you; the engine work here makes new nodes cheaper to add later. |

---

## 7. Where this leaves Axon vs n8n

After Milestones A–C, every reliability/data behavior an n8n migrator reaches for —
retry, sub-workflows, error workflows, pinned data, wait-for-webhook/approval, export/import,
versioning, binary handling, concurrency control, metrics — exists. At that point Axon is not
"catching up"; it's **ahead on the axes n8n can't easily move**:

- **AI-native execution** (model-router failover, `cortex`/`classifier`, vector memory,
  self-written tools) — and now sub-workflows let an agent node call workflows as tools.
- **Concurrent loop fan-out** — already beats n8n's single-threaded item loop; B3 makes it
  safe under load.
- **One binary, no Redis/Postgres** — preserved by every choice above.
- **Self-healing**: A1 (retry) + A3 (error workflow) + the existing `nociceptor`/`tool_writer`
  give a credible "workflow repairs itself" story n8n has no equivalent for.

### Suggested first PR
A1 (retry) + A2 (sub-workflow) share the same `0007` migration and the same
`execute_node_by_type` seam — ship them together. Highest leverage, lowest risk, and they
unlock A3 immediately.
