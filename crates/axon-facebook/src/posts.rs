use crate::auth::{page_id, page_token};
use anyhow::Result;
use axon_core::{ensure_ok, AppState};
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

    let resp = state
        .client
        .get(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}

pub async fn get(state: &AppState, post_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .get(format!("{FB_API}/{post_id}"))
        .bearer_auth(&tok)
        .query(&[("fields", POST_FIELDS)])
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
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

    // Graph write endpoints expect form-encoded params, not a JSON body.
    let mut form: Vec<(&str, String)> = vec![("message", message.to_owned())];
    match publish_time {
        Some(ts) if ts > 0 => {
            form.push(("published", "false".to_owned()));
            form.push(("scheduled_publish_time", ts.to_string()));
        }
        _ => form.push(("published", "true".to_owned())),
    }
    if let Some(l) = link {
        form.push(("link", l.to_owned()));
    }

    let resp = state
        .client
        .post(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,permalink_url")])
        .form(&form)
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}

pub async fn create_with_image(state: &AppState, message: &str, image_url: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    // Step 1 — stage the photo without publishing
    let photo: Value = if image_url.starts_with("http://") || image_url.starts_with("https://") {
        let resp = state
            .client
            .post(format!("{FB_API}/{pid}/photos"))
            .bearer_auth(&tok)
            .form(&[
                ("url", image_url),
                ("published", "false"),
                ("temporary", "true"),
            ])
            .send()
            .await?;
        ensure_ok(resp).await?.json().await?
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

        let resp = state
            .client
            .post(format!("{FB_API}/{pid}/photos"))
            .bearer_auth(&tok)
            .multipart(form)
            .send()
            .await?;
        ensure_ok(resp).await?.json().await?
    };

    let photo_id = photo["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No photo id returned from staging step"))?;

    // Step 2 — create the post with the staged photo attached.
    // Graph API expects `attached_media` as indexed, form-encoded fields whose
    // values are JSON strings. Sending the array inside an `application/json`
    // body is a known source of opaque 500s on /{page}/feed, so we form-encode.
    let attached = serde_json::to_string(&json!({ "media_fbid": photo_id }))?;
    let resp = state
        .client
        .post(format!("{FB_API}/{pid}/feed"))
        .bearer_auth(&tok)
        .query(&[("fields", "id,permalink_url")])
        .form(&[
            ("message", message),
            ("attached_media[0]", attached.as_str()),
        ])
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}

pub async fn update(state: &AppState, post_id: &str, message: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .post(format!("{FB_API}/{post_id}"))
        .bearer_auth(&tok)
        .form(&[("message", message)])
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}

pub async fn delete(state: &AppState, post_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp = state
        .client
        .delete(format!("{FB_API}/{post_id}"))
        .bearer_auth(&tok)
        .send()
        .await?;
    ensure_ok(resp).await?;
    Ok(json!({ "success": true, "deleted_post_id": post_id }))
}

pub async fn get_scheduled(state: &AppState, limit: u32) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;
    let resp = state
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
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}
pub async fn create_with_video(state: &AppState, message: &str, video_url: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let resp_value: Value = if video_url.starts_with("http://") || video_url.starts_with("https://")
    {
        let resp = state
            .client
            .post(format!("{FB_API}/{pid}/videos"))
            .bearer_auth(&tok)
            .query(&[("fields", "id,permalink_url")])
            .form(&[("file_url", video_url), ("description", message)])
            .send()
            .await?;
        ensure_ok(resp).await?.json().await?
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

        let resp = state
            .client
            .post(format!("{FB_API}/{pid}/videos"))
            .bearer_auth(&tok)
            .query(&[("fields", "id,permalink_url")])
            .multipart(form)
            .send()
            .await?;
        ensure_ok(resp).await?.json().await?
    };

    Ok(resp_value)
}
