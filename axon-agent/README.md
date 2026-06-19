# Axon 🧠

Axon is a highly autonomous, multi-platform AI agent ecosystem written entirely in strict **Rust** utilizing `tokio` for massive asynchronous I/O concurrency.

Designed to act as a self-correcting digital assistant (or highly customized persona), Axon seamlessly handles webhooks across multiple platforms, securely operates terminal shells, routes LLM requests efficiently across global providers, and maintains deep semantic context via isolated memory pipelines.

## 🌟 Core Features

### 🔌 Multi-Channel Webhooks & Messaging
Axon bridges the gap between raw LLM intelligence and real-world communication avenues natively:
- **Facebook / Meta**: Automatically reads and intelligently replies to Page Comments and Messenger Chats with engineered human-like delays and context-awareness.
- **Telegram & Discord**: Directly interfaces with chat bots to notify owners, summarize events, or actively converse.
- **Slack**: Enterprise webhook integrations for operations tracking.

### 🧠 Unified AI Memory Architecture
Context tracking is physically split into highly specific vectors to prevent context pollution:
- **Short-Term Memory (SQLite)**: Tracks fast, persistent conversation threads mapped per unique `chat_id` or `user_id`.
- **Long-Term Memory (Qdrant)**: Embeds conversational milestones, facts, and documents into a Vector Database for fast semantic retrieval during future conversations.

### ⚖️ Priority-Aware Load Balancing
Axon does not rely on a single LLM API. The embedded **Model Router** securely stores dozens of API keys across diverse providers *(Google Gemini, Anthropic, Groq, NVIDIA, Cerebras, OpenRouter, Ollama)*.
- **Stateful Priority Scaling**: Assign Priority Tiers (`1`, `2`, `3`) to specific models. Axon enforces mathematically perfect sequential load distribution purely across the highest available tier to evenly spread API usage.
- **Rate-Limit Protections**: If a provider triggers a `429 Too Many Requests`, Axon dynamically quarantines that model, seamlessly falls back to the next available tier without dropping the user's request, and automatically resurrects the locked model exactly when its cooldown logically expires.

### 🛠️ Native System Tools & MCP
Axon acts as the operating intelligence for your local and remote infrastructure:
- **Native Shell Tooling**: Secure, asynchronous `bash`/`powershell` execution explicitly piped through `tokio::process`. Processes are stream-harvested and feature dynamic `tokio::select!` timeout constraints to prevent infinite loops (like `top`) from destroying the agent reasoning loops.
- **Remote SSH Administration**: Securely manage external servers. Credentials (Passwords/Keys) are fully encrypted into SQLite. Axon securely opens tunnels, pushes execution commands, and manages files contextually natively.
- **Model Context Protocol (MCP)**: Native integration for arbitrarily expanding the agent's capabilities via standardized external tool APIs.
- **Quality Assurance Checking**: Employs a secondary fast-LLM pipeline strictly to audit the primary Agent's outputs, correcting hallucinations or tool misuse before sending messages publicly.

### 🖥️ Real-time Web Dashboard
A beautiful, mobile-friendly **Axum HTTP Web Dashboard** that grants total oversight over the agent:
- **Model Key Management**: Dynamically add models, rotate encrypted API keys, or shift Priority logic without restarting the binary.
- **SSH Credentials Management**: Securely add bare-metal servers, bind authentication strategies, and expose them as endpoints to the Agent's SSH array.
- **Memory & Log Viewer**: Live WebSocket connection broadcasting reasoning queues and tool evaluations seamlessly. Directly interact with Axon via the Dashboard Sandbox UI.
- **Agent Prompts**: Dynamically swap out the deep underlying System Prompts overriding Axon's behavior in real-time.

---

## 🚀 Getting Started

### Prerequisites
- **Rust Toolchain**: 1.70+
- **SQLite3**: For short-term caching & configuration databases.
- **Qdrant**: A running Qdrant Vector instance (Local or Cloud) for Deep Memory clustering.

### Environment & Configuration
Axon's dashboard manages all internal logic natively, but you can bootstrap your environment variables via a standard `.env` configuration mapping your foundational variables (like `PORT` and `QDRANT_URL`).

### Build & Run
```bash
cargo build --release
cargo run --release
```
_By default, the dashboard and API listeners will map to `127.0.0.1:3000`._

---

## 🔒 Security & Sandboxing

Axon is built with deep guardrails against standard LLM exploitation vectors:
- **Command Sanitization**: Native shell and SSH tools execute hard-coded regex restrictions forbidding destructive command structures (e.g. `rm -rf`, `chmod`, `chown`) natively without requiring LLM cooperation.
- **Encryption**: All API keys, SSH Tokens, and Private Keys supplied via the Dashboard are inherently encrypted using robust Rust crypto wrappers prior to striking the SQLite disk.
- **Sandboxed Execution**: Background pipelines securely lock execution threads using explicit Mutex structures to prevent parallel race conditions mapping out to standard system architecture.
