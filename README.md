# Axon 🧠

> A highly autonomous, multi-platform AI agent written entirely in **async Rust** (`tokio`) with a **Vue 3** dashboard. It answers and acts across your messaging channels, runs real tools, routes LLM requests across many providers with automatic fallback, and remembers context via SQLite + Qdrant.

**Version 0.4.0** · Rust edition 2021 · GNU AGPL v3.0

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](./LICENSE)
[![Vue 3](https://img.shields.io/badge/ui-Vue_3-42b883.svg)](https://vuejs.org/)

Axon is a single self-contained process that acts as the operating intelligence for your digital life. It bridges raw LLM power and real-world action: reading and replying to messages, executing shell commands and SSH, calling Gmail/Calendar/Drive/Outlook/Instagram/CRM, running scheduled jobs, watching sources for changes, and chaining those steps together in visual workflows — all behind a live web dashboard.

This document is the complete guide: architecture, prerequisites, installing or building it, configuration, the dashboard, the model router, memory, tools, integrations, scheduling, workflows, security, deployment, cutting releases, and troubleshooting.

---

## Table of Contents

- [🌟 Highlights](#-highlights)
- [🏗 Architecture](#-architecture)
- [📦 Repository Layout](#-repository-layout)
- [✅ Prerequisites](#-prerequisites)
- [🚀 Quick Start](#-quick-start)
- [⚙️ Configuration](#️-configuration)
- [🧠 The Agent Reasoning Loop](#-the-agent-reasoning-loop)
- [🔀 The Model Router](#-the-model-router)
- [🗄 Memory](#-memory)
- [🛠 Tools & Integrations](#-tools--integrations)
- [💬 Messaging Channels](#-messaging-channels)
- [⏰ Scheduler, Watchers & Workflows](#-scheduler-watchers--workflows)
- [🔒 Security](#-security)
- [📡 HTTP & WebSocket API](#-http--websocket-api)
- [🌐 The Dashboard](#-the-dashboard)
- [📦 Production Deployment](#-production-deployment)
- [🧰 Development](#-development)
- [🚢 Releasing](#-releasing)
- [🩹 Troubleshooting](#-troubleshooting)
- [📄 License](#-license)
- [📚 Further Reading](#-further-reading)

---

## 🌟 Highlights

- **One process.** The agent, the dashboard, the webhooks, the scheduler, and **all** integration tools (Google, Microsoft, Facebook, Instagram, CRM) run in-process — no separate server, no SSE hop.
- **Multi-channel.** Telegram, Discord, Slack, Facebook/Messenger, WhatsApp, and a built-in web chat — all out of the box.
- **Resilient model router.** Priority tiers, sticky routing, round-robin within a tier, automatic 429-quarantine with cooldown, and a last-resort `paid_model` fallback. Requests don't drop.
- **Deep, layered memory.** Short-term SQLite history per session, long-term Qdrant vector recall (always presented as *hints to verify with tools*, never ground truth), and compressed searchable tool observations.
- **Real tools.** Shell, SSH, HTTP, web search, image generation/processing, parallel workers, cron jobs, watchers, and workflows.
- **Self-correcting.** A validation pipeline (claim guard, refusal nudge, blank/raw-syntax guards, and an optional secondary-LLM quality check) catches hallucinations and tool misuse before anything is sent publicly.
- **Live dashboard.** Mobile-friendly Vue 3 UI for chat, models, tools, services, memory, files, tasks, watchers, and the visual workflow canvas — all changeable live without a restart. Installable as a phone app (PWA).
- **Secure by design.** Bearer master-key auth with brute-force throttling, fail-closed AES-256-GCM encryption of all secrets at rest, path-traversal protection, and HMAC-verified webhooks. The shell tool deliberately runs as the Axon process user — **run Axon unprivileged**; see [Security](#-security).

---

## 🏗 Architecture

```
                       ┌──────────────────────────────────────────┐
   Browser ── HTTP/WS ─▶│  axon-agent  (Axum, default port 3000)   │
   Telegram/Discord ───▶│   • Dashboard UI (static, from axon-ui)  │
   Slack webhook ──────▶│   • REST API  /api/*                     │
   Facebook webhook ───▶│   • WebSocket /ws  (live logs + chat)    │
                        │   • Agent loop, router, memory           │
                        │   • Scheduler, Watcher, Workflows        │
                        └───────┬───────────────┬──────────────────┘
                                │               │
                 SQLite (memory/axon.db)   In-process integration tools
                                │               │
                          Qdrant (vectors)  Google / Microsoft / Facebook / CRM
                                                │
                          LLM providers ◀───────┘  (Gemini, Anthropic, Groq,
                          (via model router)        Cerebras, NVIDIA, OpenRouter,
                                                     OpenAI-compatible, Ollama, …)
```

The agent is the only process users talk to. It owns the dashboard, webhooks, the reasoning loop, **and** the integration tools themselves (Gmail, Calendar, Drive, OneDrive, Outlook, Facebook pages, Instagram, CRM, etc.), which run in-process — the integrations are compiled directly into the agent (no second process, no SSE hop, no extra port). Axon can still connect to **external** MCP servers you add from the dashboard's Services/MCP page (those continue to use SSE). **Qdrant** is the only external dependency, used for long-term semantic memory.

---

## 📦 Repository Layout

| Path | What it is |
|------|-----------|
| `Cargo.toml` | **Cargo workspace root** — ties every Rust crate under `crates/` into one build (a single `target/` and `Cargo.lock`). Shared build profiles live here. |
| `crates/axon-agent/` | **The core agent + web dashboard** (Rust, package `axon`). The main binary you run: HTTP API, dashboard UI, WebSocket stream, webhooks, scheduler, watcher, workflows, and the agent reasoning loop. |
| `crates/axon-core/` | Shared types, OAuth token store, credentials handling used by the integration crates. |
| `crates/axon-google/` · `axon-microsoft/` · `axon-facebook/` · `axon-instagram/` · `axon-business/` · `axon-crm/` | **Integration tool crates** compiled **directly into `axon-agent`** as path dependencies (Google, Microsoft, Facebook, Instagram, CRM, and business/utility tools). |
| `crates/axon-image/` | Image-processing/generation library (`image_processor`) used by the agent's `image_tool`. |
| `axon-ui/` | **Web dashboard** (Vue 3 + Vite + Vue Flow). Built to static files and served by `axon-agent`. |
| `qdrant/` | Deployment scripts for the Qdrant vector DB (systemd units, backup/health/trim timers, collection setup). |
| `scripts/` | Release packaging (`package-release.sh`/`.ps1`) and end-user install scripts (`install.sh`/`install.ps1`). |
| `deployaxongcp.sh`, `deploycham*.sh`, `deployfrontend.sh` | Production build + deploy scripts (build release binaries, bundle, ship to the server). |
| `run.bat` | One-click local Windows build-and-run (builds UI, copies static, runs the agent). |
| `crates/axon-agent/config/models.toml` | **Model catalog** — the source of truth for which LLMs Axon can use. |
| `crates/axon-agent/memory/schema.sql` | SQLite schema applied on startup. |

The various `.zip`/`.tar.gz` files you may see after building are release/deploy archives and can be ignored for day-to-day operation.

---

## ✅ Prerequisites

- **Rust toolchain** 1.70+ (a current stable toolchain; `cargo`).
- **Node.js 18+** and npm (only needed to build the dashboard UI).
- **SQLite3** — bundled via the Rust crate; no separate install required for the agent.
- **Qdrant** — a running instance for long-term memory. Optional for a first run (the agent starts fine without it; semantic recall is simply disabled), but required for the full memory feature. See [`qdrant/README.md`](./qdrant/README.md) and `qdrant/install.sh`.
- API keys for at least one LLM provider (see [Configuration](#️-configuration)).

---

## 🚀 Quick Start

There are two ways to get Axon running: install a prebuilt release (fastest, no toolchain needed), or build from source (for development).

### Option A — Install a prebuilt release

On first install this **auto-generates a valid `AXON_MASTER_KEY`** (boot refuses a blank/placeholder key) and prints it — save it.

**Linux (Debian/Ubuntu)** — straight from GitHub:

```bash
curl -fsSL https://github.com/orapagier/axon/releases/latest/download/install.sh | bash
```

Installs a `systemd` service (`axon-agent`), optionally installs Qdrant, and starts it. Options: `--dir PATH`, `--version vX.Y.Z`, `--file PATH`, `--with-qdrant` / `--no-qdrant`, `--no-service`.

**Windows** — in PowerShell:

```powershell
irm https://github.com/orapagier/axon/releases/latest/download/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\Axon`, registers a hidden **Startup** launcher (runs on login), and starts it. Options: `-Dir PATH`, `-Version vX.Y.Z`, `-File PATH`, `-NoStartup`, `-NoStart`.

Either installer also accepts a local artifact already downloaded into the current directory instead of fetching from GitHub (see [Releasing](#-releasing) for the artifact names).

After install, open **http://localhost:3000**, add at least one LLM provider key to `.env` (`core/.env` on Linux, the install dir on Windows), and restart Axon.

### Option B — Build from source

The fastest path on Windows:

```bat
run.bat
```

`run.bat` will:

1. `npm install` (if needed) and `npm run build` inside `axon-ui/`.
2. Copy `axon-ui/dist/*` into `crates/axon-agent/static/`.
3. Stop any running `axon` process, then `cargo run` the agent.

Manual equivalent (any OS):

```bash
# 1) Build the dashboard
cd axon-ui
npm install
npm run build

# 2) Make the built UI available to the agent
#    (the agent serves ./static as the dashboard)
mkdir -p ../crates/axon-agent/static
cp -r dist/* ../crates/axon-agent/static/

# 3) Run the agent
cd ../crates/axon-agent
cargo run            # or: cargo run --release   (run from anywhere in the workspace)
```

Then open **http://localhost:3000**.

> The agent serves the UI from `crates/axon-agent/static/`. If you change UI code, rebuild the UI and re-copy `dist/` into `static/` — this is exactly what `run.bat` automates.

The Google/Microsoft/Facebook/Instagram/CRM tools are built into the agent and start automatically — **no second process to run.** OAuth happens from the dashboard's **Services** page; the callback returns to the agent itself (`/auth/:service/callback`).

> For OAuth redirects and Instagram media URLs to resolve correctly, set `AXON_PUBLIC_BASE_URL` (or `AXON_CALLBACK_HOST`) to the agent's public base URL — or set `instagram.public_base_url` on the Settings page. `credentials.json` (your OAuth app client IDs/secrets) must sit in the agent's working directory or its local data directory.

---

## ⚙️ Configuration

Axon reads configuration from three layers, in increasing precedence:

1. **`config/models.toml`** — the model catalog (source of truth for models).
2. **Environment variables / `.env`** — secrets and process-level settings.
3. **The dashboard settings + database** — runtime settings changeable live without a restart.

### Environment variables

Loaded automatically from (in order) `$AXON_ENV_FILE`, the working-directory `.env`, and `.env`/`axon-agent.env` next to the executable.

| Variable | Default | Purpose |
|----------|---------|---------|
| `AXON_PORT` | `3000` | Dashboard / API / webhook listen port. |
| `AXON_DB_PATH` | `memory/axon.db` | SQLite database path. |
| `AXON_MASTER_KEY` | *(unset)* | **Dashboard auth + secret-encryption key.** Boot refuses to start without it unless `AXON_DEV=1`. See [Security](#-security). |
| `AXON_MASTER_KEY_OLD` | *(unset)* | Previous master key — set for **one boot** to rotate all stored secrets onto a new `AXON_MASTER_KEY`, then remove it. See [Security](#-security). |
| `AXON_DEV` | *(unset)* | Set to `1` to allow boot with no/weak `AXON_MASTER_KEY` (falls back to a public, insecure key). Local development only. |
| `AXON_ENV_FILE` | *(unset)* | Explicit path to an env file to load first. |
| `AXON_PUBLIC_BASE_URL` / `AXON_CALLBACK_HOST` | *(unset)* | Public base URL for OAuth redirects & Instagram media. |
| `QDRANT_URL` | *(provider default)* | Qdrant endpoint for long-term memory. |
| `VOYAGE_API_KEY` | *(unset)* | Embedding key used by the memory store. |
| `TELOXIDE_TOKEN` | *(unset)* | Telegram bot token (or `messaging.telegram_token` in settings). |
| `DISCORD_TOKEN` | *(unset)* | Discord bot token (or `messaging.discord_token`). |
| `SLACK_BOT_TOKEN` | *(unset)* | Slack bot token (or `messaging.slack_token`). |
| `AXON_EXPR_ENV` | *(unset)* | Comma-separated allowlist of process env vars exposed to workflow expressions (`$env`). See [Scheduler, Watchers & Workflows](#-scheduler-watchers--workflows). |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | *(unset)* | If set, enables OpenTelemetry tracing export. |
| `RUST_LOG` | `axon=info` | Standard `tracing` log filter. |
| `AXON_LOG_FORMAT` | *(unset)* | Set to `json` for structured JSON log lines (one object per line — easy to ship to a log aggregator). Unset stays human-readable, for local `cargo run`. |
| *(provider keys)* | — | Referenced from `models.toml` as `${NAME}` placeholders, e.g. `GEMINI_API_KEY_*`, `GROQ_*`, `CEREBRAS_*`. |

### The model catalog — `config/models.toml`

On every startup, Axon **syncs `models.toml` into the database as the source of truth**: models in the file are upserted, and any model in the DB that is *not* in the file is deleted. Edit the TOML, restart, and the dashboard reflects it.

Each `[[models]]` entry:

```toml
[[models]]
name        = "gemini-flash"          # unique display name
provider    = "google"                # google | anthropic | groq | cerebras |
                                       # openai/openai_compat | ollama | nvidia | openrouter
model_id    = "gemini-2.5-flash"      # provider model id. Comma-separated = per-model fallback
api_key     = "${GEMINI_API_KEY}"     # literal key, or ${ENV_OR_SETTING} placeholder
base_url    = "https://..."           # optional; required for OpenAI-compatible providers
role        = ""                      # "" = general pool. See roles below
priority    = 1                       # lower number = higher priority tier
timeout_secs = 60                     # optional per-model call timeout
max_tokens  = 4096                    # optional (defaults to 4096)
enabled     = true                    # optional (defaults to true)
```

**Roles** route specific kinds of work to specific models:

| Role | Used for |
|------|---------|
| `""` (empty) | General pool — the default for every request, regardless of task complexity. |
| `router` | Small model used to convert natural-language schedules to cron, etc. |
| `quality_checker` | Secondary model that audits the main agent's output. |
| `axon_node` | Model used by the "Axon" node inside Workflows. |
| `image_model` | Vision-capable model for the Cortex node's Image mode. **Strict** — never falls back to general/paid_model. |
| `paid_model` | **Last-resort** fallback, only tried after every free option is exhausted. |

**`model_id` fallback chains:** a comma-separated `model_id` (e.g. `"gpt-oss-120b,gpt-oss-20b"`) lets one entry try several upstream model IDs in order before the record is considered failed.

### Runtime settings (dashboard / DB)

Most behavior is tunable live via the **Settings** page (stored in the `settings` table). Notable keys and their defaults:

- `agent.max_iterations` (`20`), `agent.max_parallel_tools` (`5`), `agent.tool_timeout_secs` (`30`)
- `agent.run_timeout_secs` (`300`) — total wall-clock budget for one run; the model-failover chain is bounded only by this.
- `agent.quality_check` (`true`), `agent.allow_tool_writing` (`true`), `agent.temp_tool_max_retries` (`2`)
- `agent.system_prompt` — the master system prompt (editable live). Ends with a `SPIRITUAL & BIBLICAL QUESTIONS` worldview section; `normalize.sql` re-appends it on boot if deleted, so edit its wording in place instead.
- `router.error_threshold` (`2`, consecutive errors before a model is parked until midnight), `router.model_call_timeout_secs` (`30`, flat per-attempt timeout; overridable per model).
- `memory.short_term_max_msgs` (`50`), `memory.long_term_top_k` (`5`).
- `websearch.enabled`, `websearch.max_results`.
- `watcher.user_name`, `watcher.user_title`, `watcher.quiet_hours_*`.
- `scheduler.nudge_prompt` — template for scheduled reminders.
- `instagram.*` — public base URL, bind address, and media-poll timeouts for IG publishing.

---

## 🧠 The Agent Reasoning Loop

What happens on each request:

1. **Context build** — loads short-term history for the session, searches long-term memory and recent tool observations (skipped for pure greetings), and routes an initial tool set.
2. **Iterate** (bounded by `agent.max_iterations` and `agent.run_timeout_secs`):
   - Pick a model by role/priority (see [Model Router](#-the-model-router)), with **sticky routing** (stays on the model that worked last turn to avoid tool-format drift).
   - Call the LLM with the filtered tool set and a flat per-attempt timeout.
   - If the model emits tool calls, execute them (up to `agent.max_parallel_tools` in parallel) and feed results back.
   - If the model answers, run the **validation pipeline**.
3. **Validation pipeline** (cheapest checks first):
   - *Claim guard* — blocks "I sent/created/deleted…" claims that have no matching successful tool-execution receipt.
   - *Refusal nudge* — if a tool was available but the model said "I can't", it's nudged to actually use the tool.
   - *Blank-response* and *raw-tool-syntax* guards.
   - *Quality check* — an optional secondary LLM (`quality_checker` role) audits tool-backed answers and can request a revision (up to 3 times).
4. **Finalize** — strips reasoning/markdown noise, resolves `<send_file>…</send_file>` tags into downloads/attachments, persists the run, and streams the answer.

---

## 🔀 The Model Router

- **Priority tiers:** models are grouped by `priority` (lower = higher). Within a tier the pick is a fair round-robin / pseudo-random rotation across distinct provider/model endpoints — no rate-limit-headroom steering; a model is only ever skipped when it's actually unavailable.
- **Fallback order per request:** preferred model (if the caller picked one) → sticky model → role pool → general pool → a sweep over any remaining free model → `paid_model` last. Failover is **immediate** — on an error, 429, or per-attempt timeout the router moves straight to the next available model with no wait.
- **Rate-limit quarantine:** a `429`/quota error parks the model for a window-based cooldown and routing falls through to the next option without dropping the request. An explicit provider reset always wins when given (a `Retry-After` header, Gemini's `retryDelay`, or an inline "try again in …"); otherwise a flat default per window:
  - *per-minute* limit → 60 s,
  - *per-hour* limit → 60 min,
  - *daily* quota exhausted → until the window resets (provider reset if given, else next UTC midnight).

  A non-rate-limit error parks the model until midnight after `router.error_threshold` consecutive failures. A successful call reinstates the model immediately; cooldowns otherwise auto-expire.
- **Editing models keeps live state:** adding, editing, or deleting a model on the Models page reloads the router from the database. Active 429 cooldowns and the lifetime call/token counters **survive** that reload, so a routine config tweak no longer un-parks every rate-limited provider or zeroes the usage figures. Changing a model's provider, model ID, base URL, or API key is treated as *fixing* it and does clear its health state — the lifetime counters still carry over. To clear a cooldown deliberately, use the per-model reset on the Models page.
- **Flat per-attempt timeout:** each call gets `router.model_call_timeout_secs` (default 30 s, overridable per model), bounded only by the overall run deadline — no adaptive/fair-share math.
- **Alerts:** rate-limit, timeout, and "paid fallback used" events are collected per run and surfaced to the operator (dashboard + messaging notifications).

Supported providers: **Google Gemini, Anthropic, Groq, Cerebras, NVIDIA, OpenRouter, OpenAI / OpenAI-compatible, Ollama.**

---

## 🗄 Memory

- **Short-term (SQLite):** the last N messages per `session_id`/`chat_id` (`memory.short_term_max_msgs`, default 50). Reconstructed into the prompt each run.
- **Long-term (Qdrant):** useful, tool-backed results are embedded and stored for semantic recall on future requests (`memory.long_term_top_k` results injected as *hints*). Requires Qdrant + an embedding key (`VOYAGE_API_KEY`).
- **Tool observations:** large tool outputs are compressed and stored, searchable via SQLite FTS, and recent ones are surfaced as context.

Memories are always presented to the model as **hints to verify with tools**, never as ground truth — this is enforced in the system prompt and the claim guard.

---

## 🛠 Tools & Integrations

### Built-in tools (in `axon-agent`)

| Tool | What it does |
|------|-------------|
| `shell_tool` | Runs `bash -c <cmd>`. The per-call timeout kills the whole process group (so backgrounded jobs can't outlive it) and output is capped at 1 MiB per stream. A guard refuses commands that are catastrophic *by accident* — an agent safeguard, **not** a sandbox (see [Security](#-security)). |
| `ssh_tool` | Run commands / transfer files on remote servers configured on the SSH page; credentials encrypted at rest. |
| `http_request` | Arbitrary HTTP requests plus a library of saved HTTP requests. |
| `web_search` | Web search with quota-rotating accounts. |
| `image_tool` | Image generation/processing via the bundled `image_processor` crate. |
| `parallel_worker` | Fan out sub-tasks in parallel. |
| `cron_job_tool` | Create scheduled jobs from the agent. |
| `watcher_tool` | Create watchers from the agent. |
| `run_workflow` | Trigger a workflow from the agent. |

When `agent.allow_tool_writing` is on, the agent can also **write a temporary tool on the fly** for a one-off need.

### Built-in integrations (in-process, from the `crates/axon-*` crates)

- **Google** — Gmail, Calendar, Drive, Docs, Sheets, Slides, Contacts, Tasks, Meet, Chat, Forms, Places, YouTube.
- **Microsoft** — Outlook, Calendar, OneDrive, Teams.
- **Facebook** — Pages, posts, comments, insights, messaging.
- **Instagram** — Publishing.
- **CRM** — Leads, deals, organizations, activities.
- **Business utilities** — Notes, tasks, datetime, text, web.

Connect the integrations from the dashboard's **Services** page (OAuth for Google/Microsoft/Facebook). Tokens persist in the DB across rebuilds. External MCP servers can also be added from the **Services → MCP** page (those run over SSE).

---

## 💬 Messaging Channels

Set the relevant token (env var or `messaging.*` setting) and restart:

| Channel | Env / Setting | Notes |
|---------|---------------|-------|
| **Telegram** | `TELOXIDE_TOKEN` / `messaging.telegram_token` | Long-polling starts automatically; replies route back to the originating workflow. |
| **Discord** | `DISCORD_TOKEN` / `messaging.discord_token` | Gateway connection. |
| **Slack** | `SLACK_BOT_TOKEN` / `messaging.slack_token` | Point your Slack app's Events URL at `POST /api/slack/events`. |
| **Facebook / Messenger** | `credentials.json` (app secret, verify token, page id) | Webhook: `GET/POST /webhook/facebook`. Comment auto-replies queued with human-like delays and a "Like" before replying. |
| **WhatsApp** | — | Webhook: `GET/POST /webhook/whatsapp`. |
| **Web chat** | — | Built into the dashboard (Chat page + live WebSocket). |

---

## ⏰ Scheduler, Watchers & Workflows

- **Scheduler** (Tasks page): natural-language or 6-field cron schedules ("every 5 minutes", "daily at 9am", "every Monday at 9am"). NL is converted to cron via the `router` model. Fired jobs run as agent tasks and can send reminders using `scheduler.nudge_prompt`.
- **Watchers** (Watchers page): poll a source (e.g. an email inbox, a shell command's output) and notify the owner when something new/changed appears, with quiet-hours support.
- **Workflows** (Workflows page): a visual DAG of nodes (Trigger, Shell, JS, Synapse/Axon agent node, IF, Switch) with edges, built on [Vue Flow](https://vueflow.dev/). Triggerable manually, on a schedule, or via `POST /webhook/external/:workflow_id`. On completion, a workflow can hand its output to the agent for post-processing. The **Stimulus** (Gmail) trigger takes optional **Subject** and **Body / Keyword** filters: plain text (no `{{ }}` expressions, since a trigger has no upstream input) that's appended to the Gmail search query and matched server-side — the subject filter scopes to the subject line, the body/keyword filter matches anywhere in the message. Empty filters fire on every new email in the label; multi-word input uses Gmail AND semantics (every word must match). Both the live poller and the manual "Execute Step" test fetch apply the filters identically.

### Expression helpers

Any node field can embed an expression in `{{ … }}`, and the **JS/Code node** runs full JavaScript. Both surfaces share the same helper globals so `{{ }}` fields and Code nodes behave identically:

| Helper | What it is |
|---|---|
| `$json` / `$input` | The previous node's output object (n8n's `$input.first().json`). |
| `$node["Name"].data` | A specific upstream node's output (also `.json` / `.output`). Resolves by node name or id; `.error`, `.name`, `.id`, `.type` are also available. |
| `$items` | Array of `{ json, data, name, id, type }` for every preceding node. |
| `$prevNode` | Metadata (`name`/`id`/`type` + output) of the immediately preceding node. |
| `$now` | Current timestamp, ISO-8601 string (UTC). Use `new Date($now)` for date math. |
| `$today` | Current date, `YYYY-MM-DD`. |
| `$jmespath(obj, expr)` | Run a [JMESPath](https://jmespath.org) query over any object/array. Invalid queries return `null` (never a run failure). |
| `$workflow` | `{ id }` of the running workflow (Code node also exposes `$execution.workflowId`, `$nodeId`, `$nodeName`). |
| `$env` | Whitelisted environment variables — **empty by default**. Only names listed in `AXON_EXPR_ENV` (comma-separated) are exposed; `AXON_MASTER_KEY` is never exposed even if listed. |

Examples:

```js
{{ $json.email.toUpperCase() }}
{{ $node["HTTP Request"].data.items.length }}
{{ $jmespath($node["API"].data, "results[?score > `0.8`].id") }}
{{ $env.REGION }}          // requires AXON_EXPR_ENV=REGION
```

**n8n → Axon cheat sheet**

| n8n | Axon |
|---|---|
| `{{ $json.field }}` | `{{ $json.field }}` (same) |
| `{{ $node["X"].json.field }}` | `{{ $node["X"].data.field }}` (`.json` also works) |
| `{{ $now }}` (Luxon DateTime) | `{{ $now }}` (ISO string; wrap in `new Date($now)`) |
| `{{ $today }}` | `{{ $today }}` (same) |
| `{{ $jmespath($json, "…") }}` | `{{ $jmespath($json, "…") }}` (same) |
| `{{ $env.VAR }}` | `{{ $env.VAR }}` — allow it first via `AXON_EXPR_ENV=VAR` |
| `{{ $items("X") }}` | `{{ $node["X"].data }}` (a node's items) or `$items` (all upstream) |

> **Security note:** unlike n8n, `$env` is deny-by-default. Nothing from the process environment is visible to expressions until you opt each variable in through `AXON_EXPR_ENV`, and the master key is hard-blocked regardless.

---

## 🔒 Security

- **Dashboard auth:** every `/api/*` route and the `/ws` WebSocket require the master key.
  - REST clients send `Authorization: Bearer <AXON_MASTER_KEY>`.
  - The browser WebSocket sends it as a (URL-encoded) `?api_key=` query parameter.
  - **If `AXON_MASTER_KEY` is unset, boot refuses to start** rather than protecting secrets with a public development key — set a strong key, or set `AXON_DEV=1` to explicitly opt into the insecure default for local development only.
- **Secret encryption:** API keys, SSH credentials, and MCP keys are encrypted with **AES-256-GCM** before being written to SQLite. The key is derived from `AXON_MASTER_KEY` with **SHA-256** (any key length is accepted; short/long keys are no longer silently truncated or padded). New ciphertext is tagged with a `v2:` prefix.
  - **Seamless upgrade:** secrets written under the older truncate/pad scheme are still read correctly and are re-encrypted to the `v2:` scheme in place on the next boot. A `v2:` value that fails to decrypt (wrong/changed master key) resolves to *empty* — the credential must be re-entered — instead of leaking the raw ciphertext.
  - **Key rotation:** changing `AXON_MASTER_KEY` normally leaves previously stored secrets unreadable. To rotate deliberately, set `AXON_MASTER_KEY_OLD` to the previous key for **one boot** alongside the new `AXON_MASTER_KEY` — every stored secret is re-encrypted under the new key on that boot — then remove `AXON_MASTER_KEY_OLD`.
- **Credential test:** the Services page has a **Test** button per stored credential (`POST /api/credentials/:id/test`) that makes a cheap, service-specific call and reports validity without ever returning the secret. Services without a known probe report "present but not testable".
- **File downloads** are restricted to the staging directory (`data/files`) via canonical-path validation, preventing path traversal.
- **Webhook authenticity:** Facebook events are verified with HMAC-SHA256 against the app secret; the verify token gates subscription.
- **Brute-force throttling:** failed dashboard auth is penalised per source — 10 free attempts, then an escalating block (30s, doubling, capped at 15 min). Because that per-source key comes from a proxy header a client can forge, a **global backstop** also delays *every* failed attempt once failures exceed 50/minute, ramping to 2s. Correct keys are never delayed, so an attacker cannot use the backstop to lock the operator out. Put the app behind the reverse proxy in `deploy/Caddyfile.example` so the forwarded-IP headers can be trusted.
- **⚠️ The shell tool is guarded, not sandboxed.** `shell_tool` runs arbitrary commands **as the Axon process user, by design** — that is the feature, and no string-level check can contain it.

  The guard catches commands that are catastrophic *by accident*: recursive deletes of `/` or `$HOME`, `mkfs`, `dd if=`, recursive `chmod`/`chown`, and account/firewall changes (`passwd`, `userdel`, `groupdel`, `iptables`, `ufw`). It matches the parsed program name and its flags, so re-spelled variants are caught too (`rm -fr /`, `rm  -rf /`, `rm -r /`, `/bin/rm -rf /`, `sudo rm -rf /`), while commands that merely *mention* one of those words in an argument are not (`cat /etc/passwd`, `grep iptables /var/log/syslog`). *Scoped* destructive commands stay allowed on purpose — `rm -rf ./build` is the tool doing its job.

  **It cannot stop a determined caller, and is not meant to.** `X=rm; $X -rf /`, a base64-piped script, or a one-line Python program all reach the same syscalls without ever spelling a refused command. Anyone able to invoke the tool already holds the master key.

  **Treat the process user's privileges as the actual boundary:** run Axon as a least-privilege user, ideally in a container, and never as root. Don't expose the dashboard publicly without a strong master key.

---

## 📡 HTTP & WebSocket API

All `/api/*` and `/ws` routes require the master key (see [Security](#-security)). Webhook, health, OAuth-callback, and media routes are public.

### Core agent & memory

| Method | Route | Purpose |
|--------|-------|---------|
| `POST` | `/api/run` | Run an agent request. |
| `GET` | `/api/runs` · `/api/runs/:id` | Recent runs / run detail. |
| `WS` | `/ws` | Live log + chat stream. |
| `GET` | `/api/memory/recent` | Recent memories. |
| `POST` | `/api/memory/search` | Semantic memory search. |
| `DELETE` | `/api/memory/:id` | Delete a memory. |

### Models & tools

| Method | Route |
|--------|-------|
| `GET`/`POST` | `/api/models` |
| `PUT`/`DELETE` | `/api/models/:name` |
| `PUT` | `/api/models/bulk` |
| `POST` | `/api/models/:name/reset` |
| `GET` | `/api/tools` · `/api/fonts` |
| `POST` | `/api/tools/reload` |
| `PUT` | `/api/tools/:name` |
| `GET`/`POST` | `/api/mcp` · `DELETE /api/mcp/:name` · `GET /api/mcp/tools` |

### Settings, services & credentials

| Method | Route |
|--------|-------|
| `GET` | `/api/settings/:key` · `PUT /api/settings/:key` |
| `GET` | `/api/integrations/status` |
| `POST` | `/api/integrations/:platform/url` · `/api/integrations/:platform/disconnect` |
| `GET` | `/api/messaging/status` · `POST /api/messaging/reconnect/:platform` |
| `GET`/`POST` | `/api/credentials` · `DELETE /api/credentials/:id` · `POST /api/credentials/:id/test` |
| `GET`/`POST` | `/api/ssh_servers` · `DELETE /api/ssh_servers/:name` |
| `GET`/`POST` | `/api/websearch/accounts` · `DELETE /api/websearch/accounts/:id` · `POST /api/websearch/reset` |

### Scheduler, watchers, workflows, synapses

| Method | Route |
|--------|-------|
| `GET`/`POST` | `/api/jobs` · `PUT /api/jobs/:id` |
| `POST` | `/api/jobs/:id/run` · `/api/jobs/:id/pause` · `/api/jobs/:id/resume` · `DELETE /api/jobs/:id/delete` |
| `GET`/`POST` | `/api/watchers` · `PUT`/`DELETE /api/watchers/:id` · `POST /api/watchers/:id/run` · `GET /api/watchers/log` |
| `GET`/`POST` | `/api/workflows` · `DELETE /api/workflows/:id` |
| `POST` | `/api/workflows/:id/run` · `/api/workflows/:id/run/:node_id` · `/api/workflows/:id/stop` |
| `GET` | `/api/workflows/:id/runs` · `/api/workflow-runs/:run_id` |
| `GET`/`POST` | `/api/synapses` · `DELETE /api/synapses/:id` · `POST /api/synapses/:id/run` · `POST /api/synapse/adhoc` |

### Files, patterns & integrations data

| Method | Route |
|--------|-------|
| `GET` | `/api/files/:dir` · `DELETE /api/files/:dir/:id` · `DELETE /api/files/delete-all` |
| `GET` | `/api/download` · `POST /api/upload` |
| `GET`/`POST` | `/api/patterns` · `PUT /api/patterns/bulk` · `PUT`/`DELETE /api/patterns/:id` · `POST /api/patterns/test` |
| `GET` | `/api/google/calendars` · `/api/google/sheets` · `/api/google/sheets/:spreadsheet_id/tabs` · `/api/fovea/folders` |

### Public (no auth) endpoints

| Method | Route | Purpose |
|--------|-------|---------|
| `GET` | `/health` · `/ready` | Health/readiness checks. |
| `GET`/`POST` | `/webhook/facebook` | Facebook verification & events. |
| `POST` | `/webhook/telegram` · `/webhook/whatsapp` | Messaging webhooks. |
| `POST` | `/webhook/external/:workflow_id` | Trigger a workflow externally. |
| `GET` | `/auth/:service/callback` | OAuth callback (Google/Microsoft/Facebook). |
| `GET`/`HEAD` | `/media/local/:token` · `/media/local/:token/:name` | Local media for Instagram publishing. |

---

## 🌐 The Dashboard

A Vue 3 + Vite SPA built into static files and served by the agent. Pages include:

- **Chat** — talk to the agent live, watch the reasoning stream.
- **Models** — manage the model catalog (add/edit/prioritize/enable/reset) live.
- **Tools** — inspect and toggle built-in and MCP tools.
- **Services** — OAuth connect/disconnect for Google/Microsoft/Facebook, MCP servers, credentials, SSH servers, web-search accounts.
- **Memory** — browse, search, and delete memories.
- **Files** — staged file uploads/downloads.
- **Tasks** — scheduled jobs (cron / natural language).
- **Watchers** — source pollers with quiet hours.
- **Workflows** — visual DAG canvas (Vue Flow) with run history.
- **Settings** — every runtime knob, including the live system prompt.
- **Docs** — in-app documentation.

To rebuild the dashboard after UI changes:

```bash
cd axon-ui && npm run build && cp -r dist/* ../crates/axon-agent/static/
```

### Installing the dashboard as an app (PWA)

The dashboard ships a web-app manifest, so any HTTPS deployment is installable as an app — no store, no separate build. On Android Chrome, open the dashboard URL, then menu (⋮) → **Add to Home screen** → **Install** (Chrome may also offer an install banner on its own). On iOS Safari use Share → **Add to Home Screen**. The installed app opens standalone (no browser chrome) with the Axon icon and dark theme. Requirements: the site must be served over HTTPS (see [Production Deployment](#-production-deployment)) — plain `http://server-ip:3000` only gets a bookmark-style shortcut, not an install.

---

## 📦 Production Deployment

The deploy scripts build the release `axon` binary (with the integration crates compiled in), build the UI, bundle everything into `axon_deploy.tar.gz`, and ship it to the configured server.

```bash
bash deployaxongcp.sh                # full build + bundle + deploy (GCP target)
bash deployaxongcp.sh --clean        # cargo clean first (full rebuild)
bash deployaxongcp.sh --skip-build   # deploy the existing tar.gz
bash deployaxongcp.sh --skip-deploy  # build + bundle only
```

Variants: `deploycham.sh` / `deploychamgcp.sh` / `deploycham-wipe.sh` / `deployaxon-wipe.sh` / `deployfrontend.sh` (frontend-only).

> **Update `TARGET_SERVER`/`GCP_INSTANCE`/`REMOTE_DIR` and SSH details at the top of each script** to your own host before using them. All integration services run in-process with the agent.

On the server:

- Run the agent under a process supervisor (**systemd**).
- Install Qdrant via `qdrant/install.sh` (also sets up backup/health/trim timers and the systemd unit).
- Set a strong `AXON_MASTER_KEY` and keep it stable (it encrypts stored secrets — see [Security](#-security) for rotating it deliberately).
- **`axon.db`/`crm.db` back up automatically** — a daily in-process sweep (`backup.enabled`/`backup.retention_days` in Settings → Backups) writes timestamped, `VACUUM INTO`-compacted snapshots to the Files page directory and prunes ones past the retention window. Like Qdrant's `axon-backup.sh`, these are **local, on-instance backups only** — they live on the same disk as the data they protect, so they don't protect against disk/instance loss. Copying them off-instance (rsync, object storage, …) is the operator's responsibility.
- **TLS is required for any internet-facing deployment** — axon-agent itself only ever binds plain HTTP (`0.0.0.0:$AXON_PORT`, default 3000). Set `AXON_DOMAIN` (or `CHAM_DOMAIN`) in `.deploy.env` before deploying and the deploy script installs [Caddy](https://caddyserver.com/) as a reverse proxy in front of it, with automatic Let's Encrypt provisioning/renewal — no certbot cron job to maintain. DNS for the domain must already point at the instance first. See `deploy/Caddyfile.example` for the template, or point your own reverse proxy/load balancer at `localhost:$AXON_PORT` if you're not using the bundled Caddy setup. Deploying without a domain set is HTTP-only and should only be used for local/internal testing.

---

## 🧰 Development

This is a standard Cargo workspace. Useful commands:

```bash
# Build everything (debug)
cargo build

# Build the release binary (LTO + stripped)
cargo build --release

# Run the agent (from the workspace root)
cargo run -p axon

# Check a single crate without producing artifacts
cargo check -p axon-google

# Watch UI in dev mode with hot reload
cd axon-ui && npm run dev
```

Build profiles (defined at the workspace root):

- `[profile.dev]` — `debug = 0`, `codegen-units = 1`, incremental off (faster full builds).
- `[profile.release]` — `opt-level = 3`, `lto = true`, `codegen-units = 1`, `strip = true`.

Set `RUST_LOG=axon=debug` for verbose tracing during development.

---

## 🚢 Releasing

How to cut a GitHub release (maintainers). This produces the artifacts installed by [Quick Start → Option A](#-quick-start).

### 1. Build the release artifacts (sanitized — no secrets)

> ⚠️ **Never upload `axon_deploy_cham.tar.gz` / `axon_deploy_gcp.tar.gz` or the `dist-cham/` `dist-gcp/` bundles.** Those are your private deploy bundles and contain your real `.env`, `credentials.json`, and **SSH private keys**. Use the packaging scripts below, which build clean bundles (`.env.example` only, no keys) and abort if any secret sneaks in.

**Linux bundle** (`axon-linux-x86_64.tar.gz`) — run under WSL from the repo root:

```bash
bash scripts/package-release.sh
```

Builds the Vue dashboard, then the `axon` binary, and assembles `axon-linux-x86_64.tar.gz`. Needs `node`/`npm` and a Rust toolchain.

**Pick your binary flavour** (the script auto-selects, `--musl` / `--native` force it):

| Flavour | Runs on | How to get it |
|---------|---------|---------------|
| **static musl** (recommended) | **any** x86_64 Linux, any glibc version | one-time: `rustup target add x86_64-unknown-linux-musl && sudo apt-get install -y musl-tools` — or have `cross` + Docker |
| native glibc (fallback) | only glibc **≥ the build host's** (build on Ubuntu 24.04 ⇒ won't run on 22.04) | nothing extra |

This project builds cleanly for musl (TLS is rustls+ring, no OpenSSL; the only C dep, `libsqlite3-sys`, is bundled). For a public download, prefer **musl** so users on any distro version can run it. If you skip the musl setup the script still produces a working glibc binary and prints exactly how to upgrade to musl.

> The UI build step auto-detects a Windows-mounted repo (`/mnt/...` under WSL) and builds the dashboard on the Linux filesystem, avoiding the shared-`node_modules` rollup error (`Cannot find module @rollup/rollup-linux-x64-gnu`).

**Windows bundle** (`axon-windows-x86_64.zip`) — run in PowerShell from the repo root:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package-release.ps1
```

Builds the dashboard and a native `axon.exe`, then zips it with its `static\`, `config\`, and `tools\` folders. `axon.exe` **cannot run alone** — it serves the dashboard from `static\` and reads `config\`/`.env` from its working directory — so the Windows release is a zip bundle, the twin of the Linux tarball.

### 2. Create the GitHub release

Tag the version and upload the artifacts **plus the two install scripts** so the one-line installers can fetch them from `releases/latest/download/…`:

- `axon-linux-x86_64.tar.gz`
- `axon-windows-x86_64.zip`
- `install.sh`      (copy of `scripts/install.sh`)
- `install.ps1`     (copy of `scripts/install.ps1`)

With the GitHub CLI:

```bash
gh release create v0.4.0 \
  axon-linux-x86_64.tar.gz \
  axon-windows-x86_64.zip \
  scripts/install.sh \
  scripts/install.ps1 \
  --title "Axon v0.4.0" \
  --notes "See README for setup. Linux: install.sh · Windows: install.ps1"
```

…or draft it in the GitHub web UI and drag the four files in.

---

## 🩹 Troubleshooting

| Symptom | Likely cause / fix |
|---------|--------------------|
| Dashboard loads but every API call 401s | Wrong/missing master key in the browser. Log in again; confirm `AXON_MASTER_KEY` matches. |
| REST works but the live log/chat stream never connects | WebSocket auth — ensure the `api_key` query param matches and is URL-safe. |
| "All models exhausted — check API keys or wait for rate limits to reset" | No usable model. Check that provider keys resolve (`${...}` placeholders must exist in env or settings), that models are `enabled`, and whether everything is on rate-limit cooldown. |
| `Model '…' has unresolved API key placeholder ${X}` | The `${X}` in `models.toml` isn't defined in the environment or the `settings` table. Add it to `.env` or the Settings page. |
| Integration tools (Gmail/Calendar/etc.) missing | The in-process integrations failed to initialize — usually a missing `credentials.json`. Check the startup log for "In-process MCP init failed". |
| Decryption warnings / garbled stored keys | `AXON_MASTER_KEY` changed (or was unset) after secrets were saved. Either re-enter the affected secrets, or rotate deliberately with `AXON_MASTER_KEY_OLD` (see [Security](#-security)). |
| Long-term memory recall does nothing | Qdrant not running or `VOYAGE_API_KEY` unset. The agent runs fine without it, but semantic recall is disabled. |
| `npm run build` fails with `'vite' is not recognized` | UI dev dependencies aren't installed. Run `npm install` in `axon-ui/` first. |
| Stuck `running` runs after a crash | Cleaned up automatically on the next startup (marked `failed`). |

Logs: set `RUST_LOG=axon=debug` for verbose tracing. Facebook auto-reply has an extra debug log at `/tmp/autoreply_debug.log` on the server.

---

## 📄 License

Licensed under the **GNU Affero General Public License v3.0** (AGPL-3.0) — see [`LICENSE`](./LICENSE).

You're free to use, study, modify, and self-host Axon. Because this is the **Affero** GPL, if you run a **modified** version of Axon as a network service, §13 requires you to make the corresponding source of your modified version available to its users. All crates in this workspace — including the bundled `image_processor` crate (`crates/axon-image`) — are covered by this license.

Axon is an **independent implementation** in Rust. Some workflow concepts and the visual-canvas UX are *inspired by* [n8n](https://n8n.io), but Axon contains no n8n source code — n8n is a separate project under its own (non-AGPL) license.

---

## 📚 Further Reading

- **[`crates/axon-agent/README.md`](./crates/axon-agent/README.md)** — crate-level notes for the core agent.
- **[`qdrant/README.md`](./qdrant/README.md)** — Qdrant setup, backups, and health timers.
- **[`AGENTS.md`](./AGENTS.md)** — guidance for AI agents working in this repo (including the `graphify` knowledge-graph tooling).

---

<p align="center"><sub>Built with 🦀 Rust, ⚡ tokio, 🟢 Vue 3, and 🛰 Axum.</sub></p>
