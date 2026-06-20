use crate::auth::{page_id, page_token};
use anyhow::Result;
use axon_core::{ensure_ok, AppState};
use serde_json::{json, Map, Value};

const FB_API: &str = "https://graph.facebook.com/v25.0";

pub async fn get_page(state: &AppState) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let resp = state
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
        .await?;
    let page_info: Value = ensure_ok(resp).await?.json().await?;

    // Fetch insights (reach, follows, unfollows). These are best-effort: some
    // pages lack the metrics, and we don't want a secondary failure to mask the
    // primary page info we already have.
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
        Ok(resp) => match ensure_ok(resp).await {
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

    let mut form: Vec<(&str, String)> = Vec::new();
    for key in ["about", "description", "phone", "website"] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            form.push((key, v.to_owned()));
        }
    }
    if form.is_empty() {
        return Ok(
            serde_json::json!({ "success": false, "message": "No fields provided to update." }),
        );
    }

    let resp = state
        .client
        .post(format!("{FB_API}/{pid}"))
        .bearer_auth(&tok)
        .form(&form)
        .send()
        .await?;
    Ok(ensure_ok(resp).await?.json().await?)
}
