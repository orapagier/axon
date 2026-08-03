use axum::{extract::{Multipart, State}, Json};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::AsyncWriteExt;

use crate::routes::{ActionResponse, AppError};
use crate::server::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Open a file, URL, or application
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct OpenRequest {
    pub target: String,
}

pub async fn open_target(Json(req): Json<OpenRequest>) -> Result<Json<ActionResponse>, AppError> {
    open::that(&req.target).map_err(|e| AppError::internal(e.to_string()))?;
    Ok(ActionResponse::ok())
}

// ─────────────────────────────────────────────────────────────────────────────
// Upload file — multipart/form-data, streaming, no size limit
//
// Fields:
//   file  — the file binary (required)
//   path  — destination path on the Windows machine (required)
//             Supports shortcuts: Desktop/, Documents/, Downloads/, OneDrive/, ~/
//
// POST /files/upload
// Content-Type: multipart/form-data
//
// NOTE: This endpoint must be registered OUTSIDE the RequestBodyLimitLayer in
// server.rs — see comment there. The route accepts unlimited size by design.
// ─────────────────────────────────────────────────────────────────────────────

pub async fn upload_file(mut multipart: Multipart) -> Result<Json<serde_json::Value>, AppError> {
    let mut dest_path: Option<String> = None;
    let mut bytes_written: u64 = 0;
    let mut filename_written: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("Multipart error: {}", e)))?
    {
        match field.name() {
            Some("path") => {
                dest_path = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(e.to_string()))?,
                );
            }
            Some("file") => {
                let path_str = dest_path
                    .as_deref()
                    .ok_or_else(|| AppError::bad_request(
                        "'path' field must come before 'file' in the multipart body",
                    ))?;

                // Resolve path shortcuts before opening the file
                let resolved = resolve_upload_path(path_str);

                // Create parent directories
                if let Some(parent) = Path::new(&resolved).parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| AppError::internal(format!(
                            "Could not create directory {}: {}", parent.display(), e
                        )))?;
                }

                // Stream field bytes directly to disk — never buffers entire file in memory
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&resolved)
                    .await
                    .map_err(|e| AppError::internal(format!(
                        "Could not open {} for writing: {}", resolved, e
                    )))?;

                // Read and write in chunks
                let mut stream = field;
                loop {
                    match stream.chunk().await {
                        Ok(Some(chunk)) => {
                            file.write_all(&chunk)
                                .await
                                .map_err(|e| AppError::internal(format!(
                                    "Write error: {}", e
                                )))?;
                            bytes_written += chunk.len() as u64;
                        }
                        Ok(None) => break,
                        Err(e) => {
                            return Err(AppError::internal(format!("Stream read error: {}", e)));
                        }
                    }
                }

                file.flush()
                    .await
                    .map_err(|e| AppError::internal(format!("Flush error: {}", e)))?;

                filename_written = Some(
                    Path::new(&resolved)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| resolved.clone()),
                );
            }
            _ => {
                // Drain unknown fields so the multipart parser stays in sync
                let mut f = field;
                while f.chunk().await.unwrap_or(None).is_some() {}
            }
        }
    }

    let path = dest_path.ok_or_else(|| AppError::bad_request("Missing 'path' field"))?;
    let filename = filename_written.ok_or_else(|| AppError::bad_request("Missing 'file' field"))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "path": path,
        "filename": filename,
        "bytes_written": bytes_written,
        "size_human": human_size(bytes_written),
    })))
}

/// Resolve path shortcuts for upload destinations.
/// Mirrors _resolve_path in windows.py so shortcuts work consistently.
fn resolve_upload_path(path: &str) -> String {
    let p = path.replace('/', "\\");

    // ~ expansion
    if p.starts_with("~\\") || p == "~" {
        if let Ok(home) = std::env::var("USERPROFILE") {
            return format!("{}\\{}", home, p.trim_start_matches('~').trim_start_matches('\\'));
        }
    }

    let p_lower = p.to_lowercase();

    // Get common base paths from environment
    let onedrive = std::env::var("OneDriveCommercial")
        .or_else(|_| std::env::var("OneDriveConsumer"))
        .or_else(|_| std::env::var("OneDrive"))
        .ok();
    let userprofile = std::env::var("USERPROFILE").ok();
    let home = userprofile.as_deref().unwrap_or("C:\\Users\\User");
    let od = onedrive.as_deref().unwrap_or(home);

    let shortcuts = [
        ("desktop\\",   format!("{}\\Desktop\\", od)),
        ("desktop",     format!("{}\\Desktop", od)),
        ("documents\\", format!("{}\\Documents\\", od)),
        ("documents",   format!("{}\\Documents", od)),
        ("downloads\\", format!("{}\\Downloads\\", home)),
        ("downloads",   format!("{}\\Downloads", home)),
        ("onedrive\\",  format!("{}\\", od)),
        ("onedrive",    od.to_string()),
    ];

    for (shortcut, full) in &shortcuts {
        if p_lower.starts_with(shortcut) {
            return format!("{}{}", full, &p[shortcut.len()..]);
        }
    }

    p
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Read file
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PathRequest {
    pub path: String,
}

/// Read a file's contents.
/// - Text files: returns `content` inline AND a `url` download link.
/// - Binary files: returns only a `url` download link.
pub async fn read_file(
    State(state): State<AppState>,
    Json(req): Json<PathRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let source = Path::new(&req.path);

    if !source.exists() {
        return Err(AppError::bad_request(format!("File not found: {}", req.path)));
    }
    if !source.is_file() {
        return Err(AppError::bad_request(format!("Not a file: {}", req.path)));
    }

    match tokio::fs::read_to_string(&req.path).await {
        Ok(content) => {
            let link = build_download_link(&req.path, &state).await?;
            Ok(Json(serde_json::json!({
                "content": content,
                "size_bytes": link.size_bytes,
                "path": req.path,
                "url": link.url,
                "filename": link.filename,
                "note": "Text file — content is inline and also available via the URL for 30 minutes."
            })))
        }
        Err(_) => {
            let link = build_download_link(&req.path, &state).await?;
            Ok(Json(serde_json::json!({
                "url": link.url,
                "filename": link.filename,
                "size_bytes": link.size_bytes,
                "path": req.path,
                "note": "Binary file — download via the URL (available for 30 minutes)."
            })))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Search files
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchRequest {
    pub path: String,
    #[serde(default = "wildcard")]
    pub pattern: String,
    #[serde(default = "yes")]
    pub recursive: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn wildcard() -> String { "*".to_string() }
fn yes() -> bool { true }
fn default_limit() -> usize { 200 }

#[derive(Serialize)]
pub struct SearchResult {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub modified: Option<String>,
}

pub async fn search_files(
    Json(req): Json<SearchRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !Path::new(&req.path).exists() {
        return Err(AppError::bad_request(format!(
            "Directory not found: {}",
            req.path
        )));
    }

    let root = req.path.clone();
    let pattern = req.pattern.clone();
    let recursive = req.recursive;
    let limit = req.limit;

    let results = tokio::task::spawn_blocking(move || {
        let mut hits: Vec<SearchResult> = Vec::new();
        walk_dir(Path::new(&root), &pattern, recursive, limit, &mut hits);
        hits
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?;

    let count = results.len();
    Ok(Json(serde_json::json!({
        "count": count,
        "results": results,
        "note": if count == 0 {
            format!("No files matching '{}' found in '{}'", req.pattern, req.path)
        } else {
            format!("Found {} file(s) matching '{}'", count, req.pattern)
        }
    })))
}

fn walk_dir(dir: &Path, pattern: &str, recursive: bool, limit: usize, hits: &mut Vec<SearchResult>) {
    if hits.len() >= limit { return; }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if hits.len() >= limit { break; }
        let path = entry.path();
        let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
        if meta.is_dir() {
            if recursive { walk_dir(&path, pattern, recursive, limit, hits); }
            continue;
        }
        if !meta.is_file() { continue; }
        let name = entry.file_name().to_string_lossy().to_string();
        if matches_pattern(&name, pattern) {
            let modified = meta.modified().ok().map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string()
            });
            hits.push(SearchResult {
                path: path.to_string_lossy().to_string(),
                name,
                size_bytes: meta.len(),
                modified,
            });
        }
    }
}

fn matches_pattern(name: &str, pattern: &str) -> bool {
    if pattern == "*" || pattern.is_empty() { return true; }
    let name_lc = name.to_lowercase();
    let pat_lc = pattern.to_lowercase();
    if !pat_lc.contains('*') { return name_lc == pat_lc; }
    let parts: Vec<&str> = pat_lc.split('*').collect();
    let mut cursor = name_lc.as_str();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() { continue; }
        if i == 0 {
            if !cursor.starts_with(part as &str) { return false; }
            cursor = &cursor[part.len()..];
        } else if i == parts.len() - 1 {
            if !cursor.ends_with(part as &str) { return false; }
        } else {
            match cursor.find(part as &str) {
                Some(pos) => cursor = &cursor[pos + part.len()..],
                None => return false,
            }
        }
    }
    true
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helper — build a public download link for any local file
// ─────────────────────────────────────────────────────────────────────────────

struct DownloadLink {
    url: String,
    filename: String,
    size_bytes: u64,
}

async fn build_download_link(file_path: &str, state: &AppState) -> Result<DownloadLink, AppError> {
    let source = Path::new(file_path);
    let meta = tokio::fs::metadata(source)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    let original_name = source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = source
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "bin".to_string());

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = format!("{}_{}.{}", sanitize_filename(&original_name), ts, ext);

    let public_dir = crate::server::public_dir();
    tokio::fs::create_dir_all(&public_dir)
        .await
        .map_err(|e| AppError::internal(format!("Failed to create public dir: {}", e)))?;

    let dest = public_dir.join(&filename);
    tokio::fs::copy(source, &dest)
        .await
        .map_err(|e| AppError::internal(format!("Failed to copy file: {}", e)))?;

    let cleanup_dir = public_dir.clone();
    tokio::spawn(async move {
        let ttl = std::time::Duration::from_secs(30 * 60);
        if let Ok(mut entries) = tokio::fs::read_dir(&cleanup_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(m) = entry.metadata().await {
                    let time = m.created().unwrap_or_else(|_| {
                        m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                    });
                    if let Ok(age) = time.elapsed() {
                        if age > ttl { let _ = tokio::fs::remove_file(entry.path()).await; }
                    }
                }
            }
        }
    });

    let public_url = &state.config.public_url;
    let base = if public_url.trim().is_empty() {
        tracing::warn!("public_url is not configured; file URL will be relative");
        String::new()
    } else {
        public_url.trim_end_matches('/').to_string()
    };

    Ok(DownloadLink {
        url: format!("{}/public/{}", base, filename),
        filename,
        size_bytes: meta.len(),
    })
}

async fn create_download_link(
    file_path: &str,
    state: &AppState,
) -> Result<Json<serde_json::Value>, AppError> {
    let link = build_download_link(file_path, state).await?;
    Ok(Json(serde_json::json!({
        "url": link.url,
        "filename": link.filename,
        "size_bytes": link.size_bytes,
        "path": file_path,
        "note": "Download via the URL (available for 30 minutes)."
    })))
}

// ─────────────────────────────────────────────────────────────────────────────
// Write file (text only — for binary use /files/upload)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct WriteRequest {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub append: bool,
}

pub async fn write_file(Json(req): Json<WriteRequest>) -> Result<Json<ActionResponse>, AppError> {
    if let Some(parent) = Path::new(&req.path).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
    }
    if req.append {
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&req.path)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        file.write_all(req.content.as_bytes())
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
    } else {
        tokio::fs::write(&req.path, &req.content)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
    }
    Ok(ActionResponse::ok())
}

// ─────────────────────────────────────────────────────────────────────────────
// List directory
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified: Option<String>,
}

pub async fn list_dir(Json(req): Json<PathRequest>) -> Result<Json<Vec<FileEntry>>, AppError> {
    let mut entries = tokio::fs::read_dir(&req.path)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let mut result = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    {
        let meta = entry.metadata().await.ok();
        let modified = meta.as_ref().and_then(|m| {
            m.modified().ok().map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string()
            })
        });
        let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        result.push(FileEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
            is_dir,
            size_bytes,
            modified,
        });
    }
    result.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(Json(result))
}

// ─────────────────────────────────────────────────────────────────────────────
// Delete
// ─────────────────────────────────────────────────────────────────────────────

pub async fn delete_file(Json(req): Json<PathRequest>) -> Result<Json<ActionResponse>, AppError> {
    let path = Path::new(&req.path);
    if path.is_dir() {
        tokio::fs::remove_dir_all(path).await.map_err(|e| AppError::internal(e.to_string()))?;
    } else {
        tokio::fs::remove_file(path).await.map_err(|e| AppError::internal(e.to_string()))?;
    }
    Ok(ActionResponse::ok())
}

// ─────────────────────────────────────────────────────────────────────────────
// Exists check
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ExistsResponse {
    pub exists: bool,
    pub is_dir: bool,
    pub is_file: bool,
}

pub async fn file_exists(Json(req): Json<PathRequest>) -> Json<ExistsResponse> {
    let path = Path::new(&req.path);
    Json(ExistsResponse {
        exists: path.exists(),
        is_dir: path.is_dir(),
        is_file: path.is_file(),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Link file (generate download URL)
// ─────────────────────────────────────────────────────────────────────────────

pub async fn link_file(
    State(state): State<AppState>,
    Json(req): Json<PathRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let source = Path::new(&req.path);
    if !source.exists() {
        return Err(AppError::bad_request(format!("File not found: {}", req.path)));
    }
    if !source.is_file() {
        return Err(AppError::bad_request(format!("Path is not a file: {}", req.path)));
    }
    create_download_link(&req.path, &state).await
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
