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

/// Resolve where to save a binary file named `name` (of `new_size` bytes) inside
/// `dir`, following the shared "overwrite same file, number different files"
/// policy used by every node and the agent when persisting binaries:
///
/// - If no file with that name exists, use it.
/// - If a file with that name exists **and has the same size**, it is assumed to
///   be the same file and gets overwritten (only the newest copy is kept).
/// - Otherwise the existing file is a genuinely different file, so a numbered
///   variant `name (1)`, `name (2)`, … is used. The same size rule is applied to
///   each numbered slot, so re-saving the same differing file overwrites its own
///   numbered copy instead of endlessly piling up.
pub fn resolve_dedup_path(dir: &std::path::Path, name: &str, new_size: u64) -> PathBuf {
    let p = std::path::Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str());

    for n in 0..10_000 {
        let candidate_name = if n == 0 {
            name.to_string()
        } else {
            match ext {
                Some(e) => format!("{stem} ({n}).{e}"),
                None => format!("{stem} ({n})"),
            }
        };
        let candidate = dir.join(&candidate_name);
        match fs::metadata(&candidate) {
            // Free slot — nothing there, use it.
            Err(_) => return candidate,
            // Same size → assume it's the same file and overwrite.
            Ok(meta) if meta.len() == new_size => return candidate,
            // Different size → a different file lives here; try the next slot.
            Ok(_) => continue,
        }
    }
    dir.join(name)
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

#[cfg(test)]
mod tests {
    use super::resolve_dedup_path;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "axon-dedup-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unused_name_returns_base() {
        let dir = scratch_dir("base");
        let path = resolve_dedup_path(&dir, "report.pdf", 100);
        assert_eq!(path, dir.join("report.pdf"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_name_same_size_overwrites() {
        let dir = scratch_dir("same");
        std::fs::write(dir.join("report.pdf"), vec![0u8; 100]).unwrap();
        // Same size → reuse the existing path (overwrite).
        let path = resolve_dedup_path(&dir, "report.pdf", 100);
        assert_eq!(path, dir.join("report.pdf"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_name_different_size_is_numbered() {
        let dir = scratch_dir("diff");
        std::fs::write(dir.join("report.pdf"), vec![0u8; 100]).unwrap();
        // Different size → keep the original, use a numbered variant.
        let path = resolve_dedup_path(&dir, "report.pdf", 200);
        assert_eq!(path, dir.join("report (1).pdf"));

        // A second differing file takes the next slot.
        std::fs::write(dir.join("report (1).pdf"), vec![0u8; 200]).unwrap();
        let path2 = resolve_dedup_path(&dir, "report.pdf", 300);
        assert_eq!(path2, dir.join("report (2).pdf"));

        // Re-saving the first differing file (size 200) overwrites its own
        // numbered copy rather than creating a new one.
        let path3 = resolve_dedup_path(&dir, "report.pdf", 200);
        assert_eq!(path3, dir.join("report (1).pdf"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extensionless_names_are_numbered() {
        let dir = scratch_dir("noext");
        std::fs::write(dir.join("archive"), vec![0u8; 10]).unwrap();
        let path = resolve_dedup_path(&dir, "archive", 20);
        assert_eq!(path, dir.join("archive (1)"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
