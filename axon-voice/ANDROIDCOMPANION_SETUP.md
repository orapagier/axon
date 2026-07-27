# Device Control (AndroidCompanion) — axon-agent Integration Guide

The device-control HTTP API (SMS, calls, contacts, camera, files, shell via Termux,
device settings, alarms) now ships **inside the Axon app** alongside the voice UI —
one APK, not two. This doc covers wiring it up so axon-agent (your Axon server) can
actually call it.

## Architecture Overview

```
Your Phone — one app (Axon)
──────────────────────────────────────────────────────────────
Axon
  ├── Voice UI (ChatActivity, WakeWordService)  — talks to axon-agent over /ws
  │
  ├── Device-control HTTP server (Ktor)         bound to 127.0.0.1:<port> — device-local only
  ├── Embedded cloudflared       spawned as a subprocess, connects out to
  │   (CloudflaredManager.kt)    Cloudflare's edge, no inbound port opened
  │        │
  │        ▼
  │   Cloudflare Tunnel ─────────────────────▶  Cloud
  │   phone.yourdomain.com                       │
  │                                               │ axon-agent (once wired, see Step 2):
  ├── Proactive pushes: ─────────────────────────▶  event ingest
  │     SmsReceiver, CallStateReceiver,               - sms_received
  │     BatteryReceiver, location updates             - call_incoming / call_missed
  │                                                    - location_changed
  │                                                    - battery_low
  └── axon-agent pulls/commands ◀──────────────── GET/POST/PUT/DELETE /... via the
                                                    credentials + synapse tool (Step 3)
```

Nothing listens on the LAN. The only way in from outside the device is through the
Cloudflare Tunnel; the only way in from *this* device is `127.0.0.1`. See "Security model"
below before wiring an LLM into this.

---

## Step 1 — One-time setup

1. Install the merged Axon app, grant the permissions it asks for.
2. **Tunnel:** on your dev machine, run `bash scripts/fetch_cloudflared.sh` from the
   axon-voice project root (downloads the `cloudflared` binaries), then rebuild and
   install the app.
3. Create a tunnel at **one.dash.cloudflare.com → Networks → Tunnels → Create a tunnel**
   → Cloudflared connector → copy the **token**. While there, add a **Public Hostname**
   pointing at `http://127.0.0.1:8080` (or whichever port is configured).
4. Open Axon → Settings → **Device control** → paste the tunnel token under
   **Cloudflare Tunnel** → Save & Restart Tunnel.
5. Tap the **Bearer token** to copy it — every API request needs it.

```bash
curl https://phone.yourdomain.com/ping
# → {"ok":true,"ts":1712345678000}

curl https://phone.yourdomain.com/status \
  -H "Authorization: Bearer YOUR_TOKEN"
```

---

## Step 2 — Proactive push events (phone → axon-agent)

The phone can proactively POST events (SMS received, calls, location, low battery) to
any HTTP URL you configure as the **Webhook URL** (Device control screen, or
`POST /config` with `{"webhook_url": "..."}`).

**Event payload:**
```json
{
  "event": "sms_received",
  "timestamp": 1712345678000,
  "data": { "from": "+639171234567", "body": "Hello!", "timestamp": 1712345678000 }
}
```

| Event              | Trigger                                    | Key `data` fields |
|--------------------|---------------------------------------------|-------------------|
| `sms_received`     | Incoming SMS                               | `from`, `body`, `timestamp` |
| `call_incoming`    | Phone starts ringing                       | `number`, `name` |
| `call_missed`      | Ringing stopped, never answered            | `number`, `name` |
| `location_changed` | Moved >50m from last pushed location       | `latitude`, `longitude`, `accuracy_meters`, `maps_url` |
| `battery_low`      | Battery ≤ threshold (default 20%)          | `percent`, `charging`, `plugged` |

**Wiring it up:** axon-agent already has a generic external-webhook receiver —
`POST /webhook/external/:workflow_id` (see `crates/axon-agent/src/webhook/external.rs`).
Build a workflow that starts with a Webhook trigger, point the phone's **Webhook URL**
at `https://<axon-agent-host>/webhook/external/<that-workflow-id>`, and branch on the
incoming `event` field. No new server code needed — just a workflow.

---

## Step 3 — axon-agent calling the phone (commands / tool use)

This is the direction that actually lets you say "text my landlord" to Axon and have
it happen. It uses axon-agent's existing generic `credentials` + `synapse` tools — no
new server code needed. Two things below differ from what you'd guess from the UI —
verified against `crates/axon-agent/src/agent/internal_tools.rs` and `tools/http.rs`.

1. In the Axon dashboard (Services page, or `POST /api/credentials`), save a credential:
   ```json
   {
     "name": "AndroidCompanion phone",
     "service": "androidcompanion",
     "data": { "bearer_token": "YOUR_TOKEN", "base_url": "https://phone.yourdomain.com" }
   }
   ```
   `bearer_token` is the field name the credential resolver actually looks for
   (`apply_http_credential` in `internal_tools.rs`). `base_url` is stored for reference
   only — see the gotcha below, it is **not** prefixed onto a Synapse's URL.

2. Pre-save a named Synapse per action (dashboard Synapses page, or `POST /api/synapses`).
   **Use the full absolute URL, not a relative path** — `apply_http_credential` only
   fills in `base_url` when the Synapse's `url` field is left *empty*; it does not join
   `base_url` + a relative path. A Synapse saved with `url: /agent/tool` will 404.
   - **"phone: send SMS"** → `method: POST`, `url: https://phone.yourdomain.com/agent/tool`,
     `body: {"tool":"sms.send","params":{}}` (placeholder — see below)
   - **"phone: check battery"** → same shape, `{"tool":"battery.get","params":{}}`
   - **"phone: run shell command"** → `{"tool":"shell.run","params":{}}` — but see the
     timeout gotcha below before wiring this one up as a saved Synapse.

   There's no `{{number}}`/`{{command}}` template substitution in this dispatch path —
   a saved Synapse's stored `body` is only ever used as-is or replaced wholesale.
   The agent supplies real values per call via `run_synapse`'s `body_override`:
   ```json
   { "tool": "run_synapse",
     "args": { "name_or_id": "phone: send SMS", "credential": "androidcompanion",
               "body_override": {"tool":"sms.send","params":{"number":"+639...","message":"..."}} } }
   ```

   **Timeout gotcha:** `run_synapse`'s DB-backed path hardcodes the request timeout to
   30s (`http_requests` has no timeout column, and `run_synapse`'s tool schema doesn't
   expose one) — but `shell.run`, `DELETE /call/log`, `DELETE /contacts/{id}`, and
   out-of-sandbox file writes/deletes block up to 90s waiting on the on-device
   Approve/Deny tap. A saved Synapse will time out client-side before that tap can land.
   For those, skip `run_synapse` and have the agent call the ad-hoc `synapse` tool
   directly instead, which does respect an explicit timeout:
   ```json
   { "tool": "synapse",
     "args": { "method": "POST", "url": "https://phone.yourdomain.com/agent/tool",
               "credential": "androidcompanion", "timeout_seconds": 120,
               "body": {"tool":"shell.run","params":{"command":"..."}} } }
   ```

3. Call `GET /agent/tools` (with the bearer token) any time you want the full current
   tool list/schema to build more Synapses — the dispatcher is `POST /agent/tool`
   with `{"tool": "sms.send", "params": {...}}`.

---

## Security model — read this before pointing an LLM at `/agent/tool`

- **Reachability:** the server binds `127.0.0.1` only. The Cloudflare Tunnel is the only
  path in from outside the phone — there's no direct LAN/Wi-Fi exposure to sniff or probe.
- **Auth:** every endpoint except `/ping` needs `Authorization: Bearer <token>`, compared in
  constant time. The token (and the cloudflared tunnel token) are stored AES-256/GCM
  encrypted under an AndroidKeyStore key, not in plaintext.
- **File sandbox:** `/sdcard/AndroidCompanion/` is the safe zone. Reads (`GET /files`,
  `GET /files/download`) work anywhere. Writes and deletes (`PUT /files`, `DELETE /files`,
  `POST /files/zip`'s destination, and the equivalent `files.write` / `files.delete` /
  `files.zip` tools) execute immediately *inside* that folder; anywhere else, they trigger
  an on-device approval prompt first.
- **On-device approval:** `shell.run` (both `POST /shell` and the `agent/tool` equivalent),
  `DELETE /call/log`, `DELETE /contacts/{id}`, and out-of-sandbox file writes/deletes all push
  a notification to the phone with Approve/Deny buttons and **block for up to 90 seconds**
  waiting for a tap. No tap (or Deny) → the call fails with `403` and
  `"requires_approval": true`. This exists because a voice agent can mishear a command or an
  LLM can hallucinate a tool call — anything irreversible needs a human physically at the
  phone to say yes.
- **Audit trail:** `GET /audit/log?lines=200` returns a local, append-only record (SMS sent,
  calls placed, shell commands run, files written/deleted, config changes, every approval
  decision) so you can reconstruct what the agent actually did.

---

## All Endpoints

### No auth required
| Method | Path    | Description  |
|--------|---------|--------------|
| GET    | /ping   | Health check |

### Auth required (`Authorization: Bearer TOKEN`)

| Method | Path                    | Body / Params                                   | Notes |
|--------|-------------------------|--------------------------------------------------|-------|
| GET    | /status                 | —                                                | server info, endpoint list |
| GET    | /config                 | —                                                | view config (secrets redacted) |
| POST/PATCH | /config             | `webhook_url`, `bearer_token`, `cloudflared_token`, `auto_answer`, `push_*`, `battery_threshold` | |
| POST   | /sms/send               | `{"number","message"}`                          | |
| GET    | /sms/inbox              | `?limit=&offset=`                               | |
| POST   | /call                   | `{"number"}`                                    | |
| GET    | /call/log               | `?limit=`                                       | |
| DELETE | /call/log               | —                                                | **on-device approval** |
| GET    | /contacts               | `?q=&limit=`                                    | |
| DELETE | /contacts/{id}          | —                                                | **on-device approval** |
| GET    | /battery, /device, /device/volume, /device/brightness, /device/storage, /device/apps, /device/clipboard, /device/ringermode, /device/audio, /wifi/info, /location | — | read-only |
| PATCH  | /device/volume, /device/brightness, /device/ringermode | | |
| POST   | /notification, /device/tts, /device/vibrate, /device/flashlight, /device/clipboard, /device/toast, /launch/app, /launch/url | | |
| POST/DELETE | /alarm, GET /alarm/status | | |
| POST   | /media/play, /media/stop | | |
| GET    | /camera/photo           | `?camera=back\|front`                           | |
| GET    | /files                  | `?path=`                                        | unrestricted read |
| GET    | /files/download         | `?path=`                                        | unrestricted read |
| PUT    | /files                  | `?path=`, body = raw bytes                      | **sandbox / approval** |
| DELETE | /files                  | `?path=`                                        | **sandbox / approval** |
| POST   | /files/zip              | `{"sources":[...],"dest"}`                      | **sandbox / approval on dest** |
| PUT    | /device/wallpaper       | body = jpeg bytes                                | |
| POST   | /shell                  | `{"command","termux?","workdir?"}`              | **on-device approval every call** |
| GET    | /agent/tools            | —                                                | tool schema for an LLM |
| POST   | /agent/tool             | `{"tool","params"}`                             | single dispatcher, mirrors the above |
| GET    | /audit/log              | `?lines=`                                       | local action history |

---

## Example axon-agent Synapses

**Reply to an SMS on command**
```
Synapse "phone: send SMS" → credential: androidcompanion → POST /agent/tool
  {"tool":"sms.send","params":{"number":"{{number}}","message":"{{message}}"}}
```

**Ask Axon "what's my phone's battery at?"**
```
Synapse "phone: check battery" → credential: androidcompanion → POST /agent/tool
  {"tool":"battery.get","params":{}}
```

---

## Troubleshooting

**Tunnel won't start / status stuck on "not running":**
→ Check `jniLibs/<your-device-ABI>/libcloudflared.so` exists (run `scripts/fetch_cloudflared.sh`,
  rebuild). Most phones are `arm64-v8a`.
→ Check the token was saved (Settings → Device control shows "Tunnel running" once it
  connects — can take a few seconds).

**Server stops when screen turns off:**
→ In Axon's Device control screen, tap "Disable Battery Optimization" and allow it.
→ On MIUI: Settings → Battery → Power saving → No restrictions → Axon
→ On OneUI: Settings → Battery → Background usage limits → Never sleeping apps → add Axon

**`shell.run` / file write outside the sandbox seems to hang:**
→ That's expected — it's waiting up to 90s for you to tap Approve/Deny on the phone's
  notification. No tap = automatic deny. Make sure any caller (a Synapse, curl, etc.)
  uses a request timeout of at least ~100s or it'll see a client timeout instead.

**Termux shows "Not found" (only relevant if you still use `shell.run` with `termux:true`):**
→ Install Termux from **F-Droid** (not Play Store — outdated there).

**Call not going through from axon-agent:**
→ `TelecomManager.placeCall()` requires the screen to be on OR battery optimization disabled.
→ Ensure CALL_PHONE permission is granted.

**Auto-answer not working:**
→ Must be enabled in: Settings → Accessibility → AndroidCompanion Auto Answer.
→ Run `adb shell dumpsys window windows | grep mCurrentFocus` during an incoming call if you
  need to add your dialer's package name to `accessibility_service_config.xml`.
