# Axon — Production Readiness Remediation Plan

> Follows from the production-readiness audit (2026-07-08). Findings and severities are summarized inline; this doc is the execution plan, not the audit itself.

## Context

The audit found Axon well-engineered in the subsystems the team clearly focused on (credential encryption, SQLite concurrency/migrations, workflow retry/metrics) but with a small set of concrete, fixable gaps that block it from being safe to run as a real production service — five of them (auth, TLS, backups, shutdown) are genuine risk, the rest are hardening. This plan closes all of them in dependency order: the two remote-attack-surface bugs first, then the operational gaps that would turn a bug into an outage or data loss, then the rest.

Everything below was verified against the current code (not just the audit's summary) before being written into this plan — file paths and line numbers are as of commit `271bfa3`.

---

## Phase 1 — Close the two remote attack surfaces

**1a. SSH client accepts any host key** — `crates/axon-agent/src/tools/ssh.rs:23-28`

`check_server_key` unconditionally returns `Ok(true)`. Replace with real verification:
- Add a `known_hosts`-style check: on first connect to a given `(host, port)`, store the received key fingerprint (SHA-256 of the public key bytes) in the existing `ssh_servers` table (add a `host_key_fingerprint TEXT` column via a new migration, following the pattern in `crates/axon-agent/src/db/migrations/` — idempotent `ALTER TABLE ... ADD COLUMN`, `tolerant_dup_column` already handles re-runs).
- On subsequent connects, compare the presented key's fingerprint against the stored one with `subtle::ConstantTimeEq` (already a dependency, used in `dashboard/auth.rs:43`) and reject on mismatch.
- Surface a clear error through `SshTool::run_command`'s existing `anyhow::bail!` pattern (`ssh.rs:62-65`) so a changed host key fails loudly instead of silently.
- This needs `state: &AppState` (for DB access) threaded into `Client`/`check_server_key`, which currently only has access to `self`. `Client` will need to carry `db: Arc<...>` and `server_name: String` fields, set at construction in `SshTool::connect`.

**1b. WhatsApp inbound webhook has no signature verification** — `crates/axon-agent/src/dashboard/api/channels.rs:323` (`whatsapp_webhook_messages`) and `crates/axon-agent/src/tools/whatsapp.rs`

WhatsApp Cloud API webhooks are configured under the same Meta App as Facebook and are signed the same way (`X-Hub-Signature-256`, HMAC-SHA256 over the raw body, keyed by the App Secret). Facebook's handler already does this correctly at `crates/axon-agent/src/webhook/facebook.rs:112-130` via `load_fb_creds().app_secret` and the local `verify_signature` (line 541-558).

- Change `whatsapp_webhook_messages`'s signature from `Json(payload): Json<Value>` to `headers: HeaderMap, body: axum::body::Bytes`, matching `fb_event`'s shape, then `serde_json::from_slice` the body after verification.
- Verify using `crate::webhook::facebook::load_fb_creds().app_secret` (same App Secret WhatsApp already shares with Facebook) and the same HMAC check — pull `verify_signature` out of `facebook.rs` into a small shared module (e.g. `crates/axon-agent/src/webhook/signature.rs`) so both call sites use one implementation, and reuse `whatsapp.rs`'s existing `constant_time_eq` (line 998) for the final comparison instead of `computed == expected` (fixes the non-constant-time-compare finding in the same diff).
- If `app_secret` is empty, log a warning and continue accepting unsigned requests (matching Facebook's current fallback behavior at `facebook.rs:120`) rather than silently breaking webhooks for anyone who hasn't configured `credentials.json` yet — but log loudly so it's visible in production.

---

## Phase 2 — TLS termination

**Problem**: `main.rs:602-616` binds plain HTTP on `0.0.0.0`; none of the three deploy scripts (`deployaxongcp.sh`, `deploycham.sh`, `deploychamgcp.sh`) set up a TLS-terminating reverse proxy.

**Approach**: Add Caddy as a reverse proxy in front of the axon-agent port — single static binary, automatic Let's Encrypt provisioning/renewal, minimal config, no certbot cron job to maintain separately.
- Add a `Caddyfile` template (new file, e.g. `deploy/Caddyfile.example`) reverse-proxying `{$AXON_DOMAIN}` → `localhost:{$AXON_PORT}`, with the WebSocket route (`/ws`) explicitly allowed (Caddy proxies WS by default, but call it out in the template comment since `/ws` is used by the dashboard chat).
- Extend each deploy script's systemd-install step (`deployaxongcp.sh` around its "Installing systemd services" block, mirrored in `deploycham.sh`/`deploychamgcp.sh`) to optionally install Caddy and drop the rendered Caddyfile, gated behind a new `AXON_DOMAIN` var in `.deploy.env` — if unset, skip the proxy install and print a warning that the instance is HTTP-only, so this stays additive and doesn't break existing deployments without a domain.
- Update `README.md`'s deployment section to document TLS as a required step for any internet-facing deployment, not an afterthought.

---

## Phase 3 — Scheduled SQLite backups

**Problem**: Qdrant has full backup/retention/health-check automation (`qdrant/axon-backup.sh` + `axon-backup.timer`); `axon.db` and `crm.db` — the databases holding encrypted credentials, OAuth tokens, and CRM PII — have none. The only existing tool is `crm_backup_db` (`crates/axon-crm/src/records.rs:253-274`), which is manual-invoke-only and has no `axon.db` equivalent.

**Approach**: add an in-process scheduled backup task, following the exact pattern already used for retention (`main.rs:477-498`, a `tokio::spawn` interval loop wrapping a `tokio::task::spawn_blocking` DB call) rather than a new external script, so it works uniformly regardless of deploy target and doesn't depend on the operator wiring up cron.

- New function `axon::maintenance::run_backup(db: &Pool, crm_pool: &SqlitePool, dir: &Path)`:
  - For `axon.db`: `VACUUM INTO` a timestamped file, same technique as `crm_backup_db` (`records.rs:259-264`) — SQL-escape the path the same way (`replace('\'', "''")`).
  - For `crm.db`: call the existing `axon_crm::records::backup_db` directly.
  - Write both into a single backup directory (default alongside `axon_core::data_files_dir()`, which `crm_backup_db` already uses — reuse it for consistency instead of introducing a third location) and prune backups older than a configurable retention (default 14 days), mirroring `axon-backup.sh`'s `RETENTION_DAYS` pattern.
- Wire a new `tokio::spawn` block into `main.rs` next to the retention sweep (~line 480), running daily, logging success/failure the same way (`tracing::info!`/`tracing::warn!`).
- Add a `backup.enabled` / `backup.retention_days` pair to `RuntimeSettings` (same mechanism as `workflow.max_concurrent_runs` etc.) so it's configurable from the dashboard without a redeploy, and expose it on the Settings page in `axon-ui`.
- Document in `README.md` that these are **local, on-instance** backups (same caveat `axon-backup.sh` already states for Qdrant's default) — off-instance copy is the operator's responsibility; don't over-promise disaster recovery from a single-node backup living next to the data it protects.
- Fold in the DB-file-permissions finding here since it's the same file-handling code path: after creating the SQLite pool in `main.rs` (~line 250, right after `SqliteConnectionManager::file`) and after each backup file is written, apply the same `fs::set_permissions(..., 0o600)` pattern already used for `tokens.json`/`credentials.json` in `crates/axon-core/src/storage.rs:348-353`, to `axon.db`, its `-wal`/`-shm` siblings, `crm.db`, and every backup file produced by this phase.

---

## Phase 4 — Shutdown signal and real health checks

**4a. Graceful shutdown only listens for SIGINT** — `main.rs:609-619`

Systemd sends `SIGTERM` on `stop`/`restart` (confirmed: `deploycham.sh` installs the unit with no `KillSignal=` override, so systemd's default applies). Add a SIGTERM branch alongside the existing `ctrl_c()` future:

```rust
let shutdown_signal = async {
    let ctrl_c = async { tokio::signal::ctrl_c().await.expect("...") };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
    tracing::info!("Shutdown signal received. Shutting down gracefully...");
};
```
No other change needed — `axum::serve(...).with_graceful_shutdown(shutdown_signal)` is already wired correctly; it just wasn't listening to the signal that actually gets sent in production.

**4b. `/health` and `/ready` are unconditional 200s** — `dashboard/server.rs:6-12`

- Keep `/health` (liveness — "is the process alive") as the trivial `"OK"` responder; that's the correct semantics for a liveness probe.
- Make `/ready` (readiness — "can this instance serve traffic") actually check dependencies: acquire a connection from `state.db` with a short timeout and run `SELECT 1`, returning `503` on failure/timeout instead of always `"READY"`. This mirrors the dependency-aware check that already exists but is disconnected from HTTP — `router::model_router::health_check()` (`router/model_router.rs:975`) — as a model for what "actually check the dependency" looks like, though `/ready`'s DB check is simpler (no LLM provider involved).
- Leave `/api/health` (`observability::health_json`) as-is; it already serves a different purpose (authenticated capacity/queue-depth reporting) and shouldn't be conflated with the public liveness/readiness probes.

---

## Phase 5 — Dependency CVEs

`crates/axon-agent/Cargo.toml:43-44,106` pin `russh`/`russh-keys` `0.45` and `rmcp` `0.1` (with `features = ["server"]`).

- **`russh`/`russh-keys` 0.45 → ≥0.60.3**: this is a major-version jump; russh's `client::Handler` trait (the one `ssh.rs` implements for `check_server_key`) and the `russh-keys` public-key types changed between these versions. Since Phase 1a already touches `check_server_key` to add real host-key verification, do this bump *as part of Phase 1a*, not separately — write the new host-key-checking logic against the upgraded crate's API from the start rather than upgrading twice. Budget time to consult the russh changelog/migration notes; this is not a drop-in version bump.
- **`rmcp` 0.1.5 → ≥1.4.0** (fixes the DNS-rebinding CVE on the server transport): also a major jump. Confirm during the bump that `features = ["server"]` is still actually needed — the audit found no `SseServer`/`StreamableHttpServer` startup code, only client usage (`mcp/client.rs`, `mcp/inprocess.rs`). If the server feature is genuinely unused, drop it entirely instead of upgrading it, which sidesteps the CVE and shrinks the dependency tree.
- **`rsa` (transitive via `russh-keys`/`sqlx-mysql`)**: no upstream fix exists for the Marvin Attack timing side-channel (RUSTSEC-2023-0071). Track it (e.g. a `deny.toml` exception with a comment linking the advisory) rather than blocking on it.
- Once these are resolved, flip `.github/workflows/ci.yml`'s `deny` job from `continue-on-error: true` to blocking (remove line 31), and update or remove the stale comment at lines 27-30 (two of the five crates it names, `rustls`/`rustls-webpki`, are already resolved at the currently pinned versions).

---

## Phase 6 — Observability & logging hardening

- **Structured logs + correlation IDs**: switch `tracing_subscriber::fmt()` (`main.rs`, near the OTel setup ~line 155-181) to `.json()` output, gated behind an env var (e.g. `AXON_LOG_FORMAT=json`) so local `cargo run` output stays human-readable. Add `tower_http::trace::TraceLayer` (the `tower-http` dependency already has this available; currently only `["fs", "cors"]` per `Cargo.toml:21` — add the `trace` feature) to `build_router` in `dashboard/server.rs`, which gives every request an automatic span with a generated request ID — the missing piece the audit flagged (zero `#[instrument]` usage, no per-request span).
- **Workflow run correlation**: pass the existing `run_id` (already generated per `workflow_runs` row) into a `tracing::info_span!("workflow_run", run_id = %run_id)` around the run-execution call in `tools/workflow.rs`, so every log line inside a run's lifetime carries it automatically — no new ID scheme needed, just propagate the one that already exists.
- **Rate limiting**: add `tower-governor` (or equivalent) as a layer on the public router in `dashboard/server.rs` — scoped to the unauthenticated routes only (`/webhook/*`, `/health`, `/ready`), leaving `protected` routes alone since those already require the master key.

---

## Phase 7 — Frontend hardening

- **CSP**: add a `Content-Security-Policy` meta tag to `axon-ui/index.html` (currently has no CSP at all) — start restrictive (`default-src 'self'; connect-src 'self' ws: wss:`) and loosen only as needed once tested against the built SPA. This directly narrows the blast radius of the `localStorage`-stored master key (`axon-ui/src/lib/api.js:2`, `App.vue:113,160`) that the audit flagged — a future XSS can no longer freely exfiltrate to an arbitrary origin.
- **Test coverage**: the existing 3 test files (`axon-ui/tests/*.test.js`) cover expression updates, markdown, and utils — none cover the Vue components or API layer. Prioritize adding component tests for the pages that handle credentials/secrets first (Services/Models pages), since those are the highest-value gap, rather than chasing blanket coverage.
- **Lint**: add an `eslint.config.js` (flat config, matching Vite/Vue 3 tooling) with the standard `eslint-plugin-vue` recommended set, and a `lint` script in `package.json` wired into `.github/workflows/ci.yml`'s `frontend` job alongside the existing `npm test`/`npm run build` steps.

---

## Verification

Each phase should be validated before moving to the next:

- **Phase 1**: manually point the SSH tool at a server, then swap the host's key (or MITM via a local test proxy) and confirm the connection is now rejected instead of silently succeeding. For WhatsApp, send a POST to `/webhook/whatsapp` with a bad/missing signature and confirm `401`, then with a valid signature (computed with the configured app secret) and confirm it still triggers workflows as before.
- **Phase 2**: deploy to a test instance with `AXON_DOMAIN` set, confirm `https://<domain>` serves the dashboard with a valid cert and `/ws` still connects (chat still streams).
- **Phase 3**: let the backup interval fire (or trigger it manually via a debug path), confirm timestamped `axon.db`/`crm.db` snapshots appear with `0600` permissions, then do a real restore drill — stop the agent, swap in a backup file, restart, confirm data is intact.
- **Phase 4**: `systemctl restart axon-agent` while a long-running workflow is mid-execution; confirm the run completes (or is cleanly marked failed) instead of leaving a `'running'` row behind. Kill the DB connection pool artificially (or point `AXON_DB_PATH` at a bad path post-boot) and confirm `/ready` returns non-200 while `/health` still returns 200.
- **Phase 5**: `cargo build --workspace` and `cargo test --workspace` pass after each dependency bump; `cargo deny check advisories` is clean (or has only the documented `rsa` exception).
- **Phase 6**: tail production-shaped logs (`AXON_LOG_FORMAT=json cargo run`) and confirm each line is valid JSON with a request/run ID; hit a webhook endpoint in a loop and confirm rate limiting kicks in with a `429`.
- **Phase 7**: `npm run build` succeeds with the CSP in place and the dashboard still functions in a browser (chat, model management, credential connect flows) with no CSP console errors; `npm run lint` runs clean in CI.

Run the full existing test suites (`cargo test --workspace`, `npm test` in `axon-ui/`) after every phase — regression coverage for the pieces already well-tested (migrations, retry/backoff, CRM transactions) should catch any accidental breakage from these changes.
