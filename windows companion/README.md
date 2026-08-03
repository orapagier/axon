# 🖥️ Win Automation API

A self-contained Windows automation API server with an embedded Cloudflare Tunnel,
controlled from n8n (or any HTTP client) from anywhere in the world.

```
n8n (anywhere) ──HTTPS──► Cloudflare ──► cloudflared (embedded) ──► axum API ──► Windows
```

**Single `.exe`. System tray icon. Survives sleep/wake cycles. Zero setup after first run.**

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
```

Generate a strong secret: `openssl rand -hex 32`

### 5. Run

Double-click `win-automation-api.exe` or run it from the terminal.  
A blue circle tray icon appears — that means it's running.

---

## 🚀 Auto-Start on Boot

Open Task Scheduler and create a task:
- **Trigger:** At log on
- **Action:** Start a program → path to `win-automation-api.exe`
- **Settings:** Check "Run only when user is logged on"
- Under **Conditions**: uncheck "Stop if the computer switches to battery power"

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

```
GET /screenshot
GET /screenshot?screen=0
GET /screenshot?screen=1&format=jpeg
GET /screenshot?crop_x=0&crop_y=0&crop_w=800&crop_h=600
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
GET /system/env
POST /system/env → { "key": "MY_VAR", "value": "hello" }
```

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

---

### 📋 Clipboard

```
GET /clipboard        → { "text": "current clipboard content" }
POST /clipboard       → { "text": "new clipboard content" }
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

## 🔗 n8n Setup

1. Add an **HTTP Request** node
2. Set **URL**: `https://automation.yourdomain.com/shell`
3. Set **Method**: `POST`
4. Add header: `Authorization` = `Bearer <your_api_secret>`
5. Set **Body**: JSON with your command

For screenshots, decode the base64 `image` field using the **Move Binary Data** node.

---

## 🔒 Security Notes

- The API only listens on `127.0.0.1` — never exposed directly to the internet
- All external traffic goes through Cloudflare's encrypted tunnel
- The Bearer token is required for every request
- Consider enabling Cloudflare Access (Zero Trust) for an extra auth layer
- The `registry/write` and `system/power` endpoints are powerful — be careful with them

---

## 🐛 Troubleshooting

**Tray icon doesn't appear:**  
Run from terminal in debug mode (`cargo run`) to see logs.

**Tunnel not connecting:**  
Check your `tunnel_token` in `config.toml`. Make sure the tunnel is active in Cloudflare dashboard.

**Commands run but nothing happens:**  
Some actions (keyboard/mouse) require the target window to be focused first. Use `/windows/focus` before typing.

**Screenshot is black:**  
Some apps with DRM (like Netflix browser) block screen capture by design.

**Build fails — cloudflared.exe not found:**  
Make sure you've placed `cloudflared.exe` in the `bin/` folder before running `cargo build`.
