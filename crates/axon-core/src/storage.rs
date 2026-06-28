use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

// ── Paths ─────────────────────────────────────────────────────────────────────

pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("axon-mcp")
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("axon-mcp")
}

/// Resolve the app's `data/files` staging/download directory.
///
/// Every node and the agent must read and write binaries through the SAME
/// directory so a file saved by one (e.g. a Sheets/Drive export) is found by
/// another (e.g. the Telegram sender) and indexed by the Files page. Honors
/// `AXON_DATA_DIR` when set, otherwise the relative `data/files` directory the
/// app creates at startup.
///
/// NOTE: do NOT hardcode an absolute `/data/files` — that points at the
/// filesystem root, a different directory from `$CWD/data/files`, and silently
/// drops files where nothing else can find them.
pub fn data_files_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AXON_DATA_DIR") {
        let base = PathBuf::from(dir);
        // Accept a path that already points at the `files` dir; otherwise use the
        // conventional `<AXON_DATA_DIR>/files` staging sub-dir.
        if base.file_name().and_then(|n| n.to_str()) == Some("files") {
            return base;
        }
        return base.join("files");
    }
    PathBuf::from("data/files")
}

fn creds_path() -> PathBuf {
    // Working-directory file always takes priority so admins can update
    // credentials.json in the deployment folder and have it picked up.
    let local = PathBuf::from("credentials.json");
    let data_path = data_dir().join("credentials.json");

    if local.exists() {
        // Sync local → data dir so both stay in sync
        let _ = std::fs::create_dir_all(data_dir());
        let _ = std::fs::copy(&local, &data_path);
        return local;
    }

    // Fall back to data dir (e.g. fresh install where only data dir has creds)
    if data_path.exists() {
        data_path
    } else {
        local // will fail later with a clear error
    }
}

fn tokens_path() -> PathBuf {
    // Working-directory file takes priority (same logic as creds_path)
    let local = PathBuf::from("tokens.json");
    let data_path = data_dir().join("tokens.json");

    if local.exists() {
        let _ = std::fs::create_dir_all(data_dir());
        let _ = std::fs::copy(&local, &data_path);
        return local;
    }

    if data_path.exists() {
        data_path
    } else {
        // Default to data dir for new writes
        let _ = std::fs::create_dir_all(data_dir());
        data_path
    }
}

/// Parse credentials.json, accepting either Axon's native flat format
/// (`{"google": {...}, "microsoft": {...}, "facebook": {...}}`) or a raw
/// Google Cloud Console OAuth client download (`{"web": {...}}` or
/// `{"installed": {...}}`), which is mapped onto the `google` section.
///
/// Without this, dropping the console-downloaded JSON in as credentials.json
/// would silently parse to empty Google creds and later fail with a confusing
/// "Google credentials not configured" / token error.
fn parse_credentials(raw: &str) -> Result<Credentials> {
    let value: serde_json::Value =
        serde_json::from_str(raw).context("credentials.json is not valid JSON")?;

    // Google Cloud Console client download → map to the google section.
    if let Some(client) = value.get("web").or_else(|| value.get("installed")) {
        let client_id = client
            .get("client_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !client_id.is_empty() {
            let client_secret = client
                .get("client_secret")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            return Ok(Credentials {
                google: Some(GoogleCreds {
                    client_id,
                    client_secret,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
    }

    // Axon's native flat format.
    serde_json::from_value(value)
        .context("expected google/microsoft/facebook sections or a Google client JSON")
}

fn ensure_dirs() -> Result<()> {
    fs::create_dir_all(config_dir())?;
    fs::create_dir_all(data_dir())?;
    Ok(())
}

// ── Credential types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GoogleCreds {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub places_api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MicrosoftCreds {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default = "default_tenant")]
    pub tenant_id: String,
}
fn default_tenant() -> String {
    "common".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FacebookCreds {
    pub app_id: String,
    pub app_secret: String,
    pub page_id: String,
    #[serde(default)]
    pub page_access_token: String,
    pub instagram_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Credentials {
    pub google: Option<GoogleCreds>,
    pub microsoft: Option<MicrosoftCreds>,
    pub facebook: Option<FacebookCreds>,
}

// ── Token types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64, // Unix timestamp ms
}

impl OAuthToken {
    pub fn is_expired(&self) -> bool {
        Utc::now().timestamp_millis() > self.expires_at - 60_000
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacebookToken {
    pub page_access_token: String,
    pub user_access_token: Option<String>,
    pub instagram_business_account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tokens {
    pub google: Option<OAuthToken>,
    pub microsoft: Option<OAuthToken>,
    pub facebook: Option<FacebookToken>,
}

// ── Storage ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Storage {
    pub credentials: Credentials,
    pub tokens: Tokens,
}

impl Storage {
    pub fn load() -> Result<Self> {
        ensure_dirs()?;

        let credentials = if creds_path().exists() {
            let raw = fs::read_to_string(creds_path()).context("reading credentials.json")?;
            parse_credentials(&raw).context("parsing credentials.json")?
        } else {
            Credentials::default()
        };

        let tokens = if tokens_path().exists() {
            let raw = fs::read_to_string(tokens_path()).context("reading tokens.json")?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            Tokens::default()
        };

        Ok(Self {
            credentials,
            tokens,
        })
    }

    pub fn reload_tokens(&mut self) -> Result<()> {
        if tokens_path().exists() {
            let raw = fs::read_to_string(tokens_path()).context("reading tokens.json")?;
            self.tokens = serde_json::from_str(&raw).unwrap_or_default();
        } else {
            self.tokens = Tokens::default();
        }
        Ok(())
    }

    pub fn save_tokens(&self) -> Result<()> {
        ensure_dirs()?;
        let json = serde_json::to_string_pretty(&self.tokens)?;
        let path = tokens_path();

        fs::write(&path, json).context("writing tokens.json")?;

        // Restrict file permissions to owner only (Unix)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn set_google_token(&mut self, token: OAuthToken) -> Result<()> {
        self.tokens.google = Some(token);
        self.save_tokens()
    }

    pub fn set_microsoft_token(&mut self, token: OAuthToken) -> Result<()> {
        self.tokens.microsoft = Some(token);
        self.save_tokens()
    }

    pub fn set_facebook_token(&mut self, token: FacebookToken) -> Result<()> {
        self.tokens.facebook = Some(token);
        self.save_tokens()
    }

    // ── Credential getters ────────────────────────────────────────────────────

    pub fn google_creds(&self) -> Result<&GoogleCreds> {
        self.credentials.google.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Google credentials not configured. \
                 Add 'google' section to {}",
                creds_path().display()
            )
        })
    }

    pub fn microsoft_creds(&self) -> Result<&MicrosoftCreds> {
        self.credentials.microsoft.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Microsoft credentials not configured. \
                 Add 'microsoft' section to {}",
                creds_path().display()
            )
        })
    }

    pub fn facebook_creds(&self) -> Result<&FacebookCreds> {
        self.credentials.facebook.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Facebook credentials not configured. \
                 Add 'facebook' section to {}",
                creds_path().display()
            )
        })
    }

    /// Clear the Facebook page_access_token from credentials.json
    /// This is used when explicitly revoking Facebook auth
    pub fn clear_facebook_creds_token(&mut self) -> Result<()> {
        if let Some(ref mut fb) = self.credentials.facebook {
            fb.page_access_token.clear();
            self.save_credentials()?;
        }
        Ok(())
    }

    fn save_credentials(&self) -> Result<()> {
        ensure_dirs()?;
        let json = serde_json::to_string_pretty(&self.credentials)?;
        let path = creds_path();
        fs::write(&path, json).context("writing credentials.json")?;

        // Restrict file permissions to owner only (Unix)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}
