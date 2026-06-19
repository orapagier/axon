# Axon — Complete User Guide

Axon is a highly autonomous, multi-platform AI agent written in Rust (async/`tokio`). It
answers and acts on messages across several channels (Facebook/Messenger, Telegram,
Discord, Slack, and a built-in web chat), routes LLM requests across many providers with
automatic fallback, runs real tools (shell, SSH, HTTP, web search, image generation, and a
large catalog of Google/Microsoft/Facebook/CRM tools via an MCP server), and remembers
context using SQLite + a Qdrant vector store.

This guide covers everything an operator needs: the architecture, prerequisites,
configuration, building and running (local and production), the web dashboard, the model
router, memory, tools, integrations, scheduling, workflows, security, and troubleshooting.

---

## 1. Repository layout

| Path | What it is |
|------|-----------|
| `axon-agent/` | **The core agent + web dashboard** (Rust). The main binary you run. Serves the HTTP API, dashboard UI, WebSocket stream, webhooks, scheduler, watcher, and the agent reasoning loop. |
| `axon-mcp-server/` | **MCP tool server** (Rust workspace). Exposes Google, Microsoft, Facebook, Instagram, CRM, and business/utility tools over the Model Context Protocol (SSE). The agent connects to it as an MCP client. |
| `axon-ui/` | **Web dashboard** (Vue 3 + Vite). Built to static files and served by `axon-agent`. |
| `axon-api-proxy/` | Standalone LLM API key-pool proxy (Rust). |
| `axon-image/` | Image-processing/generation library (Rust) used by the agent's `image_tool`. |
| `qdrant/` | Deployment scripts for the Qdrant vector DB (systemd units, backup/health/trim timers, collection setup). |
| `deploy.sh`, `deploygcp.sh`, `deployfrontend.sh` | Production build + deploy scripts (build release binaries, bundle, ship to the server). |
| `run.bat` | One-click local Windows build-and-run (builds UI, copies static, runs agent). |
| `axon-agent/config/models.toml` | **Model catalog** — the source of truth for which LLMs Axon can use. |
| `axon-agent/memory/schema.sql` | SQLite schema applied on startup. |

The various `.zip`/`.tar.gz` files are build/deploy archives and can be ignored for
day-to-day operation.

---

## 2. How the pieces fit together

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
                 SQLite (memory/axon.db)   MCP (SSE) to axon-mcp-server (:8080)
                                │               │
                          Qdrant (vectors)  Google / Microsoft / Facebook / CRM tools
                                                │
                          LLM providers ◀───────┘  (Gemini, Anthropic, Groq,
                          (via model router)        Cerebras, NVIDIA, OpenRouter,
                                                     Ollama, …)
```

- The **agent** is the only process users talk to. It owns the dashboard, the webhooks, and
  the reasoning loop.
- The **MCP server** is a separate process the agent auto-connects to at
  `http://127.0.0.1:8080/sse`. It provides the "integration" tools (Gmail, Calendar, Drive,
  OneDrive, Outlook, Facebook pages, Instagram, CRM, etc.).
- **Qdrant** is an external service used for long-term semantic memory.

---

## 3. Prerequisites

- **Rust toolchain** 1.70+ (developed/tested with a current stable toolchain; `cargo`).
- **Node.js 18+** and npm (only needed to build the dashboard UI).
- **SQLite3** — bundled via the Rust crate; no separate install required for the agent.
- **Qdrant** — a running instance for long-term memory. Optional for a first run (the agent
  starts without it; semantic recall is simply disabled), but required for the full memory
  feature. See `qdrant/README.md` and `qdrant/install.sh`.
- API keys for at least one LLM provider (see §5).

---

## 4. First-time setup (local)

The fastest path on Windows:

```bat
run.bat
```

`run.bat` will:
1. `npm install` (if needed) and `npm run build` inside `axon-ui/`.
2. Copy `axon-ui/dist/*` into `axon-agent/static/`.
3. Stop any running `axon` process, then `cargo run` the agent.

Manual equivalent (any OS):

```bash
# 1) Build the dashboard
cd axon-ui
npm install
npm run build

# 2) Make the built UI available to the agent
#    (the agent serves ./static as the dashboard)
mkdir -p ../axon-agent/static
cp -r dist/* ../axon-agent/static/

# 3) Run the agent
cd ../axon-agent
cargo run            # or: cargo run --release
```

Then open **http://localhost:3000**.

> The agent serves the UI from `axon-agent/static/`. If you change UI code, rebuild the UI
> and re-copy `dist/` into `static/` (this is exactly what `run.bat` automates).

Optionally run the **MCP server** in a second terminal to enable the Google/Microsoft/
Facebook/CRM tools:

```bash
cd axon-mcp-server
cargo run --release        # listens on 0.0.0.0:8080 by default
```

The agent auto-connects to `http://127.0.0.1:8080/sse` on startup if no MCP server is
already configured in its database.

---

## 5. Configuration

Axon reads configuration from three layers, in increasing precedence:

1. **`config/models.toml`** — the model catalog (source of truth for models).
2. **Environment variables / `.env`** — secrets and process-level settings.
3. **The dashboard settings + database** — runtime settings you can change live without a
   restart.

### 5.1 Environment variables

Loaded automatically from (in order) `$AXON_ENV_FILE`, the working-directory `.env`, and
`.env`/`axon-agent.env` next to the executable.

| Variable | Default | Purpose |
|----------|---------|---------|
| `AXON_PORT` | `3000` | Dashboard / API / webhook listen port. |
| `AXON_DB_PATH` | `memory/axon.db` | SQLite database path. |
| `AXON_MASTER_KEY` | *(unset)* | **Dashboard auth + secret encryption key.** See §10. |
| `AXON_ENV_FILE` | *(unset)* | Explicit path to an env file to load first. |
| `QDRANT_URL` | *(provider default)* | Qdrant endpoint for long-term memory. |
| `VOYAGE_API_KEY` | *(unset)* | Embedding key used by the memory store. |
| `TELOXIDE_TOKEN` | *(unset)* | Telegram bot token (or set `messaging.telegram_token` in settings). |
| `DISCORD_TOKEN` | *(unset)* | Discord bot token (or `messaging.discord_token`). |
| `SLACK_BOT_TOKEN` | *(unset)* | Slack bot token (or `messaging.slack_token`). |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | *(unset)* | If set, enables OpenTelemetry tracing export. |
| `RUST_LOG` | `axon=info` | Standard `tracing` log filter. |
| *(provider keys)* | — | Referenced from `models.toml` as `${NAME}` placeholders, e.g. `GEMINI_CANCHOWLUNG`, `GROQ_CANCHOWLUNG`, `CEREBRAS_CANCHOWLUNG`. |

### 5.2 The model catalog — `config/models.toml`

On every startup, Axon **syncs `models.toml` into the database as the source of truth**:
models in the file are upserted, and any model in the DB that is *not* in the file is
deleted. Edit the TOML, restart, and the dashboard reflects it.

Each `[[models]]` entry:

```toml
[[models]]
name        = "gemini-canchowlung"     # unique display name
provider    = "google"                 # google | anthropic | groq | cerebras |
                                        # openai/openai_compat | ollama | nvidia | openrouter
model_id    = "gemini-2.5-flash"       # provider model id. Comma-separated = per-model fallback
api_key     = "${GEMINI_CANCHOWLUNG}"  # literal key, or ${ENV_OR_SETTING} placeholder
base_url    = "https://..."            # optional; required for OpenAI-compatible providers
role        = ""                       # "" = general pool. See roles below
priority    = 1                        # lower number = higher priority tier
timeout_secs = 60                      # optional per-model call timeout
max_tokens  = 4096                     # optional (defaults to 4096)
enabled     = true                     # optional (defaults to true)
```

**Roles** route specific kinds of work to specific models:

| Role | Used for |
|------|---------|
| `""` (empty) | General pool — the default for normal requests. |
| `simple_tasks` | Fast/cheap model for conversational, no-tool, first-attempt replies. |
| `complex_tasks` | Most capable model for tool use, corrections, and long runs. |
| `router` | The small model used to convert natural-language schedules to cron, etc. |
| `quality_checker` | Secondary model that audits the main agent's output (see §6). |
| `axon_node` | Model used by the "Axon" node inside Workflows. |
| `paid_model` | **Last-resort** fallback, only tried after every free option is exhausted. |

**`model_id` fallback chains:** a comma-separated `model_id` (e.g.
`"gpt-oss-120b,gpt-oss-20b"`) lets one entry try several upstream model IDs in order before
the record is considered failed.

### 5.3 Runtime settings (dashboard / DB)

Most behavior is tunable live via the **Settings** page (stored in the `settings` table).
Notable keys and their defaults:

- `agent.max_iterations` (20), `agent.max_parallel_tools` (5), `agent.tool_timeout_secs` (30)
- `agent.request_timeout_secs` (45), `agent.request_timeout_max_secs` (120),
  `agent.min_model_chain_secs` (60), `agent.run_timeout_secs` (300)
- `agent.quality_check` (true), `agent.allow_tool_writing` (true),
  `agent.temp_tool_max_retries` (2)
- `agent.system_prompt` — the master system prompt (editable live).
- `router.rate_limit_cooldown` (minutes), `router.error_threshold` (3),
  `router.model_call_timeout_secs` (20), `router.model_health_check_interval_secs` (90),
  and the adaptive-timeout knobs (`router.model_call_timeout_{min,max,per_1k_chars,fair_share_grace}_secs`).
- `memory.short_term_max_msgs` (50), `memory.long_term_top_k` (5)
- `websearch.enabled`, `websearch.max_results`
- `watcher.user_name`, `watcher.user_title`, `watcher.quiet_hours_*`
- `scheduler.nudge_prompt` — template for scheduled reminders.
- `instagram.*` — public base URL, bind address, and media-poll timeouts for IG publishing.

---

## 6. The agent reasoning loop (what happens on each request)

1. **Context build** — loads short-term history for the session, searches long-term memory
   and recent tool observations (skipped for pure greetings), and routes an initial tool set.
2. **Iterate** (bounded by `agent.max_iterations` and `agent.run_timeout_secs`):
   - Pick a model by role/priority (see §7), with **sticky routing** (stays on the model that
     worked last turn to avoid tool-format drift).
   - Call the LLM with the filtered tool set and an adaptive timeout.
   - If the model emits tool calls, execute them (up to `agent.max_parallel_tools` in
     parallel) and feed results back.
   - If the model answers, run the **validation pipeline**.
3. **Validation pipeline** (cheapest checks first):
   - *Claim guard* — blocks "I sent/created/deleted…" claims that have no matching successful
     tool-execution receipt.
   - *Refusal nudge* — if a tool was available but the model said "I can't", it's nudged to
     actually use the tool.
   - *Blank-response* and *raw-tool-syntax* guards.
   - *Quality check* — an optional secondary LLM (`quality_checker` role) audits tool-backed
     answers and can request a revision (up to 3 times).
4. **Finalize** — strips reasoning/markdown noise, resolves `<send_file>…</send_file>` tags
   into downloads/attachments, persists the run, and streams the answer.

---

## 7. Model router & resilience

- **Priority tiers:** models are grouped by `priority` (lower = higher). Within a tier,
  traffic is round-robined across distinct provider/model endpoints, biased away from
  endpoints that are critically close to their rate limit.
- **Fallback order per request:** preferred model (if the caller picked one) → sticky model →
  role pool → general pool → a sweep over any remaining free model → `paid_model` last.
- **Rate-limit quarantine:** a `429`/quota error puts the model on a cooldown
  (`router.rate_limit_cooldown` minutes) and routing falls through to the next option without
  dropping the request. Cooldowns auto-expire.
- **Health checker:** a background task pings one available model per provider every
  ~90s and proactively benches unhealthy endpoints before a user hits them.
- **Alerts:** rate-limit, timeout, and "paid fallback used" events are collected per run and
  surfaced to the operator (dashboard + messaging notifications).

---

## 8. Memory

- **Short-term (SQLite):** the last N messages per `session_id`/`chat_id`
  (`memory.short_term_max_msgs`, default 50). Reconstructed into the prompt each run.
- **Long-term (Qdrant):** useful, tool-backed results are embedded and stored for semantic
  recall on future requests (`memory.long_term_top_k` results injected as *hints*). Requires
  Qdrant + an embedding key (`VOYAGE_API_KEY`).
- **Tool observations:** large tool outputs are compressed and stored, searchable via SQLite
  FTS, and recent ones are surfaced as context.

Memories are always presented to the model as **hints to verify with tools**, never as
ground truth — this is enforced in the system prompt and the claim guard.

---

## 9. Tools & integrations

### Built-in (in `axon-agent`)
- **`shell_tool`** — runs `bash -c <cmd>` with a per-call timeout and streamed output. A
  blocklist rejects the most catastrophic commands (`rm -rf /`, `rm -rf /*`, `mkfs`,
  `dd if=`, `chmod -R`, `chown -R`, `iptables`, `ufw`, `passwd`, `userdel`, `groupdel`).
  See §10 for the exact scope.
- **`ssh_tool`** — run commands / transfer files on remote servers configured on the SSH
  page; credentials are encrypted at rest.
- **`http_request`** / saved HTTP requests, **`web_search`**, **`image_tool`**,
  **`parallel_worker`**, **`cron_job_tool`**, **`watcher_tool`**, **`run_workflow`**.
- **Tool authoring:** when `agent.allow_tool_writing` is on, the agent can write a temporary
  tool on the fly for a one-off need.

### Via the MCP server (`axon-mcp-server`)
Gmail, Google Calendar/Drive/Docs/Sheets/Slides/Contacts/Tasks/Meet/Chat/Forms/Places/
YouTube; Microsoft Outlook/Calendar/OneDrive/Teams; Facebook pages/posts/comments/insights/
messaging; Instagram publishing; a CRM (leads/deals/orgs/activities); and business utilities
(notes, tasks, datetime, text, web).

Connect the integrations from the dashboard's **Services** page (OAuth for Google/Microsoft/
Facebook). Tokens persist in the DB across rebuilds.

---

## 10. Security

- **Dashboard auth:** every `/api/*` route and the `/ws` WebSocket require the master key.
  - REST clients send `Authorization: Bearer <AXON_MASTER_KEY>`.
  - The browser WebSocket sends it as a (URL-encoded) `?api_key=` query parameter.
  - **If `AXON_MASTER_KEY` is unset, auth is disabled entirely** (open dashboard). This is
    intended only for local dev — **always set a strong `AXON_MASTER_KEY` in production.**
- **Secret encryption:** API keys, SSH credentials, and MCP keys are encrypted with
  AES-256-GCM (key derived from `AXON_MASTER_KEY`) before being written to SQLite.
  - ⚠️ If `AXON_MASTER_KEY` is unset, encryption falls back to an insecure all-zeros key (and
    logs a warning). Changing the master key after secrets are stored makes existing secrets
    undecryptable — set it once, up front.
- **File downloads** are restricted to the staging directory (`data/files`) via canonical-path
  validation, preventing path traversal.
- **Webhook authenticity:** Facebook events are verified with HMAC-SHA256 against the app
  secret; the verify token gates subscription.
- **Shell blocklist scope (be aware):** the shell guardrail blocks the catastrophic patterns
  listed in §9 but, by design, still permits *scoped* destructive commands (e.g.
  `rm -rf ./build`). Treat the agent's shell access as you would a trusted operator account;
  do not expose the dashboard publicly without a master key, and run it as a least-privilege
  user.

---

## 11. Messaging channels

Set the relevant token (env var or `messaging.*` setting) and restart:

- **Telegram** — `TELOXIDE_TOKEN` / `messaging.telegram_token`. Long-polling starts
  automatically; the bot chats with users and streams progress.
- **Discord** — `DISCORD_TOKEN` / `messaging.discord_token`. Gateway connection.
- **Slack** — `SLACK_BOT_TOKEN` / `messaging.slack_token`. Point your Slack app's Events URL
  at `POST /api/slack/events`.
- **Facebook / Messenger** — configure `credentials.json` (app secret, verify token, page id)
  alongside the MCP server. Webhook endpoints: `GET/POST /webhook/facebook`. Comment
  auto-replies are queued with human-like delays and a "Like" before replying.
- **WhatsApp** — `GET/POST /webhook/whatsapp`.

---

## 12. Scheduler, watchers & workflows

- **Scheduler** (Tasks page): natural-language or 6-field cron schedules ("every 5 minutes",
  "daily at 9am", "every Monday at 9am"). NL is converted to cron via the `router` model.
  Fired jobs run as agent tasks and can send reminders using `scheduler.nudge_prompt`.
- **Watchers** (Watchers page): poll a source (e.g. email inbox, a shell command's output)
  and notify the owner when something new/changed appears, with quiet-hours support.
- **Workflows** (Workflows page): a visual DAG of nodes (Trigger, Shell, JS, Synapse/Axon
  agent node) with edges. Triggerable manually, on a schedule, or via
  `POST /webhook/external/:workflow_id`. On completion a workflow can hand its output to the
  agent for post-processing.

---

## 13. Production deployment

`deploy.sh` builds release binaries for `axon-agent` and `axon-mcp-server`, builds the UI,
bundles everything into `axon_deploy.tar.gz`, and ships it to the configured server. Useful
flags:

```bash
bash deploy.sh                 # full build + bundle + deploy
bash deploy.sh --clean         # cargo clean first (full rebuild)
bash deploy.sh --skip-build    # deploy the existing tar.gz
bash deploy.sh --skip-deploy   # build + bundle only
```

`deploygcp.sh` / `deployfrontend.sh` are GCP- and frontend-only variants. The target host,
remote dir, and SSH details are set at the top of each script — **update
`TARGET_SERVER`/`REMOTE_DIR` to your own host** before using them. On the server, run the
agent and MCP server under a process supervisor (systemd), and install Qdrant via
`qdrant/install.sh` (which also sets up backup/health/trim timers).

---

## 14. Troubleshooting

| Symptom | Likely cause / fix |
|---------|--------------------|
| Dashboard loads but every API call 401s | Wrong/missing master key in the browser. Log in again; confirm `AXON_MASTER_KEY` matches. |
| REST works but the live log / chat stream never connects (status "disconnected") | WebSocket auth. This was a real bug when the master key contained URL-special characters (`+ / = space …`); it is fixed in this revision (the `api_key` query param is now URL-decoded server-side). Rebuild the agent. |
| "All models exhausted — check API keys or wait for rate limits to reset" | No usable model. Check that provider keys resolve (`${...}` placeholders must exist in env or settings), that models are `enabled`, and whether everything is on rate-limit cooldown. |
| `Model '…' has unresolved API key placeholder ${X}` | The `${X}` in `models.toml` isn't defined in the environment or the `settings` table. Add it to `.env` or the Settings page. |
| Integration tools (Gmail/Calendar/etc.) missing | The MCP server isn't running or wasn't reachable at `127.0.0.1:8080/sse`. Start `axon-mcp-server`; the agent reconnects on the next restart. |
| Decryption warnings / garbled stored keys | `AXON_MASTER_KEY` changed (or was unset) after secrets were saved. Re-enter the affected secrets with the correct, stable key. |
| Long-term memory recall does nothing | Qdrant not running or `VOYAGE_API_KEY` unset. The agent runs fine without it, but semantic recall is disabled. |
| `npm run build` fails with `'vite' is not recognized` | UI dev dependencies aren't fully installed. Run `npm install` in `axon-ui/` first. |
| Stuck `running` runs after a crash | Cleaned up automatically on the next startup (marked `failed`). |

Logs: set `RUST_LOG=axon=debug` for verbose tracing. Facebook auto-reply has an extra debug
log at `/tmp/autoreply_debug.log` on the server.

---

## 15. Quick reference

- **Default URL:** http://localhost:3000
- **Agent port:** `AXON_PORT` (default `3000`)
- **MCP server:** `http://127.0.0.1:8080/sse`
- **Database:** `axon-agent/memory/axon.db` (`AXON_DB_PATH`)
- **Built UI is served from:** `axon-agent/static/`
- **Staged files:** `axon-agent/data/files/`
- **Model catalog:** `axon-agent/config/models.toml`
- **Local run:** `run.bat` (Windows) — builds UI, copies static, runs the agent.
</content>
</invoke>
