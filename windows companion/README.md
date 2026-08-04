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

### 4. Configure

Copy `config.example.toml` to `config.toml` and put it next to the `.exe`:

```toml
tunnel_token = "eyJhIjoiMTk..."   # from Cloudflare dashboard
api_secret   = "some-long-random-secret-key"
port         = 8080
public_url   = "https://automation.yourdomain.com"
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

Add `-EnableRdp` for a break-glass path in. Strongly recommended: when the
automation layer wedges, you need a way in that does not depend on the
automation layer. Reach it over Tailscale or `cloudflared access tcp`, never a
forwarded port.

### 6. Install the service

```powershell
# elevated PowerShell
.\windowsapi.exe --install
```

This copies the binary and config to `C:\ProgramData\WindowsAPI\`, restricts
that directory to SYSTEM and Administrators, registers a LocalSystem service set
to start at boot, and starts it. Confirm:

```bash
curl http://127.0.0.1:8080/ping
curl -H "Authorization: Bearer <secret>" http://127.0.0.1:8080/status
```

`/status` reports both planes, so it is the fastest way to tell "no desktop
session" apart from "something is broken".

| Command | Effect |
|---|---|
| `windowsapi --install` | Install to ProgramData, register the service, start it |
| `windowsapi --uninstall` | Stop and remove the service (leaves ProgramData) |
| `windowsapi --start` / `--stop` | Service control |
| `windowsapi --user-mode` | Run in the current console as you — no service, no lock-screen access |

All except `--user-mode` need an elevated prompt. The binary is GUI-subsystem,
so it attaches to your terminal to print results and falls back to a message box
when double-clicked.

If it exits immediately, check `windowsapi_error.log` in the install directory —
config and startup failures are written there because there is no console.

---

## 🚀 Auto-Start on Boot

Handled by the service, registered `AutoStart` by `--install`. It starts at boot
before anyone logs in, which is the whole point: the tunnel and the shell/file
routes are up while the machine sits at the login screen.

The desktop agent is launched and relaunched by the service as sessions come and
go — nothing to configure, and no `Run` key involved.

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

### 🤖 Agent Proxy

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
