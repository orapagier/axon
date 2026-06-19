use anyhow::{Context, Result};
use async_trait::async_trait;
use russh::*;
use std::path::Path;
use std::sync::Arc;

struct ServerDetails {
    host: String,
    port: u16,
    user: String,
    auth_type: String,
    password: Option<String>,
    private_key: Option<String>,
}

#[derive(Clone)]
struct Client {}

#[async_trait]
impl client::Handler for Client {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh_keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct SshTool;

impl SshTool {
    async fn get_server_details(
        state: &crate::state::AppState,
        server_name: &str,
    ) -> Result<ServerDetails> {
        if let Ok(conn) = state.db.get() {
            let mut s = conn.prepare("SELECT ip, port, username, auth_type, password, private_key FROM ssh_servers WHERE name=?1")?;
            let mut rows = s.query(rusqlite::params![server_name])?;
            if let Some(r) = rows.next()? {
                let enc_pass: Option<String> = r.get(4)?;
                let enc_priv: Option<String> = r.get(5)?;

                let password = enc_pass
                    .filter(|p| !p.is_empty())
                    .map(|p| crate::crypto::decrypt_key(&p));
                let private_key = enc_priv
                    .filter(|p| !p.is_empty())
                    .map(|p| crate::crypto::decrypt_key(&p));

                return Ok(ServerDetails {
                    host: r.get(0)?,
                    port: r.get::<_, i64>(1)? as u16,
                    user: r.get(2)?,
                    auth_type: r.get(3)?,
                    password,
                    private_key,
                });
            }
        }
        anyhow::bail!("Server '{}' not found in database", server_name)
    }

    pub async fn list_servers(state: crate::state::AppState) -> Result<serde_json::Value> {
        let conn = state.db.get().context("DB error")?;
        let mut s =
            conn.prepare("SELECT name, ip, port, username FROM ssh_servers ORDER BY name")?;
        let mut rows = s.query([])?;
        let mut lines = Vec::new();
        while let Some(r) = rows.next()? {
            let name: String = r.get(0)?;
            let ip: String = r.get(1)?;
            let port: i64 = r.get(2)?;
            let user: String = r.get(3)?;
            lines.push(format!("- {}: {}@{}:{}", name, user, ip, port));
        }
        if lines.is_empty() {
            return Ok(serde_json::json!({ "output": "No SSH servers configured." }));
        }
        Ok(serde_json::json!({ "output": lines.join("\n") }))
    }

    pub async fn run_command(
        server_name: &str,
        command: &str,
        timeout_seconds: u64,
        state: crate::state::AppState,
    ) -> Result<serde_json::Value> {
        // Guardrails against destructive commands
        let lower = command.trim().to_lowercase();
        if lower.contains("rm -rf /") || lower.contains("rm -rf /*") {
            return Ok(
                serde_json::json!({"error": "Command blocked: 'rm -rf /' is prohibited for safety."}),
            );
        }
        let parts: Vec<&str> = command.split_whitespace().collect();
        if let Some(base) = parts.first() {
            if ["chmod", "chown", "iptables", "ufw", "passwd", "mkfs"].contains(base) {
                return Ok(
                    serde_json::json!({"error": format!("Command blocked: '{}' is prohibited to prevent server lockout.", base)}),
                );
            }
        }

        let session = Self::connect(server_name, &state).await?;
        let mut channel = session.channel_open_session().await?;
        channel.exec(true, command).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status = 0u32;
        let mut timed_out = false;

        let timeout = tokio::time::sleep(std::time::Duration::from_secs(timeout_seconds));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = &mut timeout, if !timed_out => {
                    timed_out = true;
                    let _ = channel.eof().await;
                    let _ = channel.close().await;
                    break;
                }
                msg_opt = channel.wait() => {
                    match msg_opt {
                        Some(msg) => {
                            match msg {
                                russh::ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                                russh::ChannelMsg::ExtendedData { ref data, .. } => stderr.extend_from_slice(data),
                                russh::ChannelMsg::ExitStatus { exit_status: s } => exit_status = s,
                                _ => {}
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        let out_str = String::from_utf8_lossy(&stdout).to_string();
        let err_str = String::from_utf8_lossy(&stderr).to_string();

        if timed_out {
            Ok(serde_json::json!({
                "output": format!("Process timed out after {}s. Partial output captured:\n\nSTDOUT:\n{}\n\nSTDERR:\n{}", timeout_seconds, out_str.trim(), err_str.trim()),
                "timeout": true
            }))
        } else if exit_status != 0 {
            Ok(serde_json::json!({
                "output": format!("Error on {} (exit {}):\n{}{}", server_name, exit_status, err_str, out_str)
            }))
        } else {
            Ok(serde_json::json!({
                "output": if out_str.trim().is_empty() { format!("Command executed on {} (no output).", server_name) } else { out_str.trim().to_string() }
            }))
        }
    }

    pub async fn upload_file(
        server_name: &str,
        remote_path: &str,
        local_path: &str,
        state: crate::state::AppState,
    ) -> Result<serde_json::Value> {
        if local_path.is_empty() {
            anyhow::bail!("local_path is required for upload_file");
        }
        let data = std::fs::read(local_path).context("Failed to read local file")?;

        let dir_cmd = format!("mkdir -p $(dirname '{}')", remote_path);
        // Using explicit 120s timeout for upload operations
        Self::run_command(server_name, &dir_cmd, 120, state.clone()).await?;

        let session = Self::connect(server_name, &state).await?;
        let mut channel = session.channel_open_session().await?;
        channel
            .exec(true, format!("cat > '{}'", remote_path))
            .await?;
        channel.data(&data[..]).await?;
        channel.eof().await?;

        let mut exit_status = 0u32;
        let mut stderr = Vec::new();
        while let Some(msg) = channel.wait().await {
            match msg {
                russh::ChannelMsg::ExtendedData { ref data, .. } => {
                    stderr.extend_from_slice(data);
                }
                russh::ChannelMsg::ExitStatus {
                    exit_status: status,
                } => {
                    exit_status = status;
                }
                _ => {}
            }
        }

        if exit_status != 0 {
            let err_str = String::from_utf8_lossy(&stderr);
            Ok(serde_json::json!({
                "output": format!("Upload failed on {} (exit {}):\n{}", server_name, exit_status, err_str)
            }))
        } else {
            Ok(serde_json::json!({
                "output": format!("Uploaded {} bytes to {} on {}.", data.len(), remote_path, server_name)
            }))
        }
    }

    pub async fn download_file(
        server_name: &str,
        remote_path: &str,
        state: crate::state::AppState,
    ) -> Result<serde_json::Value> {
        let session = Self::connect(server_name, &state).await?;
        let mut channel = session.channel_open_session().await?;
        channel.exec(true, format!("cat '{}'", remote_path)).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status = 0u32;

        while let Some(msg) = channel.wait().await {
            match msg {
                russh::ChannelMsg::Data { ref data } => {
                    stdout.extend_from_slice(data);
                }
                russh::ChannelMsg::ExtendedData { ref data, .. } => {
                    stderr.extend_from_slice(data);
                }
                russh::ChannelMsg::ExitStatus {
                    exit_status: status,
                } => {
                    exit_status = status;
                }
                _ => {}
            }
        }

        if exit_status != 0 {
            let err_str = String::from_utf8_lossy(&stderr);
            return Ok(serde_json::json!({
                "output": format!("Download failed on {} (exit {}):\n{}", server_name, exit_status, err_str)
            }));
        }

        let filename = Path::new(remote_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();

        let staged_path = crate::files::stage_bytes(&stdout, &filename)?;

        Ok(serde_json::json!({
            "output": format!("Downloaded {} bytes from {} to local path: {}", stdout.len(), remote_path, staged_path.display()),
            "message": format!("File downloaded successfully. You can now access it at this local path to upload/send to the user: {}", staged_path.display())
        }))
    }

    async fn connect(
        server_name: &str,
        state: &crate::state::AppState,
    ) -> Result<client::Handle<Client>> {
        let details = Self::get_server_details(state, server_name).await?;
        let config_arc = Arc::new(client::Config::default());
        let sh = Client {};
        let mut session =
            client::connect(config_arc, (details.host.as_str(), details.port), sh).await?;

        tracing::info!(
            "SSH: Connected to {}:{}. Attempting authentication as '{}'...",
            details.host,
            details.port,
            details.user
        );

        if details.auth_type == "password" {
            if let Some(pwd) = details.password {
                match session.authenticate_password(&details.user, pwd).await {
                    Ok(true) => return Ok(session),
                    Ok(false) => tracing::warn!("SSH: Password auth rejected"),
                    Err(e) => tracing::warn!("SSH: Password auth protocol error: {}", e),
                }
            }
        } else if let Some(priv_key) = details.private_key {
            match russh_keys::decode_secret_key(&priv_key, None) {
                Ok(key) => match session
                    .authenticate_publickey(&details.user, Arc::new(key))
                    .await
                {
                    Ok(true) => return Ok(session),
                    Ok(false) => tracing::warn!("SSH: Public key auth rejected"),
                    Err(e) => tracing::warn!("SSH: Public key auth protocol error: {}", e),
                },
                Err(e) => tracing::warn!("SSH: Failed to decode secret key: {}", e),
            }
        }

        anyhow::bail!(
            "Authentication failed for {}. Check credentials in Dashboard.",
            server_name
        );
    }
}
