use anyhow::Result;
use axon_core::storage::FacebookToken;
use axon_core::{ensure_ok, oauth, AppState};
use serde_json::{json, Value};

pub const FB_API: &str = "https://graph.facebook.com/v25.0";

const FB_SCOPES: &[&str] = &[
    "pages_manage_posts",
    "pages_read_engagement",
    "pages_read_user_content",
    "pages_messaging",
    "pages_manage_metadata",
    "pages_manage_engagement",
    "pages_show_list",
    "read_insights",
    "whatsapp_business_management",
    "whatsapp_business_messaging",
];

const IG_SCOPES: &[&str] = &[
    "instagram_business_basic",
    "instagram_business_content_publish",
    "instagram_business_manage_comments",
    "instagram_business_manage_insights",
    "instagram_business_manage_messages",
    "pages_show_list", // Required to see linked pages
    "pages_read_engagement",
];

pub async fn auth_url(state: &AppState) -> Result<Value> {
    let storage = state.storage.read().await;
    let creds = storage.facebook_creds()?;
    let scope = FB_SCOPES.join(",");
    let redir = oauth::callback_uri("facebook");

    let url = format!(
        "https://www.facebook.com/v25.0/dialog/oauth?\
         client_id={}&redirect_uri={}&scope={}&response_type=code",
        urlenc(&creds.app_id),
        urlenc(&redir),
        urlenc(&scope),
    );
    Ok(json!({
        "url": url,
        "instructions": format!("Open in browser, log in, then the token is saved automatically via: {redir}")
    }))
}

pub async fn instagram_auth_url(state: &AppState) -> Result<Value> {
    let storage = state.storage.read().await;
    let creds = storage.facebook_creds()?;
    let scope = IG_SCOPES.join(",");
    // Reuse the facebook callback URI — only one redirect URI needs to be
    // registered in the Facebook App Dashboard. The IG-specific scopes in
    // the URL are what request Instagram permissions.
    let redir = oauth::callback_uri("facebook");

    let url = format!(
        "https://www.facebook.com/v25.0/dialog/oauth?\
         client_id={}&redirect_uri={}&scope={}&response_type=code",
        urlenc(&creds.app_id),
        urlenc(&redir),
        urlenc(&scope),
    );
    Ok(json!({
        "url": url,
        "instructions": format!(
            "Open in browser to grant Instagram permissions. \
             IMPORTANT: Your App must have the 'Instagram Graph API' product added. \
             In the new Dashboard, select 'Other' -> 'Business' or 'Manage professional presence'. \
             Make sure {redir} is registered in your App Dashboard."
        )
    }))
}

pub async fn exchange_code(state: &AppState, code: &str, _service: Option<&str>) -> Result<Value> {
    let (app_id, app_secret) = {
        let s = state.storage.read().await;
        let c = s.facebook_creds()?;
        (c.app_id.clone(), c.app_secret.clone())
    };
    let page_id = {
        let s = state.storage.read().await;
        s.facebook_creds()?.page_id.clone()
    };
    // Use "facebook" as the consistent service for the callback URI.
    // This ensures it matches the URI registered in the Facebook App Dashboard
    // and the one used in auth_url/instagram_auth_url.
    let redir = oauth::callback_uri("facebook");

    // Step 1 — short-lived user token
    let resp = state
        .client
        .get(format!("{FB_API}/oauth/access_token"))
        .query(&[
            ("client_id", &app_id),
            ("client_secret", &app_secret),
            ("redirect_uri", &redir),
            ("code", &code.to_owned()),
        ])
        .send()
        .await?;
    let short: Value = ensure_ok(resp).await?.json().await?;

    let short_token = short["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No access_token in short-lived response: {short}"))?;

    // Step 2 — long-lived user token (~60 days)
    let resp = state
        .client
        .get(format!("{FB_API}/oauth/access_token"))
        .query(&[
            ("grant_type", "fb_exchange_token"),
            ("client_id", &app_id),
            ("client_secret", &app_secret),
            ("fb_exchange_token", short_token),
        ])
        .send()
        .await?;
    let long: Value = ensure_ok(resp).await?.json().await?;

    let long_token = long["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No access_token in long-lived response: {long}"))?;

    // Step 3 — permanent page token
    let resp = state
        .client
        .get(format!("{FB_API}/{page_id}"))
        .query(&[
            ("fields", "access_token,instagram_business_account"),
            ("access_token", long_token),
        ])
        .send()
        .await?;
    let page_data: Value = ensure_ok(resp).await?.json().await?;

    let page_token = page_data["access_token"].as_str()
        .ok_or_else(|| anyhow::anyhow!("No page access_token. Make sure the page_id is correct and you're an admin of the page."))?;

    let ig_id = page_data["instagram_business_account"]["id"]
        .as_str()
        .map(|s| s.to_owned());
    let ig_connected = ig_id.is_some();

    state
        .storage
        .write()
        .await
        .set_facebook_token(FacebookToken {
            page_access_token: page_token.to_owned(),
            user_access_token: Some(long_token.to_owned()),
            instagram_business_account_id: ig_id,
        })?;

    Ok(json!({
        "success": true,
        "message": "Facebook Page authenticated!",
        "instagram_connected": ig_connected
    }))
}

pub async fn auth_status(state: &AppState) -> Result<Value> {
    let _ = state.storage.write().await.reload_tokens();
    let s = state.storage.read().await;
    match &s.tokens.facebook {
        None => {
            // Fall back to static token in credentials
            let creds = s.facebook_creds().ok();
            if creds.map_or(false, |c| !c.page_access_token.is_empty()) {
                Ok(json!({ "authenticated": true, "source": "credentials.json (static token)" }))
            } else {
                Ok(json!({ "authenticated": false }))
            }
        }
        Some(t) => Ok(json!({
            "authenticated": true,
            "source": "tokens.json",
            "token_preview": format!("{}...", &t.page_access_token[..20.min(t.page_access_token.len())]),
            "instagram_connected": t.instagram_business_account_id.is_some(),
        })),
    }
}

pub async fn debug_token(state: &AppState) -> Result<Value> {
    let tok = page_token(state).await?;
    let creds = {
        let s = state.storage.read().await;
        let c = s.facebook_creds()?;
        (c.app_id.clone(), c.app_secret.clone())
    };
    let resp = state
        .client
        .get(format!("{FB_API}/debug_token"))
        .query(&[
            ("input_token", tok.as_str()),
            ("access_token", &format!("{}|{}", creds.0, creds.1)),
        ])
        .send()
        .await?;
    let resp: Value = ensure_ok(resp).await?.json().await?;
    Ok(resp)
}

/// Resolve a valid page access token (from tokens store or credentials file).
pub async fn page_token(state: &AppState) -> Result<String> {
    let _ = state.storage.write().await.reload_tokens();
    let s = state.storage.read().await;
    if let Some(t) = &s.tokens.facebook {
        return Ok(t.page_access_token.clone());
    }
    let creds = s.facebook_creds()?;
    if !creds.page_access_token.is_empty() {
        return Ok(creds.page_access_token.clone());
    }
    Err(anyhow::anyhow!(
        "Facebook not authenticated. Call facebook_auth_url → open URL → facebook_exchange_code."
    ))
}

/// Return the configured Facebook Page ID.
pub async fn page_id(state: &AppState) -> Result<String> {
    let s = state.storage.read().await;
    Ok(s.facebook_creds()?.page_id.clone())
}

/// Return the configured Instagram Business Account ID.
pub async fn instagram_id(state: &AppState) -> Result<String> {
    let _ = state.storage.write().await.reload_tokens();
    let s = state.storage.read().await;
    if let Some(t) = &s.tokens.facebook {
        if let Some(id) = &t.instagram_business_account_id {
            return Ok(id.clone());
        }
    }
    if let Some(id) = s.facebook_creds()?.instagram_id.as_ref() {
        return Ok(id.clone());
    }
    Err(anyhow::anyhow!(
        "Instagram not connected. Make sure your Instagram Business Account is linked to your Facebook Page and you've re-authenticated to grant Instagram permissions."
    ))
}

pub async fn revoke(state: &AppState) -> Result<Value> {
    // Clear tokens from storage
    {
        let mut storage = state.storage.write().await;
        storage.tokens.facebook = None;
        storage.save_tokens()?;
        // Also clear any static token from credentials.json to prevent fallback
        storage.clear_facebook_creds_token()?;
    }

    Ok(
        json!({ "success": true, "message": "Facebook tokens successfully revoked and deleted from storage." }),
    )
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
