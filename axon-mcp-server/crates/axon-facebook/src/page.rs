use crate::auth::{page_id, page_token};
use anyhow::Result;
use axon_core::AppState;
use serde_json::{json, Map, Value};

const FB_API: &str = "https://graph.facebook.com/v25.0";

pub async fn get_page(state: &AppState) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let page_info: Value = state
        .client
        .get(format!("{FB_API}/{pid}"))
        .bearer_auth(&tok)
        .query(&[(
            "fields",
            "id,name,about,category,fan_count,followers_count,\
             website,phone,emails,hours,location,\
             rating_count,overall_star_rating,verification_status,\
             description,cover,picture",
        )])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // Fetch insights (reach, follows, unfollows)
    let insights: Value = match state
        .client
        .get(format!("{FB_API}/{pid}/insights"))
        .bearer_auth(&tok)
        .query(&[
            (
                "metric",
                "page_views,page_post_engagements,page_daily_follows,page_daily_unfollows",
            ),
            ("period", "day"),
        ])
        .send()
        .await
    {
        Ok(resp) => match resp.error_for_status() {
            Ok(r) => r.json().await.unwrap_or(json!(null)),
            Err(_) => json!(null),
        },
        Err(_) => json!(null),
    };

    Ok(json!({
        "page_info": page_info,
        "insights": insights
    }))
}

pub async fn update_page(state: &AppState, args: &Map<String, Value>) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let mut body = serde_json::Map::new();
    for key in ["about", "description", "phone", "website"] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            body.insert(key.to_owned(), Value::String(v.to_owned()));
        }
    }
    if body.is_empty() {
        return Ok(
            serde_json::json!({ "success": false, "message": "No fields provided to update." }),
        );
    }

    let resp: Value = state
        .client
        .post(format!("{FB_API}/{pid}"))
        .bearer_auth(&tok)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}
