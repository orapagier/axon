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

/// OAuth URL for the "Connect a Page as a credential" flow (the Facebook node's
/// Connect button). Identical to `auth_url` but carries `state=fbcred` so the
/// callback saves every Page the user manages as its own credential instead of
/// overwriting the global Page token.
pub async fn connect_url(state: &AppState) -> Result<Value> {
    let storage = state.storage.read().await;
    let creds = storage.facebook_creds()?;
    let scope = FB_SCOPES.join(",");
    let redir = oauth::callback_uri("facebook");

    let url = format!(
        "https://www.facebook.com/v25.0/dialog/oauth?\
         client_id={}&redirect_uri={}&scope={}&response_type=code&state=fbcred",
        urlenc(&creds.app_id),
        urlenc(&redir),
        urlenc(&scope),
    );
    Ok(json!({
        "url": url,
        "instructions": format!("Open in browser, log in, then each Page you manage is saved as a credential via: {redir}")
    }))
}

/// Exchange an OAuth `code` for the list of Pages the authenticated user
/// manages, each with its own permanent Page access token. Used by the
/// "Connect a Page" flow to create one credential per Page.
///
/// Returns `{ pages: [ { page_id, page_name, page_access_token, instagram_id } ] }`.
pub async fn exchange_code_pages(state: &AppState, code: &str) -> Result<Value> {
    let (app_id, app_secret) = {
        let s = state.storage.read().await;
        let c = s.facebook_creds()?;
        (c.app_id.clone(), c.app_secret.clone())
    };
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

    // Step 3 — list every Page the user manages, each with a permanent page token
    let resp = state
        .client
        .get(format!("{FB_API}/me/accounts"))
        .query(&[
            ("fields", "id,name,access_token,instagram_business_account"),
            ("limit", "100"),
            ("access_token", long_token),
        ])
        .send()
        .await?;
    let accounts: Value = ensure_ok(resp).await?.json().await?;

    let raw_pages = accounts
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();

    // Build the credential list AND subscribe every Page to this App's webhooks.
    // OAuth alone only yields tokens — Meta does not deliver a Page's events
    // until the Page is subscribed via `POST /{page-id}/subscribed_apps`. Doing
    // it here for *every* managed Page is what makes "whichever Page I pick in
    // the dropdown" actually receive webhook events, not just the one Page that
    // happened to be subscribed by hand in the App Dashboard.
    let mut pages: Vec<Value> = Vec::new();
    for p in &raw_pages {
        let Some(page_id) = p.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(token) = p.get("access_token").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or(page_id);
        let ig_id = p
            .get("instagram_business_account")
            .and_then(|i| i.get("id"))
            .and_then(|v| v.as_str());

        let subscribed = match subscribe_page(state, page_id, token).await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    "FB connect: failed to subscribe page '{name}' ({page_id}) to webhooks: {e}"
                );
                false
            }
        };

        pages.push(json!({
            "page_id": page_id,
            "page_name": name,
            "page_access_token": token,
            "instagram_id": ig_id,
            "webhooks_subscribed": subscribed,
        }));
    }

    if pages.is_empty() {
        return Err(anyhow::anyhow!(
            "No Pages found for this account. Make sure you granted the pages_show_list \
             permission and that you are an admin of at least one Page."
        ));
    }

    Ok(json!({ "pages": pages }))
}

/// Page webhook fields to activate on subscription. These mirror exactly what
/// the webhook handler (`webhook::facebook`) knows how to process: `feed`
/// (posts/comments/reactions/likes/shares), `mention`, `ratings`, and the
/// Messenger events (`messages`, `messaging_postbacks`, `message_reactions`).
/// Passive receipts (`message_deliveries`, `message_reads`) are intentionally
/// omitted — they're high-volume and the dispatcher skips them by default; add
/// them here if a workflow ever needs to trigger on them.
const FB_WEBHOOK_FIELDS: &str =
    "feed,mention,ratings,messages,messaging_postbacks,message_reactions";

/// Subscribe a Page to this App's webhooks via `POST /{page-id}/subscribed_apps`
/// using the Page's own access token. This is the step OAuth does *not* perform:
/// without it Meta delivers no events for the Page, so a connected Page shows in
/// the dropdown and can post but never fires a trigger. Idempotent — calling it
/// again just refreshes the subscribed field set.
pub async fn subscribe_page(state: &AppState, page_id: &str, page_token: &str) -> Result<Value> {
    let resp = state
        .client
        .post(format!("{FB_API}/{page_id}/subscribed_apps"))
        .query(&[
            ("subscribed_fields", FB_WEBHOOK_FIELDS),
            ("access_token", page_token),
        ])
        .send()
        .await?;
    let v: Value = ensure_ok(resp).await?.json().await?;
    Ok(v)
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

    // Subscribe this Page to the App's webhooks so Meta delivers its events.
    // Non-fatal: posting still works without it, but triggers won't fire.
    if let Err(e) = subscribe_page(state, &page_id, page_token).await {
        tracing::warn!("FB exchange_code: failed to subscribe page {page_id} to webhooks: {e}");
    }

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
