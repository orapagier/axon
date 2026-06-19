use crate::auth::{page_id, page_token};
use anyhow::Result;
use axon_core::AppState;
use serde_json::Value;

const FB_API: &str = "https://graph.facebook.com/v25.0";

pub async fn page_insights(
    state: &AppState,
    metric: &str,
    period: &str,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Value> {
    let tok = page_token(state).await?;
    let pid = page_id(state).await?;

    let mut params = vec![
        ("metric".to_owned(), metric.to_owned()),
        ("period".to_owned(), period.to_owned()),
    ];
    if let Some(s) = since {
        params.push(("since".to_owned(), s.to_owned()));
    }
    if let Some(u) = until {
        params.push(("until".to_owned(), u.to_owned()));
    }

    let resp: Value = state
        .client
        .get(format!("{FB_API}/{pid}/insights"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn post_insights(state: &AppState, post_id: &str) -> Result<Value> {
    let tok = page_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{FB_API}/{post_id}/insights"))
        .bearer_auth(&tok)
        .query(&[(
            "metric",
            "post_impressions,post_impressions_unique,\
             post_engaged_users,post_clicks,\
             post_reactions_by_type_total",
        )])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}
