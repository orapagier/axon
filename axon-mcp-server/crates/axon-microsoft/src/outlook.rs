use crate::auth::access_token;
use anyhow::Result;
use axon_core::AppState;
use serde_json::{json, Value};

const BASE: &str = "https://graph.microsoft.com/v1.0";
const MSG_SEL: &str = "id,subject,from,toRecipients,receivedDateTime,isRead,bodyPreview,importance,hasAttachments,conversationId";

pub async fn list(
    state: &AppState,
    max_items: u32,
    folder_id: Option<&str>,
    filter: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let base = match folder_id {
        Some(f) => format!("{BASE}/me/mailFolders/{f}/messages"),
        None => format!("{BASE}/me/messages"),
    };
    let mut params = vec![
        ("$top", max_items.to_string()),
        ("$select", MSG_SEL.to_owned()),
        ("$orderby", "receivedDateTime desc".to_owned()),
    ];
    if let Some(f) = filter {
        params.push(("$filter", f.to_owned()));
    }

    let resp: Value = state
        .client
        .get(&base)
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn get(state: &AppState, message_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let sel  = "id,subject,from,toRecipients,ccRecipients,receivedDateTime,isRead,body,importance,hasAttachments,conversationId";
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/messages/{message_id}"))
        .bearer_auth(&tok)
        .query(&[("$select", sel)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
    is_html: bool,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut msg = json!({
        "subject": subject,
        "body": { "contentType": if is_html { "HTML" } else { "Text" }, "content": body },
        "toRecipients": [{ "emailAddress": { "address": to } }],
    });
    if let Some(cc) = cc {
        msg["ccRecipients"] = json!([{"emailAddress":{"address":cc}}]);
    }
    if let Some(bcc) = bcc {
        msg["bccRecipients"] = json!([{"emailAddress":{"address":bcc}}]);
    }

    state
        .client
        .post(format!("{BASE}/me/sendMail"))
        .bearer_auth(&tok)
        .json(&json!({"message": msg}))
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true }))
}

pub async fn send_with_attachment(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    local_path: &str,
    is_html: bool,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let data = std::fs::read(local_path)?;
    if data.len() > 3_000_000 {
        anyhow::bail!("File is too large ({} bytes). The Outlook sendMail API has a 3MB limit for attachments. For larger files, use OneDrive instead and share a link.", data.len());
    }
    let filename = std::path::Path::new(local_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

    let msg = json!({
        "subject": subject,
        "body": { "contentType": if is_html { "HTML" } else { "Text" }, "content": body },
        "toRecipients": [{ "emailAddress": { "address": to } }],
        "attachments": [{
            "@odata.type": "#microsoft.graph.fileAttachment",
            "name": filename,
            "contentBytes": encoded
        }]
    });

    state
        .client
        .post(format!("{BASE}/me/sendMail"))
        .bearer_auth(&tok)
        .json(&json!({"message": msg}))
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true }))
}

pub async fn reply(
    state: &AppState,
    message_id: &str,
    body: &str,
    reply_all: bool,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let endpoint = if reply_all { "replyAll" } else { "reply" };
    state
        .client
        .post(format!("{BASE}/me/messages/{message_id}/{endpoint}"))
        .bearer_auth(&tok)
        .json(&json!({"message": {}, "comment": body}))
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true }))
}

pub async fn search(state: &AppState, query: &str, max_items: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/messages"))
        .bearer_auth(&tok)
        .query(&[
            ("$search", format!("\"{query}\"")),
            ("$top", max_items.to_string()),
            ("$select", MSG_SEL.to_owned()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn delete(state: &AppState, message_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/me/messages/{message_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true }))
}

pub async fn mark_read(state: &AppState, message_id: &str, is_read: bool) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .patch(format!("{BASE}/me/messages/{message_id}"))
        .bearer_auth(&tok)
        .json(&json!({"isRead": is_read}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn list_folders(state: &AppState) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/mailFolders"))
        .bearer_auth(&tok)
        .query(&[(
            "$select",
            "id,displayName,totalItemCount,unreadItemCount,childFolderCount",
        )])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn download_attachment(
    state: &AppState,
    message_id: &str,
    attachment_id: &str,
    filename: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!(
            "{BASE}/me/messages/{message_id}/attachments/{attachment_id}"
        ))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let b64 = resp["contentBytes"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No contentBytes found in attachment"))?;
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)?;

    let download_dir = std::path::PathBuf::from("/data/files");
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(filename);
    std::fs::write(&path, &decoded)?;

    Ok(json!({
        "success": true,
        "file_path": path.to_string_lossy(),
        "message": "Attachment downloaded successfully and available at this local path."
    }))
}
