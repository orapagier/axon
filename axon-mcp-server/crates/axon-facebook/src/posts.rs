use crate::auth::{page_id, page_token};
use anyhow::Result;
use axon_core::AppState;
use serde_json::{json, Value};

const FB_API: &str = "https://graph.facebook.com/v25.0";

const POST_FIELDS: &str = "id,message,story,created_time,full_picture,permalink_url,\
     likes.summary(true),comments.summary(true),shares";

pub async fn list(state: &AppState, limit: u32, after: Option<&str>) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let mut params = vec![
        ("fields".to_owned(), POST_FIELDS.to_owned()),
        ("limit".to_owned(), limit.to_string()),
    ];
    if let Some(cursor) = after {
        params.push(("after".to_owned(), cursor.to_owned()));
    }

    let resp: Value = state
        .client
        .get(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn get(state: &AppState, post_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{FB_API}/{post_id}"))
        .bearer_auth(&tok)
        .query(&[("fields", POST_FIELDS)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Create a post. If `publish_time` is Some, it will be scheduled (published=false).
pub async fn create(
    state: &AppState,
    message: &str,
    link: Option<&str>,
    publish_time: Option<u64>,
) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let mut body = json!({ "message": message });

    match publish_time {
        Some(ts) if ts > 0 => {
            body["published"] = json!(false);
            body["scheduled_publish_time"] = json!(ts);
        }
        _ => {
            body["published"] = json!(true);
        }
    }
    if let Some(l) = link {
        body["link"] = json!(l);
    }

    let resp: Value = state
        .client
        .post(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,permalink_url")])
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn create_with_image(state: &AppState, message: &str, image_url: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    // Step 1 — stage the photo without publishing
    let photo: Value = if image_url.starts_with("http://") || image_url.starts_with("https://") {
        state
            .client
            .post(format!("{FB_API}/{pid}/photos"))
            .bearer_auth(&tok)
            .json(&json!({ "url": image_url, "published": false, "temporary": true }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?
    } else {
        // Local file upload
        let content = std::fs::read(image_url)
            .map_err(|e| anyhow::anyhow!("Failed to read local file {}: {}", image_url, e))?;
        let mime = mime_guess::from_path(image_url).first_or_octet_stream();
        let fname = std::path::Path::new(image_url)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.jpg")
            .to_string();

        let part = reqwest::multipart::Part::bytes(content)
            .file_name(fname)
            .mime_str(mime.as_ref())?;

        let form = reqwest::multipart::Form::new()
            .text("published", "false")
            .text("temporary", "true")
            .part("source", part);

        state
            .client
            .post(format!("{FB_API}/{pid}/photos"))
            .bearer_auth(&tok)
            .multipart(form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?
    };

    let photo_id = photo["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No photo id returned from staging step"))?;

    // Step 2 — create the post with the staged photo attached
    let resp: Value = state
        .client
        .post(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,permalink_url")])
        .json(&json!({
            "message":        message,
            "attached_media": [{ "media_fbid": photo_id }],
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn update(state: &AppState, post_id: &str, message: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{FB_API}/{post_id}"))
        .bearer_auth(&tok)
        .json(&json!({ "message": message }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn delete(state: &AppState, post_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    state
        .client
        .delete(format!("{FB_API}/{post_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true, "deleted_post_id": post_id }))
}

pub async fn get_scheduled(state: &AppState, limit: u32) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;
    let resp: Value = state
        .client
        .get(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&[
            ("limit", limit.to_string()),
            ("is_published", "false".to_owned()),
            (
                "fields",
                "id,message,scheduled_publish_time,permalink_url".to_owned(),
            ),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}
pub async fn create_with_video(state: &AppState, message: &str, video_url: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let resp: Value = if video_url.starts_with("http://") || video_url.starts_with("https://") {
        state
            .client
            .post(format!("{FB_API}/{pid}/videos"))
            .bearer_auth(&tok)
            .query(&[("fields", "id,permalink_url")])
            .json(&json!({ "file_url": video_url, "description": message }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?
    } else {
        // Local file upload
        let content = std::fs::read(video_url)
            .map_err(|e| anyhow::anyhow!("Failed to read local file {}: {}", video_url, e))?;
        let mime = mime_guess::from_path(video_url).first_or_octet_stream();
        let fname = std::path::Path::new(video_url)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video.mp4")
            .to_string();

        let part = reqwest::multipart::Part::bytes(content)
            .file_name(fname)
            .mime_str(mime.as_ref())?;

        let form = reqwest::multipart::Form::new()
            .text("description", message.to_string())
            .part("source", part);

        state
            .client
            .post(format!("{FB_API}/{pid}/videos"))
            .bearer_auth(&tok)
            .query(&[("fields", "id,permalink_url")])
            .multipart(form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?
    };

    Ok(resp)
}
