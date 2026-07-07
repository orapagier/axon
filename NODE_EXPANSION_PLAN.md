# Axon Node Expansion Plan

**Goal:** close the capability gap between Axon's workflow engine and n8n's most-used
nodes, one node at a time. Axon is already strong on **AI, messaging, HTTP, SQL, and
Gmail** (trigger + send — see Phase 3 note). The gaps are in **data plumbing,
format/utility conversion, workflow-as-API, and a few high-value connectors**. This
plan adds them in leverage order so each phase unlocks whole new *workflow shapes*,
not just one more integration.

> Status legend: `[ ]` not started · `[~]` in progress · `[x]` done
> Effort: **S** ≈ half-day · **M** ≈ 1–2 days · **L** ≈ 3+ days

---

## Architecture decision: data model & conventions (read first)

**Decision: keep Axon's single-Value data model. Do NOT adopt n8n's item-array
engine.** Close the capability gap additively (new nodes + one convention) rather
than rewriting the execution core.

### The two models
- **n8n = item-array.** Every node receives an *array of items* and is expected to
  **map over each item automatically** in one execution pass. Iteration is implicit,
  baked into the data plane.
- **Axon = single Value.** Each node's `execute()` takes one predecessor `Value` and
  returns one `Value` (which *may* be a JSON array). The engine does **not** auto-map;
  iteration is **explicit** via the `loop` node and visible on the canvas.

### Why single-Value is the right foundation for Axon
1. **Axon is agent-first, not ETL-first.** The center of gravity is
   Cortex/Classifier/messaging/MCP — orchestration and reasoning, not bulk row
   crunching, which is exactly where n8n's implicit mapping pays off.
2. **It preserves Axon's speed edge.** The Rust engine already gives the Loop node
   real `Parallelism` that n8n's single-threaded JS Loop can't. An item-array rewrite
   is large, touches every node + every expression path, and risks that advantage.
3. **Node authoring stays tiny.** "One Value in, one Value out" is why nodes are ~40
   lines and why this whole plan is additive.
4. **Explicit iteration is safer + more readable.** For a tool whose runs have side
   effects (send message, call a tool), a *visible* Loop beats a node that silently
   ran 500 times.

### The convention this locks in
> **A node whose input is a collection receives an array `Value`, and iterates it
> internally.** The list-shaping nodes (Merge, Filter, Aggregate, Split Out, Sort/
> Limit/Dedupe) all agree on this so they compose with each other and with `loop`.

Primary input is still "the most recent predecessor by `position`" (as `soma` /
`javascript` already read `node_results`). No engine rewrite — a list node just
expects that primary `Value` to be an array and loops over it in its own `execute()`.

### What the engine already gives us (verified in code)
**Fan-in synchronization already exists.** `run_inner` executes via in-degree
counting (Kahn's algorithm): a node with two incoming edges only runs after *both*
predecessors resolve. Not-taken branch edges still release in-degree, `live_inputs`
tracks which branches actually ran, and skip-propagation emits `skipped` results so
nothing hangs — the code even anticipates "a merge node fed by both branches"
(`workflow.rs`, `run_inner`, edge-release block). **Merge is therefore mostly a node,
not an engine feature.** The hard half is already built.

### Multi-input caveats (why Task 1.0 exists)
Three verified facts mean a Merge node **cannot** just scan `node_results`:
1. **`node_results` is pre-seeded with stale cache.** Before a run starts it is
   backfilled from up to 25 *prior* runs (expression fallback / Execute Step
   snapshot). A naive scan can merge results from nodes that never ran this run.
2. **Skipped branches leave entries** — `status: "skipped"`, output
   `{"skipped": true, ...}` — that must be filtered out.
3. **Dispatch doesn't pass edges.** From inside `execute()` a node can't tell which
   results are its *direct* predecessors, nor which input (left/right) each feeds.
   Edges already persist `target_handle` and the canvas records `targetHandle`, so
   the data exists — it just isn't handed to the node.

Task 1.0 fixes all three with one small dispatch change + one helper. That is the
only engine-adjacent work in this plan.

### The one trade-off, and how we pay it down
Cost: "do X to each item" always costs an explicit Loop (n8n hides it). Mitigation, in
order:
1. **Standardize the array-input convention above** — do this before building Merge.
2. **Ship the Phase-1 list nodes** — they absorb most per-item work, so `loop` is only
   needed when a *multi-node sub-branch* must run per item (where an explicit loop is
   genuinely clearer anyway).
3. **(Optional, later — engine-level, NOT Phase 1) a "Run Once Per Item" toggle** on
   select nodes (Soma, Cortex, Synapse). When on, the engine maps that single node over
   an array input — giving n8n-style implicit mapping *selectively and opt-in*, without
   converting the whole engine. This is the best-of-both middle path; treat it as a
   future enhancement with its own design pass.

### n8n's Loop vs Axon's Loop (why the settings differ)
n8n has **two** iteration concepts: implicit item-mapping (every node) **and** an
explicit *Loop Over Items* node for batching / loop-back sub-graphs. Axon has **one**
loop, so it does **both** jobs — which is why it needs richer knobs:

| Axon `loop` setting | Purpose | n8n equivalent |
|---|---|---|
| **Items** | Array *expression* to iterate | n8n pulls from the input connection; Axon takes it explicitly |
| **Array Path** | Pick the array field if Items resolves to an object | — (n8n items are already an array) |
| **Parallelism** | Run N iterations concurrently | ⚡ none — n8n's Loop is single-threaded |
| **Batch Size** | Items per iteration (`{{ $node["Loop"].current }}` = the slice) | same as n8n "Batch Size" |
| **Max Iterations** | Safety cap against runaway fan-out | — (Axon-specific guardrail) |

Mechanically, Axon's Loop is a **fan-out** (resolve the array up front, engine spreads
the downstream body across items, optionally in parallel), whereas n8n's is a
**loop-back** ("send me a batch, I'll return for the next").

---

## Dependency policy (binding for every phase)

Deploy target is a **1 GB e2-micro**, deliberately trimmed to a **single
reqwest 0.12 + rustls stack** (malloc arena caps, shrunk SQLite pools). Every new
crate must respect that:

- `default-features = false`, enable only what's needed.
- **rustls only** where TLS is involved. `lettre` and every IMAP crate default to
  `native-tls` — a second TLS stack is a regression; configure rustls explicitly.
- No second HTTP client stack; wrap HTTP APIs over the shared clients in `http.rs`.
- Already in tree (free to use): `chrono`, `chrono-tz`, `sha2`, `hmac`, `uuid`
  (v4), and gzip/deflate machinery via reqwest's compression features (verify with
  `cargo tree` before adding `flate2` directly).

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
   in `execute_node_dispatch` (`workflow.rs`, currently ~line 488 — anchor by
   function name, line numbers drift).
4. **No-retry list** (only for control/branch nodes that must not re-run) —
   `execute_node_by_type` (`workflow.rs`, ~line 590).

Add `#[cfg(test)] mod tests` in the executor file (see `condition.rs` for the
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
See **"Architecture decision"** above — the list-shaping nodes operate on an
**array `Value`** per the locked-in convention (primary input = most recent
predecessor by `position`, expected to be an array). Multi-input nodes (Merge)
additionally require Task 1.0.

**Naming:** type keys stay literal/n8n-parity for developer clarity; `displayName`
can carry the neuro theme (as `database` already shows as *Hippocampus*). Suggested
neuro names are noted per node — optional. Pick final displayNames **before**
building — they end up embedded in saved workflows via `$node["Name"]` references.

---

## Phase 1 — Data plumbing (highest leverage) — the biggest structural hole

Today Axon can **split** flow (IF / Switch / Approval fork into branches) but has
**no way to rejoin or reshape lists**. Every fork is currently a dead-end. Fix that
first. The engine's fan-in sync already works (see above), so this phase is almost
entirely node code.

| # | Node (type key) | displayName (neuro) | Effort | Outputs |
|---|---|---|---|---|
| 1.0 | — (dispatch plumbing) | — | S | — |
| 1.1 | `merge` | Plexus (Merge) | M | 1 |
| 1.2 | `filter` | Synaptic Gate (Filter) | S | 1 |
| 1.3 | `aggregate` | Summation (Aggregate) | M | 1 |
| 1.4 | `splitOut` | Split Out | S | 1 |
| 1.5 | `sortLimit` | Sort / Limit / Dedupe | M | 1 |

- [x] **1.0 Multi-input plumbing** — the one engine-adjacent task, done once:
  - [x] Helper `direct_predecessor_outputs(target_node_id, edges,
    this_run_results)` in `workflow.rs` (table-driven tests:
    `multi_input_plumbing_tests`). Sources from the run's `ordered_results`
    (this-run sequence) — NOT `node_results` — because (a) `ordered_results`
    already excludes the prior-run cache seed AND a skipped node keeps a *stale
    success* in `node_results` via `.or_insert`. So it: (a) considers only
    results produced **this run**, (b) filters `status == "skipped"` entries,
    (c) groups outputs by the incoming edge's `target_handle` (normalized;
    missing → `input_main_0`) so Merge can tell input 0 from input 1.
  - [x] UI: two-input-handle rendering seam — `getNodeInputs()` in `nodes.js`
    + `CanvasNode.vue` `inputs` computed (mirror of `dynamicOutputs`). A node
    type declaring `inputs: 2` (or a label array) in `NODE_TYPES` now renders
    two input handles; edges already persist `targetHandle`.
  - [x] Wired the helper into the dispatch path: `merge_inputs` threaded through
    `execute_node_by_type` → `execute_node_dispatch` (all 6 call sites), computed
    once per node in `run_inner` (gated to `node_type == "merge"`) from
    `edges` + `ordered_results`. `merge` also added to the `can_iterate`
    exclusion so it is never mapped per-item after a Loop.
- [x] **1.1 Merge** (`merge` / *Plexus*) — join/append two branches. Executor
  `nodes/merge.rs` (11 table-driven tests) consumes the 1.0 helper. Modes:
  `append` (default), `mergeByKey` (SQL-style left join on a field, with
  optional per-side `field1`/`field2`), `mergeByPosition` (zip by index),
  `combine` (cartesian). **Skipped-branch semantics done:** the 1.0 helper drops
  the not-taken side, so with one live side Merge passes it through unchanged for
  *every* mode — never errors or nulls the dead side. Field-merge is a full union
  (second input enriches the first; conflicts → input 2 wins). `NODE_TYPES.merge`
  entry renders two input handles via the 1.0 UI seam. *This is the #1 unlock.*
  - Remaining DoD item: manual canvas E2E (Phase 1 verification tests 1–3);
    logic is covered by unit tests + compile/build.
- [x] **1.2 Filter** (`filter` / *Synaptic Gate*) — keep/drop array items matching
  a condition. Executor `nodes/filter.rs` (12 table-driven tests) reuses
  `evaluate_condition_typed` (shared with IF/Switch) so operators never drift.
  **Per-item field access is the key difference from IF:** IF resolves one `value1`
  expression once and routes the whole item; Filter tests a *different* value per
  item, which the engine can't pre-resolve, so each condition names a `field`
  (dot/bracket path relative to the item, via `parse_path_pointer`; blank = the item
  itself, for scalar arrays). `value2` is still interpolated once (constant across
  items). Combine `all`/`any`; `keep: matching|notMatching` inverts the gate;
  optional `arrayPath` unwraps a `{ results: [...] }` wrapper. Bare object → 1-item
  list (mirrors Merge's `flatten_items`, not Loop's aggressive scan). One output;
  dropped items disappear from the stream. Dispatch uses the Soma/`$json`
  primary-input convention (most recent predecessor by position); not in the
  no-retry list (pure transform). `NODE_TYPES.filter` entry in `nodes.js`.
  - Remaining DoD item: manual canvas E2E (Phase 1 verification test 1 with a Filter
    on a branch); logic is covered by unit tests + backend/UI build.
- [x] **1.3 Aggregate / Summarize** (`aggregate` / *Summation*) — roll an array
  into one item. Executor `nodes/aggregate.rs` (13 table-driven tests). Each
  aggregation names an `operation` (`sum`/`avg`/`min`/`max`/`count`/`concat`/
  `collectField`), a source `field` (dot/bracket path per item; blank = the item
  itself, for scalar arrays) and an `outputField` (defaults to the field's last
  segment, or the op name when no field); several aggregations compose into one
  summary object. Numeric ops coerce via the shared `val_to_number` and skip
  non-numeric; concat/collectField skip missing/null; `count` counts all items (or
  only those with the field present). **Output is a bare object** (a reducer's
  result is one item) — `{{ $node["Aggregate"].total }}` reads it directly, and the
  list nodes still treat a bare object as a 1-item list so it composes. Empty
  numeric set → `avg` null, `sum` 0; no aggregations → `{ count: N }`. Shares
  Filter's `to_items`/`field_value`/`arrayPath` convention. Dispatch uses the
  Soma/`$json` primary-input convention; `NODE_TYPES.aggregate` in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + build.
- [x] **1.4 Split Out** (`splitOut`) — explode a list field into individual items
  (inverse of Aggregate). Executor `nodes/split_out.rs` (12 table-driven tests).
  Operates over the primary input as a list: for EACH source item it reads the array
  at `fieldToSplitOut`, emits one output per element, and per `include`
  (`noOtherFields`/`allOtherFields`/`selectedOtherFields`) optionally carries the
  source's other fields onto each element (excluding the split field's top-level
  segment; `fieldsToInclude` names the selected ones). All per-source results
  concatenate. Object elements are used directly; scalar elements — or any element
  when `destinationFieldName` is set — wrap as `{ <dest>: el }` (dest defaults to the
  split field's last segment). The exploded element wins over carried fields on a key
  conflict. Missing field → contributes nothing; non-array field → single element.
  Shares Filter/Aggregate's `to_items`/`arrayPath` convention. Dispatch uses the
  Soma/`$json` primary-input convention; `NODE_TYPES.splitOut` in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + build.
- [x] **1.5 Sort / Limit / Remove Duplicates** (`sortLimit`) — shipped as **one
  node**, structured as a pipeline rather than a one-of `mode` so the common "top N
  unique" needs no chaining. Executor `nodes/sort_limit.rs` (13 table-driven tests).
  Three independently-toggled stages applied in a fixed order: **dedupe** (keep
  first by `dedupeBy` key fields, or whole item) → **sort** (`sortRules`: multiple
  field rules, each `asc`/`desc` and typed `auto`/`number`/`string`/`date`; blank
  field sorts the item itself; stable; missing values sort last) → **limit** (`keep`
  first/last `maxItems`; 0 = no limit). Nothing enabled = pass-through. Reuses the
  shared `val_to_number`/`val_to_datetime`/`val_to_string` + `cfg_usize` helpers and
  the Filter/Aggregate `to_items`/`arrayPath` convention. UI gates each stage's
  params on its boolean toggle via `displayOptions`. Dispatch uses the Soma/`$json`
  primary-input convention; `NODE_TYPES.sortLimit` in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + build.

**Phase 1 complete** (1.0–1.5): the list toolkit — Merge, Filter, Aggregate, Split
Out, Sort/Limit/Dedupe — all share the array-input convention and compose with each
other and with Loop. Every fork can now rejoin/reshape. Only shared DoD gap across
1.1–1.5 is the manual canvas E2E pass (Phase 1 verification tests 1–3).

**Phase 1 verification:**
1. `Stimulus → Switch → (two branches) → Merge → Soma` — branches rejoin.
2. `Stimulus → IF → Merge` where IF takes **one** branch — Merge passes the live
   side through (skipped-side semantics).
3. Re-run test 1 as a targeted "Execute Step" on Merge — confirm the stale-cache
   filter (1.0a) keeps prior-run results out.
4. Unit tests per node (pure functions, table-driven).

---

## Phase 2 — Format & utility (make Synapse/Myelin actually useful)

Synapse can *fetch* and Myelin can *store*, but nothing **parses or transforms** the
payload. These turn raw bytes into structured data.

| # | Node (type key) | displayName | Effort | Crate dep |
|---|---|---|---|---|
| 2.1 | `dateTime` | Chronon (Date & Time) | S | none — `chrono`/`chrono-tz` in tree |
| 2.2 | `crypto` | Enzyme (Crypto) | S | none — `sha2`/`hmac`/`uuid` in tree |
| 2.3 | `htmlExtract` | Retina (HTML Extract) | M | `scraper` |
| 2.4 | `extractFromFile` | Digest (Extract from File) | M | `csv`, `calamine` (xlsx) |
| 2.5 | `convertToFile` | Convert to File | M | `csv` |
| 2.6 | `compression` | Compression (zip/gzip) | S | `zip` (gzip likely in tree) |
| 2.7 | `xml` / `markdown` | XML / Markdown | S | `quick-xml`, `pulldown-cmark` |
| 2.8 | `pdfText` | PDF Text | M–L | see warning — demand-driven |

- [x] **2.1 Date & Time** (`dateTime` / *Chronon*) — parse/format/add/subtract/
  diff/extract; timezone-aware. Executor `nodes/date_time.rs` (18 table-driven
  tests) is a thin config layer over **`axon_core::flexidate`** (universal datetime
  reconciliation, already powers both Calendar integrations) for parsing, plus
  `chrono`/`chrono-tz` (both in tree) for arithmetic, formatting, and zone
  conversion. Five `operation`s: `getCurrentDate` (now, optionally date-only),
  `format` (presets — ISO/date/time/datetime/human/RFC2822/unix/unixMs — or a
  custom strftime string, pre-validated so a bad token errors instead of
  panicking), `addSubtract` (calendar-aware for months/quarters/years — chrono
  clamps day-of-month, e.g. Mar 31 − 1mo = Feb 28; duration-based & fractional for
  weeks→seconds), `diff` (whole calendar months/quarters/years via
  `full_months_between`; fractional for smaller units), `extract`
  (year/month/day/hour/minute/second/ISO-weekday/dayOfYear/ISO-week/quarter → a
  number). Input values keep their JSON type through `interpolate_config`, so a
  Unix-timestamp number parses as readily as a string. A `timezone` (IANA, default
  `flexidate::default_tz` = Asia/Manila) anchors naive/date-only inputs and
  converts zoned ones. Output mirrors Soma: result lands under `outputField`
  (per-op default) and `includeInputFields` merges it onto the incoming item.
  Dispatch uses the Soma/`$json` primary-input convention; not in the no-retry list
  (pure transform). `NODE_TYPES.dateTime` in `nodes.js` gates each operation's
  params via `displayOptions`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + backend/UI
    build.
- [x] **2.2 Crypto** (`crypto` / *Enzyme*) — hash / HMAC / UUID. Executor
  `nodes/crypto.rs` (12 table-driven tests incl. NIST/RFC vectors). **Zero new
  deps** — reuses `sha2`/`hmac`/`hex`/`base64`/`uuid`, the same crates that back
  the master-key crypto and the GitHub/Facebook webhook signature checks. Three
  `operation`s: `hash` (digest a value), `hmac` (keyed HMAC with a secret — the
  "sign" side of webhook verification: compute it, compare to the provider header
  with an IF node), `generateUuid` (v4). Algorithm ∈ SHA-224/256/384/512
  (name-normalized so "SHA-256"=="sha256"); output encodes as `hex` (default —
  GitHub/Stripe), `base64` (Shopify), or `base64url`. Values coerce via
  `val_to_string` so a number hashes as its plain string. Asymmetric-key signing
  (RSA/ECDSA) is deliberately out of scope — it needs a new crate, and the plan
  pins this to zero deps. Output mirrors `dateTime`/Soma (`outputField` +
  `includeInputFields`). Dispatch uses the Soma/`$json` primary-input convention;
  not in the no-retry list (pure transform). `NODE_TYPES.crypto` in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + backend/UI
    build.
- [x] **2.3 HTML Extract** (`htmlExtract` / *Retina*) — CSS-selector extraction →
  turns "Synapse fetch a page" into real **web scraping**. Executor
  `nodes/html_extract.rs` (15 table-driven tests) over `scraper 0.27`
  (`default-features = false` — compile-heavy but runtime-light, no TLS/HTTP of
  its own, per dependency policy). Each extraction rule: `key` + `cssSelector` +
  `returnValue` (`text` — whitespace-collapsed under `trimValues` — inner `html`,
  or an `attribute`; elements missing the attribute are skipped, not null holes)
  + `returnArray` (first match vs all matches). HTML source: the `html` config
  expression, falling back to the primary input — a string as-is, or its
  `body`/`html`/`data`/`text` field, so a raw Synapse response works unconfigured.
  Output is ONE object of all extraction keys (a page reduces to one item);
  `includeInputFields` merges onto the incoming item (Soma/`dateTime`/`crypto`
  convention). Missing rules / key / selector / attribute name are teaching
  errors; invalid selectors error with the key + selector named. Dispatch uses
  the Soma/`$json` primary-input convention; not in the no-retry list (pure
  transform). `NODE_TYPES.htmlExtract` in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + build.
- [x] **2.4 Extract from File** (`extractFromFile` / *Digest*) — **CSV /
  spreadsheet → JSON**. Executor `nodes/extract_from_file.rs` (20 table-driven
  tests, incl. a hand-crafted in-test XLSX fixture) over `csv 1.4` +
  `calamine 0.36` (`default-features = false`, `dates` feature reuses in-tree
  chrono; both pure Rust — no new TLS/HTTP stack). Three byte `source`s: `file`
  (path; blank auto-detects the standard binary descriptor `binary.local_path`
  that Myelin retrieve / Telegram download / Synapse file responses emit),
  `text` (raw CSV — how a `text/csv` HTTP fetch arrives, since Synapse returns
  text bodies as strings; spreadsheet+text is a teaching error), and `base64`
  (line-wrap tolerant). CSV: `delimiter` (with `tab`/`\t` alias), BOM strip,
  lossy-UTF-8 byte records (Latin-1 exports don't fail), flexible/ragged rows,
  blank-line skip, optional `inferTypes` (numbers/bools; leading-zero IDs stay
  text). Spreadsheet: XLSX/XLS/XLSB/ODS via `open_workbook_auto_from_rs` format
  sniffing, `sheetName` (blank = first; unknown sheet errors listing the real
  ones), typed cells (integral floats → ints, dates → naive ISO strings, error
  cells surface "#DIV/0!" text). Shared: `headerRow` (blank/duplicate headers →
  `column_N`/`name_2`), `maxRows` cap. **Output is a bare array of row items**
  (objects, or arrays when `headerRow` off) — the list-node convention, so it
  composes with Filter/Aggregate/Split Out/Sort-Limit/Loop directly. Dispatch
  uses the Soma/`$json` primary-input convention; `NODE_TYPES.extractFromFile`
  in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + build.
- [x] **2.5 Convert to File** (`convertToFile`) — **JSON → CSV / JSON / text /
  binary file**, the inverse of Digest. Executor `nodes/convert_to_file.rs`
  (15 table-driven tests). **Zero new deps** — `csv` is already in tree (2.4);
  JSON/text/base64 are std + serde. Four `operation`s: `csv` (a list of items →
  one row each: object items keyed by a first-seen header union with missing
  fields as empty cells, scalar items in a `value` column, a list of arrays
  written positionally with no header; `delimiter` with the `tab` alias,
  `headerRow`, optional UTF-8 `bom` so Excel opens non-ASCII text), `json`
  (pretty by default, compact via `pretty: false`), `text` (a string as-is; a
  list joins one item per line), `fromBase64` (n8n's "move base64 string to
  file" — line-wrap tolerant, optional `mimeType`, default octet-stream).
  Source is the `data` expression, falling back to the primary input
  (list-node convention; `arrayPath` unwraps wrappers; empty list → empty
  file, Null input → teaching error). Bytes stage via `files::stage_bytes`
  (same-named file overwritten — newest only) and the **output mirrors Myelin
  store/retrieve**: file facts + the standardized `binary` descriptor (both
  key conventions), so Telegram send, Gmail attachments, SSH/Drive/OneDrive
  uploads and Myelin consume it directly. `fileName` sanitized with a per-op
  default; a missing extension is auto-appended (except fromBase64). **XLSX
  output deliberately deferred** — writing it needs a new crate
  (`rust_xlsxwriter` + `zip`) and CSV already opens in Excel; build it
  alongside 2.6 Compression's `zip` when a workflow actually needs real
  sheets. Dispatch uses the Soma/`$json` primary-input convention; not in the
  no-retry list (idempotent overwrite). `NODE_TYPES.convertToFile` in
  `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests +
    backend/UI build.
- [x] **2.6 Compression** (`compression`) — zip/unzip/gzip/gunzip. Executor
  `nodes/compression.rs` (11 table-driven tests). **Zero *new* deps** — `zip`
  and `flate2` were already resolved transitively (`zip` via calamine's XLSX
  reader in 2.4, `flate2` via `zip`'s deflate backend and reqwest's response
  decompression); promoted to direct deps at the same versions/backend
  (`zlib-rs`, pure Rust) so `cargo tree` shows no new compile weight. Four
  `operation`s: `zip` (bundle the primary input — a single item or an array —
  into one archive; each item resolves via its binary descriptor when
  present, else a string writes as text and anything else as compact JSON;
  duplicate entry names get a numbered suffix so nothing silently
  overwrites), `unzip` (explode an archive into its entries, each staged and
  returned as **a bare array** of file descriptors — the list-node
  convention, so it composes with Loop/Filter/Split Out), `gzip` (compress a
  single value — a staged file, string, or JSON — embedding the original
  file name in the gzip header like the standard CLI, so `gunzip` recovers it
  with zero config), `gunzip` (decompress, recovering the embedded/derived
  name unless `fileName` overrides it). `unzip`/`gunzip` share Extract from
  File's `file`/`base64` source convention (auto-detect the binary descriptor,
  explicit `filePath`, or `data`); no `text` source since archive/gzip bytes
  are never meaningfully raw text. Dispatch uses the Soma/`$json` primary-input
  convention; not in the no-retry list (pure transform). `NODE_TYPES.compression`
  in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests + build.
- [x] **2.7 XML / Markdown** — two node types, built together.
  - **`xml`** — Executor `nodes/xml.rs` (21 table-driven tests) over `quick-xml`
    (new dep, `default-features = false`, no TLS/HTTP of its own). Uses the
    fast-xml-parser convention so the two operations round-trip: attributes
    become `@_name` keys, text becomes `#text` (only when mixed with
    attrs/children — a pure text leaf is just a plain string, an empty leaf is
    `""`), repeated sibling tags group into an array. `xmlToJson` parses into
    `{ <rootTag>: value }` and merges onto the incoming item like `htmlExtract`
    (`includeInputFields`, root key wins on conflict); source is the `xml`
    field falling back to the primary input (same body/xml/data/text probing
    as `htmlExtract`). `jsonToXml` serializes back to a string: a single-key
    object's key becomes the root tag (the round-trip case), anything else
    wraps under `rootName` (default `root`); a bare array at the root wraps as
    an `item` child so the output is always one well-formed root element.
    `pretty` (indented, default on) and `declaration` (default on) are
    configurable; output lands under `outputField`, `dateTime`/`crypto`'s
    convention. Malformed XML (mismatched tags) surfaces quick-xml's own error
    instead of panicking.
  - **`markdown`** — Executor `nodes/markdown.rs` (16 table-driven tests).
    `toHtml` uses `pulldown-cmark` (new dep, `features = ["html"]` only,
    `default-features = false`) — CommonMark plus optional GFM tables/
    strikethrough/tasklists/footnotes (`gfm` toggle, default on) — a correct,
    well-tested renderer, so that direction is full-fidelity. `toMarkdown` (the
    harder, unconstrained direction) walks the DOM with `scraper` — already in
    tree for 2.3, so the only new crate this adds is `ego-tree` (`scraper`'s
    own tree type, already pulled in transitively; just named directly since
    Rust requires that for a type used by path) — mapping the common tags
    (headings, paragraphs, `**bold**`/`_em_`, links, images, `-`/`1.` lists,
    inline/fenced code with whitespace preserved verbatim, blockquote, hr, br).
    Unknown tags pass through as transparent containers (their text still
    surfaces); `script`/`style`/`head` are dropped; HTML formatting whitespace
    between block elements collapses away. Explicitly scoped as a **pragmatic
    converter, not spec-complete** — good enough for message bodies and
    scraped content (the plan's actual use case: Telegram/Discord/email/Slack
    round-tripping), not a full HTML-to-Markdown engine. Both operations share
    the `outputField`/`includeInputFields` convention.
  - Both dispatch through the Soma/`$json` primary-input convention; neither is
    in the no-retry list (pure transforms). `NODE_TYPES.xml` and
    `NODE_TYPES.markdown` in `nodes.js`.
  - Remaining DoD item: manual canvas E2E; logic covered by unit tests +
    backend/UI build.
- [ ] **2.8 PDF Text** — split out from 2.4 deliberately: Rust PDF text extraction
  (`pdf-extract`, `lopdf`) is **flaky on real-world PDFs** (panics, garbled text on
  scanned/complex layouts). Own line item so it can't stall the spreadsheet path;
  build only when a workflow actually needs it, and wrap extraction so a bad PDF
  fails the item, not the process.

---

## Phase 3 — Workflow-as-API & generic email

**Corrected premise:** Axon is *not* email-less. It already has a **Gmail trigger**
(`execute_gmail_trigger` + the `check_and_trigger_gmail` background poller, wired
into Stimulus with label/subject/body queries + attachment download) and
**`gmail_send`** as a registered tool callable from the tool node. What's actually
missing: **custom HTTP responses** (workflow-as-API) and **non-Google mailboxes**.
Priority follows accordingly.

| # | Node (type key) | displayName | Effort | Notes |
|---|---|---|---|---|
| 3.1 | `respondToWebhook` | Efferent (Respond) | M | the real Phase-3 gem |
| 3.2 | `email` (send) | Axon Terminal (Email) | M | SMTP via `lettre` — **rustls** |
| 3.3 | `rss` | RSS Read | S | `feed-rs` |
| 3.4 | `emailTrigger` | Email Trigger (IMAP) | L | demand-driven — see note |
| 3.5 | `sms` | SMS (Twilio) | S | HTTP wrapper, low priority |

- [x] **3.1 Respond to Webhook** (`respondToWebhook` / *Efferent*) — a workflow
  answers the live HTTP request that triggered it with its own status/headers/
  body, so a workflow can **be an API**. Executor `nodes/respond_to_webhook.rs`
  (13 table-driven tests) + a run-id-keyed oneshot registry mirroring
  `trigger_data` (registered in `run_in_background_inner` BEFORE spawn — same
  invariant as payload staging — so the node can't race the registration; a
  `ChannelCleanup` RAII guard in `run_inner` drops an unfired sender on any
  exit/suspend path so the caller falls back instantly). Handler side
  (`webhook/external.rs`): a delivery whose workflow has an enabled
  `respondToWebhook` node runs via `run_in_background_for_webhook` and holds
  the request open up to `workflow.webhook_respond_timeout_secs` (default 30s,
  new RuntimeSettings accessor); respond fired → custom response, channel
  closed/timeout → the legacy `{ok, run_id}` ack, and the run continues either
  way. Modes: `firstIncomingItem` (default — echoes the primary input, first
  element of a list), `json` (string body must parse; expression-resolved
  objects ride as-is), `text`, `noData`; `statusCode` validated 100–599;
  `responseHeaders` fixedCollection can override the by-body-kind content-type.
  One-shot by construction: first respond wins, a second (or a manual editor
  run) reports `responded: false` + preview instead of erroring. In the
  no-retry list AND the `can_iterate` exclusion (a per-item map after a Loop
  would burn the channel on item 0). `NODE_TYPES.respondToWebhook` in
  `nodes.js`. Unlocks: form backends, Slack slash-command responses,
  signed-webhook handshakes (pairs with 2.2 Crypto).
  - Remaining DoD item: manual canvas E2E (curl an external-webhook workflow
    with/without the node); logic covered by unit tests + backend build.
- [ ] **3.2 Send Email (SMTP)** — for non-Gmail/transactional senders.
  Credential-backed. `lettre` with `default-features = false` +
  rustls transport (per dependency policy). Build when a concrete non-Gmail sender
  shows up — Gmail sending already works via the tool node.
- [ ] **3.3 RSS Read** — feed monitoring.
- [ ] **3.4 Email Trigger (IMAP)** — **demoted to demand-driven** (was priority 2):
  the Gmail trigger already covers most inbound-email automation, and this is the
  plan's only L-effort item. Build only when a real non-Gmail mailbox shows up.
  When it does: `async-imap` with rustls, integrate as a new Stimulus source
  alongside the Gmail poller (same background-poll pattern). Pairs with the
  Classifier.
- [ ] **3.5 SMS/Twilio** — optional; thin wrapper over the shared HTTP client.

---

## Phase 4 — AI extensions (build on Cortex/Classifier/Qdrant)

Small additions that meaningfully extend the agent layer you already have.

| # | Node (type key) | displayName | Effort | Notes |
|---|---|---|---|---|
| 4.1 | `informationExtractor` | Extractor | M | schema-guided JSON out |
| 4.2 | `vectorStore` | Neocortex (RAG) | L | embed → upsert → semantic search |
| 4.3 | `summarize` / `sentiment` | Summarize / Sentiment | S | LLM presets |

- [ ] **4.1 Information Extractor** — schema-guided structured JSON extraction.
  Classifier only *tags*; this *pulls fields*. Reuse the Cortex/Classifier LLM path
  and set `expects_structured_output` (the raw-JSON loop guard rejects structured
  node output otherwise — established pattern).
- [ ] **4.2 Vector Store / RAG node** — the `qdrant/` folder exists but Engram is
  key-value, not semantic. A first-class **embed → upsert → semantic-search** node
  makes retrieval a workflow step. Reuse the provider-configurable embedder.
- [ ] **4.3 Summarize / Sentiment** — thin LLM presets over the Cortex path (also
  set `expects_structured_output` where output is JSON).

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

1. **Multi-input plumbing + Merge** (1.0, 1.1) — unlocks fan-out/fan-in; nothing
   else compares in leverage.
2. **Filter / Aggregate / Split Out / Sort-Limit** (1.2–1.5) — finish the list
   toolkit while the array convention and test harness are fresh; all S/M.
3. **Respond to Webhook** (3.1) — workflows become APIs; plumbing already exists.
4. **Date & Time + Crypto** (2.1, 2.2) — zero new deps, `flexidate` does the heavy
   lifting; removes the "drop to JavaScript" tax almost for free.
5. **HTML Extract + Extract from File** (2.3, 2.4) — scrape-then-process.
6. **Email (3.2 / 3.4) only when a non-Gmail need actually appears**; everything
   else as demanded.

## Per-node Definition of Done
- [ ] Executor file + unit tests (pure logic table-driven like `condition.rs`).
- [ ] Registered in `nodes/mod.rs` + dispatch arm in `workflow.rs`.
- [ ] `NODE_TYPES` entry in `nodes.js` (icon, description, properties).
- [ ] No-retry list updated if it's a control/branch node.
- [ ] New crates comply with the dependency policy (rustls-only,
      `default-features = false`); spot-check with `cargo tree`.
- [ ] Manual run in the canvas exercising the node end-to-end.
- [ ] `graphify update .` run to refresh the knowledge graph.
- [ ] Committed + pushed to `main`.
