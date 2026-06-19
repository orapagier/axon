use crate::auth::access_token;
use anyhow::Result;
use axon_core::AppState;
use serde_json::{json, Value};

const BASE: &str = "https://graph.microsoft.com/v1.0";
const SEL: &str = "id,name,file,folder,size,lastModifiedDateTime,webUrl";

pub async fn list(state: &AppState, folder_id: Option<&str>, max_count: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let base = match folder_id {
        Some(f) => format!("{BASE}/me/drive/items/{f}/children"),
        None => format!("{BASE}/me/drive/root/children"),
    };
    let resp: Value = state
        .client
        .get(&base)
        .bearer_auth(&tok)
        .query(&[("$top", max_count.to_string()), ("$select", SEL.to_owned())])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn search(state: &AppState, query: &str, max_count: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let q = url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/drive/root/search(q='{q}')"))
        .bearer_auth(&tok)
        .query(&[("$top", max_count.to_string()), ("$select", SEL.to_owned())])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn move_file(state: &AppState, item_id: &str, new_folder_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .patch(format!("{BASE}/me/drive/items/{item_id}"))
        .bearer_auth(&tok)
        .json(&json!({
            "parentReference": { "id": new_folder_id }
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(json!({ "success": true, "moved_to": new_folder_id, "file": resp }))
}

pub async fn upload_binary(
    state: &AppState,
    local_path: &str,
    name: &str,
    folder_id: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let path = match folder_id {
        Some(f) => format!("{BASE}/me/drive/items/{f}:/{name}:/content"),
        None => format!("{BASE}/me/drive/root:/{name}:/content"),
    };
    let data = std::fs::read(local_path)?;
    let resp: Value = state
        .client
        .put(&path)
        .bearer_auth(&tok)
        .body(data)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn delete(state: &AppState, item_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/me/drive/items/{item_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true }))
}

pub async fn download_binary(state: &AppState, item_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let meta: Value = state
        .client
        .get(format!("{BASE}/me/drive/items/{item_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let name = meta["name"].as_str().unwrap_or("downloaded_file");

    let bytes = state
        .client
        .get(format!("{BASE}/me/drive/items/{item_id}/content"))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let download_dir = std::path::PathBuf::from("/data/files");
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(name);
    std::fs::write(&path, &bytes)?;
    Ok(json!({
        "name": name,
        "file_path": path.to_string_lossy(),
        "message": "File downloaded successfully. You can now access it at this local path to upload/send to the user."
    }))
}

pub async fn create_share_link(state: &AppState, item_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/me/drive/items/{item_id}/createLink"))
        .bearer_auth(&tok)
        .json(&json!({ "type": "view", "scope": "anonymous" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let share_url = resp["link"]["webUrl"].as_str().unwrap_or("");
    Ok(json!({ "share_link": share_url }))
}
