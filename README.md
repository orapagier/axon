<p align="center">
  <img src="axon/static/favicon.png" alt="Axon" width="180" />
</p>

<h1 align="center">Axon</h1>

<p align="center">
  <strong>A self-hosted, multi-model AI agent with tool use, memory, and multi-platform messaging.</strong>
</p>

<p align="center">
  Built entirely in Rust &nbsp;·&nbsp; Zero Python runtime required &nbsp;·&nbsp; Runs on a single $5 VPS
</p>

---

## Why "Axon"?

In neuroscience, an **axon** is the long fiber of a nerve cell that carries electrical signals from the brain to the rest of the body — connecting thought to action. This project embodies the same idea: a central AI brain that **receives signals** from any platform (Telegram, Discord, Slack, web), **thinks** using multiple LLM providers, **acts** through tools (email, calendar, SSH, files, Facebook), and **remembers** through a persistent memory system. Like a biological axon, it's the connective tissue between intelligence and the real world.

---

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Features](#features)
- [Project Structure](#project-structure)
- [Axon Agent](#axon-agent)
  - [Agent Loop](#agent-loop)
  - [Model Router](#model-router)
  - [Tool System](#tool-system)
  - [Memory System](#memory-system)
  - [Scheduler](#scheduler)
  - [Dashboard](#dashboard)
  - [Messaging Integrations](#messaging-integrations)
- [Axon MCP Server](#axon-mcp-server)
  - [Google Workspace](#google-workspace)
  - [Microsoft 365](#microsoft-365)
  - [Facebook / Meta](#facebook--meta)
  - [Business Tools](#business-tools)
- [Getting Started](#getting-started)
  - [Prerequisites](#prerequisites)
  - [Configuration](#configuration)
  - [Building](#building)
  - [Running Locally](#running-locally)
  - [Deploying to Production](#deploying-to-production)
- [Environment Variables](#environment-variables)
- [Model Configuration](#model-configuration)
- [API Reference](#api-reference)
- [License](#license)

---

## Overview

**Axon** is a fully self-hosted AI agent platform built in Rust. It consists of two main components:

| Component | Description | Port |
|---|---|---|
| **Axon Agent** | The core AI brain — agent loop, model routing, tool execution, memory, scheduling, web dashboard, and messaging gateway | `3000` |
| **Axon MCP Server** | A [Model Context Protocol](https://modelcontextprotocol.io/) server that provides 50+ tools for Google Workspace, Microsoft 365, Facebook, and general business utilities | `8080` |

The agent connects to the MCP server as a tool provider, giving the AI access to your email, calendar, cloud storage, social media, and more — all through natural language.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         USER INTERACTION                            │
│   Telegram  ·  Discord  ·  Slack  ·  Web Dashboard  ·  REST API    │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
                    ┌──────────▼──────────┐
                    │    AXON AGENT       │
                    │    (Rust, :3000)    │
                    ├────────────────────┤
                    │  ┌──────────────┐  │
                    │  │  Agent Loop  │  │ ◄── Iterative think → act → observe
                    │  └──────┬───────┘  │
                    │         │          │
                    │  ┌──────▼───────┐  │
                    │  │ Model Router │  │ ◄── Multi-provider, multi-key, role-based
                    │  └──────┬───────┘  │
                    │         │          │
                    │  ┌──────▼───────┐  │
                    │  │ Tool Router  │  │ ◄── Regex pattern matching → tool selection
                    │  └──────┬───────┘  │
                    │         │          │
                    │  ┌──────▼───────┐  │
                    │  │Tool Registry │  │ ◄── Python, internal, MCP, temp tools
                    │  └──────┬───────┘  │
                    │         │          │
                    │  ┌──────▼───────┐  │
                    │  │   Memory     │  │ ◄── Short-term (session) + Long-term (SQLite FTS5)
                    │  └──────┬───────┘  │
                    │         │          │
                    │  ┌──────▼───────┐  │
                    │  │  Scheduler   │  │ ◄── Cron jobs with natural language scheduling
                    │  └──────────────┘  │
                    └────────┬───────────┘
                             │ MCP Protocol (SSE/HTTP)
                    ┌────────▼───────────┐
                    │  AXON MCP SERVER   │
                    │  (Rust, :8080)     │
                    ├────────────────────┤
                    │  Google Workspace  │ ◄── Gmail, Calendar, Drive, Contacts
                    │  Microsoft 365     │ ◄── Outlook, Calendar, OneDrive, Teams
                    │  Facebook / Meta   │ ◄── Pages, Posts, Comments, Insights, Messenger
                    │  Business Tools    │ ◄── Notes, Tasks, Contacts, Web, DateTime, Text
                    └────────────────────┘
```

---

## Features

### Core Agent
- **Multi-model routing** — Supports Google Gemini, OpenRouter, Cerebras, Groq, Ollama, OpenAI, and Anthropic simultaneously with automatic failover and rate-limit handling
- **Role-based model assignment** — Assign different models to different roles: `complex_tasks`, `simple_tasks`, `router`, `quality_checker`, `memory_compressor`, `paid_model`
- **Iterative agent loop** — Think → Act → Observe cycle with up to 20 iterations per task
- **Quality checking** — Automatic response validation catches fabrication and critical errors before replying
- **Tool writing** — If a needed tool doesn't exist, the agent can write one on the fly in Python
- **Parallel tool execution** — Run multiple tools simultaneously for faster responses
- **Streaming responses** — Real-time progress updates through WebSocket

### Memory
- **Short-term memory** — Per-session conversation history (configurable depth)
- **Long-term memory** — Semantic search using SQLite FTS5 full-text search
- **Observations** — AI-compressed learnings from tool results, providing context for future tasks
- **Embedding support** — Optional Voyage AI embeddings for semantic similarity search

### Scheduling
- **Natural language scheduling** — "every 5 minutes", "daily at 9am", "every Monday"
- **Cron expression generation** — Automatically converts natural language to cron syntax
- **Stop conditions** — Jobs can auto-stop after N runs, after a specific date, or when a condition is met
- **Multi-platform delivery** — Scheduled task results are sent back to the originating platform

### Messaging Platforms
- **Telegram** — Full bot with file upload/download support
- **Discord** — Gateway bot integration
- **Slack** — Event-based webhook integration
- **Web Dashboard** — Built-in real-time chat with WebSocket streaming

### Dashboard
- **Chat interface** — Real-time conversation with streaming progress indicators
- **Run history** — Browse past agent runs with iteration details, models used, tools called
- **Model management** — Add, edit, enable/disable, and monitor models with live status
- **Tool management** — View all tools (Python, internal, MCP, temp), enable/disable
- **Pattern editor** — Create and test regex tool-routing patterns
- **Memory browser** — Search and manage long-term memories
- **Job scheduler** — Create, pause, resume, delete scheduled tasks
- **MCP server management** — Connect/disconnect MCP servers dynamically
- **Settings** — Configure all agent behavior from the UI
- **File management** — Upload and download files through the dashboard

---

## Project Structure

```
Axon/
├── axon/                          # Axon Agent (core)
│   ├── src/
│   │   ├── main.rs                # Entry point, initialization
│   │   ├── state.rs               # Shared application state
│   │   ├── agent/
│   │   │   ├── loop.rs            # Agent loop (think → act → observe)
│   │   │   ├── context.rs         # Run context and event types
│   │   │   └── tool_writer.rs     # Dynamic tool generation
│   │   ├── config/                # Configuration loading (TOML, env, DB)
│   │   ├── dashboard/
│   │   │   ├── server.rs          # Axum router setup
│   │   │   ├── api.rs             # REST API endpoints
│   │   │   └── ws.rs              # WebSocket handler
│   │   ├── memory/
│   │   │   ├── short_term.rs      # Session-based conversation memory
│   │   │   ├── long_term.rs       # Persistent FTS5 memory
│   │   │   ├── compressor.rs      # AI-powered observation compression
│   │   │   ├── embeddings.rs      # Voyage AI embedding client
│   │   │   └── store.rs           # Unified memory interface
│   │   ├── messaging/
│   │   │   ├── telegram.rs        # Telegram bot
│   │   │   ├── discord.rs         # Discord gateway
│   │   │   ├── slack.rs           # Slack webhook
│   │   │   ├── streaming.rs       # Streaming response handler
│   │   │   └── gateway.rs         # Messaging hub
│   │   ├── providers/
│   │   │   ├── openai_compat.rs   # OpenAI-compatible API (Gemini, Groq, Cerebras, OpenRouter)
│   │   │   ├── anthropic.rs       # Anthropic Claude API
│   │   │   ├── ollama.rs          # Ollama API
│   │   │   └── types.rs           # Unified message/response types
│   │   ├── router/
│   │   │   ├── model_router.rs    # Multi-model routing with failover
│   │   │   └── tool_router.rs     # LLM-powered tool selection
│   │   ├── scheduler/
│   │   │   ├── engine.rs          # Cron scheduler engine
│   │   │   ├── store.rs           # Job persistence
│   │   │   └── nl_parser.rs       # Natural language → cron parser
│   │   ├── tools/
│   │   │   ├── registry.rs        # Tool registration and management
│   │   │   ├── runner.rs          # Python tool runner (subprocess)
│   │   │   ├── schema.rs          # Tool definition schema
│   │   │   ├── ssh.rs             # Built-in SSH tool
│   │   │   └── file_handler.rs    # File upload/download handler
│   │   ├── mcp/
│   │   │   └── client.rs          # MCP client (connects to MCP servers)
│   │   └── files.rs               # File staging and cleanup
│   ├── config/
│   │   ├── models.toml            # Model configuration
│   │   └── ssh_servers.json       # SSH server definitions
│   ├── memory/
│   │   └── schema.sql             # Database schema
│   ├── static/                    # Dashboard frontend (HTML/CSS/JS)
│   ├── tools/                     # Python tool scripts
│   └── .env                       # Environment variables
│
├── axon-mcp-server/               # Axon MCP Server
│   ├── src/
│   │   └── main.rs                # MCP server entry point + HTTP/SSE transport
│   ├── crates/
│   │   ├── axon-core/             # Shared state, OAuth, token storage
│   │   ├── axon-google/           # Google Workspace (Gmail, Calendar, Drive)
│   │   ├── axon-microsoft/        # Microsoft 365 (Outlook, Calendar, OneDrive, Teams)
│   │   ├── axon-facebook/         # Facebook (Pages, Posts, Comments, Insights, Messenger)
│   │   └── axon-business/         # Business utilities (Notes, Tasks, Contacts, Web, DateTime, Text)
│   └── credentials.example.json   # OAuth credentials template
│
├── deploy.sh                      # Build + bundle + deploy script
└── README.md                      # This file
```

---

## Axon Agent

### Agent Loop

The agent follows an iterative **Think → Act → Observe** loop:

1. **Receive task** — From any platform (dashboard, Telegram, Discord, Slack, API)
2. **Search memory** — Retrieve relevant long-term memories and recent observations
3. **Route tools** — LLM-powered tool selection based on the task
4. **Select model** — Role-based model routing with automatic failover
5. **Generate response** — LLM produces text and/or tool calls
6. **Execute tools** — Run tools in parallel, collect results
7. **Compress observations** — AI summarizes tool results for future context
8. **Quality check** — Validate response for fabrication or critical errors
9. **Repeat or respond** — Continue iterating or deliver final answer

Each run is logged with iteration details, models used, tools called, and token counts.

### Model Router

The model router manages multiple LLM providers and API keys with intelligent failover:

- **Multi-key pooling** — Multiple API keys per provider, round-robin selection
- **Role-based routing** — Different models for different task types:
  - `complex_tasks` — Main reasoning (Gemini Flash, GPT-OSS-120B, Qwen-3-235B)
  - `simple_tasks` — Quick responses (Cerebras Llama)
  - `router` — Fast tool selection (Cerebras Qwen)
  - `quality_checker` — Response validation (Gemini Flash Lite)
  - `memory_compressor` — Observation compression (Cerebras Llama-8B)
  - `paid_model` — Ultimate fallback for high-priority tasks
- **Automatic failover** — On rate limit or error, marks model unavailable and tries the next
- **Cooldown recovery** — Rate-limited models auto-recover after configurable cooldown

### Tool System

Axon supports four types of tools:

| Type | Description | Location |
|---|---|---|
| **Python** | Custom tools written as Python scripts | `tools/*.py` |
| **Internal** | Built into the Rust binary (SSH, memory, scheduler, parallel worker) | `src/tools/` |
| **MCP** | Tools provided by connected MCP servers | Remote |
| **Temporary** | Agent-generated tools written on the fly | `tools_temp/*.py` |

The **Tool Router** uses two methods to select which tools to present to the LLM:
1. **Regex pattern matching** — Fast first pass using configurable patterns (e.g., `\b(my\s+)?email\b` → `gmail_list`)
2. **LLM classification** — Secondary pass using a fast model to classify the task and select appropriate tools

### Memory System

| Layer | Storage | Purpose |
|---|---|---|
| **Short-term** | SQLite table | Per-session conversation history (last N messages) |
| **Long-term** | SQLite FTS5 | Persistent knowledge, searchable by full-text |
| **Observations** | SQLite FTS5 | AI-compressed learnings from tool results |

The memory compressor runs in the background after each tool call, summarizing raw results into concise observations. These observations are injected into future agent prompts as context.

### Scheduler

The built-in scheduler allows the agent (or the user) to create recurring tasks:

- **Natural language** → Cron expression conversion ("every 5 minutes" → `*/5 * * * *`)
- **Job lifecycle** — Create, pause, resume, delete
- **Stop conditions** — Max runs, end date, or custom condition
- **Platform-aware** — Results are delivered back to the originating platform (Telegram, Discord, etc.)

### Dashboard

The web dashboard is a full-featured management UI served at `http://localhost:3000`:

- **Chat** — Real-time conversation with streaming progress (iterations, tool calls, model selection)
- **Runs** — Browse and inspect past agent runs
- **Models** — CRUD management for all LLM providers
- **Tools** — View, enable/disable, reload tools
- **Patterns** — Create and test regex routing patterns
- **Memory** — Browse and search/delete long-term memories
- **Jobs** — Manage scheduled tasks
- **MCP** — Connect/disconnect MCP tool servers
- **Messaging** — View status of connected messaging platforms
- **Settings** — Configure all agent behavior

### Messaging Integrations

| Platform | Transport | Features |
|---|---|---|
| **Telegram** | Long-polling | Text, file upload/download, streaming updates |
| **Discord** | Gateway WebSocket | Text, streaming updates |
| **Slack** | Events API (webhook) | Text, threaded replies |
| **Dashboard** | WebSocket | Full streaming with iteration details |

---

## Axon MCP Server

The MCP server exposes 50+ tools via the [Model Context Protocol](https://modelcontextprotocol.io/) standard. It supports both **HTTP+SSE** and **stdio** transports.

### Google Workspace

| Tool | Description |
|---|---|
| `google_auth_status` | Check Google OAuth status |
| `google_auth_url` | Get OAuth authorization URL |
| `gmail_list` | List inbox emails |
| `gmail_read` | Read a specific email |
| `gmail_search` | Search emails |
| `gmail_send` | Send an email |
| `gmail_send_with_attachment` | Send email with file attachment |
| `gmail_reply` | Reply to an email |
| `gmail_trash` | Trash an email |
| `gcal_list_events` | List calendar events |
| `gcal_create_event` | Create a calendar event |
| `gcal_delete_event` | Delete a calendar event |
| `gdrive_list` | List files in Google Drive |
| `gdrive_search` | Search for files |
| `gdrive_read` | Read file content |
| `gdrive_upload_text` | Upload text content |
| `gdrive_upload_binary` | Upload binary files |
| `gdrive_download` | Download a file |

### Microsoft 365

| Tool | Description |
|---|---|
| `microsoft_auth_status` | Check Microsoft OAuth status |
| `microsoft_auth_url` | Get OAuth authorization URL |
| `outlook_list_emails` | List Outlook inbox |
| `outlook_read_email` | Read a specific email |
| `outlook_search` | Search emails |
| `outlook_send_email` | Send an email |
| `outlook_send_with_attachment` | Send email with attachment |
| `outlook_reply` | Reply to email |
| `outlook_trash` | Trash an email |
| `mscal_list_events` | List calendar events |
| `mscal_create_event` | Create calendar event |
| `mscal_delete_event` | Delete calendar event |
| `onedrive_list` | List OneDrive files |
| `onedrive_search` | Search OneDrive |
| `onedrive_read` | Read file content |
| `onedrive_upload_text` | Upload text content |
| `onedrive_upload_binary` | Upload binary files |
| `onedrive_download` | Download a file |
| `teams_send_message` | Send a Teams message |

### Facebook / Meta

| Tool | Description |
|---|---|
| `facebook_auth_status` | Check Facebook auth status |
| `facebook_auth_url` | Get OAuth authorization URL |
| `fb_get_page` | Get page details |
| `fb_create_post` | Create a page post |
| `fb_list_posts` | List page posts |
| `fb_delete_post` | Delete a post |
| `fb_get_comments` | Get comments on a post |
| `fb_reply_comment` | Reply to a comment |
| `fb_get_insights` | Get page analytics/insights |
| `fb_get_conversations` | List Messenger conversations |
| `fb_send_message` | Send a Messenger message |

### Business Tools

| Tool | Description |
|---|---|
| `note_create` | Create a note |
| `note_list` | List all notes |
| `note_read` | Read a note |
| `note_update` | Update a note |
| `note_delete` | Delete a note |
| `task_create` | Create a task |
| `task_list` | List all tasks |
| `task_update` | Update a task |
| `task_delete` | Delete a task |
| `contact_create` | Create a contact |
| `contact_list` | List contacts |
| `contact_search` | Search contacts |
| `contact_update` | Update a contact |
| `contact_delete` | Delete a contact |
| `web_request` | Make HTTP requests |
| `datetime_now` | Get current date/time |
| `datetime_convert` | Convert between timezones |
| `text_summarize` | Summarize text |
| `text_translate` | Translate text |

---

## Getting Started

### Prerequisites

- **Rust** 1.75+ (with `cargo`)
- **At least one LLM API key** (Gemini, OpenRouter, Groq, Cerebras, OpenAI, or Anthropic)
- **Python 3.8+** (only if using Python-based tools)
- **For MCP OAuth tools**: Google Cloud Console / Microsoft Azure / Meta Developer credentials

### Configuration

#### 1. Axon Agent

```bash
cd axon
cp .env.example .env
```

Edit `.env` with your API keys:

```env
# Required: At least one LLM provider
GEMINI_API_KEY_1=your_gemini_key

# Optional: Additional providers for failover
OPENROUTER_API_KEY=your_openrouter_key
CEREBRAS_API_KEY=your_cerebras_key
GROQ_API_KEY=your_groq_key

# Optional: Messaging platforms
TELOXIDE_TOKEN=your_telegram_bot_token
DISCORD_TOKEN=your_discord_bot_token
SLACK_BOT_TOKEN=xoxb-your-slack-token

# Optional: Embeddings
VOYAGE_API_KEY=your_voyage_key

# Server config
AXON_PORT=3000
AXON_DB_PATH=memory/axon.db
RUST_LOG=axon=info,tower_http=info
```

#### 2. Model Configuration

Edit `config/models.toml` to configure your LLM providers:

```toml
[[models]]
name       = "gemini-main"
provider   = "google"              # google, openrouter, cerebras, groq, ollama, openai, anthropic
model_id   = "gemini-2.5-flash"
api_key    = "${GEMINI_API_KEY_1}"  # References .env variable
role       = "complex_tasks"       # complex_tasks, simple_tasks, router, quality_checker, memory_compressor, paid_model
priority   = 1                     # Lower = tried first
max_tokens = 4096
enabled    = true
```

API keys in `models.toml` use the `${ENV_VAR_NAME}` syntax to reference environment variables or dashboard settings.

#### 3. Axon MCP Server

```bash
cd axon-mcp-server
cp credentials.example.json credentials.json
```

Edit `credentials.json` with your OAuth app credentials:

```json
{
  "google": {
    "client_id": "your-google-client-id.apps.googleusercontent.com",
    "client_secret": "GOCSPX-your-google-secret"
  },
  "microsoft": {
    "client_id": "your-azure-app-id",
    "client_secret": "your-azure-secret",
    "tenant_id": "common"
  },
  "facebook": {
    "app_id": "your-meta-app-id",
    "app_secret": "your-meta-app-secret",
    "page_id": "your-page-id",
    "page_access_token": "your-page-token"
  }
}
```

#### 4. SSH Servers (Optional)

Edit `config/ssh_servers.json`:

```json
{
  "servers": [
    {
      "name": "production",
      "host": "192.168.1.100",
      "port": 22,
      "username": "ubuntu",
      "key_path": "config/ssh_keys/production.key"
    }
  ]
}
```

### Building

#### Build locally (development)

```bash
# Axon Agent
cd axon && cargo build --release

# MCP Server
cd axon-mcp-server && cargo build --release
```

#### Build for deployment (WSL/Linux)

The included `deploy.sh` handles everything:

```bash
# Full pipeline: build → bundle → deploy to server
bash deploy.sh

# Clean rebuild + deploy
bash deploy.sh --clean

# Build + bundle only (no deploy)
bash deploy.sh --skip-deploy

# Deploy existing bundle (no rebuild)
bash deploy.sh --skip-build
```

### Running Locally

```bash
# Terminal 1: Start MCP Server
cd axon-mcp-server
cargo run

# Terminal 2: Start Axon Agent
cd axon
cargo run
```

Then open `http://localhost:3000` in your browser.

Connect the MCP server from the dashboard: **MCP tab → Add Server → URL: `http://127.0.0.1:8080`**

### Deploying to Production

1. Configure `deploy.sh` with your server details:
   ```bash
   KEY="$ROOT_DIR/your-server.key"
   TARGET_SERVER="ubuntu@your-server-ip"
   ```

2. Run the deployment:
   ```bash
   bash deploy.sh
   ```

3. On first deploy, install systemd services:
   ```bash
   ssh -i your-server.key ubuntu@your-server-ip
   sudo ./run.sh --install
   ```

The deploy script creates a `axon_deploy.tar.gz` containing both binaries, config files, the dashboard, and a `run.sh` service manager that supports:

```bash
./run.sh start      # Start services
./run.sh stop       # Stop services
./run.sh restart    # Restart services
./run.sh status     # Check service status
./run.sh --install  # Install as systemd services
```

---

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `AXON_PORT` | No | Dashboard port (default: `3000`) |
| `AXON_DB_PATH` | No | SQLite database path (default: `memory/axon.db`) |
| `RUST_LOG` | No | Log level (default: `axon=info,tower_http=info`) |
| `GEMINI_API_KEY_*` | Yes* | Google Gemini API keys |
| `OPENROUTER_API_KEY` | No | OpenRouter API key |
| `CEREBRAS_API_KEY` | No | Cerebras API key |
| `GROQ_API_KEY` | No | Groq API key |
| `OPENAI_API_KEY` | No | OpenAI API key |
| `ANTHROPIC_API_KEY` | No | Anthropic API key |
| `VOYAGE_API_KEY` | No | Voyage AI embedding key |
| `TELOXIDE_TOKEN` | No | Telegram bot token |
| `DISCORD_TOKEN` | No | Discord bot token |
| `SLACK_BOT_TOKEN` | No | Slack bot token |
| `AXON_CALLBACK_HOST` | No | MCP server callback URL (default: `http://localhost:8080`) |

\* At least one LLM provider API key is required.

---

## Model Configuration

Models are configured in `config/models.toml` and synced to the SQLite database at startup. The TOML file is the source of truth — models removed from it are also removed from the database.

### Supported Providers

| Provider | `provider` value | Base URL |
|---|---|---|
| Google Gemini | `google` | (built-in) |
| OpenRouter | `openrouter` | `https://openrouter.ai/api/v1` |
| Cerebras | `cerebras` | `https://api.cerebras.ai/v1` |
| Groq | `groq` | `https://api.groq.com/openai/v1` |
| Ollama | `ollama` | `https://ollama.com` or local URL |
| OpenAI | `openai` | (built-in) |
| Anthropic | `anthropic` | (built-in) |

### Model Roles

| Role | Purpose | Recommended Model |
|---|---|---|
| `complex_tasks` | Main reasoning, tool calling | Gemini 2.5 Flash, GPT-4o |
| `simple_tasks` | Quick conversational replies | Cerebras Qwen, Llama |
| `router` | Fast tool selection (internal) | Cerebras Qwen-3 |
| `quality_checker` | Response validation | Gemini Flash Lite |
| `memory_compressor` | Observation summarization | Cerebras Llama-8B |
| `paid_model` | Ultimate fallback | Any paid model |
| *(empty)* | General pool | Any model |

---

## API Reference

All endpoints are prefixed with `/api` and served on the agent's port (default `3000`).

### Chat & Runs
| Method | Endpoint | Description |
|---|---|---|
| `WS` | `/ws` | WebSocket for real-time streaming chat |
| `POST` | `/api/run` | Execute a task via REST API |
| `GET` | `/api/runs` | List past runs (supports `?limit=`, `?offset=`, `?status=`) |
| `GET` | `/api/runs/:id` | Get run details with iterations and tool calls |

### Models
| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/models` | List all models with status |
| `POST` | `/api/models` | Add a new model |
| `PUT` | `/api/models/:name` | Update model config |
| `DELETE` | `/api/models/:name` | Delete a model |
| `POST` | `/api/models/:name/reset` | Reset model error state |

### Tools
| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/tools` | List all registered tools |
| `PUT` | `/api/tools/:name` | Enable/disable a tool |
| `POST` | `/api/tools/reload` | Reload tools from disk |

### Patterns
| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/patterns` | List tool routing patterns |
| `POST` | `/api/patterns` | Add a new pattern |
| `PUT` | `/api/patterns/:id` | Toggle pattern enabled/disabled |
| `DELETE` | `/api/patterns/:id` | Delete a pattern |
| `POST` | `/api/patterns/test` | Test routing for a given input |

### Memory
| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/memory/recent` | List recent memories |
| `POST` | `/api/memory/search` | Search memories by query |
| `DELETE` | `/api/memory/:id` | Delete a memory |

### Jobs
| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/jobs` | List scheduled jobs |
| `POST` | `/api/jobs` | Create a new job |
| `PUT` | `/api/jobs/:id` | Update a job |
| `POST` | `/api/jobs/:id/run` | Trigger a job run now |
| `POST` | `/api/jobs/:id/pause` | Pause a job |
| `POST` | `/api/jobs/:id/resume` | Resume a job |
| `DELETE` | `/api/jobs/:id/delete` | Delete a job |

### MCP & Messaging
| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/mcp` | List connected MCP servers |
| `POST` | `/api/mcp` | Connect a new MCP server |
| `DELETE` | `/api/mcp/:name` | Disconnect an MCP server |
| `GET` | `/api/messaging/status` | Get messaging platform statuses |
| `POST` | `/api/messaging/reconnect/:platform` | Reconnect a messaging platform |

### Settings & Files
| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/settings` | Get all settings |
| `POST` | `/api/settings` | Update a setting |
| `PUT` | `/api/settings/:key` | Update setting by key |
| `GET` | `/api/files/:dir` | List files (incoming/outgoing) |
| `GET` | `/api/download` | Download a file |
| `POST` | `/api/upload` | Upload a file |

---

## Tech Stack

- **Language**: Rust (2021 edition)
- **Web Framework**: Axum 0.7
- **Database**: SQLite (via rusqlite + r2d2 pool) with FTS5, WAL mode
- **Async Runtime**: Tokio
- **SSH**: russh (pure Rust SSH/SFTP)
- **MCP Protocol**: rmcp crate
- **Frontend**: Vanilla HTML/CSS/JS (no framework, no build step)

---

## License

Private project. All rights reserved.
