# 🖥️ Win Automation API

A self-contained Windows automation API server with an embedded Cloudflare Tunnel,
controlled from n8n (or any HTTP client) from anywhere in the world.

```
n8n (anywhere) ──HTTPS──► Cloudflare ──► cloudflared (embedded) ──► axum API ──► Windows
```

**Single `.exe`. Runs headless with no window. Survives sleep/wake cycles. Zero setup after first run.**

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

### 5. Run

Double-click `windowsapi.exe` or run it from the terminal. There is no window
and no tray icon — release builds are compiled as a GUI-subsystem binary and run
silently in the background. Confirm it is up with:

```bash
curl http://127.0.0.1:8080/ping
```

If it exits immediately, check `windowsapi_error.log` next to the `.exe` —
config and startup failures are written there because there is no console.

Run with `--no-install` to start it without installing itself or touching the
registry (see below).

---

## 🚀 Auto-Start on Boot

This is automatic. On a successful start — **after** the config validates — the
app copies itself to `%LOCALAPPDATA%\WindowsAPI\` and registers that copy under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, plus an entry in Installed
Apps so it can be removed from Settings.

- Pass `--no-install` to skip this entirely, e.g. when test-running a build.
- A run that fails config validation installs nothing.
- When installed via `installer.iss`, Inno Setup owns the Installed Apps entry
  and the app does not add a second one.

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

### 💻 Shell / PowerShell

```
POST /shell
```
```json
{
  "command": "Get-Date",
  "shell": "powershell",
  "timeout_secs": 30,
  "cwd": "C:\\Users\\you"
}
```
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
There is no console and no tray icon by design. Check `windowsapi_error.log`
next to the `.exe` — config errors, port conflicts, and repeated tunnel failures
are written there. For live logs, run a debug build (`cargo run`).

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
