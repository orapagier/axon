use anyhow::Result;
use chrono::Utc;
use reqwest::Client;
use serde_json::Value;

use crate::ensure_ok;
use crate::storage::OAuthToken;

/// Exchange an authorization code for tokens at a standard token endpoint.
pub async fn exchange_code(
    client: &Client,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
    extra: &[(&str, &str)], // extra form fields (e.g. scope)
) -> Result<OAuthToken> {
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", redirect_uri),
    ];
    params.extend_from_slice(extra);

    let resp = client.post(token_url).form(&params).send().await?;
    let resp: Value = ensure_ok(resp).await?.json().await?;

    parse_token_response(resp)
}

/// Use a refresh token to obtain a new access token.
pub async fn refresh_token(
    client: &Client,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    extra: &[(&str, &str)],
) -> Result<OAuthToken> {
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ];
    params.extend_from_slice(extra);

    let resp = client.post(token_url).form(&params).send().await?;
    let resp: Value = ensure_ok(resp).await?.json().await?;

    // Microsoft refresh may not return a new refresh_token — keep the old one
    let new_refresh = resp["refresh_token"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| Some(refresh_token.to_owned()));

    let expires_in = resp["expires_in"].as_u64().unwrap_or(3600);
    let expires_at = Utc::now().timestamp_millis() + (expires_in as i64 * 1000);

    Ok(OAuthToken {
        access_token: resp["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no access_token in refresh response"))?
            .to_owned(),
        refresh_token: new_refresh,
        expires_at,
    })
}

/// Parse a token endpoint response into an `OAuthToken`.
fn parse_token_response(resp: Value) -> Result<OAuthToken> {
    let access_token = resp["access_token"]
        .as_str()
        .ok_or_else(|| {
            let desc = resp["error_description"]
                .as_str()
                .or_else(|| resp["error"].as_str())
                .unwrap_or("unknown error");
            anyhow::anyhow!("token exchange failed: {desc}")
        })?
        .to_owned();

    let refresh_token = resp["refresh_token"].as_str().map(str::to_owned);
    let expires_in = resp["expires_in"].as_u64().unwrap_or(3600);
    let expires_at = Utc::now().timestamp_millis() + (expires_in as i64 * 1000);

    Ok(OAuthToken {
        access_token,
        refresh_token,
        expires_at,
    })
}

/// Standard OAuth2 callback port used by all services.
pub const CALLBACK_PORT: u16 = 8080;

pub fn callback_uri(service: &str) -> String {
    let host = std::env::var("AXON_CALLBACK_HOST")
        .or_else(|_| std::env::var("AXON_PUBLIC_BASE_URL"))
        .or_else(|_| std::env::var("instagram.public_base_url"))
        .unwrap_or_else(|_| format!("http://localhost:{CALLBACK_PORT}"));

    // If host doesn't start with http, prepend it (assuming http if no scheme)
    let base = if host.starts_with("http") {
        host
    } else {
        format!("http://{}", host)
    };

    format!("{}/auth/{}/callback", base.trim_end_matches('/'), service)
}
