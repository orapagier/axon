# Axon CRM — Production-Readiness Plan

## Context

`crates/axon-crm` (v2.0.0, ~3,900 lines) is a single-user/single-company CRM exposed as 30 agent tools (leads, deals, orgs, activities, plus pipeline/dashboard/search/export views) backed by its own SQLite database (`crm.db`). It runs in-process inside the agent (`crates/axon-agent/src/mcp/inprocess.rs`), same single-binary model as the rest of Axon: local dev on Windows or on the Linux server behind a cloudflared tunnel. Inspiration is a GHL-style CRM paired with the existing n8n-like workflow automation.

The foundation is solid — WAL + foreign keys + CHECK constraints + indexes, soft-delete/archive with dependency guards, parameterized queries with LIKE escaping, enum/email/currency/timestamp validation, confirm-gated hard deletes, 5 passing integration tests, and a 730-line `crm-guide.md`. An audit (2026-07-04) found correctness and operational gaps that would bite in real use. This plan fixes them and adds GHL-style trigger automation.

**Design decisions:**

- Single-user/single-company, self-hosted. Not SaaS — no multi-tenancy or per-user permissions.
- Money migrates to integer cents.
- Must work identically local and on the server (no sync story; configurable data dir + backups instead).
- **Architecture: the crate stays; workflows are the automation surface.** Considered replacing the crate with "CRM nodes" in the n8n-like WorkflowsPage. Resolution: the crate's 30 tools *already* surface as workflow nodes (WorkflowsPage has a CRM node category; `inprocess.rs` exposes `crm_*` to the tool registry) — the crate is the data backbone those nodes call, not a competitor to them. Nodes are verbs; the crate is the noun store (integrity, dedupe, links, reporting) that n8n-style engines always delegate to an external CRM. What's missing for GHL parity is **triggers** ("when lead created → run workflow") — added as Phase 4. A dedicated record-browsing UI page is optional Phase 5.

## Findings (audit results, 2026-07-04)

All 5 tests pass (`cargo test -p axon-crm`). Gaps, ordered by severity:

| # | Severity | Issue | Where |
|---|----------|-------|-------|
| 1 | P0 | CRM pool sets no `busy_timeout`; agent DB uses 10s (precedent: commit ea0ef42). Concurrent writes (agent + workflow + scheduler) can fail with SQLITE_BUSY. | `crates/axon-crm/src/db.rs:12` |
| 2 | P0 | `total_value` / pipeline / dashboard sums add amounts across mixed currencies into one number — wrong reports the moment a non-USD deal exists. | `deals.rs:119`, `deals.rs:324`, `views.rs:282` |
| 3 | P0 | Amounts stored as `REAL` (f64) — float drift in money math. | `db.rs:81`, all of `deals.rs` |
| 4 | P0 | `expected_close`/`occurred_at` accept any RFC3339 offset, but stale/overdue/closing-soon logic compares strings lexicographically against `Utc::now().to_rfc3339()` — wrong results for non-UTC offsets (e.g. `+10:00`). | `views.rs:250-331`, `utils.rs:210` |
| 5 | P1 | Hard-deleting a lead with **archived** deals passes the active-deal check, then hits the raw SQLite `ON DELETE RESTRICT` error — confusing instead of teaching. | `leads.rs:206-217` |
| 6 | P1 | Schema setup is ad-hoc (`CREATE TABLE IF NOT EXISTS` + `ensure_column`) instead of the versioned `MIGRATIONS: &[Migration]` pattern the agent DB uses. The cents migration (#3) needs real versioning anyway. | `db.rs:25` vs `crates/axon-agent/src/db/mod.rs:30` |
| 7 | P1 | No length caps on text fields — a looping agent can write megabytes into `notes`/`body`. | `utils.rs` |
| 8 | P1 | `crm_export_snapshot` returns the entire DB as one tool result — blows agent context on a real dataset; no file export, no scheduled backup. | `records.rs:194` |
| 9 | P1 | No duplicate guard — agents will re-create leads with the same email / orgs with the same name. | `leads.rs:51`, `orgs.rs` |
| 10 | P2 | `data_dir()` isn't configurable (`dirs::data_local_dir()/axon-mcp` hardcoded; only `data_files_dir()` honors `AXON_DATA_DIR`) — server deployments can't place `crm.db` on a mounted/backed-up volume. | `crates/axon-core/src/storage.rs:15` |
| 11 | P2 | Existence checks (`ensure_org_exists` etc.) run outside the insert/update transaction — small TOCTOU window. Low risk single-writer; fix opportunistically while touching queries. | `leads.rs`, `deals.rs` |
| 12 | P2 | No UI to browse/edit records — tool-only today. | axon-ui |

## Phase 1 — Data safety & correctness (do first)

**1.1 busy_timeout** — in `db.rs::migrate`, add `PRAGMA busy_timeout = 10000` alongside the existing pragmas (mirror `crates/axon-agent/src/main.rs:236`).

**1.2 Versioned migrations** — port the agent's migration pattern (`crates/axon-agent/src/db/mod.rs`, `MIGRATIONS` array + `PRAGMA user_version`) into `axon-crm/src/db.rs`. Baseline migration = current schema; the existing `CREATE TABLE IF NOT EXISTS` + `ensure_column("deleted_at")` calls become migration v1 so existing databases adopt versioning cleanly.

**1.3 Money → integer cents** — migration v2:
- Add `amount_minor INTEGER NOT NULL DEFAULT 0 CHECK(amount_minor >= 0)` to `deals`; backfill `ROUND(amount * 100)`; rebuild the table in the migration to drop the old `REAL` column cleanly.
- Tool API keeps accepting/returning decimal `"amount"` (agents think in dollars): parse input → cents via round-half-even; serialize `amount_minor / 100.0` plus raw `amount_minor` in responses.
- All aggregations (`SUM`) move to integer cents.

**1.4 Per-currency aggregation** — `deal_list` `total_value`, `pipeline_summary`, and `dashboard_summary` pipeline values group by `currency`, returning e.g. `"total_value": {"USD": 125000.0, "EUR": 4000.0}`. Update `crm-guide.md` examples.

**1.5 UTC normalization** — in `validate_rfc3339_opt` (rename to `parse_rfc3339_utc`), parse and **rewrite** the stored value as UTC RFC3339 with a fixed format so lexicographic comparisons are always correct. Apply to `expected_close` and `occurred_at` on create/update/convert. Migration v3 normalizes existing rows.

**1.6 Teaching error for archived-deal FK** — in `leads::delete`, count linked deals regardless of `deleted_at` and return "archived deal(s) still reference this lead — restore and delete them, or archive the lead instead."

**1.7 Field length caps** — add `max_len` enforcement in `utils.rs` (e.g. name/title 500, email/phone 200, notes/body 64 KB, tags ≤ 50 × 100 chars) with teaching errors; apply in all create/update paths.

**1.8 Transactional existence checks** — while touching create/update in `leads.rs`/`deals.rs`, move the `ensure_*_exists` + INSERT/UPDATE into one transaction.

## Phase 2 — Operational readiness ✅ Done (2026-07-04)

Also shipped alongside Phase 2 (operator decision, not in the original plan): **agent CRM access is read-only by default** — the 15 `crm_*` write tools (create/update/delete/convert/archive/restore) follow the Facebook/Instagram workflow-only pattern (`CRM_WRITE_TOOLS` in `registry.rs`), with a **Settings → CRM toggle** (`crm.agent_write_tools`, seeded off) that grants the agent full read/write without a restart. Workflow nodes keep full access via `all`/`run` regardless. A new `crm_backup_db` tool (VACUUM INTO → Files page) brings the tool count to 33.

**2.1 Configurable data dir** — make `axon_core::storage::data_dir()` honor `AXON_DATA_DIR` (opt-in env override, same convention as `data_files_dir()`; default unchanged so existing deployments are unaffected). Document that `crm.db` lives there.

**2.2 Export to file + slim tool output** — `crm_export_snapshot` gains `to_file` (default `true` when the dataset exceeds ~200 records): writes timestamped JSON into `data_files_dir()` (so it lands in the Files page and is fetchable by workflow nodes), returns path + counts only. Inline full dump only for small datasets or explicit `to_file: false`.

**2.3 Backup guidance + automation** — document in `crm-guide.md`: SQLite online backup (`VACUUM INTO` is simplest under WAL) and a sample scheduled workflow (existing scheduler) that runs `crm_export_snapshot` weekly. Optional: `crm_backup_db` tool that runs `VACUUM INTO` to `data_files_dir()`.

**2.4 Duplicate guard** — `crm_lead_create`: if an active lead with the same (case-insensitive) email exists, return a teaching error carrying the existing id unless `allow_duplicate: true`. Same for `crm_org_create` on exact name match. Update `lib.rs::tool_list` descriptions so agents learn the flow.

## Phase 3 — Tests & docs ✅ Done (2026-07-04)

- Regression tests in `src/tests.rs` (all landed interleaved with Phases 1–2; 14 tests green): mixed-currency totals, cents round-trip + migration on a legacy REAL db file, non-UTC timestamp normalization (stale/overdue correctness), archived-deal delete error, duplicate guard, length caps, busy DB smoke (two pools writing).
- `crm-guide.md` audited line-by-line against the code (tool count, caps, defaults, error strings, gating, response shapes) — one stale claim fixed: connection pool is 3, not 8 (RAM-trim change). `axon-ui` DocsPage has no embedded CRM docs (only a one-line node-category mention), so no drift there.
- `cargo clippy --all-targets -p axon-crm -- -D warnings` and `cargo fmt --check` clean.

## Phase 4 — GHL-style automation: CRM triggers + sample workflows ✅ Done (2026-07-04)

Implemented as a first-class **CRM Stimulus trigger** (the workflow-scheduler poll-first pattern the Gmail trigger uses, not the notification watcher engine — that's where "run a workflow on CRM change" actually lives):

- **`crm_changes_since` view tool** (tool #34) — change feed of active leads/deals/orgs with `updated_at > since` cursor, rows tagged `created`/`updated`, exclusive `cursor` + `has_more` for resumption; both comparison sides normalized to fixed UTC (SQLite `strftime` twin of `format_utc`).
- **Backend trigger** — `check_and_trigger_crm` in `workflow.rs`: events `lead_created` / `deal_created` / `deal_stage_changed` (deal_id→stage map in `trigger_config.crm_known_stages`, seeded at baseline, `previous_stage` in payload) / `any_change`; cursor in `trigger_config.crm_cursor` advances before firing; payload staged per-RUN via `trigger_data.rs`; `execute_crm_trigger` gives manual Execute Step a live 24h→30d test fetch. Pure `filter_crm_hits` covered by 6 unit tests.
- **Frontend** — Stimulus node Trigger Type gains **CRM** (event picker, poll interval, output-shape notice); UI rebuilt into `axon-agent/static`.
- **Docs** — `crm-guide.md`: `crm_changes_since` reference, "CRM Triggers" section, and 4 sample automations (new lead → welcome email; Qualified → convert-to-deal with a loop-safe `deal_count == 0` guard; deal Won → Telegram; weekly dashboard digest + backup via Circadian).

## Phase 5 (optional, defer until wanted) — Dedicated CRM page in axon-ui

Record *browsing/editing* UI (GHL-style pipeline board, contact timeline). Not required for the CRM to be production-ready — agent chat + workflow nodes cover operation; build this when hand-editing records becomes a real need.

- **Backend:** `/api/crm/*` routes in `crates/axon-agent/src/dashboard/server.rs` + a new `dashboard/api/crm.rs`, thin wrappers that call the existing `CrmService::call` (reuse tool logic 1:1; no duplicate SQL). Endpoints: list/search/get/create/update/archive/restore for the 4 entities + `pipeline`, `dashboard`.
- **Frontend:** `axon-ui/src/pages/CrmPage.vue` + NAV entry in `App.vue` (reuse existing table/panel styling from ModelsPage/TasksPage):
  - Leads table (status filter, search, inline status change)
  - Deals kanban grouped by stage with per-stage totals (per-currency)
  - Orgs list; record drawer showing `crm_record_overview` (linked records + activity timeline, log-activity form)
  - Archive/restore; dashboard tiles from `crm_dashboard_summary`
- Build via `run.bat` (vite build → copy to `axon-agent/static`).

## Out of scope (deliberate)

- Multi-user auth/permissions, per-user audit trail — single-user product.
- FTS5 search — LIKE with indexes is fine at single-company scale; revisit if >~50k records.
- Field-level encryption of CRM data — DB lives on user-controlled host; master-key scheme already protects credentials.
- Messaging-gateway send tools from CRM — policy: gateways are chat/workflow only.

## Verification

1. `cargo test -p axon-crm` — all existing + new tests green.
2. `cargo clippy --all-targets -p axon-crm -- -D warnings`.
3. Migration check: copy a pre-change `crm.db`, open with new build, confirm `user_version` bump, cents backfill, timestamp normalization, and that all rows survive.
4. Run the app locally, exercise via chat: create org → lead (duplicate email rejected) → convert to deal in EUR + deal in USD → `crm_pipeline_summary` shows per-currency totals → export snapshot lands in Files page.
5. Phase 4: create a CRM watcher ("New lead", on_change) via the UI, create a lead through chat, confirm the linked workflow fires with the lead payload staged per RUN id.
6. Phase 5 (if built): open the CRM page, verify leads table, kanban stage change, record drawer, archive/restore round-trip.
7. Commit + push each phase to `main`; `graphify update .` after each code change.

## Execution order & sizing

| Phase | Size | Risk |
|-------|------|------|
| 1 (data safety) | ~1 session | Migration is the only risky bit — mitigated by verification step 3 |
| 2 (ops) | ~1 session | Low; `AXON_DATA_DIR` change touches axon-core (verify other services unaffected) |
| 3 (tests/docs) | interleaved | — |
| 4 (CRM triggers) | ~1 session | Medium — touches watcher engine; follow trigger-payload staging pattern |
| 5 (UI page, optional) | 1–2 sessions | Low; additive; defer until wanted |
