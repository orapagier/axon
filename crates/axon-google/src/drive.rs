use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};
use std::path::Path;

const BASE: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3";

fn guess_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("html") | Some("htm") => "text/html",
        Some("xml") => "application/xml",
        Some("md") => "text/markdown",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("zip") => "application/zip",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

pub async fn list(
    state: &AppState,
    max_results: u32,
    folder_id: Option<&str>,
    mime_type: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut q_parts = vec!["trashed=false".to_owned()];
    if let Some(f) = folder_id {
        q_parts.push(format!("'{}' in parents", f));
    }
    if let Some(m) = mime_type {
        q_parts.push(format!("mimeType='{}'", m));
    }

    let resp: Value = state.client
        .get(format!("{BASE}/files"))
        .bearer_auth(&tok)
        .query(&[
            ("pageSize", max_results.to_string()),
            ("q",        q_parts.join(" and ")),
            ("orderBy",  "modifiedTime desc".into()),
            ("fields",   "files(id,name,mimeType,size,modifiedTime,parents,webViewLink,shared),nextPageToken".into()),
        ])
        .send().await?.ensure_ok().await?.json().await?;
    Ok(resp)
}

pub async fn search(state: &AppState, query: &str, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    // Drive query syntax uses single-quoted string literals. A raw single quote in the
    // search term breaks the syntax (e.g. "it's" → name contains 'it's'). Escape them.
    let escaped = query.replace('\'', "\\'");
    let q =
        format!("(name contains '{escaped}' or fullText contains '{escaped}') and trashed=false");
    let resp: Value = state
        .client
        .get(format!("{BASE}/files"))
        .bearer_auth(&tok)
        .query(&[
            ("pageSize", max_results.to_string()),
            ("q", q),
            (
                "fields",
                "files(id,name,mimeType,size,modifiedTime,webViewLink)".into(),
            ),
        ])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn upload_binary(
    state: &AppState,
    local_path: &str,
    name: &str,
    mime_type: &str,
    folder_id: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut meta = json!({ "name": name, "mimeType": mime_type });
    if let Some(f) = folder_id {
        meta["parents"] = json!([f]);
    }

    let data = std::fs::read(local_path)?;
    let boundary = "axon_mcp_drive_boundary";

    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n",
            serde_json::to_string(&meta)?
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}\r\nContent-Type: {mime_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());

    let resp: Value = state
        .client
        .post(format!(
            "{UPLOAD}/files?uploadType=multipart&fields=id,name,webViewLink"
        ))
        .bearer_auth(&tok)
        .header(
            "Content-Type",
            format!("multipart/related; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Recursively upload a local folder into Drive, preserving subfolder structure.
pub async fn upload_folder(
    state: &AppState,
    local_folder_path: &str,
    folder_name: Option<&str>,
    parent_folder_id: Option<&str>,
    include_hidden: bool,
) -> Result<Value> {
    let local_root = Path::new(local_folder_path);
    if !local_root.exists() {
        anyhow::bail!("Local folder does not exist: {local_folder_path}");
    }
    if !local_root.is_dir() {
        anyhow::bail!("Path is not a folder: {local_folder_path}");
    }

    let root_name = folder_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            local_root
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Uploaded Folder".to_string());

    let root_folder = create_folder(state, &root_name, parent_folder_id).await?;
    let root_folder_id = root_folder
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Drive create_folder response missing id"))?
        .to_string();

    let mut uploaded_files: Vec<Value> = Vec::new();
    let mut skipped_entries: Vec<Value> = Vec::new();
    let mut files_uploaded: u64 = 0;
    let mut folders_created: u64 = 1; // root folder
    let mut stack = vec![(local_root.to_path_buf(), root_folder_id.clone())];

    while let Some((current_local_dir, current_remote_parent)) = stack.pop() {
        let mut entries: Vec<std::fs::DirEntry> =
            std::fs::read_dir(&current_local_dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());

        for entry in entries {
            let entry_name = entry.file_name().to_string_lossy().to_string();
            if !include_hidden && entry_name.starts_with('.') {
                skipped_entries.push(json!({
                    "path": entry.path().to_string_lossy(),
                    "reason": "hidden entry skipped",
                }));
                continue;
            }

            let entry_path = entry.path();
            let entry_type = entry.file_type()?;

            if entry_type.is_dir() {
                let created =
                    create_folder(state, &entry_name, Some(&current_remote_parent)).await?;
                let child_folder_id = created
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Drive create_folder response missing id"))?
                    .to_string();
                folders_created += 1;
                stack.push((entry_path, child_folder_id));
                continue;
            }

            if entry_type.is_file() {
                let local_file = entry_path.to_string_lossy().to_string();
                let mime_type = guess_mime_type(&entry_path);
                let uploaded = upload_binary(
                    state,
                    &local_file,
                    &entry_name,
                    mime_type,
                    Some(&current_remote_parent),
                )
                .await?;
                files_uploaded += 1;
                uploaded_files.push(json!({
                    "local_path": local_file,
                    "drive_file": uploaded,
                }));
                continue;
            }

            skipped_entries.push(json!({
                "path": entry_path.to_string_lossy(),
                "reason": "unsupported filesystem entry type",
            }));
        }
    }

    Ok(json!({
        "success": true,
        "root_folder": root_folder,
        "files_uploaded": files_uploaded,
        "folders_created": folders_created,
        "uploaded_files": uploaded_files,
        "skipped_entries": skipped_entries,
    }))
}

pub async fn share(
    state: &AppState,
    file_id: &str,
    role: &str,
    r#type: &str,
    email: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut body = json!({ "role": role, "type": r#type });
    if let Some(e) = email {
        body["emailAddress"] = json!(e);
    }
    let send_email = email.is_some();

    let mut resp: Value = state
        .client
        .post(format!("{BASE}/files/{file_id}/permissions"))
        .bearer_auth(&tok)
        .query(&[("sendNotificationEmail", send_email.to_string())])
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;

    if r#type == "anyone" || r#type == "domain" {
        let meta: Value = state
            .client
            .get(format!("{BASE}/files/{file_id}"))
            .bearer_auth(&tok)
            .query(&[("fields", "webViewLink")])
            .send()
            .await?
            .ensure_ok()
            .await?
            .json()
            .await?;

        resp["share_link"] = meta["webViewLink"].clone();
    }

    Ok(resp)
}

pub async fn delete(state: &AppState, file_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "deletedFileId": file_id }))
}

pub async fn download_binary(state: &AppState, file_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;

    // Fetch both name and mimeType so we can detect Google Workspace files.
    let meta: Value = state
        .client
        .get(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .query(&[("fields", "name,mimeType")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    let name = meta["name"]
        .as_str()
        .unwrap_or("downloaded_file")
        .to_owned();
    let mime_type = meta["mimeType"].as_str().unwrap_or("");

    // Google Workspace files (Docs, Sheets, Slides, …) cannot be fetched with
    // ?alt=media — the API returns 403. They must go through the /export endpoint
    // which converts them to a portable format on the fly.
    let (bytes, export_name) = if mime_type.starts_with("application/vnd.google-apps.") {
        let (export_mime, extension) = match mime_type {
            "application/vnd.google-apps.document" => (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "docx",
            ),
            "application/vnd.google-apps.spreadsheet" => (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "xlsx",
            ),
            "application/vnd.google-apps.presentation" => (
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "pptx",
            ),
            "application/vnd.google-apps.drawing" => ("image/svg+xml", "svg"),
            "application/vnd.google-apps.script" => {
                ("application/vnd.google-apps.script+json", "json")
            }
            _ => ("application/pdf", "pdf"),
        };
        let b = state
            .client
            .get(format!("{BASE}/files/{file_id}/export"))
            .bearer_auth(&tok)
            .query(&[("mimeType", export_mime)])
            .send()
            .await?
            .ensure_ok()
            .await?
            .bytes()
            .await?;
        let export_name = format!("{name}.{extension}");
        (b, export_name)
    } else {
        let b = state
            .client
            .get(format!("{BASE}/files/{file_id}"))
            .bearer_auth(&tok)
            .query(&[("alt", "media")])
            .send()
            .await?
            .ensure_ok()
            .await?
            .bytes()
            .await?;
        (b, name.clone())
    };

    let download_dir = axon_core::data_files_dir();
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(&export_name);
    std::fs::write(&path, &bytes)?;
    Ok(json!({
        "name": export_name,
        "file_path": path.to_string_lossy(),
        "message": "File downloaded successfully. You can now access it at this local path to upload/send to the user."
    }))
}

pub async fn export(state: &AppState, file_id: &str, mime_type: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let meta: Value = state
        .client
        .get(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .query(&[("fields", "name")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    let name = meta["name"].as_str().unwrap_or("exported_file").to_owned();

    let bytes = state
        .client
        .get(format!("{BASE}/files/{file_id}/export"))
        .bearer_auth(&tok)
        .query(&[("mimeType", mime_type)])
        .send()
        .await?
        .ensure_ok()
        .await?
        .bytes()
        .await?;

    let ext = match mime_type {
        "application/pdf" => "pdf",
        "text/csv" => "csv",
        "application/zip" => "zip",
        "text/plain" => "txt",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => "bin",
    };

    let export_name = format!("{name}.{ext}");
    let download_dir = axon_core::data_files_dir();
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(&export_name);
    std::fs::write(&path, &bytes)?;

    Ok(json!({
        "name": export_name,
        "file_path": path.to_string_lossy(),
        "mime_type": mime_type,
        "message": format!("File exported as {mime_type} successfully.")
    }))
}

pub async fn move_file(state: &AppState, file_id: &str, new_folder_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;

    // 1. Get current parents
    let meta: Value = state
        .client
        .get(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .query(&[("fields", "parents")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;

    let current_parents = meta["parents"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();

    // 2. Move file
    let resp: Value = state
        .client
        .patch(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .query(&[
            ("addParents", new_folder_id),
            ("removeParents", &current_parents),
            ("fields", "id,name,parents"),
        ])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;

    Ok(json!({ "success": true, "moved_to": new_folder_id, "file": resp }))
}

/// Get metadata for a single file by ID.
pub async fn get_file(state: &AppState, file_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .query(&[(
            "fields",
            "id,name,mimeType,size,modifiedTime,parents,webViewLink,shared,description",
        )])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Create a new folder in Drive.
pub async fn create_folder(state: &AppState, name: &str, parent_id: Option<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut meta = json!({
        "name":     name,
        "mimeType": "application/vnd.google-apps.folder",
    });
    if let Some(p) = parent_id {
        meta["parents"] = json!([p]);
    }
    let resp: Value = state
        .client
        .post(format!("{BASE}/files"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,name,webViewLink")])
        .json(&meta)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Rename a file or update its description without touching its content.
pub async fn rename_file(
    state: &AppState,
    file_id: &str,
    new_name: Option<&str>,
    description: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut patch = json!({});
    if let Some(n) = new_name {
        patch["name"] = json!(n);
    }
    if let Some(d) = description {
        patch["description"] = json!(d);
    }
    if patch.as_object().map_or(true, |o| o.is_empty()) {
        anyhow::bail!("rename_file: at least one of new_name or description must be provided");
    }
    let resp: Value = state
        .client
        .patch(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,name,description")])
        .json(&patch)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Copy a file to an optional destination folder.
pub async fn copy_file(
    state: &AppState,
    file_id: &str,
    new_name: Option<&str>,
    destination_folder_id: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut body = json!({});
    if let Some(n) = new_name {
        body["name"] = json!(n);
    }
    if let Some(f) = destination_folder_id {
        body["parents"] = json!([f]);
    }
    let resp: Value = state
        .client
        .post(format!("{BASE}/files/{file_id}/copy"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,name,webViewLink")])
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Move a file to the trash (recoverable). Use `delete` for permanent removal.
pub async fn trash_file(state: &AppState, file_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .patch(format!("{BASE}/files/{file_id}"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,name,trashed")])
        .json(&json!({ "trashed": true }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// List all permissions (people/groups with access) on a file or folder.
pub async fn list_permissions(state: &AppState, file_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/files/{file_id}/permissions"))
        .bearer_auth(&tok)
        .query(&[(
            "fields",
            "permissions(id,type,role,emailAddress,displayName)",
        )])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Revoke a specific permission from a file. Use `list_permissions` to find the permission ID.
pub async fn remove_permission(
    state: &AppState,
    file_id: &str,
    permission_id: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!(
            "{BASE}/files/{file_id}/permissions/{permission_id}"
        ))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "removedPermissionId": permission_id }))
}
