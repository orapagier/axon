# Axon Node Expansion Plan

**Goal:** close the capability gap between Axon's workflow engine and n8n's most-used
nodes, one node at a time. Axon is already strong on **AI, messaging, HTTP, and SQL**.
The gaps are in **data plumbing, format/utility conversion, email, and a few
high-value connectors**. This plan adds them in leverage order so each phase unlocks
whole new *workflow shapes*, not just one more integration.

> Status legend: `[ ]` not started · `[~]` in progress · `[x]` done
> Effort: **S** ≈ half-day · **M** ≈ 1–2 days · **L** ≈ 3+ days

---

## 0. How to add a node (the repeatable recipe)

Every node touches the same 5 places. Reuse `soma.rs` / `condition.rs` as templates.

### Backend (Rust) — 4 edits
1. **Executor** — new file `crates/axon-agent/src/tools/workflow/nodes/<name>.rs`
   exposing `pub(crate) fn execute(config: &Value, ...) -> Result<Value, String>`
   (async only if it needs `state`/IO). Values in `config` arrive **already
   expression-resolved** by `interpolate_config` — just `config.get("param")`.
2. **Module registration** — add `pub(crate) mod <name>;` to
   `crates/axon-agent/src/tools/workflow/nodes/mod.rs`.
3. **Dispatch arm** — add `"<type>" => nodes::<name>::execute(config, ...).await,`
   in `execute_node_dispatch` (`crates/axon-agent/src/tools/workflow.rs:500`).
4. **No-retry list** (only for control/branch nodes that must not re-run) —
   `execute_node_by_type` (`workflow.rs:600`).

Add `#[cfg(test)] mod tests` in the executor file (see `condition.rs:166` for the
pattern — pure functions, table-driven).

### Frontend (Vue/JS) — 1 edit
5. **Palette + form** — add a `NODE_TYPES.<type>` entry in
   `axon-ui/src/lib/nodes.js` with `displayName`, `name`, `icon` (emoji),
   `description`, and `properties[]` (n8n-style descriptors: `string`, `number`,
   `boolean`, `options`, `multiOptions`, `fixedCollection`, `collection`,
   `credential`, `notice`). Set `dynamicOutputs: true` for N-output nodes (see
   `switch`). The canvas auto-renders via `CanvasNodeDefault.vue`; add a custom
   `nodes/*.vue` only for special UX.

### Data-model note (read before Phase 1)
Axon nodes currently output a **single `Value`**, and a node reads its predecessors
from `node_results` (sorted by `position`; primary input = last). n8n is
**item-array** based. The list-shaping nodes below therefore operate on an **array
`Value`** as their item stream. Decide once: keep the "primary input is one Value
that may be an array" convention. This keeps parity without rewriting the engine.

**Naming:** type keys stay literal/n8n-parity for developer clarity; `displayName`
can carry the neuro theme (as `database` already shows as *Hippocampus*). Suggested
neuro names are noted per node — optional.

---

## Phase 1 — Data plumbing (highest leverage) — the biggest structural hole

Today Axon can **split** flow (IF / Switch / Approval fork into branches) but has
**no way to rejoin or reshape lists**. Every fork is currently a dead-end. Fix that first.

| # | Node (type key) | displayName (neuro) | Effort | Outputs |
|---|---|---|---|---|
| 1.1 | `merge` | Plexus (Merge) | M | 1 |
| 1.2 | `filter` | Synaptic Gate (Filter) | S | 1 |
| 1.3 | `aggregate` | Summation (Aggregate) | M | 1 |
| 1.4 | `splitOut` | Split Out | S | 1 |
| 1.5 | `sortLimit` | Sort / Limit / Dedupe | M | 1 |

- [ ] **1.1 Merge** — join/append two branches. Modes: `append`, `mergeByKey`
  (SQL-style join on a field), `mergeByPosition`, `combine`. Reads **multiple**
  predecessors from `node_results` (dispatch already passes it — see how `javascript`
  and `soma` consume `node_results`). *This is the #1 unlock.*
- [ ] **1.2 Filter** — keep/drop array items matching a condition. Reuse
  `evaluate_condition_typed` (already used by `condition.rs`). One output; dropped
  items disappear from the stream.
- [ ] **1.3 Aggregate / Summarize** — roll an array into one item:
  `sum`/`avg`/`min`/`max`/`count`/`concat`/`collectField`. Complements Loop.
- [ ] **1.4 Split Out** — explode a list field into individual items (inverse of
  Aggregate). Enables per-item fan-out into Loop/Cortex.
- [ ] **1.5 Sort / Limit / Remove Duplicates** — item-list utilities. Can ship as
  one node with a `mode` option or three tiny nodes. Sort by field ± direction,
  limit N (head/tail), dedupe by key.

**Phase 1 verification:** build a test workflow `Stimulus → Switch → (two branches) →
Merge → Soma` and confirm branches rejoin. Add unit tests per node (pure functions).

---

## Phase 2 — Format & utility (make Synapse/Myelin actually useful)

Synapse can *fetch* and Myelin can *store*, but nothing **parses or transforms** the
payload. These turn raw bytes into structured data.

| # | Node (type key) | displayName | Effort | Crate dep |
|---|---|---|---|---|
| 2.1 | `dateTime` | Chronon (Date & Time) | S | `chrono` (likely present) |
| 2.2 | `crypto` | Enzyme (Crypto) | S | `sha2`, `hmac`, `uuid` |
| 2.3 | `htmlExtract` | Retina (HTML Extract) | M | `scraper` |
| 2.4 | `extractFromFile` | Digest (Extract from File) | M | `csv`, `calamine` (xlsx) |
| 2.5 | `convertToFile` | Convert to File | M | `csv` |
| 2.6 | `compression` | Compression (zip/gzip) | S | `zip`, `flate2` |
| 2.7 | `xml` / `markdown` | XML / Markdown | S | `quick-xml`, `pulldown-cmark` |

- [ ] **2.1 Date & Time** — parse/format/add/subtract/diff; timezones. Extremely
  common; today only doable in a JavaScript node.
- [ ] **2.2 Crypto** — hash / HMAC / sign / UUID. Needed for **webhook signature
  verification** and idempotency keys.
- [ ] **2.3 HTML Extract** — CSS-selector extraction → turns "Synapse fetch a page"
  into real **web scraping**.
- [ ] **2.4 Extract from File** — CSV / XLSX / PDF-text → JSON. Myelin stores files
  but can't read a spreadsheet today.
- [ ] **2.5 Convert to File** — JSON → CSV/XLSX/text for export/attachments.
- [ ] **2.6 Compression** — zip/unzip/gzip for archives & attachments.
- [ ] **2.7 XML / Markdown** — XML↔JSON and Markdown↔HTML converters.

---

## Phase 3 — Communication (close the email gap)

Glaring omission: Axon has Telegram/WhatsApp/Discord/Slack/Facebook but **no email**,
even though the Classifier's own description says *"e.g. an email."*

| # | Node (type key) | displayName | Effort | Notes |
|---|---|---|---|---|
| 3.1 | `email` (send) | Axon Terminal (Email) | M | SMTP via `lettre` |
| 3.2 | `emailTrigger` | Email Trigger (IMAP) | L | IMAP poll → new Stimulus source |
| 3.3 | `respondToWebhook` | Efferent (Respond) | M | return custom HTTP response |
| 3.4 | `rss` | RSS Read | S | `feed-rs` |
| 3.5 | `sms` | SMS (Twilio) | S | HTTP wrapper, low priority |

- [ ] **3.1 Send Email (SMTP)** — one of n8n's top-3 actions. Credential-backed.
- [ ] **3.2 Email Trigger (IMAP)** — inbound-email automation; integrates as a new
  trigger source alongside `stimulus`. Pairs perfectly with the Classifier.
- [ ] **3.3 Respond to Webhook** — return a custom HTTP body/status so a workflow can
  **be an API**, not just receive. Wire into the existing webhook path
  (`crates/axon-agent/src/webhook/external.rs`).
- [ ] **3.4 RSS Read** — feed monitoring.
- [ ] **3.5 SMS/Twilio** — optional; thin HTTP wrapper.

---

## Phase 4 — AI extensions (build on Cortex/Classifier/Qdrant)

Small additions that meaningfully extend the agent layer you already have.

| # | Node (type key) | displayName | Effort | Notes |
|---|---|---|---|---|
| 4.1 | `informationExtractor` | Extractor | M | schema-guided JSON out |
| 4.2 | `vectorStore` | Neocortex (RAG) | L | embed → upsert → semantic search |
| 4.3 | `summarize` / `sentiment` | Summarize / Sentiment | S | LLM presets |

- [ ] **4.1 Information Extractor** — schema-guided structured JSON extraction.
  Classifier only *tags*; this *pulls fields*. Reuse the Cortex/Classifier LLM path.
- [ ] **4.2 Vector Store / RAG node** — the `qdrant/` folder exists but Engram is
  key-value, not semantic. A first-class **embed → upsert → semantic-search** node
  makes retrieval a workflow step. Reuse the provider-configurable embedder.
- [ ] **4.3 Summarize / Sentiment** — thin LLM presets over the Cortex path.

---

## Phase 5 — Connectors (only as use cases demand)

Most Google Workspace needs are already covered by the **MCP Tool** node. Add these
only when a real workflow needs them.

- [ ] **5.1 Notion** (M) — pages/databases CRUD.
- [ ] **5.2 Airtable** (M) — base/table CRUD.
- [ ] **5.3 Redis** (S) — cache / pub-sub / rate-limit.
- [ ] **5.4 AWS S3 / object storage** (M) — file storage beyond Myelin-local.
- [ ] **5.5 Stripe** (M) — payments/webhooks.

---

## Suggested build order (if you only do a few at a time)

1. **Merge** (1.1) — unlocks fan-out/fan-in; nothing else compares in leverage.
2. **Email send + IMAP trigger** (3.1, 3.2) — a whole new automation category.
3. **HTML Extract + Extract from File** (2.3, 2.4) — scrape-then-process.
4. **Filter / Aggregate / Split Out** (1.2–1.4) — full list-processing toolkit.
5. **Date & Time + Crypto** (2.1, 2.2) — remove the "drop to JavaScript" tax.
6. Everything else as needed.

## Per-node Definition of Done
- [ ] Executor file + unit tests (pure logic table-driven like `condition.rs`).
- [ ] Registered in `nodes/mod.rs` + dispatch arm in `workflow.rs`.
- [ ] `NODE_TYPES` entry in `nodes.js` (icon, description, properties).
- [ ] No-retry list updated if it's a control/branch node.
- [ ] Manual run in the canvas exercising the node end-to-end.
- [ ] `graphify update .` run to refresh the knowledge graph.
- [ ] Committed + pushed to `main`.
