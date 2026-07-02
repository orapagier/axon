use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};

const BASE: &str = "https://chat.googleapis.com/v1";

// ── Spaces ────────────────────────────────────────────────────────────────────

/// List all Chat spaces the authenticated user is a member of.
pub async fn list_spaces(state: &AppState, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/spaces"))
        .bearer_auth(&tok)
        .query(&[("pageSize", max_results.to_string())])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Get details of a specific space.
/// `space_name` format: "spaces/XXXXXXXXX"
pub async fn get_space(state: &AppState, space_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{space_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// List members of a space.
pub async fn list_members(state: &AppState, space_name: &str, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{space_name}/members"))
        .bearer_auth(&tok)
        .query(&[("pageSize", max_results.to_string())])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Messages ──────────────────────────────────────────────────────────────────

/// List messages in a space, newest first.
pub async fn list_messages(
    state: &AppState,
    space_name: &str,
    max_results: u32,
    filter: Option<&str>, // e.g. "createTime > \"2024-01-01T00:00:00Z\""
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params = vec![
        ("pageSize", max_results.to_string()),
        ("orderBy", "createTime desc".to_owned()),
    ];
    if let Some(f) = filter {
        params.push(("filter", f.to_owned()));
    }

    let resp: Value = state
        .client
        .get(format!("{BASE}/{space_name}/messages"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a single message by resource name.
/// `message_name` format: "spaces/SPACE_ID/messages/MESSAGE_ID"
pub async fn get_message(state: &AppState, message_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{message_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Send a plain-text message to a space.
pub async fn send_message(
    state: &AppState,
    space_name: &str,
    text: &str,
    thread_key: Option<&str>, // set to reply within a thread
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut body = json!({ "text": text });

    // Threading: if thread_key is provided, the message will be posted in that thread.
    let mut params: Vec<(&str, String)> = vec![];
    if let Some(tk) = thread_key {
        body["thread"] = json!({ "threadKey": tk });
        params.push((
            "messageReplyOption",
            "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD".to_owned(),
        ));
    }

    let resp: Value = state
        .client
        .post(format!("{BASE}/{space_name}/messages"))
        .bearer_auth(&tok)
        .query(&params)
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Send a message with a card (structured rich content).
/// `card` should be a valid Google Chat card JSON object.
/// See: https://developers.google.com/chat/api/guides/v2/cards/overview
pub async fn send_card_message(
    state: &AppState,
    space_name: &str,
    text: Option<&str>,
    card: Value,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut body = json!({ "cardsV2": [{ "card": card }] });
    if let Some(t) = text {
        body["text"] = json!(t);
    }

    let resp: Value = state
        .client
        .post(format!("{BASE}/{space_name}/messages"))
        .bearer_auth(&tok)
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Update the text of an existing message.
pub async fn update_message(state: &AppState, message_name: &str, new_text: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .patch(format!("{BASE}/{message_name}"))
        .bearer_auth(&tok)
        .query(&[("updateMask", "text")])
        .json(&json!({ "text": new_text }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Delete a message.
pub async fn delete_message(state: &AppState, message_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/{message_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "deletedMessage": message_name }))
}

// ── Reactions ─────────────────────────────────────────────────────────────────

/// Add an emoji reaction to a message.
/// `emoji` is a unicode emoji string (e.g. "👍") or a custom emoji resource name.
pub async fn add_reaction(state: &AppState, message_name: &str, emoji: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/{message_name}/reactions"))
        .bearer_auth(&tok)
        .json(&json!({ "emoji": { "unicode": emoji } }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// List reactions on a message.
pub async fn list_reactions(state: &AppState, message_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{message_name}/reactions"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Remove a reaction from a message.
/// `reaction_name` format: "spaces/SPACE/messages/MESSAGE/reactions/REACTION"
pub async fn delete_reaction(state: &AppState, reaction_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/{reaction_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "deletedReaction": reaction_name }))
}
