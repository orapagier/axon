use crate::auth::{page_id, page_token};
use anyhow::Result;
use axon_core::{ensure_ok, AppState};
use serde_json::{json, Value};

const FB_API: &str = "https://graph.facebook.com/v25.0";

pub async fn list(state: &AppState, object_id: &str, limit: u32) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .get(format!("{FB_API}/{object_id}/comments"))
        .bearer_auth(&tok)
        .query(&[
            ("limit", limit.to_string()),
            ("order", "reverse_chronological".to_owned()),
            ("summary", "true".to_owned()),
            (
                "fields",
                "id,message,from{id,name},created_time,like_count,can_reply_privately,attachment"
                    .to_owned(),
            ),
        ])
        .send()
        .await?;
    let resp: Value = ensure_ok(resp).await?.json().await?;

    // Format response to make commenter names easily accessible
    let comments = resp.get("data").and_then(|d| d.as_array());
    if let Some(comments) = comments {
        let formatted: Vec<Value> = comments
            .iter()
            .map(|c| {
                let commenter_name = c
                    .get("from")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("Unknown");
                let commenter_id = c
                    .get("from")
                    .and_then(|f| f.get("id"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                json!({
                    "id": c.get("id"),
                    "commenter_name": commenter_name,
                    "commenter_id": commenter_id,
                    "message": c.get("message").and_then(|m| m.as_str()).unwrap_or(""),
                    "created_time": c.get("created_time"),
                    "like_count": c.get("like_count").and_then(|l| l.as_i64()).unwrap_or(0),
                    "has_attachment": c.get("attachment").is_some(),
                    "can_reply_privately": c.get("can_reply_privately"),
                })
            })
            .collect();
        let total = resp
            .get("summary")
            .and_then(|s| s.get("total_count"))
            .and_then(|t| t.as_i64())
            .unwrap_or(formatted.len() as i64);
        Ok(json!({
            "comments": formatted,
            "total_count": total,
            "showing": formatted.len(),
        }))
    } else {
        Ok(resp)
    }
}

pub async fn reply(state: &AppState, comment_id: &str, message: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .post(format!("{FB_API}/{comment_id}/comments"))
        .bearer_auth(&tok)
        .form(&[("message", message)])
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}

pub async fn delete(state: &AppState, comment_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .delete(format!("{FB_API}/{comment_id}"))
        .bearer_auth(&tok)
        .send()
        .await?;
    ensure_ok(resp).await?;
    Ok(json!({ "success": true, "deleted_comment_id": comment_id }))
}

pub async fn set_hidden(state: &AppState, comment_id: &str, hide: bool) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .post(format!("{FB_API}/{comment_id}"))
        .bearer_auth(&tok)
        .form(&[("is_hidden", if hide { "true" } else { "false" })])
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}

pub async fn like(state: &AppState, object_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .post(format!("{FB_API}/{object_id}/likes"))
        .bearer_auth(&tok)
        .send()
        .await?;

    // Like endpoints return plain text ("true") or empty bodies on success,
    // so we must read the body once and then interpret it.
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{status} from {FB_API}/{object_id}/likes: {}", text.trim());
    }
    if text.is_empty() || text.trim() == "true" {
        return Ok(json!({"success": true}));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => Ok(v),
        Err(_) => Ok(json!({"response": text})),
    }
}

pub async fn react(
    state: &AppState,
    object_id: &str,
    reaction_type: Option<&str>,
) -> Result<Value> {
    let tok = page_token(state).await?;
    let r_type = reaction_type.map(|s| s.to_uppercase());

    if let Some(r) = &r_type {
        if r == "ANGRY" {
            return Err(anyhow::anyhow!("ANGRY reaction is not allowed (guardrail)"));
        }
    }

    let is_like = r_type.as_deref() == Some("LIKE") || r_type.is_none();

    let url = if is_like {
        format!("{}/{}/likes", FB_API, object_id)
    } else {
        format!("{}/{}/reactions", FB_API, object_id)
    };

    let mut req = state.client.post(url.clone()).bearer_auth(&tok);
    if !is_like {
        if let Some(r) = r_type {
            req = req.query(&[("type", r)]);
        }
    }

    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{status} from {url}: {}", text.trim());
    }
    if text.is_empty() || text.trim() == "true" {
        return Ok(json!({"success": true}));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => Ok(v),
        Err(_) => Ok(json!({"response": text})),
    }
}

pub async fn get(state: &AppState, comment_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .get(format!("{FB_API}/{comment_id}"))
        .bearer_auth(&tok)
        .query(&[(
            "fields",
            "id,message,from{id,name},created_time,like_count,can_reply_privately,attachment",
        )])
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}

pub async fn unreact(
    state: &AppState,
    object_id: &str,
    reaction_type: Option<&str>,
) -> Result<Value> {
    let tok = page_token(state).await?;

    let url = if reaction_type.is_some() {
        format!("{}/{}/reactions", FB_API, object_id)
    } else {
        format!("{}/{}/likes", FB_API, object_id)
    };

    let resp = state.client.delete(url).bearer_auth(&tok).send().await?;
    ensure_ok(resp).await?;
    Ok(json!({ "success": true }))
}

/// Fetch recent comments across the latest posts — no object_id needed.
/// This enables "check new facebook comments" without the user specifying a post.
pub async fn recent_comments(
    state: &AppState,
    post_count: u32,
    comment_limit: u32,
) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    // Step 1: Get recent posts
    let resp = state
        .client
        .get(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&[
            ("fields", "id,message,created_time".to_owned()),
            ("limit", post_count.to_string()),
        ])
        .send()
        .await?;
    let posts_resp: Value = ensure_ok(resp).await?.json().await?;

    let posts = posts_resp.get("data").and_then(|d| d.as_array());
    let mut results = Vec::new();
    let mut total_comments = 0u64;

    if let Some(posts) = posts {
        // Step 2: Get comments for each post
        for post in posts {
            let post_id = match post.get("id").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => continue,
            };
            let post_msg = post
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("(no text)");
            let post_time = post
                .get("created_time")
                .and_then(|t| t.as_str())
                .unwrap_or("");

            // Fetch comments for this post
            let comments_result = list(state, post_id, comment_limit).await;
            match comments_result {
                Ok(comment_data) => {
                    let count = comment_data
                        .get("showing")
                        .and_then(|s| s.as_u64())
                        .unwrap_or(0);
                    if count > 0 {
                        total_comments += count;
                        results.push(json!({
                            "post_id": post_id,
                            "post_message": &post_msg[..post_msg.len().min(100)],
                            "post_time": post_time,
                            "comments": comment_data.get("comments"),
                            "comment_count": count,
                        }));
                    }
                }
                Err(_) => continue,
            }
        }
    }

    Ok(json!({
        "posts_checked": posts.map(|p| p.len()).unwrap_or(0),
        "total_comments": total_comments,
        "posts_with_comments": results,
    }))
}
