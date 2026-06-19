use anyhow::Context;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFile {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub size_bytes: usize,
    pub platform: Option<String>,
    pub chat_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub filename: String,
    pub mime_type: Option<String>,
    pub path: String,
    pub direction: String,
    pub size_bytes: Option<i64>,
    pub platform: Option<String>,
    pub chat_id: Option<String>,
    pub created_at: String,
}

#[derive(Clone)]
pub struct FileHandler {
    incoming_dir: PathBuf,
    _outgoing_dir: PathBuf,
    db: Arc<Pool<SqliteConnectionManager>>,
}

impl FileHandler {
    pub fn new(db: Arc<Pool<SqliteConnectionManager>>) -> anyhow::Result<Self> {
        let dir = PathBuf::from("data/files");
        std::fs::create_dir_all(&dir)?;
        Ok(FileHandler {
            incoming_dir: dir.clone(),
            _outgoing_dir: dir,
            db,
        })
    }
    /// Recursively scans JSON for paths in data/files and indexes them
    pub async fn register_from_json(&self, val: &serde_json::Value, platform: Option<String>) {
        match val {
            serde_json::Value::String(s) => {
                let path = std::path::Path::new(s);
                let is_likely_ours = if s.len() > 3 {
                    let s_low = s.to_lowercase().replace("\\", "/");
                    s_low.contains("data/files")
                        || s_low.contains("/data/")
                        || s_low.contains("\\data\\")
                        || s_low.contains("axon-agent/data")
                } else {
                    false
                };

                if is_likely_ours && path.exists() && path.is_file() {
                    tracing::info!("Found potential file path in JSON: {}", s);
                    let filename = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    if let Ok(bytes) = std::fs::read(path) {
                        let hash = format!("{:x}", Sha256::digest(&bytes));
                        tracing::info!(
                            "Registering discovered file: {} (hash: {})",
                            filename,
                            hash
                        );
                        let _ = self
                            .store_path(
                                hash,
                                filename,
                                s.clone(),
                                "application/octet-stream".to_string(), // Default
                                bytes.len(),
                                platform,
                            )
                            .await;
                    }
                }
            }
            serde_json::Value::Object(map) => {
                for v in map.values() {
                    Box::pin(self.register_from_json(v, platform.clone())).await;
                }
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    Box::pin(self.register_from_json(v, platform.clone())).await;
                }
            }
            _ => {}
        }
    }

    /// Register an existing file in the staging directory into the database
    pub async fn store_path(
        &self,
        id: String,
        filename: String,
        path: String,
        mime_type: String,
        size: usize,
        platform: Option<String>,
    ) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "INSERT OR REPLACE INTO files (id,filename,mime_type,path,direction,size_bytes,platform,chat_id,created_at) VALUES (?1,?2,?3,?4,'incoming',?5,?6,?7,datetime('now'))",
            rusqlite::params![id, filename, mime_type, path, size as i64, platform, None::<String>],
        )?;
        Ok(())
    }

    pub async fn store_incoming(&self, file: AgentFile) -> anyhow::Result<(String, String)> {
        let hash = format!("{:x}", Sha256::digest(&file.bytes));
        let safe = file
            .filename
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();

        let staged_name = format!("{}_{}", &hash[..8], safe);
        let dest = self.incoming_dir.join(&staged_name);

        if !dest.exists() {
            tokio::fs::write(&dest, &file.bytes)
                .await
                .context("write incoming file")?;
        }

        let path_str = dest.to_string_lossy().to_string();
        self.store_path(
            hash.clone(),
            file.filename,
            path_str.clone(),
            file.mime_type,
            file.size_bytes,
            file.platform,
        )
        .await?;

        Ok((hash, path_str))
    }
    pub async fn read(&self, id: &str) -> anyhow::Result<AgentFile> {
        let (filename, mime_type, path) = {
            let conn = self.db.get().context("DB pool")?;
            conn.query_row(
                "SELECT filename,mime_type,path FROM files WHERE id=?1",
                rusqlite::params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .context("File not found")?
        };
        let bytes = tokio::fs::read(&path).await.context("read file")?;
        Ok(AgentFile {
            id: id.to_string(),
            filename,
            mime_type: mime_type.unwrap_or_else(|| {
                mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .to_string()
            }),
            size_bytes: bytes.len(),
            bytes,
            platform: None,
            chat_id: None,
        })
    }
    pub fn list(&self, direction: &str) -> anyhow::Result<Vec<FileRecord>> {
        let conn = self.db.get().context("DB pool")?;
        let mut s = conn.prepare("SELECT id,filename,mime_type,path,direction,size_bytes,platform,chat_id,created_at FROM files WHERE direction=?1 ORDER BY created_at DESC LIMIT 100")?;
        let rows = s.query_map(rusqlite::params![direction], |r| {
            Ok(FileRecord {
                id: r.get(0)?,
                filename: r.get(1)?,
                mime_type: r.get(2)?,
                path: r.get(3)?,
                direction: r.get(4)?,
                size_bytes: r.get(5)?,
                platform: r.get(6)?,
                chat_id: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?;

        let res: Vec<FileRecord> = rows.filter_map(|r| r.ok()).collect();
        Ok(res)
    }

    pub async fn delete(&self, id: &str) -> anyhow::Result<()> {
        let path: Option<String> = {
            let conn = self.db.get().context("DB pool")?;
            conn.query_row(
                "SELECT path FROM files WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .optional()?
        };

        if let Some(p) = path {
            let _ = tokio::fs::remove_file(p).await;
            let conn = self.db.get().context("DB pool")?;
            conn.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![id])?;
        }
        Ok(())
    }

    pub async fn delete_all(&self, direction: Option<&str>) -> anyhow::Result<usize> {
        let to_delete: Vec<String> = {
            let conn = self.db.get().context("DB pool")?;
            let sql = if direction.is_some() {
                "SELECT path FROM files WHERE direction = ?1"
            } else {
                "SELECT path FROM files"
            };
            let mut stmt = conn.prepare(sql)?;
            if let Some(dir) = direction {
                let rows = stmt.query_map(rusqlite::params![dir], |r| r.get::<_, String>(0))?;
                rows.filter_map(|r| r.ok()).collect()
            } else {
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.filter_map(|r| r.ok()).collect()
            }
        };

        for path in &to_delete {
            let _ = tokio::fs::remove_file(path).await;
        }

        let conn = self.db.get().context("DB pool")?;
        if let Some(dir) = direction {
            conn.execute(
                "DELETE FROM files WHERE direction = ?1",
                rusqlite::params![dir],
            )?;
        } else {
            conn.execute("DELETE FROM files", [])?;
        }

        Ok(to_delete.len())
    }

    pub async fn cleanup_old(&self, max_age: std::time::Duration) -> anyhow::Result<usize> {
        let threshold = (chrono::Utc::now() - chrono::Duration::from_std(max_age).unwrap())
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        let to_delete: Vec<(String, String)> = {
            let conn = self.db.get().context("DB pool")?;
            let mut s = conn.prepare("SELECT id, path FROM files WHERE created_at < ?1")?;
            let rows = s.query_map(rusqlite::params![threshold], |r| {
                let id: String = r.get(0)?;
                let path: String = r.get(1)?;
                Ok((id, path))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let mut count = 0;
        for (id, path) in to_delete {
            let _ = tokio::fs::remove_file(path).await;
            if let Ok(conn) = self.db.get() {
                let _ = conn.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![id]);
            }
            count += 1;
        }

        if count > 0 {
            tracing::info!("Cleaned up {} old stored files", count);
        }
        Ok(count)
    }
}
