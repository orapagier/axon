## Universal Notification Hub (Option 3)

### Problem
Background work (scheduler jobs, watchers, runtime errors) either silently drops output or only reaches Telegram. The dashboard bell is localStorage-only and only listens to WS events from the Chat page — so failures like "task ran but `watcher.notify_chat_id` is empty" are invisible everywhere except server logs.

### Goal
A server-backed notification system: notifications persist in SQLite, broadcast to every connected dashboard client, and show in the bell on every page. Sources: scheduler (jobs), error reporting (the existing funnel all background paths already use), and — as the primary trigger for this work — job delivery failures.

### Architecture

A new `NotifyHub` lives on `AppState` (alongside `messaging`). It does two things on every notification:
1. **Persist** → insert into `notifications` table (source, level, title, message, read, created_at).
2. **Broadcast** → `tokio::broadcast::Sender<AgentEvent>` (reuses the existing `Notification` variant — empty `run_id` so the frontend's run-scoped guard at `ChatPage.vue:326` doesn't drop it).

Sending to Telegram stays where it is — the hub is **additive**, not a replacement for delivery. Every WS socket subscribes to the broadcast channel in `handle_socket` and forwards broadcasts to the browser alongside run-scoped events. The bell store is rewritten to be **server-backed** (load from `/api/notifications`, accept WS pushes, mark-read/delete hit the server).

### Key existing pieces this reuses
- `AgentEvent::Notification` variant already exists (`agent/context.rs:156`) with the exact JSON shape the frontend already handles.
- `ChatPage.vue:325-335` already routes `notification` WS events into the bell via `notifyBell`.
- `error_reporting.rs::send_global_error_notification` is already the single funnel all background error paths use (agent loop, watcher triage, workflows) — hooking it once covers all three.
- Migration system is versioned & idempotent (`db/mod.rs:208`); new tables go in numbered files.
- `SchedulerEngine` already holds `Arc<MessagingHub>` directly — adding `Arc<NotifyHub>` follows the identical pattern.

---

### Implementation

#### 1. DB migration — `crates/axon-agent/src/db/migrations/0028_notifications.sql` (new)
```sql
CREATE TABLE IF NOT EXISTS notifications (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    source     TEXT NOT NULL,          -- 'scheduler' | 'watcher' | 'agent.runtime' | 'agent.router' | 'workflow' | 'system'
    level      TEXT NOT NULL DEFAULT 'info',  -- 'info' | 'warning' | 'error'
    title      TEXT NOT NULL DEFAULT '',
    message    TEXT NOT NULL,
    read       INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_notifications_created ON notifications(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notifications_unread ON notifications(read, created_at DESC);
```
Register in the `MIGRATIONS` array (`db/mod.rs:~192`) as version 28. Add `"notifications"` to the idempotency test's table list.

#### 2. `NotifyHub` — `crates/axon-agent/src/notify.rs` (new)
```rust
pub struct NotifyHub {
    db: Arc<Pool<SqliteConnectionManager>>,
    tx: broadcast::Sender<AgentEvent>,   // capacity 256; lagging clients skip stale
}
impl NotifyHub {
    pub fn new(db) -> Arc<Self>;          // constructs the broadcast channel internally
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    pub async fn emit(&self, source, level, title, message);  // persist + broadcast
    // CRUD for REST API:
    pub fn list(&self, only_unread: bool, limit) -> Result<Vec<NotificationRow>>;
    pub fn mark_read(&self, id: Option<i64>);   // None = all
    pub fn delete(&self, id: Option<i64>);      // None = all
    pub fn unread_count(&self) -> Result<i64>;
}
```
- `emit` does the DB insert via `tokio::task::spawn_blocking` (r2d2 is blocking) and broadcasts `AgentEvent::Notification { run_id: String::new(), level, title, message }`.
- Broadcast send errors (no receivers / lag) are silently OK — persistence is the source of truth.

#### 3. Wire into `AppState` — `state.rs` + `main.rs`
- Add `pub notify: Arc<NotifyHub>` to `AppState` (clone-cheap).
- Construct the hub in `main.rs` just before the `AppState` literal (~L508); pass `Arc<NotifyHub>` into `SchedulerEngine::new` alongside the existing `Arc<MessagingHub>`.

#### 4. Subscribe sockets — `dashboard/ws.rs`
In `handle_socket`, after splitting the socket, take `state.notify.subscribe()` and `tokio::select!` between the existing per-run `rx.recv()` and the broadcast receiver, forwarding both to `sender`. Empty-`run_id` broadcasts pass through ChatPage's guard correctly.

#### 5. Emit points — rewire the funnels (additive; existing Telegram sends stay)

**a. `error_reporting.rs::send_global_error_notification`** — after the existing Telegram send (success *or* failure), call `state.notify.emit(source, level, summary, details)`. This automatically covers agent-loop router/runtime errors, watcher triage failures, and workflow failures (`tools/workflow.rs:3012,3330`).

**b. `scheduler/engine.rs`** — two notification branches (cron fire ~L247-295, `run_once` ~L346-405). After delivery:
- Success → `emit("scheduler", "info", job_name, "Sent to {platform}")`
- Discarded (empty chat_id) → `emit("scheduler", "warning", job_name, "Output discarded — no notify_chat_id configured. Set watcher.notify_chat_id.")` ← **the bug you hit**
- Send failed → `emit("scheduler", "error", job_name, "Delivery to {platform} failed: {e}")`

**c. `watcher/engine.rs::send_notification`** (~L1480) — after the existing Telegram send, emit a `watcher`/`info` notification with the triage text, so hits are reload-safe and visible beyond the moment of the toast.

**d. Gateway-down detection (optional, lowest priority)** — when scheduler/watcher find `messaging.telegram` is `None`, emit a one-time `system`/`warning` so the user learns *why* nothing arrives. Guarded so it doesn't spam.

#### 6. REST API — `crates/axon-agent/src/dashboard/api/notifications.rs` (new) + routes
- `GET    /api/notifications?unread=1&limit=50` → list
- `GET    /api/notifications/unread_count` → badge polling on reconnect
- `POST   /api/notifications/mark_read` `{id?}` → mark one or all
- `DELETE /api/notifications` `{id?}` → delete one or all

Register routes in `dashboard/server.rs`.

#### 7. Frontend — server-backed bell + app-wide WS

**`axon-ui/src/lib/notifications.js`** — rewrite from localStorage to server-backed store:
- `notifications` ref loads from `GET /api/notifications` on init.
- `addNotification` becomes a WS-event handler (no longer a public write API).
- `markRead` / `deleteNotification` / `clearAllNotifications` call the REST endpoints.
- On reconnect (WS status flips to `connected`), re-fetch to catch anything missed while offline.

**`axon-ui/src/lib/ws.js`** — replace the single `eventHandler` with a `Set<handler>` and expose `subscribe(handler) -> unsub`. `connectWs()` becomes arg-less and connects on module load (app-wide). Keeps the single shared socket that already exists.

**`axon-ui/src/App.vue`** — call `connectWs()` on mount and `subscribe` a handler that routes `type === 'notification'` into the bell store (`ev.title`/`ev.message`/`ev.level` → ok flag, matching ChatPage's existing contract).

**`axon-ui/src/components/NotificationBell.vue`** — minimal changes: bind to the rewritten store; keep existing UI. Unread badge already reactive.

**`axon-ui/src/pages/ChatPage.vue:1304`** — change `connectWs(handleWsEvent)` to `subscribe(handleWsEvent)` (new multi-subscriber API).

---

### Out of scope (explicitly)
- Migrating existing localStorage notifications (ephemeral toast history — start fresh).
- Per-job notification targeting from the Tasks page UI — can layer on later now that emit points exist.
- Email/push delivery (Telegram/Discord/Slack already exist; the hub is dashboard-only).

### Testing
- Unit: `NotifyHub` persist + broadcast round-trip on a fresh in-memory DB (extend the `fresh_db_initializes_and_is_idempotent` pattern).
- Integration: scheduler fires with empty `notify_chat_id` → bell receives a `warning`; mark-read clears unread count.
- Manual: open dashboard on two tabs, trigger a job from the Tasks page "Run" button on tab A, confirm the bell updates on tab B.

### Files touched
- **New (3 files):** `db/migrations/0028_notifications.sql`, `crates/axon-agent/src/notify.rs`, `crates/axon-agent/src/dashboard/api/notifications.rs`
- **Backend edits:** `db/mod.rs`, `state.rs`, `main.rs`, `dashboard/ws.rs`, `dashboard/server.rs`, `error_reporting.rs`, `scheduler/engine.rs`, `watcher/engine.rs`
- **Frontend edits:** `lib/notifications.js` (rewrite), `lib/ws.js` (multi-subscriber), `App.vue`, `pages/ChatPage.vue` (one-line), `components/NotificationBell.vue` (minor)

### Rollout / risk
Non-breaking. Existing Telegram delivery unchanged — the hub is purely additive. The bell gains a server-backed history and app-wide reach. Migration 0028 is idempotent. If the broadcast has no subscribers, notifications still persist and surface on next load.

### Build verification (before marking done)
`cargo build -p axon-agent`, `cd axon-ui && npm run build`, then the existing test suite.