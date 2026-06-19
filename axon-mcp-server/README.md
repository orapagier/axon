# axon-mcp (Rust)

A zero-dependency, single-binary MCP server for **Google Workspace**, **Microsoft 365**, **Facebook Pages**, and **local business tools** — written in Rust for resource-constrained servers.

```
RAM at idle:  ~2–5 MB
Binary size:  ~6–12 MB (stripped release)
Runtime:      none — one binary, scp and run
```

---

## Workspace Structure

```
axon-mcp-rust/
├── src/
│   └── main.rs                  # MCP server binary + OAuth callback HTTP server
├── crates/
│   ├── axon-core/               # Shared: AppState, Storage, OAuth helpers, macros
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── storage.rs       # Credentials + token persistence (~/.config/axon-mcp)
│   │       └── oauth.rs         # Token exchange + refresh helpers
│   ├── axon-google/             # Google Workspace tools
│   │   └── src/
│   │       ├── lib.rs           # Tool catalogue + dispatcher
│   │       ├── auth.rs          # OAuth2 flow + token refresh
│   │       ├── gmail.rs         # Gmail: list, get, send, reply, labels, search
│   │       ├── calendar.rs      # Calendar: CRUD, quick-add, free/busy
│   │       └── drive.rs         # Drive: list, search, upload, download, share
│   ├── axon-microsoft/          # Microsoft 365 tools
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── auth.rs
│   │       ├── outlook.rs       # Mail: list, get, send, reply, forward, folders
│   │       ├── calendar.rs      # Calendar: CRUD, accept/decline
│   │       ├── onedrive.rs      # Files: list, search, upload, download, delete
│   │       └── teams.rs         # Teams: channels, messages, chats
│   ├── axon-facebook/           # Facebook Page tools
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── auth.rs          # OAuth + long-lived page token exchange
│   │       ├── page.rs          # Page info and update
│   │       ├── posts.rs         # Posts: CRUD, image, schedule
│   │       ├── comments.rs      # Comments: list, reply, hide, like
│   │       ├── insights.rs      # Analytics: page, post, fans, reach
│   │       └── messaging.rs     # Messenger: conversations, send text/image
│   └── axon-business/           # Offline business utilities (no API keys needed)
│       └── src/
│           ├── lib.rs
│           ├── notes.rs         # Local Markdown notes (CRUD, search, export)
│           ├── tasks.rs         # Local task list with priority, due date, overdue
│           ├── contacts.rs      # Local address book (CRUD, search, tags)
│           ├── datetime.rs      # Timezone convert, diff, add, format
│           └── text.rs          # Word count, email/URL extract, slugify, template
├── credentials.example.json
├── Makefile
└── Cargo.toml
```

---

## CRM Module

`axon-crm` is the local CRM layer for organizations, leads, deals, and activity history.

- Storage: SQLite in the Axon data directory (`crm.db`)
- Core records: organizations, leads, deals, activities
- Workflow tools: lead-to-deal conversion, cross-CRM search, record overviews, dashboard summary
- Migration support: imports legacy `crm_orgs.json` and `crm_activities.json` on first run

---

## Quick Start

```bash
# 1. Build
make release

# 2. Set up credentials
make creds-init          # creates ~/.config/axon-mcp/credentials.json
make creds-edit          # fill in your API keys

# 3. Run
./target/release/axon-mcp

# 4. Or install system-wide
make install             # copies to /usr/local/bin/axon-mcp
```

---

## Credentials Setup

### Google Workspace
1. [Google Cloud Console](https://console.cloud.google.com/) → New Project
2. Enable APIs: Gmail, Calendar, Drive, Docs, Sheets, People, YouTube Data API v3, Places API
3. OAuth 2.0 credentials → Web app → Redirect URI: `http://localhost:8080/auth/google/callback`
4. Copy `client_id` and `client_secret`
5. Optional (recommended for Places API): add `places_api_key` under the `google` credentials object

### Microsoft 365
1. [Azure Portal](https://portal.azure.com/) → App registrations → New
2. Redirect URI (Web): `http://localhost:8080/auth/microsoft/callback`
3. API Permissions (Delegated): `offline_access`, `Mail.ReadWrite`, `Mail.Send`,
   `Calendars.ReadWrite`, `Files.ReadWrite`, `Team.ReadBasic.All`,
   `ChannelMessage.Send`, `Chat.ReadWrite`, `User.Read`
4. Certificates & secrets → New secret
5. Copy `Application ID`, secret, `Directory (tenant) ID`

### Facebook Page & Instagram
1. [Meta for Developers](https://developers.facebook.com/) → New App → Business
2. Facebook Login → Redirect URI: `http://localhost:8080/auth/facebook/callback`
3. Permissions: `pages_manage_posts`, `pages_read_engagement`, `pages_messaging`, `pages_manage_metadata`, `read_insights`, `pages_show_list`, `instagram_basic`, `instagram_content_publish`, `instagram_manage_comments`, `instagram_manage_insights`, `instagram_manage_messages`
4. Copy `App ID`, `App Secret`, and your **Page ID** (Page → Settings → About)

---

## Authentication

Each service needs a one-time browser auth:

```
1. Your agent calls:   google_auth_url
2. Open the returned URL in a browser
3. Sign in and approve
4. Token auto-saved via the built-in callback server on :8080
   — OR copy the 'code' param from the redirect URL and call:
      google_exchange_code { "code": "..." }
```

Tokens are stored at `~/.local/share/axon-mcp/tokens.json` (mode 0600).

---

## Connecting to Your Rust Agent

```json
{
  "mcpServers": {
    "axon-mcp": {
      "command": "/usr/local/bin/axon-mcp"
    }
  }
}
```

Or via stdio from your Rust agent:
```rust
let mut child = std::process::Command::new("/usr/local/bin/axon-mcp")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .spawn()?;
```

---

## Cross-Compilation (deploy to a different arch)

```bash
# Install cross
cargo install cross

# Static Linux x86_64 (works on any Linux distro, no glibc dependency)
make cross-linux-musl

# ARM64 (Raspberry Pi 4, Oracle Cloud ARM, Ampere)
make cross-arm64
```

---

## Adding More Tools

Each service crate is self-contained. To add a tool:

**1. Add an `async fn` in the relevant module:**
```rust
// crates/axon-google/src/drive.rs
pub async fn create_folder(state: &AppState, name: &str, parent_id: Option<&str>) -> Result<Value> {
    let tok  = access_token(state).await?;
    let mut meta = json!({ "name": name, "mimeType": "application/vnd.google-apps.folder" });
    if let Some(p) = parent_id { meta["parents"] = json!([p]); }
    let resp: Value = state.client.post("https://www.googleapis.com/drive/v3/files")
        .bearer_auth(&tok).json(&meta).send().await?.error_for_status()?.json().await?;
    Ok(resp)
}
```

**2. Add it to `tool_list()` in `lib.rs`:**
```rust
Tool { name: "gdrive_create_folder".into(),
       description: Some("Create a folder in Google Drive.".into()),
       input_schema: schema!({"name":{"type":"string"},"parent_id":{"type":"string"}}, ["name"]) },
```

**3. Add a match arm in the `call()` dispatcher in `lib.rs`:**
```rust
"gdrive_create_folder" => drive::create_folder(&self.0, s("name")?, a.get("parent_id").and_then(|v| v.as_str())).await,
```

That's it. No registration elsewhere — `main.rs` calls `tool_list()` dynamically.

---

## Tool Summary

| Crate | Module | Tools |
|-------|--------|-------|
| axon-google | auth | `google_auth_url/exchange_code/auth_status/revoke` |
| axon-google | gmail | list, get, send, reply, search, trash, mark_read, list_labels |
| axon-google | calendar | list_events, create/update/delete_event, quick_add, get_freebusy |
| axon-google | drive | list, search, upload_text, download_text, share, delete |
| axon-microsoft | auth | `microsoft_auth_url/exchange_code/auth_status/revoke` |
| axon-microsoft | outlook | list, get, send, reply, search, delete, mark_read, list_folders |
| axon-microsoft | calendar | list/create/update/delete_event, accept/decline |
| axon-microsoft | onedrive | list, search, upload/download_text, delete |
| axon-microsoft | teams | list_joined, list_channels, send_message, list/send chats |
| axon-facebook | auth | `facebook_auth_url/exchange_code/auth_status/debug_token` |
| axon-facebook | page | get_page, update_page |
| axon-facebook | posts | list, get, create, create_with_image, update, delete, schedule |
| axon-facebook | comments | list, reply, delete, hide, like, unlike |
| axon-facebook | insights | page_insights, post_insights, page_fans, page_reach |
| axon-facebook | messaging | list_conversations, get_conversation, send text/image |
| axon-facebook | instagram | get_account, list_media, create_post, list_comments, reply, insights |
| axon-crm | orgs | create, list, get, update, delete, search |
| axon-crm | leads | create, list, get, update, delete, search, convert_to_deal |
| axon-crm | deals | create, list, get, update, delete, search, pipeline_summary |
| axon-crm | activities | log, list, get, update, delete |
| axon-crm | records | archive, restore, archived_list, export_snapshot |
| axon-crm | views | search_all, record_overview, dashboard_summary |
| axon-business | notes | create, list, get, update, delete, search, export |
| axon-business | tasks | create, list, get, complete, update, delete, overdue |
| axon-business | contacts | create, list, get, search, update, delete |
| axon-business | datetime | now, convert, diff, add, format |
| axon-business | text | word_count, summarize_lines, extract_emails/urls, slugify, template |
