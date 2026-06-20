use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use serde_json::{json, Value};

use crate::auth::access_token;

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

pub async fn list(state: &AppState, max_results: u32, query: Option<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params = vec![("maxResults", max_results.to_string())];
    if let Some(q) = query {
        params.push(("q", q.to_string()));
    }

    let list: Value = state
        .client
        .get(format!("{BASE}/messages"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;

    let msgs = list["messages"].as_array();
    if msgs.map_or(true, |m| m.is_empty()) {
        return Ok(json!({ "messages": [] }));
    }

    // Fetch metadata for each message in parallel (up to 20)
    let ids: Vec<String> = msgs
        .unwrap()
        .iter()
        .take(20)
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect();

    let mut handles = Vec::new();
    for id in ids {
        let c = state.client.clone();
        let t = tok.clone();
        handles.push(tokio::spawn(async move {
            c.get(format!("{BASE}/messages/{id}"))
                .bearer_auth(&t)
                .query(&[
                    ("format", "metadata"),
                    ("metadataHeaders", "Subject"),
                    ("metadataHeaders", "From"),
                    ("metadataHeaders", "To"),
                    ("metadataHeaders", "Date"),
                    ("metadataHeaders", "Message-ID"),
                ])
                .send()
                .await?
                .ensure_ok()
                .await?
                .json::<Value>()
                .await
                .map_err(anyhow::Error::from)
        }));
    }

    let mut result = Vec::new();
    for h in handles {
        if let Ok(Ok(msg)) = h.await {
            let headers: std::collections::HashMap<String, String> = msg["payload"]["headers"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|h| {
                            let name = h["name"].as_str()?;
                            let value = h["value"].as_str()?;
                            Some((name.to_lowercase(), value.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            result.push(json!({
                "id":        msg["id"],
                "threadId":  msg["threadId"],
                "labelIds":  msg["labelIds"],
                "snippet":   msg["snippet"],
                "subject":   headers.get("subject"),
                "from":      headers.get("from"),
                "to":        headers.get("to"),
                "date":      headers.get("date"),
            }));
        }
    }
    Ok(json!({ "messages": result, "nextPageToken": list["nextPageToken"] }))
}

pub async fn get(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let msg: Value = state
        .client
        .get(format!("{BASE}/messages/{id}"))
        .bearer_auth(&tok)
        .query(&[("format", "full")])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;

    let headers: std::collections::HashMap<String, String> = msg["payload"]["headers"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|h| {
            Some((
                h["name"].as_str()?.to_lowercase(),
                h["value"].as_str()?.to_owned(),
            ))
        })
        .collect();

    let body = extract_body(&msg["payload"]);

    // Extract attachments
    let mut attachments = Vec::new();
    fn find_attachments(part: &Value, out: &mut Vec<Value>) {
        if let Some(filename) = part["filename"].as_str() {
            if !filename.is_empty() {
                if let Some(att_id) = part["body"]["attachmentId"].as_str() {
                    out.push(json!({
                        "filename": filename,
                        "mimeType": part["mimeType"].as_str().unwrap_or(""),
                        "attachmentId": att_id
                    }));
                }
            }
        }
        if let Some(parts) = part["parts"].as_array() {
            for p in parts {
                find_attachments(p, out);
            }
        }
    }
    find_attachments(&msg["payload"], &mut attachments);

    Ok(json!({
        "id":        msg["id"],
        "threadId":  msg["threadId"],
        "labelIds":  msg["labelIds"],
        "snippet":   msg["snippet"],
        "subject":   headers.get("subject"),
        "from":      headers.get("from"),
        "to":        headers.get("to"),
        "date":      headers.get("date"),
        "messageId": headers.get("message-id"),
        "body":      body,
        "attachments": attachments,
    }))
}

pub async fn send(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut lines = vec![
        format!("To: {to}"),
        format!("Subject: {subject}"),
        "Content-Type: text/plain; charset=utf-8".to_owned(),
        "MIME-Version: 1.0".to_owned(),
    ];
    if let Some(cc) = cc {
        lines.push(format!("Cc: {cc}"));
    }
    if let Some(bcc) = bcc {
        lines.push(format!("Bcc: {bcc}"));
    }
    lines.push(String::new());
    lines.push(body.to_owned());

    let raw = URL_SAFE_NO_PAD.encode(lines.join("\r\n"));
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send_with_attachment(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    local_path: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let data = std::fs::read(local_path)?;
    let filename = std::path::Path::new(local_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let encoded_file = base64::engine::general_purpose::STANDARD.encode(data);

    let boundary = "axon_gmail_boundary";
    let mut header_lines = format!(
        "To: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\""
    );
    if let Some(cc) = cc {
        header_lines.push_str(&format!("\r\nCc: {cc}"));
    }
    if let Some(bcc) = bcc {
        header_lines.push_str(&format!("\r\nBcc: {bcc}"));
    }

    let mut raw_body = Vec::new();
    raw_body.extend_from_slice(
        format!(
            "{header_lines}\r\n\r\n\
--{boundary}\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
{body}\r\n\
--{boundary}\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename=\"{filename}\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
{encoded_file}\r\n\
--{boundary}--"
        )
        .as_bytes(),
    );

    let raw = URL_SAFE_NO_PAD.encode(raw_body);
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn reply(
    state: &AppState,
    thread_id: &str,
    message_id: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let lines = vec![
        format!("To: {to}"),
        format!(
            "Subject: {}",
            if subject.to_lowercase().starts_with("re:") {
                subject.to_owned()
            } else {
                format!("Re: {subject}")
            }
        ),
        format!("In-Reply-To: {message_id}"),
        format!("References: {message_id}"),
        "Content-Type: text/plain; charset=utf-8".to_owned(),
        "MIME-Version: 1.0".to_owned(),
        String::new(),
        body.to_owned(),
    ];
    let raw = URL_SAFE_NO_PAD.encode(lines.join("\r\n"));
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw, "threadId": thread_id }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn search(state: &AppState, query: &str, max_results: u32) -> Result<Value> {
    list(state, max_results, Some(query)).await
}

pub async fn trash(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/trash"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn mark_read(state: &AppState, ids: Vec<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/messages/batchModify"))
        .bearer_auth(&tok)
        .json(&json!({ "ids": ids, "removeLabelIds": ["UNREAD"] }))
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "count": ids.len() }))
}

pub async fn add_label(state: &AppState, id: &str, label_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/modify"))
        .bearer_auth(&tok)
        .json(&json!({ "addLabelIds": [label_id] }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn remove_label(state: &AppState, id: &str, label_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/modify"))
        .bearer_auth(&tok)
        .json(&json!({ "removeLabelIds": [label_id] }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn list_labels(state: &AppState) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/labels"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn mark_unread(state: &AppState, ids: Vec<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/messages/batchModify"))
        .bearer_auth(&tok)
        .json(&json!({ "ids": ids, "addLabelIds": ["UNREAD"] }))
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "count": ids.len() }))
}

pub async fn untrash(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/untrash"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Permanently delete a message. Unlike `trash`, this is irreversible.
pub async fn delete(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/messages/{id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "deletedId": id }))
}

/// Forward an email by fetching its body and re-sending to a new recipient.
pub async fn forward(
    state: &AppState,
    message_id: &str,
    to: &str,
    extra_note: Option<&str>,
) -> Result<Value> {
    // Fetch the original message so we can re-use its subject and body.
    let original = get(state, message_id).await?;
    let orig_subject = original["subject"].as_str().unwrap_or("(no subject)");
    let orig_body = original["body"].as_str().unwrap_or("");
    let orig_from = original["from"].as_str().unwrap_or("");
    let orig_date = original["date"].as_str().unwrap_or("");

    let fwd_subject = if orig_subject.to_lowercase().starts_with("fwd:") {
        orig_subject.to_owned()
    } else {
        format!("Fwd: {orig_subject}")
    };

    let note = extra_note.unwrap_or("");
    let fwd_body = format!(
        "{note}\r\n\r\n---------- Forwarded message ----------\r\nFrom: {orig_from}\r\nDate: {orig_date}\r\nSubject: {orig_subject}\r\n\r\n{orig_body}"
    );

    send(state, to, &fwd_subject, &fwd_body, None, None).await
}

// ── Drafts ────────────────────────────────────────────────────────────────────

fn build_raw_mime(
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> String {
    let mut lines = vec![
        format!("To: {to}"),
        format!("Subject: {subject}"),
        "Content-Type: text/plain; charset=utf-8".to_owned(),
        "MIME-Version: 1.0".to_owned(),
    ];
    if let Some(cc) = cc {
        lines.push(format!("Cc: {cc}"));
    }
    if let Some(bcc) = bcc {
        lines.push(format!("Bcc: {bcc}"));
    }
    lines.push(String::new());
    lines.push(body.to_owned());
    URL_SAFE_NO_PAD.encode(lines.join("\r\n"))
}

pub async fn create_draft(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let raw = build_raw_mime(to, subject, body, cc, bcc);
    let resp: Value = state
        .client
        .post(format!("{BASE}/drafts"))
        .bearer_auth(&tok)
        .json(&json!({ "message": { "raw": raw } }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn list_drafts(state: &AppState, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/drafts"))
        .bearer_auth(&tok)
        .query(&[("maxResults", max_results.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn get_draft(state: &AppState, draft_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/drafts/{draft_id}"))
        .bearer_auth(&tok)
        .query(&[("format", "full")])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Replace a draft's content entirely (PUT semantics).
pub async fn update_draft(
    state: &AppState,
    draft_id: &str,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let raw = build_raw_mime(to, subject, body, cc, bcc);
    let resp: Value = state
        .client
        .put(format!("{BASE}/drafts/{draft_id}"))
        .bearer_auth(&tok)
        .json(&json!({ "message": { "raw": raw } }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send_draft(state: &AppState, draft_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/drafts/send"))
        .bearer_auth(&tok)
        .json(&json!({ "id": draft_id }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn delete_draft(state: &AppState, draft_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/drafts/{draft_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "deletedDraftId": draft_id }))
}

// ── Internal body extractor ───────────────────────────────────────────────────

fn extract_body(payload: &Value) -> String {
    if let Some(data) = payload["body"]["data"].as_str() {
        // Gmail body data is URL-safe base64, but padding presence varies.
        // Try NO_PAD first; fall back to the padded variant so neither case silently fails.
        let bytes = URL_SAFE_NO_PAD
            .decode(data)
            .or_else(|_| URL_SAFE.decode(data));
        if let Ok(b) = bytes {
            if let Ok(text) = String::from_utf8(b) {
                return text;
            }
        }
    }
    if let Some(parts) = payload["parts"].as_array() {
        for part in parts {
            let mime = part["mimeType"].as_str().unwrap_or("");
            if mime == "text/plain" || mime == "text/html" {
                let body = extract_body(part);
                if !body.is_empty() {
                    return body;
                }
            }
        }
        for part in parts {
            let body = extract_body(part);
            if !body.is_empty() {
                return body;
            }
        }
    }
    String::new()
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
            "{BASE}/messages/{message_id}/attachments/{attachment_id}"
        ))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;

    let data = resp["data"].as_str().unwrap_or("");

    // Gmail returns attachments in URL-safe base64. Ensure it's padded.
    let data_padded = if data.len() % 4 != 0 {
        let pad_len = 4 - (data.len() % 4);
        format!("{}{}", data, "=".repeat(pad_len))
    } else {
        data.to_owned()
    };

    let bytes = match URL_SAFE.decode(&data_padded) {
        Ok(b) => b,
        Err(_) => URL_SAFE_NO_PAD.decode(data).unwrap_or_default(),
    };

    let download_dir = std::path::PathBuf::from("/data/files");
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(filename);
    std::fs::write(&path, &bytes)?;

    Ok(json!({
        "file_path": path.to_string_lossy(),
        "message": "Attachment downloaded successfully. You can now access it at this local path to upload/send to the user."
    }))
}
