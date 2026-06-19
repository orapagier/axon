use crate::auth::{page_id, page_token};
use anyhow::Result;
use axon_core::AppState;
use serde_json::{json, Value};

const FB_API: &str = "https://graph.facebook.com/v25.0";

pub async fn list_conversations(state: &AppState, limit: u32, unread_only: bool) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;
    let mut resp: Value = state
        .client
        .get(format!("{FB_API}/{pid}/conversations"))
        .bearer_auth(&tok)
        .query(&[
            (
                "fields",
                "id,participants,updated_time,message_count,unread_count,snippet",
            ),
            ("limit", &limit.to_string()),
            ("platform", "messenger"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if unread_only {
        if let Some(arr) = resp.get_mut("data").and_then(|d| d.as_array_mut()) {
            arr.retain(|v| v.get("unread_count").and_then(|u| u.as_u64()).unwrap_or(0) > 0);
        }
    }
    Ok(resp)
}

pub async fn get_conversation(
    state: &AppState,
    conversation_id: &str,
    limit: u32,
) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{FB_API}/{conversation_id}/messages"))
        .bearer_auth(&tok)
        .query(&[
            ("fields", "id,message,from,to,created_time,attachments"),
            ("limit", &limit.to_string()),
            ("order", "reverse_chronological"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send_text(state: &AppState, recipient_id: &str, message: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;
    let resp: Value = state
        .client
        .post(format!("{FB_API}/{pid}/messages"))
        .bearer_auth(&tok)
        .json(&json!({
            "recipient": { "id": recipient_id },
            "message":   { "text": message },
            "messaging_type": "RESPONSE"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send_image(state: &AppState, recipient_id: &str, image_url: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;
    let resp: Value = state
        .client
        .post(format!("{FB_API}/{pid}/messages"))
        .bearer_auth(&tok)
        .json(&json!({
            "recipient": { "id": recipient_id },
            "message": {
                "attachment": {
                    "type": "image",
                    "payload": { "url": image_url, "is_reusable": true },
                },
            },
            "messaging_type": "RESPONSE"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}
