# 🖥️ Win Automation API

A self-contained Windows automation API server with an embedded Cloudflare Tunnel,
controlled from axon-agent, n8n, or any HTTP client, from anywhere in the world.

```
caller (anywhere) ──HTTPS──► Cloudflare ──► cloudflared ──► axum API ──► Windows
```

**Single `.exe`. Runs headless as a Windows service. Reachable while the machine is locked.**

---

## 🧭 Two planes

Windows draws a hard line through this API, and the design has to respect it.

A service runs in session 0 with no window station. It can execute anything as
LocalSystem, but `SendInput` goes nowhere, screen capture returns black, and the
clipboard it sees is not yours. Those routes only work from a process inside the
logged-on user's desktop — which in turn does not exist when nobody is logged on.

So the app is two processes:

```
                        ┌──────────────────────────────────────────┐
   HTTPS + Bearer ─────► │  Plane A — windowsapi.exe --service      │
                        │  Windows service, LocalSystem, session 0 │
                        │  owns cloudflared + the public listener  │
                        │                                          │
                        │  /shell /files/* /system/* /processes    │
                        │  /registry/* /agent /status              │
                        └───────────────┬──────────────────────────┘
                                        │ 127.0.0.1, per-boot token
                                        ▼
                        ┌──────────────────────────────────────────┐
                        │  Plane B — windowsapi.exe --session-agent │
                        │  spawned into WinSta0\Default as you      │
                        │                                          │
                        │  /screenshot /clipboard /keyboard/*       │
                        │  /mouse/* /windows/* /notify /open        │
                        └──────────────────────────────────────────┘
```

Callers see one API on one URL. Plane A forwards desktop routes to Plane B and
relaunches it automatically on logon, unlock and fast-user-switch.

### What works when

| State | `/shell` `/files/*` `/system/*` `/registry/*` | `/screenshot` `/clipboard` `/keyboard/*` `/mouse/*` `/windows/*` |
|---|---|---|
| Logged in, unlocked | ✅ | ✅ |
| **Locked screen** | ✅ | ❌ 503 `NO_DESKTOP_SESSION` |
| Logged out / login screen | ✅ | ❌ 503 `NO_DESKTOP_SESSION` |
| Lid closed on AC (after `prepare-laptop.ps1`) | ✅ | ✅ |
| Asleep | ❌ nothing is reachable | ❌ |

The locked-screen row is not a bug and cannot be engineered away from inside the
process. Windows switches the input desktop to Winlogon when you lock; DXGI
Desktop Duplication returns `DXGI_ERROR_ACCESS_LOST` and GDI `BitBlt` returns
black. To capture or drive your actual desktop, the session must be unlocked —
either don't let the machine lock, or unlock it over RDP first (see
`scripts/prepare-laptop.ps1 -EnableRdp`, then `tscon <id> /dest:console`).

Check live state any time with `GET /status`.

---

## 📋 Prerequisites

- Windows 10 or 11 (64-bit)
- Rust + Cargo (`https://rustup.rs`) — only needed to build
- A Cloudflare account with a domain
- A configured Cloudflare Tunnel (see setup below)

---

## 🔧 One-Time Setup

### 1. Create a Cloudflare Tunnel

1. Go to [Cloudflare Zero Trust](https://one.dash.cloudflare.com) → **Networks → Tunnels**
2. Click **Create a tunnel** → choose **Cloudflared**
3. Name it (e.g. `my-pc-automation`)
4. Under **Public Hostname**, add:
   - Subdomain: `automation` (or whatever you want)
   - Domain: your domain
   - Service: `http://localhost:8080`
5. On the **Install connector** step, copy the token from the command shown:
   ```
   cloudflared.exe service install <THIS_LONG_TOKEN_HERE>
   ```
   Just grab the token part, not the whole command.

### 2. Download cloudflared.exe

Download from: https://github.com/cloudflare/cloudflared/releases/latest  
Get `cloudflared-windows-amd64.exe`, rename it to `cloudflared.exe`,  
and drop it in the `bin/` folder of this project.

### 3. Build

```bash
cargo build --release
```

This embeds `cloudflared.exe` inside the output binary — the result is a single `.exe`.

To also produce the double-click installer (requires [Inno Setup 6](https://jrsoftware.org/isdl.php)):

```powershell
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer.iss
# -> Output\windowsapi-setup.exe
```

### 4. Configure

Copy `config.example.toml` to `config.toml` and put it next to the `.exe`:

```toml
tunnel_token = "eyJhIjoiMTk..."   # from Cloudflare dashboard
api_secret   = "some-long-random-secret-key"
port         = 8080
public_url   = "https://automation.yourdomain.com"

# Optional. "auto" (default) starts on QUIC and falls back to HTTP/2 if QUIC
# proves unstable; "http2" pins TCP from the start, which is the right answer
# behind consumer NAT that drops long-lived UDP.
# tunnel_protocol = "auto"

# Optional. Address family used to reach the Cloudflare edge: "auto" (default),
# "4", or "6". cloudflared's own default is IPv4-only; we default to "auto"
# because a machine whose IPv4 path to the edge is being dropped will often
# reach it perfectly well over IPv6.
# tunnel_edge_ip_version = "auto"
```

Generate a strong secret: `openssl rand -hex 32`

**`config.example.toml` must only ever contain placeholders** — it is committed
to git and is what the installer hands to every fresh install.

Startup refuses to continue if `tunnel_token` or `api_secret` is still a
placeholder, is empty, is under 32 characters, or is built from too few distinct
characters. That secret is the only thing between the public internet and full
control of the machine.

### 5. Prepare the machine (laptops especially)

```powershell
# elevated PowerShell
.\scripts\prepare-laptop.ps1
```

A sleeping laptop has no network, so the tunnel drops and every route fails no
matter how the API is built. This script stops the machine sleeping while it is
plugged in, makes lid-close a no-op on AC, and keeps the network up in connected
standby. Battery behaviour is left alone. `-Revert` undoes it.

It also stops Windows powering down the network adapter, and puts the wireless
radio in maximum-performance mode. **Do not skip this on a laptop that flaps.**
A Wi-Fi card Windows is allowed to park does not announce it — the link still
reads "connected" while outbound connections time out for tens of seconds at a
time. That is indistinguishable from a bad ISP from inside the app, and it is
the most common cause of a tunnel that drops on a laptop which browses the web
perfectly well.

Add `-EnableRdp` for a break-glass path in. Strongly recommended: when the
automation layer wedges, you need a way in that does not depend on the
automation layer. Reach it over Tailscale or `cloudflared access tcp`, never a
forwarded port.

### 6. Install

**Double-click install (recommended).** Put these two files in the same folder:

```
some-folder\
  windowsapi-setup.exe     ← from Output\
  config.toml              ← your filled-in config
```

Double-click `windowsapi-setup.exe`, approve the UAC prompt, and it does the
rest. The `config.toml` sitting beside the installer is read **at install time**
and travels with the binary into `C:\ProgramData\WindowsAPI\`. No wizard pages,
no directory to pick — the install location has to match the service
registration, so it is fixed.

If you leave out `config.toml`, setup still completes but the service cannot
start; the finished page offers to open the placeholder config in Notepad and
starts the service as soon as you save and close it.

Reinstalling over a configured machine warns before replacing a config that is
already filled in.

**Or install from the plain exe.** Same idea, no Inno needed — put
`windowsapi.exe` and `config.toml` in a folder and double-click the exe. It
raises its own UAC prompt and installs itself.

**Or from a terminal:**

```powershell
.\windowsapi.exe --install
```

Any of these copies the binary and config to `C:\ProgramData\WindowsAPI\`,
restricts that directory to SYSTEM and Administrators, registers a LocalSystem
service set to start at boot, and starts it. Confirm:

```bash
curl http://127.0.0.1:8080/ping
curl -H "Authorization: Bearer <secret>" http://127.0.0.1:8080/status
```

`/status` reports both planes, so it is the fastest way to tell "no desktop
session" apart from "something is broken".

| Command | Effect |
|---|---|
| `windowsapi` *(no args)* | Same as `--install` — this is what double-clicking does |
| `windowsapi --install` | Install to ProgramData, register the service, start it |
| `windowsapi --uninstall` | Stop and remove the service (leaves ProgramData) |
| `windowsapi --start` / `--stop` | Service control |
| `windowsapi --user-mode` | Run in the current console as you — no service, no lock-screen access |
| `--quiet` | Log results instead of showing a dialog |

All except `--user-mode` need elevation, and will raise their own UAC prompt if
you are not already elevated. The binary is GUI-subsystem, so it attaches to
your terminal to print results and falls back to a message box when
double-clicked.

> `--quiet` is **required** when calling this exe from a script or installer.
> Without a console to attach to, the message-box fallback is both invisible
> under `runhidden` and modal, so the caller waits on it forever. `installer.iss`
> passes it on every call for exactly this reason.

If it exits immediately, check `windowsapi_error.log` in the install directory —
config and startup failures are written there because there is no console.

---

## 🚀 Auto-Start on Boot

Handled by the service, registered `AutoStart` by `--install`. It starts at boot
before anyone logs in, which is the whole point: the tunnel and the shell/file
routes are up while the machine sits at the login screen.

The desktop agent is launched and relaunched by the service as sessions come and
go — nothing to configure there.

`--install` also adds the app to **Startup Apps**, visible in Settings > Apps >
Startup and Task Manager > Startup apps:

```
HKLM\...\CurrentVersion\Run
  Windows Automation API = "C:\ProgramData\WindowsAPI\windowsapi.exe" --ensure-running --quiet
```

This is not what starts the machine connected — the service does that, earlier,
with nobody logged in. It is there for two other reasons. A background service
that owns the machine's remote access should be somewhere its owner can *see* it,
and a service registration appears in neither of those lists. And it is a
per-logon backstop: if the service is stopped for any reason — a failed start at
boot, a manual stop nobody undid, an exhausted SCM recovery ladder — this brings
it back at the next logon.

`--ensure-running` no-ops when the service is already up, and needs no admin
rights: `--install` grants interactive users `SERVICE_START` on the service (and
only start — not stop, not reconfigure). `--uninstall` removes the entry.

> **Upgrading from the old build?** Earlier versions installed to
> `%LOCALAPPDATA%\WindowsAPI` and auto-started via
> `HKCU\...\CurrentVersion\Run`. That entry is not removed automatically —
> delete it, or you will run two copies and the second will fail to bind the
> port:
> ```powershell
> reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v WindowsAPI /f
> reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\WindowsAPI" /f
> ```

---

## 📡 API Reference

All requests require the header:
```
Authorization: Bearer <your_api_secret>
```

Base URL: `https://automation.yourdomain.com`

---

### 🏓 Health Check
```
GET /ping
```
No auth required.
```json
{ "status": "ok", "version": "1.0.0" }
```

---

### 🧭 Status

```
GET /status      (also POST)
```
Reports which plane is answering. Check this first when a desktop route fails —
a 503 at 3am is usually "the laptop is at the login screen", not a bug.

```json
{
  "version": "1.0.0",
  "service_plane": {
    "ready": true,
    "routes": ["/shell", "/files/*", "/system/*", "/processes", "/registry/*"],
    "available_at_lockscreen": true
  },
  "desktop_plane": {
    "ready": false,
    "console_session": null,
    "routes": ["/screenshot", "/clipboard", "/keyboard/*", "/mouse/*", "/windows/*", "/notify", "/open"],
    "available_at_lockscreen": false,
    "detail": "No desktop agent. Nobody is logged on, or the agent is restarting."
  }
}
```

---

### 💻 Shell / PowerShell

```
POST /shell
```
```json
{
  "command": "Get-Date",
  "shell": "powershell",
  "timeout_secs": 30,
  "cwd": "C:\\Users\\you",
  "run_as": "system"
}
```

**`run_as` picks which plane runs the command:**

| Value | Runs as | Available at lock screen | Notes |
|---|---|---|---|
| `"system"` *(default)* | LocalSystem | ✅ | Full machine privilege |
| `"user"` | The logged-on user | ❌ 503 | Your real profile and environment |

LocalSystem is not simply "Administrator++". It cannot see `HKCU`, your mapped
drives, or anything DPAPI-protected for your account, and on the network it
authenticates as the machine account rather than as you. If a command works in
your own terminal but returns empty or "not found" over the API, that mismatch
is almost always why — send `"run_as": "user"`.

The desktop agent is launched with the elevated half of your UAC split token, so
`run_as: "user"` is still elevated, and synthetic input can reach elevated
windows (UIPI would otherwise drop it silently).
Response:
```json
{
  "stdout": "Tuesday, February 24, 2026 ...",
  "stderr": "",
  "exit_code": 0,
  "success": true
}
```
`shell` can be `"powershell"` (default) or `"cmd"`.

**Examples:**
```json
{ "command": "Get-Process | Sort CPU -Desc | Select -First 5 | ConvertTo-Json" }
{ "command": "dir C:\\Users\\you\\Downloads", "shell": "cmd" }
{ "command": "New-Item -Path C:\\temp\\test.txt -ItemType File" }
```

---

### ⌨️ Keyboard

**Type text:**
```
POST /keyboard/type
```
```json
{
  "text": "Hello, World!",
  "delay_ms": 50
}
```

**Press key / hotkey:**
```
POST /keyboard/key
```
```json
{ "keys": ["ctrl", "c"] }
{ "keys": ["win", "d"] }
{ "keys": ["alt", "f4"] }
{ "keys": ["f5"] }
{ "keys": ["ctrl", "shift", "esc"] }
```

Supported keys: `ctrl`, `alt`, `shift`, `win`, `enter`, `tab`, `esc`, `space`, `backspace`,
`delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `up`, `down`, `left`, `right`,
`f1`–`f12`, `printscreen`, `capslock`, `numlock`, `scrolllock`, and any single character.

---

### 🖱️ Mouse

**Move:**
```
POST /mouse/move
```
```json
{ "x": 960, "y": 540, "mode": "abs" }
{ "x": 10, "y": -20, "mode": "rel" }
```

**Click:**
```
POST /mouse/click
```
```json
{ "x": 100, "y": 200, "button": "left" }
{ "x": 100, "y": 200, "button": "right" }
{ "x": 100, "y": 200, "double": true }
{ "button": "left", "delay_ms": 200 }
```
Omit `x`/`y` to click at the current mouse position.

**Scroll:**
```
POST /mouse/scroll
```
```json
{ "y": 3 }
{ "y": -3, "mouse_x": 500, "mouse_y": 400 }
```
Positive `y` = scroll down, negative = scroll up.

**Drag:**
```
POST /mouse/drag
```
```json
{ "from_x": 100, "from_y": 100, "to_x": 500, "to_y": 300, "button": "left" }
```

---

### 📸 Screenshot

Options work as query parameters **or** as a JSON body — the body form is what
`/agent` uses, since the proxy only forwards JSON.

```
GET  /screenshot
GET  /screenshot?screen=1&format=jpeg
GET  /screenshot?crop_x=0&crop_y=0&crop_w=800&crop_h=600
POST /screenshot   { "screen": 1, "format": "jpeg" }
```
Response:
```json
{
  "image": "<base64 PNG or JPEG>",
  "format": "png",
  "width": 1920,
  "height": 1080,
  "screen_index": 0,
  "screen_count": 2
}
```

**Save straight to a URL instead of base64:**
```
GET  /screenshot/save
POST /screenshot/save   { "screen": 0, "format": "png" }
```
```json
{
  "filename": "screenshot_1740000000_a1b2c3d4e5f60718.png",
  "path": "C:\\...\\public\\screenshot_1740000000_a1b2c3d4e5f60718.png",
  "url": "https://automation.yourdomain.com/public/screenshot_1740000000_a1b2c3d4e5f60718.png"
}
```
Files under `/public` are served without auth and deleted after 30 minutes.
Their names carry a random suffix so they cannot be enumerated.

---

### 📂 Files

**Open file/app/URL:**
```
POST /open
```
```json
{ "target": "C:\\Users\\you\\doc.pdf" }
{ "target": "https://google.com" }
{ "target": "notepad" }
```

**Read file:**
```
POST /files/read
```
```json
{ "path": "C:\\Users\\you\\notes.txt" }
```

**Write file:**
```
POST /files/write
```
```json
{ "path": "C:\\temp\\out.txt", "content": "hello", "append": false }
```

**List directory:**
```
POST /files/list
```
```json
{ "path": "C:\\Users\\you\\Downloads" }
```

**Delete:**
```
POST /files/delete
```
```json
{ "path": "C:\\temp\\old.txt" }
```

**Check existence:**
```
POST /files/exists
```
```json
{ "path": "C:\\temp\\file.txt" }
```
Response: `{ "exists": true, "is_dir": false, "is_file": true }`

**Search:**
```
POST /files/search
```
```json
{ "path": "C:\\Users\\you", "pattern": "*.pdf", "recursive": true, "limit": 200 }
```
`pattern` supports `*` wildcards and is case-insensitive. Defaults: `*`,
recursive, limit 200.

**Get a download URL for any local file:**
```
POST /files/link
```
```json
{ "path": "C:\\Users\\you\\report.pdf" }
```
Copies the file into `public/` and returns `{ url, filename, size_bytes }`.
Valid for 30 minutes.

**Upload a file to this machine:**
```
POST /files/upload
Content-Type: multipart/form-data
```
Fields, in this order:
- `path` — destination on the Windows machine (**must come before `file`**)
- `file` — the binary

Supports the shortcuts `Desktop/`, `Documents/`, `Downloads/`, `OneDrive/`, `~/`.
Streamed to disk, so there is no size limit — every other endpoint caps request
bodies at 10 MB.

---

### ⚡ System

**Info:**
```
GET /system/info
```
Returns OS, CPU, RAM, hostname, uptime, etc.

**Power:**
```
POST /system/power
```
```json
{ "action": "lock" }
{ "action": "sleep" }
{ "action": "shutdown", "delay_secs": 60 }
{ "action": "restart" }
{ "action": "hibernate" }
{ "action": "logoff" }
{ "action": "cancel_shutdown" }
```

**Environment variables:**
```
GET  /system/env       → list all
POST /system/env       → list all (empty body)
POST /system/env/set   → { "key": "MY_VAR", "value": "hello" }
```
Writes are process-scoped only — they do not persist or affect other processes.

---

### ⚙️ Processes

**List (sorted by CPU):**
```
GET /processes
```

**Kill:**
```
POST /processes/kill
```
```json
{ "pid": 1234 }
{ "name": "notepad.exe" }
```
`name` is an exact, case-insensitive match — killing `"chrome.exe"` will not
also take down `chromedriver.exe`.

---

### 📋 Clipboard

```
GET  /clipboard       → { "text": "current clipboard content" }
POST /clipboard       → read (empty body)
POST /clipboard/set   → { "text": "new clipboard content" }
```

---

### 🪟 Windows

**List open windows:**
```
GET /windows
```
Returns: `[{ "hwnd": 12345, "title": "Notepad", "class": "Notepad" }, ...]`

**Actions (match by title or hwnd):**
```
POST /windows/focus    → { "title": "Notepad" }
POST /windows/close    → { "title": "Notepad" }
POST /windows/minimize → { "hwnd": 12345 }
POST /windows/maximize → { "title": "Chrome" }
POST /windows/resize   → { "title": "Notepad", "x": 0, "y": 0, "width": 800, "height": 600 }
```

---

### 🔔 Notifications

```
POST /notify
```
```json
{
  "title": "n8n Alert",
  "body": "Workflow completed successfully",
  "app_id": "My Automation"
}
```
Shows a native Windows toast notification.

---

### 🗂️ Registry

**Read:**
```
POST /registry/read
```
```json
{
  "key": "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
  "name": "ProductName"
}
```

**Write:**
```
POST /registry/write
```
```json
{
  "key": "HKEY_CURRENT_USER\\SOFTWARE\\MyApp",
  "name": "Setting",
  "value": "hello",
  "value_type": "string"
}
```
Types: `string`, `dword`, `qword`, `expand_string`, `binary`, `multi_string`

---

### 🧰 Tool RPC (axon-agent)

The protocol axon-agent's `DeviceTool` speaks. Register this machine on the
Devices page with **Kind: Windows Companion**, **Base URL** = your `public_url`,
**Bearer Token** = your `api_secret`, and the agent can drive it with the same
code path it uses for AndroidCompanion.

```
GET  /agent/tools    → the catalogue the LLM reads
POST /agent/tool     → { "tool": "shell.run", "params": { "command": "Get-Date" } }
```

`/agent/tools` returns 31 tools, each tagged with the plane it runs on:

```json
{
  "name": "shell.run",
  "description": "Run a PowerShell or cmd command as LocalSystem...",
  "params": ["command", "shell? (powershell|cmd)", "timeout_secs?", "cwd?"],
  "plane": "service",
  "availability": "Always available, including while the machine is locked."
}
```

That `plane` field is the point. A model reading this catalogue knows *before
calling* that `shell.run` survives a lock screen and `screen.capture` does not,
rather than discovering it through a 503 at 3am.

| Namespace | Tools |
|---|---|
| `shell.*` | `run` (LocalSystem), `run_as_user` (logged-in user) |
| `files.*` | `read` `write` `list` `search` `delete` `exists` `link` |
| `system.*` | `info` `status` `power` |
| `process.*` | `list` `kill` |
| `registry.*` | `read` `write` |
| `screen.*` | `capture` |
| `clipboard.*` | `get` `set` |
| `input.*` | `type` `key` |
| `mouse.*` | `click` `move` `scroll` `drag` |
| `window.*` | `list` `focus` `close` `resize` |
| `notify.*` / `launch.*` | `push` / `open` |

Notes:

- `shell.run` and `shell.run_as_user` are the same endpoint; the tool name sets
  `run_as`, and a caller-supplied `run_as` is ignored. Otherwise `shell.run`
  could silently become a desktop call and 503 on a locked machine.
- `params` may be a JSON object **or** a string containing one — axon-agent's
  synapse tool serialises it before sending.
- Binary results come back as download URLs, so `screen.capture` returns
  `image: { url, note }`, never base64.
- Unknown tool names get a "did you mean" suggestion.
- Tools are an explicit allow-list. Adding a REST route does not expose it here.

---

### 🤖 Agent Proxy

Lower-level alternative to `/agent/tool` — dispatches to any route by path
rather than by tool name. Both share the same dispatcher.

```
POST /agent
```
```json
{ "method": "POST", "path": "/screenshot", "body": { "screen": 0 } }
```
Dispatches internally to any other endpoint and returns its JSON. Built for
callers that can only issue one request shape.

- Every read-only route accepts `POST` with an empty body as well as `GET`, so
  `method: "POST"` always works.
- Base64 blobs in the response are written to `public/` and replaced with
  `{ url, note }`. This applies only to binary fields (`image`, `photo`,
  `screenshot`, …) and never to endpoints that return text you asked for
  (`/files/read`, `/clipboard`, `/shell`, …), so file contents are never
  swapped out for a link.
- Proxying to `/agent` or `/public/*` is rejected, including when disguised with
  a query string or extra slashes.

---

## 🔗 n8n Setup

1. Add an **HTTP Request** node
2. Set **URL**: `https://automation.yourdomain.com/shell`
3. Set **Method**: `POST`
4. Add header: `Authorization` = `Bearer <your_api_secret>`
5. Set **Body**: JSON with your command

For screenshots, decode the base64 `image` field using the **Move Binary Data** node.

---

## 🔒 Security Notes

This API grants complete control of the machine — arbitrary shell execution,
registry writes, file read/write, keyboard and mouse input. Treat `api_secret`
as a root password.

> ⚠️ **Running as a service raises the stakes.** Before the split, a leaked
> `api_secret` meant remote code execution as you. Now `/shell` defaults to
> LocalSystem, so it means **SYSTEM-level RCE reachable from the public
> internet**, available even when nobody is logged in. The two mitigations below
> are worth treating as part of the setup, not as follow-ups.

**1. Put Cloudflare Access in front of the tunnel.** SSO or mTLS enforced at
Cloudflare's edge, before a request ever reaches your machine. Free at this
scale, and it means a leaked bearer token alone is not game over. Zero Trust →
Access → Applications → Self-hosted, pointed at your tunnel hostname.

**2. Restrict what the token can reach.** There is currently one secret for the
whole API. If you only need screenshots from a given workflow, that workflow
still holds a credential that can call `/shell` as SYSTEM. Per-capability tokens
are the obvious next hardening step and are not implemented yet.

**Install directory.** `--install` drops inherited ACEs on
`C:\ProgramData\WindowsAPI` and grants only SYSTEM and Administrators. This
matters: the service executes `cloudflared.exe` from that directory as SYSTEM,
so a path any standard user could write to would be a straightforward privilege
escalation. Do not loosen it, and do not move the install somewhere world-writable.

**Loopback hop.** Plane A authenticates to Plane B with a token generated per
service start from `BCryptGenRandom` and handed over via an inherited stdin pipe,
so it never appears in the process list. The desktop agent deliberately never
reads `config.toml` — `api_secret` stays in the service. The loopback port is
bound to `127.0.0.1` and is never routed through the tunnel. A local process
could still reach that port if it guessed the token; a named pipe with a DACL
would close that gap and is the natural next step.

- The API only listens on `127.0.0.1` — never exposed directly to the internet
- All external traffic goes through Cloudflare's encrypted tunnel
- The Bearer token is required for every request, compared in constant time
- `api_secret` must be ≥32 high-entropy characters; startup refuses weak values
- **Never commit `config.toml`**, and never put real credentials in
  `config.example.toml` — that file is committed and ships in the installer
- If a secret has ever been committed, rotate it. Deleting it from the working
  tree does not remove it from git history
- Consider enabling Cloudflare Access (Zero Trust) for an extra auth layer
- No CORS headers are sent, so a web page the user visits cannot read from this
  port even though it is bound to localhost
- Two routes need no auth: `/ping`, and `/public/:filename` for the temporary
  download files (random names, deleted after 30 minutes)
- The `registry/write`, `system/power`, and `shell` endpoints are powerful — be
  careful with them

---

## 🐛 Troubleshooting

**It exits immediately / nothing happens:**  
There is no console and no tray icon by design. Check
`C:\ProgramData\WindowsAPI\windowsapi_error.log` — config errors, port
conflicts, and repeated tunnel failures are written there. For live logs, run a
debug build (`cargo run -- --user-mode`).

**Service won't start / `sc query WindowsAPI` shows exit code 1610:**  
1610 is `ERROR_BAD_CONFIGURATION` — the service loaded `config.toml` and refused
it. The error log names the exact field. Almost always a placeholder
`tunnel_token` or an `api_secret` under 32 characters.

**Everything works except screenshot / clipboard / keyboard / mouse:**  
Call `GET /status`. If `desktop_plane.ready` is `false`, no interactive session
exists — the machine is locked, at the login screen, or nobody is logged on.
This is expected, not a fault; see the "What works when" table above.

**Desktop routes return 503 even though I'm logged in:**  
The agent may have failed to launch. Check the error log for
`WTSQueryUserToken failed` (the service is not running as LocalSystem) or
`Desktop agent failed to bind` (something else holds `session_port` — set a free
one in `config.toml`).

**Typing/clicking does nothing in an elevated window:**  
UIPI blocks input from a lower-integrity process. The agent is normally launched
with the elevated half of your UAC split token, which avoids this. If your
account is a standard user, there is no elevated token to use and this is a
Windows-level restriction.

**Commands can't find my files / registry keys / mapped drives:**  
`/shell` defaults to `run_as: "system"`, and LocalSystem has no `HKCU`, no user
profile, and no mapped drives. Send `"run_as": "user"`.

**Two copies running / port bind failures after upgrading:**  
The old `HKCU\...\Run` autostart entry is still there. See the upgrade note in
Auto-Start on Boot.

**"Please set a secure api_secret" on startup:**  
`config.toml` still has the placeholder, or the secret is under 32 characters or
too repetitive. Generate one with `openssl rand -hex 32`.

**"Failed to bind 127.0.0.1:8080":**  
Another copy is already running (check Task Manager for `windowsapi.exe`), or
something else holds the port. Change `port` in `config.toml`.

**Tunnel not connecting:**  
Check `tunnel_token` in `config.toml` and that the tunnel is active in the
Cloudflare dashboard. After three quick failures in a row the app logs the
likely cause and backs off, up to 5 minutes between attempts.

**Tunnel keeps flapping / the host 502s intermittently:**

Start with `GET /status` on the loopback port (`127.0.0.1:8080`, not through the
tunnel — through the tunnel it can only ever answer when the tunnel is up). The
`tunnel` block is built for exactly this question:

| Field | What it means when it stands out |
|---|---|
| `restarts` | Climbing steadily means something is genuinely failing. Should be 0 for a healthy machine. |
| `waiting_for_uplink` | `true` means the machine cannot reach the internet at all. **This is your network, not the app** — and nothing is being restarted, deliberately. |
| `network_reachable` | `false` at the last outage says the same thing. |
| `secs_without_edge` | How long the tunnel has been down right now. |
| `restart_budget_spent` | Restarts have hit their hourly cap and stopped helping. Look at link quality. |
| `ready_connections` | 1-4 is fine. The tunnel works on one. |
| `fell_back_to_http2` | QUIC was unusable here; the transport already moved. |

Ordinary things to try, in order:

1. **Run `scripts\prepare-laptop.ps1` elevated.** On a laptop this is the fix
   more often than anything in the app. A parked Wi-Fi adapter looks exactly
   like a bad ISP.
2. **Pin `tunnel_protocol = "http2"`** if the log shows QUIC reconnect churn.
3. Leave `tunnel_edge_ip_version` at `auto` so a broken IPv4 path can fail over
   to IPv6.

What the app will *not* do is restart cloudflared to make you feel better about
it. A restart costs about 45 seconds of hard downtime — DNS, feature fetch and
protocol negotiation all happen before the first edge connection — while
cloudflared's own retry reconnects a dropped connection in seconds without
disturbing the others. So the supervisor waits five minutes of *continuous* zero
connections, with the Cloudflare edge confirmed TCP-reachable throughout, before
it restarts anything, and never restarts at all while the uplink is down. If you
see edge drops in `cloudflared.log` while `restarts` stays at 0 and
`ready_connections` recovers, that is the design working, not a stuck
supervisor.

**The tunnel keeps dropping / the host 502s at random:**  
Start with `cloudflared.log` in the install directory and `GET /status`, then
work down this list — the order matters, because the top two look identical
from outside.

1. **Is it the internet?** Ping your router, then ping anything past it. A
   gateway that answers in single-digit milliseconds while `1.1.1.1` and DNS
   time out is a dead uplink, and no tunnel can survive one. The supervisor
   detects this itself: on every restart it TCP-connects to the Cloudflare
   edge, and if that fails too it records `"network_reachable": false` on
   `/status`, says so in the log, and does *not* blame the transport. After
   three such restarts it writes a plain-language note to
   `windowsapi_error.log`. Nothing in this app can work around a flapping ISP
   link — a wired connection or a second WAN is the only real fix.
2. **Is it QUIC?** cloudflared defaults to QUIC over UDP/7844, and plenty of
   consumer routers and ISPs quietly drop long-lived UDP flows. The signature
   is `failed to dial to edge with quic: timeout: no recent network activity`
   in `cloudflared.log` *while* plain TCP to the edge still works. Left alone,
   the supervisor falls back to HTTP/2 over TCP after two such failures and
   logs that it did. Set `tunnel_protocol = "http2"` in `config.toml` to skip
   the discovery and pin it from the start.
3. **Is it the token?** Three failures inside 60 s of starting each, with the
   edge reachable, means a revoked or mistyped `tunnel_token`. The error log
   says so explicitly.

`GET /status` carries a `tunnel` block for exactly this — `healthy` is the
answer to "can the outside world reach this machine", which is *not* the same
question as "is cloudflared running":

```json
"tunnel": {
  "running": true, "healthy": true, "protocol": "quic",
  "ready_connections": 4, "restarts": 0,
  "quic_closed_connections": 0, "fell_back_to_http2": false,
  "metrics_reachable": true, "metrics_port": 20241,
  "network_reachable": null, "last_event": "started on quic"
}
```

Read it over `127.0.0.1:8080`, not through the tunnel — over the tunnel, a
reply proves the tunnel is up and the interesting cases cannot occur.

**The service died and never came back:**  
It should now: `--install` registers recovery actions (restart after 5 s, 15 s,
then 60 s, counter reset daily) including for non-crash exits, which is what a
config or port-bind failure produces. Confirm with
`sc qfailure WindowsAPI`. Installs made before this existed have no recovery
policy at all — re-run `--install` to add it.

**Commands run but nothing happens:**  
Some actions (keyboard/mouse) require the target window to be focused first. Use
`/windows/focus` before typing.

**Screenshot is black:**  
Some apps with DRM (like Netflix in a browser) block screen capture by design.

**Uploads over a few MB fail:**  
Send them to `/files/upload` as multipart, not `/files/write`. Every other
endpoint caps request bodies at 10 MB.

**Build fails — cloudflared.exe not found:**  
Place `cloudflared.exe` in the `bin/` folder before running `cargo build`. It is
gitignored because of its size, so a fresh clone will not have it.

**Build fails — "current package believes it's in a workspace":**  
This crate is standalone and carries an empty `[workspace]` table in its
`Cargo.toml` to stay out of the Axon workspace at the repo root. Don't remove it.
