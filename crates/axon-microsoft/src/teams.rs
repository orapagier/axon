use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};

const BASE: &str = "https://graph.microsoft.com/v1.0";

pub async fn list_joined(state: &AppState) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/joinedTeams"))
        .bearer_auth(&tok)
        .query(&[("$select", "id,displayName,description,isArchived")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn list_channels(state: &AppState, team_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/teams/{team_id}/channels"))
        .bearer_auth(&tok)
        .query(&[("$select", "id,displayName,description,membershipType")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send_message(
    state: &AppState,
    team_id: &str,
    channel_id: &str,
    content: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!(
            "{BASE}/teams/{team_id}/channels/{channel_id}/messages"
        ))
        .bearer_auth(&tok)
        .json(&json!({"body":{"contentType":"text","content":content}}))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn list_chats(state: &AppState, max_count: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/chats"))
        .bearer_auth(&tok)
        .query(&[
            ("$top", max_count.to_string()),
            ("$expand", "members".to_owned()),
            (
                "$select",
                "id,chatType,topic,lastUpdatedDateTime".to_owned(),
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

pub async fn send_chat_message(state: &AppState, chat_id: &str, content: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/me/chats/{chat_id}/messages"))
        .bearer_auth(&tok)
        .json(&json!({"body":{"contentType":"text","content":content}}))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}
